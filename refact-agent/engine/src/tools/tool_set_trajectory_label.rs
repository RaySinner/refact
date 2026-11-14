pub use refact_tools::tools::tool_set_trajectory_label::*;

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
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_history::trajectory_metrics::TRAJECTORY_LABEL_MAX_CHARS;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::trajectory_runtime::set_trajectory_label;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::ChatSession;
    use refact_runtime_api::trajectory_runtime::load_trajectory_for_chat;

    async fn context(
        gcx: Arc<crate::global_context::GlobalContext>,
        chat_id: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                chat_id.to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ))
    }

    #[tokio::test]
    async fn labels_invoking_live_session_and_persists_it() {
        let workspace = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let chat_id = "label-current-session";
        let session = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), session.clone());
        let mut tool = ToolSetTrajectoryLabel {
            config_path: String::new(),
        };
        tool.tool_execute(
            context(gcx.clone(), chat_id).await,
            &"call-1".to_string(),
            &HashMap::from([("label".to_string(), json!("Focused label"))]),
        )
        .await
        .unwrap();

        let session = session.lock().await;
        assert_eq!(session.thread.title, "Focused label");
        assert!(!session.thread.is_title_generated);
        drop(session);
        let loaded = load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), chat_id).await.unwrap();
        assert_eq!(loaded.thread.title, "Focused label");
        assert!(!loaded.thread.is_title_generated);
    }

    #[test]
    fn validates_required_non_empty_and_bounded_label() {
        assert!(refact_chat_history::trajectory_metrics::validate_trajectory_label("  ").is_err());
        assert!(
            refact_chat_history::trajectory_metrics::validate_trajectory_label(
                &"x".repeat(TRAJECTORY_LABEL_MAX_CHARS + 1)
            )
            .is_err()
        );
        assert_eq!(
            refact_chat_history::trajectory_metrics::validate_trajectory_label(
                "  valid\n label  "
            )
            .unwrap(),
            "valid label"
        );
    }
}
