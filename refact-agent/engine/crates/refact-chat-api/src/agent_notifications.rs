use std::sync::Arc;

use async_trait::async_trait;
use refact_tasks::types::TaskBoard;

use crate::{DeliveryOutcome, PendingDelivery, PushMode};

/// Host seam for planner notifications. The concrete implementation lives in the
/// engine (it needs task storage, trajectories and the delivery pipeline); this
/// crate only owns the call sites so other crates can reach them.
#[async_trait]
pub trait ChatAgentHost: Send + Sync {
    async fn notify_agents_finished(
        &self,
        task_id: &str,
        board: &TaskBoard,
        all_finished: bool,
        planner_chat_id: Option<&str>,
    ) -> Result<(), String>;

    async fn notify_blocking_question(
        &self,
        task_id: &str,
        card_id: &str,
        planner_chat_id: &str,
        question_id: &str,
        question: &str,
        push: PushMode,
    ) -> Result<DeliveryOutcome, String>;

    async fn deliver_pending(
        &self,
        chat_id: &str,
        delivery: PendingDelivery,
    ) -> Result<DeliveryOutcome, String>;
}

/// Access to the agent host from a composite context (`ChatCtx`, `AppState`).
pub trait ChatAgentScope {
    fn chat_agent_host(&self) -> &Arc<dyn ChatAgentHost>;
}

pub async fn notify_planner_agents_finished<S: ChatAgentScope>(
    scope: S,
    task_id: &str,
    board: &TaskBoard,
    all_finished: bool,
    planner_chat_id: Option<&str>,
) -> Result<(), String> {
    scope
        .chat_agent_host()
        .notify_agents_finished(task_id, board, all_finished, planner_chat_id)
        .await
}

pub async fn notify_planner_blocking_question<S: ChatAgentScope>(
    scope: S,
    task_id: &str,
    card_id: &str,
    planner_chat_id: &str,
    question_id: &str,
    question: &str,
    push: PushMode,
) -> Result<DeliveryOutcome, String> {
    scope
        .chat_agent_host()
        .notify_blocking_question(
            task_id,
            card_id,
            planner_chat_id,
            question_id,
            question,
            push,
        )
        .await
}
