use refact_agents_core::delivery::{deliver_to_agent, publish_runner_queue};
use refact_chat_api::delivery::deliver_to_chat;
use refact_chat_api::{DeliveryOutcome, PendingDelivery, PushMode};

use crate::app_state::AppState;

async fn live_runner(app: &AppState, chat_id: &str) -> Option<String> {
    let agent_id = app.agents.find_agent_id_by_child_chat_id(chat_id).await?;
    app.agents.has_runtime(&agent_id).await.then_some(agent_id)
}

pub async fn route_to_runner(
    app: &AppState,
    chat_id: &str,
    delivery: PendingDelivery,
) -> Option<Result<DeliveryOutcome, String>> {
    let agent_id = live_runner(app, chat_id).await?;
    Some(deliver_to_agent(app.clone(), &agent_id, delivery).await)
}

pub async fn update_runner_delivery(
    app: &AppState,
    chat_id: &str,
    id: &str,
    push: Option<PushMode>,
    cancel: bool,
) -> Option<Result<(), String>> {
    let agent_id = live_runner(app, chat_id).await?;
    let result = app
        .agents
        .update_pending_delivery(&agent_id, id, push, cancel)
        .await;
    if result.is_ok() {
        publish_runner_queue(app, &agent_id).await;
    }
    Some(result)
}

pub async fn recover_runner_deliveries(app: &AppState, chat_id: &str) -> Result<usize, String> {
    let Some(agent_id) = app.agents.find_agent_id_by_child_chat_id(chat_id).await else {
        return Ok(0);
    };
    if app.agents.has_runtime(&agent_id).await {
        return Ok(0);
    }
    let session = app.chat.sessions.read().await.get(chat_id).cloned();
    if let Some(session) = session {
        session
            .lock()
            .await
            .set_runner_pending_deliveries(Vec::new());
    }
    let mut recovered = 0;
    for mut delivery in app.agents.pending_deliveries(&agent_id).await {
        let id = delivery.id.clone();
        delivery.wake = false;
        Box::pin(deliver_to_chat(app.clone(), chat_id, delivery)).await?;
        app.agents.acknowledge_delivery(&agent_id, &id).await?;
        recovered += 1;
    }
    publish_runner_queue(app, &agent_id).await;
    Ok(recovered)
}
