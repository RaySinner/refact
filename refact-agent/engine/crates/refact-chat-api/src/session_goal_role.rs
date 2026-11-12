use refact_core::chat_types::ChatMessage;

use crate::goal_role::GoalInstallReport;
use crate::types::ChatSession;
use crate::GoalBudget;

/// Session-local variant of [`crate::goal_role::current_base_goal`]: reads the
/// accepted-control projection so archived goals do not count as current.
pub fn try_current_base_goal(session: &ChatSession) -> Result<Option<ChatMessage>, String> {
    let projection = session.try_accepted_control_projection()?;
    Ok(crate::goal_role::current_base_goal(&projection).cloned())
}

pub fn current_base_goal(session: &ChatSession) -> Option<ChatMessage> {
    try_current_base_goal(session).ok().flatten()
}

pub fn synthesize_current_goal(session: &ChatSession) -> Option<String> {
    try_synthesize_current_goal(session).ok().flatten()
}

pub fn try_synthesize_current_goal(session: &ChatSession) -> Result<Option<String>, String> {
    let projection = session.try_accepted_control_projection()?;
    Ok(crate::goal_role::synthesize_current_goal(&projection))
}

pub fn goal_delta_events(session: &ChatSession) -> Vec<ChatMessage> {
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
    session: &mut ChatSession,
    mode: &str,
    body: &str,
    active: bool,
    budget: GoalBudget,
) -> GoalInstallReport {
    session.install_goal(mode, body, active, budget)
}
