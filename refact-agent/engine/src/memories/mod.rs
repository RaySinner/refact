use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex as AMutex;

use chrono::{Duration, Local, Utc};
use refact_core::knowledge_frontmatter::KnowledgeFrontmatter;
use refact_knowledge_graph::memory_types::EnrichmentParams;
use tracing::{info, warn};
use uuid::Uuid;

use crate::at_commands::at_commands::AtCommandsContext;
use crate::chat::ctx::ChatCtx;
use crate::global_context::{GlobalContext, GlobalContextCtx, SharedGlobalContext};
use crate::http::routers::v1::knowledge_ops::auto_link_memory;
use crate::knowledge_graph::build_knowledge_graph;
use crate::knowledge_graph::kg_subchat::{check_deprecation, enrich_knowledge_metadata};

pub mod format;
pub mod index;
pub mod search;
pub mod storage;

pub use format::*;
pub use index::{enrichment_current_root_id, enrichment_root_id};
pub use refact_knowledge_graph::memo::MemoRecord;
pub use refact_knowledge_graph::memory_types::MemoryWriteOutcome;
pub use refact_runtime_api::memories::{
    get_global_knowledge_dir_for_ctx, update_memory_document_frontmatter_for_ctx,
};
pub use search::{memories_search, memories_search_for_enrichment};
pub use storage::*;

pub async fn update_memory_document_frontmatter<F>(
    gcx: &SharedGlobalContext,
    doc_path: &Path,
    update: F,
) -> Result<bool, String>
where
    F: FnOnce(&mut KnowledgeFrontmatter) -> Result<bool, String>,
{
    update_memory_document_frontmatter_for_ctx(&gcx.chat_ctx(gcx.clone()), doc_path, update).await
}

pub async fn memories_add_enriched(
    ccx: Arc<AMutex<AtCommandsContext>>,
    content: &str,
    params: EnrichmentParams,
) -> Result<PathBuf, String> {
    let gcx = ccx.lock().await.app.gcx.clone();

    let exact_duplicate = {
        let signature = refact_buddy_core::memory_dedup::content_signature(content);
        let idx = gcx.knowledge_index.lock().await;
        idx.first_path_for_signature(&signature).cloned()
    };
    if let Some(existing) = exact_duplicate {
        info!(
            "memories: enriched save is an exact duplicate of {}, touching instead",
            existing.display()
        );
        match touch_memory_observation(&gcx, &existing).await {
            Ok(()) => return Ok(existing),
            Err(err) => {
                warn!(
                    "memories: failed to touch enriched duplicate {}: {}, saving normally",
                    existing.display(),
                    err
                );
                evict_stale_index_path(&gcx, &existing).await;
            }
        }
    }

    let entities = extract_entities(content);
    let detected_paths = extract_file_paths(content);

    let kg = build_knowledge_graph(gcx.clone()).await;

    let candidate_files: Vec<String> = {
        let mut files = params.base_filenames.clone();
        files.extend(detected_paths.clone());
        files.into_iter().take(30).collect()
    };

    let candidate_docs: Vec<(String, String)> = kg
        .active_docs()
        .take(20)
        .map(|d| {
            let id = d
                .frontmatter
                .id
                .clone()
                .unwrap_or_else(|| d.path.to_string_lossy().to_string());
            let title = d
                .frontmatter
                .title
                .clone()
                .unwrap_or_else(|| "Untitled".to_string());
            (id, title)
        })
        .collect();

    let enrichment = enrich_knowledge_metadata(
        gcx.clone(),
        content,
        &entities,
        &candidate_files,
        &candidate_docs,
        params.source_chat_id.as_deref(),
    )
    .await;

    let (final_title, final_tags, final_filenames, final_kind, final_links, review_days) =
        match enrichment {
            Ok(e) => {
                let mut tags = params.base_tags.clone();
                tags.extend(e.tags);
                tags.extend(extract_fallback_tags(content, &detected_paths, &entities));
                let tags = normalize_memory_tags(&tags, 16);

                let mut files = params.base_filenames.clone();
                files.extend(e.filenames);
                files.sort();
                files.dedup();

                let kind = e.kind.unwrap_or_else(|| params.base_kind.clone());

                (
                    e.title.or(params.base_title.clone()).or_else(|| {
                        content
                            .lines()
                            .next()
                            .map(|l| l.trim_start_matches('#').trim().to_string())
                    }),
                    if tags.is_empty() {
                        let mut fallback = vec![params.base_kind.clone()];
                        fallback.extend(extract_fallback_tags(content, &detected_paths, &entities));
                        normalize_memory_tags(&fallback, 16)
                    } else {
                        tags
                    },
                    files,
                    kind,
                    e.links,
                    e.review_after_days.unwrap_or(90),
                )
            }
            Err(e) => {
                warn!(
                    "Enrichment failed, using defaults with fallback tags: {}",
                    e
                );
                let mut tags = params.base_tags.clone();

                let fallback_tags = extract_fallback_tags(content, &detected_paths, &entities);
                tags.extend(fallback_tags);

                if tags.is_empty() {
                    tags.push(params.base_kind.clone());
                }

                let tags = normalize_memory_tags(&tags, 16);

                (
                    params.base_title.or_else(|| {
                        content
                            .lines()
                            .next()
                            .map(|l| l.trim_start_matches('#').trim().to_string())
                    }),
                    tags,
                    params.base_filenames,
                    params.base_kind,
                    vec![],
                    90,
                )
            }
        };

    let now = Local::now();
    let content_hash = compute_content_hash_hex(content);
    let summary = first_nonempty_line(content);

    // Optional “short description”: prefer second non-empty line if present,
    // otherwise fall back to summary.
    let description = {
        let nonempty: Vec<&str> = content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if nonempty.len() >= 2 {
            Some(nonempty[1].trim_start_matches('#').trim().to_string())
        } else {
            summary.clone()
        }
    };

    // “Related” fields are intended for fast, cheap retrieval.
    // For now we populate them from detected paths/entities as best-effort.
    let related_files: Vec<String> = {
        let mut files = detected_paths.clone();
        files.sort();
        files.dedup();
        files.into_iter().take(50).collect()
    };
    let related_entities: Vec<String> = {
        let mut ents = entities.clone();
        ents.sort();
        ents.dedup();
        ents.into_iter().take(50).collect()
    };
    let frontmatter = KnowledgeFrontmatter {
        id: Some(Uuid::new_v4().to_string()),
        title: final_title.clone(),
        tags: final_tags.clone(),
        created: Some(now.format("%Y-%m-%d").to_string()),
        updated: Some(now.format("%Y-%m-%d").to_string()),
        filenames: final_filenames.clone(),
        links: final_links,
        kind: Some(final_kind),
        status: Some("active".to_string()),
        superseded_by: None,
        deprecated_at: None,
        review_after: Some(
            (now + Duration::days(review_days))
                .format("%Y-%m-%d")
                .to_string(),
        ),
        source_chat_id: params.source_chat_id.clone(),

        created_at: Some(Utc::now().to_rfc3339()),
        summary,
        description,
        entities: entities.clone(),
        related_files,
        related_entities,
        content_hash: Some(content_hash),
        source_tool: Some("memories_add_enriched".to_string()),
        source_confidence: None,
        source_trajectory_id: None,
        source_message_range: None,
        source_commit: None,
        topic: None,
        last_used_at: None,
        use_count: 0,
        last_injected_at: None,
        dismissed_count: 0,
        source_id: None,
        source_content_hash: None,
        review_needed: None,
        occurrences: 0,
        signal_key: None,
        last_observed: None,
        extra: Default::default(),
    };

    let outcome = memories_add_dedup(&gcx, &frontmatter, content).await?;
    if !outcome.created() {
        return Ok(outcome.path().clone());
    }
    let file_path = outcome.path().clone();
    let new_doc_id = frontmatter.id.clone().unwrap();

    let mut updated_frontmatter = frontmatter.clone();
    if let Err(e) = auto_link_memory(gcx.clone(), &mut updated_frontmatter, content, &file_path).await
    {
        warn!("Auto-linking failed for new memory: {}", e);
    } else if updated_frontmatter.links != frontmatter.links {
        if let Err(e) =
            rewrite_memory_document(&gcx, &file_path, &updated_frontmatter, content).await
        {
            warn!("Failed to update memory with auto-links: {}", e);
        } else {
            info!(
                "Auto-linked {} docs to {}",
                updated_frontmatter.links.len(),
                file_path.display()
            );
        }
    }

    let deprecation_candidates =
        kg.get_deprecation_candidates(&final_tags, &final_filenames, &entities, Some(&new_doc_id));

    if !deprecation_candidates.is_empty() {
        let snippet: String = content.chars().take(500).collect();

        match check_deprecation(
            gcx.clone(),
            final_title.as_deref().unwrap_or("Untitled"),
            &final_tags,
            &final_filenames,
            &snippet,
            &deprecation_candidates,
            params.source_chat_id.as_deref(),
        )
        .await
        {
            Ok(result) => {
                for decision in result.deprecate {
                    if decision.confidence >= 0.75 {
                        if let Some(doc) = kg.get_doc_by_id(&decision.target_id) {
                            if let Err(e) = deprecate_document(
                                &gcx,
                                &doc.path,
                                Some(&new_doc_id),
                                &decision.reason,
                            )
                            .await
                            {
                                warn!("Failed to deprecate {}: {}", decision.target_id, e);
                            } else {
                                info!(
                                    "Deprecated {} (confidence: {:.2}): {}",
                                    decision.target_id, decision.confidence, decision.reason
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Deprecation check failed: {}", e);
            }
        }
    }

    Ok(file_path)
}

pub struct EngineMemoryRuntime;

#[async_trait::async_trait]
impl refact_runtime_api::memories::MemoryRuntime for EngineMemoryRuntime {
    async fn memories_search(
        &self,
        gcx: Arc<GlobalContext>,
        query: &str,
        top_n_memories: usize,
        top_n_trajectories: usize,
        exclude_trajectory_id: Option<&str>,
    ) -> Result<Vec<MemoRecord>, String> {
        memories_search(
            gcx,
            query,
            top_n_memories,
            top_n_trajectories,
            exclude_trajectory_id,
        )
        .await
    }
    async fn memories_add_enriched(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        content: &str,
        params: EnrichmentParams,
    ) -> Result<PathBuf, String> {
        memories_add_enriched(ccx, content, params).await
    }
    async fn touch_memory_observation_for_ctx(
        &self,
        ctx: &ChatCtx,
        doc_path: &Path,
    ) -> Result<(), String> {
        touch_memory_observation_for_ctx(ctx, doc_path).await
    }
    async fn memories_upsert_by_signal_key_for_ctx(
        &self,
        ctx: &ChatCtx,
        signal_key: &str,
        frontmatter: &KnowledgeFrontmatter,
        content: &str,
    ) -> Result<MemoryWriteOutcome, String> {
        memories_upsert_by_signal_key_for_ctx(ctx, signal_key, frontmatter, content).await
    }
    async fn memories_add_dedup_for_ctx(
        &self,
        ctx: &ChatCtx,
        frontmatter: &KnowledgeFrontmatter,
        content: &str,
    ) -> Result<MemoryWriteOutcome, String> {
        memories_add_dedup_for_ctx(ctx, frontmatter, content).await
    }
    async fn rewrite_memory_document_for_ctx(
        &self,
        ctx: &ChatCtx,
        doc_path: &Path,
        frontmatter: &KnowledgeFrontmatter,
        body: &str,
    ) -> Result<(), String> {
        rewrite_memory_document_for_ctx(ctx, doc_path, frontmatter, body).await
    }

    async fn enrichment_current_root_id(
        &self,
        gcx: Arc<GlobalContext>,
        current_chat_id: Option<&str>,
    ) -> Option<String> {
        enrichment_current_root_id(gcx, current_chat_id).await
    }

    async fn memories_search_for_enrichment(
        &self,
        gcx: Arc<GlobalContext>,
        query: &str,
        top_n_memories: usize,
        top_n_trajectories: usize,
        exclude_trajectory_id: Option<&str>,
        diagnostic_chat_id: Option<&str>,
    ) -> Result<Vec<MemoRecord>, String> {
        memories_search_for_enrichment(
            gcx,
            query,
            top_n_memories,
            top_n_trajectories,
            exclude_trajectory_id,
            diagnostic_chat_id,
        )
        .await
    }
}
