pub use refact_tools::tools::tool_cron_list::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::schedule::scheduler_timezone;
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
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::Utc;
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
use refact_scheduler_core::{
 cron_task_response, next_run_ms,
    session_cron_store, CronStore, Job,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_scheduler_core::{
        Action, AgentTarget, Delivery, InMemoryCronStore, JsonFileCronStore, Trigger,
        DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS,
    };

    fn session_task(id: &str) -> Job {
        let mut task = Job::new_cron_agent_chat(
            "*/5 * * * *".to_string(),
            "session prompt".to_string(),
            format!("{id} desc"),
            true,
            false,
            1000,
        );
        task.id = id.to_string();
        task.set_existing_chat(Some("chat".to_string()));
        task.set_mode(Some("agent".to_string()));
        task.auto_expire_after_ms = DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS;
        task
    }

    fn durable_task(id: &str) -> Job {
        let mut task = Job::new_cron_agent_chat(
            "0 9 * * *".to_string(),
            "durable prompt".to_string(),
            format!("{id} desc"),
            true,
            true,
            1000,
        );
        task.id = id.to_string();
        task.set_existing_chat(Some("chat".to_string()));
        task.set_mode(Some("agent".to_string()));
        task.auto_expire_after_ms = DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS;
        task
    }

    fn command_task(id: &str) -> Job {
        let mut task = session_task(id);
        task.action = Action::Command {
            argv: vec!["printf".to_string(), "hello".to_string()],
            target: AgentTarget::ExistingChat {
                chat_id: "chat".to_string(),
            },
            cwd: None,
            env: None,
            timeout_secs: None,
        };
        task
    }

    fn interval_task(id: &str) -> Job {
        let mut task = session_task(id);
        task.set_trigger(Trigger::Interval {
            every_ms: 30 * 60_000,
        });
        task
    }

    fn webhook_task(id: &str) -> Job {
        let mut task = command_task(id);
        task.set_trigger(Trigger::Webhook {
            hook_id: "deploy".to_string(),
        });
        task.delivery = Delivery::None;
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
            "chat".to_string(),
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
        tool: &mut ToolCronList,
        ccx: Arc<AMutex<AtCommandsContext>>,
        scope: Option<&str>,
    ) -> Vec<Value> {
        let mut args = HashMap::new();
        if let Some(s) = scope {
            args.insert("scope".to_string(), json!(s));
        }
        let (_, messages) = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();
        let message = match messages.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(m) => m,
            ContextEnum::ContextFile(_) => panic!("expected chat message"),
        };
        let text = match message.content {
            ChatContent::SimpleText(t) => t,
            _ => panic!("expected text content"),
        };
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn cron_list_scope_session_sees_session_store() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(session_task("cron_list_sess_a"))
            .await
            .unwrap();
        let mut tool = ToolCronList::with_stores(String::new(), session_store, None);
        let ccx = test_ccx().await;

        let session_only = run_tool(&mut tool, ccx.clone(), Some("session")).await;
        assert_eq!(session_only.len(), 1);
        assert_eq!(session_only[0]["id"], json!("cron_list_sess_a"));
        assert_eq!(session_only[0]["durable"], json!(false));

        let durable_only = run_tool(&mut tool, ccx.clone(), Some("durable")).await;
        assert_eq!(durable_only.len(), 0);

        let all = run_tool(&mut tool, ccx, None).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["id"], json!("cron_list_sess_a"));
    }

    #[tokio::test]
    async fn cron_list_scope_all_sees_session_and_durable() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(session_task("cron_list_all_sess"))
            .await
            .unwrap();

        let temp = tempfile::tempdir().unwrap();
        let durable_store: Arc<dyn CronStore> =
            Arc::new(JsonFileCronStore::new(temp.path()).unwrap());
        durable_store
            .add(durable_task("cron_list_all_dur"))
            .await
            .unwrap();

        let mut tool = ToolCronList::with_stores(String::new(), session_store, Some(durable_store));
        let ccx = test_ccx().await;

        let all = run_tool(&mut tool, ccx.clone(), None).await;
        assert_eq!(all.len(), 2);
        let ids: Vec<&str> = all.iter().map(|v| v["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"cron_list_all_sess"));
        assert!(ids.contains(&"cron_list_all_dur"));

        let session_only = run_tool(&mut tool, ccx.clone(), Some("session")).await;
        assert_eq!(session_only.len(), 1);
        assert_eq!(session_only[0]["id"], json!("cron_list_all_sess"));

        let durable_only = run_tool(&mut tool, ccx, Some("durable")).await;
        assert_eq!(durable_only.len(), 1);
        assert_eq!(durable_only[0]["id"], json!("cron_list_all_dur"));
    }

    #[tokio::test]
    async fn cron_list_includes_target_chat_and_mode() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(session_task("cron_list_target_fields"))
            .await
            .unwrap();
        let mut tool = ToolCronList::with_stores(String::new(), session_store, None);
        let ccx = test_ccx().await;

        let items = run_tool(&mut tool, ccx, Some("session")).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["chat_id"], json!("chat"));
        assert_eq!(items[0]["target"], json!("existing_chat"));
        assert_eq!(items[0]["isolated"], json!(false));
        assert_eq!(items[0]["mode"], json!("agent"));
        assert_eq!(items[0]["action_kind"], json!("agent_turn"));
    }

    #[tokio::test]
    async fn cron_list_includes_command_action_kind() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(command_task("cron_list_command_action"))
            .await
            .unwrap();
        let mut tool = ToolCronList::with_stores(String::new(), session_store, None);
        let ccx = test_ccx().await;

        let items = run_tool(&mut tool, ccx, Some("session")).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["action_kind"], json!("command"));
        assert_eq!(items[0]["chat_id"], json!("chat"));
    }

    #[tokio::test]
    async fn cron_list_includes_interval_and_webhook_trigger_fields() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(interval_task("cron_list_interval"))
            .await
            .unwrap();
        session_store
            .add(webhook_task("cron_list_webhook"))
            .await
            .unwrap();
        let mut tool = ToolCronList::with_stores(String::new(), session_store, None);
        let ccx = test_ccx().await;

        let items = run_tool(&mut tool, ccx, Some("session")).await;
        let interval = items
            .iter()
            .find(|item| item["id"] == json!("cron_list_interval"))
            .unwrap();
        assert_eq!(interval["trigger_kind"], json!("interval"));
        assert_eq!(interval["human_schedule"], json!("every 30m"));
        assert_eq!(interval["every_ms"], json!(30 * 60_000));
        assert_eq!(interval["hook_id"], Value::Null);
        assert_eq!(interval["action_kind"], json!("agent_turn"));
        assert_eq!(interval["delivery_kind"], json!("chat"));
        assert_eq!(interval["delivery"]["kind"], json!("chat"));

        let webhook = items
            .iter()
            .find(|item| item["id"] == json!("cron_list_webhook"))
            .unwrap();
        assert_eq!(webhook["trigger_kind"], json!("webhook"));
        assert_eq!(webhook["human_schedule"], json!("webhook"));
        assert_eq!(webhook["hook_id"], json!("deploy"));
        assert_eq!(webhook["next_fire_at_ms"], json!(0));
        assert_eq!(webhook["action_kind"], json!("command"));
        assert_eq!(webhook["delivery_kind"], json!("none"));
        assert_eq!(webhook["delivery"]["kind"], json!("none"));
    }

    #[tokio::test]
    async fn cron_list_includes_isolated_target() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let mut task = session_task("cron_list_isolated_target");
        set_job_isolated(&mut task);
        session_store.add(task).await.unwrap();
        let mut tool = ToolCronList::with_stores(String::new(), session_store, None);
        let ccx = test_ccx().await;

        let items = run_tool(&mut tool, ccx, Some("session")).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["chat_id"], Value::Null);
        assert_eq!(items[0]["target"], json!("isolated"));
        assert_eq!(items[0]["isolated"], json!(true));
    }

    #[tokio::test]
    async fn cron_list_next_fire_uses_scheduler_timezone() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(session_task("cron_list_tz_check"))
            .await
            .unwrap();
        let mut tool = ToolCronList::with_stores(String::new(), session_store, None);
        let ccx = test_ccx().await;

        let items = run_tool(&mut tool, ccx, Some("session")).await;
        assert_eq!(items.len(), 1);

        let now_ms = Utc::now().timestamp_millis().max(0) as u64;
        let tz = scheduler_timezone();
        let expected = next_run_ms("*/5 * * * *", now_ms, tz).unwrap_or(0);
        let actual = items[0]["next_fire_at_ms"].as_u64().unwrap();
        assert!(
            actual > 0,
            "next_fire_at_ms should be positive, got {actual}"
        );
        assert!(
            actual.abs_diff(expected) < 60_000,
            "next_fire_at_ms {actual} should be close to scheduler_timezone-based {expected}"
        );
    }
}
