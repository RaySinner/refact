use std::sync::Arc;

use crate::global_context::GlobalContext;
use refact_runtime_api::GlobalContextCtx;
use super::types::{TaskBoard, TaskMeta};
use super::events::{emit_task_event_via, TaskEvent};

pub use refact_runtime_api::task_storage::{
    active_agent_count, create_task, create_task_for_ctx, delete_task, delete_task_for_ctx,
    find_task_dir, find_task_dir_for_ctx, get_all_tasks_dirs, get_all_tasks_dirs_for_ctx,
    get_global_tasks_dir, get_global_tasks_dir_for_ctx, get_task_trajectory_dir,
    infer_task_id_from_chat_id, list_task_trajectories, list_task_trajectories_for,
    list_task_trajectories_for_ctx, list_tasks, list_tasks_for_ctx, load_board, load_board_for_ctx,
    load_planner_instructions, load_planner_instructions_for_ctx, load_task_meta,
    load_task_meta_for_ctx, next_planner_chat_id, next_planner_chat_id_for_ctx, save_board,
    save_board_for_ctx, save_planner_instructions, save_planner_instructions_for_ctx,
    save_task_meta, save_task_meta_for_ctx, should_list_task_trajectory, update_board_atomic,
    update_board_atomic_for_ctx, update_task_name, update_task_name_for_ctx, update_task_stats,
    update_task_stats_for_ctx,
};
pub use refact_runtime_api::task_storage::BoardConflict;
pub use refact_tasks::storage::validate_task_id;

pub struct EngineTaskStorageRuntime;

#[async_trait::async_trait]
impl refact_runtime_api::task_storage::TaskStorageRuntime for EngineTaskStorageRuntime {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn ctx_from_gcx(&self, gcx: Arc<GlobalContext>) -> crate::chat::ctx::ChatCtx {
        gcx.chat_ctx(gcx.clone())
    }

    async fn save_task_meta_for_ctx(
        &self,
        ctx: &crate::chat::ctx::ChatCtx,
        task_id: &str,
        meta: &TaskMeta,
    ) -> Result<(), String> {
        refact_runtime_api::task_storage::save_task_meta_file_for_ctx(ctx, task_id, meta).await
    }

    async fn create_task(
        &self,
        ctx: &crate::chat::ctx::ChatCtx,
        name: &str,
    ) -> Result<TaskMeta, String> {
        create_task_for_ctx(ctx, name).await
    }

    async fn next_planner_chat_id(
        &self,
        ctx: &crate::chat::ctx::ChatCtx,
        task_id: &str,
    ) -> Result<String, String> {
        next_planner_chat_id_for_ctx(ctx, task_id).await
    }

    async fn emit_board_changed(
        &self,
        ctx: &crate::chat::ctx::ChatCtx,
        task_id: &str,
        board: &TaskBoard,
    ) {
        emit_task_event_via(
            &Some(ctx.chat.task_events_tx.clone()),
            &Some(ctx.chat.task_events_seq.clone()),
            TaskEvent::BoardChanged {
                task_id: task_id.to_string(),
                rev: board.rev,
                board: board.clone(),
            },
        )
        .await;
    }

    async fn emit_task_updated(
        &self,
        ctx: &crate::chat::ctx::ChatCtx,
        task_id: &str,
        meta: &TaskMeta,
    ) {
        refact_runtime_api::task_storage::emit_task_updated_via(
            &ctx.chat.sessions,
            &Some(ctx.chat.task_events_tx.clone()),
            &Some(ctx.chat.task_events_seq.clone()),
            task_id.to_string(),
            meta.clone(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_buddy_core::user_action::UserAction;
    use crate::tasks::types::{
        AbVariantFinish, AbVariantInfo, AbVariants, BoardCard, StatusUpdate, TaskBoard, TaskMeta,
        TaskStatus,
    };
    use chrono::Utc;
    use serde_json::json;

    #[tokio::test]
    async fn task_failed_pushed_on_status_transition() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let temp = tempfile::tempdir().unwrap();
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() =
                vec![temp.path().to_path_buf()];
        }
        let task_id = "task-failed-ring";
        let task_dir = temp.path().join(".refact/tasks").join(task_id);
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        let now = Utc::now().to_rfc3339();
        let meta = TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now.clone(),
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
        };
        save_task_meta(gcx.clone(), task_id, &meta).await.unwrap();
        save_board(
            gcx.clone(),
            task_id,
            &TaskBoard {
                cards: vec![BoardCard {
                    id: "card-1".to_string(),
                    title: "Card".to_string(),
                    column: "doing".to_string(),
                    priority: "P1".to_string(),
                    depends_on: Vec::new(),
                    instructions: String::new(),
                    assignee: None,
                    agent_chat_id: None,
                    retry_count: 0,
                    status_updates: vec![StatusUpdate {
                        timestamp: now.clone(),
                        message: "failure reason with details".to_string(),
                    }],
                    comments: vec![],
                    final_report: None,
                    final_report_structured: None,
                    verifier_report: None,
                    created_at: now.clone(),
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
                    target_files: Vec::new(),
                    scope_guard_mode: Default::default(),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        update_board_atomic(gcx.clone(), task_id, |board| {
            board.cards[0].column = "failed".to_string();
            Ok(())
        })
        .await
        .unwrap();

        let user_activity = gcx.user_activity.clone();
        let ring = user_activity.lock().await;
        assert!(ring.snapshot().iter().any(|action| matches!(
            action,
            UserAction::TaskFailed { task_id, reason_short, .. }
                if task_id == "task-failed-ring" && reason_short == "failure reason with details"
        )));
    }

    #[test]
    fn planner_listing_hides_subagentic_child_links() {
        assert!(!refact_runtime_api::task_storage::should_list_task_trajectory(
            "planner",
            &json!({"link_type": "gather_files"})
        ));
        assert!(!refact_runtime_api::task_storage::should_list_task_trajectory(
            "planner",
            &json!({"link_type": "subagent"})
        ));
    }

    #[test]
    fn planner_listing_keeps_real_planner_chats() {
        assert!(refact_runtime_api::task_storage::should_list_task_trajectory("planner", &json!({})));
        assert!(refact_runtime_api::task_storage::should_list_task_trajectory(
            "planner",
            &json!({"link_type": "handoff"})
        ));
        assert!(refact_runtime_api::task_storage::should_list_task_trajectory(
            "planner",
            &json!({"link_type": "mode_transition"})
        ));
    }

    #[test]
    fn non_planner_listing_keeps_child_links() {
        assert!(refact_runtime_api::task_storage::should_list_task_trajectory(
            "subchats",
            &json!({"link_type": "gather_files"})
        ));
    }

    fn test_meta(task_id: &str, name: &str, updated_at: &str) -> TaskMeta {
        TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: name.to_string(),
            status: TaskStatus::Planning,
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
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
        }
    }

    async fn setup_workspace(root: &std::path::Path) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        gcx
    }

    #[tokio::test]
    async fn list_tasks_rebuilds_missing_index_and_uses_it_afterward() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_workspace(temp.path()).await;
        let tasks_dir = temp.path().join(".refact/tasks");
        let task_dir = tasks_dir.join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        let meta = test_meta("task-1", "Task", "2026-01-01T00:00:00Z");
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&meta).unwrap(),
        )
        .await
        .unwrap();

        let listed = list_tasks(gcx).await.unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "task-1");
        assert!(crate::tasks::index::task_index_path(&tasks_dir).exists());
    }

    #[tokio::test]
    async fn create_update_delete_task_maintains_index() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_workspace(temp.path()).await;
        let meta = create_task(gcx.clone(), "Indexed Task").await.unwrap();
        let tasks_dir = temp.path().join(".refact/tasks");
        let index = crate::tasks::index::read_task_index(&tasks_dir)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].id, meta.id);

        let updated = update_task_name(gcx.clone(), &meta.id, "Renamed")
            .await
            .unwrap();
        let index = crate::tasks::index::read_task_index(&tasks_dir)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].meta.name, updated.name);

        delete_task(gcx.clone(), &meta.id).await.unwrap();
        let index = crate::tasks::index::read_task_index(&tasks_dir)
            .await
            .unwrap()
            .unwrap();
        assert!(index.entries.is_empty());
    }

    #[test]
    fn active_agent_count_counts_unfinished_ab_variants() {
        fn card(id: &str) -> BoardCard {
            BoardCard {
                id: id.to_string(),
                title: id.to_string(),
                column: "doing".to_string(),
                priority: "P1".to_string(),
                depends_on: vec![],
                instructions: String::new(),
                assignee: Some("ab".to_string()),
                agent_chat_id: None,
                retry_count: 0,
                status_updates: vec![],
                comments: vec![],
                final_report: None,
                final_report_structured: None,
                verifier_report: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
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
        fn variant(key: &str, finished: bool) -> AbVariantInfo {
            AbVariantInfo {
                agent_id: format!("agent-{key}"),
                chat_id: format!("chat-{key}"),
                worktree: format!("/tmp/{key}"),
                worktree_name: None,
                branch: None,
                model: None,
                finish: finished.then(|| AbVariantFinish {
                    success: true,
                    final_report: "done".to_string(),
                    final_report_structured: None,
                    completed_at: "2026-01-01T00:00:01Z".to_string(),
                    commit_hash: None,
                }),
            }
        }

        let mut ab = card("T-1");
        ab.ab_variants = Some(AbVariants {
            a: variant("a", true),
            b: variant("b", false),
            winner: None,
        });
        let mut singleton = card("T-2");
        singleton.assignee = Some("agent".to_string());
        singleton.agent_chat_id = Some("chat-single".to_string());

        assert_eq!(
            refact_runtime_api::task_storage::active_agent_count(&[ab, singleton]),
            2
        );
    }

    #[tokio::test]
    async fn list_tasks_repairs_corrupt_index() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = setup_workspace(temp.path()).await;
        let tasks_dir = temp.path().join(".refact/tasks");
        let task_dir = tasks_dir.join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        let meta = test_meta("task-1", "Task", "2026-01-01T00:00:00Z");
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&meta).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(crate::tasks::index::task_index_path(&tasks_dir), "not-json")
            .await
            .unwrap();

        let listed = list_tasks(gcx).await.unwrap();

        assert_eq!(listed.len(), 1);
        assert!(crate::tasks::index::read_task_index(&tasks_dir)
            .await
            .unwrap()
            .is_some());
    }
}
