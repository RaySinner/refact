pub use refact_tools::tools::tool_agent_lifecycle::*;

#[cfg(test)]
use std::process::Command;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
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
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
use refact_tasks::types::BoardCard;
#[cfg(test)]
use crate::tools::planner_delivery::{self, CardGuard, CardTarget};
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_worktrees::service::WorktreeService;
#[cfg(test)]
use refact_chat_api::ChatCommand;
#[cfg(test)]
use refact_runtime_api::ChatSessionFacade;
#[cfg(test)]
use refact_runtime_api::SessionState;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::chat_types::{DeliveryOutcome, PendingDelivery, PushMode};
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{TaskBoard, TaskMeta, TaskStatus};
    use crate::tools::tools_description::Tool;
    use refact_runtime_api::{
        ChatSessionSnapshot, ChatSessionUpdate, CreateSessionRequest, RuntimeTrajectorySnapshot,
    };
    use std::sync::Mutex as StdMutex;

    struct MockChatFacade {
        state: StdMutex<Option<SessionState>>,
        pushed: StdMutex<Vec<(String, ChatCommand)>>,
        delivered: StdMutex<Vec<(String, PendingDelivery)>>,
        mutate_on_probe: StdMutex<Option<(Arc<GlobalContext>, bool)>>,
    }

    impl MockChatFacade {
        fn new(state: Option<SessionState>) -> Self {
            Self {
                state: StdMutex::new(state),
                pushed: StdMutex::new(vec![]),
                delivered: StdMutex::new(vec![]),
                mutate_on_probe: StdMutex::new(None),
            }
        }

        fn deliveries(&self) -> Vec<(String, PendingDelivery)> {
            self.delivered.lock().unwrap().clone()
        }

        fn pushed_commands(&self) -> Vec<(String, ChatCommand)> {
            self.pushed.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ChatSessionFacade for MockChatFacade {
        async fn session_snapshot(&self, _chat_id: &str) -> Result<ChatSessionSnapshot, String> {
            Ok(ChatSessionSnapshot {
                messages: vec![],
                thread: refact_chat_api::ThreadParams::default(),
                session_state: self.state.lock().unwrap().unwrap_or(SessionState::Idle),
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
            let mutation = self.mutate_on_probe.lock().unwrap().take();
            if let Some((gcx, rebind)) = mutation {
                storage::update_board_atomic(gcx, "task-1", move |board| {
                    let card = board.get_card_mut("T-39").unwrap();
                    if rebind {
                        card.agent_chat_id = Some("replacement-chat".to_string());
                    } else {
                        card.column = "done".to_string();
                    }
                    Ok(())
                })
                .await?;
            }
            Ok(*self.state.lock().unwrap())
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

    fn test_card(
        column: &str,
        agent_chat_id: Option<String>,
        worktree: Option<String>,
    ) -> BoardCard {
        BoardCard {
            id: "T-39".to_string(),
            title: "Lifecycle card".to_string(),
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
            agent_worktree: worktree,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    fn run_git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));
        if !output.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn init_repo(root: &Path) {
        run_git(root, &["init"]);
        run_git(root, &["checkout", "-b", "main"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        run_git(root, &["add", "file.txt"]);
        run_git(root, &["commit", "-m", "initial"]);
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

    fn output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    #[test]
    fn tool_agent_lifecycle_descriptions_are_planner_only() {
        let cancel = ToolCancelAgent::new().tool_description();
        let pause = ToolPauseAgent::new().tool_description();
        let resume = ToolResumeAgent::new().tool_description();

        assert_eq!(cancel.name, "cancel_agent");
        assert_eq!(pause.name, "pause_agent");
        assert_eq!(resume.name, "resume_agent");
        assert!(cancel.description.contains("Planner-only"));
        assert!(pause.description.contains("Planner-only"));
        assert!(resume.description.contains("Planner-only"));
        assert_eq!(
            cancel.input_schema["required"],
            json!(["card_id", "reason"])
        );
        assert_eq!(pause.input_schema["required"], json!(["card_id", "reason"]));
        assert_eq!(resume.input_schema["required"], json!(["card_id"]));
        assert!(cancel.input_schema["properties"]
            .get("retain_worktree")
            .is_some());
    }

    #[tokio::test]
    async fn tool_agent_lifecycle_rejects_non_planner_role() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string()), None),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(Some(SessionState::Idle)));
        let ccx = planner_ccx(gcx, mock.clone(), "agents").await;
        let call_id = "call".to_string();

        let err = ToolCancelAgent::new()
            .tool_execute(
                ccx.clone(),
                &call_id,
                &args(&[("card_id", json!("T-39")), ("reason", json!("stop"))]),
            )
            .await
            .unwrap_err();
        assert!(err.contains("can only be called by the task planner"));

        let err = ToolPauseAgent::new()
            .tool_execute(
                ccx.clone(),
                &call_id,
                &args(&[("card_id", json!("T-39")), ("reason", json!("wait"))]),
            )
            .await
            .unwrap_err();
        assert!(err.contains("can only be called by the task planner"));

        let err = ToolResumeAgent::new()
            .tool_execute(ccx, &call_id, &args(&[("card_id", json!("T-39"))]))
            .await
            .unwrap_err();
        assert!(err.contains("can only be called by the task planner"));
        assert!(mock.pushed_commands().is_empty());
    }

    #[tokio::test]
    async fn lifecycle_tools_reject_planned_done_failed_cards() {
        for column in ["planned", "done", "failed"] {
            let temp = tempfile::tempdir().unwrap();
            let gcx = write_task(
                temp.path(),
                test_card(column, Some("agent-chat-1".to_string()), None),
            )
            .await;
            let mock = Arc::new(MockChatFacade::new(Some(SessionState::Idle)));
            let ccx = planner_ccx(gcx, mock.clone(), "planner").await;
            let call_id = "call".to_string();

            let err = ToolCancelAgent::new()
                .tool_execute(
                    ccx.clone(),
                    &call_id,
                    &args(&[("card_id", json!("T-39")), ("reason", json!("stop"))]),
                )
                .await
                .unwrap_err();
            assert!(err.to_ascii_lowercase().contains(column), "{err}");

            let err = ToolPauseAgent::new()
                .tool_execute(
                    ccx.clone(),
                    &call_id,
                    &args(&[("card_id", json!("T-39")), ("reason", json!("wait"))]),
                )
                .await
                .unwrap_err();
            assert!(err.to_ascii_lowercase().contains(column), "{err}");

            let err = ToolResumeAgent::new()
                .tool_execute(ccx, &call_id, &args(&[("card_id", json!("T-39"))]))
                .await
                .unwrap_err();
            assert!(err.to_ascii_lowercase().contains(column), "{err}");
            assert!(mock.pushed_commands().is_empty());
        }
    }

    #[tokio::test]
    async fn cancel_agent_retains_worktree_by_default_and_pushes_abort() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("agent-worktree");
        tokio::fs::create_dir_all(&worktree).await.unwrap();
        let worktree_str = worktree.to_string_lossy().to_string();
        let gcx = write_task(
            temp.path(),
            test_card(
                "doing",
                Some("agent-chat-1".to_string()),
                Some(worktree_str.clone()),
            ),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(Some(SessionState::Generating)));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let output = output_text(
            ToolCancelAgent::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[
                        ("card_id", json!("T-39")),
                        ("reason", json!("wrong branch")),
                    ]),
                )
                .await
                .unwrap(),
        );

        let pushed = mock.pushed_commands();
        assert_eq!(pushed.len(), 1);
        assert_eq!(pushed[0].0, "agent-chat-1");
        assert!(matches!(pushed[0].1, ChatCommand::Abort {}));
        assert!(
            mock.deliveries().is_empty(),
            "cancel must remain an abort control command"
        );
        assert!(output.contains("Worktree retained"));

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-39").unwrap();
        assert_eq!(card.column, "failed");
        assert_eq!(
            card.final_report.as_deref(),
            Some("Cancelled: wrong branch")
        );
        assert_eq!(card.agent_worktree.as_deref(), Some(worktree_str.as_str()));
        assert!(worktree.exists());
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message == "Cancelled by planner: wrong branch"));
    }

    #[tokio::test]
    async fn cancel_agent_cleanup_removes_worktree_when_requested() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = write_task(
            &source,
            test_card("doing", Some("agent-chat-1".to_string()), None),
        )
        .await;
        let service =
            WorktreeService::new(gcx.cache_dir.clone(), source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(refact_worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-39/agent".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-39".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let worktree = created.worktree.meta.root.clone();
        storage::update_board_atomic(gcx.clone(), "task-1", {
            let worktree = worktree.clone();
            let branch = created.worktree.meta.branch.clone();
            let name = created.worktree.meta.id.clone();
            move |board| {
                let card = board.get_card_mut("T-39").unwrap();
                card.agent_worktree = Some(worktree.to_string_lossy().to_string());
                card.agent_branch = branch.clone();
                card.agent_worktree_name = Some(name.clone());
                Ok(())
            }
        })
        .await
        .unwrap();
        let mock = Arc::new(MockChatFacade::new(Some(SessionState::ExecutingTools)));
        let ccx = planner_ccx(gcx.clone(), mock, "planner").await;

        let output = output_text(
            ToolCancelAgent::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[
                        ("card_id", json!("T-39")),
                        ("reason", json!("superseded")),
                        ("retain_worktree", json!(false)),
                    ]),
                )
                .await
                .unwrap(),
        );

        assert!(output.contains("Worktree cleanup completed"));
        assert!(!worktree.exists());
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_err());
        assert!(run_git(
            &source,
            &["branch", "--list", "refact/task/task-1/card/T-39/agent"]
        )
        .trim()
        .is_empty());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-39").unwrap();
        assert_eq!(card.column, "failed");
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(card.agent_branch.is_none());
    }

    #[tokio::test]
    async fn cleanup_refuses_path_outside_worktree_cache() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside-worktree");
        tokio::fs::create_dir_all(&outside).await.unwrap();
        let mut card = test_card(
            "doing",
            Some("agent-chat-1".to_string()),
            Some(outside.to_string_lossy().to_string()),
        );
        card.agent_branch = Some("refact/task/task-1/card/T-39/agent".to_string());
        let gcx = write_task(temp.path(), card).await;
        let mock = Arc::new(MockChatFacade::new(Some(SessionState::ExecutingTools)));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let err = ToolCancelAgent::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("card_id", json!("T-39")),
                    ("reason", json!("unsafe")),
                    ("retain_worktree", json!(false)),
                ]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("outside Refact worktree cache"), "{err}");
        assert!(outside.exists());
        assert!(mock.pushed_commands().is_empty());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(board.get_card("T-39").unwrap().column, "doing");
    }

    #[tokio::test]
    async fn cleanup_refuses_unsafe_branch_names() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let cache_dir = temp.path().join("cache");
        let config_dir = temp.path().join("config");
        let gcx =
            crate::global_context::tests::make_test_gcx_with_dirs(cache_dir, config_dir).await;
        let service =
            WorktreeService::new(gcx.cache_dir.clone(), source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(refact_worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-39/agent".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-39".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut card = test_card(
            "doing",
            Some("agent-chat-1".to_string()),
            Some(created.worktree.meta.root.to_string_lossy().to_string()),
        );
        card.agent_branch = Some("main".to_string());
        card.agent_worktree_name = Some(created.worktree.meta.id.clone());
        let task_dir = source.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
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
        let mock = Arc::new(MockChatFacade::new(Some(SessionState::ExecutingTools)));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let err = ToolCancelAgent::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("card_id", json!("T-39")),
                    ("reason", json!("unsafe branch")),
                    ("retain_worktree", json!(false)),
                ]),
            )
            .await
            .unwrap_err();

        assert!(
            err.contains("unsafe branch") || err.contains("Recorded branch"),
            "{err}"
        );
        assert!(created.worktree.meta.root.exists());
        assert!(mock.pushed_commands().is_empty());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(board.get_card("T-39").unwrap().column, "doing");
    }

    #[tokio::test]
    async fn push_guard_rejects_stale_agent_chat_id() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-2".to_string()), None),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(Some(SessionState::ExecutingTools)));

        let report = push_if_card_current_and_live(
            gcx.clone(),
            "task-1",
            "T-39",
            "agent-chat-1",
            true,
            mock.clone(),
            ChatCommand::Abort {},
        )
        .await
        .unwrap();

        assert!(!report.pushed);
        assert!(report
            .skipped_reason
            .as_deref()
            .unwrap()
            .contains("agent_chat_id changed"));
        assert!(mock.pushed_commands().is_empty());
    }

    #[tokio::test]
    async fn delivery_revalidates_card_after_live_session_probe() {
        for rebind in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let gcx = write_task(
                temp.path(),
                test_card("doing", Some("agent-chat-1".to_string()), None),
            )
            .await;
            let mock = Arc::new(MockChatFacade::new(Some(SessionState::Idle)));
            *mock.mutate_on_probe.lock().unwrap() = Some((gcx.clone(), rebind));
            let target = CardTarget {
                card_id: "T-39".to_string(),
                title: "card".to_string(),
                chat_id: "agent-chat-1".to_string(),
            };
            let report = planner_delivery::deliver_to_card(
                &gcx.chat_ctx(gcx.clone()),
                mock.clone(),
                "task-1",
                &target,
                CardGuard::doing_with_live_session(),
                planner_delivery::single_message_delivery(
                    "hello".to_string(),
                    PushMode::Append,
                    "test",
                    true,
                ),
            )
            .await;
            assert!(!report.reached_agent());
            assert!(report.short_status().contains(if rebind {
                "agent_chat_id changed"
            } else {
                "now in column 'done'"
            }));
            assert!(mock.deliveries().is_empty());
            assert!(mock.pushed_commands().is_empty());
        }
    }

    #[tokio::test]
    async fn lifecycle_pause_resume_propagate_all_push_modes() {
        for (raw, expected) in [
            ("preempt", PushMode::Preempt),
            ("append", PushMode::Append),
            ("when_idle", PushMode::WhenIdle),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let gcx = write_task(
                temp.path(),
                test_card("doing", Some("agent-chat-1".to_string()), None),
            )
            .await;
            let mock = Arc::new(MockChatFacade::new(Some(SessionState::ExecutingTools)));
            let ccx = planner_ccx(gcx, mock.clone(), "planner").await;
            let call_args = args(&[
                ("card_id", json!("T-39")),
                ("reason", json!("review")),
                ("push", json!(raw)),
            ]);
            ToolPauseAgent::new()
                .tool_execute(ccx.clone(), &"pause".to_string(), &call_args)
                .await
                .unwrap();
            ToolResumeAgent::new()
                .tool_execute(ccx, &"resume".to_string(), &call_args)
                .await
                .unwrap();
            let deliveries = mock.deliveries();
            assert_eq!(deliveries.len(), 2);
            for (_, delivery) in &deliveries {
                assert_eq!(delivery.push, expected);
                assert!(delivery.wake);
                assert_eq!(delivery.messages.len(), 1);
                assert_eq!(delivery.messages[0].role, "user");
            }
            assert_eq!(deliveries[0].1.source, "tools.pause_agent");
            assert_eq!(deliveries[1].1.source, "tools.resume_agent");
            assert!(mock.pushed_commands().is_empty());
        }
    }

    #[tokio::test]
    async fn pause_agent_pushes_pause_message_and_updates_card() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string()), None),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(Some(SessionState::ExecutingTools)));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let output = output_text(
            ToolPauseAgent::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[("card_id", json!("T-39")), ("reason", json!("need review"))]),
                )
                .await
                .unwrap(),
        );

        let deliveries = mock.deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].0, "agent-chat-1");
        assert_eq!(deliveries[0].1.push, PushMode::Append);
        assert!(deliveries[0].1.wake);
        assert_eq!(
            deliveries[0].1.messages[0].content.content_text_only(),
            "[Planner PAUSE] need review\n\nWait for resume signal."
        );
        assert!(mock.pushed_commands().is_empty());
        assert!(output.contains("use resume_agent to continue"));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-39").unwrap();
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message == "Paused by planner: need review"));
    }

    #[tokio::test]
    async fn resume_agent_pushes_resume_message_and_updates_card() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            test_card("doing", Some("agent-chat-1".to_string()), None),
        )
        .await;
        let mock = Arc::new(MockChatFacade::new(Some(SessionState::Idle)));
        let ccx = planner_ccx(gcx.clone(), mock.clone(), "planner").await;

        let output = output_text(
            ToolResumeAgent::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[
                        ("card_id", json!("T-39")),
                        ("note", json!("Continue with tests")),
                    ]),
                )
                .await
                .unwrap(),
        );

        let deliveries = mock.deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].0, "agent-chat-1");
        assert_eq!(deliveries[0].1.push, PushMode::Append);
        assert!(deliveries[0].1.wake);
        assert_eq!(
            deliveries[0].1.messages[0].content.content_text_only(),
            "[Planner RESUME]\n\nContinue with tests"
        );
        assert!(mock.pushed_commands().is_empty());
        assert!(output.contains("Agent state: idle"));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-39").unwrap();
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message == "Resumed by planner"));
    }
}
