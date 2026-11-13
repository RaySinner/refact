use refact_tasks::comments::{add_comment_to_board, CreateCardComment};
use refact_tasks::events::TaskEvent;
use refact_tasks::types::{CardComment, TaskBoard};

use crate::chat_ctx::ChatCtx;

pub async fn create_card_comment_for_ctx(
    ctx: &ChatCtx,
    task_id: &str,
    request: CreateCardComment,
) -> Result<(TaskBoard, CardComment), String> {
    let card_id = request.card_id.clone();
    let (board, comment) = crate::task_storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        Ok(Some(add_comment_to_board(board, request)?))
    })
    .await?;
    let comment = comment.ok_or_else(|| "comment was not created".to_string())?;
    refact_tasks::events_emit::emit_task_event_via(
        &Some(ctx.chat.task_events_tx.clone()),
        &Some(ctx.chat.task_events_seq.clone()),
        TaskEvent::TaskCommentsChanged {
            task_id: task_id.to_string(),
            card_id,
        },
    )
    .await;
    Ok((board, comment))
}
