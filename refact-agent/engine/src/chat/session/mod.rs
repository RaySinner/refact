use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use refact_runtime_api::SessionsHandle;
use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

use super::types::*;

#[cfg(test)]
use std::collections::{HashSet, VecDeque};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::Mutex as StdMutex;
#[cfg(test)]
use std::time::Instant;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use tokio::sync::{broadcast, Notify};
#[cfg(test)]
use tracing::{info, warn};
#[cfg(test)]
use uuid::Uuid;
#[cfg(test)]
use crate::call_validation::{ChatContent, ChatMessage, ChatUsage};
#[cfg(test)]
use crate::chat::diagnostics::{make_ui_only_error_message, make_ui_only_error_message_with_info};
#[cfg(test)]
use crate::chat::internal_roles::{event, EventSubkind, EVENT_ROLE, GOAL_ROLE, PLAN_ROLE};
#[cfg(test)]
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};
#[cfg(test)]
use crate::tools::tools_description::WaitInterruptCause;
#[cfg(test)]
use super::config::limits;
#[cfg(test)]
use super::trajectories::{task_context_from_task_meta, trajectory_meta_title, TrajectoryEvent};

#[cfg(test)]
mod tests_goal;
#[cfg(test)]
mod tests_messages;
#[cfg(test)]
mod tests_misc;
#[cfg(test)]
mod tests_queue;
#[cfg(test)]
mod tests_runtime;
#[cfg(test)]
mod tests_snapshot;
#[cfg(test)]
mod tests_stream;
#[cfg(test)]
mod tests_support;
#[cfg(test)]
mod tests_tool_decisions;

pub use super::session_runtime::{
    clean_background_processes_for_chat, close_all_chat_sessions,
    get_or_create_session_with_trajectory, snapshot_with_agents, start_session_cleanup_task,
    try_restore_session_if_trajectory_exists,
};

pub(crate) use refact_chat_api::chat_local_types::has_displayable_assistant_content;
pub(crate) use refact_chat_api::session::{
    goal_snapshot_from_messages, should_replace_background_agent,
};
#[cfg(test)]
pub(crate) use refact_chat_api::session::{
    apply_goal_runtime_projection, background_process_cleanup_notice, epoch_ms_now,
    goal_budget_has_hard_limits, goal_event_subkind, goal_events_from_messages,
    goal_runtime_projection, is_active_compression_phase, is_background_agent_terminal,
    is_goal_projection_event, is_terminal_runtime_state, message_affects_goal_projection,
    sse_broadcast_outcome, stamp_created_at_ms, synthesized_goal_content, tool_decision_message,
    tool_result_counts_as_goal_evidence, StreamStartRefusal, ToolDecisionOutcome,
};

pub use refact_chat_api::types::SessionsMap;

pub struct EngineSessionsHandle(pub SessionsMap);

#[async_trait]
impl SessionsHandle for EngineSessionsHandle {
    fn as_any(&self) -> &dyn Any {
        &self.0
    }

    async fn session_ids(&self) -> Vec<String> {
        self.0.read().await.keys().cloned().collect()
    }

    async fn session_state(&self, chat_id: &str) -> Option<SessionState> {
        let session_arc = self.0.read().await.get(chat_id).cloned()?;
        let session = session_arc.lock().await;
        Some(session.runtime.state)
    }
}

pub fn sessions_map_from_handle(handle: &std::sync::Arc<dyn SessionsHandle>) -> SessionsMap {
    handle
        .as_any()
        .downcast_ref::<SessionsMap>()
        .expect("engine sessions handle wraps SessionsMap")
        .clone()
}

pub fn create_sessions_map() -> SessionsMap {
    Arc::new(ARwLock::new(HashMap::new()))
}
