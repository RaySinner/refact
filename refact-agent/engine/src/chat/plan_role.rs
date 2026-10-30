pub use refact_chat_api::plan_role::*;

pub fn current_base_plan(
    session: &crate::chat::types::ChatSession,
) -> Option<crate::call_validation::ChatMessage> {
    try_current_base_plan(session).ok().flatten()
}

pub fn try_current_base_plan(
    session: &crate::chat::types::ChatSession,
) -> Result<Option<crate::call_validation::ChatMessage>, String> {
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
