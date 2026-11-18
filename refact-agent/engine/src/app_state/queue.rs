#[allow(unused_imports)]
use super::*;

/// Engine side of the `CommandQueueStarter` seam: the dispatch loop stays in
/// `chat::queue::dispatch`, and lower crates restart it through
/// `ChatCtx::queue_processor`.
pub struct EngineCommandQueueStarter;

impl refact_runtime_api::command_queue_start::CommandQueueStarter for EngineCommandQueueStarter {
    fn process_command_queue(
        &self,
        app: AppState,
        session_arc: Arc<AMutex<ChatSession>>,
        processor_running: Arc<AtomicBool>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        crate::chat::process_command_queue(app, session_arc, processor_running)
    }
}
