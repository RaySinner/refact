use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use git2::Repository;
use tokio::fs;
use tracing::warn;
use uuid::Uuid;
use chrono::Utc;

use crate::global_context::GlobalContext;
use crate::global_context::GlobalContextCtx;
use crate::files_correction::get_project_dirs;
use super::types::{TaskBoard, TaskMeta, TaskStatus, TrajectoryInfo};
use super::events::{
    emit_task_event_for_services, emit_task_event_via, emit_task_updated_via, TaskEvent,
};

pub use refact_runtime_api::task_storage::{
    active_agent_count, create_task, find_task_dir_for_ctx, infer_task_id_from_chat_id, load_board,
    load_board_for_ctx, load_task_meta, load_task_meta_for_ctx, next_planner_chat_id, save_board,
    save_board_for_ctx, save_task_meta, save_task_meta_for_ctx, update_board_atomic,
    update_board_atomic_for_ctx, update_task_stats, update_task_stats_for_ctx,
};
pub(crate) use refact_runtime_api::task_storage::get_all_tasks_dirs_for_ctx;
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
        save_task_meta_impl(ctx, task_id, meta).await
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
        emit_task_updated_via(
            &ctx.chat.sessions,
            &Some(ctx.chat.task_events_tx.clone()),
            &Some(ctx.chat.task_events_seq.clone()),
            task_id.to_string(),
            meta.clone(),
        )
        .await;
    }
}

const TASKS_DIR: &str = "tasks";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardConflict {
    pub expected: u64,
    pub actual: u64,
}

impl BoardConflict {
    pub fn message(&self) -> String {
        format!(
            "Board rev mismatch: expected {}, actual {}",
            self.expected, self.actual
        )
    }
}

pub async fn get_tasks_dir_for_ctx(ctx: &crate::chat::ctx::ChatCtx) -> Result<PathBuf, String> {
    let project_dirs = crate::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    let workspace_root = project_dirs.first().ok_or("No workspace folder found")?;
    Ok(workspace_root.join(".refact").join(TASKS_DIR))
}

pub async fn get_global_tasks_dir(gcx: Arc<GlobalContext>) -> PathBuf {
    let config_dir = gcx.config_dir.clone();
    config_dir.join(TASKS_DIR)
}

pub fn get_global_tasks_dir_for_ctx(ctx: &crate::chat::ctx::ChatCtx) -> PathBuf {
    ctx.paths.config_dir.join(TASKS_DIR)
}

pub async fn get_all_tasks_dirs(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = get_project_dirs(gcx.clone())
        .await
        .into_iter()
        .map(|p| p.join(".refact").join(TASKS_DIR))
        .filter(|p| p.exists())
        .collect();

    let global_dir = get_global_tasks_dir(gcx).await;
    if global_dir.exists() {
        dirs.push(global_dir);
    }

    dirs
}

pub async fn find_task_dir(gcx: Arc<GlobalContext>, task_id: &str) -> Result<PathBuf, String> {
    find_task_dir_for_ctx(&gcx.chat_ctx(gcx.clone()), task_id).await
}

pub async fn ensure_tasks_dir_for_ctx(ctx: &crate::chat::ctx::ChatCtx) -> Result<PathBuf, String> {
    let dir = get_tasks_dir_for_ctx(ctx).await?;
    if !dir.exists() {
        fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

pub async fn list_tasks(gcx: Arc<GlobalContext>) -> Result<Vec<TaskMeta>, String> {
    list_tasks_for_ctx(&gcx.chat_ctx(gcx.clone())).await
}

pub async fn list_tasks_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
) -> Result<Vec<TaskMeta>, String> {
    let tasks_dirs = get_all_tasks_dirs_for_ctx(ctx).await;

    let mut tasks = vec![];
    let mut seen_ids = HashSet::new();

    for tasks_dir in tasks_dirs {
        if !tasks_dir.exists() {
            continue;
        }

        match super::index::list_tasks_from_index_or_rebuild(&tasks_dir).await {
            Ok(metas) => {
                for meta in metas {
                    if seen_ids.insert(meta.id.clone()) {
                        tasks.push(meta);
                    }
                }
            }
            Err(e) => warn!("Failed to list tasks from {:?}: {}", tasks_dir, e),
        }
    }

    tasks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(tasks)
}

async fn save_task_meta_impl(
    ctx: &crate::chat::ctx::ChatCtx,
    task_id: &str,
    meta: &TaskMeta,
) -> Result<(), String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let meta_path = task_dir.join("meta.yaml");
    let content = serde_yaml::to_string(meta).map_err(|e| e.to_string())?;
    fs::write(&meta_path, content)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(tasks_dir) = task_dir.parent() {
        super::index::upsert_task_index_entry(tasks_dir, meta).await?;
    }
    Ok(())
}

pub async fn load_planner_instructions(
    gcx: Arc<GlobalContext>,
    task_id: &str,
) -> Result<String, String> {
    load_planner_instructions_for_ctx(&gcx.chat_ctx(gcx.clone()), task_id).await
}

pub async fn load_planner_instructions_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
    task_id: &str,
) -> Result<String, String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let path = task_dir.join("planner_instructions.md");
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&path).await.map_err(|e| e.to_string())
}

pub async fn save_planner_instructions(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    content: &str,
) -> Result<(), String> {
    save_planner_instructions_for_ctx(&gcx.chat_ctx(gcx.clone()), task_id, content).await
}

pub async fn save_planner_instructions_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
    task_id: &str,
    content: &str,
) -> Result<(), String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let path = task_dir.join("planner_instructions.md");
    fs::write(&path, content).await.map_err(|e| e.to_string())
}

fn detect_git_branch_and_commit(workspace_root: &Path) -> (Option<String>, Option<String>) {
    let repo = match Repository::open(workspace_root) {
        Ok(r) => r,
        Err(_) => return (None, None),
    };

    let branch = match repo.head() {
        Ok(head) => head.shorthand().map(|s| s.to_string()),
        Err(_) => None,
    };

    let commit = match repo.head() {
        Ok(head) => match head.peel_to_commit() {
            Ok(c) => Some(c.id().to_string()),
            Err(_) => None,
        },
        Err(_) => None,
    };

    (branch, commit)
}

pub async fn create_task_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
    name: &str,
) -> Result<TaskMeta, String> {
    let tasks_dir = ensure_tasks_dir_for_ctx(ctx).await?;
    let task_id = Uuid::new_v4().to_string();
    let task_dir = tasks_dir.join(&task_id);

    fs::create_dir_all(&task_dir)
        .await
        .map_err(|e| e.to_string())?;
    fs::create_dir_all(task_dir.join("trajectories").join("planner"))
        .await
        .map_err(|e| e.to_string())?;
    fs::create_dir_all(task_dir.join("trajectories").join("agents"))
        .await
        .map_err(|e| e.to_string())?;

    let project_dirs = crate::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    let (base_branch, base_commit) = project_dirs
        .first()
        .map(|p| detect_git_branch_and_commit(p))
        .unwrap_or((None, None));

    let now = Utc::now().to_rfc3339();
    let has_user_provided_name = !name.trim().is_empty() && name.to_lowercase() != "new task";
    let meta = TaskMeta {
        schema_version: 1,
        id: task_id.clone(),
        name: if has_user_provided_name {
            name.to_string()
        } else {
            "New Task".to_string()
        },
        status: TaskStatus::Planning,
        created_at: now.clone(),
        updated_at: now,
        cards_total: 0,
        cards_done: 0,
        cards_failed: 0,
        agents_active: 0,
        base_branch,
        base_commit,
        default_agent_model: None,
        is_name_generated: has_user_provided_name,
        last_agents_summary_at: None,
        planner_session_state: None,
    };

    save_task_meta_for_ctx(ctx, &task_id, &meta).await?;
    save_board_for_ctx(ctx, &task_id, &TaskBoard::default()).await?;
    save_planner_instructions_for_ctx(ctx, &task_id, "").await?;

    emit_task_event_for_services(
        &ctx.chat,
        TaskEvent::TaskCreated {
            task_id: task_id.clone(),
            meta: meta.clone(),
        },
    )
    .await;
    {
        let ev = crate::buddy::actor::make_runtime_event(
            "task_created",
            &format!("Task created: {}", meta.name),
            "task",
            &format!("task_{}", task_id),
            "completed",
            None,
        );
        ctx.buddy_event_sink.enqueue_event(ev).await;
    }

    Ok(meta)
}

pub async fn delete_task(gcx: Arc<GlobalContext>, task_id: &str) -> Result<(), String> {
    delete_task_for_ctx(&gcx.chat_ctx(gcx.clone()), task_id).await
}

pub async fn delete_task_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
    task_id: &str,
) -> Result<(), String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let tasks_dir = task_dir.parent().map(Path::to_path_buf);
    fs::remove_dir_all(&task_dir)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(tasks_dir) = tasks_dir {
        if let Err(e) = super::index::remove_task_index_entry(&tasks_dir, task_id).await {
            warn!(
                "Failed to remove task {} from index {:?}: {}",
                task_id, tasks_dir, e
            );
        }
    }
    emit_task_event_for_services(
        &ctx.chat,
        TaskEvent::TaskDeleted {
            task_id: task_id.to_string(),
        },
    )
    .await;
    Ok(())
}

pub async fn update_task_name(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    name: &str,
) -> Result<TaskMeta, String> {
    update_task_name_for_ctx(&gcx.chat_ctx(gcx.clone()), task_id, name).await
}

pub async fn update_task_name_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
    task_id: &str,
    name: &str,
) -> Result<TaskMeta, String> {
    let mut meta = load_task_meta_for_ctx(ctx, task_id).await?;
    meta.name = name.to_string();
    meta.is_name_generated = true;
    meta.updated_at = Utc::now().to_rfc3339();
    save_task_meta_for_ctx(ctx, task_id, &meta).await?;
    emit_task_updated_via(
        &ctx.chat.sessions,
        &Some(ctx.chat.task_events_tx.clone()),
        &Some(ctx.chat.task_events_seq.clone()),
        task_id.to_string(),
        meta.clone(),
    )
    .await;
    Ok(meta)
}

pub fn get_task_trajectory_dir(task_dir: &PathBuf, role: &str, agent_id: Option<&str>) -> PathBuf {
    let base = task_dir.join("trajectories").join(role);
    match agent_id {
        Some(id) => base.join(id),
        None => base,
    }
}

fn should_list_task_trajectory(role: &str, data: &serde_json::Value) -> bool {
    if role != "planner" {
        return true;
    }
    match data.get("link_type").and_then(|v| v.as_str()) {
        Some("handoff" | "mode_transition" | "branch") | None => true,
        Some(_) => false,
    }
}

/// Compute the next planner chat id in the standard `planner-{task_id}-{n}` format.
///
/// NOTE: this is not strictly atomic — concurrent callers can pick the same
/// number. Callers that care about uniqueness must serialize themselves.
pub async fn next_planner_chat_id_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
    task_id: &str,
) -> Result<String, String> {
    let existing = list_task_trajectories_for_ctx(
        ctx,
        task_id,
        "planner",
        None,
        crate::chat::trajectory_index::TrajectoryIndexListingCaller::TaskPlannerIdAllocation,
    )
    .await?;
    let prefix = format!("planner-{}-", task_id);
    let max_num = existing
        .iter()
        .filter_map(|t| {
            t.id.strip_prefix(&prefix)
                .and_then(|s| s.parse::<u32>().ok())
        })
        .max()
        .unwrap_or(0);
    Ok(format!("planner-{}-{}", task_id, max_num + 1))
}

pub async fn list_task_trajectories(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    role: &str,
    agent_id: Option<&str>,
) -> Result<Vec<TrajectoryInfo>, String> {
    list_task_trajectories_for(
        gcx,
        task_id,
        role,
        agent_id,
        crate::chat::trajectory_index::TrajectoryIndexListingCaller::TaskTrajectoryApi,
    )
    .await
}

pub async fn list_task_trajectories_for(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    role: &str,
    agent_id: Option<&str>,
    caller: crate::chat::trajectory_index::TrajectoryIndexListingCaller,
) -> Result<Vec<TrajectoryInfo>, String> {
    list_task_trajectories_for_ctx(
        &gcx.chat_ctx(gcx.clone()),
        task_id,
        role,
        agent_id,
        caller,
    )
    .await
}

pub async fn list_task_trajectories_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
    task_id: &str,
    role: &str,
    agent_id: Option<&str>,
    caller: crate::chat::trajectory_index::TrajectoryIndexListingCaller,
) -> Result<Vec<TrajectoryInfo>, String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let traj_dir = get_task_trajectory_dir(&task_dir, role, agent_id);

    if !traj_dir.exists() {
        return Ok(vec![]);
    }

    let coordinator = ctx.chat.trajectory_index_coordinator.clone();
    let entries = crate::chat::trajectory_index::list_trajectory_entries_with_rollout_for(
        &coordinator,
        &traj_dir,
        None,
        caller,
    )
    .await?;
    let mut trajectories: Vec<TrajectoryInfo> = entries
        .into_iter()
        .filter(|entry| {
            should_list_task_trajectory(
                role,
                &serde_json::json!({ "link_type": entry.link_type.as_deref() }),
            )
        })
        .map(|entry| TrajectoryInfo {
            id: entry.id,
            title: entry.title,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
            session_state: None,
            mode: Some(entry.mode),
            parent_id: entry.parent_id,
            waiting_for_card_ids: entry.waiting_for_card_ids,
        })
        .collect();

    trajectories.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let sessions = ctx.chat.sessions.read().await;
    for traj in &mut trajectories {
        if let Some(session_arc) = sessions.get(&traj.id) {
            let session = session_arc.lock().await;
            traj.session_state = Some(session.runtime.state.to_string());
            if !session.waiting_for_card_ids.is_empty() {
                traj.waiting_for_card_ids = session.waiting_for_card_ids.clone();
            }
        }
    }

    Ok(trajectories)
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
        assert!(!should_list_task_trajectory(
            "planner",
            &json!({"link_type": "gather_files"})
        ));
        assert!(!should_list_task_trajectory(
            "planner",
            &json!({"link_type": "subagent"})
        ));
    }

    #[test]
    fn planner_listing_keeps_real_planner_chats() {
        assert!(should_list_task_trajectory("planner", &json!({})));
        assert!(should_list_task_trajectory(
            "planner",
            &json!({"link_type": "handoff"})
        ));
        assert!(should_list_task_trajectory(
            "planner",
            &json!({"link_type": "mode_transition"})
        ));
    }

    #[test]
    fn non_planner_listing_keeps_child_links() {
        assert!(should_list_task_trajectory(
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

        assert_eq!(active_agent_count(&[ab, singleton]), 2);
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
