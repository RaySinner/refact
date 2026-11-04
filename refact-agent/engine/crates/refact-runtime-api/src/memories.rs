use std::path::{Path, PathBuf};
use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::Mutex as AMutex;
use refact_knowledge_graph::kg_structs::KnowledgeFrontmatter;
use crate::{AtCommandsContext, ChatCtx};
use crate::global_context::GlobalContext;
use refact_knowledge_graph::memo::MemoRecord;
use refact_knowledge_graph::memory_types::{EnrichmentParams, MemoryWriteOutcome};

#[async_trait]
pub trait MemoryRuntime: Send + Sync {
    async fn memories_search(
        &self,
        gcx: Arc<GlobalContext>,
        query: &str,
        top_n_memories: usize,
        top_n_trajectories: usize,
        exclude_trajectory_id: Option<&str>,
    ) -> Result<Vec<MemoRecord>, String>;
    async fn memories_add_enriched(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        content: &str,
        params: EnrichmentParams,
    ) -> Result<PathBuf, String>;
    async fn touch_memory_observation_for_ctx(
        &self,
        ctx: &ChatCtx,
        doc_path: &Path,
    ) -> Result<(), String>;
    async fn memories_upsert_by_signal_key_for_ctx(
        &self,
        ctx: &ChatCtx,
        signal_key: &str,
        frontmatter: &KnowledgeFrontmatter,
        content: &str,
    ) -> Result<MemoryWriteOutcome, String>;
    async fn memories_add_dedup_for_ctx(
        &self,
        ctx: &ChatCtx,
        frontmatter: &KnowledgeFrontmatter,
        content: &str,
    ) -> Result<MemoryWriteOutcome, String>;
    async fn rewrite_memory_document_for_ctx(
        &self,
        ctx: &ChatCtx,
        doc_path: &Path,
        frontmatter: &KnowledgeFrontmatter,
        body: &str,
    ) -> Result<(), String>;
}

pub async fn memories_search(
    gcx: Arc<GlobalContext>,
    query: &str,
    top_n_memories: usize,
    top_n_trajectories: usize,
    exclude_trajectory_id: Option<&str>,
) -> Result<Vec<MemoRecord>, String> {
    gcx.memory_runtime
        .clone()
        .memories_search(
            gcx,
            query,
            top_n_memories,
            top_n_trajectories,
            exclude_trajectory_id,
        )
        .await
}

pub async fn memories_add_enriched(
    ccx: Arc<AMutex<AtCommandsContext>>,
    content: &str,
    params: EnrichmentParams,
) -> Result<PathBuf, String> {
    let runtime = ccx.lock().await.app.gcx.memory_runtime.clone();
    runtime.memories_add_enriched(ccx, content, params).await
}

pub async fn touch_memory_observation_for_ctx(
    ctx: &ChatCtx,
    doc_path: &Path,
) -> Result<(), String> {
    ctx.memory_runtime
        .touch_memory_observation_for_ctx(ctx, doc_path)
        .await
}

pub async fn memories_upsert_by_signal_key_for_ctx(
    ctx: &ChatCtx,
    signal_key: &str,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Result<MemoryWriteOutcome, String> {
    ctx.memory_runtime
        .memories_upsert_by_signal_key_for_ctx(ctx, signal_key, frontmatter, content)
        .await
}

pub async fn memories_add_dedup_for_ctx(
    ctx: &ChatCtx,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Result<MemoryWriteOutcome, String> {
    ctx.memory_runtime
        .memories_add_dedup_for_ctx(ctx, frontmatter, content)
        .await
}

pub async fn rewrite_memory_document_for_ctx(
    ctx: &ChatCtx,
    doc_path: &Path,
    frontmatter: &KnowledgeFrontmatter,
    body: &str,
) -> Result<(), String> {
    ctx.memory_runtime
        .rewrite_memory_document_for_ctx(ctx, doc_path, frontmatter, body)
        .await
}

pub fn get_global_knowledge_dir_for_ctx(ctx: &ChatCtx) -> PathBuf {
    ctx.paths.config_dir.join("knowledge")
}

pub async fn update_memory_document_frontmatter_for_ctx<F>(
    ctx: &ChatCtx,
    doc_path: &Path,
    update: F,
) -> Result<bool, String>
where
    F: FnOnce(&mut KnowledgeFrontmatter) -> Result<bool, String>,
{
    let existing_text = tokio::fs::read_to_string(doc_path)
        .await
        .map_err(|e| format!("Failed to read memory file: {}", e))?;
    let (mut frontmatter, content_start) = KnowledgeFrontmatter::parse(&existing_text);
    if content_start == 0 && existing_text.starts_with("---") {
        return Err("Failed to parse memory frontmatter".to_string());
    }
    let body = existing_text.get(content_start..).unwrap_or("").to_string();
    if !update(&mut frontmatter)? {
        return Ok(false);
    }
    rewrite_memory_document_for_ctx(ctx, doc_path, &frontmatter, &body)
        .await
        .map(|_| true)
}
