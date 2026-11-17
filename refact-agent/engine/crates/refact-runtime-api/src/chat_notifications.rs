use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use tokio::task::JoinHandle;

use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_chat_api::types::ChatSession;
use refact_chat_api::{
    ChatEvent, ChatMessage, DeliveryOutcome, ExecProcessSpawn, PendingDelivery, PushMode,
};
use refact_exec::{ExecStatus, ProcessCompletionEvent, ProcessSpawnEvent};

use crate::chat_ctx::ChatCtx;

/// Stable, restart-safe dedupe key for a process completion notice. The exec
/// process id is unique per process, so one completion can only land once even
/// if the broadcast is replayed or the engine restarts mid-delivery.
pub fn process_completion_delivery_id(event: &ProcessCompletionEvent) -> String {
    format!("process-completed-{}", event.process_id)
}

pub fn spawn_notification_subscriber(ctx: ChatCtx) -> JoinHandle<()> {
    let mut completion_rx = ctx.runtime.exec_registry.subscribe_completion();
    let mut spawn_rx = ctx.runtime.exec_registry.subscribe_spawn();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = wait_for_shutdown(ctx.clone()) => break,
                event = completion_rx.recv() => match event {
                    Ok(event) => handle_process_completion(ctx.clone(), event).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        tracing::warn!("process completion notification subscriber lagged by {count} event(s)");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                event = spawn_rx.recv() => match event {
                    Ok(event) => handle_process_spawn(ctx.clone(), event).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        tracing::warn!("process spawn notification subscriber lagged by {count} event(s)");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    })
}

async fn wait_for_shutdown(ctx: ChatCtx) {
    while !ctx.runtime.shutdown_flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn handle_process_spawn(ctx: ChatCtx, event: ProcessSpawnEvent) {
    let session_arc = {
        let sessions = ctx.chat.sessions.read().await;
        sessions.get(&event.chat_id).cloned()
    };
    let Some(session_arc) = session_arc else {
        return;
    };
    let mut session = session_arc.lock().await;
    if session.closed {
        return;
    }
    let seq = session.event_seq.saturating_add(1);
    session.emit(ChatEvent::ExecProcessSpawned {
        chat_id: event.chat_id,
        seq,
        process: ExecProcessSpawn {
            process_id: event.process_id.to_string(),
            command_preview: event.command_preview,
            mode: event.mode.to_string(),
            tty: event.tty,
            status: status_label(&event.status).to_string(),
            started_at: event.started_at_ms,
        },
    });
}

/// Deliver a background/service process completion into its owning chat using
/// the unified delivery hub, with the `push` mode selected when the process was
/// started (default `append`).
///
/// The hub owns restoring an unloaded session, runner-owned routing, dedupe and
/// waking the queue, so this function only builds the delivery and reports
/// failures honestly instead of dropping them.
pub async fn handle_process_completion(ctx: ChatCtx, event: ProcessCompletionEvent) {
    if let Err(error) = deliver_process_completion(ctx, &event).await {
        tracing::warn!(
            process_id = %event.process_id,
            chat_id = %event.chat_id,
            "process completion notification was not delivered: {error}"
        );
    }
}

pub async fn deliver_process_completion(
    ctx: ChatCtx,
    event: &ProcessCompletionEvent,
) -> Result<DeliveryOutcome, String> {
    let chat_id = event.chat_id.clone();
    ctx.chat
        .facade
        .deliver_messages(&chat_id, process_completion_delivery(event))
        .await
}

pub fn process_completion_delivery(event: &ProcessCompletionEvent) -> PendingDelivery {
    PendingDelivery::with_id(
        process_completion_delivery_id(event),
        vec![process_completion_message(event)],
        event.push,
        "exec.registry".to_string(),
        true,
    )
}

pub fn inject_process_completion_message(session: &mut ChatSession, event: ProcessCompletionEvent) {
    let envelope = process_completion_envelope_event(&event);
    session.add_message(process_completion_message(&event));
    session.emit(envelope);
}

pub fn process_completion_envelope_event(completion: &ProcessCompletionEvent) -> ChatEvent {
    ChatEvent::ProcessCompleted {
        process_id: completion.process_id.to_string(),
        status: status_label(&completion.status).to_string(),
        exit_code: completion.exit_code,
        short_description: completion.short_description.clone(),
        mode: completion.mode.to_string(),
    }
}

pub fn process_completion_message(completion: &ProcessCompletionEvent) -> ChatMessage {
    let status = status_label(&completion.status);
    let mode = completion.mode.to_string();
    let exit_code = completion.exit_code;
    let process_id = completion.process_id.to_string();
    let duration_ms = completion.duration_ms;
    let short_description = completion.short_description.clone();
    let exit_text = exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let content = format!(
        "Background process '{}' {} (exit {})",
        short_description, status, exit_text
    );
    event(
        EventSubkind::ProcessCompleted,
        "exec.registry",
        json!({
            "process_id": process_id,
            "status": status,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "short_description": short_description,
            "mode": mode,
            "push": PushMode::as_str(completion.push),
        }),
        content,
    )
}

pub fn status_label(status: &ExecStatus) -> &'static str {
    match status {
        ExecStatus::Starting => "starting",
        ExecStatus::Running => "running",
        ExecStatus::Exited { .. } => "exited",
        ExecStatus::SandboxLauncherFailed { .. } => "sandbox_launcher_failed",
        ExecStatus::Failed { .. } => "failed",
        ExecStatus::Killed => "killed",
        ExecStatus::TimedOut => "timed_out",
    }
}
