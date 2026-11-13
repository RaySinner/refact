//! One owner for model-based context reconstruction. Raw history is append-only.
//!
//! The crate-typed surface (compression phases, the attempt guard, the manual
//! rebuild preview/apply shapes, the gate cap resolver) now lives in
//! `refact_chat_api::context_rebuild`; the gcx-bound pipeline stays local in the
//! `pipeline` submodule. Everything below is a shim, so
//! `crate::chat::context_rebuild::<symbol>` keeps resolving for its callers.
mod pipeline;

pub use refact_chat_api::context_rebuild::{
    resolve_effective_compression_cap, CompactionOutcome, ManualCompressionApplyResult,
    ManualCompressionPreview,
};

/// An attempt older than this is treated as abandoned: its reservation can no
/// longer be trusted to reach a terminal phase, so it must not wedge generation,
/// the command queue, or the quiet compression gate forever.
///
/// Also the point at which a user waiting on that attempt deserves to be told
/// something is wrong, so `queue.rs` reuses this exact threshold instead of
/// inventing a second one.
pub use refact_chat_api::compression_gate::{
    compression_attempt_active, COMPRESSION_ATTEMPT_STALE_AFTER,
};

pub use refact_chat_api::pending_context_rebuild::{request_rebuild, PendingContextRebuild};

pub use pipeline::{
    apply_context_rebuild_with_reason, apply_deterministic_compaction_for_recovery,
    apply_manual_context_rebuild, drain_pending, preview_manual_context_rebuild, rebuild_session,
};
