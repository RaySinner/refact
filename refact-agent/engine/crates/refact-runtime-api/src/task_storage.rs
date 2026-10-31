use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use refact_buddy_core::user_action::UserAction;
use refact_tasks::types::{TaskBoard, TaskMeta};
use tokio::sync::Mutex as AMutex;

use crate::global_context::GlobalContext;
use crate::ChatCtx;

pub use refact_tasks::storage::{active_agent_count, infer_task_id_from_chat_id};

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
    async fn emit_board_changed(&self, ctx: &ChatCtx, task_id: &str, board: &TaskBoard);
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

pub async fn load_task_meta(gcx: Arc<GlobalContext>, task_id: &str) -> Result<TaskMeta, String> {
    load_task_meta_for_ctx(&gcx.task_storage_runtime.ctx_from_gcx(gcx.clone()), task_id).await
}

pub async fn save_task_meta(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    meta: &TaskMeta,
) -> Result<(), String> {
    save_task_meta_for_ctx(
        &gcx.task_storage_runtime.ctx_from_gcx(gcx.clone()),
        task_id,
        meta,
    )
    .await
}

pub async fn load_board(gcx: Arc<GlobalContext>, task_id: &str) -> Result<TaskBoard, String> {
    load_board_for_ctx(&gcx.task_storage_runtime.ctx_from_gcx(gcx.clone()), task_id).await
}

pub async fn save_board(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    board: &TaskBoard,
) -> Result<(), String> {
    save_board_for_ctx(
        &gcx.task_storage_runtime.ctx_from_gcx(gcx.clone()),
        task_id,
        board,
    )
    .await
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
    update_board_atomic_for_ctx(
        &gcx.task_storage_runtime.ctx_from_gcx(gcx.clone()),
        task_id,
        updater,
    )
    .await
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
        .emit_board_changed(ctx, task_id, &board)
        .await;
    Ok((board, result))
}

pub async fn create_task(gcx: Arc<GlobalContext>, name: &str) -> Result<TaskMeta, String> {
    gcx.task_storage_runtime
        .create_task(&gcx.task_storage_runtime.ctx_from_gcx(gcx.clone()), name)
        .await
}

pub async fn update_task_stats(gcx: Arc<GlobalContext>, task_id: &str) -> Result<TaskMeta, String> {
    update_task_stats_for_ctx(&gcx.task_storage_runtime.ctx_from_gcx(gcx.clone()), task_id).await
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
    gcx.task_storage_runtime
        .next_planner_chat_id(&gcx.task_storage_runtime.ctx_from_gcx(gcx.clone()), task_id)
        .await
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
