//! Generation-start seam. `chat::queue::dispatch` starts a turn by asking the
//! generation pipeline to run (preamble + knowledge, then the LLM/tool loop), and
//! that pipeline needs the engine's subchat, prompt assembly and stream machinery.
//! The engine installs the implementation through `ChatCtx::generation`, so a
//! lower crate that owns the command loop can start a turn without reaching into
//! the engine (the same shape as `spawn` and `goal_verifier_runtime`).

use std::sync::Arc;

use async_trait::async_trait;
use refact_chat_api::types::ChatSession;
use tokio::sync::Mutex as AMutex;

use crate::app_state::AppState;

#[async_trait]
pub trait GenerationStarter: Send + Sync {
    /// Run one full generation turn: proactive context steps, preamble, LLM
    /// stream, the multi-step tool loop, and the boundary deliveries.
    async fn start_generation(&self, app: AppState, session_arc: Arc<AMutex<ChatSession>>);

    /// Assemble the session preamble (system prompt, project context, skills,
    /// MCP lazy hint) and inject auto knowledge enrichment for the turn.
    async fn prepare_session_preamble_and_knowledge(
        &self,
        app: AppState,
        session_arc: Arc<AMutex<ChatSession>>,
    );
}

/// Identical signature to the engine's own `start_generation`; only the
/// installed starter differs.
pub async fn start_generation(app: AppState, session_arc: Arc<AMutex<ChatSession>>) {
    let starter = app.ctx.generation.clone();
    starter.start_generation(app, session_arc).await;
}

/// Identical signature to the engine's own `prepare_session_preamble_and_knowledge`.
pub async fn prepare_session_preamble_and_knowledge(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) {
    let starter = app.ctx.generation.clone();
    starter
        .prepare_session_preamble_and_knowledge(app, session_arc)
        .await;
}
