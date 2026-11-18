//! Command-queue starter seam. A landed delivery (or a goal-monitor nudge) has to
//! restart the chat's command-queue processor, and that processor is the engine's
//! own dispatch loop. The engine installs the implementation through
//! `ChatCtx::queue_processor`, so a lower crate can start the processor without
//! reaching into the engine (the same shape as `generation_start`).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use refact_chat_api::types::ChatSession;
use tokio::sync::Mutex as AMutex;

use crate::app_state::AppState;

pub trait CommandQueueStarter: Send + Sync {
    /// The engine's `chat::queue::dispatch::process_command_queue` future, boxed
    /// exactly as the engine already builds it, so restarts stay non-blocking.
    fn process_command_queue(
        &self,
        app: AppState,
        session_arc: Arc<AMutex<ChatSession>>,
        processor_running: Arc<AtomicBool>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
}

/// Identical signature to the engine's own `process_command_queue`; only the
/// installed starter differs.
pub fn process_command_queue(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
    processor_running: Arc<AtomicBool>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    let starter = app.ctx.queue_processor.clone();
    starter.process_command_queue(app, session_arc, processor_running)
}
