use crate::tasks::storage;
use crate::tasks::types::{CardComment, TaskBoard};

pub(crate) use refact_tasks::comments::{add_comment_to_board, CreateCardComment};
pub use refact_tasks::comments_cap::{COMMENT_BODY_MAX_CHARS, COMMENT_CAP};

pub(crate) async fn create_card_comment_for_ctx(
    ctx: &crate::chat::ctx::ChatCtx,
    task_id: &str,
    request: CreateCardComment,
) -> Result<(TaskBoard, CardComment), String> {
    let card_id = request.card_id.clone();
    let (board, comment) = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        Ok(Some(add_comment_to_board(board, request)?))
    })
    .await?;
    let comment = comment.ok_or_else(|| "comment was not created".to_string())?;
    crate::tasks::events::emit_task_event_via(
        &Some(ctx.chat.task_events_tx.clone()),
        &Some(ctx.chat.task_events_seq.clone()),
        crate::tasks::events::TaskEvent::TaskCommentsChanged {
            task_id: task_id.to_string(),
            card_id,
        },
    )
    .await;
    Ok((board, comment))
}
