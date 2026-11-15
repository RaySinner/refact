use std::sync::Arc;

use crate::global_context::GlobalContext;
use crate::GlobalContextCtx;
use refact_tasks::types::{TaskBoard, TaskMeta};
use crate::task_events::{emit_task_event_via, TaskEvent};

pub use crate::task_storage::{
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
pub use crate::task_storage::BoardConflict;
pub use refact_tasks::storage::validate_task_id;

pub struct EngineTaskStorageRuntime;

#[async_trait::async_trait]
impl crate::task_storage::TaskStorageRuntime for EngineTaskStorageRuntime {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn ctx_from_gcx(&self, gcx: Arc<GlobalContext>) -> crate::chat_ctx::ChatCtx {
        gcx.chat_ctx(gcx.clone())
    }

    async fn save_task_meta_for_ctx(
        &self,
        ctx: &crate::chat_ctx::ChatCtx,
        task_id: &str,
        meta: &TaskMeta,
    ) -> Result<(), String> {
        crate::task_storage::save_task_meta_file_for_ctx(ctx, task_id, meta).await
    }

    async fn create_task(
        &self,
        ctx: &crate::chat_ctx::ChatCtx,
        name: &str,
    ) -> Result<TaskMeta, String> {
        create_task_for_ctx(ctx, name).await
    }

    async fn next_planner_chat_id(
        &self,
        ctx: &crate::chat_ctx::ChatCtx,
        task_id: &str,
    ) -> Result<String, String> {
        next_planner_chat_id_for_ctx(ctx, task_id).await
    }

    async fn emit_board_changed(
        &self,
        ctx: &crate::chat_ctx::ChatCtx,
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
        ctx: &crate::chat_ctx::ChatCtx,
        task_id: &str,
        meta: &TaskMeta,
    ) {
        crate::task_storage::emit_task_updated_via(
            &ctx.chat.sessions,
            &Some(ctx.chat.task_events_tx.clone()),
            &Some(ctx.chat.task_events_seq.clone()),
            task_id.to_string(),
            meta.clone(),
        )
        .await;
    }
}
