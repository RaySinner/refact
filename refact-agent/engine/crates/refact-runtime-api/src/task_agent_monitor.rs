//! Task-board helpers shared by the chat agent monitor and its tools.

use crate::task_storage::update_board_atomic_for_ctx;
use crate::ChatCtx;

pub async fn append_card_target_files(
    ctx: ChatCtx,
    task_id: &str,
    card_id: &str,
    files: Vec<String>,
) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    let card_id_owned = card_id.to_string();
    update_board_atomic_for_ctx(&ctx, task_id, move |board| {
        let card = board
            .get_card_mut(&card_id_owned)
            .ok_or_else(|| format!("Card {} not found", card_id_owned))?;
        for file in &files {
            if !card.target_files.contains(file) {
                card.target_files.push(file.clone());
            }
        }
        Ok(())
    })
    .await
    .map(|_| ())
}
