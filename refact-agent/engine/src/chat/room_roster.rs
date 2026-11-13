use std::sync::Arc;

use crate::global_context::GlobalContext;

pub use refact_chat_api::room_roster::*;

/// The roster for the card the calling agent works on, or `None` outside a room.
///
/// This is the only part that touches the board: everything else in this module is pure.
pub async fn load_room_roster(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    card_id: &str,
    self_chat_id: Option<&str>,
) -> Option<String> {
    if task_id.is_empty() || card_id.is_empty() {
        return None;
    }
    let board = crate::tasks::storage::load_board(gcx, task_id)
        .await
        .ok()?;
    let card = board.get_card(card_id)?;
    roster_for_card(card, self_chat_id)
}
