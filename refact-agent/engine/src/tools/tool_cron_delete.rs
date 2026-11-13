pub use refact_tools::tools::tool_cron_delete::*;

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
use refact_runtime_api::scheduler::active_durable_cron_store_for_services;
#[cfg(test)]
#[allow(unused_imports)]
use refact_scheduler_core::{ session_cron_store, CronStore};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_scheduler_core::{
        InMemoryCronStore, Job, JsonFileCronStore, DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS,
    };

    fn test_task(id: &str, durable: bool) -> Job {
        let mut task = Job::new_cron_agent_chat(
            "*/5 * * * *".to_string(),
            "Check the build".to_string(),
            "Check build".to_string(),
            true,
            durable,
            1000,
        );
        task.id = id.to_string();
        task.set_existing_chat(Some("chat".to_string()));
        task.set_mode(Some("agent".to_string()));
        task.auto_expire_after_ms = DEFAULT_RECURRING_AUTO_EXPIRE_AFTER_MS;
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
            "cron-delete-test".to_string(),
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
        tool: &mut ToolCronDelete,
        ccx: Arc<AMutex<AtCommandsContext>>,
        id: &str,
    ) -> Value {
        let mut args = HashMap::new();
        args.insert("id".to_string(), json!(id));
        let (_, contexts) = tool
            .tool_execute(ccx, &"call".to_string(), &args)
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

    #[tokio::test]
    async fn cron_delete_removes_session_task() {
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        session_store
            .add(test_task("cron_del_sess", false))
            .await
            .unwrap();
        let mut tool = ToolCronDelete::with_stores(String::new(), session_store.clone(), None);
        let ccx = test_ccx().await;

        let result = run_tool(&mut tool, ccx, "cron_del_sess").await;

        assert_eq!(result, json!({ "removed": true }));
        assert!(session_store.list().await.is_empty());
    }

    #[tokio::test]
    async fn cron_delete_removes_durable_task() {
        let temp = tempfile::tempdir().unwrap();
        let durable_store: Arc<dyn CronStore> =
            Arc::new(JsonFileCronStore::new(temp.path()).unwrap());
        durable_store
            .add(test_task("cron_del_dur", true))
            .await
            .unwrap();
        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let mut tool =
            ToolCronDelete::with_stores(String::new(), session_store, Some(durable_store.clone()));
        let ccx = test_ccx().await;

        let result = run_tool(&mut tool, ccx, "cron_del_dur").await;

        assert_eq!(result, json!({ "removed": true }));
        assert!(durable_store.list().await.is_empty());
    }

    #[tokio::test]
    async fn cron_delete_uses_same_durable_path_as_list() {
        use crate::tools::tool_cron_list::ToolCronList;

        let temp = tempfile::tempdir().unwrap();
        let durable_store: Arc<dyn CronStore> =
            Arc::new(JsonFileCronStore::new(temp.path()).unwrap());
        durable_store
            .add(test_task("cron_shared_path", true))
            .await
            .unwrap();

        let session_store: Arc<dyn CronStore> = Arc::new(InMemoryCronStore::new());
        let mut delete_tool = ToolCronDelete::with_stores(
            String::new(),
            session_store.clone(),
            Some(durable_store.clone()),
        );
        let mut list_tool =
            ToolCronList::with_stores(String::new(), session_store, Some(durable_store));

        let ccx = test_ccx().await;

        let before = {
            let args = HashMap::new();
            let (_, msgs) = list_tool
                .tool_execute(ccx.clone(), &"call".to_string(), &args)
                .await
                .unwrap();
            let ContextEnum::ChatMessage(m) = msgs.into_iter().next().unwrap() else {
                panic!()
            };
            let ChatContent::SimpleText(t) = m.content else {
                panic!()
            };
            serde_json::from_str::<Vec<Value>>(&t).unwrap()
        };
        assert_eq!(before.len(), 1);
        assert_eq!(before[0]["id"], json!("cron_shared_path"));

        let delete_result = run_tool(&mut delete_tool, ccx.clone(), "cron_shared_path").await;
        assert_eq!(delete_result, json!({ "removed": true }));

        let after = {
            let args = HashMap::new();
            let (_, msgs) = list_tool
                .tool_execute(ccx, &"call".to_string(), &args)
                .await
                .unwrap();
            let ContextEnum::ChatMessage(m) = msgs.into_iter().next().unwrap() else {
                panic!()
            };
            let ChatContent::SimpleText(t) = m.content else {
                panic!()
            };
            serde_json::from_str::<Vec<Value>>(&t).unwrap()
        };
        assert!(after.is_empty());
    }
}
