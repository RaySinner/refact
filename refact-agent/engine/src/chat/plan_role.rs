pub use refact_chat_api::plan_role::*;

// Session-local variants live in refact-chat-api::session_plan_role and are
// re-exported here explicitly so they override the glob-imported generic
// names above (the generic `try_current_base_plan<S: PlanRoleSession>` and
// `current_base_plan<S>` would otherwise win at call sites that pass a
// `&crate::chat::types::ChatSession` and silently drop the queued
// post_tool_side_effects/pending_deliveries extension).
pub use refact_chat_api::session_plan_role::{current_base_plan, try_current_base_plan};
