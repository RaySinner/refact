pub use refact_chat_api::goal_role::*;

pub fn try_current_base_goal(
    session: &crate::chat::types::ChatSession,
) -> Result<Option<crate::call_validation::ChatMessage>, String> {
    let projection = session.try_accepted_control_projection()?;
    Ok(refact_chat_api::goal_role::current_base_goal(&projection).cloned())
}

pub fn current_base_goal(
    session: &crate::chat::types::ChatSession,
) -> Option<crate::call_validation::ChatMessage> {
    try_current_base_goal(session).ok().flatten()
}

pub fn synthesize_current_goal(session: &crate::chat::types::ChatSession) -> Option<String> {
    try_synthesize_current_goal(session).ok().flatten()
}

pub fn try_synthesize_current_goal(
    session: &crate::chat::types::ChatSession,
) -> Result<Option<String>, String> {
    let projection = session.try_accepted_control_projection()?;
    Ok(refact_chat_api::goal_role::synthesize_current_goal(
        &projection,
    ))
}

pub fn goal_delta_events(
    session: &crate::chat::types::ChatSession,
) -> Vec<crate::call_validation::ChatMessage> {
    session
        .accepted_control_messages()
        .filter(|message| {
            message.role == "event"
                && message
                    .extra
                    .get("event")
                    .and_then(|event| event.get("subkind"))
                    .and_then(serde_json::Value::as_str)
                    == Some("goal_delta")
        })
        .collect()
}

pub fn install_goal(
    session: &mut crate::chat::types::ChatSession,
    mode: &str,
    body: &str,
    active: bool,
    budget: refact_chat_api::GoalBudget,
) -> GoalInstallReport {
    session.install_goal(mode, body, active, budget)
}
