use std::sync::Arc;

use async_trait::async_trait;
use refact_chat_api::{DeliveryOutcome, PendingDelivery, SessionsMap};

use crate::registry::BackgroundAgentRegistry;

/// Host seam for the shared agent helpers: the runtime app state supplies the
/// agent registry, the chat session map, and the unified delivery entry point.
#[async_trait]
pub trait AgentHost: Clone + Send + Sync {
    fn agents(&self) -> &Arc<BackgroundAgentRegistry>;
    fn sessions(&self) -> &SessionsMap;
    async fn deliver(
        &self,
        chat_id: &str,
        delivery: PendingDelivery,
    ) -> Result<DeliveryOutcome, String>;
}

#[async_trait]
impl<H: AgentHost + ?Sized> AgentHost for &H {
    fn agents(&self) -> &Arc<BackgroundAgentRegistry> {
        (**self).agents()
    }

    fn sessions(&self) -> &SessionsMap {
        (**self).sessions()
    }

    async fn deliver(
        &self,
        chat_id: &str,
        delivery: PendingDelivery,
    ) -> Result<DeliveryOutcome, String> {
        (**self).deliver(chat_id, delivery).await
    }
}
