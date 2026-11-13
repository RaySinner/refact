pub use refact_agents_core::tool_agent::ToolAgentSpec;
pub use refact_runtime_api::tool_agent::ToolAgentHandle;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use serde_json::json;

    use crate::agents::types::BgAgentStatus;
    use crate::app_state::AppState;
    use crate::chat::types::ChatSession;

    fn spec(child_chat_id: &str) -> ToolAgentSpec {
        ToolAgentSpec {
            tool_name: "review".to_string(),
            title: "Review: auth flow".to_string(),
            prompt: "Review the auth flow".to_string(),
            model: "test/model".to_string(),
            model_type: Some("thinking".to_string()),
            target_files: vec!["src/auth.rs".to_string()],
            child_chat_id: child_chat_id.to_string(),
            parent_tool_call_id: "tool-call-1".to_string(),
        }
    }

    async fn app_with_parent_session(
        parent_chat_id: &str,
    ) -> (AppState, Arc<tokio::sync::Mutex<ChatSession>>) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let session = Arc::new(tokio::sync::Mutex::new(ChatSession::new(
            parent_chat_id.to_string(),
        )));
        app.chat
            .sessions
            .write()
            .await
            .insert(parent_chat_id.to_string(), session.clone());
        (app, session)
    }

    async fn begin_handle(
        app: &AppState,
        parent_chat_id: &str,
        root_chat_id: &str,
        child_chat_id: &str,
    ) -> ToolAgentHandle {
        ToolAgentHandle::begin(app, parent_chat_id, root_chat_id, spec(child_chat_id))
            .await
            .expect("begin")
    }

    #[tokio::test]
    async fn begin_registers_a_running_tool_agent_with_the_child_chat() {
        let (app, _session) = app_with_parent_session("parent-begin").await;
        let handle = begin_handle(&app, "parent-begin", "root-begin", "tool-child-begin").await;
        let record = app.agents.get_any(handle.agent_id()).await.expect("record");

        assert_eq!(record.kind, crate::agents::types::BgAgentKind::Tool);
        assert_eq!(record.child_chat_id.as_deref(), Some("tool-child-begin"));
        assert_eq!(record.parent_tool_call_id.as_deref(), Some("tool-call-1"));
        assert_eq!(record.status, BgAgentStatus::Running);
        assert_eq!(record.config_name, "review");
        assert_eq!(record.model, "test/model");
        assert_eq!(record.model_type.as_deref(), Some("thinking"));
        assert_eq!(record.target_files, vec!["src/auth.rs".to_string()]);
        assert_eq!(record.parent_root_chat_id.as_deref(), Some("root-begin"));
    }

    #[tokio::test]
    async fn begin_emits_a_background_agent_updated_into_the_parent_session() {
        let (app, session) = app_with_parent_session("parent-emit").await;
        let handle = begin_handle(&app, "parent-emit", "parent-emit", "tool-child-emit").await;

        let session = session.lock().await;
        let summary = session
            .background_agents
            .get(handle.agent_id())
            .expect("background agent summary");
        assert_eq!(summary.kind, "tool");
        assert_eq!(summary.status, "running");
        assert_eq!(summary.child_chat_id.as_deref(), Some("tool-child-emit"));
        assert_eq!(summary.parent_tool_call_id.as_deref(), Some("tool-call-1"));
    }

    #[tokio::test]
    async fn progress_and_usage_update_the_record() {
        let (app, _session) = app_with_parent_session("parent-progress").await;
        let handle =
            begin_handle(&app, "parent-progress", "root-progress", "tool-child-progress").await;

        handle.progress("reading files", 3).await;
        handle.current_tool(Some("grep: auth".to_string())).await;
        handle.add_usage(120, Some(0.5)).await;

        let record = app
            .agents
            .get_any(handle.agent_id())
            .await
            .expect("record");
        assert_eq!(record.step_count, 3);
        assert_eq!(record.progress.as_deref(), Some("reading files"));
        assert_eq!(record.current_tool.as_deref(), Some("grep: auth"));
        assert_eq!(record.tokens_used, 120);
        assert_eq!(record.cost_usd, Some(0.5));
        assert!(record.last_activity.is_some());
    }

    #[tokio::test]
    async fn complete_marks_terminal_with_summary() {
        let (app, _session) = app_with_parent_session("parent-complete").await;
        let handle =
            begin_handle(&app, "parent-complete", "root-complete", "tool-child-complete").await;
        let agent_id = handle.agent_id().to_string();

        handle
            .complete("review done".to_string())
            .await
            .expect("complete");

        let record = app
            .agents
            .get_any(&agent_id)
            .await
            .expect("record");
        assert_eq!(record.status, BgAgentStatus::Completed);
        assert_eq!(record.result_summary.as_deref(), Some("review done"));
        assert!(record.finished_at.is_some());
        assert_eq!(record.child_chat_id.as_deref(), Some("tool-child-complete"));
    }

    #[tokio::test]
    async fn fail_marks_failed() {
        let (app, _session) = app_with_parent_session("parent-fail").await;
        let handle = begin_handle(&app, "parent-fail", "root-fail", "tool-child-fail").await;
        let agent_id = handle.agent_id().to_string();

        handle.fail("boom".to_string()).await.expect("fail");

        let record = app
            .agents
            .get_any(&agent_id)
            .await
            .expect("record");
        assert_eq!(record.status, BgAgentStatus::Failed);
        assert_eq!(record.error.as_deref(), Some("boom"));
        assert!(record.finished_at.is_some());
    }

    #[tokio::test]
    async fn cancelled_marks_cancelled() {
        let (app, _session) = app_with_parent_session("parent-cancelled").await;
        let handle =
            begin_handle(&app, "parent-cancelled", "root-cancelled", "tool-child-cancelled").await;
        let agent_id = handle.agent_id().to_string();

        handle
            .cancelled(Some("stop".to_string()))
            .await
            .expect("cancelled");

        let record = app
            .agents
            .get_any(&agent_id)
            .await
            .expect("record");
        assert_eq!(record.status, BgAgentStatus::Cancelled);
        assert_eq!(record.error.as_deref(), Some("stop"));
        assert!(record.finished_at.is_some());
    }

    #[tokio::test]
    async fn registry_cancel_sets_the_handle_abort_flag() {
        let (app, _session) = app_with_parent_session("parent-cancel-flag").await;
        let handle =
            begin_handle(&app, "parent-cancel-flag", "root-cancel-flag", "tool-child-flag").await;

        assert!(!handle.aborted());
        app.agents
            .cancel("parent-cancel-flag", handle.agent_id(), None)
            .await
            .expect("cancel");
        assert!(handle.aborted());

        let record = app
            .agents
            .get_any(handle.agent_id())
            .await
            .expect("record");
        assert_eq!(record.status, BgAgentStatus::Cancelled);
    }

    #[tokio::test]
    async fn extras_carry_the_gui_contract_keys() {
        let (app, _session) = app_with_parent_session("parent-extras").await;
        let handle =
            begin_handle(&app, "parent-extras", "root-extras", "tool-child-extras").await;

        let extras = handle.extras("running", Some("thinking"));
        let expected = [
            "background_agent_id",
            "background_agent_kind",
            "background_agent_status",
            "child_chat_id",
            "parent_chat_id",
            "model",
            "model_type",
            "title",
        ];
        assert_eq!(extras.len(), 8);
        for key in expected {
            assert!(extras.contains_key(key), "missing contract key: {key}");
        }
        assert_eq!(extras["background_agent_kind"], json!("tool"));
        assert_eq!(extras["background_agent_id"], json!(handle.agent_id()));
        assert_eq!(extras["background_agent_status"], json!("running"));
        assert_eq!(extras["child_chat_id"], json!("tool-child-extras"));
        assert_eq!(extras["parent_chat_id"], json!("parent-extras"));
        assert_eq!(extras["model"], json!("test/model"));
        assert_eq!(extras["model_type"], json!("thinking"));
        assert_eq!(extras["title"], json!("Review: auth flow"));

        let null_model_type = handle.extras("running", None);
        assert_eq!(null_model_type["model_type"], serde_json::Value::Null);
    }
}
