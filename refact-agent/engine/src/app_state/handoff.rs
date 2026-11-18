use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex as AMutex;

use refact_chat_api::ChatSession;
use refact_runtime_api::chat_ctx::PendingHandoffFinalizer;
use refact_runtime_api::global_context::SharedGlobalContext;

/// Engine side of the pending-handoff seam: the destination-chat construction in
/// `tools::tool_handoff_to_mode` needs the subchat launcher, the task storage and
/// the trajectory store, all of which are engine handles.
pub struct EnginePendingHandoffFinalizer {
    gcx: SharedGlobalContext,
}

impl EnginePendingHandoffFinalizer {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl PendingHandoffFinalizer for EnginePendingHandoffFinalizer {
    async fn finalize_pending_handoff(
        &self,
        session_arc: &Arc<AMutex<ChatSession>>,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        crate::tools::tool_handoff_to_mode::finalize_pending_handoff(
            self.gcx.clone(),
            session_arc,
            payload,
        )
        .await
    }
}
