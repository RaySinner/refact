pub use refact_tools::tools::tool_task_broadcast::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{HashMap, HashSet};
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
use refact_runtime_api::ChatCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::planner_delivery::{self, CardGuard, CardTarget, DeliveryReport};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::task_tool_helpers::required_string;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::PushMode;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_context::GlobalContext;
    use refact_chat_api::ChatCommand;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus};
    use crate::tools::tools_description::Tool;
    use refact_core::chat_types::{DeliveryOutcome, PendingDelivery};
    use refact_runtime_api::{
        ChatSessionFacade, ChatSessionSnapshot, ChatSessionUpdate, CreateSessionRequest,
        RuntimeTrajectorySnapshot, SessionState,
    };
    use chrono::Utc;
    use std::sync::Mutex as StdMutex;

    struct MockChatFacade {
        delivered: StdMutex<Vec<(String, PendingDelivery)>>,
        failures: StdMutex<HashMap<String, String>>,
    }

    impl MockChatFacade {
        fn new(failures: &[(&str, &str)]) -> Self {
            Self {
                delivered: StdMutex::new(vec![]),
                failures: StdMutex::new(
                    failures
                        .iter()
                        .map(|(chat_id, error)| ((*chat_id).to_string(), (*error).to_string()))
                        .collect(),
                ),
            }
        }

        fn deliveries(&self) -> Vec<(String, PendingDelivery)> {
            self.delivered.lock().unwrap().clone()
        }

        fn delivered_chat_ids(&self) -> Vec<String> {
            self.deliveries()
                .into_iter()
                .map(|(chat_id, _)| chat_id)
                .collect()
        }
    }

    #[async_trait]
    impl ChatSessionFacade for MockChatFacade {
        async fn session_snapshot(&self, _chat_id: &str) -> Result<ChatSessionSnapshot, String> {
            Ok(ChatSessionSnapshot {
                messages: vec![],
                thread: refact_chat_api::ThreadParams::default(),
                session_state: SessionState::Idle,
                pause_reasons: vec![],
                goal: None,
            })
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
            panic!("task_broadcast must deliver through deliver_messages");
        }

        async fn deliver_messages(
            &self,
            chat_id: &str,
            delivery: PendingDelivery,
        ) -> Result<DeliveryOutcome, String> {
            if let Some(error) = self.failures.lock().unwrap().get(chat_id).cloned() {
                return Err(error);
            }
            self.delivered
                .lock()
                .unwrap()
                .push((chat_id.to_string(), delivery));
            Ok(DeliveryOutcome::Queued)
        }

        async fn session_state(&self, _chat_id: &str) -> Result<Option<SessionState>, String> {
            Ok(Some(SessionState::Idle))
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

    fn test_card(id: &str, title: &str, column: &str, agent_chat_id: Option<&str>) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: title.to_string(),
            column: column.to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: Some("agent-1".to_string()),
            agent_chat_id: agent_chat_id.map(str::to_string),
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

    fn test_card_with_heartbeat(
        id: &str,
        title: &str,
        column: &str,
        agent_chat_id: Option<&str>,
        last_heartbeat_at: &str,
    ) -> BoardCard {
        BoardCard {
            last_heartbeat_at: Some(last_heartbeat_at.to_string()),
            ..test_card(id, title, column, agent_chat_id)
        }
    }

    fn task_meta(cards_total: usize, agents_active: usize) -> TaskMeta {
        let now = Utc::now().to_rfc3339();
        TaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total,
            cards_done: 0,
            cards_failed: 0,
            agents_active,
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
        cards: Vec<BoardCard>,
    ) -> Arc<crate::global_context::GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        let agents_active = cards
            .iter()
            .filter(|card| card.column == "doing" && card.agent_chat_id.is_some())
            .count();
        storage::save_task_meta(
            gcx.clone(),
            "task-1",
            &task_meta(cards.len(), agents_active),
        )
        .await
        .unwrap();
        storage::save_board(
            gcx.clone(),
            "task-1",
            &TaskBoard {
                cards,
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

    fn output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    #[tokio::test]
    async fn tool_task_broadcast_rejects_non_planner_role() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card("T-22", "auto-nudge", "doing", Some("chat-22"))],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[]));
        let ccx = planner_ccx(gcx, mock.clone(), "agents").await;

        let err = ToolTaskBroadcast::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("message", json!("Use API Y"))]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by the task planner"));
        assert!(mock.deliveries().is_empty());
    }

    #[tokio::test]
    async fn tool_task_broadcast_excludes_cards_and_updates_notified_cards() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![
                test_card("T-22", "auto-nudge", "doing", Some("chat-22")),
                test_card("T-23", "documents", "doing", Some("chat-23")),
                test_card("T-29", "check_agents", "doing", Some("chat-29")),
            ],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[]));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let output = output_text(
            ToolTaskBroadcast::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[
                        (
                            "message",
                            json!("API X is now deprecated, use Y in all new code"),
                        ),
                        ("priority", json!("info")),
                        ("exclude_cards", json!(["T-23"])),
                    ]),
                )
                .await
                .unwrap(),
        );

        let deliveries = mock.deliveries();
        assert_eq!(deliveries.len(), 2);
        assert_eq!(mock.delivered_chat_ids(), vec!["chat-22", "chat-29"]);
        for (_, delivery) in deliveries {
            assert_eq!(delivery.messages.len(), 1);
            assert_eq!(
                delivery.messages[0].content.content_text_only(),
                "[Planner FYI] API X is now deprecated, use Y in all new code"
            );
            assert_eq!(delivery.push, PushMode::Append);
            assert_eq!(delivery.source, "tools.task_broadcast");
        }

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(board.get_card("T-22").unwrap().status_updates.len(), 2);
        assert_eq!(
            board.get_card("T-22").unwrap().status_updates[0].message,
            "Broadcast pending: API X is now deprecated, use Y in all new code"
        );
        assert_eq!(
            board.get_card("T-22").unwrap().status_updates[1].message,
            "Broadcast delivered: API X is now deprecated, use Y in all new code"
        );
        assert!(board.get_card("T-22").unwrap().last_heartbeat_at.is_none());
        assert!(board.get_card("T-23").unwrap().status_updates.is_empty());
        assert_eq!(board.get_card("T-29").unwrap().status_updates.len(), 2);
        assert_eq!(
            board.get_card("T-29").unwrap().status_updates[0].message,
            "Broadcast pending: API X is now deprecated, use Y in all new code"
        );
        assert_eq!(
            board.get_card("T-29").unwrap().status_updates[1].message,
            "Broadcast delivered: API X is now deprecated, use Y in all new code"
        );
        assert!(output.contains("📢 Broadcast sent to 2 agents"));
        assert!(output.contains("- T-23 (documents): skipped (in exclude_cards)"));
        assert!(output.contains("- T-22 (auto-nudge): notified"));
        assert!(output.contains("- T-29 (check_agents): notified"));
    }

    #[tokio::test]
    async fn task_broadcast_does_not_update_heartbeat() {
        let temp = tempfile::tempdir().unwrap();
        let initial_heartbeat = "2026-05-22T00:00:00+00:00";
        let gcx = write_task(
            temp.path(),
            vec![test_card_with_heartbeat(
                "T-22",
                "auto-nudge",
                "doing",
                Some("chat-22"),
                initial_heartbeat,
            )],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[]));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        ToolTaskBroadcast::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("message", json!("Use API Y"))]),
            )
            .await
            .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(
            board.get_card("T-22").unwrap().last_heartbeat_at.as_deref(),
            Some(initial_heartbeat)
        );
    }

    #[tokio::test]
    async fn task_broadcast_appends_delivered_status_on_success() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card("T-22", "auto-nudge", "doing", Some("chat-22"))],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[]));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        ToolTaskBroadcast::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("message", json!("Use API Y"))]),
            )
            .await
            .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let status_updates = &board.get_card("T-22").unwrap().status_updates;
        assert_eq!(status_updates.len(), 2);
        assert_eq!(status_updates[0].message, "Broadcast pending: Use API Y");
        assert_eq!(status_updates[1].message, "Broadcast delivered: Use API Y");
    }

    #[tokio::test]
    async fn tool_task_broadcast_empty_doing_list_returns_no_running_agents() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![
                test_card("T-22", "auto-nudge", "planned", Some("chat-22")),
                test_card("T-23", "documents", "doing", None),
            ],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[]));
        let ccx = planner_ccx(gcx, mock.clone(), "planner").await;

        let output = output_text(
            ToolTaskBroadcast::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[("message", json!("Use API Y"))]),
                )
                .await
                .unwrap(),
        );

        assert!(mock.deliveries().is_empty());
        assert!(output.contains("No running agents"));
    }

    #[tokio::test]
    async fn tool_task_broadcast_skips_card_finished_before_push() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card("T-22", "auto-nudge", "doing", Some("chat-22"))],
        )
        .await;
        storage::update_board_atomic(gcx.clone(), "task-1", |board| {
            let card = board.get_card_mut("T-22").unwrap();
            card.column = "done".to_string();
            card.completed_at = Some(Utc::now().to_rfc3339());
            Ok(())
        })
        .await
        .unwrap();
        let mock = Arc::new(MockChatFacade::new(&[]));
        let target = CardTarget {
            card_id: "T-22".to_string(),
            title: "auto-nudge".to_string(),
            chat_id: "chat-22".to_string(),
        };

        let (result, delivered) = broadcast_to_target(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            target,
            "Use API Y",
            &BroadcastPriority::Steer.format_message("Use API Y"),
            PushMode::Append,
            mock.clone(),
        )
        .await;
        let output = format_output(&[result], 0, "Use API Y");

        assert!(!delivered);

        assert!(mock.deliveries().is_empty());
        assert!(output.contains("📢 Broadcast sent to 0 agents"));
        assert!(output.contains("- T-22 (auto-nudge): skipped (card T-22 is now in column 'done')"));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-22").unwrap();
        assert_eq!(card.column, "done");
        assert!(card.status_updates.is_empty());
    }

    #[tokio::test]
    async fn tool_task_broadcast_skips_agent_chat_id_changed_before_push() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card("T-22", "auto-nudge", "doing", Some("chat-22"))],
        )
        .await;
        storage::update_board_atomic(gcx.clone(), "task-1", |board| {
            let card = board.get_card_mut("T-22").unwrap();
            card.agent_chat_id = Some("replacement-chat".to_string());
            Ok(())
        })
        .await
        .unwrap();
        let mock = Arc::new(MockChatFacade::new(&[]));
        let target = CardTarget {
            card_id: "T-22".to_string(),
            title: "auto-nudge".to_string(),
            chat_id: "chat-22".to_string(),
        };

        let (result, delivered) = broadcast_to_target(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            target,
            "Use API Y",
            &BroadcastPriority::Steer.format_message("Use API Y"),
            PushMode::Append,
            mock.clone(),
        )
        .await;
        let output = format_output(&[result], 0, "Use API Y");

        assert!(!delivered);

        assert!(mock.deliveries().is_empty());
        assert!(output.contains("📢 Broadcast sent to 0 agents"));
        assert!(output.contains("- T-22 (auto-nudge): skipped (agent_chat_id changed"));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-22").unwrap();
        assert_eq!(card.agent_chat_id.as_deref(), Some("replacement-chat"));
        assert!(card.status_updates.is_empty());
    }

    #[tokio::test]
    async fn tool_task_broadcast_reports_push_failures_without_aborting() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![
                test_card("T-22", "auto-nudge", "doing", Some("chat-22")),
                test_card("T-29", "check_agents", "doing", Some("chat-29")),
            ],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[("chat-29", "queue unavailable")]));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let output = output_text(
            ToolTaskBroadcast::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[("message", json!("Use API Y"))]),
                )
                .await
                .unwrap(),
        );

        assert_eq!(mock.delivered_chat_ids(), vec!["chat-22"]);
        assert!(output.contains("📢 Broadcast sent to 1 agent"));
        assert!(output.contains("- T-22 (auto-nudge): notified"));
        assert!(
            output.contains("- T-29 (check_agents): failed")
                && output.contains("queue unavailable"),
            "{output}"
        );

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(board.get_card("T-22").unwrap().status_updates.len(), 2);
        assert_eq!(
            board.get_card("T-22").unwrap().status_updates[0].message,
            "Broadcast pending: Use API Y"
        );
        assert_eq!(
            board.get_card("T-22").unwrap().status_updates[1].message,
            "Broadcast delivered: Use API Y"
        );
        assert_eq!(board.get_card("T-29").unwrap().status_updates.len(), 2);
        assert_eq!(
            board.get_card("T-29").unwrap().status_updates[0].message,
            "Broadcast pending: Use API Y"
        );
        let failed_status = &board.get_card("T-29").unwrap().status_updates[1].message;
        assert!(
            failed_status.starts_with("Broadcast not delivered")
                && failed_status.contains("queue unavailable"),
            "failed broadcast must not read as delivered, got: {failed_status}"
        );
    }

    #[tokio::test]
    async fn task_broadcast_priority_urgent_still_defaults_to_append() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card("T-22", "auto-nudge", "doing", Some("chat-22"))],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[]));
        let ccx = planner_ccx(gcx, mock.clone(), "planner").await;

        ToolTaskBroadcast::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("message", json!("Use API Y")),
                    ("priority", json!("urgent")),
                ]),
            )
            .await
            .unwrap();

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
    }

    #[tokio::test]
    async fn task_broadcast_propagates_explicit_push_to_every_target() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![
                test_card("T-22", "auto-nudge", "doing", Some("chat-22")),
                test_card("T-29", "check_agents", "doing", Some("chat-29")),
            ],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[]));
        let ccx = planner_ccx(gcx, mock.clone(), "planner").await;

        ToolTaskBroadcast::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("message", json!("Stop and re-read the plan")),
                    ("push", json!("preempt")),
                ]),
            )
            .await
            .unwrap();

        let deliveries = mock.deliveries();
        assert_eq!(deliveries.len(), 2);
        for (_, delivery) in deliveries {
            assert_eq!(delivery.push, PushMode::Preempt);
        }
    }

    #[tokio::test]
    async fn task_broadcast_rejects_invalid_push_before_any_delivery() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card("T-22", "auto-nudge", "doing", Some("chat-22"))],
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(&[]));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let err = ToolTaskBroadcast::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("message", json!("hi")), ("push", json!("now"))]),
            )
            .await
            .unwrap_err();

        assert!(err.to_ascii_lowercase().contains("push"), "{err}");
        assert!(mock.deliveries().is_empty());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert!(board.get_card("T-22").unwrap().status_updates.is_empty());
    }
}
