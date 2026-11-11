use refact_core::chat_types::ChatMessage;

use crate::types::ChatSession;

/// Session-local variant of [`crate::plan_role::try_current_base_plan`]:
/// appends the session's queued control state (`post_tool_side_effects` +
/// `pending_deliveries`) after the active-context projection, so queued
/// accepted controls count as current while archived ones do not. Ties on
/// version resolve to the LAST max (`max_by_key`), i.e. queued plans win.
pub fn try_current_base_plan(session: &ChatSession) -> Result<Option<ChatMessage>, String> {
    // Queued accepted controls are not yet part of stored history, so append them
    // after projection instead of treating archived controls as current.
    let mut messages = refact_core::active_context::active_context(&session.messages)
        .map_err(|error| error.to_string())?
        .messages;
    messages.extend(session.post_tool_side_effects.iter().cloned());
    messages.extend(
        session
            .pending_deliveries
            .iter()
            .flat_map(|delivery| delivery.messages.iter().cloned()),
    );
    Ok(messages
        .into_iter()
        .filter(|message| message.role == "plan")
        .filter_map(|message| {
            let version = message.extra.get("plan")?.get("version")?.as_u64()?;
            u32::try_from(version)
                .ok()
                .map(|version| (version, message))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, message)| message))
}

/// Infallible variant of [`try_current_base_plan`]; `None` on invalid
/// boundaries.
pub fn current_base_plan(session: &ChatSession) -> Option<ChatMessage> {
    try_current_base_plan(session).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal_roles;
    use refact_core::chat_types::PendingDelivery;

    fn stored_plan(content: &str, version: u32, message_id: &str) -> ChatMessage {
        let mut message = internal_roles::plan("agent", version, content, None);
        message.message_id = message_id.to_string();
        message
    }

    #[test]
    fn empty_session_has_no_plan() {
        let session = ChatSession::new("empty".into());
        assert!(try_current_base_plan(&session).unwrap().is_none());
        assert!(current_base_plan(&session).is_none());
    }

    #[test]
    fn stored_plan_wins_over_empty_queue() {
        let mut session = ChatSession::new("stored".into());
        session.add_message(stored_plan("stored plan", 1, "stored-1"));
        let plan = current_base_plan(&session).unwrap();
        assert_eq!(plan.message_id, "stored-1");
        assert_eq!(plan.content.content_text_only(), "stored plan");
    }

    /// Keeps a WhenIdle delivery queued instead of landing immediately.
    fn mid_turn(session: &mut ChatSession) {
        session.turn_depth = 1;
    }

    #[test]
    fn queued_delivery_plan_is_visible() {
        let mut session = ChatSession::new("queued".into());
        mid_turn(&mut session);
        let base = PendingDelivery::new(
            vec![stored_plan("queued plan", 1, "queued-1")],
            refact_core::chat_types::PushMode::WhenIdle,
            "test",
            false,
        );
        session.enqueue_delivery(base).unwrap();
        // The delivery has not landed in stored history yet, so the only way to
        // see the plan is through the queued-deliveries extension.
        assert!(session.messages.is_empty());
        let plan = current_base_plan(&session).unwrap();
        assert_eq!(plan.message_id, "queued-1");
        assert_eq!(plan.content.content_text_only(), "queued plan");
    }

    #[test]
    fn highest_version_wins_between_stored_and_queued() {
        let mut session = ChatSession::new("mixed".into());
        session.add_message(stored_plan("stored plan", 1, "stored-1"));
        mid_turn(&mut session);
        let queued_newer = PendingDelivery::new(
            vec![stored_plan("queued plan", 2, "queued-2")],
            refact_core::chat_types::PushMode::WhenIdle,
            "test",
            false,
        );
        session.enqueue_delivery(queued_newer).unwrap();
        let plan = current_base_plan(&session).unwrap();
        assert_eq!(plan.message_id, "queued-2");
    }

    #[test]
    fn ties_resolve_to_last_max_in_queue_order() {
        let mut session = ChatSession::new("ties".into());
        session.add_message(stored_plan("stored plan", 2, "stored-2"));
        mid_turn(&mut session);
        let queued_same_version = PendingDelivery::new(
            vec![stored_plan("queued plan", 2, "queued-2")],
            refact_core::chat_types::PushMode::WhenIdle,
            "test",
            false,
        );
        session.enqueue_delivery(queued_same_version).unwrap();
        session.add_message(stored_plan("later stored plan", 1, "stored-1"));
        // ancestry: active_context preserves stored order; the queue is
        // appended after it, so the queued plan at the equal version is the
        // LAST max element and must win.
        let plan = current_base_plan(&session).unwrap();
        assert_eq!(plan.message_id, "queued-2");
    }

    #[test]
    fn post_tool_side_effects_are_visible() {
        let mut session = ChatSession::new("side-effects".into());
        session
            .post_tool_side_effects
            .push_back(stored_plan("side-effect plan", 1, "side-1"));
        let plan = current_base_plan(&session).unwrap();
        assert_eq!(plan.message_id, "side-1");
        assert_eq!(plan.content.content_text_only(), "side-effect plan");
    }

    #[test]
    fn non_plan_messages_are_ignored() {
        let mut session = ChatSession::new("noise".into());
        session.add_message(ChatMessage::new(
            "assistant".into(),
            "hello".into(),
        ));
        session.add_message(internal_roles::plan_delta(
            "test",
            serde_json::json!({}),
            "delta note",
        ));
        assert!(current_base_plan(&session).is_none());
    }

    #[test]
    fn plan_missing_version_metadata_is_ignored() {
        let mut session = ChatSession::new("malformed".into());
        let mut message = ChatMessage::new("plan".into(), "no meta".into());
        message.message_id = "no-meta".into();
        session.add_message(message);
        assert!(current_base_plan(&session).is_none());
    }

    #[test]
    fn legacy_summary_boundary_is_an_error_not_a_missing_plan() {
        let mut session = ChatSession::new("legacy".into());
        let mut legacy = ChatMessage::new("assistant".into(), "legacy".into());
        legacy.summarized_range = Some((0, 0));
        session.add_message(legacy);
        assert!(try_current_base_plan(&session).is_err());
        assert!(current_base_plan(&session).is_none());
    }

    #[test]
    fn oversized_version_ids_outside_u32_are_ignored() {
        let mut session = ChatSession::new("oversized".into());
        let mut message = ChatMessage::new("plan".into(), "huge version".into());
        message.message_id = "huge".into();
        message
            .extra
            .insert("plan".into(), serde_json::json!({ "version": u64::MAX }));
        session.add_message(message);
        assert!(current_base_plan(&session).is_none());
    }
}
