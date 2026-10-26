//! Buddy speaking into a live task-agent chat.
//!
//! Monolith glue layer for delivering interjections. Pure logic lives in `refact_buddy_core::chat_interjection`.

use tracing::debug;

use crate::app_state::AppState;
use crate::call_validation::ChatMessage;
use crate::chat::delivery::deliver_to_chat;
use crate::chat::internal_roles::{event, EventSubkind};
use crate::chat::types::SessionState;
use refact_core::chat_types::{DeliveryOutcome, PendingDelivery, PushMode};

use super::settings::BuddySettings;

pub use refact_buddy_core::chat_interjection::*;

/// The message Buddy delivers. An `event` role message, matching every other
/// non-user participant in this engine's transcripts.
fn interjection_message(target: &InterjectionTarget) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        INTERJECTION_SOURCE,
        serde_json::json!({
            "kind": "agent_interjection",
            "task_id": target.task_id,
            "card_id": target.card_id,
            "agent_chat_id": target.chat_id,
            "idle_secs": target.idle_for.as_secs(),
            "is_room_member": target.is_room_member,
        }),
        build_interjection_text(target),
    )
}

/// Resolve the agents worth speaking to.
///
/// The identity comes from `load_board`, **not** from the fact that triggered
/// this. `TaskHealthObserver` collapses every `doing` card in a task into a
/// single max-heartbeat and emits one `TaskStuck` fact, so the fact cannot say
/// which agent is stuck. It only says *there is stuck work*; the board says to
/// whom we are talking.
///
/// A target must be an agent whose session we can see, is not mid-turn, and has
/// actually been quiet for `INTERJECTION_MIN_IDLE`. Anything we cannot prove
/// is skipped rather than guessed at.
pub async fn collect_interjection_targets(
    app: &AppState,
    task_ids: &[String],
) -> Vec<InterjectionTarget> {
    let mut targets = Vec::new();
    for task_id in task_ids {
        let board = crate::tasks::storage::load_board(app.gcx.clone(), task_id)
            .await
            .ok();
        let candidates = collect_board_candidates(task_id, board.as_ref());

        for candidate in candidates {
            let idle_for = match agent_idle_for(app, &candidate.chat_id).await {
                Ok(idle_for) => idle_for,
                Err(reason) => {
                    debug!(
                        target: "buddy.chat_interjection",
                        task_id = %candidate.task_id,
                        card_id = %candidate.card_id,
                        agent_chat_id = %candidate.chat_id,
                        reason = reason.as_str(),
                        "buddy interjection skipped: agent is not an idle target"
                    );
                    continue;
                }
            };
            if idle_for < INTERJECTION_MIN_IDLE {
                debug!(
                    target: "buddy.chat_interjection",
                    task_id = %candidate.task_id,
                    card_id = %candidate.card_id,
                    agent_chat_id = %candidate.chat_id,
                    idle_secs = idle_for.as_secs(),
                    "buddy interjection skipped: agent has not been silent long enough"
                );
                continue;
            }
            targets.push(InterjectionTarget {
                task_id: candidate.task_id,
                card_id: candidate.card_id,
                chat_id: candidate.chat_id,
                card_title: candidate.card_title,
                idle_for,
                is_room_member: candidate.is_room_member,
            });
        }
    }
    targets
}

/// How long the agent's session has been silent, or the reason there is no
/// answer: the chat is not live, or it is mid-turn.
///
/// `turn_depth` is checked as well as the state, because `Idle` is transient
/// between steps of a multi-step tool loop: the same reason
/// `PushMode::WhenIdle` waits for `turn_depth == 0` rather than for `Idle`.
async fn agent_idle_for(
    app: &AppState,
    chat_id: &str,
) -> Result<std::time::Duration, InterjectionSkipReason> {
    let session_arc = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    }
    .ok_or(InterjectionSkipReason::AgentChatUnknown)?;
    let session = session_arc.lock().await;
    if session.closed {
        return Err(InterjectionSkipReason::AgentChatUnknown);
    }
    let busy = session.turn_depth > 0
        || !matches!(
            session.runtime.state,
            SessionState::Idle | SessionState::Completed
        );
    if busy {
        return Err(InterjectionSkipReason::AgentBusy);
    }
    Ok(session.last_activity.elapsed())
}

/// One interjection, gated and delivered.
///
/// On success the caller must record the emission against the speech budget
/// (see `BuddyService::record_interjection_emission`), otherwise the budget
/// never advances and the intent becomes unlimited.
pub async fn maybe_interject_with_agent(
    app: AppState,
    target: &InterjectionTarget,
    settings: BuddySettings,
    rotation: refact_buddy_core::state::SpeechRotationState,
    auto_quiet_window: Option<(u8, u8)>,
    llm_budget_exhausted: bool,
) -> Result<DeliveryOutcome, InterjectionSkipReason> {
    let now = chrono::Utc::now();
    let local_hour = chrono::Timelike::hour(&chrono::Local::now());
    gate_interjection(
        &settings,
        &rotation,
        &target.chat_id,
        local_hour,
        auto_quiet_window,
        now,
        llm_budget_exhausted,
    )?;

    let delivery = PendingDelivery::with_id(
        interjection_delivery_id(target),
        vec![interjection_message(target)],
        // Always `WhenIdle`: the notice must wait for the whole turn, and must
        // never abort one.
        PushMode::WhenIdle,
        INTERJECTION_SOURCE,
        true,
    );

    match deliver_to_chat(app, &target.chat_id, delivery).await {
        Ok(DeliveryOutcome::Duplicate) => Err(InterjectionSkipReason::AlreadyDelivered),
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            debug!(
                target: "buddy.chat_interjection",
                task_id = %target.task_id,
                card_id = %target.card_id,
                agent_chat_id = %target.chat_id,
                error = %error,
                "buddy interjection delivery failed"
            );
            Err(InterjectionSkipReason::DeliveryFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
        let app = crate::app_state::AppState::from_gcx(
            crate::global_context::tests::make_test_gcx().await,
        )
        .await;

        let session = Arc::new(tokio::sync::Mutex::new(
            crate::chat::types::ChatSession::new("agent-busy".to_string()),
        ));
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
        let session = Arc::new(tokio::sync::Mutex::new(
            crate::chat::types::ChatSession::new("agent-queued".to_string()),
        ));
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
        let source = include_str!("chat_interjection.rs");
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
