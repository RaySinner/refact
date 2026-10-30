use refact_core::chat_types::ChatMessage;

use crate::goal_role::GoalRoleSession;
use crate::plan_role::PlanRoleSession;
use crate::types::ChatSession;

impl GoalRoleSession for ChatSession {
    fn goal_role_messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    fn add_goal_role_message(&mut self, message: ChatMessage) {
        self.add_message(message);
    }
}

impl PlanRoleSession for ChatSession {
    fn plan_role_messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    fn add_plan_role_message(&mut self, message: ChatMessage) {
        self.add_message(message);
    }
}
