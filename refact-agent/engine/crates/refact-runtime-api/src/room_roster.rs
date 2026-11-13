use crate::global_context::SharedGlobalContext;
use crate::task_storage::load_board;

use refact_chat_api::room_roster::roster_for_card;

/// The roster for the card the calling agent works on, or `None` outside a room.
///
/// This is the only part that touches the board: everything else in
/// `refact_chat_api::room_roster` is pure.
pub async fn load_room_roster(
    gcx: SharedGlobalContext,
    task_id: &str,
    card_id: &str,
    self_chat_id: Option<&str>,
) -> Option<String> {
    if task_id.is_empty() || card_id.is_empty() {
        return None;
    }
    let board = load_board(gcx, task_id).await.ok()?;
    let card = board.get_card(card_id)?;
    roster_for_card(card, self_chat_id)
}
