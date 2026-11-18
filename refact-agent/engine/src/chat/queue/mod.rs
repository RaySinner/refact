// The command-queue loop, its helpers and the setparams handling all live in
// crates now; this file only re-exports them, so `chat::queue` is a pure facade
// and is not an engine edge for `chat::handlers`, `chat::generation` or
// `chat::task_agent_monitor`.
//
// The import prelude below is what the queue's own tests reach through
// `use super::*`, so it is compiled under `cfg(test)` only.
#![allow(unused_imports)]

mod dispatch;
mod helpers;
mod setparams;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests_common;

pub use dispatch::*;
pub use helpers::*;
pub use setparams::*;

#[cfg(test)]
use std::collections::{BTreeSet, VecDeque};
#[cfg(test)]
use std::pin::Pin;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use tracing::warn;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::buddy::chat_reactions::{maybe_enqueue_chat_reaction, AcceptedUserMessage};
#[cfg(test)]
use crate::call_validation::{ChatContent, ChatMessage, ContextFile};
#[cfg(test)]
use refact_buddy_core::user_action::UserAction;
#[cfg(test)]
use crate::files_correction::get_project_dirs;
#[cfg(test)]
use crate::ext::hooks::HookEvent;
#[cfg(test)]
use crate::ext::hooks_runner::{HookPayload, first_block_reason, get_project_dir_string, run_hooks};
#[cfg(test)]
use crate::chat::internal_roles::{self, mode_switch_event_with_diff, EventSubkind};
#[cfg(test)]
use crate::chat::plan_role;
#[cfg(test)]
use crate::yaml_configs::customization_registry::get_mode_config;
#[cfg(test)]
use crate::yaml_configs::customization_types::ModeConfig;

#[cfg(test)]
use crate::chat::types::*;
#[cfg(test)]
use crate::chat::browser_context;
#[cfg(test)]
use crate::chat::content::parse_content_with_attachments;
#[cfg(test)]
use refact_runtime_api::generation_start::{
    prepare_session_preamble_and_knowledge, start_generation,
};
#[cfg(test)]
use crate::chat::goal_verifier::{
    should_verify_goal_on_done, verify_goal_before_completion, GoalCompletionGateOutcome,
};
#[cfg(test)]
use crate::chat::tools::{
    acquire_session_turn_tool_pool, execute_tools_with_session,
    resolve_tool_call_aliases_with_catalog,
};
#[cfg(test)]
use crate::chat::trajectories::maybe_save_trajectory_background_with_intent;
#[cfg(test)]
use crate::ext::slash_expand::expand_slash_command;
#[cfg(test)]
use crate::ext::skills_context::{expand_skill_includes, SKILLS_CONTEXT_MARKER};
#[cfg(test)]
use crate::worktrees::service::WorktreeService;
#[cfg(test)]
use crate::worktrees::types::{WorktreeMeta, WorktreeReference};
