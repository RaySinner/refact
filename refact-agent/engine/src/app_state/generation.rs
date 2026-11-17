#[allow(unused_imports)]
use super::*;

/// Engine side of the `GenerationStarter` seam: the turn pipeline stays in
/// `chat::generation`, and lower crates start a turn through `ChatCtx::generation`.
pub struct EngineGenerationStarter;

#[async_trait]
impl refact_runtime_api::generation_start::GenerationStarter for EngineGenerationStarter {
    async fn start_generation(&self, app: AppState, session_arc: Arc<AMutex<ChatSession>>) {
        crate::chat::generation::start_generation(app, session_arc).await;
    }

    async fn prepare_session_preamble_and_knowledge(
        &self,
        app: AppState,
        session_arc: Arc<AMutex<ChatSession>>,
    ) {
        crate::chat::generation::prepare_session_preamble_and_knowledge(app, session_arc).await;
    }
}
