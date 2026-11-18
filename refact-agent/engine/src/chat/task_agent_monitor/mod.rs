// Task agent failure detection and automatic cleanup now lives in
// `refact_tools::chat_task_agent_monitor`; this module re-exports it so
// `crate::chat::task_agent_monitor::*` keeps resolving for the engine.
//
// The tests stay in the engine: they build a real `AppState` from
// `make_test_gcx`, which only the engine can construct. The prelude below is what
// they reach through `use super::*`; the crate's own imports are private to it.
pub use refact_tools::chat_task_agent_monitor::*;

pub(crate) mod broadcast;
pub(crate) mod finish;
pub(crate) mod monitor;

#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::time::Duration;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::Utc;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::json;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::time::sleep;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use uuid::Uuid;
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::types::BuddyRuntimeEvent;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::agent_notifications::ChatAgentHost;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::{event, EventSubkind};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::types::{ChatSession, SessionState, TaskMeta};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{DeliveryOutcome, PendingDelivery, PushMode};
#[cfg(test)]
#[allow(unused_imports)]
use crate::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use crate::call_validation::{ChatMessage, ChatUsage};
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::ctx::ChatCtx;
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::get_or_create_session_with_trajectory;
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::retry_policy::{
    classify_llm_error_for_retry, classify_user_error, user_error_info, RetryDecision,
    UserErrorCategory,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tasks::storage;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tasks::types::{BoardCard, StatusUpdate};
#[cfg(test)]
#[allow(unused_imports)]
use crate::worktrees::git;
#[cfg(test)]
#[allow(unused_imports)]
use crate::worktrees::service::WorktreeService;

#[cfg(test)]
mod tests;
