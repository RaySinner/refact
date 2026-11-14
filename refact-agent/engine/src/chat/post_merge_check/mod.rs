//! Engine-side shim for the post-merge check workflow. The workflow bodies live in
//! `refact_runtime_api::post_merge_check`; the verification executor and the shell
//! policy reach them through the `CardVerifier` seam.

pub use refact_runtime_api::post_merge_check::*;

#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use {
    crate::global_context::GlobalContext,
    crate::tasks::storage,
    async_trait::async_trait,
    chrono::Utc,
    refact_runtime_api::GlobalContextCtx,
    refact_tasks::types::VerificationOutcome,
    std::path::{Path, PathBuf},
    std::sync::Arc,
    std::time::Duration,
    tokio::process::Command,
};

#[cfg(test)]
mod tests;
