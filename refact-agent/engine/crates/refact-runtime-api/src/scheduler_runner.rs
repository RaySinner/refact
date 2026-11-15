use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant as TokioInstant;

use crate::app_state::AppState;
use refact_core::call_validation::ChatMessage;
use refact_chat_api::internal_roles::{event, EventSubkind};
use crate::session_runtime::get_or_create_session_with_trajectory;
use crate::session_runtime::try_restore_session_if_trajectory_exists;

use refact_core::chat_types::{DeliveryOutcome, PushMode};
use crate::files_correction::get_active_project_path;
use crate::global_context::SharedGlobalContext;
use refact_scheduler_core::schedule::scheduler_timezone;

use crate::scheduler_delivery::{deliver, deliver_notice, deliver_scheduled_turn};
use crate::scheduler_exec_action::{
    command_error_summary, command_failure_can_retry, command_record_error, command_retry_text,
    run_command, CommandRunResult,
};
use refact_scheduler_core::jitter::JitterConfig;
use refact_scheduler_core::retry::{classify, retry_delay_ms};
use refact_scheduler_core::schedule::{
    MissedRunGraceConfig, recurring_missed_grace_state, runner_enabled, scheduled_fire_at_ms,
};
use refact_scheduler_core::planning::{
    command_job_has_non_chat_delivery, event_fire_count, job_is_isolated,
    job_was_advanced_before_fire, missed_one_shot_task, now_ms, record_run,
    runnable_chat_id, runnable_command_job, runnable_isolated_job,
    task_can_use_cron_lane, task_final_after_fire,
};
use refact_scheduler_core::store::{CronStore, InMemoryCronStore, JsonFileCronStore};
use refact_scheduler_core::types::{
    Action, AgentTarget, CommandSpec, Delivery, Job, SchedulerConfig, Trigger,
};

const DEFAULT_SLEEP_MS: u64 = 60_000;
pub const IDLE_DEFER_MS: u64 = 30_000;
pub const INVALID_TARGET_DEFER_MS: u64 = 60_000;
pub const DAY_MS: u64 = 24 * 60 * 60 * 1000;

pub struct CronRunner {
    pub store: Arc<dyn CronStore>,
    pub gcx: SharedGlobalContext,
    pub shutdown_flag: Arc<AtomicBool>,
    pub change_notify: Arc<Notify>,
    pub jitter_cfg: JitterConfig,
    run_semaphore: Arc<Semaphore>,
    pub deferred_until_ms: HashMap<String, u64>,
}

impl CronRunner {
    pub fn new(store: Arc<dyn CronStore>, gcx: SharedGlobalContext) -> Self {
        let shutdown_flag = gcx.shutdown_flag.clone();
        let change_notify = store.change_notify();
        let max_concurrent_runs = gcx.scheduler_config.max_concurrent_runs.max(1);
        Self {
            store,
            gcx,
            shutdown_flag,
            change_notify,
            jitter_cfg: JitterConfig::default(),
            run_semaphore: Arc::new(Semaphore::new(max_concurrent_runs)),
            deferred_until_ms: HashMap::new(),
        }
    }

    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    pub async fn fire_manual_job(
        gcx: SharedGlobalContext,
        mut task: Job,
        now: u64,
    ) -> Result<bool, String> {
        if !runner_enabled(&gcx.scheduler_config) {
            return Ok(false);
        }
        task.trigger_at_ms = Some(now);
        let store = Arc::new(InMemoryCronStore::new());
        let runner = Self::new(store, gcx);
        runner.fire_with_missed(&task, false, now, false).await
    }

    pub async fn fire_store_job_now(
        store: Arc<dyn CronStore>,
        gcx: SharedGlobalContext,
        job_id: &str,
        now: u64,
    ) -> Result<bool, String> {
        if !runner_enabled(&gcx.scheduler_config) {
            return Ok(false);
        }
        let mut task = store
            .get(job_id)
            .await
            .ok_or_else(|| format!("Scheduled task {job_id} not found"))?;
        task.trigger_at_ms = Some(now);
        let mut runner = Self::new(store.clone(), gcx);
        let fired = runner.handle_due_task(task, now).await;
        if !fired {
            if let Some(mut stored) = store
                .get(job_id)
                .await
                .filter(|task| task.last_status.as_deref() == Some("deferred"))
            {
                stored.trigger_at_ms = Some(now);
                let _ = store.replace(stored).await?;
            }
        }
        Ok(fired)
    }

    async fn run(mut self) {
        if !runner_enabled(&self.gcx.scheduler_config) {
            return;
        }
        self.catch_up().await;

        loop {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                break;
            }

            let now = now_ms();
            let tasks = self.store.list().await;
            let next = tasks
                .iter()
                .filter_map(|task| self.scheduled_fire_at_ms(task, now))
                .min()
                .unwrap_or(now + DEFAULT_SLEEP_MS);
            let sleep_until = TokioInstant::now() + Duration::from_millis(next.saturating_sub(now));

            tokio::select! {
                _ = tokio::time::sleep_until(sleep_until) => {}
                _ = self.change_notify.notified() => continue,
                _ = wait_for_shutdown(self.shutdown_flag.clone()) => break,
            }

            self.fire_due_tasks(now_ms()).await;
        }
    }

    pub async fn catch_up(&mut self) {
        let now = now_ms();
        let mut missed_counts = HashMap::<String, u64>::new();

        for task in self.store.list().await {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                break;
            }
            if !task.durable {
                continue;
            }
            if task.recurring {
                self.resume_recurring_task(&task, now).await;
                continue;
            }
            if !missed_one_shot_task(&task, now) {
                continue;
            }
            let chat_id = task.chat_id().map(str::to_string);
            if let Some(chat_id) = &chat_id {
                let app = crate::app_state_from_gcx(self.gcx.clone()).await;
                let restored =
                    try_restore_session_if_trajectory_exists(app.chat_ctx(), &self.gcx.chat_sessions, chat_id)
                        .await;
                if !restored {
                    tracing::warn!(
                        "skipping missed durable one-shot {}: no trajectory found for chat {}",
                        task.id,
                        chat_id
                    );
                    let _ = self
                        .record_run(
                            &task.id,
                            "deferred",
                            Some("no trajectory found".into()),
                            now,
                        )
                        .await;
                    self.defer_invalid_target_task(&task, now);
                    continue;
                }
            } else if !job_is_isolated(&task) && !command_job_has_non_chat_delivery(&task) {
                continue;
            }
            match self.fire_with_missed(&task, true, now, true).await {
                Ok(true) => {
                    if let Some(chat_id) = chat_id {
                        *missed_counts.entry(chat_id).or_default() += 1;
                    }
                    if let Err(error) = self.store.remove(&task.id).await {
                        tracing::warn!(
                            "failed to remove caught-up scheduled task {}: {}",
                            task.id,
                            error
                        );
                    }
                }
                Ok(false) => {
                    let _ = self
                        .record_run(
                            &task.id,
                            "deferred",
                            Some("delivery not accepted".into()),
                            now,
                        )
                        .await;
                    self.defer_task(&task, now);
                }
                Err(error) => {
                    tracing::warn!("failed to catch up scheduled task {}: {}", task.id, error);
                    let _ = self
                        .record_run(&task.id, "deferred", Some(error), now)
                        .await;
                    self.defer_task(&task, now);
                }
            }
        }

        for (chat_id, missed_count) in missed_counts {
            self.emit_catch_up_notice(&chat_id, missed_count).await;
        }
    }

    async fn resume_recurring_task(&self, task: &Job, now: u64) {
        if !matches!(
            task.trigger,
            Trigger::Cron { .. } | Trigger::Interval { .. }
        ) {
            return;
        }
        if task
            .trigger_at_ms
            .is_some_and(|trigger_at_ms| trigger_at_ms <= now)
        {
            return;
        }
        let from_ms = task.last_fired_at_ms.unwrap_or(task.created_at_ms);
        let Some(state) = recurring_missed_grace_state(
            task,
            from_ms,
            now,
            scheduler_timezone(),
            self.missed_grace_config(),
        ) else {
            return;
        };
        if state.due_ms.is_some() && !state.should_fire {
            let Some(advance_to_ms) = state.advance_last_fired_at_ms else {
                return;
            };
            if let Err(error) = self
                .store
                .update_fired(&task.id, advance_to_ms, task.fire_count)
                .await
            {
                tracing::warn!("failed to resume scheduled task {}: {}", task.id, error);
            }
        }
    }

    fn missed_grace_config(&self) -> MissedRunGraceConfig {
        MissedRunGraceConfig::from(&self.gcx.scheduler_config)
    }

    fn recent_runs_cap(&self) -> usize {
        self.gcx.scheduler_config.recent_runs_cap
    }

    pub async fn fire_due_tasks(&mut self, now: u64) {
        if !runner_enabled(&self.gcx.scheduler_config) {
            return;
        }
        let due_tasks = self
            .store
            .list()
            .await
            .into_iter()
            .filter(|task| {
                self.task_is_due(task, now) || self.task_needs_missed_fast_forward(task, now)
            })
            .collect::<Vec<_>>();

        let mut handles = Vec::new();
        let max_concurrent_runs = self.gcx.scheduler_config.max_concurrent_runs.max(1);
        for task in due_tasks {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                break;
            }
            match self.fast_forward_recurring_if_missed(&task, now).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        "failed to fast-forward scheduled task {}: {}",
                        task.id,
                        error
                    );
                    continue;
                }
            }
            if task_can_use_cron_lane(&task) && handles.len() >= max_concurrent_runs {
                self.defer_task(&task, now);
                continue;
            }
            if task_can_use_cron_lane(&task) && self.run_semaphore.available_permits() == 0 {
                self.defer_task(&task, now);
                continue;
            }
            if !task_can_use_cron_lane(&task) {
                self.handle_due_task(task, now).await;
                continue;
            }
            let Ok(permit) = self.run_semaphore.clone().try_acquire_owned() else {
                self.defer_task(&task, now);
                continue;
            };
            let store = self.store.clone();
            let gcx = self.gcx.clone();
            let shutdown_flag = self.shutdown_flag.clone();
            let change_notify = self.change_notify.clone();
            let jitter_cfg = self.jitter_cfg.clone();
            let task_id = task.id.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit;
                let mut runner = CronRunner {
                    store,
                    gcx,
                    shutdown_flag,
                    change_notify,
                    jitter_cfg,
                    run_semaphore: Arc::new(Semaphore::new(1)),
                    deferred_until_ms: HashMap::new(),
                };
                runner.handle_due_task(task, now).await;
                (task_id, runner.deferred_until_ms)
            }));
        }
        for handle in handles {
            if let Ok((task_id, deferred)) = handle.await {
                if deferred.contains_key(&task_id) {
                    self.deferred_until_ms.extend(deferred);
                } else {
                    self.deferred_until_ms.remove(&task_id);
                }
            }
        }
    }

    pub async fn handle_due_task(&mut self, task: Job, now: u64) -> bool {
        if matches!(&task.action, Action::Command { .. }) {
            return self.handle_due_command_task(task, now).await;
        }
        if job_is_isolated(&task) {
            return self.handle_due_isolated_task(task, now).await;
        }

        let chat_id = match runnable_chat_id(&task) {
            Ok(chat_id) => chat_id,
            Err("missing chat_id") => {
                self.handle_unfireable_task(&task, now, "missing chat_id")
                    .await;
                return false;
            }
            Err(reason) => {
                self.handle_unfireable_task(&task, now, reason).await;
                return false;
            }
        };

        match chat_fire_status(&self.gcx, chat_id).await {
            ChatFireStatus::Fireable => {}
            ChatFireStatus::Busy => {}
            ChatFireStatus::Missing => {
                if task.durable {
                    let app = crate::app_state_from_gcx(self.gcx.clone()).await;
                    let restored = try_restore_session_if_trajectory_exists(
                        app.chat_ctx(),
                        &self.gcx.chat_sessions,
                        chat_id,
                    )
                    .await;
                    if !restored {
                        tracing::warn!(
                            "durable task {} deferred: no trajectory found for chat {}",
                            task.id,
                            chat_id
                        );
                        if let Err(error) = self
                            .record_run(
                                &task.id,
                                "deferred",
                                Some("no trajectory found".to_string()),
                                now,
                            )
                            .await
                        {
                            tracing::warn!(
                                "failed to record deferred scheduled task {}: {}",
                                task.id,
                                error
                            );
                        }
                        self.defer_invalid_target_task(&task, now);
                        return false;
                    }
                    match chat_fire_status(&self.gcx, chat_id).await {
                        ChatFireStatus::Fireable => {}
                        ChatFireStatus::Busy => {}
                        status => {
                            tracing::warn!(
                                "durable task {} deferred after session restore ({:?})",
                                task.id,
                                status
                            );
                            if let Err(error) = self
                                .record_run(
                                    &task.id,
                                    "deferred",
                                    Some(format!("session restore status: {status:?}")),
                                    now,
                                )
                                .await
                            {
                                tracing::warn!(
                                    "failed to record deferred scheduled task {}: {}",
                                    task.id,
                                    error
                                );
                            }
                            self.defer_invalid_target_task(&task, now);
                            return false;
                        }
                    }
                } else {
                    self.handle_unfireable_task(&task, now, "chat session not found")
                        .await;
                    return false;
                }
            }
            ChatFireStatus::Closed => {
                self.handle_unfireable_task(&task, now, "chat session is closed")
                    .await;
                return false;
            }
        }

        let final_fire = task_final_after_fire(&task, now);
        match self.fire(&task, final_fire, now).await {
            Ok(true) => {}
            Ok(false) => {
                if self.record_skipped_after_advance(&task, now).await {
                    return false;
                }
                if let Err(error) = self.record_run(&task.id, "deferred", None, now).await {
                    tracing::warn!(
                        "failed to record deferred scheduled task {}: {}",
                        task.id,
                        error
                    );
                }
                self.defer_task(&task, now);
                return false;
            }
            Err(error) => {
                tracing::warn!("failed to fire scheduled task {}: {}", task.id, error);
                if let Err(store_error) = self
                    .record_run(&task.id, "deferred", Some(error), now)
                    .await
                {
                    tracing::warn!(
                        "failed to record deferred task {}: {}",
                        task.id,
                        store_error
                    );
                }
                self.defer_task(&task, now);
                return false;
            }
        }

        if let Err(error) = self.record_run(&task.id, "fired", None, now).await {
            tracing::warn!(
                "failed to record fired scheduled task {}: {}",
                task.id,
                error
            );
        }
        if final_fire {
            match self.store.remove(&task.id).await {
                Ok(true) => self.emit_auto_expired_notice(&task).await,
                Ok(false) => {
                    tracing::warn!("expired scheduled task {} was already removed", task.id);
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to remove expired scheduled task {}: {}",
                        task.id,
                        error
                    );
                }
            }
        } else if !task.recurring {
            if let Err(error) = self.remove_task(&task).await {
                tracing::warn!(
                    "failed to remove fired one-shot scheduled task {}: {}",
                    task.id,
                    error
                );
            }
        }
        self.deferred_until_ms.remove(&task.id);
        true
    }

    async fn handle_due_command_task(&mut self, task: Job, now: u64) -> bool {
        let cmd = match task.command_spec() {
            Some(cmd) => cmd,
            None => {
                self.handle_unfireable_task(&task, now, "command action is missing")
                    .await;
                return false;
            }
        };
        if let Err(reason) = runnable_command_job(&task) {
            self.handle_unfireable_task(&task, now, reason).await;
            return false;
        }
        if matches!(task.delivery, Delivery::Chat) {
            if let AgentTarget::ExistingChat { chat_id } = &cmd.target {
                if chat_id.is_empty() {
                    self.handle_unfireable_task(&task, now, "missing chat_id")
                        .await;
                    return false;
                }
                match chat_fire_status(&self.gcx, chat_id).await {
                    ChatFireStatus::Fireable => {}
                    ChatFireStatus::Busy => {}
                    ChatFireStatus::Missing => {
                        if task.durable {
                            let app = crate::app_state_from_gcx(self.gcx.clone()).await;
                            let restored = try_restore_session_if_trajectory_exists(
                                app.chat_ctx(),
                                &self.gcx.chat_sessions,
                                chat_id,
                            )
                            .await;
                            if !restored {
                                if let Err(error) = self
                                    .record_run(
                                        &task.id,
                                        "deferred",
                                        Some("no trajectory found".to_string()),
                                        now,
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        "failed to record deferred scheduled task {}: {}",
                                        task.id,
                                        error
                                    );
                                }
                                self.defer_invalid_target_task(&task, now);
                                return false;
                            }
                        } else {
                            self.handle_unfireable_task(&task, now, "chat session not found")
                                .await;
                            return false;
                        }
                    }
                    ChatFireStatus::Closed => {
                        self.handle_unfireable_task(&task, now, "chat session is closed")
                            .await;
                        return false;
                    }
                }
            }
        }

        let final_fire = task_final_after_fire(&task, now);
        let result = match self
            .fire_command_with_missed(&task, &cmd, final_fire, now, false)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!("failed to fire scheduled task {}: {}", task.id, error);
                if self
                    .handle_classifiable_fire_error(&task, final_fire, now, &error)
                    .await
                {
                    return false;
                }
                self.handle_unfireable_task(&task, now, &error).await;
                return false;
            }
        };

        let error = (result.status == "error").then(|| command_record_error(&result));
        if let Err(store_error) = self.record_run(&task.id, &result.status, error, now).await {
            tracing::warn!(
                "failed to record scheduled task {} status {}: {}",
                task.id,
                result.status,
                store_error
            );
        }
        if result.status == "error" && command_failure_can_retry(&result) {
            match self
                .schedule_retry(&task, now, &command_retry_text(&result))
                .await
            {
                Ok(true) => {
                    self.deferred_until_ms.remove(&task.id);
                    return false;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!("failed to schedule retry for task {}: {}", task.id, error);
                }
            }
        }
        if final_fire {
            match self.store.remove(&task.id).await {
                Ok(true) => self.emit_auto_expired_notice(&task).await,
                Ok(false) => {
                    tracing::warn!("expired scheduled task {} was already removed", task.id);
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to remove expired scheduled task {}: {}",
                        task.id,
                        error
                    );
                }
            }
        } else if !task.recurring {
            if let Err(error) = self.remove_task(&task).await {
                tracing::warn!(
                    "failed to remove fired one-shot scheduled task {}: {}",
                    task.id,
                    error
                );
            }
        }
        self.deferred_until_ms.remove(&task.id);
        result.status == "fired"
    }

    async fn handle_due_isolated_task(&mut self, task: Job, now: u64) -> bool {
        if let Err(reason) = runnable_isolated_job(&task) {
            self.handle_unfireable_task(&task, now, reason).await;
            return false;
        }

        let final_fire = task_final_after_fire(&task, now);
        match self.fire(&task, final_fire, now).await {
            Ok(true) => {}
            Ok(false) => {
                if self.record_skipped_after_advance(&task, now).await {
                    return false;
                }
                if let Err(error) = self.record_run(&task.id, "deferred", None, now).await {
                    tracing::warn!(
                        "failed to record deferred scheduled task {}: {}",
                        task.id,
                        error
                    );
                }
                self.defer_task(&task, now);
                return false;
            }
            Err(error) => {
                tracing::warn!("failed to fire scheduled task {}: {}", task.id, error);
                if let Err(store_error) = self
                    .record_run(&task.id, "deferred", Some(error), now)
                    .await
                {
                    tracing::warn!(
                        "failed to record deferred task {}: {}",
                        task.id,
                        store_error
                    );
                }
                self.defer_task(&task, now);
                return false;
            }
        }

        if let Err(error) = self.record_run(&task.id, "fired", None, now).await {
            tracing::warn!(
                "failed to record fired scheduled task {}: {}",
                task.id,
                error
            );
        }
        if final_fire {
            match self.store.remove(&task.id).await {
                Ok(true) => self.emit_auto_expired_notice(&task).await,
                Ok(false) => {
                    tracing::warn!("expired scheduled task {} was already removed", task.id);
                }
                Err(error) => {
                    tracing::warn!(
                        "failed to remove expired scheduled task {}: {}",
                        task.id,
                        error
                    );
                }
            }
        } else if !task.recurring {
            if let Err(error) = self.remove_task(&task).await {
                tracing::warn!(
                    "failed to remove fired one-shot scheduled task {}: {}",
                    task.id,
                    error
                );
            }
        }
        self.deferred_until_ms.remove(&task.id);
        true
    }

    async fn handle_unfireable_task(&mut self, task: &Job, now: u64, reason: &str) {
        if task.recurring || task.durable {
            tracing::warn!(
                "deferred scheduled task {} because it is not fireable: {}",
                task.id,
                reason
            );
            if let Err(error) = self
                .record_run(&task.id, "deferred", Some(reason.to_string()), now)
                .await
            {
                tracing::warn!(
                    "failed to record deferred scheduled task {}: {}",
                    task.id,
                    error
                );
            }
            self.defer_invalid_target_task(task, now);
            return;
        }

        tracing::warn!(
            "removing one-shot scheduled task {} because it is not fireable: {}",
            task.id,
            reason
        );
        if let Err(error) = self
            .record_run(&task.id, "skipped", Some(reason.to_string()), now)
            .await
        {
            tracing::warn!(
                "failed to record skipped scheduled task {}: {}",
                task.id,
                error
            );
        }
        if let Err(error) = self.remove_task(task).await {
            tracing::warn!(
                "failed to remove unfireable one-shot scheduled task {}: {}",
                task.id,
                error
            );
        }
    }

    async fn handle_classifiable_fire_error(
        &mut self,
        task: &Job,
        final_fire: bool,
        now: u64,
        error: &str,
    ) -> bool {
        if classify(error).is_none() {
            return false;
        }
        if let Err(store_error) = self
            .record_run(&task.id, "error", Some(error.to_string()), now)
            .await
        {
            tracing::warn!(
                "failed to record retryable scheduled task {} error: {}",
                task.id,
                store_error
            );
        }
        match self.schedule_retry(task, now, error).await {
            Ok(true) => {
                self.deferred_until_ms.remove(&task.id);
                return true;
            }
            Ok(false) => {}
            Err(schedule_error) => {
                tracing::warn!(
                    "failed to schedule retry for task {}: {}",
                    task.id,
                    schedule_error
                );
            }
        }
        if final_fire {
            match self.store.remove(&task.id).await {
                Ok(true) => self.emit_auto_expired_notice(task).await,
                Ok(false) => {
                    tracing::warn!("expired scheduled task {} was already removed", task.id)
                }
                Err(remove_error) => tracing::warn!(
                    "failed to remove expired scheduled task {}: {}",
                    task.id,
                    remove_error
                ),
            }
        } else if !task.recurring {
            if let Err(remove_error) = self.remove_task(task).await {
                tracing::warn!(
                    "failed to remove failed one-shot scheduled task {}: {}",
                    task.id,
                    remove_error
                );
            }
        } else {
            self.deferred_until_ms.remove(&task.id);
        }
        true
    }

    async fn record_skipped_after_advance(&mut self, task: &Job, now: u64) -> bool {
        if !job_was_advanced_before_fire(task, now) {
            return false;
        }
        if let Err(error) = self
            .record_run(
                &task.id,
                "skipped",
                Some("slot advanced before enqueue".to_string()),
                now,
            )
            .await
        {
            tracing::warn!(
                "failed to record skipped scheduled task {}: {}",
                task.id,
                error
            );
        }
        self.deferred_until_ms.remove(&task.id);
        true
    }

    async fn schedule_retry(&self, task: &Job, now: u64, error: &str) -> Result<bool, String> {
        let Some(category) = classify(error) else {
            return Ok(false);
        };
        if self.shutdown_flag.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let mut stored = match self.store.get(&task.id).await {
            Some(stored) => stored,
            None => return Ok(false),
        };
        let retry_cfg = self.gcx.scheduler_config.retry.clone();
        let Some(delay_ms) = retry_delay_ms(&retry_cfg, stored.retry_attempts) else {
            return Ok(false);
        };
        stored.retry_attempts = stored.retry_attempts.saturating_add(1);
        stored.trigger_at_ms = Some(now.saturating_add(delay_ms));
        let next_retry_at_ms = stored.trigger_at_ms;
        let retry_attempts = stored.retry_attempts;
        if !self.store.replace(stored).await? {
            return Ok(false);
        }
        tracing::info!(
            task_id = %task.id,
            ?category,
            retry_attempts,
            ?next_retry_at_ms,
            "scheduled retry for transient scheduler failure"
        );
        Ok(true)
    }

    async fn fast_forward_recurring_if_missed(&self, task: &Job, now: u64) -> Result<bool, String> {
        if !task.recurring || task.trigger_at_ms.is_some() || task.retry_attempts > 0 {
            return Ok(false);
        }
        let from_ms = task.last_fired_at_ms.unwrap_or(task.created_at_ms);
        let Some(state) = recurring_missed_grace_state(
            task,
            from_ms,
            now,
            scheduler_timezone(),
            self.missed_grace_config(),
        ) else {
            return Ok(false);
        };
        if state.due_ms.is_some() && !state.should_fire {
            let Some(advance_to_ms) = state.advance_last_fired_at_ms else {
                return Ok(false);
            };
            self.store
                .update_fired(&task.id, advance_to_ms, task.fire_count)
                .await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn record_run(
        &self,
        job_id: &str,
        status: &str,
        error: Option<String>,
        now_ms: u64,
    ) -> Result<(), String> {
        record_run(
            &self.store,
            job_id,
            status,
            error,
            now_ms,
            self.recent_runs_cap(),
        )
        .await
    }

    async fn remove_task(&mut self, task: &Job) -> Result<(), String> {
        let _ = self.store.remove(&task.id).await?;
        self.deferred_until_ms.remove(&task.id);
        Ok(())
    }

    fn defer_task(&mut self, task: &Job, now: u64) {
        self.deferred_until_ms
            .insert(task.id.clone(), now + IDLE_DEFER_MS);
    }

    fn defer_invalid_target_task(&mut self, task: &Job, now: u64) {
        self.deferred_until_ms
            .insert(task.id.clone(), now + INVALID_TARGET_DEFER_MS);
    }

    fn scheduled_fire_at_ms(&self, task: &Job, now: u64) -> Option<u64> {
        if task.trigger_at_ms.is_none() && task.is_paused() {
            return None;
        }
        if let Some(trigger_at_ms) = task.trigger_at_ms.filter(|at| *at <= now) {
            // A fresh explicit run-now overrides an older backoff; retrying the
            // same rejected admission must still honor its defer timer.
            let new_trigger = task.last_status.as_deref() != Some("deferred")
                || task
                    .recent_runs
                    .last()
                    .is_none_or(|run| trigger_at_ms > run.at_ms);
            if new_trigger || !self.deferred_until_ms.contains_key(&task.id) {
                return Some(trigger_at_ms);
            }
        }
        if let Some(deferred_at) = self.deferred_until_ms.get(&task.id).copied() {
            return Some(deferred_at);
        }
        if task.retry_attempts > 0 && task.trigger_at_ms.is_some() {
            return task.trigger_at_ms;
        }
        if self.task_needs_missed_fast_forward(task, now) {
            return Some(now);
        }
        scheduled_fire_at_ms(task, now, &self.jitter_cfg, self.missed_grace_config())
    }

    pub fn task_is_due(&self, task: &Job, now: u64) -> bool {
        self.scheduled_fire_at_ms(task, now)
            .is_some_and(|fire_at| fire_at <= now)
    }

    fn task_needs_missed_fast_forward(&self, task: &Job, now: u64) -> bool {
        if !task.recurring || task.trigger_at_ms.is_some() || task.retry_attempts > 0 {
            return false;
        }
        recurring_missed_grace_state(
            task,
            task.last_fired_at_ms.unwrap_or(task.created_at_ms),
            now,
            scheduler_timezone(),
            self.missed_grace_config(),
        )
        .is_some_and(|state| state.due_ms.is_some() && !state.should_fire)
    }

    pub async fn fire(&self, task: &Job, final_fire: bool, fired_at_ms: u64) -> Result<bool, String> {
        self.fire_with_missed(task, final_fire, fired_at_ms, false)
            .await
    }

    pub async fn fire_with_missed(
        &self,
        task: &Job,
        final_fire: bool,
        fired_at_ms: u64,
        missed: bool,
    ) -> Result<bool, String> {
        if let Some(cmd) = task.command_spec() {
            let _ = self
                .fire_command_with_missed(task, &cmd, final_fire, fired_at_ms, missed)
                .await?;
            return Ok(true);
        }
        if job_is_isolated(&task) {
            return self
                .fire_isolated_with_missed(task, final_fire, fired_at_ms, missed)
                .await;
        }

        let chat_id = runnable_chat_id(task).map_err(|reason| {
            format!(
                "Scheduled task {} is not a runnable chat job: {reason}",
                task.id
            )
        })?;
        let _session_arc = {
            let sessions = self.gcx.chat_sessions.read().await;
            sessions.get(chat_id).cloned()
        }
        .ok_or_else(|| format!("Chat session {chat_id} not found"))?;
        let app = crate::app_state_from_gcx(self.gcx.clone()).await;
        let event_message = if missed {
            cron_fire_message_with_missed(task, final_fire, missed)
        } else {
            cron_fire_message(task, final_fire)
        };
        let prompt = task.prompt().unwrap_or_default().to_string();
        let outcome = deliver_scheduled_turn(
            &app,
            chat_id,
            task,
            fired_at_ms,
            vec![event_message, scheduled_prompt_message(prompt)],
        )
        .await?;
        Ok(matches!(
            outcome,
            DeliveryOutcome::Delivered | DeliveryOutcome::Queued | DeliveryOutcome::Duplicate
        ))
    }

    async fn fire_command_with_missed(
        &self,
        task: &Job,
        cmd: &CommandSpec,
        final_fire: bool,
        fired_at_ms: u64,
        missed: bool,
    ) -> Result<CommandRunResult, String> {
        runnable_command_job(task).map_err(|reason| {
            format!(
                "Scheduled task {} is not a runnable command job: {reason}",
                task.id
            )
        })?;
        let app = crate::app_state_from_gcx(self.gcx.clone()).await;
        let result = run_command(&app, task, cmd).await;
        if let Err(error) = self
            .deliver_command_result(&app, task, cmd, final_fire, fired_at_ms, missed, &result)
            .await
        {
            if let Err(store_error) = self.record_delivery_error(&task.id, error).await {
                tracing::warn!(
                    "failed to record delivery error for scheduled task {}: {}",
                    task.id,
                    store_error
                );
            }
        } else if let Err(store_error) = self.clear_delivery_error(&task.id).await {
            tracing::warn!(
                "failed to clear delivery error for scheduled task {}: {}",
                task.id,
                store_error
            );
        }
        Ok(result)
    }

    async fn clear_delivery_error(&self, job_id: &str) -> Result<(), String> {
        let Some(mut task) = self.store.get(job_id).await else {
            return Ok(());
        };
        task.last_delivery_error = None;
        if self.store.replace(task).await? {
            Ok(())
        } else {
            Ok(())
        }
    }

    async fn record_delivery_error(&self, job_id: &str, error: String) -> Result<(), String> {
        let mut task = self
            .store
            .get(job_id)
            .await
            .ok_or_else(|| format!("Scheduled task {job_id} not found"))?;
        task.last_delivery_error = Some(error);
        if self.store.replace(task).await? {
            Ok(())
        } else {
            Err(format!("Scheduled task {job_id} not found"))
        }
    }

    async fn deliver_command_result(
        &self,
        app: &AppState,
        task: &Job,
        _cmd: &CommandSpec,
        _final_fire: bool,
        fired_at_ms: u64,
        _missed: bool,
        result: &CommandRunResult,
    ) -> Result<(), String> {
        let mut delivery_job = task.clone();
        delivery_job.last_status = Some(result.status.clone());
        delivery_job.last_error = (result.status == "error").then(|| command_record_error(result));
        delivery_job.last_fired_at_ms = Some(fired_at_ms);
        delivery_job.fire_count = if job_was_advanced_before_fire(task, fired_at_ms) {
            task.fire_count.saturating_sub(1)
        } else {
            task.fire_count
        };
        let output = if result.status == "error" {
            command_error_summary(result)
        } else {
            result.stdout.clone()
        };
        deliver(app, &delivery_job, &output).await
    }

    async fn fire_isolated_with_missed(
        &self,
        task: &Job,
        final_fire: bool,
        fired_at_ms: u64,
        missed: bool,
    ) -> Result<bool, String> {
        runnable_isolated_job(task).map_err(|reason| {
            format!(
                "Scheduled task {} is not a runnable isolated chat job: {reason}",
                task.id
            )
        })?;
        let chat_id = format!("cron_{}_{}", task.id, task.fire_count);
        let app = crate::app_state_from_gcx(self.gcx.clone()).await;
        let session_arc =
            get_or_create_session_with_trajectory(app.chat_ctx(), &app.chat.sessions, &chat_id).await;
        let event_message = if missed {
            cron_fire_message_with_missed(task, final_fire, missed)
        } else {
            cron_fire_message(task, final_fire)
        };
        let prompt = task.prompt().unwrap_or_default().to_string();
        let _ = session_arc;
        deliver_scheduled_turn(
            &app,
            &chat_id,
            task,
            fired_at_ms,
            vec![event_message, scheduled_prompt_message(prompt)],
        )
        .await?;
        Ok(true)
    }

    /// Pure notices: routed through the delivery layer (default append) and
    /// never waking a chat.
    async fn emit_catch_up_notice(&self, chat_id: &str, missed_count: u64) {
        if !self.gcx.chat_sessions.read().await.contains_key(chat_id) {
            return;
        }
        let app = crate::app_state_from_gcx(self.gcx.clone()).await;
        if let Err(error) = deliver_notice(
            &app,
            chat_id,
            format!("cron-catch-up-{chat_id}-{}", now_ms()),
            catch_up_notice_message(missed_count),
            PushMode::Append,
        )
        .await
        {
            tracing::warn!("failed to deliver scheduler catch-up notice: {error}");
        }
    }

    async fn emit_auto_expired_notice(&self, task: &Job) {
        let Some(chat_id) = task.chat_id() else {
            return;
        };
        if !self.gcx.chat_sessions.read().await.contains_key(chat_id) {
            return;
        }
        let app = crate::app_state_from_gcx(self.gcx.clone()).await;
        if let Err(error) = deliver_notice(
            &app,
            chat_id,
            format!("cron-auto-expired-{}", task.id),
            auto_expired_notice_message(task),
            PushMode::Append,
        )
        .await
        {
            tracing::warn!(
                "failed to deliver auto-expiry notice for scheduled task {}: {error}",
                task.id
            );
        }
    }
}

pub fn spawn(store: Arc<dyn CronStore>, gcx: SharedGlobalContext) -> JoinHandle<()> {
    CronRunner::new(store, gcx).spawn()
}

pub async fn spawn_from_active_project(gcx: SharedGlobalContext) -> Vec<JoinHandle<()>> {
    if !runner_enabled(&gcx.scheduler_config) {
        return Vec::new();
    }

    let mut handles = vec![spawn(session_cron_store(), gcx.clone())];
    if let Some(project_root) = get_active_project_path(gcx.clone()).await {
        match JsonFileCronStore::new(project_root) {
            Ok(store) => handles.push(spawn(Arc::new(store), gcx)),
            Err(error) => tracing::warn!("durable scheduler runner disabled: {error}"),
        }
    }
    handles
}

pub use refact_scheduler_core::runtime_state::{session_cron_store, runner_change_notify, notify_runner_change};

pub fn spawn_if_enabled(
    store: Arc<dyn CronStore>,
    config: SchedulerConfig,
) -> Option<JoinHandle<()>> {
    if !runner_enabled(&config) {
        return None;
    }
    // best-effort: we don't have a gcx here, so just spawn a no-op task that mirrors the
    // configured kill-switch semantics. Real spawn happens via spawn_from_active_project.
    let _ = store;
    Some(tokio::spawn(async {}))
}

#[derive(Debug, Eq, PartialEq)]
enum ChatFireStatus {
    Fireable,
    Busy,
    Missing,
    Closed,
}

pub async fn chat_is_idle(gcx: &SharedGlobalContext, chat_id: &str) -> bool {
    chat_fire_status(gcx, chat_id).await == ChatFireStatus::Fireable
}

async fn chat_fire_status(gcx: &SharedGlobalContext, chat_id: &str) -> ChatFireStatus {
    let session_arc = {
        let sessions = gcx.chat_sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    let Some(session_arc) = session_arc else {
        return ChatFireStatus::Missing;
    };
    let session = session_arc.lock().await;
    if session.closed {
        return ChatFireStatus::Closed;
    }
    if session.is_idle() && !session.command_queue.iter().any(|r| !r.priority) {
        ChatFireStatus::Fireable
    } else {
        ChatFireStatus::Busy
    }
}





pub fn scheduled_prompt_message(prompt: String) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: refact_core::call_validation::ChatContent::SimpleText(prompt),
        ..Default::default()
    }
}

pub fn cron_fire_message(task: &Job, final_fire: bool) -> ChatMessage {
    cron_fire_message_with_missed(task, final_fire, false)
}

fn cron_fire_message_with_missed(task: &Job, final_fire: bool, missed: bool) -> ChatMessage {
    let mut payload = json!({
        "task_id": task.id,
        "cron": task.cron_expr().unwrap_or_default(),
        "recurring": task.recurring,
        "fire_count": event_fire_count(task),
        "final": final_fire,
        "action_kind": task.action_kind(),
    });
    if missed {
        payload["missed"] = json!(true);
    }
    event(
        EventSubkind::CronFire,
        "scheduler.cron",
        payload,
        task.prompt().unwrap_or(&task.description).to_string(),
    )
}

fn catch_up_notice_message(missed_count: u64) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        "scheduler.cron",
        json!({ "missed_count": missed_count }),
        format!("Caught up {missed_count} missed scheduled tasks"),
    )
}

fn auto_expired_notice_message(task: &Job) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        "scheduler.cron",
        json!({
            "task_id": task.id,
            "reason": "auto_expired",
        }),
        format!(
            "Recurring task '{}' auto-expired after {}d",
            task.description,
            task.auto_expire_after_ms / DAY_MS
        ),
    )
}

async fn wait_for_shutdown(shutdown_flag: Arc<AtomicBool>) {
    while !shutdown_flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
