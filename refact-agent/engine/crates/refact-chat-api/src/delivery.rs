use crate::agent_notifications::ChatAgentScope;
use crate::{DeliveryOutcome, PendingDelivery};

/// Deliver a batch of messages to `chat_id`.
///
/// Restores the target session from its trajectory when needed and rejects
/// closed or unavailable chats honestly instead of silently dropping messages.
/// A background-agent runner that owns this chat gets the delivery first.
pub async fn deliver_to_chat<S: ChatAgentScope>(
    scope: S,
    chat_id: &str,
    delivery: PendingDelivery,
) -> Result<DeliveryOutcome, String> {
    scope.chat_agent_host().deliver_pending(chat_id, delivery).await
}
