use crate::MessageProvenance;
use refact_tasks::types::{BoardCard, TeamMember};

/// Provenance for `member` of `card` — who this is, in which room, wearing which accent.
///
/// The single place that reads a `TeamMember` to answer "who was that". Callers pass the card they
/// already loaded rather than re-reading the board: a delivery stamps provenance while holding
/// the roster it resolved the peer from, so a second read would only add latency and a window in
/// which the room changed mid-message.
pub fn member_provenance(card: &BoardCard, member: &TeamMember) -> MessageProvenance {
    let same_role_count = card
        .team()
        .iter()
        .filter(|peer| peer.role.trim() == member.role.trim())
        .count();
    let position_in_role = card
        .team()
        .iter()
        .filter(|peer| peer.role.trim() == member.role.trim())
        .position(|peer| {
            peer.agent_chat_id.as_deref() == member.agent_chat_id.as_deref()
                && peer.agent_id.as_deref() == member.agent_id.as_deref()
        })
        .map(|index| index + 1)
        .unwrap_or(1);

    MessageProvenance {
        chat_id: member
            .agent_chat_id
            .clone()
            .or_else(|| member.agent_id.clone())
            .unwrap_or_default(),
        role: member.role.trim().to_string(),
        display_name: refact_tasks::rooms::member_display_name(
            &member.role,
            position_in_role,
            same_role_count,
        ),
        card_id: Some(card.id.clone()),
    }
}

/// Provenance for whoever is speaking from `caller_chat_id` in `card`, if that is a room member.
pub fn room_member_provenance(card: &BoardCard, caller_chat_id: &str) -> Option<MessageProvenance> {
    let provenance = member_provenance(card, card.team_member_by_chat_id(caller_chat_id)?);
    (!provenance.chat_id.is_empty()).then_some(provenance)
}
