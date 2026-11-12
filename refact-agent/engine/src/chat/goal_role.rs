pub use refact_chat_api::goal_role::*;

// Session-local variants live in refact-chat-api::session_goal_role and are
// re-exported here explicitly so they override the glob-imported generic names
// above (the generic `current_base_goal<S: GoalRoleSession>` would otherwise win
// at call sites that pass a `&ChatSession` and read archived goals as current).
pub use refact_chat_api::session_goal_role::{
    current_base_goal, goal_delta_events, install_goal, synthesize_current_goal,
    try_current_base_goal, try_synthesize_current_goal,
};
