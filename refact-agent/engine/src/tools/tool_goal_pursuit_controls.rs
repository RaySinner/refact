pub use refact_tools::tools::tool_goal_pursuit_controls::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::{self, EventSubkind};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::types::{ChatSession, GoalStatus, PendingDelivery, PushMode};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use refact_chat_api::GoalBudget;

    fn session_with_goal() -> ChatSession {
        let mut session = ChatSession::new("goal-controls-test".to_string());
        session.install_goal("agent", "ship it", true, GoalBudget::default());
        session
    }

    #[test]
    fn purge_cancels_only_goal_wake_deliveries() {
        let mut session = session_with_goal();
        session.turn_depth = 1;
        for (id, source) in [
            ("goal", "chat.goal_monitor"),
            ("other", "process.subscribe"),
        ] {
            session
                .enqueue_delivery(PendingDelivery::with_id(
                    id,
                    vec![internal_roles::event(
                        EventSubkind::GoalPursuit,
                        source,
                        json!({}),
                        "pending",
                    )],
                    PushMode::WhenIdle,
                    source,
                    true,
                ))
                .unwrap();
        }
        session.purge_goal_generated_wakes();
        assert_eq!(session.pending_deliveries.len(), 2);
        assert_eq!(session.pending_deliveries[0].id, "goal");
        assert!(!session.pending_deliveries[0].wake);
        assert_eq!(session.pending_deliveries[1].id, "other");
        assert!(session.pending_deliveries[1].wake);
        session.turn_depth = 0;
        let (delivered, wake) = session.drain_pending_deliveries();
        assert_eq!(delivered, vec!["goal", "other"]);
        assert!(wake);
        assert!(!session.delivery_wake_sources.contains("goal"));
        assert!(session.delivery_wake_sources.contains("external"));
    }

    #[test]
    fn require_pursuable_goal_rejects_missing_or_held_goals() {
        let session = ChatSession::new("no-goal".to_string());
        assert!(require_pursuable_goal(&session).is_err());

        let mut paused = session_with_goal();
        paused.goal_set_status(GoalStatus::Paused);
        assert!(require_pursuable_goal(&paused).is_err());

        let active = session_with_goal();
        assert!(require_pursuable_goal(&active).is_ok());
    }

    #[test]
    fn snooze_helper_sets_projection_and_survives_rebuild() {
        let mut session = session_with_goal();
        assert!(session.goal_set_snooze(Some(99_000)));
        assert_eq!(
            session.goal.as_ref().unwrap().snoozed_until_ms,
            Some(99_000)
        );

        session.rebuild_goal_projection_from_messages();
        assert_eq!(
            session.goal.as_ref().unwrap().snoozed_until_ms,
            Some(99_000)
        );

        assert!(session.goal_reset_no_progress() || session.goal.is_some());
        assert_eq!(session.goal.as_ref().unwrap().snoozed_until_ms, None);
    }

    #[test]
    fn purge_removes_only_goal_wake_deliveries() {
        let mut session = session_with_goal();
        session.turn_depth = 1;
        for (id, source) in [("goal", "chat.goal_monitor"), ("other", "scheduler")] {
            session
                .enqueue_delivery(PendingDelivery::with_id(
                    id,
                    vec![internal_roles::event(
                        EventSubkind::SystemNotice,
                        source,
                        json!({}),
                        "wake",
                    )],
                    PushMode::WhenIdle,
                    source,
                    true,
                ))
                .unwrap();
        }
        session.purge_goal_generated_wakes();
        assert_eq!(session.pending_deliveries.len(), 2);
        assert_eq!(session.pending_deliveries[0].id, "goal");
        assert!(!session.pending_deliveries[0].wake);
        assert_eq!(session.pending_deliveries[1].id, "other");
        assert!(session.pending_deliveries[1].wake);
        session.turn_depth = 0;
        let (delivered, wake) = session.drain_pending_deliveries();
        assert_eq!(delivered, vec!["goal", "other"]);
        assert!(wake);
        assert!(!session.delivery_wake_sources.contains("goal"));
        assert!(session.delivery_wake_sources.contains("external"));
    }

    #[test]
    fn purge_removes_goal_generated_regenerates_only() {
        let mut session = session_with_goal();
        session
            .command_queue
            .push_back(refact_chat_api::CommandRequest {
                client_request_id: "goal-nudge-x".to_string(),
                priority: true,
                command: refact_chat_api::ChatCommand::Regenerate {},
            });
        session
            .command_queue
            .push_back(refact_chat_api::CommandRequest {
                client_request_id: "user-x".to_string(),
                priority: false,
                command: refact_chat_api::ChatCommand::Regenerate {},
            });

        let now = std::time::Instant::now();
        session
            .command_enqueued_at
            .insert("goal-nudge-x".to_string(), now);
        session
            .command_enqueued_at
            .insert("user-x".to_string(), now);

        session.purge_goal_generated_wakes();
        assert_eq!(session.command_queue.len(), 1);
        assert!(!session.command_enqueued_at.contains_key("goal-nudge-x"));
        assert!(session.command_enqueued_at.contains_key("user-x"));
        assert_eq!(
            session.command_queue.front().unwrap().client_request_id,
            "user-x"
        );
    }
}
