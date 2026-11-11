use refact_chat_api::{BackgroundAgentSummary, ChatEvent};
use tokio::sync::oneshot;

use crate::host::AgentHost;
use crate::types::BackgroundAgent;

pub struct SpawnHandle {
    pub agent_id: String,
    pub child_chat_id: String,
    pub worktree_branch: Option<String>,
    pub auto_merge: Option<bool>,
    pub completion_rx: oneshot::Receiver<BackgroundAgent>,
}

pub async fn emit_background_agent_update<H: AgentHost>(app: H, record: &BackgroundAgent) {
    let mut destinations = vec![record.parent_chat_id.clone()];
    if let Some(root_chat_id) = record.parent_root_chat_id.as_ref() {
        if root_chat_id != &record.parent_chat_id {
            destinations.push(root_chat_id.clone());
        }
    }
    for chat_id in destinations {
        emit_background_agent_update_to_session(&app, record, &chat_id).await;
    }
}

async fn emit_background_agent_update_to_session<H: AgentHost>(
    app: &H,
    record: &BackgroundAgent,
    chat_id: &str,
) {
    let session_arc = {
        let sessions = app.sessions().read().await;
        sessions.get(chat_id).cloned()
    };
    let Some(session_arc) = session_arc else {
        return;
    };
    let mut session = session_arc.lock().await;
    if session.closed {
        return;
    }
    let agent = BackgroundAgentSummary::from(record);
    session.upsert_background_agent(agent.clone());
    let seq = session.event_seq.saturating_add(1);
    session.emit(ChatEvent::BackgroundAgentUpdated {
        chat_id: chat_id.to_string(),
        seq,
        agent,
    });
}
