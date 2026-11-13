pub use refact_tools::tools::tool_agent_steer::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
use crate::tools::planner_delivery::{self, CardGuard, CardTarget, DeliveryReport};
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_core::chat_types::PushMode;
#[cfg(test)]
use refact_runtime_api::SessionState;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_chat_api::ChatCommand;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus};
    use crate::tools::tools_description::Tool;
    use refact_core::chat_types::{DeliveryOutcome, PendingDelivery};
    use refact_runtime_api::{
        ChatSessionFacade, ChatSessionSnapshot, ChatSessionUpdate, CreateSessionRequest,
        RuntimeTrajectorySnapshot,
    };
    use chrono::Utc;
    use std::sync::Mutex as StdMutex;

    struct MockChatFacade {
        state: StdMutex<SessionState>,
        pushed: StdMutex<Vec<(String, ChatCommand)>>,
        delivered: StdMutex<Vec<(String, PendingDelivery)>>,
        messages: StdMutex<Vec<ChatMessage>>,
        thread: StdMutex<refact_chat_api::ThreadParams>,
        updates: StdMutex<Vec<ChatSessionUpdate>>,
    }

    fn test_agent_thread() -> refact_chat_api::ThreadParams {
        refact_chat_api::ThreadParams {
            task_meta: Some(ThreadTaskMeta {
                task_id: "task-1".to_string(),
                role: "agents".to_string(),
                agent_id: Some("agent-1".to_string()),
                card_id: Some("T-29".to_string()),
                planner_chat_id: Some("planner".to_string()),
            }),
            ..Default::default()
        }
    }

    impl MockChatFacade {
        fn new(state: SessionState) -> Self {
            Self::with_messages(state, vec![])
        }

        fn with_messages(state: SessionState, messages: Vec<ChatMessage>) -> Self {
            Self {
                state: StdMutex::new(state),
                pushed: StdMutex::new(vec![]),
                delivered: StdMutex::new(vec![]),
                messages: StdMutex::new(messages),
                thread: StdMutex::new(refact_chat_api::ThreadParams {
                    auto_compact_enabled: Some(true),
                    ..test_agent_thread()
                }),
                updates: StdMutex::new(vec![]),
            }
        }

        fn pushed_commands(&self) -> Vec<(String, ChatCommand)> {
            self.pushed.lock().unwrap().clone()
        }

        fn deliveries(&self) -> Vec<(String, PendingDelivery)> {
            self.delivered.lock().unwrap().clone()
        }

        fn updates(&self) -> Vec<ChatSessionUpdate> {
            self.updates.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ChatSessionFacade for MockChatFacade {
        async fn session_snapshot(&self, _chat_id: &str) -> Result<ChatSessionSnapshot, String> {
            Ok(ChatSessionSnapshot {
                messages: self.messages.lock().unwrap().clone(),
                thread: self.thread.lock().unwrap().clone(),
                session_state: *self.state.lock().unwrap(),
                pause_reasons: vec![],
                goal: None,
            })
        }

        async fn update_session(
            &self,
            _chat_id: &str,
            update: ChatSessionUpdate,
        ) -> Result<(), String> {
            *self.messages.lock().unwrap() = update.messages.clone();
            self.updates.lock().unwrap().push(update);
            Ok(())
        }

        async fn create_session(&self, _request: CreateSessionRequest) -> Result<(), String> {
            Ok(())
        }

        async fn push_command(&self, chat_id: &str, command: ChatCommand) -> Result<(), String> {
            self.pushed
                .lock()
                .unwrap()
                .push((chat_id.to_string(), command));
            Ok(())
        }

        async fn deliver_messages(
            &self,
            chat_id: &str,
            delivery: PendingDelivery,
        ) -> Result<DeliveryOutcome, String> {
            self.delivered
                .lock()
                .unwrap()
                .push((chat_id.to_string(), delivery));
            Ok(DeliveryOutcome::Queued)
        }

        async fn session_state(&self, _chat_id: &str) -> Result<Option<SessionState>, String> {
            Ok(Some(*self.state.lock().unwrap()))
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

    fn test_card(column: &str, agent_chat_id: Option<String>) -> BoardCard {
        BoardCard {
            id: "T-29".to_string(),
            title: "Steerable card".to_string(),
            column: column.to_string(),
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
            created_at: Utc::now().to_rfc3339(),
            started_at: Some(Utc::now().to_rfc3339()),
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
        let now = Utc::now().to_rfc3339();
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
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        storage::save_task_meta(gcx.clone(), "task-1", &task_meta())
            .await
            .unwrap();
        storage::save_board(
            gcx.clone(),
            "task-1",
            &TaskBoard {
                cards: vec![card],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        gcx
    }

    async fn planner_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
        facade: Arc<dyn ChatSessionFacade>,
        role: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
        let mut app = refact_runtime_api::app_state_from_gcx(gcx).await;
        app.chat.facade = facade;
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app,
                4096,
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

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
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
    fn tool_agent_steer_formats_all_priorities() {
        assert_eq!(
            AgentSteerPriority::Info.format_message("heads up"),
            "[Planner FYI] heads up"
        );
        assert_eq!(
            AgentSteerPriority::Steer.format_message("add tests"),
            "[Planner STEER] add tests\n\nPlease adapt based on this and continue."
        );
        assert_eq!(
            AgentSteerPriority::Urgent.format_message("stop"),
            "[Planner URGENT] stop\n\nStop current action and address this."
        );
    }

    #[tokio::test]
    async fn tool_agent_steer_rejects_non_planner_role() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string())),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(SessionState::Idle));
        let ccx = planner_ccx(gcx, mock.clone(), "agents").await;
        let mut tool = ToolAgentSteer::new();

        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("card_id", json!("T-29")), ("message", json!("add tests"))]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by the task planner"));
        assert!(mock.pushed_commands().is_empty());
    }

    #[tokio::test]
    async fn tool_agent_steer_rejects_card_not_doing() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("planned", Some("agent-chat-1".to_string())),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(SessionState::Idle));
        let ccx = planner_ccx(gcx, mock.clone(), "planner").await;
        let mut tool = ToolAgentSteer::new();

        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("card_id", json!("T-29")), ("message", json!("add tests"))]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("must be in 'doing' column"));
        assert!(mock.pushed_commands().is_empty());
    }

    #[tokio::test]
    async fn tool_agent_steer_rejects_missing_agent_chat_id() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), test_card("doing", None)).await;
        let mock = Arc::new(MockChatFacade::new(SessionState::Idle));
        let ccx = planner_ccx(gcx, mock.clone(), "planner").await;
        let mut tool = ToolAgentSteer::new();

        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("card_id", json!("T-29")), ("message", json!("add tests"))]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("has no agent_chat_id"));
        assert!(mock.pushed_commands().is_empty());
    }

    #[tokio::test]
    async fn tool_agent_steer_pushes_user_message_and_updates_card() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string())),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(SessionState::ExecutingTools));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;
        let mut tool = ToolAgentSteer::new();

        let output = tool_output_text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("card_id", json!("T-29")),
                    ("message", json!("Please add a test for empty filter list")),
                ]),
            )
            .await
            .unwrap(),
        );

        let deliveries = mock.deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].0, "agent-chat-1");
        let delivery = &deliveries[0].1;
        assert_eq!(delivery.messages.len(), 1);
        assert_eq!(delivery.messages[0].role, "user");
        assert_eq!(
            delivery.messages[0].content.content_text_only(),
            "[Planner STEER] Please add a test for empty filter list\n\nPlease adapt based on this and continue."
        );
        assert_eq!(
            delivery.push,
            PushMode::Append,
            "omitted push must default to append"
        );
        assert_eq!(delivery.source, "tools.agent_steer");
        assert!(mock.pushed_commands().is_empty());

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-29").unwrap();
        assert!(card.last_heartbeat_at.is_some());
        assert_eq!(card.status_updates.len(), 1);
        assert_eq!(
            card.status_updates[0].message,
            "Planner steered: Please add a test for empty filter list"
        );
        assert!(output.contains("✅ Steered T-29"));
        assert!(output.contains("⚙️ Executing tools"));
    }

    #[tokio::test]
    async fn tool_agent_steer_skips_autocompaction_when_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string())),
        )
        .await;
        let mut tool_result = ChatMessage::new("tool".to_string(), "x".repeat(40_000));
        tool_result.tool_call_id = "call-large".to_string();
        let messages = vec![
            ChatMessage::new("user".to_string(), "hi".to_string()),
            ChatMessage {
                role: "context_file".to_string(),
                content: ChatContent::ContextFiles(vec![refact_core::chat_types::ContextFile {
                    file_name: "foo.rs".to_string(),
                    file_content: "old".to_string(),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            ChatMessage {
                role: "context_file".to_string(),
                content: ChatContent::ContextFiles(vec![refact_core::chat_types::ContextFile {
                    file_name: "foo.rs".to_string(),
                    file_content: "new".to_string(),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            ChatMessage {
                role: "assistant".to_string(),
                tool_calls: Some(vec![refact_core::chat_types::ChatToolCall {
                    id: "call-large".to_string(),
                    tool_type: "function".to_string(),
                    function: refact_core::chat_types::ChatToolFunction {
                        name: "shell".to_string(),
                        arguments: "{}".to_string(),
                    },
                    index: Some(0),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                }]),
                ..Default::default()
            },
            tool_result,
        ];
        let mock = Arc::new(MockChatFacade::with_messages(
            SessionState::Completed,
            messages,
        ));
        *mock.thread.lock().unwrap() = refact_chat_api::ThreadParams {
            auto_compact_enabled: Some(false),
            ..test_agent_thread()
        };
        let ccx = planner_ccx(gcx, mock.clone(), "planner").await;
        let mut tool = ToolAgentSteer::new();

        let output = tool_output_text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("card_id", json!("T-29")),
                    ("message", json!("Continue now")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(mock.updates().is_empty());
        assert!(!output.contains("Context rebuild: queued before resumed generation."));
        assert_eq!(mock.deliveries().len(), 1);
    }

    async fn steer_with(extra: &[(&str, Value)]) -> (Arc<MockChatFacade>, String) {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string())),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(SessionState::ExecutingTools));
        let ccx = planner_ccx(gcx, mock.clone(), "planner").await;
        let mut call_args = vec![("card_id", json!("T-29")), ("message", json!("keep going"))];
        call_args.extend(extra.iter().cloned());

        let output = tool_output_text(
            ToolAgentSteer::new()
                .tool_execute(ccx, &"call".to_string(), &args(&call_args))
                .await
                .unwrap(),
        );
        (mock, output)
    }

    #[tokio::test]
    async fn steer_priority_urgent_still_defaults_to_append() {
        let (mock, output) = steer_with(&[("priority", json!("urgent"))]).await;

        let deliveries = mock.deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(
            deliveries[0].1.push,
            PushMode::Append,
            "priority=urgent must NOT imply preempt"
        );
        assert!(deliveries[0].1.messages[0]
            .content
            .content_text_only()
            .starts_with("[Planner URGENT]"));
        assert!(output.contains("push=append"));
    }

    #[tokio::test]
    async fn steer_propagates_explicit_push_modes() {
        for (raw, expected) in [
            ("preempt", PushMode::Preempt),
            ("append", PushMode::Append),
            ("when_idle", PushMode::WhenIdle),
        ] {
            let (mock, output) = steer_with(&[("push", json!(raw))]).await;
            let deliveries = mock.deliveries();
            assert_eq!(deliveries.len(), 1);
            assert_eq!(deliveries[0].1.push, expected, "push={raw}");
            assert!(output.contains(&format!("push={raw}")));
        }
    }

    #[tokio::test]
    async fn steer_rejects_invalid_push_before_touching_the_board() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string())),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(SessionState::Idle));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let err = ToolAgentSteer::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("card_id", json!("T-29")),
                    ("message", json!("hi")),
                    ("push", json!("immediately")),
                ]),
            )
            .await
            .unwrap_err();

        assert!(err.to_ascii_lowercase().contains("push"), "{err}");
        assert!(mock.deliveries().is_empty());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert!(board.get_card("T-29").unwrap().status_updates.is_empty());
    }

    #[tokio::test]
    async fn steer_reports_not_delivered_when_card_is_rebound_after_reservation() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string())),
        )
        .await;
        let target = CardTarget {
            card_id: "T-29".to_string(),
            title: "Steerable card".to_string(),
            chat_id: "agent-chat-1".to_string(),
        };
        storage::update_board_atomic(gcx.clone(), "task-1", |board| {
            board.get_card_mut("T-29").unwrap().agent_chat_id =
                Some("replacement-chat".to_string());
            Ok(())
        })
        .await
        .unwrap();
        let mock = Arc::new(MockChatFacade::new(SessionState::Idle));

        let report = planner_delivery::deliver_to_card(
            &gcx.chat_ctx(gcx.clone()),
            mock.clone(),
            "task-1",
            &target,
            CardGuard::doing(),
            planner_delivery::single_message_delivery(
                "hello".to_string(),
                PushMode::Append,
                "tools.agent_steer",
                true,
            ),
        )
        .await;

        assert!(!report.reached_agent());
        assert!(report.short_status().contains("agent_chat_id changed"));
        assert!(mock.deliveries().is_empty());
    }
}
