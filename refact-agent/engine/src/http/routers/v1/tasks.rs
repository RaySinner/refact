pub use refact_http::routers::v1::tasks::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashSet;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::Ordering;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use axum::extract::Path;
#[cfg(test)]
#[allow(unused_imports)]
use axum::extract::Query;
#[cfg(test)]
#[allow(unused_imports)]
use axum::extract::State;
#[cfg(test)]
#[allow(unused_imports)]
use axum::http::{Response, StatusCode};
#[cfg(test)]
#[allow(unused_imports)]
use axum::response::Json;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::Utc;
#[cfg(test)]
#[allow(unused_imports)]
use hyper::Body;
#[cfg(test)]
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::broadcast;
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::actor::{make_runtime_event, BuddyMutation};
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::types::BuddyActivity;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::types::{ChatSession, SessionState};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::SessionsMap;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_history::trajectory_event::TrajectoryEvent;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_history::trajectory_index::{
    remove_trajectory_index_entry_with_rollout, TrajectoryIndexCoordinator,
    TrajectoryIndexListingCaller,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_history::trajectory_paths::validate_trajectory_id;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::custom_error::ScratchError;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::chat_ctx::ChatCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_comments::create_card_comment_for_ctx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage_runtime as storage;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::trajectory_runtime::{
    find_trajectory_path, save_initial_task_chat_trajectory, save_trajectory_snapshot,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use refact_tasks::comments::CreateCardComment;
#[cfg(test)]
#[allow(unused_imports)]
use refact_tasks::events::{TaskEvent, TaskEventEnvelope};
#[cfg(test)]
#[allow(unused_imports)]
use refact_tasks::types::{
    BoardCard, StatusUpdate, TaskBoard, TaskMeta, TaskStatus, TeamMember, TrajectoryInfo,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_tools::tools::tool_task_documents::CreateDocumentRequest;
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::service::WorktreeService;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::trajectories::save_trajectory_snapshot;
    use crate::global_context::GlobalContext;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use refact_chat_api::{ChatMessage, TaskMeta as ChatTaskMeta};
    use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
    use uuid::Uuid;

    async fn setup_task(root: &std::path::Path, task_id: &str) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        let task_dir = root.join(".refact/tasks").join(task_id);
        tokio::fs::create_dir_all(task_dir.join("trajectories/planner"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(task_dir.join("trajectories/agents/agent-1"))
            .await
            .unwrap();
        let now = Utc::now().to_rfc3339();
        let meta = TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: task_id.to_string(),
            status: TaskStatus::Planning,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 0,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        storage::save_task_meta(gcx.clone(), task_id, &meta)
            .await
            .unwrap();
        storage::save_board(gcx.clone(), task_id, &TaskBoard::default())
            .await
            .unwrap();
        gcx
    }

    fn app(gcx: Arc<GlobalContext>) -> AppState {
        gcx.app_state(gcx.clone())
    }

    #[tokio::test]
    async fn handle_create_task_with_target_files_returns_updated_card_counts() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];

        let Json(meta) = handle_create_task(
            State(app(gcx.clone())),
            Json(CreateTaskRequest {
                name: "Task with targets".to_string(),
                target_files: vec!["src/lib.rs".to_string()],
            }),
        )
        .await
        .unwrap();

        assert_eq!(meta.cards_total, 1);
        let tasks_dir = temp.path().join(".refact/tasks");
        let index = crate::tasks::index::read_task_index(&tasks_dir)
            .await
            .unwrap()
            .unwrap();
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.id == meta.id)
            .unwrap();
        assert_eq!(entry.meta.cards_total, 1);
    }

    #[tokio::test]
    async fn create_planner_chat_with_mode_lists_with_that_mode() {
        let temp = tempfile::tempdir().unwrap();
        let task_id = "task-mode";
        let gcx = setup_task(temp.path(), task_id).await;

        let Json(default_res) = handle_create_planner_chat(
            State(app(gcx.clone())),
            Path(task_id.to_string()),
            hyper::body::Bytes::new(),
        )
        .await
        .unwrap();
        assert_eq!(default_res["mode"], json!("task_planner"));

        let Json(agent_res) = handle_create_planner_chat(
            State(app(gcx.clone())),
            Path(task_id.to_string()),
            hyper::body::Bytes::from_static(br#"{"mode":"agent"}"#),
        )
        .await
        .unwrap();
        assert_eq!(agent_res["mode"], json!("agent"));
        let agent_chat_id = agent_res["chat_id"].as_str().unwrap().to_string();

        let rejected = handle_create_planner_chat(
            State(app(gcx.clone())),
            Path(task_id.to_string()),
            hyper::body::Bytes::from_static(br#"{"mode":"task_agent"}"#),
        )
        .await
        .unwrap_err();
        assert_eq!(rejected.0, StatusCode::BAD_REQUEST);

        let Json(list) = handle_list_task_trajectories(
            State(app(gcx.clone())),
            Path((task_id.to_string(), "planner".to_string())),
        )
        .await
        .unwrap();
        let agent_entry = list
            .iter()
            .find(|t| t.id == agent_chat_id)
            .expect("agent-mode chat should be listed in the planner bucket");
        assert_eq!(agent_entry.mode.as_deref(), Some("agent"));
    }

    fn snapshot(
        chat_id: &str,
        task_id: &str,
        role: &str,
        agent_id: Option<&str>,
        card_id: Option<&str>,
        planner_chat_id: Option<&str>,
    ) -> TrajectorySnapshot {
        TrajectorySnapshot {
            goal: None,
            goal_ledger: Vec::new(),
            goal_verification_blocked_until_ms: None,
            chat_id: chat_id.to_string(),
            title: chat_id.to_string(),
            model: "test-model".to_string(),
            mode: "task_planner".to_string(),
            tool_use: "agent".to_string(),
            messages: vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            created_at: Utc::now().to_rfc3339(),
            boost_reasoning: false,
            checkpoints_enabled: false,
            context_tokens_cap: None,
            auto_compression_cap: None,
            auto_compression_cap_pending: true,
            include_project_info: false,
            is_title_generated: false,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            autonomous_no_confirm: false,
            version: 1,
            task_meta: Some(ChatTaskMeta {
                task_id: task_id.to_string(),
                role: role.to_string(),
                agent_id: agent_id.map(str::to_string),
                card_id: card_id.map(str::to_string),
                planner_chat_id: planner_chat_id
                    .map(str::to_string)
                    .or_else(|| (role == "planner").then(|| chat_id.to_string())),
            }),
            worktree: None,
            parent_id: None,
            link_type: None,
            root_chat_id: None,
            reasoning_effort: None,
            thinking_budget: None,
            temperature: None,
            frequency_penalty: None,
            max_tokens: None,
            parallel_tool_calls: None,
            previous_response_id: None,
            active_skill: None,
            auto_enrichment_enabled: None,
            buddy_meta: None,
            auto_compact_enabled: None,
            frozen_request_prefix: None,
            claude_code_identity: None,
            reactive_compact_attempts: None,
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
            pending_deliveries: Vec::new(),
        }
    }

    async fn save_snapshot(gcx: Arc<GlobalContext>, snapshot: TrajectorySnapshot) {
        save_trajectory_snapshot(gcx, snapshot).await.unwrap();
    }

    async fn save_planner(gcx: Arc<GlobalContext>, task_id: &str, chat_id: &str) {
        save_snapshot(gcx, snapshot(chat_id, task_id, "planner", None, None, None)).await;
    }

    async fn save_agent(
        gcx: Arc<GlobalContext>,
        task_id: &str,
        chat_id: &str,
        agent_id: &str,
        card_id: Option<&str>,
        planner_chat_id: &str,
    ) {
        save_snapshot(
            gcx,
            snapshot(
                chat_id,
                task_id,
                "agents",
                Some(agent_id),
                card_id,
                Some(planner_chat_id),
            ),
        )
        .await;
    }

    async fn insert_live_agent_session(
        gcx: Arc<GlobalContext>,
        task_id: &str,
        chat_id: &str,
        agent_id: &str,
        card_id: Option<&str>,
        planner_chat_id: &str,
    ) {
        let mut session = crate::chat::types::ChatSession::new(chat_id.to_string());
        session.thread.task_meta = Some(ChatTaskMeta {
            task_id: task_id.to_string(),
            role: "agents".to_string(),
            agent_id: Some(agent_id.to_string()),
            card_id: card_id.map(str::to_string),
            planner_chat_id: Some(planner_chat_id.to_string()),
        });
        gcx.chat_sessions.write().await.insert(
            chat_id.to_string(),
            Arc::new(tokio::sync::Mutex::new(session)),
        );
    }

    async fn insert_live_planner_session_with_state(
        gcx: Arc<GlobalContext>,
        task_id: &str,
        chat_id: &str,
        state: crate::chat::types::SessionState,
    ) {
        let mut session = crate::chat::types::ChatSession::new(chat_id.to_string());
        session.thread.task_meta = Some(ChatTaskMeta {
            task_id: task_id.to_string(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: Some(chat_id.to_string()),
        });
        session.set_runtime_state(state, None);
        gcx.chat_sessions.write().await.insert(
            chat_id.to_string(),
            Arc::new(tokio::sync::Mutex::new(session)),
        );
    }

    async fn insert_live_planner_session(gcx: Arc<GlobalContext>, task_id: &str, chat_id: &str) {
        insert_live_planner_session_with_state(
            gcx,
            task_id,
            chat_id,
            crate::chat::types::SessionState::Idle,
        )
        .await;
    }

    fn status<T>(result: Result<Json<T>, (StatusCode, String)>) -> StatusCode {
        match result {
            Ok(_) => panic!("expected request to fail"),
            Err((status, _)) => status,
        }
    }

    fn delete_status(result: Result<Json<Value>, (StatusCode, Json<Value>)>) -> StatusCode {
        match result {
            Ok(_) => panic!("expected request to fail"),
            Err((status, _)) => status,
        }
    }

    fn delete_error_body(
        result: Result<Json<Value>, (StatusCode, Json<Value>)>,
    ) -> (StatusCode, Value) {
        match result {
            Ok(_) => panic!("expected request to fail"),
            Err((status, Json(body))) => (status, body),
        }
    }

    fn no_force() -> Query<DeletePlannerChatQuery> {
        Query(DeletePlannerChatQuery { force: false })
    }

    fn force() -> Query<DeletePlannerChatQuery> {
        Query(DeletePlannerChatQuery { force: true })
    }

    fn create_card_request(rev: u64, id: &str) -> UpdateBoardRequest {
        UpdateBoardRequest {
            rev,
            patch: BoardPatch::CreateCard {
                id: id.to_string(),
                title: format!("Card {}", id),
                priority: None,
                depends_on: Vec::new(),
                instructions: String::new(),
                target_files: Vec::new(),
            },
        }
    }

    #[tokio::test]
    async fn handle_patch_board_serial_patches_increment_rev() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-serial").await;

        let first = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-serial".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(first.rev, 1);
        assert!(first.get_card("card-a").is_some());

        let second = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-serial".to_string()),
            Json(create_card_request(1, "card-b")),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(second.rev, 2);
        assert!(second.get_card("card-a").is_some());
        assert!(second.get_card("card-b").is_some());
    }

    #[tokio::test]
    async fn handle_patch_board_concurrent_patches_one_succeeds_one_409() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-concurrent").await;
        let mut board = storage::load_board(gcx.clone(), "task-concurrent")
            .await
            .unwrap();
        board.rev = 5;
        storage::save_board(gcx.clone(), "task-concurrent", &board)
            .await
            .unwrap();

        static GATE_READY: AtomicUsize = AtomicUsize::new(0);
        GATE_READY.store(0, AtomicOrdering::SeqCst);
        async fn wait_for_both_ready() {
            GATE_READY.fetch_add(1, AtomicOrdering::SeqCst);
            while GATE_READY.load(AtomicOrdering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        }

        let first = {
            let gcx = gcx.clone();
            tokio::spawn(async move {
                wait_for_both_ready().await;
                handle_patch_board(
                    State(app(gcx)),
                    Path("task-concurrent".to_string()),
                    Json(create_card_request(5, "card-a")),
                )
                .await
            })
        };
        let second = {
            let gcx = gcx.clone();
            tokio::spawn(async move {
                wait_for_both_ready().await;
                handle_patch_board(
                    State(app(gcx)),
                    Path("task-concurrent".to_string()),
                    Json(create_card_request(5, "card-b")),
                )
                .await
            })
        };

        let results = vec![first.await.unwrap(), second.await.unwrap()];
        let successes = results.iter().filter(|result| result.is_ok()).count();
        let conflicts = results
            .iter()
            .filter(|result| {
                matches!(
                    result,
                    Err((StatusCode::CONFLICT, message)) if message.contains("actual 6")
                )
            })
            .count();

        assert_eq!(successes, 1);
        assert_eq!(conflicts, 1);

        let successful_board = results
            .into_iter()
            .find_map(Result::ok)
            .map(|json| json.0)
            .unwrap();
        assert_eq!(successful_board.rev, 6);
        let has_card_a = successful_board.get_card("card-a").is_some();
        let has_card_b = successful_board.get_card("card-b").is_some();
        assert_ne!(has_card_a, has_card_b);

        let stored = storage::load_board(gcx, "task-concurrent").await.unwrap();
        assert_eq!(stored.rev, 6);
        assert_eq!(stored.cards.len(), 1);
        assert_eq!(stored.get_card("card-a").is_some(), has_card_a);
        assert_eq!(stored.get_card("card-b").is_some(), has_card_b);
    }

    #[tokio::test]
    async fn board_patch_add_comment_persists() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-comment").await;
        let created = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-comment".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap()
        .0;
        assert!(created.get_card("card-a").is_some());

        let board = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-comment".to_string()),
            Json(UpdateBoardRequest {
                rev: 1,
                patch: BoardPatch::AddComment {
                    card_id: "card-a".to_string(),
                    body: "Looks good from the tiny chaos desk.".to_string(),
                    author_role: "planner".to_string(),
                    author_id: Some("planner-chat".to_string()),
                    reply_to: None,
                },
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(board.rev, 2);
        let stored = storage::load_board(gcx, "task-comment").await.unwrap();
        let comment = &stored.get_card("card-a").unwrap().comments[0];
        assert_eq!(comment.author_role, "planner");
        assert_eq!(comment.author_id.as_deref(), Some("planner-chat"));
        assert_eq!(comment.body, "Looks good from the tiny chaos desk.");
        assert!(comment.reply_to.is_none());
        assert_eq!(comment.id.len(), 36);
        assert!(Uuid::parse_str(&comment.id).is_ok());
    }

    #[tokio::test]
    async fn add_comment_rejects_empty_body() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-empty-body").await;
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-empty-body".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap();

        let result = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-empty-body".to_string()),
            Json(UpdateBoardRequest {
                rev: 1,
                patch: BoardPatch::AddComment {
                    card_id: "card-a".to_string(),
                    body: String::new(),
                    author_role: "user".to_string(),
                    author_id: None,
                    reply_to: None,
                },
            }),
        )
        .await;

        assert_eq!(status(result), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_comment_rejects_whitespace_only_body() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-ws-body").await;
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-ws-body".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap();

        let result = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-ws-body".to_string()),
            Json(UpdateBoardRequest {
                rev: 1,
                patch: BoardPatch::AddComment {
                    card_id: "card-a".to_string(),
                    body: "   \t\n  ".to_string(),
                    author_role: "user".to_string(),
                    author_id: None,
                    reply_to: None,
                },
            }),
        )
        .await;

        assert_eq!(status(result), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_comment_validates_author_role_enum() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-role-enum").await;
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-role-enum".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap();

        let result = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-role-enum".to_string()),
            Json(UpdateBoardRequest {
                rev: 1,
                patch: BoardPatch::AddComment {
                    card_id: "card-a".to_string(),
                    body: "Hello.".to_string(),
                    author_role: "invalid_role".to_string(),
                    author_id: None,
                    reply_to: None,
                },
            }),
        )
        .await;

        assert_eq!(status(result), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_comment_rejects_unknown_reply_to() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-reply-to").await;
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-reply-to".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap();

        let result = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-reply-to".to_string()),
            Json(UpdateBoardRequest {
                rev: 1,
                patch: BoardPatch::AddComment {
                    card_id: "card-a".to_string(),
                    body: "Reply to nothing.".to_string(),
                    author_role: "user".to_string(),
                    author_id: None,
                    reply_to: Some("nonexistent-id".to_string()),
                },
            }),
        )
        .await;

        assert_eq!(status(result), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_comment_returns_full_uuid_id() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-uuid-id").await;
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-uuid-id".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap();
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-uuid-id".to_string()),
            Json(UpdateBoardRequest {
                rev: 1,
                patch: BoardPatch::AddComment {
                    card_id: "card-a".to_string(),
                    body: "Full UUID test.".to_string(),
                    author_role: "user".to_string(),
                    author_id: None,
                    reply_to: None,
                },
            }),
        )
        .await
        .unwrap();

        let board = storage::load_board(gcx, "task-uuid-id").await.unwrap();
        let comment = &board.get_card("card-a").unwrap().comments[0];

        assert_eq!(comment.id.len(), 36);
        assert!(Uuid::parse_str(&comment.id).is_ok());
    }

    #[tokio::test]
    async fn delete_card_returns_404_for_missing_card() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-del-missing").await;

        let result = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-del-missing".to_string()),
            Json(UpdateBoardRequest {
                rev: 0,
                patch: BoardPatch::DeleteCard {
                    card_id: "nonexistent".to_string(),
                    force: None,
                },
            }),
        )
        .await;

        assert_eq!(status(result), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_card_refuses_active_agent_card_without_force() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-del-active").await;
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-del-active".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap();
        storage::update_board_atomic(gcx.clone(), "task-del-active", |board| {
            if let Some(card) = board.get_card_mut("card-a") {
                card.column = "doing".to_string();
                card.agent_chat_id = Some("agent-chat".to_string());
            }
            Ok(())
        })
        .await
        .unwrap();
        let board = storage::load_board(gcx.clone(), "task-del-active")
            .await
            .unwrap();

        let result = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-del-active".to_string()),
            Json(UpdateBoardRequest {
                rev: board.rev,
                patch: BoardPatch::DeleteCard {
                    card_id: "card-a".to_string(),
                    force: None,
                },
            }),
        )
        .await;

        assert_eq!(status(result), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_card_with_force_removes_active_agent_card() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-del-force").await;
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-del-force".to_string()),
            Json(create_card_request(0, "card-a")),
        )
        .await
        .unwrap();
        storage::update_board_atomic(gcx.clone(), "task-del-force", |board| {
            if let Some(card) = board.get_card_mut("card-a") {
                card.column = "doing".to_string();
                card.agent_chat_id = Some("agent-chat".to_string());
            }
            Ok(())
        })
        .await
        .unwrap();
        let board = storage::load_board(gcx.clone(), "task-del-force")
            .await
            .unwrap();

        let result = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-del-force".to_string()),
            Json(UpdateBoardRequest {
                rev: board.rev,
                patch: BoardPatch::DeleteCard {
                    card_id: "card-a".to_string(),
                    force: Some(true),
                },
            }),
        )
        .await;

        assert!(result.is_ok());
        let board = storage::load_board(gcx, "task-del-force").await.unwrap();
        assert!(board.get_card("card-a").is_none());
    }

    #[tokio::test]
    async fn delete_task_refuses_active_worktree_without_force() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-del-wt").await;
        let _ = handle_patch_board(
            State(app(gcx.clone())),
            Path("task-del-wt".to_string()),
            Json(create_card_request(0, "card-wt")),
        )
        .await
        .unwrap();
        storage::update_board_atomic(gcx.clone(), "task-del-wt", |board| {
            if let Some(card) = board.get_card_mut("card-wt") {
                card.agent_worktree = Some("/tmp/worktree".to_string());
            }
            Ok(())
        })
        .await
        .unwrap();

        let result = handle_delete_task(
            State(app(gcx.clone())),
            Path("task-del-wt".to_string()),
            Query(DeleteTaskQuery { force: false }),
        )
        .await;

        let (status_code, msg) = result.unwrap_err();
        assert_eq!(status_code, StatusCode::CONFLICT);
        assert!(msg.contains("card-wt"));
    }

    #[tokio::test]
    async fn delete_task_returns_404_for_missing_task() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-del-404").await;

        let result = handle_delete_task(
            State(app(gcx)),
            Path("task-does-not-exist".to_string()),
            Query(DeleteTaskQuery { force: true }),
        )
        .await;

        assert_eq!(status(result), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn tasks_subscribe_emits_heartbeat_not_snapshot() {
        use futures::StreamExt;
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-hb").await;

        let response = handle_tasks_subscribe(State(app(gcx.clone())))
            .await
            .unwrap();
        let mut body_stream = response.into_body();

        let chunk1 = body_stream.next().await.unwrap().unwrap();
        let s1 = String::from_utf8(chunk1.to_vec()).unwrap();
        let event1: serde_json::Value =
            serde_json::from_str(s1.strip_prefix("data: ").unwrap().trim_end()).unwrap();
        assert_eq!(event1["type"], "snapshot");

        let ts = Utc::now().to_rfc3339();
        let seq = gcx
            .task_events_seq
            .as_ref()
            .unwrap()
            .fetch_add(1, Ordering::SeqCst);
        gcx.task_events_tx
            .as_ref()
            .unwrap()
            .send(TaskEventEnvelope {
                seq,
                event: TaskEvent::Heartbeat { ts },
            })
            .ok();

        let chunk2 = body_stream.next().await.unwrap().unwrap();
        let s2 = String::from_utf8(chunk2.to_vec()).unwrap();
        let event2: serde_json::Value =
            serde_json::from_str(s2.strip_prefix("data: ").unwrap().trim_end()).unwrap();
        assert_eq!(event2["type"], "heartbeat");
        assert!(event2.get("tasks").is_none());
    }

    #[tokio::test]
    async fn tasks_subscribe_does_not_emit_duplicate_initial_snapshot() {
        use futures::StreamExt;
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-hb-dedup").await;

        let response = handle_tasks_subscribe(State(app(gcx.clone())))
            .await
            .unwrap();
        let mut body_stream = response.into_body();

        let chunk1 = body_stream.next().await.unwrap().unwrap();
        let s1 = String::from_utf8(chunk1.to_vec()).unwrap();
        let event1: serde_json::Value =
            serde_json::from_str(s1.strip_prefix("data: ").unwrap().trim_end()).unwrap();
        assert_eq!(event1["type"], "snapshot");

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(100), body_stream.next()).await;
        assert!(
            result.is_err(),
            "No duplicate event should appear within 100ms of initial snapshot"
        );
    }

    #[tokio::test]
    async fn board_patch_add_comment_unknown_card_errors() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-comment-missing").await;

        let result = handle_patch_board(
            State(app(gcx)),
            Path("task-comment-missing".to_string()),
            Json(UpdateBoardRequest {
                rev: 0,
                patch: BoardPatch::AddComment {
                    card_id: "missing".to_string(),
                    body: "Nobody home.".to_string(),
                    author_role: "planner".to_string(),
                    author_id: None,
                    reply_to: None,
                },
            }),
        )
        .await;

        assert_eq!(status(result), StatusCode::NOT_FOUND);
    }

    mod handle_delete_planner_chat {
        use super::*;

        #[tokio::test]
        async fn delete_planner_chat_rejected_when_live_agent_references_it() {
            let temp = tempfile::tempdir().unwrap();
            let gcx = setup_task(temp.path(), "task-1").await;
            save_planner(gcx.clone(), "task-1", "planner-chat").await;
            insert_live_agent_session(
                gcx.clone(),
                "task-1",
                "agent-chat",
                "agent-1",
                Some("card-a"),
                "planner-chat",
            )
            .await;
            let planner_path = temp
                .path()
                .join(".refact/tasks/task-1/trajectories/planner/planner-chat.json");

            let result = handle_delete_planner_chat(
                State(app(gcx)),
                Path(("task-1".to_string(), "planner-chat".to_string())),
                no_force(),
            )
            .await;
            let (status, body) = delete_error_body(result);

            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(
                body["error"],
                "planner chat has active or persisted agent references"
            );
            assert_eq!(body["hint"], "Pass ?force=true to delete anyway.");
            assert_eq!(body["agent_refs"][0]["chat_id"], "agent-chat");
            assert_eq!(body["agent_refs"][0]["card_id"], "card-a");
            assert_eq!(body["agent_refs"][0]["source"], "live");
            assert!(planner_path.exists());
        }

        #[tokio::test]
        async fn delete_planner_chat_rejected_when_persisted_agent_references_it() {
            let temp = tempfile::tempdir().unwrap();
            let gcx = setup_task(temp.path(), "task-1").await;
            save_planner(gcx.clone(), "task-1", "planner-chat").await;
            save_agent(
                gcx.clone(),
                "task-1",
                "agent-chat",
                "agent-1",
                Some("card-a"),
                "planner-chat",
            )
            .await;
            let planner_path = temp
                .path()
                .join(".refact/tasks/task-1/trajectories/planner/planner-chat.json");

            let result = handle_delete_planner_chat(
                State(app(gcx)),
                Path(("task-1".to_string(), "planner-chat".to_string())),
                no_force(),
            )
            .await;
            let (status, body) = delete_error_body(result);

            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body["agent_refs"].as_array().unwrap().len(), 1);
            assert_eq!(body["agent_refs"][0]["chat_id"], "agent-chat");
            assert_eq!(body["agent_refs"][0]["card_id"], "card-a");
            assert_eq!(body["agent_refs"][0]["source"], "persisted");
            assert!(planner_path.exists());
        }

        #[tokio::test]
        async fn delete_planner_chat_force_true_proceeds_with_warning_and_status_update() {
            let temp = tempfile::tempdir().unwrap();
            let gcx = setup_task(temp.path(), "task-1").await;
            save_planner(gcx.clone(), "task-1", "planner-chat").await;
            let _ = handle_patch_board(
                State(app(gcx.clone())),
                Path("task-1".to_string()),
                Json(create_card_request(0, "card-a")),
            )
            .await
            .unwrap();
            save_agent(
                gcx.clone(),
                "task-1",
                "agent-chat",
                "agent-1",
                Some("card-a"),
                "planner-chat",
            )
            .await;
            let planner_path = temp
                .path()
                .join(".refact/tasks/task-1/trajectories/planner/planner-chat.json");

            let result = handle_delete_planner_chat(
                State(app(gcx.clone())),
                Path(("task-1".to_string(), "planner-chat".to_string())),
                force(),
            )
            .await;

            assert!(result.is_ok());
            assert!(!planner_path.exists());
            let board = storage::load_board(gcx, "task-1").await.unwrap();
            let card = board.get_card("card-a").unwrap();
            let status_update = card.status_updates.last().unwrap();
            assert_eq!(
            status_update.message,
            "Planner chat planner-chat deleted while this agent still referenced it; planner_chat_id link is now dangling."
        );
        }

        #[tokio::test]
        async fn delete_planner_chat_with_no_refs_succeeds_without_force() {
            let temp = tempfile::tempdir().unwrap();
            let gcx = setup_task(temp.path(), "task-1").await;
            save_planner(gcx.clone(), "task-1", "planner-chat").await;
            let planner_path = temp
                .path()
                .join(".refact/tasks/task-1/trajectories/planner/planner-chat.json");
            assert!(planner_path.exists());

            let result = handle_delete_planner_chat(
                State(app(gcx)),
                Path(("task-1".to_string(), "planner-chat".to_string())),
                no_force(),
            )
            .await;

            assert!(result.is_ok());
            assert!(!planner_path.exists());
        }

        #[tokio::test]
        async fn delete_planner_chat_not_blocked_by_own_live_session() {
            let temp = tempfile::tempdir().unwrap();
            let gcx = setup_task(temp.path(), "task-1").await;
            save_planner(gcx.clone(), "task-1", "planner-chat").await;
            insert_live_planner_session(gcx.clone(), "task-1", "planner-chat").await;
            let planner_path = temp
                .path()
                .join(".refact/tasks/task-1/trajectories/planner/planner-chat.json");
            assert!(planner_path.exists());

            let result = handle_delete_planner_chat(
                State(app(gcx)),
                Path(("task-1".to_string(), "planner-chat".to_string())),
                no_force(),
            )
            .await;

            assert!(result.is_ok());
            assert!(!planner_path.exists());
        }

        #[tokio::test]
        async fn delete_planner_chat_not_blocked_by_sibling_planner_chat() {
            let temp = tempfile::tempdir().unwrap();
            let gcx = setup_task(temp.path(), "task-1").await;
            save_planner(gcx.clone(), "task-1", "planner-chat").await;
            insert_live_planner_session(gcx.clone(), "task-1", "planner-chat").await;
            insert_live_planner_session(gcx.clone(), "task-1", "chat-2").await;
            let planner_path = temp
                .path()
                .join(".refact/tasks/task-1/trajectories/planner/planner-chat.json");
            assert!(planner_path.exists());

            let result = handle_delete_planner_chat(
                State(app(gcx)),
                Path(("task-1".to_string(), "planner-chat".to_string())),
                no_force(),
            )
            .await;

            assert!(result.is_ok());
            assert!(!planner_path.exists());
        }
    }

    #[tokio::test]
    async fn delete_planner_chat_rejects_other_task() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-1").await;
        storage::create_task(gcx.clone(), "other task")
            .await
            .unwrap();
        let task_2_path = temp.path().join(".refact/tasks/task-2");
        let created_task_path = std::fs::read_dir(temp.path().join(".refact/tasks"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name().and_then(|name| name.to_str()) != Some("task-1")
                    && path.join("meta.yaml").exists()
            })
            .unwrap();
        std::fs::rename(&created_task_path, &task_2_path).unwrap();
        let meta_path = task_2_path.join("meta.yaml");
        let mut meta: TaskMeta =
            serde_yaml::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
        meta.id = "task-2".to_string();
        std::fs::write(&meta_path, serde_yaml::to_string(&meta).unwrap()).unwrap();
        save_snapshot(
            gcx.clone(),
            snapshot("shared-chat", "task-2", "planner", None, None, None),
        )
        .await;

        let result = handle_delete_planner_chat(
            State(app(gcx)),
            Path(("task-1".to_string(), "shared-chat".to_string())),
            no_force(),
        )
        .await;

        assert_eq!(delete_status(result), StatusCode::FORBIDDEN);
        assert!(temp
            .path()
            .join(".refact/tasks/task-2/trajectories/planner/shared-chat.json")
            .exists());
    }

    #[tokio::test]
    async fn delete_planner_chat_rejects_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-1").await;

        let result = handle_delete_planner_chat(
            State(app(gcx)),
            Path(("task-1".to_string(), "../../etc/passwd".to_string())),
            no_force(),
        )
        .await;

        assert_eq!(delete_status(result), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_planner_chat_rejects_invalid_chat_id() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-1").await;

        let result = handle_delete_planner_chat(
            State(app(gcx)),
            Path(("task-1".to_string(), "bad.chat".to_string())),
            no_force(),
        )
        .await;

        assert_eq!(delete_status(result), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn handle_create_task_document_creates_document_and_returns_detail() {
        use crate::tools::tool_task_documents::CreateDocumentRequest;
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-doc-create").await;
        let result = handle_create_task_document(
            State(app(gcx.clone())),
            Path("task-doc-create".to_string()),
            Json(CreateDocumentRequest {
                slug: "my-plan".to_string(),
                name: "My Plan".to_string(),
                kind: "plan".to_string(),
                content: "body text".to_string(),
                pinned: Some(true),
                relevant_cards: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.0.slug, "my-plan");
        assert_eq!(result.0.name, "My Plan");
        assert_eq!(result.0.content, "body text");
        assert!(result.0.pinned);
        assert_eq!(result.0.version, 1);
    }

    #[tokio::test]
    async fn update_task_meta_event_includes_live_planner_session_state() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-meta-state").await;
        save_planner(gcx.clone(), "task-meta-state", "planner-paused").await;
        insert_live_planner_session_with_state(
            gcx.clone(),
            "task-meta-state",
            "planner-paused",
            crate::chat::types::SessionState::Paused,
        )
        .await;
        let mut rx = gcx.task_events_tx.as_ref().unwrap().subscribe();

        let Json(response) = handle_update_task_meta(
            State(app(gcx.clone())),
            Path("task-meta-state".to_string()),
            Json(UpdateTaskMetaRequest {
                name: Some("Renamed task".to_string()),
                base_branch: None,
                base_commit: None,
                default_agent_model: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.planner_session_state.as_deref(), Some("paused"));
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event.event {
            TaskEvent::TaskUpdated { task_id, meta } => {
                assert_eq!(task_id, "task-meta-state");
                assert_eq!(meta.name, "Renamed task");
                assert_eq!(meta.planner_session_state.as_deref(), Some("paused"));
            }
            other => panic!("unexpected task event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn handle_list_task_documents_returns_empty_when_no_documents() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-docs-empty").await;
        let result = handle_list_task_documents(
            State(app(gcx.clone())),
            Path("task-docs-empty".to_string()),
        )
        .await
        .unwrap();
        assert!(result.0.documents.is_empty());
        assert_eq!(result.0.task_id, "task-docs-empty");
    }

    #[tokio::test]
    async fn handle_update_task_document_returns_404_for_unknown_slug() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-doc-404").await;
        let result = handle_update_task_document(
            State(app(gcx.clone())),
            Path(("task-doc-404".to_string(), "no-such-slug".to_string())),
            Json(UpdateTaskDocumentRequest {
                content: "whatever".to_string(),
                pinned: None,
            }),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::NOT_FOUND, _))));
    }

    #[tokio::test]
    async fn handle_pin_task_document_returns_detail() {
        use crate::tools::tool_task_documents::CreateDocumentRequest;
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-doc-pin").await;
        let _ = handle_create_task_document(
            State(app(gcx.clone())),
            Path("task-doc-pin".to_string()),
            Json(CreateDocumentRequest {
                slug: "pin-plan".to_string(),
                name: "Pin Plan".to_string(),
                kind: "plan".to_string(),
                content: "body text".to_string(),
                pinned: Some(false),
                relevant_cards: None,
            }),
        )
        .await
        .unwrap();

        let result = handle_pin_task_document(
            State(app(gcx.clone())),
            Path(("task-doc-pin".to_string(), "pin-plan".to_string())),
            Json(PinTaskDocumentRequest { pinned: true }),
        )
        .await
        .unwrap();

        assert_eq!(result.0.slug, "pin-plan");
        assert_eq!(result.0.content, "body text");
        assert!(result.0.pinned);
        assert_eq!(result.0.version, 2);
    }

    #[tokio::test]
    async fn handle_update_task_document_accepts_pinned_in_same_request() {
        use crate::tools::tool_task_documents::CreateDocumentRequest;
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-doc-update-pin").await;
        let _ = handle_create_task_document(
            State(app(gcx.clone())),
            Path("task-doc-update-pin".to_string()),
            Json(CreateDocumentRequest {
                slug: "combined-plan".to_string(),
                name: "Combined Plan".to_string(),
                kind: "plan".to_string(),
                content: "v1".to_string(),
                pinned: Some(true),
                relevant_cards: None,
            }),
        )
        .await
        .unwrap();

        let result = handle_update_task_document(
            State(app(gcx.clone())),
            Path((
                "task-doc-update-pin".to_string(),
                "combined-plan".to_string(),
            )),
            Json(UpdateTaskDocumentRequest {
                content: "v2".to_string(),
                pinned: Some(false),
            }),
        )
        .await
        .unwrap();

        assert_eq!(result.0.content, "v2");
        assert!(!result.0.pinned);
        assert_eq!(result.0.version, 2);
        let history = handle_history_task_document(
            State(app(gcx.clone())),
            Path((
                "task-doc-update-pin".to_string(),
                "combined-plan".to_string(),
            )),
        )
        .await
        .unwrap();
        assert_eq!(history.0.history.len(), 1);
        assert_eq!(history.0.history[0].version, 1);
    }

    #[tokio::test]
    async fn handle_history_task_document_returns_404_for_unknown_slug() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_task(temp.path(), "task-doc-history-404").await;
        let result = handle_history_task_document(
            State(app(gcx.clone())),
            Path((
                "task-doc-history-404".to_string(),
                "no-such-slug".to_string(),
            )),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::NOT_FOUND, _))));
    }
}
