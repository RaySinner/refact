pub use refact_tools::tools::tool_spawn_ab::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::process::Command;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::Utc;
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
use crate::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::ChatCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::{event, EventSubkind};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::verifier::ExpectedCardState;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::verifier::schedule_card_verifier_after_finish_for_ctx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
#[allow(unused_imports)]
use refact_tasks::types::{AbVariantInfo, AbVariants, BoardCard, StatusUpdate};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::task_tool_helpers::{wait_for_agent_abort, AGENT_ABORT_TIMEOUT};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_spawn_agent::{
    abandoned_worktrees_error, build_agent_prompt, build_agent_thread_params,
    find_abandoned_worktrees, prepare_agent_worktree_with_suffix, resolve_agent_model,
    resolve_invoking_planner_chat_id, PreparedWorktree,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::service::WorktreeService;
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::types::WorktreeMeta;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::ChatCommand;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::CreateSessionRequest;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_context::GlobalContext;
    use refact_tasks::types::{AbVariantFinish, TaskBoard, TaskMeta, TaskStatus};
    use std::path::Path;

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

    async fn set_workspace(gcx: Arc<GlobalContext>, root: &Path) {
        let root = root.canonicalize().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root];
    }

    fn task_meta(task_id: &str) -> TaskMeta {
        let now = Utc::now().to_rfc3339();
        TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        }
    }

    fn test_card() -> BoardCard {
        BoardCard {
            id: "T-1".to_string(),
            title: "Card T-1".to_string(),
            column: "planned".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: "Do the thing".to_string(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: Utc::now().to_rfc3339(),
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

    async fn test_app(gcx: &Arc<GlobalContext>) -> AppState {
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await
    }

    async fn setup_repo_task() -> (Arc<GlobalContext>, tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let task_dir = source.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&task_meta("task-1")).unwrap(),
        )
        .await
        .unwrap();
        let mut board = TaskBoard::default();
        board.cards.push(test_card());
        tokio::fs::write(
            task_dir.join("board.yaml"),
            serde_yaml::to_string(&board).unwrap(),
        )
        .await
        .unwrap();
        (gcx, temp, source)
    }

    #[test]
    fn spawn_ab_ab_variants_field_defaults_to_none() {
        let card: BoardCard = serde_json::from_str(
            r#"{
                "id":"T-1",
                "title":"legacy",
                "column":"planned",
                "assignee":null,
                "agent_chat_id":null,
                "created_at":"2026-05-16T00:00:00Z",
                "started_at":null,
                "completed_at":null
            }"#,
        )
        .unwrap();

        assert!(card.ab_variants.is_none());
    }

    #[tokio::test]
    async fn spawn_ab_creates_2_worktrees() {
        let (gcx, _temp, _source) = setup_repo_task().await;
        let meta = storage::load_task_meta(gcx.clone(), "task-1")
            .await
            .unwrap();

        let prepared = prepare_ab_worktrees(&test_app(&gcx).await,
            &meta,
            "task-1",
            "T-1",
            "model-a".to_string(),
            "model-b".to_string(),
        )
        .await
        .unwrap();

        assert!(prepared.a.prepared.worktree_path().is_dir());
        assert!(prepared.b.prepared.worktree_path().is_dir());
        assert_ne!(
            prepared.a.prepared.worktree_path(),
            prepared.b.prepared.worktree_path()
        );
        assert!(prepared
            .a
            .prepared
            .branch_name()
            .as_deref()
            .unwrap()
            .ends_with("-a"));
        assert!(prepared
            .b
            .prepared
            .branch_name()
            .as_deref()
            .unwrap()
            .ends_with("-b"));

        prepared.a.prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
        prepared.b.prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
    }

    #[tokio::test]
    async fn spawn_ab_planner_context_prefers_live_root_controller() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut planner_session =
            refact_chat_api::ChatSession::new("controller-planner".to_string());
        planner_session.thread.task_meta = Some(refact_chat_api::TaskMeta {
            task_id: "task-1".to_string(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: Some("controller-planner".to_string()),
        });
        gcx.chat_sessions.write().await.insert(
            "controller-planner".to_string(),
            Arc::new(tokio::sync::Mutex::new(planner_session)),
        );
        let ccx = Arc::new(tokio::sync::Mutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx.clone()).await,
                4096,
                20,
                false,
                vec![],
                "planner-child".to_string(),
                Some("controller-planner".to_string()),
                "model".to_string(),
                Some(refact_chat_api::TaskMeta {
                    task_id: "task-1".to_string(),
                    role: "planner".to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: Some("stale-planner".to_string()),
                }),
                None,
            )
            .await,
        ));

        let (_, task_id, planner_chat_id, _) = planner_context(&ccx, &HashMap::new(), "spawn_ab")
            .await
            .unwrap();

        assert_eq!(task_id, "task-1");
        assert_eq!(planner_chat_id, "controller-planner");
    }

    #[tokio::test]
    async fn pick_ab_winner_cleans_up_loser() {
        let (gcx, _temp, _source) = setup_repo_task().await;
        let meta = storage::load_task_meta(gcx.clone(), "task-1")
            .await
            .unwrap();
        let prepared = prepare_ab_worktrees(&test_app(&gcx).await,
            &meta,
            "task-1",
            "T-1",
            "model-a".to_string(),
            "model-b".to_string(),
        )
        .await
        .unwrap();
        let winner_root = prepared.a.prepared.worktree_path();
        let loser_root = prepared.b.prepared.worktree_path();
        let loser_branch = prepared.b.prepared.branch_name().unwrap();
        let variants = AbVariants {
            a: variant_info(&prepared.a),
            b: variant_info(&prepared.b),
            winner: None,
        };
        let mut variants = variants;
        variants.a.finish = Some(AbVariantFinish {
            success: true,
            final_report: "winner report".to_string(),
            final_report_structured: None,
            completed_at: Utc::now().to_rfc3339(),
            commit_hash: Some("abc123".to_string()),
        });
        storage::update_board_atomic(gcx.clone(), "task-1", move |board| {
            let card = board.get_card_mut("T-1").unwrap();
            card.column = "doing".to_string();
            card.assignee = Some("ab".to_string());
            card.ab_variants = Some(variants.clone());
            Ok(())
        })
        .await
        .unwrap();

        let (winner_chat_id, loser_key) = pick_ab_winner_impl(&test_app(&gcx).await, "task-1", "T-1", "a")
            .await
            .unwrap();
        let board = storage::load_board(gcx.clone(), "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();

        assert_eq!(loser_key, "b");
        assert_eq!(winner_chat_id, prepared.a.chat_id);
        assert_eq!(
            card.agent_worktree.as_deref(),
            Some(winner_root.to_str().unwrap())
        );
        assert_eq!(card.column, "done");
        assert_eq!(card.final_report.as_deref(), Some("winner report"));
        assert_eq!(
            card.ab_variants
                .as_ref()
                .and_then(|variants| variants.winner.as_deref()),
            Some("a")
        );
        assert!(winner_root.is_dir());
        assert!(!loser_root.exists());
        let branches = run_git(
            &prepared.a.prepared.source_workspace_root(),
            &["branch", "--list", &loser_branch],
        );
        assert!(branches.trim().is_empty());

        let err = pick_ab_winner_impl(&test_app(&gcx).await, "task-1", "T-1", "a")
            .await
            .unwrap_err();
        assert!(err.contains("already picked A/B winner"), "{err}");

        cleanup_ab_variant(&test_app(&gcx).await, &variant_info(&prepared.a))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pick_ab_winner_stale_state_noops_before_loser_cleanup() {
        let (gcx, _temp, _source) = setup_repo_task().await;
        let meta = storage::load_task_meta(gcx.clone(), "task-1")
            .await
            .unwrap();
        let prepared = prepare_ab_worktrees(&test_app(&gcx).await,
            &meta,
            "task-1",
            "T-1",
            "model-a".to_string(),
            "model-b".to_string(),
        )
        .await
        .unwrap();
        let loser_root = prepared.b.prepared.worktree_path();
        let variants = AbVariants {
            a: variant_info(&prepared.a),
            b: variant_info(&prepared.b),
            winner: None,
        };
        let mut variants = variants;
        variants.a.finish = Some(AbVariantFinish {
            success: true,
            final_report: "winner report".to_string(),
            final_report_structured: None,
            completed_at: Utc::now().to_rfc3339(),
            commit_hash: Some("abc123".to_string()),
        });
        storage::update_board_atomic(gcx.clone(), "task-1", {
            let variants = variants.clone();
            move |board| {
                let card = board.get_card_mut("T-1").unwrap();
                card.column = "doing".to_string();
                card.assignee = Some("ab".to_string());
                card.ab_variants = Some(variants.clone());
                Ok(())
            }
        })
        .await
        .unwrap();
        let board = storage::load_board(gcx.clone(), "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        let expected_state = ExpectedCardState::from_card(board.rev, card);
        let winner_variant = variants.a.clone();
        let winner_finish = variants.a.finish.clone().unwrap();
        storage::update_board_atomic(gcx.clone(), "task-1", |board| {
            board.get_card_mut("T-1").unwrap().title = "newer title".to_string();
            Ok(())
        })
        .await
        .unwrap();

        let promoted = promote_ab_winner_if_current(&test_app(&gcx).await.chat_ctx(),
            "task-1",
            "T-1",
            "a",
            expected_state,
            winner_variant,
            winner_finish,
        )
        .await
        .unwrap();

        assert!(promoted.is_none());
        let board = storage::load_board(gcx.clone(), "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.title, "newer title");
        assert_eq!(card.column, "doing");
        assert_eq!(
            card.ab_variants
                .as_ref()
                .and_then(|variants| variants.winner.as_deref()),
            None
        );
        assert!(loser_root.exists());

        prepared.a.prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
        prepared.b.prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
    }

    #[tokio::test]
    async fn spawn_ab_loser_cleanup_aborts_before_delete() {
        use refact_chat_api::ChatSession;
        use refact_runtime_api::SessionState;
        use std::sync::atomic::Ordering;
        use tokio::sync::Mutex as AMutex;

        let (gcx, _temp, _source) = setup_repo_task().await;
        let meta = storage::load_task_meta(gcx.clone(), "task-1")
            .await
            .unwrap();
        let prepared = prepare_ab_worktrees(&test_app(&gcx).await,
            &meta,
            "task-1",
            "T-1",
            "model-a".to_string(),
            "model-b".to_string(),
        )
        .await
        .unwrap();
        let loser = variant_info(&prepared.b);
        let loser_root = prepared.b.prepared.worktree_path();
        let session_arc = Arc::new(AMutex::new(ChatSession::new(loser.chat_id.clone())));
        {
            let mut session = session_arc.lock().await;
            session.runtime.state = SessionState::Generating;
            session
                .queue_processor_running
                .store(true, Ordering::SeqCst);
        }
        gcx.chat_sessions
            .write()
            .await
            .insert(loser.chat_id.clone(), session_arc.clone());

        let error = cleanup_ab_variant_with_timeout(&test_app(&gcx).await,
            &loser,
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap_err();

        assert!(error.contains("Failed to abort A/B variant"), "{error}");
        assert!(loser_root.exists());
        {
            let session = session_arc.lock().await;
            assert!(session.abort_flag.load(Ordering::SeqCst));
            assert_eq!(session.runtime.state, SessionState::Idle);
            session
                .queue_processor_running
                .store(false, Ordering::SeqCst);
        }
        cleanup_ab_variant(&test_app(&gcx).await, &loser).await.unwrap();
        assert!(!loser_root.exists());

        prepared.a.prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
    }

    #[tokio::test]
    async fn pick_ab_winner_requires_finished_variant() {
        let (gcx, _temp, _source) = setup_repo_task().await;
        let meta = storage::load_task_meta(gcx.clone(), "task-1")
            .await
            .unwrap();
        let prepared = prepare_ab_worktrees(&test_app(&gcx).await,
            &meta,
            "task-1",
            "T-1",
            "model-a".to_string(),
            "model-b".to_string(),
        )
        .await
        .unwrap();
        let variants = AbVariants {
            a: variant_info(&prepared.a),
            b: variant_info(&prepared.b),
            winner: None,
        };
        storage::update_board_atomic(gcx.clone(), "task-1", move |board| {
            let card = board.get_card_mut("T-1").unwrap();
            card.column = "doing".to_string();
            card.assignee = Some("ab".to_string());
            card.ab_variants = Some(variants.clone());
            Ok(())
        })
        .await
        .unwrap();

        let error = pick_ab_winner_impl(&test_app(&gcx).await, "task-1", "T-1", "a")
            .await
            .unwrap_err();

        assert!(error.contains("has not finished yet"), "{error}");
        prepared.a.prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
        prepared.b.prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
    }

    #[tokio::test]
    async fn pick_ab_winner_with_invalid_winner_returns_error() {
        let (gcx, _temp, _source) = setup_repo_task().await;
        let err = pick_ab_winner_impl(&test_app(&gcx).await, "task-1", "T-1", "c")
            .await
            .unwrap_err();

        assert!(err.contains("Invalid winner"));
    }
}
