//! Buddy speaking into a live task-agent chat.
//!
//! Monolith glue layer for delivering interjections. The pure logic
//! (`gate_interjection`, `collect_board_candidates`, `build_interjection_text`,
//! `interjection_delivery_id`) lives in `refact_buddy_core::chat_interjection`;
//! the delivery glue lives in `refact_runtime_api::buddy_chat_interjection`.

pub use refact_runtime_api::buddy_chat_interjection::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::app_state::AppState;
    use crate::chat::types::{ChatSession, SessionState};

    fn target(chat_id: &str) -> InterjectionTarget {
        InterjectionTarget {
            task_id: "task-1".to_string(),
            card_id: "T-6".to_string(),
            chat_id: chat_id.to_string(),
            card_title: "Buddy may speak into an agent chat".to_string(),
            idle_for: std::time::Duration::from_secs(600),
            is_room_member: false,
        }
    }

    #[tokio::test]
    async fn interjection_skips_agent_with_active_turn() {
        let app = crate::app_state::app_state_from_gcx(
            crate::global_context::tests::make_test_gcx().await,
        )
        .await;

        let session = Arc::new(tokio::sync::Mutex::new(ChatSession::new(
            "agent-busy".to_string(),
        )));
        {
            let mut guard = session.lock().await;
            guard.runtime.state = SessionState::Generating;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert("agent-busy".to_string(), session);
        assert_eq!(
            agent_idle_for(&app, "agent-busy").await,
            Err(InterjectionSkipReason::AgentBusy),
            "a generating agent is not a target"
        );

        // A non-zero `turn_depth` means the multi-step tool loop is still open
        // even though the state may read `Idle` between steps.
        let session = Arc::new(tokio::sync::Mutex::new(ChatSession::new(
            "agent-queued".to_string(),
        )));
        {
            let mut guard = session.lock().await;
            guard.turn_depth = 1;
        }
        app.chat
            .sessions
            .write()
            .await
            .insert("agent-queued".to_string(), session);
        assert_eq!(
            agent_idle_for(&app, "agent-queued").await,
            Err(InterjectionSkipReason::AgentBusy)
        );

        // Unknown chat: we cannot prove idleness, so we stay quiet.
        assert_eq!(
            agent_idle_for(&app, "agent-missing").await,
            Err(InterjectionSkipReason::AgentChatUnknown)
        );
    }

    #[test]
    fn interjection_never_uses_preempt_for_idle_chat() {
        // The invariant, stated once: this module must never name a push mode
        // stronger than `WhenIdle`. A source-level assertion is the only check
        // that survives someone adding a `Preempt` branch later.
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/refact-runtime-api/src/buddy_chat_interjection.rs"
        ));
        // The literal is assembled so this assertion does not match itself.
        let forbidden = ["PushMode::", "Preempt"].concat();
        assert!(
            !source.contains(&forbidden),
            "chat_interjection must never construct a preempting push mode"
        );
        assert!(source.contains("PushMode::WhenIdle"));
    }

    #[test]
    fn interjection_message_is_a_provenance_stamped_event() {
        let message = interjection_message(&target("agent-1"));
        assert_eq!(message.role, "event");
        assert_eq!(
            message.extra["event"]["source"],
            serde_json::json!(INTERJECTION_SOURCE)
        );
        assert_eq!(
            message.extra["event"]["payload"]["kind"],
            serde_json::json!("agent_interjection")
        );
        assert_eq!(
            message.extra["event"]["payload"]["card_id"],
            serde_json::json!("T-6")
        );
    }
}
