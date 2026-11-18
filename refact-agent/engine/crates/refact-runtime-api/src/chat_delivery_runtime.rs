//! Unified message delivery into a chat.
//!
//! Every producer (planner, cron, hooks, background runner, tools) reaches a
//! chat through [`deliver_to_chat`]. The `push` mode on the delivery decides
//! where it lands relative to an in-flight turn:
//!
//! * `preempt` — abort the draft, record a cancellation note, land now.
//! * `append` (default) — at the next safe message boundary, before the next
//!   generation, never inside an open assistant/tool-result window.
//! * `when_idle` — only after the whole turn (including the multi-step tool
//!   loop) has finished.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::Mutex as AMutex;

use crate::command_queue_start::process_command_queue;

use crate::app_state::AppState;
use crate::trajectories::store::{maybe_save_trajectory_with_intent, try_save_trajectory_with_intent};
use refact_chat_api::types::{
    ChatSession, DeliveryOutcome, PendingDelivery, PushMode, SessionState, TrajectoryCommitIntent,
};

pub use refact_chat_api::delivery::deliver_to_chat;

/// Deliver a batch of messages to `chat_id`.
///
/// Restores the target session from its trajectory when needed and rejects
/// closed or unavailable chats honestly instead of silently dropping messages.
/// A background-agent runner that owns this chat gets the delivery first.
pub async fn deliver_pending(
    app: AppState,
    chat_id: &str,
    delivery: PendingDelivery,
) -> Result<DeliveryOutcome, String> {
    if delivery.messages.is_empty() {
        return Err("delivery must contain at least one message".to_string());
    }

    if let Some(patch) = &delivery.thread_patch {
        let fields = patch
            .as_object()
            .ok_or("delivery thread_patch must be an object")?;
        if fields
            .iter()
            .any(|(key, value)| !matches!(key.as_str(), "mode" | "model") || !value.is_string())
        {
            return Err("delivery thread_patch supports only string mode and model fields".into());
        }
    }

    if let Some(result) =
        crate::agents_delivery::route_to_runner(&app, chat_id, delivery.clone()).await
    {
        return result;
    }

    let sessions = app.chat.sessions.clone();
    if let Some(session) = sessions.read().await.get(chat_id).cloned() {
        if session.lock().await.closed {
            return Err(format!("chat '{chat_id}' is closed"));
        }
    }
    if !crate::session_runtime::try_restore_session_if_trajectory_exists(app.chat_ctx(), &sessions, chat_id).await
    {
        return Err(format!(
            "chat '{chat_id}' does not exist or cannot be restored"
        ));
    }
    let session_arc = sessions
        .read()
        .await
        .get(chat_id)
        .cloned()
        .ok_or_else(|| format!("chat '{chat_id}' is unavailable"))?;

    let (outcome, wake, should_start_processor, processor_running, queue_notify) = {
        let mut session = session_arc.lock().await;
        if session.closed {
            return Err(format!("chat '{chat_id}' is closed"));
        }
        let wake = delivery.wake;
        let outcome = session.enqueue_delivery(delivery)?;
        // Only a landed delivery can need a restart; queued ones will be drained
        // at their boundary by the turn that is already running.
        let should_start_processor = wake
            && (outcome == DeliveryOutcome::Delivered
                || (outcome == DeliveryOutcome::Duplicate
                    && !session.delivery_wake_sources.is_empty()))
            && matches!(
                session.runtime.state,
                SessionState::Idle | SessionState::Completed | SessionState::WaitingUserInput
            );
        (
            outcome,
            wake,
            should_start_processor,
            session.queue_processor_running.clone(),
            session.queue_notify.clone(),
        )
    };

    try_save_trajectory_with_intent(
        app.clone(),
        session_arc.clone(),
        TrajectoryCommitIntent::Required,
    )
    .await?;

    if should_start_processor {
        // Coalesce wakes: at most one non-interrupting processor restart.
        if !processor_running.swap(true, Ordering::SeqCst) {
            tokio::spawn(process_command_queue(
                app,
                session_arc.clone(),
                processor_running,
            ));
        } else {
            queue_notify.notify_one();
        }
    } else if wake {
        queue_notify.notify_one();
    }

    Ok(outcome)
}

/// Update a still-pending delivery. Runner-owned deliveries are routed to the
/// runner first; otherwise the session's own pending queue is edited, which
/// works while a generation is in flight.
pub async fn update_pending_delivery_in_chat(
    app: AppState,
    chat_id: &str,
    delivery_id: &str,
    push: Option<PushMode>,
    cancel: bool,
) -> Result<(), String> {
    if let Some(result) =
        crate::agents_delivery::update_runner_delivery(&app, chat_id, delivery_id, push, cancel)
            .await
    {
        return result;
    }

    let session_arc = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    let Some(session_arc) = session_arc else {
        return Err(format!("chat '{chat_id}' is not active"));
    };

    let ready = {
        let mut session = session_arc.lock().await;
        session.update_pending_delivery(delivery_id, push, cancel)?
    };

    if ready {
        drain_deliveries_at_boundary(app.clone(), session_arc.clone()).await;
    }
    try_save_trajectory_with_intent(app, session_arc, TrajectoryCommitIntent::Required).await?;
    Ok(())
}

/// Land every pending delivery whose boundary is now open. Called from the
/// generation/tool boundaries; safe to call when nothing is pending.
pub async fn drain_deliveries_at_boundary(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> bool {
    let (delivered, wake, processor_running, queue_notify) = {
        let mut session = session_arc.lock().await;
        session.drain_post_tool_side_effects();
        if session.pending_deliveries.is_empty() {
            return false;
        }
        let (delivered_ids, wake) = session.drain_pending_deliveries();

        (
            !delivered_ids.is_empty(),
            wake,
            session.queue_processor_running.clone(),
            session.queue_notify.clone(),
        )
    };

    if !delivered {
        return false;
    }

    maybe_save_trajectory_with_intent(
        app.clone(),
        session_arc.clone(),
        TrajectoryCommitIntent::Required,
    )
    .await;

    if wake {
        let idle = {
            let session = session_arc.lock().await;
            matches!(
                session.runtime.state,
                SessionState::Idle | SessionState::Completed | SessionState::WaitingUserInput
            )
        };
        if idle && !processor_running.swap(true, Ordering::SeqCst) {
            tokio::spawn(process_command_queue(app, session_arc, processor_running));
        } else {
            queue_notify.notify_one();
        }
    }

    true
}

