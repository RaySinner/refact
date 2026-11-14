//! Memory-lifecycle `_for_ctx` seam. `refact-tools` needs the near-duplicate,
//! source-lookup and archive helpers, whose bodies live in the engine because
//! they reach the knowledge-directory scan and frontmatter rewrite machinery.
//! They depend only on `ChatCtx` services, so the crate owns the paths and the
//! engine installs the implementation (same pattern as `MemoryRuntime` and
//! `TaskStorageRuntime`).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use refact_buddy_core::memory_lifecycle_model::{MemoryLifecycleOp, MemorySource};

use crate::chat_ctx::ChatCtx;

#[async_trait]
pub trait MemoryLifecycleRuntime: Send + Sync {
    async fn propose_supersede_for_near_duplicate_for_ctx(
        &self,
        ctx: &ChatCtx,
        new_path: &Path,
    ) -> Option<MemoryLifecycleOp>;

    async fn find_memory_by_source_for_ctx(
        &self,
        ctx: &ChatCtx,
        source: MemorySource,
        source_id: Option<&str>,
        source_content_hash: Option<&str>,
    ) -> Option<PathBuf>;

    async fn archive_memory_file_checked_for_ctx(
        &self,
        ctx: &ChatCtx,
        path: &Path,
        superseded_by: Option<&str>,
    ) -> Result<bool, String>;
}

pub async fn propose_supersede_for_near_duplicate_for_ctx(
    ctx: &ChatCtx,
    new_path: &Path,
) -> Option<MemoryLifecycleOp> {
    ctx.memory_lifecycle
        .propose_supersede_for_near_duplicate_for_ctx(ctx, new_path)
        .await
}

pub async fn find_memory_by_source_for_ctx(
    ctx: &ChatCtx,
    source: MemorySource,
    source_id: Option<&str>,
    source_content_hash: Option<&str>,
) -> Option<PathBuf> {
    ctx.memory_lifecycle
        .find_memory_by_source_for_ctx(ctx, source, source_id, source_content_hash)
        .await
}

pub async fn archive_memory_file_checked_for_ctx(
    ctx: &ChatCtx,
    path: &Path,
    superseded_by: Option<&str>,
) -> Result<bool, String> {
    ctx.memory_lifecycle
        .archive_memory_file_checked_for_ctx(ctx, path, superseded_by)
        .await
}
