pub use refact_runtime_api::scheduler_runner::*;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use tokio::time::Instant as TokioInstant;
#[cfg(test)]
use crate::call_validation::ChatMessage;
#[cfg(test)]
use crate::chat::internal_roles::event;
#[cfg(test)]
use refact_core::chat_types::PushMode;
#[cfg(test)]
use crate::global_context::SharedGlobalContext;
#[cfg(test)]
use super::planning::now_ms;
#[cfg(test)]
use super::planning::record_run;
#[cfg(test)]
use super::planning::task_final_after_fire;
#[cfg(test)]
use super::store::CronStore;
#[cfg(test)]
use super::types::Action;
#[cfg(test)]
use super::types::AgentTarget;
#[cfg(test)]
use super::types::CronRunRecord;
#[cfg(test)]
use super::types::Delivery;
#[cfg(test)]
use super::types::Job;
#[cfg(test)]
use super::types::SchedulerConfig;
#[cfg(test)]
use super::types::Trigger;

#[cfg(test)]
use crate::chat::types::max_queue_size;
#[cfg(test)]
fn set_job_isolated(task: &mut Job) {
    if let Action::AgentTurn { target, .. } = &mut task.action {
        *target = AgentTarget::Isolated;
    }
}
#[cfg(test)]
fn set_job_model(task: &mut Job, value: Option<String>) {
    if let Action::AgentTurn { model, .. } = &mut task.action {
        *model = value;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use chrono::{TimeZone, Utc};
    use tokio::sync::Mutex as AMutex;

    use super::*;
    use crate::chat::internal_roles::EVENT_ROLE;
    use crate::chat::types::{ChatSession, SessionState};
    use crate::scheduler::store::InMemoryCronStore;
    use crate::scheduler::types::{DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS, RECENT_RUNS_CAP};

    fn task(id: &str, now: u64) -> Job {
        let mut task = Job::new_cron_agent_chat(
            "*/1 * * * *".to_string(),
            "scheduled prompt".to_string(),
            "scheduled prompt".to_string(),
            true,
            false,
            now - 120_000,
        );
        task.id = id.to_string();
        task.set_existing_chat(Some("chat-1".to_string()));
        task.set_mode(Some("agent".to_string()));
        task.last_fired_at_ms = Some(now - 120_000);
        task.auto_expire_after_ms = DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS;
        task
    }

    fn expired_task(id: &str, now: u64) -> Job {
        let mut task = task(id, now);
        task.created_at_ms = now - 2 * DAY_MS - 1;
        task.auto_expire_after_ms = 2 * DAY_MS;
        task
    }

    fn one_shot_task(id: &str, now: u64) -> Job {
        let mut task = task(id, now);
        task.recurring = false;
        task.last_fired_at_ms = None;
        task.auto_expire_after_ms = 0;
        task
    }

    fn due_task(id: &str, now: u64) -> Job {
        let mut task = task(id, now);
        task.created_at_ms = now - 120_000;
        task.last_fired_at_ms = Some(now - 120_000);
        task
    }

    fn interval_task(id: &str, now: u64, every_ms: u64) -> Job {
        let mut task = task(id, now);
        task.trigger = Trigger::Interval { every_ms };
        task.recurring = true;
        task.created_at_ms = now - every_ms * 2;
        task.last_fired_at_ms = Some(now - every_ms);
        task
    }

    fn once_trigger_task(id: &str, now: u64) -> Job {
        let mut task = task(id, now);
        task.trigger = Trigger::Once {
            at_ms: now.saturating_sub(1_000),
        };
        task.recurring = false;
        task.created_at_ms = now.saturating_sub(60_000);
        task.last_fired_at_ms = None;
        task.auto_expire_after_ms = 0;
        task
    }

    fn isolated_due_task(id: &str, now: u64) -> Job {
        let mut task = due_task(id, now);
        set_job_isolated(&mut task);
        set_job_model(&mut task, Some("model-1".to_string()));
        task
    }

    fn isolated_interval_task(id: &str, now: u64, every_ms: u64) -> Job {
        let mut task = interval_task(id, now, every_ms);
        set_job_isolated(&mut task);
        set_job_model(&mut task, Some("model-1".to_string()));
        task
    }

    fn command_due_task(id: &str, now: u64, argv: Vec<String>) -> Job {
        let mut task = due_task(id, now);
        task.description = format!("{id} command");
        task.action = Action::Command {
            argv,
            target: AgentTarget::ExistingChat {
                chat_id: "chat-1".to_string(),
            },
            cwd: None,
            env: None,
            timeout_secs: Some(5),
        };
        task
    }

    #[cfg(not(target_os = "windows"))]
    fn stdout_command(text: &str) -> Vec<String> {
        vec!["printf".to_string(), text.to_string()]
    }

    #[cfg(target_os = "windows")]
    fn stdout_command(text: &str) -> Vec<String> {
        vec!["cmd".to_string(), "/C".to_string(), format!("echo {text}")]
    }

    #[cfg(not(target_os = "windows"))]
    fn empty_command() -> Vec<String> {
        vec!["true".to_string()]
    }

    #[cfg(target_os = "windows")]
    fn empty_command() -> Vec<String> {
        vec!["cmd".to_string(), "/C".to_string(), "exit 0".to_string()]
    }

    #[cfg(not(target_os = "windows"))]
    fn slow_empty_command() -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), "sleep 0.2".to_string()]
    }

    #[cfg(target_os = "windows")]
    fn slow_empty_command() -> Vec<String> {
        vec![
            "cmd".to_string(),
            "/C".to_string(),
            "ping -n 2 127.0.0.1 >nul".to_string(),
        ]
    }

    #[cfg(not(target_os = "windows"))]
    fn failing_command() -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), "exit 7".to_string()]
    }

    #[cfg(target_os = "windows")]
    fn failing_command() -> Vec<String> {
        vec!["cmd".to_string(), "/C".to_string(), "exit 7".to_string()]
    }

    fn one_shot_command_due_task(id: &str, now: u64, argv: Vec<String>) -> Job {
        let mut task = command_due_task(id, now, argv);
        task.trigger = Trigger::Once {
            at_ms: now.saturating_sub(1_000),
        };
        task.recurring = false;
        task.last_fired_at_ms = None;
        task.auto_expire_after_ms = 0;
        task
    }

    #[cfg(not(target_os = "windows"))]
    fn transient_failing_command() -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '429 Too Many Requests\\n' >&2; exit 7".to_string(),
        ]
    }

    #[cfg(target_os = "windows")]
    fn transient_failing_command() -> Vec<String> {
        vec![
            "cmd".to_string(),
            "/C".to_string(),
            "echo 429 Too Many Requests 1>&2 & exit 7".to_string(),
        ]
    }

    fn timezone_cron_task(id: &str, created_at_ms: u64) -> Job {
        let mut task = Job::new_cron_agent_chat(
            "0 9 * * *".to_string(),
            "scheduled prompt".to_string(),
            "scheduled prompt".to_string(),
            true,
            false,
            created_at_ms,
        );
        task.id = id.to_string();
        task.trigger = Trigger::Cron {
            expr: "0 9 * * *".to_string(),
            tz: Some("Asia/Kolkata".to_string()),
        };
        task.set_existing_chat(Some("chat-1".to_string()));
        task.set_mode(Some("agent".to_string()));
        task.auto_expire_after_ms = DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS;
        task
    }

    fn utc_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> u64 {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp_millis() as u64
    }

    async fn gcx_with_session(state: SessionState) -> (tempfile::TempDir, SharedGlobalContext) {
        let workspace = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let mut session = ChatSession::new("chat-1".to_string());
        session.set_runtime_state(state, None);
        gcx.chat_sessions
            .write()
            .await
            .insert("chat-1".to_string(), Arc::new(AMutex::new(session)));
        (workspace, gcx)
    }

    async fn gcx_with_session_and_config<F>(
        state: SessionState,
        configure: F,
    ) -> (tempfile::TempDir, SharedGlobalContext)
    where
        F: FnOnce(&mut crate::scheduler::types::SchedulerConfig),
    {
        let workspace = tempfile::tempdir().unwrap();
        let mut gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        Arc::get_mut(&mut gcx).unwrap().scheduler_config =
            crate::scheduler::types::test_scheduler_config_with(configure);
        let mut session = ChatSession::new("chat-1".to_string());
        session.set_runtime_state(state, None);
        gcx.chat_sessions
            .write()
            .await
            .insert("chat-1".to_string(), Arc::new(AMutex::new(session)));
        (workspace, gcx)
    }

    async fn gcx_with_closed_session() -> (tempfile::TempDir, SharedGlobalContext) {
        let (workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let session = session(&gcx).await;
        session.lock().await.close_event_channel();
        (workspace, gcx)
    }

    async fn session(gcx: &SharedGlobalContext) -> Arc<AMutex<ChatSession>> {
        gcx.chat_sessions
            .read()
            .await
            .get("chat-1")
            .cloned()
            .unwrap()
    }

    async fn isolated_session_ids(gcx: &SharedGlobalContext, task_id: &str) -> Vec<String> {
        let prefix = format!("cron_{task_id}_");
        let sessions = gcx.chat_sessions.read().await;
        let mut ids = sessions
            .keys()
            .filter(|id| id.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    async fn assert_isolated_prompt(gcx: &SharedGlobalContext, chat_id: &str, task_id: &str) {
        let session_arc = {
            let sessions = gcx.chat_sessions.read().await;
            sessions.get(chat_id).cloned().unwrap()
        };
        let deadline = TokioInstant::now() + Duration::from_secs(2);
        loop {
            {
                let session = session_arc.lock().await;
                let fire_event = session.messages.iter().any(|message| {
                    message.role == EVENT_ROLE
                        && message.extra["event"]["subkind"].as_str() == Some("cron_fire")
                        && message.extra["event"]["payload"]["task_id"].as_str() == Some(task_id)
                });
                let prompt_queued = session.pending_deliveries.iter().any(|delivery| {
                    delivery.messages.iter().any(|message| {
                        message.role == "user"
                            && message.content.content_text_only() == "scheduled prompt"
                    })
                });
                let prompt_added = session.messages.iter().any(|message| {
                    message.role == "user"
                        && message.content.content_text_only() == "scheduled prompt"
                });
                if fire_event && (prompt_queued || prompt_added) {
                    return;
                }
            }
            assert!(
                TokioInstant::now() < deadline,
                "isolated session must receive the scheduled prompt"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn event_message<'a>(
        session: &'a ChatSession,
        subkind: &str,
        task_id: &str,
    ) -> &'a crate::call_validation::ChatMessage {
        session
            .messages
            .iter()
            .find(|message| {
                message.role == EVENT_ROLE
                    && message.extra["event"]["subkind"].as_str() == Some(subkind)
                    && message.extra["event"]["payload"]["task_id"].as_str() == Some(task_id)
            })
            .unwrap()
    }

    async fn wait_for_fire(gcx: &SharedGlobalContext) {
        let deadline = TokioInstant::now() + Duration::from_secs(2);
        loop {
            {
                let session = session(gcx).await;
                let session = session.lock().await;
                let event_injected = session
                    .messages
                    .iter()
                    .any(|message| message.role == EVENT_ROLE);
                let prompt_queued = session.pending_deliveries.iter().any(|delivery| {
                    delivery.messages.iter().any(|message| {
                        message.role == "user"
                            && message.content.content_text_only() == "scheduled prompt"
                    })
                });
                let prompt_added = session.messages.iter().any(|message| {
                    message.role == "user"
                        && message.content.content_text_only() == "scheduled prompt"
                });
                if event_injected && (prompt_queued || prompt_added) {
                    return;
                }
            }
            assert!(
                TokioInstant::now() < deadline,
                "scheduled task did not fire"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn assert_no_fire(gcx: &SharedGlobalContext) -> impl std::future::Future<Output = ()> + '_ {
        async move {
            let session = session(gcx).await;
            let session = session.lock().await;
            assert!(session.messages.is_empty());
            assert!(session.command_queue.is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fires_due_task() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store.add(task("cron_fire_due", now)).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let handle = spawn(store.clone(), gcx.clone());

        tokio::time::advance(Duration::from_secs(2)).await;
        wait_for_fire(&gcx).await;
        gcx.shutdown_flag.store(true, Ordering::Relaxed);
        handle.abort();

        let session = session(&gcx).await;
        let session = session.lock().await;
        let event_message = session
            .messages
            .iter()
            .find(|message| message.role == EVENT_ROLE)
            .unwrap();
        assert_eq!(
            event_message.extra["event"]["payload"]["task_id"],
            json!("cron_fire_due")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn session_store_runner_fires_session_task() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store.add(task("cron_session_fire", now)).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let handle = spawn(store, gcx.clone());

        tokio::time::advance(Duration::from_secs(2)).await;
        wait_for_fire(&gcx).await;
        gcx.shutdown_flag.store(true, Ordering::Relaxed);
        handle.abort();

        let session = session(&gcx).await;
        let session = session.lock().await;
        let event_message = event_message(&session, "cron_fire", "cron_session_fire");
        assert_eq!(
            event_message.extra["event"]["payload"]["recurring"],
            json!(true)
        );
    }

    #[tokio::test]
    async fn one_shot_removed_after_normal_fire() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(one_shot_task("cron_one_shot_removed", now))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        assert!(store.list().await.is_empty());
        let session = session(&gcx).await;
        let session = session.lock().await;
        let fire_event = event_message(&session, "cron_fire", "cron_one_shot_removed");
        assert_eq!(fire_event.extra["event"]["payload"]["final"], json!(false));
        assert_eq!(
            fire_event.extra["event"]["payload"]["recurring"],
            json!(false)
        );
    }

    #[tokio::test]
    async fn once_trigger_fires_and_is_removed() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(once_trigger_task("once_trigger_due", now))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        assert!(store.list().await.is_empty());
        let session = session(&gcx).await;
        let session = session.lock().await;
        let fire_event = event_message(&session, "cron_fire", "once_trigger_due");
        assert_eq!(
            fire_event.extra["event"]["payload"]["recurring"],
            json!(false)
        );
    }

    #[tokio::test]
    async fn disabled_scheduler_does_not_fire_due_task() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store.add(due_task("cron_disabled", now)).await.unwrap();
        let (_workspace, gcx) = gcx_with_session_and_config(SessionState::Idle, |config| {
            config.enabled = false;
        })
        .await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("cron_disabled").await.unwrap();
        assert_eq!(stored.last_fired_at_ms, Some(now - 120_000));
        assert_eq!(stored.fire_count, 0);
        assert!(stored.recent_runs.is_empty());
        assert_no_fire(&gcx).await;
    }

    #[tokio::test]
    async fn one_shot_remains_when_enqueue_is_rejected() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(one_shot_task("cron_one_shot_queue_full", now))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        {
            let session_arc = session(&gcx).await;
            let mut session = session_arc.lock().await;
            for idx in 0..max_queue_size() {
                session.pending_deliveries.push_back(
                    refact_core::chat_types::PendingDelivery::with_id(
                        format!("queued-{idx}"),
                        vec![scheduled_prompt_message("queued".into())],
                        PushMode::Append,
                        "test",
                        false,
                    ),
                );
            }
        }
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("cron_one_shot_queue_full").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("deferred"));
        assert_eq!(stored.fire_count, 0);
        assert_eq!(stored.last_fired_at_ms, None);
        assert!(runner
            .deferred_until_ms
            .contains_key("cron_one_shot_queue_full"));
        assert!(stored
            .last_error
            .as_deref()
            .unwrap()
            .contains("queue is full"));
        assert!(!runner.task_is_due(&stored, now + 1));
        let arc = session(&gcx).await;
        {
            let mut session = arc.lock().await;
            assert!(session.messages.iter().all(|m| m.role != EVENT_ROLE));
            session.pending_deliveries.clear();
        }
        runner.fire_due_tasks(now + IDLE_DEFER_MS).await;
        assert!(store.get("cron_one_shot_queue_full").await.is_none());
        assert_eq!(
            arc.lock()
                .await
                .messages
                .iter()
                .filter(|m| m.role == "user")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn enqueue_rejection_does_not_mark_recurring_fired() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(due_task("cron_recurring_queue_full", now))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        {
            let session_arc = session(&gcx).await;
            let mut session = session_arc.lock().await;
            for idx in 0..max_queue_size() {
                session.pending_deliveries.push_back(
                    refact_core::chat_types::PendingDelivery::with_id(
                        format!("queued-{idx}"),
                        vec![scheduled_prompt_message("queued".into())],
                        PushMode::Append,
                        "test",
                        false,
                    ),
                );
            }
        }
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("cron_recurring_queue_full").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("deferred"));
        assert_eq!(stored.fire_count, 0);
        assert_eq!(stored.last_fired_at_ms, Some(now - 120_000));
        let session = session(&gcx).await;
        let session = session.lock().await;
        assert!(session.messages.iter().all(|m| m.role != EVENT_ROLE));
    }

    #[tokio::test]
    async fn interval_job_fires_on_cadence() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(interval_task("interval_due", now, 60_000))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("interval_due").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.last_fired_at_ms, Some(now));
        assert_eq!(stored.fire_count, 1);
        assert!(!runner.task_is_due(&stored, now + 59_999));
        assert!(runner.task_is_due(&stored, now + 60_000));
    }

    #[tokio::test]
    async fn overdue_recurring_job_fast_forwards_without_burst() {
        let now = now_ms();
        let every_ms = 60_000;
        let store = Arc::new(InMemoryCronStore::new());
        let mut task = interval_task("interval_overdue_fast_forward", now, every_ms);
        task.last_fired_at_ms = Some(now - 10 * every_ms);
        store.add(task).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("interval_overdue_fast_forward").await.unwrap();
        assert_eq!(stored.last_fired_at_ms, Some(now));
        assert_eq!(stored.fire_count, 0);
        assert!(stored.recent_runs.is_empty());
        assert_no_fire(&gcx).await;
        assert!(!runner.task_is_due(&stored, now + every_ms - 1));
        assert!(runner.task_is_due(&stored, now + every_ms));
    }

    #[tokio::test]
    async fn command_job_runs_and_delivers_stdout() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(command_due_task(
                "command_stdout",
                now,
                stdout_command("hello command"),
            ))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("command_stdout").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.fire_count, 1);
        let session = session(&gcx).await;
        let session = session.lock().await;
        let fire_event = event_message(&session, "cron_fire", "command_stdout");
        assert_eq!(
            fire_event.extra["event"]["payload"]["action_kind"],
            json!("command")
        );
        let output = session
            .messages
            .iter()
            .find(|message| message.role == "plain_text")
            .expect("stdout message");
        assert_eq!(
            output.content.content_text_only().trim_end(),
            "hello command"
        );
    }

    #[tokio::test]
    async fn command_job_empty_stdout_is_silent_but_fired() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(command_due_task("command_silent", now, empty_command()))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("command_silent").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.fire_count, 1);
        let session = session(&gcx).await;
        let session = session.lock().await;
        assert!(session.messages.is_empty());
        assert!(session.command_queue.is_empty());
    }

    #[tokio::test]
    async fn recurring_command_advances_before_fire_without_double_counting() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(command_due_task(
                "command_advance_before_fire",
                now,
                stdout_command("advanced"),
            ))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("command_advance_before_fire").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert!(stored
            .last_fired_at_ms
            .is_some_and(|last_fired_at_ms| last_fired_at_ms <= now));
        assert_eq!(stored.fire_count, 1);
        assert_eq!(stored.recent_runs.len(), 1);
        assert_eq!(stored.recent_runs[0].status, "fired");
        let session = session(&gcx).await;
        let session = session.lock().await;
        let fire_event = event_message(&session, "cron_fire", "command_advance_before_fire");
        assert_eq!(fire_event.extra["event"]["payload"]["fire_count"], json!(1));
    }

    #[tokio::test]
    async fn concurrency_cap_defers_over_limit_command_runs() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        for id in ["command_cap_a", "command_cap_b", "command_cap_c"] {
            store
                .add(command_due_task(id, now, slow_empty_command()))
                .await
                .unwrap();
        }
        let (_workspace, gcx) = gcx_with_session_and_config(SessionState::Idle, |config| {
            config.max_concurrent_runs = 2;
        })
        .await;
        let mut runner = CronRunner::new(store.clone(), gcx);

        runner.fire_due_tasks(now).await;

        let jobs = store.list().await;
        let fired = jobs
            .iter()
            .filter(|job| job.last_status.as_deref() == Some("fired"))
            .count();
        let pending = jobs
            .iter()
            .filter(|job| job.last_status.as_deref() != Some("fired"))
            .collect::<Vec<_>>();
        assert_eq!(fired, 2);
        assert_eq!(pending.len(), 1);
        assert_eq!(
            runner.deferred_until_ms.get(&pending[0].id),
            Some(&(now + IDLE_DEFER_MS))
        );
    }

    #[tokio::test]
    async fn command_job_nonzero_exit_records_error() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(command_due_task("command_error", now, failing_command()))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("command_error").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert_eq!(stored.fire_count, 1);
        assert_eq!(stored.retry_attempts, 0);
        assert_eq!(stored.trigger_at_ms, None);
        assert!(stored
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("Scheduled command failed"));
        let session = session(&gcx).await;
        let session = session.lock().await;
        let notice = event_message(&session, "system_notice", "command_error");
        assert_eq!(
            notice.extra["event"]["payload"]["action_kind"],
            json!("command")
        );
    }

    #[tokio::test]
    async fn transient_command_failure_retries_until_max_then_gives_up() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(one_shot_command_due_task(
                "command_transient_retry",
                now,
                transient_failing_command(),
            ))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx);

        runner.fire_due_tasks(now).await;
        let stored = store.get("command_transient_retry").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert_eq!(stored.fire_count, 1);
        assert_eq!(stored.retry_attempts, 1);
        assert_eq!(stored.trigger_at_ms, Some(now + 60_000));
        assert!(!runner.task_is_due(&stored, now + 59_999));

        runner.fire_due_tasks(now + 60_000).await;
        let stored = store.get("command_transient_retry").await.unwrap();
        assert_eq!(stored.fire_count, 2);
        assert_eq!(stored.retry_attempts, 2);
        assert_eq!(stored.trigger_at_ms, Some(now + 180_000));

        runner.fire_due_tasks(now + 180_000).await;
        let stored = store.get("command_transient_retry").await.unwrap();
        assert_eq!(stored.fire_count, 3);
        assert_eq!(stored.retry_attempts, 3);
        assert_eq!(stored.trigger_at_ms, Some(now + 480_000));

        runner.fire_due_tasks(now + 480_000).await;

        assert!(store.get("command_transient_retry").await.is_none());
    }

    #[tokio::test]
    async fn command_success_resets_retry_backoff() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut task = command_due_task("command_retry_reset", now, stdout_command("ok"));
        task.retry_attempts = 2;
        task.trigger_at_ms = Some(now);
        store.add(task).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx);

        runner.fire_due_tasks(now).await;

        let stored = store.get("command_retry_reset").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.retry_attempts, 0);
        assert_eq!(stored.trigger_at_ms, None);
    }

    #[tokio::test]
    async fn command_delivery_failure_records_separate_error_without_retry() {
        let router = axum::Router::new().route(
            "/hook",
            axum::routing::post(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(router.into_make_service());
        let server_task = tokio::spawn(server);
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut task = command_due_task("command_delivery_error", now, empty_command());
        task.delivery = Delivery::Webhook {
            url: format!("http://127.0.0.1:{port}/hook"),
            token: None,
        };
        store.add(task).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx);

        runner.fire_due_tasks(now).await;

        let stored = store.get("command_delivery_error").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.last_error, None);
        assert!(stored
            .last_delivery_error
            .as_deref()
            .unwrap_or_default()
            .contains("webhook delivery returned status"));
        assert_eq!(stored.retry_attempts, 0);
        assert_eq!(stored.trigger_at_ms, None);
        assert_eq!(stored.recent_runs.len(), 1);
        assert_eq!(stored.recent_runs[0].status, "fired");
        assert_eq!(stored.recent_runs[0].error, None);
        server_task.abort();
    }

    #[tokio::test]
    async fn timezone_cron_job_fires_at_local_time() {
        let now = utc_ms(2026, 1, 1, 3, 30);
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(timezone_cron_task("tz_cron_due", utc_ms(2026, 1, 1, 3, 29)))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());
        runner.jitter_cfg.recurring_frac = 0.0;

        runner.fire_due_tasks(now).await;

        let stored = store.get("tz_cron_due").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.last_fired_at_ms, Some(now));
        let session = session(&gcx).await;
        let session = session.lock().await;
        let fire_event = event_message(&session, "cron_fire", "tz_cron_due");
        assert_eq!(
            fire_event.extra["event"]["payload"]["task_id"],
            json!("tz_cron_due")
        );
    }

    #[tokio::test]
    async fn paused_job_never_fires_without_trigger_at() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut task = due_task("paused_cron", now);
        task.enabled = false;
        task.paused_at_ms = Some(now - 1_000);
        store.add(task).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("paused_cron").await.unwrap();
        assert_eq!(stored.fire_count, 0);
        assert!(stored.recent_runs.is_empty());
        assert_no_fire(&gcx).await;
    }

    #[tokio::test]
    async fn trigger_at_fires_paused_job_once_and_clears() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut task = due_task("run_now_paused", now);
        task.enabled = false;
        task.paused_at_ms = Some(now - 1_000);
        task.trigger_at_ms = Some(now);
        store.add(task).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("run_now_paused").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.fire_count, 1);
        assert_eq!(stored.trigger_at_ms, None);
        assert!(!runner.task_is_due(&stored, now + 60_000));
    }

    #[tokio::test]
    async fn trigger_at_overrides_existing_defer_once() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut task = due_task("run_now_deferred", now);
        task.trigger_at_ms = Some(now);
        store.add(task.clone()).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());
        runner
            .deferred_until_ms
            .insert(task.id.clone(), now + IDLE_DEFER_MS);

        runner.fire_due_tasks(now).await;

        let stored = store.get("run_now_deferred").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.trigger_at_ms, None);
        assert!(!runner.deferred_until_ms.contains_key("run_now_deferred"));
    }

    #[tokio::test]
    async fn record_run_caps_history_and_updates_status() {
        let now = now_ms();
        let store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let mut task = due_task("history_cap", now);
        task.fire_count = 7;
        task.recent_runs = (0..RECENT_RUNS_CAP)
            .map(|idx| CronRunRecord {
                at_ms: idx as u64,
                status: "old".to_string(),
                error: None,
            })
            .collect();
        store.add(task).await.unwrap();

        record_run(&store, "history_cap", "fired", None, now, RECENT_RUNS_CAP)
            .await
            .unwrap();
        record_run(
            &store,
            "history_cap",
            "deferred",
            Some("busy".to_string()),
            now + 1,
            RECENT_RUNS_CAP,
        )
        .await
        .unwrap();

        let stored = store.get("history_cap").await.unwrap();
        assert_eq!(stored.recent_runs.len(), RECENT_RUNS_CAP);
        assert_eq!(stored.recent_runs[0].at_ms, 2);
        assert_eq!(stored.recent_runs[RECENT_RUNS_CAP - 1].at_ms, now + 1);
        assert_eq!(stored.last_status.as_deref(), Some("deferred"));
        assert_eq!(stored.last_error.as_deref(), Some("busy"));
        assert_eq!(stored.last_fired_at_ms, Some(now));
        assert_eq!(stored.fire_count, 8);
    }

    #[tokio::test]
    async fn runner_uses_configurable_recent_run_cap() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(due_task("history_config_cap", now))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session_and_config(SessionState::Idle, |config| {
            config.recent_runs_cap = 1;
        })
        .await;
        let runner = CronRunner::new(store.clone(), gcx);

        runner
            .record_run("history_config_cap", "fired", None, now)
            .await
            .unwrap();
        runner
            .record_run("history_config_cap", "deferred", None, now + 1)
            .await
            .unwrap();

        let stored = store.get("history_config_cap").await.unwrap();
        assert_eq!(stored.recent_runs.len(), 1);
        assert_eq!(stored.recent_runs[0].at_ms, now + 1);
        assert_eq!(stored.recent_runs[0].status, "deferred");
    }

    #[tokio::test]
    async fn due_task_without_chat_id_does_not_spin() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut one_shot = one_shot_task("cron_no_chat_one_shot", now);
        one_shot.set_existing_chat(None);
        store.add(one_shot).await.unwrap();
        let mut recurring = due_task("cron_no_chat_recurring", now);
        recurring.set_existing_chat(None);
        store.add(recurring).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.list().await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, "cron_no_chat_recurring");
        assert_eq!(
            runner.deferred_until_ms["cron_no_chat_recurring"],
            now + INVALID_TARGET_DEFER_MS
        );
        assert!(!runner.task_is_due(&stored[0], now + INVALID_TARGET_DEFER_MS - 1));
    }

    #[tokio::test]
    async fn missing_chat_one_shot_removed_or_deferred() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut one_shot = one_shot_task("cron_missing_chat_one_shot", now);
        one_shot.set_existing_chat(Some("missing-chat".to_string()));
        store.add(one_shot).await.unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut runner = CronRunner::new(store.clone(), gcx);

        runner.fire_due_tasks(now).await;

        assert!(store.list().await.is_empty());
        assert!(runner.deferred_until_ms.is_empty());
    }

    #[tokio::test]
    async fn closed_chat_task_does_not_hot_loop() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(due_task("cron_closed_chat_recurring", now))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_closed_session().await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.list().await.into_iter().next().unwrap();
        assert_eq!(
            runner.deferred_until_ms["cron_closed_chat_recurring"],
            now + INVALID_TARGET_DEFER_MS
        );
        assert!(!runner.task_is_due(&stored, now + INVALID_TARGET_DEFER_MS - 1));
        assert!(!chat_is_idle(&gcx, "chat-1").await);
        let session = session(&gcx).await;
        let session = session.lock().await;
        assert!(session.messages.is_empty());
        assert!(session.command_queue.is_empty());
    }

    #[tokio::test]
    async fn spawn_from_active_project_starts_session_runner_without_project() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        gcx.shutdown_flag.store(true, Ordering::Relaxed);

        let handles = spawn_from_active_project(gcx).await;

        assert_eq!(handles.len(), 1);
        for handle in handles {
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn default_push_accepts_while_generating() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store.add(task("cron_defer", now)).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Generating).await;
        session(&gcx).await.lock().await.start_stream().unwrap();
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.list().await.into_iter().next().unwrap();
        assert_eq!(stored.last_fired_at_ms, Some(now));
        assert_eq!(stored.fire_count, 1);
        assert!(!runner.deferred_until_ms.contains_key("cron_defer"));
        let session = session(&gcx).await;
        let session = session.lock().await;
        assert!(session.messages.is_empty());
        assert_eq!(session.pending_deliveries.len(), 1);
        assert_eq!(session.pending_deliveries[0].push, PushMode::Append);
    }

    #[tokio::test]
    async fn isolated_due_job_creates_fresh_session_and_skips_idle_gate() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(isolated_due_task("cron_isolated_due", now))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Generating).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.get("cron_isolated_due").await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert!(stored
            .last_fired_at_ms
            .is_some_and(|last_fired_at_ms| last_fired_at_ms <= now));
        assert_eq!(stored.fire_count, 1);
        let ids = isolated_session_ids(&gcx, "cron_isolated_due").await;
        assert_eq!(ids, vec!["cron_cron_isolated_due_0".to_string()]);
        assert_isolated_prompt(&gcx, &ids[0], "cron_isolated_due").await;
        let existing = session(&gcx).await;
        let existing = existing.lock().await;
        assert!(existing.messages.is_empty());
        assert!(existing.command_queue.is_empty());
    }

    #[tokio::test]
    async fn recurring_isolated_job_creates_distinct_session_per_fire() {
        let now = now_ms();
        let every_ms = 60_000;
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(isolated_interval_task(
                "cron_isolated_recurring",
                now,
                every_ms,
            ))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;
        runner.fire_due_tasks(now + every_ms).await;

        let ids = isolated_session_ids(&gcx, "cron_isolated_recurring").await;
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"cron_cron_isolated_recurring_0".to_string()));
        assert!(ids.contains(&"cron_cron_isolated_recurring_1".to_string()));
        assert_ne!(ids[0], ids[1]);
        let stored = store.get("cron_isolated_recurring").await.unwrap();
        assert_eq!(stored.fire_count, 2);
    }

    #[tokio::test]
    async fn recurring_auto_expires_after_horizon() {
        let now = now_ms();
        let mut task = task("cron_expire", now);
        task.created_at_ms = now - DAY_MS;
        task.auto_expire_after_ms = DAY_MS;

        assert!(!task_final_after_fire(&task, now));
        task.created_at_ms -= 1;
        assert!(task_final_after_fire(&task, now));
        task.recurring = false;
        assert!(!task_final_after_fire(&task, now));
    }

    #[tokio::test]
    async fn webhook_trigger_with_zero_auto_expire_survives_fire() {
        let now = now_ms();
        let store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut task = command_due_task("cron_webhook_no_expire", now, empty_command());
        task.set_trigger(Trigger::Webhook {
            hook_id: "deploy".to_string(),
        });
        task.recurring = true;
        task.created_at_ms = now - 2 * DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS;
        task.auto_expire_after_ms = 0;
        task.trigger_at_ms = Some(now);
        store.add(task.clone()).await.unwrap();
        let mut runner = CronRunner::new(store.clone(), gcx);

        assert!(!task_final_after_fire(&task, now));
        assert!(runner.handle_due_task(task.clone(), now).await);

        let stored = store.get(&task.id).await.unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("fired"));
        assert_eq!(stored.auto_expire_after_ms, 0);
    }

    #[tokio::test]
    async fn final_fire_event_payload_has_final_true() {
        let now = now_ms();
        let message = cron_fire_message(&expired_task("cron_final", now), true);

        assert_eq!(message.extra["event"]["subkind"], json!("cron_fire"));
        assert_eq!(
            message.extra["event"]["payload"]["task_id"],
            json!("cron_final")
        );
        assert_eq!(message.extra["event"]["payload"]["final"], json!(true));
    }

    #[tokio::test]
    async fn expired_task_removed_from_store() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store
            .add(expired_task("cron_expire_removed", now))
            .await
            .unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        assert!(store.list().await.is_empty());
        let session = session(&gcx).await;
        let session = session.lock().await;
        let fire_event = event_message(&session, "cron_fire", "cron_expire_removed");
        assert_eq!(fire_event.extra["event"]["payload"]["final"], json!(true));
        let notice_event = event_message(&session, "system_notice", "cron_expire_removed");
        assert_eq!(
            notice_event.extra["event"]["payload"],
            json!({"task_id": "cron_expire_removed", "reason": "auto_expired"})
        );
        assert_eq!(
            notice_event.content.content_text_only(),
            "Recurring task 'scheduled prompt' auto-expired after 2d"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_flag_cancels_runner() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        store.add(task("cron_shutdown", now)).await.unwrap();
        let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
        gcx.shutdown_flag.store(true, Ordering::Relaxed);

        let handle = spawn(store, gcx);
        tokio::time::advance(Duration::from_millis(200)).await;

        assert!(handle.is_finished());
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn durable_one_shot_missing_trajectory_is_deferred_not_fired() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut one_shot = one_shot_task("cron_durable_no_traj", now);
        one_shot.set_existing_chat(Some("missing-traj-chat".to_string()));
        one_shot.durable = true;
        store.add(one_shot).await.unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        assert_eq!(
            store.list().await.len(),
            1,
            "durable one-shot must remain in store when no trajectory exists"
        );
        assert!(
            runner
                .deferred_until_ms
                .contains_key("cron_durable_no_traj"),
            "durable one-shot must be deferred when no trajectory exists"
        );
        let sessions = gcx.chat_sessions.read().await;
        assert!(
            !sessions.contains_key("missing-traj-chat"),
            "no empty session should be created for a durable task with no trajectory"
        );
    }

    #[tokio::test]
    async fn durable_recurring_missing_trajectory_does_not_hot_loop() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut recurring = due_task("cron_durable_recurring_no_traj", now);
        recurring.set_existing_chat(Some("missing-traj-recurring".to_string()));
        recurring.durable = true;
        recurring.trigger_at_ms = Some(now);
        store.add(recurring).await.unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());

        runner.fire_due_tasks(now).await;

        let stored = store.list().await;
        assert_eq!(stored.len(), 1, "recurring task should remain in store");
        assert_eq!(
            stored[0].fire_count, 0,
            "task must not fire when no trajectory exists"
        );
        assert!(
            runner
                .deferred_until_ms
                .contains_key("cron_durable_recurring_no_traj"),
            "task must be deferred when no trajectory exists"
        );
        let sessions = gcx.chat_sessions.read().await;
        assert!(
            !sessions.contains_key("missing-traj-recurring"),
            "no empty session should be created for a durable task with no trajectory"
        );
    }

    #[tokio::test]
    async fn durable_one_shot_catch_up_fires_with_missed_when_session_available() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut one_shot = one_shot_task("cron_catch_up_missed", now);
        one_shot.set_existing_chat(Some("catch-up-chat".to_string()));
        one_shot.durable = true;
        store.add(one_shot).await.unwrap();

        let workspace = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let mut session = crate::chat::types::ChatSession::new("catch-up-chat".to_string());
        session.set_runtime_state(SessionState::Idle, None);
        gcx.chat_sessions
            .write()
            .await
            .insert("catch-up-chat".to_string(), Arc::new(AMutex::new(session)));

        let mut runner = CronRunner::new(store.clone(), gcx.clone());
        runner.catch_up().await;

        assert!(
            store.list().await.is_empty(),
            "missed durable one-shot should be removed after catch-up"
        );
        let sessions = gcx.chat_sessions.read().await;
        let session_arc = sessions.get("catch-up-chat").unwrap();
        let session = session_arc.lock().await;
        let fire_event = session.messages.iter().find(|m| {
            m.role == EVENT_ROLE && m.extra["event"]["subkind"].as_str() == Some("cron_fire")
        });
        assert!(
            fire_event.is_some(),
            "catch-up should inject cron_fire event"
        );
        assert_eq!(
            fire_event.unwrap().extra["event"]["payload"]["missed"],
            json!(true),
            "catch-up fire must carry missed=true"
        );
    }

    #[tokio::test]
    async fn durable_one_shot_catch_up_skips_when_no_trajectory() {
        let now = now_ms();
        let store = Arc::new(InMemoryCronStore::new());
        let mut one_shot = one_shot_task("cron_catch_up_no_traj", now);
        one_shot.set_existing_chat(Some("no-traj-catch-up-chat".to_string()));
        one_shot.durable = true;
        store.add(one_shot).await.unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut runner = CronRunner::new(store.clone(), gcx.clone());
        runner.catch_up().await;

        assert_eq!(
            store.list().await.len(),
            1,
            "task should remain when no trajectory exists during catch-up"
        );
        let sessions = gcx.chat_sessions.read().await;
        assert!(
            !sessions.contains_key("no-traj-catch-up-chat"),
            "no empty session should be created during catch-up when no trajectory exists"
        );
    }

    #[tokio::test]
    async fn mode_and_model_land_atomically_at_each_push_boundary() {
        for push in [PushMode::Append, PushMode::WhenIdle, PushMode::Preempt] {
            let now = now_ms();
            let store = Arc::new(InMemoryCronStore::new());
            let mut job = due_task("boundary", now);
            job.push = push;
            job.set_mode(Some("explore".into()));
            set_job_model(&mut job, Some("scheduled-model".into()));
            store.add(job.clone()).await.unwrap();
            let (_workspace, gcx) = gcx_with_session(SessionState::Idle).await;
            let arc = session(&gcx).await;
            {
                let mut session = arc.lock().await;
                session.thread.model = "original".into();
                session.start_stream().unwrap();
                session.turn_depth = 1;
            }
            let mut runner = CronRunner::new(store.clone(), gcx.clone());
            runner.handle_due_task(job.clone(), now).await;
            assert_eq!(store.get(&job.id).await.unwrap().fire_count, 1);
            // Same fire retried at another wall-clock instant is accepted once.
            assert!(runner.fire(&job, false, now + 500).await.unwrap());
            let mut session = arc.lock().await;
            if push != PushMode::Preempt {
                assert_eq!(session.thread.model, "original");
                assert_eq!(session.pending_deliveries.len(), 1);
                assert_eq!(session.pending_deliveries[0].messages.len(), 2);
                session.finish_stream(Some("stop".into()));
                session.drain_pending_deliveries();
                if push == PushMode::WhenIdle {
                    assert_eq!(session.thread.model, "original");
                    session.turn_depth = 0;
                    session.drain_pending_deliveries();
                }
            }
            assert_eq!(session.thread.model, "scheduled-model");
            assert_eq!(session.thread.mode, "explore");
            let events: Vec<_> = session
                .messages
                .iter()
                .filter(|m| {
                    m.role == "user"
                        || m.extra
                            .get("event")
                            .is_some_and(|event| event["subkind"] == "cron_fire")
                })
                .collect();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].extra["event"]["subkind"], "cron_fire");
            assert_eq!(events[1].role, "user");
            assert!(session.command_queue.is_empty());
        }
    }
}
