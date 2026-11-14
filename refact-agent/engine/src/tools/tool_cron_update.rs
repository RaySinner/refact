pub use refact_tools::tools::tool_cron_update::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::scheduler::active_durable_cron_store_for_services;
#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
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
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::schedule::parse_schedule;
#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::{
 push_from_value, push_schema, session_cron_store,
    CronStore, Job, Trigger,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::PushMode;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_cron_create::human_schedule_for_trigger;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_scheduler_core::{
        InMemoryCronStore, JsonFileCronStore, DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS,
    };

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn test_task(id: &str, durable: bool) -> Job {
        let mut task = Job::new_cron_agent_chat(
            "*/5 * * * *".to_string(),
            "Check".to_string(),
            "Check".to_string(),
            true,
            durable,
            1_000,
        );
        task.id = id.to_string();
        task.set_existing_chat(Some("chat".to_string()));
        task.set_mode(Some("agent".to_string()));
        task
    }

    async fn test_ccx() -> Arc<AMutex<AtCommandsContext>> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = crate::at_commands::at_commands::at_commands_new_with_abort(
            refact_runtime_api::app_state_from_gcx(gcx).await,
            4096,
            20,
            false,
            Vec::new(),
            "cron-update-test".to_string(),
            None,
            "model".to_string(),
            None,
            None,
            None,
        )
        .await;
        Arc::new(AMutex::new(ccx))
    }

    async fn run_tool(
        tool: &mut ToolCronUpdate,
        ccx: Arc<AMutex<AtCommandsContext>>,
        items: &[(&str, Value)],
    ) -> Value {
        let (_, contexts) = tool
            .tool_execute(ccx, &"call".to_string(), &args(items))
            .await
            .unwrap();
        let ContextEnum::ChatMessage(message) = contexts.into_iter().next().unwrap() else {
            panic!("expected chat message")
        };
        let ChatContent::SimpleText(text) = message.content else {
            panic!("expected simple text")
        };
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn update_preserves_push_when_omitted_and_accepts_explicit_modes() {
        let mut job = test_task("push", false);
        job.push = PushMode::WhenIdle;
        let input = CronUpdateInput::from_args(&args(&[
            ("id", json!("push")),
            ("description", json!("changed")),
        ]))
        .unwrap();
        apply_update(&mut job, input, 1000).unwrap();
        assert_eq!(job.push, PushMode::WhenIdle);
        for (wire, mode) in [
            ("preempt", PushMode::Preempt),
            ("append", PushMode::Append),
            ("when_idle", PushMode::WhenIdle),
        ] {
            let input =
                CronUpdateInput::from_args(&args(&[("id", json!("push")), ("push", json!(wire))]))
                    .unwrap();
            apply_update(&mut job, input, 1000).unwrap();
            assert_eq!(job.push, mode);
        }
    }

    #[tokio::test]
    async fn cron_update_edits_schedule_to_interval() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(test_task("cron_update_interval", false))
            .await
            .unwrap();
        let mut tool =
            ToolCronUpdate::with_stores(String::new(), session_store.clone(), None, 2_000);
        let ccx = test_ccx().await;

        let result = run_tool(
            &mut tool,
            ccx,
            &[
                ("id", json!("cron_update_interval")),
                ("every", json!("30m")),
            ],
        )
        .await;

        let stored = session_store.get("cron_update_interval").await.unwrap();
        assert_eq!(
            stored.trigger,
            Trigger::Interval {
                every_ms: 30 * 60_000
            }
        );
        assert_eq!(result["human_schedule"], json!("every 30 minutes"));
        assert_eq!(result["updated"], json!(true));
    }

    #[tokio::test]
    async fn cron_update_non_time_trigger_clears_auto_expire() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let mut task = test_task("cron_update_once", false);
        task.auto_expire_after_ms = DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS;
        session_store.add(task).await.unwrap();
        let mut tool =
            ToolCronUpdate::with_stores(String::new(), session_store.clone(), None, 2_000);
        let ccx = test_ccx().await;

        run_tool(
            &mut tool,
            ccx,
            &[("id", json!("cron_update_once")), ("at", json!("in 30m"))],
        )
        .await;

        let stored = session_store.get("cron_update_once").await.unwrap();
        assert!(!stored.recurring);
        assert_eq!(stored.auto_expire_after_ms, 0);
    }

    #[tokio::test]
    async fn cron_update_pauses_and_sets_run_now() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(test_task("cron_update_pause", false))
            .await
            .unwrap();
        let mut tool =
            ToolCronUpdate::with_stores(String::new(), session_store.clone(), None, 5_000);
        let ccx = test_ccx().await;

        run_tool(
            &mut tool,
            ccx,
            &[
                ("id", json!("cron_update_pause")),
                ("enabled", json!(false)),
                ("run_now", json!(true)),
            ],
        )
        .await;

        let stored = session_store.get("cron_update_pause").await.unwrap();
        assert!(!stored.enabled);
        assert_eq!(stored.paused_at_ms, Some(5_000));
        assert_eq!(stored.trigger_at_ms, Some(5_000));
    }

    #[tokio::test]
    async fn cron_update_resumes() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let mut task = test_task("cron_update_resume", false);
        task.enabled = false;
        task.paused_at_ms = Some(1_000);
        session_store.add(task).await.unwrap();
        let mut tool =
            ToolCronUpdate::with_stores(String::new(), session_store.clone(), None, 5_000);
        let ccx = test_ccx().await;

        run_tool(
            &mut tool,
            ccx,
            &[
                ("id", json!("cron_update_resume")),
                ("enabled", json!(true)),
            ],
        )
        .await;

        let stored = session_store.get("cron_update_resume").await.unwrap();
        assert!(stored.enabled);
        assert_eq!(stored.paused_at_ms, None);
    }

    #[tokio::test]
    async fn cron_update_updates_durable_task() {
        let temp = tempfile::tempdir().unwrap();
        let durable_store: Arc<dyn CronStore> =
            Arc::new(JsonFileCronStore::new(temp.path()).unwrap());
        durable_store
            .add(test_task("cron_update_durable", true))
            .await
            .unwrap();
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let mut tool = ToolCronUpdate::with_stores(
            String::new(),
            session_store,
            Some(durable_store.clone()),
            5_000,
        );
        let ccx = test_ccx().await;

        run_tool(
            &mut tool,
            ccx,
            &[
                ("id", json!("cron_update_durable")),
                ("description", json!("Updated")),
            ],
        )
        .await;

        let stored = durable_store.get("cron_update_durable").await.unwrap();
        assert_eq!(stored.description, "Updated");
    }

    #[tokio::test]
    async fn cron_update_unknown_id_errors() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let mut tool = ToolCronUpdate::with_stores(String::new(), session_store, None, 5_000);
        let ccx = test_ccx().await;

        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("id", json!("missing")), ("every", json!("30m"))]),
            )
            .await
            .unwrap_err();

        assert_eq!(err, "Scheduled task `missing` not found");
    }
}
