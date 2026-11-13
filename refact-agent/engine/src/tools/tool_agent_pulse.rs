pub use refact_tools::tools::tool_agent_pulse::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ChatToolCall;
#[cfg(test)]
use refact_core::chat_types::ChatUsage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_tasks::types::AbVariantInfo;
#[cfg(test)]
use refact_tasks::types::BoardCard;
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_runtime_api::ChatSessionFacade;
#[cfg(test)]
use refact_runtime_api::ChatSessionSnapshot;
#[cfg(test)]
use refact_runtime_api::SessionState;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{StatusUpdate, TaskBoard, TaskMeta, TaskStatus};
    use crate::tools::tools_description::Tool;
    use refact_chat_api::{ChatCommand, ThreadParams};
    use refact_runtime_api::{ChatSessionUpdate, CreateSessionRequest, RuntimeTrajectorySnapshot};
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
        let now = Utc::now();
        let created_at = (now - chrono::Duration::minutes(1)).to_rfc3339();
        let heartbeat_at = (now - chrono::Duration::seconds(12)).to_rfc3339();
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
            status_updates: vec![StatusUpdate {
                timestamp: created_at.clone(),
                message: "started".to_string(),
            }],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: created_at.clone(),
            started_at: Some(created_at),
            last_heartbeat_at: Some(heartbeat_at),
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

    fn ab_test_card() -> BoardCard {
        let mut card = test_card(None);
        card.assignee = Some("ab".to_string());
        card.ab_variants = Some(refact_tasks::types::AbVariants {
            a: AbVariantInfo {
                agent_id: "agent-a".to_string(),
                chat_id: "agent-chat-a".to_string(),
                worktree: "/tmp/agent-a".to_string(),
                worktree_name: Some("worktree-a".to_string()),
                branch: Some("branch-a".to_string()),
                model: Some("model-a".to_string()),
                finish: None,
            },
            b: AbVariantInfo {
                agent_id: "agent-b".to_string(),
                chat_id: "agent-chat-b".to_string(),
                worktree: "/tmp/agent-b".to_string(),
                worktree_name: Some("worktree-b".to_string()),
                branch: Some("branch-b".to_string()),
                model: Some("model-b".to_string()),
                finish: None,
            },
            winner: None,
        });
        card
    }

    fn task_meta() -> TaskMeta {
        let now = "2026-05-22T00:00:00Z".to_string();
        TaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
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

    fn tool_call(name: &str, arguments: Value) -> ChatToolCall {
        ChatToolCall {
            id: "call-1".to_string(),
            index: Some(0),
            function: refact_core::chat_types::ChatToolFunction {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        }
    }

    #[test]
    fn tool_agent_pulse_description_is_correct() {
        let desc = ToolAgentPulse::new().tool_description();

        assert_eq!(desc.name, "agent_pulse");
        assert_eq!(desc.input_schema["required"], json!(["card_id"]));
        assert!(desc.input_schema["properties"].get("task_id").is_some());
        assert!(desc.description.contains("planner-only"));
    }

    #[tokio::test]
    async fn tool_agent_pulse_rejects_non_planner_role() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), test_card(Some("agent-chat-1".to_string()))).await;
        let ccx = planner_ccx(gcx, "agents", Arc::new(MockChatSessionFacade::default())).await;
        let mut tool = ToolAgentPulse::new();
        let args = HashMap::from([("card_id".to_string(), json!("T-29"))]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by the task planner"));
    }

    #[tokio::test]
    async fn tool_agent_pulse_session_not_found_returns_graceful_message() {
        let temp = tempfile::tempdir().unwrap();
        let facade = Arc::new(MockChatSessionFacade::default());
        facade.snapshots.lock().unwrap().insert(
            "agent-chat-1".to_string(),
            Err("session missing".to_string()),
        );
        let gcx = write_task(temp.path(), test_card(Some("agent-chat-1".to_string()))).await;
        let ccx = planner_ccx(gcx, "planner", facade).await;
        let mut tool = ToolAgentPulse::new();
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-29")),
            ("task_id".to_string(), json!("task-1")),
        ]);

        let output = tool_output_text(
            tool.tool_execute(ccx, &"call".to_string(), &args)
                .await
                .unwrap(),
        );

        assert!(output.contains("# Agent Pulse: T-29"));
        assert!(output.contains("**State:** ❓ last known: doing"));
        assert!(output.contains("Session snapshot unavailable"));
        assert!(output.contains("## Last assistant message\n> (none)"));
        assert!(output.contains("## Last tool call\n(none)"));
    }

    #[tokio::test]
    async fn tool_agent_pulse_rejects_mismatched_task_id() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx, "planner", Arc::new(MockChatSessionFacade::default())).await;
        let mut tool = ToolAgentPulse::new();
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-29")),
            ("task_id".to_string(), json!("task-2")),
        ]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(
            err,
            "task_id override is not allowed from this planner chat"
        );
    }

    #[tokio::test]
    async fn tool_agent_pulse_extracts_tool_and_message_previews() {
        let temp = tempfile::tempdir().unwrap();
        let facade = Arc::new(MockChatSessionFacade::default());
        let assistant_text = format!(
            "Adding sticky alerts logic to format_agent_status. The plan is to {} token=supersecret",
            "continue carefully. ".repeat(20)
        );
        let snapshot = ChatSessionSnapshot {
            messages: vec![
                ChatMessage {
                    role: "assistant".to_string(),
                    content: ChatContent::SimpleText("Earlier".to_string()),
                    usage: Some(refact_core::chat_types::ChatUsage {
                        prompt_tokens: 10_000,
                        completion_tokens: 5_000,
                        total_tokens: 15_000,
                        cache_creation_tokens: None,
                        cache_read_tokens: None,
                        metering_usd: None,
                    }),
                    ..Default::default()
                },
                ChatMessage {
                    role: "assistant".to_string(),
                    content: ChatContent::SimpleText(assistant_text),
                    tool_calls: Some(vec![tool_call(
                        "patch",
                        json!({
                            "path": "src/tools/tool_task_check_agents.rs",
                            "old_str": "old code",
                            "replacement": "new code"
                        }),
                    )]),
                    usage: Some(refact_core::chat_types::ChatUsage {
                        prompt_tokens: 20_000,
                        completion_tokens: 3_000,
                        total_tokens: 23_000,
                        cache_creation_tokens: None,
                        cache_read_tokens: None,
                        metering_usd: None,
                    }),
                    thinking_blocks: Some(vec![json!({"type": "thinking", "thinking": "hidden"})]),
                    ..Default::default()
                },
            ],
            thread: ThreadParams {
                context_tokens_cap: Some(200_000),
                ..Default::default()
            },
            session_state: SessionState::ExecutingTools,
            pause_reasons: vec![],
            goal: None,
        };
        facade
            .snapshots
            .lock()
            .unwrap()
            .insert("agent-chat-1".to_string(), Ok(snapshot));
        let gcx = write_task(temp.path(), test_card(Some("agent-chat-1".to_string()))).await;
        let ccx = planner_ccx(gcx, "planner", facade).await;
        let mut tool = ToolAgentPulse::new();
        let args = HashMap::from([("card_id".to_string(), json!("T-29"))]);

        let output = tool_output_text(
            tool.tool_execute(ccx, &"call".to_string(), &args)
                .await
                .unwrap(),
        );

        assert!(output.contains("**State:** ⚙️ executing tools"));
        assert!(output.contains("**Last activity:** 12s ago"));
        assert!(output.contains("**Tokens used:** ~38k / 200k"));
        assert!(output.contains("**Currently editing:** src/tools/tool_task_check_agents.rs"));
        assert!(output.contains("`patch(path=src/tools/tool_task_check_agents.rs"));
        assert!(output.contains("Adding sticky alerts logic to format_agent_status"));
        assert!(output.contains('…'));
        assert!(!output.contains("hidden"));
        assert!(!output.contains("supersecret"));
    }

    #[tokio::test]
    async fn tool_agent_pulse_resolves_ab_variant_chat() {
        let temp = tempfile::tempdir().unwrap();
        let facade = Arc::new(MockChatSessionFacade::default());
        let snapshot = ChatSessionSnapshot {
            messages: vec![ChatMessage {
                role: "assistant".to_string(),
                content: ChatContent::SimpleText("Variant B is editing.".to_string()),
                ..Default::default()
            }],
            thread: ThreadParams {
                context_tokens_cap: Some(100_000),
                ..Default::default()
            },
            session_state: SessionState::Generating,
            pause_reasons: vec![],
            goal: None,
        };
        facade
            .snapshots
            .lock()
            .unwrap()
            .insert("agent-chat-b".to_string(), Ok(snapshot));
        facade.states.lock().unwrap().insert(
            "agent-chat-b".to_string(),
            Ok(Some(SessionState::Generating)),
        );
        let gcx = write_task(temp.path(), ab_test_card()).await;
        let ccx = planner_ccx(gcx, "planner", facade).await;
        let mut tool = ToolAgentPulse::new();
        let args = HashMap::from([("card_id".to_string(), json!("T-29/b"))]);

        let output = tool_output_text(
            tool.tool_execute(ccx, &"call".to_string(), &args)
                .await
                .unwrap(),
        );

        assert!(output.contains("# Agent Pulse: T-29/b"));
        assert!(output.contains("check_agents redesign [A/B B]"));
        assert!(output.contains("**A/B variant:** B"));
        assert!(output.contains("**State:** 🔄 generating response"));
        assert!(output.contains("Variant B is editing."));
    }

    #[tokio::test]
    async fn pulse_uses_live_session_activity_when_session_present() {
        use refact_chat_api::ChatSession;
        use std::time::{Duration, Instant};

        let temp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let heartbeat_at = (now - chrono::Duration::hours(1)).to_rfc3339();
        let created_at = (now - chrono::Duration::hours(2)).to_rfc3339();
        let mut card = test_card(Some("agent-chat-1".to_string()));
        card.last_heartbeat_at = Some(heartbeat_at);
        card.created_at = created_at.clone();
        card.started_at = Some(created_at);
        card.status_updates = vec![];
        let gcx = write_task(temp.path(), card).await;

        let mut session = ChatSession::new("agent-chat-1".to_string());
        session.last_activity = Instant::now()
            .checked_sub(Duration::from_secs(30))
            .unwrap_or_else(Instant::now);
        gcx.chat_sessions
            .write()
            .await
            .insert("agent-chat-1".to_string(), Arc::new(AMutex::new(session)));

        let facade = Arc::new(MockChatSessionFacade::default());
        let ccx = planner_ccx(gcx, "planner", facade).await;
        let mut tool = ToolAgentPulse::new();
        let args = HashMap::from([("card_id".to_string(), json!("T-29"))]);

        let output = tool_output_text(
            tool.tool_execute(ccx, &"call".to_string(), &args)
                .await
                .unwrap(),
        );

        assert!(
            output.contains("**Last activity:** 30s ago"),
            "got: {output}"
        );
        assert!(
            !output.contains("1h ago"),
            "should not show stale heartbeat: {output}"
        );
    }

    #[tokio::test]
    async fn pulse_falls_back_to_board_when_no_session() {
        let temp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let five_min_ago = (now - chrono::Duration::minutes(5)).to_rfc3339();
        let mut card = test_card(Some("agent-chat-1".to_string()));
        card.last_heartbeat_at = Some(five_min_ago.clone());
        card.started_at = Some(five_min_ago.clone());
        card.created_at = five_min_ago;
        card.status_updates = vec![];
        let gcx = write_task(temp.path(), card).await;

        let facade = Arc::new(MockChatSessionFacade::default());
        let ccx = planner_ccx(gcx, "planner", facade).await;
        let mut tool = ToolAgentPulse::new();
        let args = HashMap::from([("card_id".to_string(), json!("T-29"))]);

        let output = tool_output_text(
            tool.tool_execute(ccx, &"call".to_string(), &args)
                .await
                .unwrap(),
        );

        assert!(
            output.contains("**Last activity:** 5m ago"),
            "got: {output}"
        );
    }
}
