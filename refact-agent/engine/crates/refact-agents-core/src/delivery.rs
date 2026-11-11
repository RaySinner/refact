use refact_chat_api::{DeliveryOutcome, PendingDelivery};

use crate::host::AgentHost;

pub async fn deliver_to_agent<H: AgentHost>(
    app: H,
    agent_id: &str,
    delivery: PendingDelivery,
) -> Result<DeliveryOutcome, String> {
    if delivery.thread_patch.is_some() {
        return Err(
            "thread parameter overrides are not supported by a live background runner".to_string(),
        );
    }
    let agents = app.agents();
    let outcome = agents.enqueue_delivery(agent_id, delivery).await?;
    publish_runner_queue(&app, agent_id).await;
    Ok(outcome)
}

pub async fn publish_runner_queue<H: AgentHost>(app: &H, agent_id: &str) {
    let agents = app.agents();
    let Ok(record) = agents.get_any(agent_id).await else {
        return;
    };
    let Some(chat_id) = record.child_chat_id else {
        return;
    };
    let session = app.sessions().read().await.get(&chat_id).cloned();
    if let Some(session) = session {
        let mut session = session.lock().await;
        session.set_runner_pending_deliveries(agents.pending_deliveries(agent_id).await);
    }
}
