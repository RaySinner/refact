pub use refact_tools::tools::tool_agent_chat_summary::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_runtime_api::ChatSessionFacade;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus};
    use crate::tools::tools_description::Tool;
    use refact_chat_api::{ChatCommand, ThreadParams};
    use refact_runtime_api::{
        ChatSessionSnapshot, ChatSessionUpdate, CreateSessionRequest, RuntimeTrajectorySnapshot,
        SessionState,
    };
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MockChatSessionFacade {
        snapshots: StdMutex<HashMap<String, Result<ChatSessionSnapshot, String>>>,
        states: StdMutex<HashMap<String, Result<Option<SessionState>, String>>>,
    }

    #[async_trait]
    impl ChatSessionFacade for MockChatSessionFacade {
        async fn session_snapshot(&self, chat_id: &str) -> Result<ChatSessionSnapshot, String> {
            self.snapshots
                .lock()
                .unwrap()
                .get(chat_id)
                .cloned()
                .unwrap_or_else(|| Err("missing session".to_string()))
        }

        async fn update_session(
            &self,
            _chat_id: &str,
            _update: ChatSessionUpdate,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn create_session(&self, _request: CreateSessionRequest) -> Result<(), String> {
            Ok(())
        }

        async fn push_command(&self, _chat_id: &str, _command: ChatCommand) -> Result<(), String> {
            Ok(())
        }

        async fn session_state(&self, chat_id: &str) -> Result<Option<SessionState>, String> {
            self.states
                .lock()
                .unwrap()
                .get(chat_id)
                .cloned()
                .unwrap_or(Ok(None))
        }

        async fn maybe_save_session(&self, _chat_id: &str) -> Result<(), String> {
            Ok(())
        }

        async fn save_trajectory_snapshot(
            &self,
            _snapshot: RuntimeTrajectorySnapshot,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn test_card(agent_chat_id: Option<String>) -> BoardCard {
        BoardCard {
            id: "T-29".to_string(),
            title: "check_agents redesign".to_string(),
            column: "doing".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: Some("agent-1".to_string()),
            agent_chat_id,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: "2026-05-22T00:00:00Z".to_string(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    fn task_meta() -> TaskMeta {
        TaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: "2026-05-22T00:00:00Z".to_string(),
            updated_at: "2026-05-22T00:00:00Z".to_string(),
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        }
    }

    async fn write_task(
        root: &std::path::Path,
        card: BoardCard,
    ) -> Arc<crate::global_context::GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        let mut board = TaskBoard::default();
        board.cards.push(card);
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&task_meta()).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(
            task_dir.join("board.yaml"),
            serde_yaml::to_string(&board).unwrap(),
        )
        .await
        .unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        gcx
    }

    async fn planner_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
        role: &str,
        facade: Arc<dyn ChatSessionFacade>,
    ) -> Arc<AMutex<AtCommandsContext>> {
        let mut app = refact_runtime_api::app_state_from_gcx(gcx).await;
        app.chat.facade = facade;
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app,
                200_000,
                20,
                false,
                vec![],
                "planner-chat".to_string(),
                None,
                "model".to_string(),
                Some(ThreadTaskMeta {
                    task_id: "task-1".to_string(),
                    role: role.to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: None,
                }),
                None,
            )
            .await,
        ))
    }

    fn tool_output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    #[test]
    fn tool_agent_chat_summary_description_is_correct() {
        let desc = ToolAgentChatSummary::new().tool_description();

        assert_eq!(desc.name, "agent_chat_summary");
        assert_eq!(desc.input_schema["required"], json!(["card_id"]));
        assert!(desc.input_schema["properties"].get("focus").is_some());
        assert!(desc.input_schema["properties"].get("task_id").is_some());
        assert!(desc.description.contains("Planner-only"));
    }

    #[tokio::test]
    async fn tool_agent_chat_summary_rejects_non_planner_role() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), test_card(Some("agent-chat-1".to_string()))).await;
        let ccx = planner_ccx(gcx, "agents", Arc::new(MockChatSessionFacade::default())).await;
        let mut tool = ToolAgentChatSummary::new();
        let args = HashMap::from([("card_id".to_string(), json!("T-29"))]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by the task planner"));
    }

    #[tokio::test]
    async fn tool_agent_chat_summary_empty_session_returns_no_messages() {
        let temp = tempfile::tempdir().unwrap();
        let facade = Arc::new(MockChatSessionFacade::default());
        facade.snapshots.lock().unwrap().insert(
            "agent-chat-1".to_string(),
            Ok(ChatSessionSnapshot {
                messages: vec![],
                thread: ThreadParams::default(),
                session_state: SessionState::Idle,
                pause_reasons: vec![],
                goal: None,
            }),
        );
        facade
            .states
            .lock()
            .unwrap()
            .insert("agent-chat-1".to_string(), Ok(Some(SessionState::Idle)));
        let gcx = write_task(temp.path(), test_card(Some("agent-chat-1".to_string()))).await;
        let ccx = planner_ccx(gcx, "planner", facade).await;
        let mut tool = ToolAgentChatSummary::new();
        let args = HashMap::from([("card_id".to_string(), json!("T-29"))]);

        let output = tool_output_text(
            tool.tool_execute(ccx, &"call".to_string(), &args)
                .await
                .unwrap(),
        );

        assert!(output.contains("# Agent Chat Summary: T-29"));
        assert!(output.contains("No messages found"));
    }

    #[test]
    fn tool_agent_chat_summary_mock_subchat_result_rendered() {
        let raw = r#"{
            "decisions": ["Used sticky alerts at top regardless of pagination"],
            "files_touched": ["src/tools/tool_task_check_agents.rs (modified)"],
            "blockers": ["None"],
            "next_steps": ["Run cargo test"],
            "current_state": "finishing tests",
            "confidence_score": 87
        }"#;

        let output = render_summary_response("T-29", raw);

        assert!(output.contains("# Agent Chat Summary: T-29"));
        assert!(output.contains("**Confidence:** 87/100"));
        assert!(output.contains("**Current state:** finishing tests"));
        assert!(output.contains("- Used sticky alerts at top regardless of pagination"));
        assert!(output.contains("- src/tools/tool_task_check_agents.rs (modified)"));
        assert!(output.contains("- Run cargo test"));
    }
}
