// The completion gate moved to `refact_runtime_api::goal_verifier_monitor`; the
// engine keeps the LLM half (`monitor.rs`) and the test surface. `monitor` and
// `tests` resolve names through `use super::*`, so the prelude the original
// single file carried is kept here verbatim.
#![allow(unused_imports)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex as AMutex;
use uuid::Uuid;

use crate::app_state::AppState;

use crate::at_commands::at_commands::AtCommandsContext;

pub use refact_chat_api::goal_verifier::{
    abort_goal_verification, apply_goal_budget_exhausted_terminal, apply_goal_verdict,
    apply_goal_verdict_guarded, apply_goal_verdict_guarded_with_delivery,
    begin_goal_verification_if_needed, goal_text_for_verifier, goal_verification_message,
    handle_verifier_failure, handle_verifier_failure_with_delivery, parse_criteria_verdicts,
    parse_goal_verdict, should_verify_goal_on_done, verifier_cancel_requested,
    verifier_sampling_parameters, GoalCompletionGateOutcome, GoalVerificationApplyOutcome,
    GoalVerificationBegin, GoalVerdict, GoalVerifierReply, GOAL_VERIFIER_BLOCKED_BACKOFF_MS,
    GOAL_VERIFIER_MAX_TOKENS, GOAL_VERIFIER_SOURCE, INCONCLUSIVE_GAP,
};

use crate::call_validation::{ChatMessage, ChatMeta, ChatUsage, SamplingParameters};

use crate::chat::prepare::{prepare_chat_passthrough, ChatPrepareOptions, ToolChoice};

use crate::chat::stream_core::{
    run_llm_stream, ChoiceFinal, LlmStreamOutcome, StreamCollector, StreamRunParams,
    ABORT_ERROR_MESSAGE,
};

use crate::chat::types::*;

use crate::constants::CHAT_TOP_N;

use crate::llm::params::CacheControl;

use crate::scratchpad_abstract::HasTokenizerAndEot;

use refact_tool_api::ToolDesc;

pub use refact_runtime_api::goal_verifier_monitor::verify_goal_before_completion;

mod monitor;
#[allow(unused_imports)]
pub use monitor::*;

#[cfg(test)]
mod tests;
