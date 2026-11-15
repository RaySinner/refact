//! `AppState`-shaped memory-op drivers.
//!
//! The three functions below only glue `refact-buddy-core`'s closure-taking
//! drivers to the engine-provided `MemoryLifecycleRuntime` reachable from
//! `ChatCtx`, so they live here instead of in the engine buddy module.

use std::path::Path;

use refact_buddy_core::memory_lifecycle_model::MemoryOpsState;
use refact_buddy_core::settings::AutonomyLevel;
use refact_buddy_core::storage::{
    apply_artifact_decisions_with, apply_memory_batch_with, drain_memory_ops_with,
};

use crate::app_state::AppState;
use crate::memory_lifecycle_runtime::apply_memory_lifecycle_op_status_for_ctx;

pub async fn drain_memory_ops(
    project_root: &Path,
    gcx: AppState,
    autonomy: AutonomyLevel,
    max_applies: usize,
) -> Result<(MemoryOpsState, usize), String> {
    drain_memory_ops_with(project_root, autonomy, max_applies, |working| {
        let ctx = gcx.chat_ctx();
        async move { apply_memory_lifecycle_op_status_for_ctx(&ctx, &working).await }
    })
    .await
}

pub async fn apply_memory_batch(
    project_root: &Path,
    gcx: AppState,
    batch_key: &str,
    max_applies: usize,
) -> Result<(MemoryOpsState, usize, usize, usize), String> {
    apply_memory_batch_with(project_root, batch_key, max_applies, |working| {
        let ctx = gcx.chat_ctx();
        async move { apply_memory_lifecycle_op_status_for_ctx(&ctx, &working).await }
    })
    .await
}

#[cfg_attr(not(test), allow(dead_code))]
pub async fn apply_artifact_decisions(
    project_root: &Path,
    gcx: AppState,
    decisions: &[(String, bool)],
) -> Result<(MemoryOpsState, usize, usize), String> {
    apply_artifact_decisions_with(project_root, decisions, |working| {
        let ctx = gcx.chat_ctx();
        async move { apply_memory_lifecycle_op_status_for_ctx(&ctx, &working).await }
    })
    .await
}
