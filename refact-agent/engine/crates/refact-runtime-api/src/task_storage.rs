use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use refact_buddy_core::user_action::UserAction;
use refact_chat_api::types::SessionsMap;
use refact_chat_history::trajectory_index::{
    list_trajectory_entries_with_rollout_for, TrajectoryIndexListingCaller,
};
use refact_tasks::events::{TaskEvent, TaskEventEnvelope};
use refact_tasks::events_emit::{emit_task_event_for_services, emit_task_event_via};
use refact_tasks::types::{TaskBoard, TaskMeta, TrajectoryInfo};
use tokio::sync::Mutex as AMutex;

use crate::global_context::GlobalContext;
use crate::ChatCtx;

pub use refact_tasks::storage::{
    active_agent_count, get_task_trajectory_dir, infer_task_id_from_chat_id, BoardConflict,
};

pub async fn get_all_tasks_dirs_for_ctx(ctx: &ChatCtx) -> Vec<std::path::PathBuf> {
    refact_tasks::storage::get_all_tasks_dirs_for_ctx(ctx).await
}

pub async fn find_task_dir_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
) -> Result<std::path::PathBuf, String> {
    refact_tasks::storage::find_task_dir_for_ctx(ctx, task_id).await
}

pub async fn load_board_for_ctx(ctx: &ChatCtx, task_id: &str) -> Result<TaskBoard, String> {
    refact_tasks::storage::load_board_for_ctx(ctx, task_id).await
}

pub async fn load_task_meta_for_ctx(ctx: &ChatCtx, task_id: &str) -> Result<TaskMeta, String> {
    refact_tasks::storage::load_task_meta_for_ctx(ctx, task_id).await
}

pub async fn save_board_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
    board: &TaskBoard,
) -> Result<(), String> {
    refact_tasks::storage::save_board_for_ctx(ctx, task_id, board).await
}

pub fn get_global_tasks_dir_for_ctx(ctx: &ChatCtx) -> std::path::PathBuf {
    refact_tasks::storage::get_global_tasks_dir_for_ctx(ctx)
}

pub async fn list_tasks_for_ctx(ctx: &ChatCtx) -> Result<Vec<TaskMeta>, String> {
    refact_tasks::storage::list_tasks_for_ctx(ctx).await
}

pub async fn load_planner_instructions_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
) -> Result<String, String> {
    refact_tasks::storage::load_planner_instructions_for_ctx(ctx, task_id).await
}

pub async fn save_planner_instructions_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
    content: &str,
) -> Result<(), String> {
    refact_tasks::storage::save_planner_instructions_for_ctx(ctx, task_id, content).await
}

impl refact_tasks::storage::TaskPaths for ChatCtx {
    fn task_project_dirs(&self) -> Vec<std::path::PathBuf> {
        let workspace_folders = self.workspace.workspace_folders.lock().unwrap().clone();
        refact_worktrees::path_mappings::project_dirs_for_unscoped_paths(
            self.paths.cache_dir.as_path(),
            workspace_folders.as_slice(),
        )
    }

    fn task_config_dir(&self) -> std::path::PathBuf {
        self.paths.config_dir.clone()
    }
}

#[async_trait]
pub trait TaskStorageRuntime: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
    fn ctx_from_gcx(&self, gcx: Arc<GlobalContext>) -> ChatCtx;
    async fn save_task_meta_for_ctx(
        &self,
        ctx: &ChatCtx,
        task_id: &str,
        meta: &TaskMeta,
    ) -> Result<(), String>;
    async fn create_task(&self, ctx: &ChatCtx, name: &str) -> Result<TaskMeta, String>;
    async fn next_planner_chat_id(&self, ctx: &ChatCtx, task_id: &str) -> Result<String, String>;
    async fn emit_board_changed(&self, ctx: &ChatCtx, task_id: &str, rev: u64);
    async fn emit_task_updated(&self, ctx: &ChatCtx, task_id: &str, meta: &TaskMeta);
}

static BOARD_LOCKS: std::sync::OnceLock<AMutex<HashMap<String, Arc<AMutex<()>>>>> =
    std::sync::OnceLock::new();

fn get_board_locks() -> &'static AMutex<HashMap<String, Arc<AMutex<()>>>> {
    BOARD_LOCKS.get_or_init(|| AMutex::new(HashMap::new()))
}

async fn get_board_lock(task_id: &str) -> Arc<AMutex<()>> {
    let mut locks = get_board_locks().lock().await;
    locks
        .entry(task_id.to_string())
        .or_insert_with(|| Arc::new(AMutex::new(())))
        .clone()
}

fn ctx_for(gcx: &Arc<GlobalContext>) -> ChatCtx {
    gcx.task_storage_runtime.ctx_from_gcx(gcx.clone())
}

pub async fn load_task_meta(gcx: Arc<GlobalContext>, task_id: &str) -> Result<TaskMeta, String> {
    load_task_meta_for_ctx(&ctx_for(&gcx), task_id).await
}

pub async fn save_task_meta(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    meta: &TaskMeta,
) -> Result<(), String> {
    save_task_meta_for_ctx(&ctx_for(&gcx), task_id, meta).await
}

pub async fn load_board(gcx: Arc<GlobalContext>, task_id: &str) -> Result<TaskBoard, String> {
    load_board_for_ctx(&ctx_for(&gcx), task_id).await
}

pub async fn save_board(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    board: &TaskBoard,
) -> Result<(), String> {
    save_board_for_ctx(&ctx_for(&gcx), task_id, board).await
}

pub async fn get_global_tasks_dir(gcx: Arc<GlobalContext>) -> std::path::PathBuf {
    get_global_tasks_dir_for_ctx(&ctx_for(&gcx))
}

pub async fn get_all_tasks_dirs(gcx: Arc<GlobalContext>) -> Vec<std::path::PathBuf> {
    get_all_tasks_dirs_for_ctx(&ctx_for(&gcx)).await
}

pub async fn find_task_dir(
    gcx: Arc<GlobalContext>,
    task_id: &str,
) -> Result<std::path::PathBuf, String> {
    find_task_dir_for_ctx(&ctx_for(&gcx), task_id).await
}

pub async fn list_tasks(gcx: Arc<GlobalContext>) -> Result<Vec<TaskMeta>, String> {
    list_tasks_for_ctx(&ctx_for(&gcx)).await
}

pub async fn load_planner_instructions(
    gcx: Arc<GlobalContext>,
    task_id: &str,
) -> Result<String, String> {
    load_planner_instructions_for_ctx(&ctx_for(&gcx), task_id).await
}

pub async fn save_planner_instructions(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    content: &str,
) -> Result<(), String> {
    save_planner_instructions_for_ctx(&ctx_for(&gcx), task_id, content).await
}

pub async fn update_board_atomic<F, T>(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    updater: F,
) -> Result<(TaskBoard, T), String>
where
    F: FnOnce(&mut TaskBoard) -> Result<T, String>,
    T: Default,
{
    update_board_atomic_for_ctx(&ctx_for(&gcx), task_id, updater).await
}

pub async fn update_board_atomic_for_ctx<F, T>(
    ctx: &ChatCtx,
    task_id: &str,
    updater: F,
) -> Result<(TaskBoard, T), String>
where
    F: FnOnce(&mut TaskBoard) -> Result<T, String>,
    T: Default,
{
    let lock = get_board_lock(task_id).await;
    let _guard = lock.lock().await;

    let mut board = load_board_for_ctx(ctx, task_id).await?;
    let failed_before: HashSet<String> = board
        .cards
        .iter()
        .filter(|card| card.column == "failed" || card.column == "regressed")
        .map(|card| card.id.clone())
        .collect();
    let result = updater(&mut board)?;
    let new_failed = board
        .cards
        .iter()
        .filter(|card| {
            (card.column == "failed" || card.column == "regressed")
                && !failed_before.contains(&card.id)
        })
        .map(|card| {
            let reason_short = card
                .final_report
                .as_deref()
                .filter(|report| !report.trim().is_empty())
                .or_else(|| {
                    card.status_updates
                        .last()
                        .map(|update| update.message.as_str())
                })
                .unwrap_or(&card.title)
                .chars()
                .take(80)
                .collect::<String>();
            UserAction::TaskFailed {
                task_id: task_id.to_string(),
                reason_short,
                ts: Utc::now(),
            }
        })
        .collect::<Vec<_>>();
    board.rev += 1;
    save_board_for_ctx(ctx, task_id, &board).await?;
    if !new_failed.is_empty() {
        let user_activity = ctx.buddy.user_activity.clone();
        if let Ok(mut ring) = user_activity.try_lock() {
            for action in new_failed {
                ring.push(action);
            }
        };
    }
    ctx.task_storage_runtime
        .emit_board_changed(ctx, task_id, board.rev)
        .await;
    Ok((board, result))
}

pub async fn create_task(gcx: Arc<GlobalContext>, name: &str) -> Result<TaskMeta, String> {
    let ctx = ctx_for(&gcx);
    ctx.task_storage_runtime.create_task(&ctx, name).await
}

pub async fn create_task_for_ctx(ctx: &ChatCtx, name: &str) -> Result<TaskMeta, String> {
    let meta = refact_tasks::storage::create_task_files_for_ctx(ctx, name).await?;
    emit_task_event_for_services(
        &ctx.chat,
        TaskEvent::TaskCreated {
            task_id: meta.id.clone(),
            meta: meta.clone(),
        },
    )
    .await;
    let ev = refact_buddy_core::actor::make_runtime_event(
        "task_created",
        &format!("Task created: {}", meta.name),
        "task",
        &format!("task_{}", meta.id),
        "completed",
        None,
    );
    ctx.buddy_event_sink.enqueue_event(ev).await;
    Ok(meta)
}

pub async fn delete_task(gcx: Arc<GlobalContext>, task_id: &str) -> Result<(), String> {
    delete_task_for_ctx(&ctx_for(&gcx), task_id).await
}

pub async fn delete_task_for_ctx(ctx: &ChatCtx, task_id: &str) -> Result<(), String> {
    refact_tasks::storage::delete_task_files_for_ctx(ctx, task_id).await?;
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
    update_task_name_for_ctx(&ctx_for(&gcx), task_id, name).await
}

pub async fn update_task_name_for_ctx(
    ctx: &ChatCtx,
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

pub async fn update_task_stats(gcx: Arc<GlobalContext>, task_id: &str) -> Result<TaskMeta, String> {
    update_task_stats_for_ctx(&ctx_for(&gcx), task_id).await
}

pub async fn update_task_stats_for_ctx(ctx: &ChatCtx, task_id: &str) -> Result<TaskMeta, String> {
    let mut meta = load_task_meta_for_ctx(ctx, task_id).await?;
    let board = load_board_for_ctx(ctx, task_id).await?;

    meta.cards_total = board.cards.len();
    meta.cards_done = board.cards.iter().filter(|c| c.column == "done").count();
    meta.cards_failed = board
        .cards
        .iter()
        .filter(|c| c.column == "failed" || c.column == "regressed")
        .count();
    meta.agents_active = active_agent_count(&board.cards);
    meta.updated_at = Utc::now().to_rfc3339();

    save_task_meta_for_ctx(ctx, task_id, &meta).await?;
    ctx.task_storage_runtime
        .emit_task_updated(ctx, task_id, &meta)
        .await;
    Ok(meta)
}

pub async fn next_planner_chat_id(
    gcx: Arc<GlobalContext>,
    task_id: &str,
) -> Result<String, String> {
    let ctx = ctx_for(&gcx);
    ctx.task_storage_runtime.next_planner_chat_id(&ctx, task_id).await
}

/// Compute the next planner chat id in the standard `planner-{task_id}-{n}` format.
///
/// NOTE: this is not strictly atomic — concurrent callers can pick the same
/// number. Callers that care about uniqueness must serialize themselves.
pub async fn next_planner_chat_id_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
) -> Result<String, String> {
    let existing = list_task_trajectories_for_ctx(
        ctx,
        task_id,
        "planner",
        None,
        TrajectoryIndexListingCaller::TaskPlannerIdAllocation,
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

pub async fn save_task_meta_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
    meta: &TaskMeta,
) -> Result<(), String> {
    ctx.task_storage_runtime
        .save_task_meta_for_ctx(ctx, task_id, meta)
        .await
}

pub async fn save_task_meta_file_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
    meta: &TaskMeta,
) -> Result<(), String> {
    refact_tasks::storage::save_task_meta_file_for_ctx(ctx, task_id, meta).await
}

pub fn should_list_task_trajectory(role: &str, data: &serde_json::Value) -> bool {
    if role != "planner" {
        return true;
    }
    match data.get("link_type").and_then(|v| v.as_str()) {
        Some("handoff" | "mode_transition" | "branch") | None => true,
        Some(_) => false,
    }
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
        TrajectoryIndexListingCaller::TaskTrajectoryApi,
    )
    .await
}

pub async fn list_task_trajectories_for(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    role: &str,
    agent_id: Option<&str>,
    caller: TrajectoryIndexListingCaller,
) -> Result<Vec<TrajectoryInfo>, String> {
    list_task_trajectories_for_ctx(&ctx_for(&gcx), task_id, role, agent_id, caller).await
}

pub async fn list_task_trajectories_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
    role: &str,
    agent_id: Option<&str>,
    caller: TrajectoryIndexListingCaller,
) -> Result<Vec<TrajectoryInfo>, String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let traj_dir = get_task_trajectory_dir(&task_dir, role, agent_id);

    if !traj_dir.exists() {
        return Ok(vec![]);
    }

    let coordinator = ctx.chat.trajectory_index_coordinator.clone();
    let entries =
        list_trajectory_entries_with_rollout_for(&coordinator, &traj_dir, None, caller).await?;
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

pub async fn enrich_task_meta_with_session_state(sessions: &SessionsMap, meta: &mut TaskMeta) {
    let all_session_arcs = {
        let sessions = sessions.read().await;
        sessions.values().cloned().collect::<Vec<_>>()
    };

    let mut has_paused = false;
    let mut has_waiting_ide = false;
    let mut has_waiting_user_input = false;
    let mut has_generating = false;
    let mut has_executing_tools = false;
    let mut has_error = false;
    for session_arc in all_session_arcs {
        let session = session_arc.lock().await;
        let is_planner = session.thread.task_meta.as_ref().is_some_and(|task_meta| {
            task_meta.role == "planner" && task_meta.task_id == meta.id
        });
        if !is_planner {
            continue;
        }
        match session.runtime.state {
            refact_chat_api::SessionState::Starting => {}
            refact_chat_api::SessionState::Paused => has_paused = true,
            refact_chat_api::SessionState::WaitingIde => has_waiting_ide = true,
            refact_chat_api::SessionState::WaitingUserInput => has_waiting_user_input = true,
            refact_chat_api::SessionState::Generating => has_generating = true,
            refact_chat_api::SessionState::ExecutingTools => has_executing_tools = true,
            refact_chat_api::SessionState::Error => has_error = true,
            refact_chat_api::SessionState::Idle | refact_chat_api::SessionState::Completed => {}
        }
    }

    meta.planner_session_state = if has_paused {
        Some(refact_chat_api::SessionState::Paused.to_string())
    } else if has_waiting_ide {
        Some(refact_chat_api::SessionState::WaitingIde.to_string())
    } else if has_waiting_user_input {
        Some(refact_chat_api::SessionState::WaitingUserInput.to_string())
    } else if has_generating {
        Some(refact_chat_api::SessionState::Generating.to_string())
    } else if has_executing_tools {
        Some(refact_chat_api::SessionState::ExecutingTools.to_string())
    } else if has_error {
        Some(refact_chat_api::SessionState::Error.to_string())
    } else {
        None
    };
}

pub async fn enrich_task_with_session_state_for_services(
    chat: &crate::services::ChatServices,
    task: &mut TaskMeta,
) {
    enrich_task_meta_with_session_state(&chat.sessions, task).await;
}

pub async fn emit_task_updated_via(
    sessions: &SessionsMap,
    task_events_tx: &Option<tokio::sync::broadcast::Sender<TaskEventEnvelope>>,
    task_events_seq: &Option<Arc<std::sync::atomic::AtomicU64>>,
    task_id: String,
    mut meta: TaskMeta,
) {
    enrich_task_meta_with_session_state(sessions, &mut meta).await;
    emit_task_event_via(
        task_events_tx,
        task_events_seq,
        TaskEvent::TaskUpdated { task_id, meta },
    )
    .await;
}
