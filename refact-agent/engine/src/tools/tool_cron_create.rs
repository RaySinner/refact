pub use refact_tools::tools::tool_cron_create::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::schedule::scheduler_timezone;
#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::PathBuf;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::{TimeZone, Utc};
#[cfg(test)]
#[allow(unused_imports)]
use chrono_tz::Tz;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::{Mutex as AMutex, Notify};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::{event, EventSubkind};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::files_correction::get_active_project_path_for_services;
#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::schedule::parse_schedule;
#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::{
    delivery_kind, human_schedule_for_trigger as scheduler_human_schedule_for_trigger, next_run_ms,
    push_from_value, push_schema, session_cron_store, Action, AgentTarget,
    CronStore, Delivery, Job, JsonFileCronStore, Trigger, delivery_from_value,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::PushMode;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use refact_scheduler_core::{scheduled_tasks_path, InMemoryCronStore};

    fn fixed_now_ms() -> u64 {
        Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis() as u64
    }

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn input(items: &[(&str, Value)]) -> CronCreateInput {
        input_from_args(&args(items)).unwrap()
    }

    fn default_input() -> CronCreateInput {
        input(&[
            ("cron", json!("*/5 * * * *")),
            ("prompt", json!("Check the build")),
            ("description", json!("Check build")),
        ])
    }

    fn tool_output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected simple text"),
            },
            _ => panic!("expected chat message"),
        }
    }

    fn runtime(
        session_store: Arc<dyn CronStore>,
        durable_store: Option<Arc<dyn CronStore>>,
        change_notify: Arc<Notify>,
    ) -> CronCreateRuntime {
        CronCreateRuntime {
            session_store,
            durable_store,
            change_notify,
            now_ms: fixed_now_ms(),
            timezone: chrono_tz::UTC,
            chat_id: Some("chat-1".to_string()),
            mode: Some("agent".to_string()),
            model: Some("model-1".to_string()),
        }
    }

    fn test_task(id: &str) -> Job {
        let mut task = Job::new_cron_agent_chat(
            "*/5 * * * *".to_string(),
            "Check".to_string(),
            "Check".to_string(),
            true,
            false,
            fixed_now_ms(),
        );
        task.id = id.to_string();
        task
    }

    #[tokio::test]
    async fn valid_recurring_creates() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let app = refact_runtime_api::app_state_from_gcx(gcx).await;
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                "cron-chat".to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ));
        let before = session_cron_store().list().await.len();
        let change_notify = refact_scheduler_core::runner_change_notify();
        let notified = change_notify.notified();
        let mut tool = ToolCronCreate {
            config_path: String::new(),
        };
        let output = tool_output_text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("cron", json!("*/5 * * * *")),
                    ("prompt", json!("Check the build")),
                    ("description", json!("Check build")),
                    ("durable", json!(false)),
                ]),
            )
            .await
            .unwrap(),
        );
        let output: Value = serde_json::from_str(&output).unwrap();
        let after = session_cron_store().list().await;
        let created = after
            .iter()
            .find(|task| task.id == output["id"].as_str().unwrap())
            .unwrap();

        assert_eq!(after.len(), before + 1);
        assert!(created.id.starts_with("cron_"));
        assert_eq!(output["human_schedule"], json!("every 5 minutes"));
        assert!(created.recurring);
        assert!(!created.durable);
        assert_eq!(created.chat_id(), Some("cron-chat"));
        tokio::time::timeout(std::time::Duration::from_secs(1), notified)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn invalid_cron_rejected() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let err = create_cron_job(
            input(&[
                ("cron", json!("* * * *")),
                ("prompt", json!("Check")),
                ("description", json!("Check")),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap_err();

        assert!(err.contains("Invalid cron expression"));
    }

    #[tokio::test]
    async fn every_creates_interval() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            input(&[
                ("every", json!("30m")),
                ("prompt", json!("Check")),
                ("description", json!("Check")),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.task.trigger,
            Trigger::Interval {
                every_ms: 30 * 60_000
            }
        );
        assert!(outcome.task.recurring);
        assert_eq!(outcome.human_schedule, "every 30 minutes");
    }

    #[tokio::test]
    async fn at_creates_once() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            input(&[
                ("at", json!("in 30m")),
                ("prompt", json!("Check")),
                ("description", json!("Check")),
                ("recurring", json!(true)),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.task.trigger,
            Trigger::Once {
                at_ms: fixed_now_ms() + 30 * 60_000
            }
        );
        assert!(!outcome.task.recurring);
        assert!(outcome.human_schedule.starts_with("once at "));
    }

    #[tokio::test]
    async fn webhook_trigger_creates_dormant_job() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            input(&[
                ("trigger", json!({"kind":"webhook", "hook_id":"deploy"})),
                ("command", json!("printf hi")),
                ("delivery", json!("none")),
                ("description", json!("Deploy hook")),
            ]),
            runtime(session_store.clone(), None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.task.trigger,
            Trigger::Webhook {
                hook_id: "deploy".to_string()
            }
        );
        assert_eq!(outcome.human_schedule, "webhook deploy");
        assert_eq!(
            next_run_ms(&outcome.task, fixed_now_ms(), chrono_tz::UTC),
            None
        );
        let jobs = session_store.jobs_by_hook_id("deploy").await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, outcome.task.id);
    }

    #[tokio::test]
    async fn bad_schedule_combo_rejected() {
        let err = input_from_args(&args(&[
            ("cron", json!("*/5 * * * *")),
            ("every", json!("30m")),
            ("prompt", json!("Check")),
            ("description", json!("Check")),
        ]))
        .and_then(|input| {
            parse_create_schedule(&input, fixed_now_ms())?;
            Ok(())
        })
        .unwrap_err();

        assert!(err.contains("exactly one of cron, every, or at must be set"));
    }

    #[tokio::test]
    async fn cron_only_is_unchanged() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            default_input(),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.task.trigger,
            Trigger::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            }
        );
        assert!(outcome.task.recurring);
        assert_eq!(outcome.human_schedule, "every 5 minutes");
        assert!(!outcome.task.is_isolated());
        assert_eq!(outcome.task.chat_id(), Some("chat-1"));
    }

    #[tokio::test]
    async fn isolated_true_creates_isolated_target() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            input(&[
                ("cron", json!("*/5 * * * *")),
                ("prompt", json!("Check")),
                ("description", json!("Check")),
                ("isolated", json!(true)),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert!(outcome.task.is_isolated());
        assert_eq!(outcome.task.chat_id(), None);
        assert_eq!(outcome.task.mode(), Some("agent"));
        assert_eq!(job_model(&outcome.task), Some("model-1"));
    }

    #[tokio::test]
    async fn isolated_default_creates_existing_chat_target() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            default_input(),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert!(!outcome.task.is_isolated());
        assert_eq!(outcome.task.chat_id(), Some("chat-1"));
        assert_eq!(outcome.task.mode(), Some("agent"));
        assert_eq!(job_model(&outcome.task), None);
    }

    #[tokio::test]
    async fn command_string_creates_command_action() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            input(&[
                ("cron", json!("*/5 * * * *")),
                ("command", json!("printf 'hi frog'")),
                ("description", json!("Print frog")),
                ("cwd", json!(".")),
                ("timeout_secs", json!(12)),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert_eq!(outcome.task.action_kind(), "command");
        match outcome.task.action {
            Action::Command {
                argv,
                target,
                cwd,
                timeout_secs,
                ..
            } => {
                assert_eq!(argv, vec!["printf".to_string(), "hi frog".to_string()]);
                assert_eq!(
                    target,
                    AgentTarget::ExistingChat {
                        chat_id: "chat-1".to_string()
                    }
                );
                assert_eq!(cwd.as_deref(), Some("."));
                assert_eq!(timeout_secs, Some(12));
            }
            _ => panic!("expected command action"),
        }
    }

    #[tokio::test]
    async fn webhook_delivery_creates_command_job() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            input(&[
                ("cron", json!("*/5 * * * *")),
                ("command", json!("printf hi")),
                ("description", json!("Print frog")),
                (
                    "delivery",
                    json!({"kind": "webhook", "url": "http://127.0.0.1/hook", "token": "secret"}),
                ),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert_eq!(outcome.task.action_kind(), "command");
        assert_eq!(
            outcome.task.delivery,
            Delivery::Webhook {
                url: "http://127.0.0.1/hook".to_string(),
                token: Some("secret".to_string()),
            }
        );
        let output = delivery_output(&outcome.task.delivery);
        assert_eq!(output["has_token"], json!(true));
        assert_eq!(output.get("token"), None);
    }

    #[tokio::test]
    async fn notifier_delivery_creates_command_job() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            input(&[
                ("cron", json!("*/5 * * * *")),
                ("command", json!("printf hi")),
                ("description", json!("Print frog")),
                (
                    "delivery",
                    json!({"kind": "notifier", "integration_id": "notifier_telegram", "target": "chat-1"}),
                ),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        assert_eq!(outcome.task.action_kind(), "command");
        assert_eq!(
            outcome.task.delivery,
            Delivery::Notifier {
                integration_id: "notifier_telegram".to_string(),
                target: Some("chat-1".to_string()),
            }
        );
        let output = delivery_output(&outcome.task.delivery);
        assert_eq!(output["kind"], json!("notifier"));
        assert_eq!(output["integration_id"], json!("notifier_telegram"));
        assert_eq!(output["target"], json!("chat-1"));
    }

    #[tokio::test]
    async fn webhook_delivery_rejects_prompt_job() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let err = create_cron_job(
            input(&[
                ("cron", json!("*/5 * * * *")),
                ("prompt", json!("Check")),
                ("description", json!("Check")),
                (
                    "delivery",
                    json!({"kind": "webhook", "url": "http://127.0.0.1/hook"}),
                ),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap_err();

        assert_eq!(err, "non-chat delivery is only supported for command jobs");
    }

    #[tokio::test]
    async fn notifier_delivery_rejects_prompt_job() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let err = create_cron_job(
            input(&[
                ("cron", json!("*/5 * * * *")),
                ("prompt", json!("Check")),
                ("description", json!("Check")),
                (
                    "delivery",
                    json!({"kind": "notifier", "integration_id": "notifier_telegram"}),
                ),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap_err();

        assert_eq!(err, "non-chat delivery is only supported for command jobs");
    }

    #[tokio::test]
    async fn command_argv_creates_command_action() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let outcome = create_cron_job(
            input(&[
                ("cron", json!("*/5 * * * *")),
                ("command_argv", json!(["printf", "hi frog"])),
                ("description", json!("Print frog")),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap();

        match outcome.task.action {
            Action::Command { argv, .. } => {
                assert_eq!(argv, vec!["printf".to_string(), "hi frog".to_string()]);
            }
            _ => panic!("expected command action"),
        }
    }

    #[test]
    fn prompt_and_command_are_rejected() {
        let err = input_from_args(&args(&[
            ("cron", json!("*/5 * * * *")),
            ("prompt", json!("Check")),
            ("command", json!("printf hi")),
            ("description", json!("Check")),
        ]))
        .unwrap_err();

        assert_eq!(err, "exactly one action is allowed: prompt XOR command");
    }

    #[test]
    fn command_and_command_argv_are_rejected() {
        let err = input_from_args(&args(&[
            ("cron", json!("*/5 * * * *")),
            ("command", json!("printf hi")),
            ("command_argv", json!(["printf", "hi"])),
            ("description", json!("Check")),
        ]))
        .unwrap_err();

        assert_eq!(err, "exactly one of `command` or `command_argv` is allowed");
    }

    #[tokio::test]
    async fn no_match_in_year_rejected() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let err = create_cron_job(
            input(&[
                ("cron", json!("0 0 29 2 *")),
                ("prompt", json!("Leap check")),
                ("description", json!("Leap check")),
            ]),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap_err();

        assert_eq!(err, "matches no calendar date in the next year");
    }

    #[tokio::test]
    async fn cap_enforced() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        for idx in 0..MAX_CRON_JOBS {
            session_store
                .add(test_task(&format!("cron_{idx}")))
                .await
                .unwrap();
        }

        let err = create_cron_job(
            default_input(),
            runtime(session_store, None, Arc::new(Notify::new())),
        )
        .await
        .unwrap_err();

        assert_eq!(err, "Too many scheduled jobs (max 50). Cancel one first.");
    }

    #[tokio::test]
    async fn durable_writes_to_disk() {
        let temp = tempfile::tempdir().unwrap();
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let durable_store: Arc<dyn CronStore> =
            Arc::new(JsonFileCronStore::new(temp.path()).unwrap());
        let outcome = create_cron_job(
            input(&[
                ("cron", json!("0 9 * * 1-5")),
                ("prompt", json!("Standup prep")),
                ("description", json!("Standup prep")),
                ("durable", json!(true)),
            ]),
            runtime(
                session_store.clone(),
                Some(durable_store),
                Arc::new(Notify::new()),
            ),
        )
        .await
        .unwrap();

        assert!(outcome.task.durable);
        assert!(session_store.list().await.is_empty());
        assert!(scheduled_tasks_path(temp.path()).is_file());
        let reloaded = JsonFileCronStore::new(temp.path()).unwrap();
        assert_eq!(reloaded.list().await, vec![outcome.task]);
    }

    #[tokio::test]
    async fn omitted_durable_defaults_to_project_store() {
        let temp = tempfile::tempdir().unwrap();
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let durable_store: Arc<dyn CronStore> =
            Arc::new(JsonFileCronStore::new(temp.path()).unwrap());
        let outcome = create_cron_job(
            input(&[
                ("cron", json!("0 10 * * *")),
                ("prompt", json!("Daily check")),
                ("description", json!("Daily check")),
            ]),
            runtime(
                session_store.clone(),
                Some(durable_store),
                Arc::new(Notify::new()),
            ),
        )
        .await
        .unwrap();

        assert!(outcome.task.durable);
        assert!(session_store.list().await.is_empty());
        let reloaded = JsonFileCronStore::new(temp.path()).unwrap();
        assert_eq!(reloaded.list().await, vec![outcome.task]);
    }

    #[tokio::test]
    async fn explicit_session_only_overrides_project_store_default() {
        let temp = tempfile::tempdir().unwrap();
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let durable_store: Arc<dyn CronStore> =
            Arc::new(JsonFileCronStore::new(temp.path()).unwrap());
        let outcome = create_cron_job(
            input(&[
                ("cron", json!("0 11 * * *")),
                ("prompt", json!("Session check")),
                ("description", json!("Session check")),
                ("durable", json!(false)),
            ]),
            runtime(
                session_store.clone(),
                Some(durable_store),
                Arc::new(Notify::new()),
            ),
        )
        .await
        .unwrap();

        assert!(!outcome.task.durable);
        assert_eq!(session_store.list().await, vec![outcome.task]);
        assert!(JsonFileCronStore::new(temp.path())
            .unwrap()
            .list()
            .await
            .is_empty());
    }
}
