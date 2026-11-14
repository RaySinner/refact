pub use refact_tools::tools::buddy::surface::*;

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
use uuid::Uuid;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::{AppState, BuddyServices};
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::autonomous_redaction::redact_and_cap_text;
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::actor::{make_runtime_event, BuddyMutation};
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::types::{BuddyActivity, BuddySpeechItem};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::buddy_runtime::{buddy_apply, buddy_enqueue_event};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::actor::BuddyService;
    use crate::buddy::runtime_queue::RuntimeQueue;
    use crate::buddy::settings::BuddySettings;
    use crate::global_context::GlobalContext;

    async fn ccx(gcx: Arc<GlobalContext>) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4000,
                20,
                false,
                vec![],
                "test-chat".to_string(),
                None,
                "test-model".to_string(),
                None,
                None,
            )
            .await,
        ))
    }

    async fn gcx_with_buddy() -> (tempfile::TempDir, Arc<GlobalContext>) {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let service = BuddyService::new(
            dir.path().to_path_buf(),
            crate::buddy::state::default_buddy_state(),
            BuddySettings::default(),
            Vec::new(),
            RuntimeQueue::new(),
            tokio::sync::broadcast::channel(16).0,
            None,
        );
        let buddy_arc = gcx.buddy.clone();
        *buddy_arc.lock().await = Some(service);
        (dir, gcx)
    }

    fn args(items: Vec<(&str, Value)>) -> HashMap<String, Value> {
        items
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    #[tokio::test]
    async fn buddy_log_activity_does_not_panic_with_minimal_args() {
        let (_dir, gcx) = gcx_with_buddy().await;
        let mut tool = ToolBuddyLogActivity {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx(gcx).await,
                &"call".to_string(),
                &args(vec![
                    ("title", json!("Tiny win")),
                    ("description", json!("A small helpful activity happened")),
                ]),
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn buddy_speak_rejects_invalid_intent() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut tool = ToolBuddySpeak {
            config_path: String::new(),
        };
        let result = tool
            .tool_execute(
                ccx(gcx).await,
                &"call".to_string(),
                &args(vec![
                    ("text", json!("Hello")),
                    ("mood", json!("playful")),
                    ("intent", json!("not_real")),
                ]),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn buddy_runtime_event_dedupes_on_dedupe_key() {
        let (_dir, gcx) = gcx_with_buddy().await;
        let mut tool = ToolBuddyRuntimeEvent {
            config_path: String::new(),
        };
        let tool_call_id = "call".to_string();
        let call_args = args(vec![
            ("title", json!("Workflow started")),
            ("status", json!("started")),
            ("dedupe_key", json!("same-key")),
        ]);
        tool.tool_execute(ccx(gcx.clone()).await, &tool_call_id, &call_args)
            .await
            .unwrap();
        let buddy_arc = gcx.buddy.clone();
        let first_len = buddy_arc
            .lock()
            .await
            .as_ref()
            .unwrap()
            .runtime_queue
            .items
            .len();
        tool.tool_execute(ccx(gcx.clone()).await, &tool_call_id, &call_args)
            .await
            .unwrap();
        let second_len = buddy_arc
            .lock()
            .await
            .as_ref()
            .unwrap()
            .runtime_queue
            .items
            .len();
        assert_eq!(first_len, 1);
        assert_eq!(second_len, first_len);
    }
}
