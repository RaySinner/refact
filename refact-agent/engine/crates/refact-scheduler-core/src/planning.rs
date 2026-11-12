use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};

use crate::schedule::{next_run_ms, scheduler_timezone};
use crate::store::CronStore;
use crate::types::{Action, AgentTarget, CronRunRecord, Delivery, Job, Trigger};

pub fn runnable_chat_id(job: &Job) -> Result<&str, &'static str> {
    if !runner_supported_trigger(&job.trigger) {
        return Err("trigger is not supported by the chat runner yet");
    }
    match (&job.action, &job.delivery) {
        (
            Action::AgentTurn {
                target: AgentTarget::ExistingChat { chat_id },
                ..
            },
            Delivery::Chat,
        ) if !chat_id.is_empty() => Ok(chat_id),
        (Action::AgentTurn { .. }, Delivery::Chat) => Err("missing chat_id"),
        (Action::AgentTurn { .. }, _) => Err("non-chat delivery is not supported yet"),
        (Action::Command { .. }, _) => Err("command actions use the command runner"),
    }
}

pub fn runnable_isolated_job(job: &Job) -> Result<(), &'static str> {
    if !runner_supported_trigger(&job.trigger) {
        return Err("trigger is not supported by the chat runner yet");
    }
    match (&job.action, &job.delivery) {
        (
            Action::AgentTurn {
                target: AgentTarget::Isolated,
                ..
            },
            Delivery::Chat,
        ) => Ok(()),
        (Action::AgentTurn { .. }, Delivery::Chat) => Err("target is not isolated"),
        (Action::AgentTurn { .. }, _) => Err("non-chat delivery is not supported yet"),
        (Action::Command { .. }, _) => Err("command actions use the command runner"),
    }
}

pub fn runnable_command_job(job: &Job) -> Result<(), &'static str> {
    if !runner_supported_trigger(&job.trigger) {
        return Err("trigger is not supported by the command runner yet");
    }
    match (&job.action, &job.delivery) {
        (Action::Command { argv, .. }, _) if argv.is_empty() => Err("command argv is empty"),
        (
            Action::Command {
                target: AgentTarget::ExistingChat { chat_id },
                ..
            },
            Delivery::Chat,
        ) if chat_id.is_empty() => Err("missing chat_id"),
        (Action::Command { .. }, Delivery::Chat) => Ok(()),
        (Action::Command { .. }, _) => Ok(()),
        (Action::AgentTurn { .. }, _) => Err("action is not command"),
    }
}

pub fn runner_supported_trigger(trigger: &Trigger) -> bool {
    matches!(
        trigger,
        Trigger::Cron { .. }
            | Trigger::Interval { .. }
            | Trigger::Once { .. }
            | Trigger::Manual
            | Trigger::Webhook { .. }
    )
}

pub fn scheduled_thread_patch(task: &Job) -> Option<Value> {
    let mut patch = serde_json::Map::new();
    if let Some(mode) = task.mode() {
        patch.insert("mode".to_string(), json!(mode));
    }
    if let Some(model) = job_model(task) {
        patch.insert("model".to_string(), json!(model));
    }
    if patch.is_empty() {
        None
    } else {
        Some(Value::Object(patch))
    }
}

pub fn job_is_isolated(task: &Job) -> bool {
    matches!(
        &task.action,
        Action::AgentTurn {
            target: AgentTarget::Isolated,
            ..
        } | Action::Command {
            target: AgentTarget::Isolated,
            ..
        }
    )
}

pub fn command_job_has_non_chat_delivery(task: &Job) -> bool {
    matches!(&task.action, Action::Command { .. }) && !matches!(task.delivery, Delivery::Chat)
}

pub fn task_can_use_cron_lane(task: &Job) -> bool {
    matches!(task.action, Action::Command { .. }) || job_is_isolated(task)
}

pub fn job_model(task: &Job) -> Option<&str> {
    match &task.action {
        Action::AgentTurn { model, .. } => model.as_deref().filter(|model| !model.is_empty()),
        _ => None,
    }
}

pub fn event_fire_count(task: &Job) -> u32 {
    if task
        .last_fired_at_ms
        .is_some_and(|fired_at_ms| job_was_advanced_before_fire(task, fired_at_ms))
    {
        task.fire_count
    } else {
        task.fire_count.saturating_add(1)
    }
}

pub fn job_was_advanced_before_fire(task: &Job, fired_at_ms: u64) -> bool {
    task.recurring
        && task.last_status.as_deref() == Some("advanced")
        && task
            .last_fired_at_ms
            .is_some_and(|last_fired_at_ms| last_fired_at_ms <= fired_at_ms)
}

pub async fn record_run(
    store: &Arc<dyn CronStore>,
    job_id: &str,
    status: &str,
    error: Option<String>,
    now_ms: u64,
    recent_runs_cap: usize,
) -> Result<(), String> {
    let mut task = store
        .get(job_id)
        .await
        .ok_or_else(|| format!("Scheduled task {job_id} not found"))?;
    let already_advanced = job_was_advanced_before_fire(&task, now_ms);
    if already_advanced {
        if let Some(run) = task
            .recent_runs
            .last_mut()
            .filter(|run| run.at_ms == now_ms)
        {
            run.status = status.to_string();
            run.error = error.clone();
        } else {
            task.recent_runs.push(CronRunRecord {
                at_ms: now_ms,
                status: status.to_string(),
                error: error.clone(),
            });
            cap_recent_runs(&mut task, recent_runs_cap);
        }
    } else {
        task.recent_runs.push(CronRunRecord {
            at_ms: now_ms,
            status: status.to_string(),
            error: error.clone(),
        });
        cap_recent_runs(&mut task, recent_runs_cap);
    }
    task.last_status = Some(status.to_string());
    task.last_error = error;
    // A rejected agent prompt remains the same outstanding fire even after a
    // restart or a long backoff. Do not let missed-run grace fast-forward it.
    if status == "deferred" && matches!(task.action, Action::AgentTurn { .. }) {
        task.trigger_at_ms.get_or_insert(now_ms);
    }
    if status == "fired" || status == "error" {
        if !already_advanced {
            task.last_fired_at_ms = Some(now_ms);
            task.fire_count = task.fire_count.saturating_add(1);
        }
        if status == "fired" {
            task.retry_attempts = 0;
        }
        if task
            .trigger_at_ms
            .is_some_and(|trigger_at_ms| trigger_at_ms <= now_ms)
        {
            task.trigger_at_ms = None;
        }
    }
    if store.replace(task).await? {
        Ok(())
    } else {
        Err(format!("Scheduled task {job_id} not found"))
    }
}

pub fn cap_recent_runs(task: &mut Job, cap: usize) {
    if cap == 0 {
        task.recent_runs.clear();
        return;
    }
    if task.recent_runs.len() > cap {
        task.recent_runs.drain(0..task.recent_runs.len() - cap);
    }
}

pub fn missed_one_shot_task(task: &Job, now: u64) -> bool {
    !task.recurring
        && matches!(task.trigger, Trigger::Cron { .. })
        && task.last_fired_at_ms.is_none()
        && task.fire_count == 0
        && next_run_ms(task, task.created_at_ms, scheduler_timezone())
            .is_some_and(|next| next < now)
}

pub fn task_final_after_fire(task: &Job, now: u64) -> bool {
    task.recurring
        && task.auto_expire_after_ms > 0
        && now.saturating_sub(task.created_at_ms) > task.auto_expire_after_ms
}

pub fn now_ms() -> u64 {
    Utc::now().timestamp_millis().max(0) as u64
}