//! Subchat runtime that no longer needs the engine: privacy gating, retry
//! diagnostics, stateful-session plumbing, trajectory progress persistence and
//! subchat config resolution. The engine's `subchat` module re-exports these
//! names, and the pieces that still need the engine island (the subchat run loop,
//! `refact_tools::chat_prepare`, `refact_tools::chat_tools`) stay in
//! `refact-lsp::subchat`.

pub mod config;
pub mod diagnostics;
pub mod privacy;
pub mod session;
pub mod state;

pub use config::{
    resolve_subagent_confirmation_defaults, resolve_subchat_config,
    resolve_subchat_config_with_explicit_params, resolve_subchat_config_with_parent,
    run_subchat_once_with_explicit_params,
};

pub const MAX_CONTEXT_LIMIT_COMPACT_ATTEMPTS: usize = 1;
pub const MAX_EMPTY_CHOICE_RETRIES: usize = 2;
pub const EMPTY_CHOICE_ERROR_PREFIX: &str = "subchat produced no visible content";
pub const PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS: usize = 2_000;
pub const PARENT_COMPACTION_DIAGNOSTIC_REDACTION_LOOKAHEAD_CHARS: usize = 512;
pub const PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED: &str = "\n...[truncated]";
pub const PARTIAL_OUTPUT_STREAM_ERROR: &str =
    "Stream interrupted after partial output and all retry attempts failed.";
pub const GUARDED_REPORT_INSTRUCTION: &str = "Guarded file contents may be used to complete the task, but do not quote or reproduce them verbatim in the final report.";

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use refact_chat_api::SubchatConfig;

pub fn subchat_retries_allowed(config: &SubchatConfig) -> bool {
    config.tool_name != "mode_transition"
}

pub fn is_aborted(abort_flag: &Option<Arc<AtomicBool>>) -> bool {
    abort_flag
        .as_ref()
        .map(|f| f.load(Ordering::SeqCst))
        .unwrap_or(false)
}
