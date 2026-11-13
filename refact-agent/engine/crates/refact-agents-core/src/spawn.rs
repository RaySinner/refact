use std::sync::Arc;

use refact_chat_api::{BackgroundAgentSummary, ChatEvent, GoalBudget, GoalCriterion, PushMode, TaskMeta};
use refact_core::worktree_meta::WorktreeMeta;
use serde_json::Value;
use tokio::sync::{mpsc::UnboundedSender, oneshot, Mutex as AMutex};

use crate::host::AgentHost;
use crate::types::{BackgroundAgent, BgAgentKind};

pub struct SpawnHandle {
    pub agent_id: String,
    pub child_chat_id: String,
    pub worktree_branch: Option<String>,
    pub auto_merge: Option<bool>,
    pub completion_rx: oneshot::Receiver<BackgroundAgent>,
}

#[derive(Clone)]
pub struct SpawnRequest {
    pub kind: BgAgentKind,
    pub parent_chat_id: String,
    pub parent_root_chat_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub config_name: String,
    pub title: String,
    pub prompt: String,
    pub tools: Option<Vec<String>>,
    pub target_files: Vec<String>,
    pub max_steps: usize,
    pub model: String,
    pub model_type: Option<String>,
    pub goal: Option<SpawnGoal>,
    pub plan: Option<String>,
    pub worktree_mode: SpawnWorktreeMode,
    pub parent_subchat_tx: Option<Arc<AMutex<UnboundedSender<Value>>>>,
    pub parent_worktree: Option<WorktreeMeta>,
    pub parent_task_meta: Option<TaskMeta>,
    pub subchat_depth: usize,
    pub notify_parent: NotifyParent,
    pub completion_push: PushMode,
}

#[derive(Clone)]
pub struct SpawnGoal {
    pub content: String,
    pub criteria: Vec<GoalCriterion>,
    pub budget: Option<GoalBudget>,
}

#[derive(Clone, Default)]
pub enum SpawnWorktreeMode {
    #[default]
    Inherit,
    Isolated {
        auto_merge: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NotifyParent {
    Auto,
    Silent,
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
