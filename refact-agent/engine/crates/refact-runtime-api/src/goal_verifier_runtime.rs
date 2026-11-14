//! Goal-verifier runner seam. Running the verifier needs the engine's chat
//! preparation, tokenizer cache and LLM stream, while the `validate_goal` tool
//! needs only the call, so the engine installs the implementation through
//! `ChatCtx::goal_verifier` (the same shape as `MemoryRuntime`).

use std::sync::Arc;

use async_trait::async_trait;
use refact_chat_api::goal_verifier::GoalVerifierReply;
use refact_chat_api::types::ChatSession;
use tokio::sync::Mutex as AMutex;

use crate::app_state::AppState;

#[async_trait]
pub trait GoalVerifierRunner: Send + Sync {
    async fn run_goal_verifier(
        &self,
        app: &AppState,
        session_arc: Arc<AMutex<ChatSession>>,
    ) -> Result<GoalVerifierReply, String>;
}

/// Identical signature to the engine's own `run_goal_verifier`; only the
/// installed runner differs.
pub async fn run_goal_verifier(
    app: AppState,
    session_arc: Arc<AMutex<ChatSession>>,
) -> Result<GoalVerifierReply, String> {
    let runner = app.ctx.goal_verifier.clone();
    runner.run_goal_verifier(&app, session_arc).await
}
