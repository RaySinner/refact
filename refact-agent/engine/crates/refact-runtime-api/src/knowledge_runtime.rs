use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tracing::{info, warn};

use refact_core::chat_types::{ChatContent, ChatMessage};
use refact_core::knowledge_frontmatter::KnowledgeFrontmatter;
use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
use refact_files::path_utils::canonicalize_normalized_path;
use refact_knowledge_graph::enrichment::{
    build_bounded_enrichment_context_files, context_message_stats, estimate_tokens,
    get_existing_context_file_paths, has_knowledge_enrichment_near, normalize_query,
    should_enrich, source_fingerprint, EnrichmentDecision, FORCED_KNOWLEDGE_SCORE_THRESHOLD,
    KNOWLEDGE_ENRICHMENT_MARKER, KNOWLEDGE_SCORE_THRESHOLD,
};
use refact_knowledge_graph::enrichment_cache::{
    fingerprint_bytes, fingerprint_hex, fingerprint_strings, CachedEnrichmentResult, CachedMemo,
    EnrichmentCacheDisposition, EnrichmentCacheKey,
};
use refact_knowledge_graph::memo::MemoRecord;
use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};

use crate::files_correction::get_project_dirs;
use crate::files_in_workspace::get_file_text_from_memory_or_disk;
use crate::global_context::{GlobalContext, SharedGlobalContext};

pub async fn enrich_messages_with_knowledge(
    gcx: Arc<GlobalContext>,
    messages: &mut Vec<ChatMessage>,
    current_chat_id: Option<&str>,
    force_enrichment: bool,
) {
    let last_user_idx = match messages.iter().rposition(|m| m.role == "user") {
        Some(idx) => idx,
        None => {
            record_enrichment(
                current_chat_id,
                PerfComponent::EnrichmentSkipNoUser,
                PerfOutcome::Skipped,
                0,
                None,
                Some(messages.len() as u64),
                None,
            );
            return;
        }
    };
    let query_raw = messages[last_user_idx].content.content_text_only();

    if has_knowledge_enrichment_near(messages, last_user_idx) {
        record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentSkipAlreadyPresent,
            PerfOutcome::Skipped,
            0,
            None,
            Some(messages.len() as u64),
            None,
        );
        return;
    }

    let normalize_started = perf_diagnostics::is_enabled().then(Instant::now);
    let query_normalized = normalize_query(&query_raw);
    record_enrichment(
        current_chat_id,
        PerfComponent::EnrichmentQueryNormalize,
        PerfOutcome::Success,
        elapsed_us(normalize_started),
        Some(query_normalized.len() as u64),
        Some(1),
        Some(estimate_tokens(&query_normalized)),
    );

    let decision = should_enrich(messages, &query_raw, &query_normalized, force_enrichment);
    match decision {
        EnrichmentDecision::FirstUser => record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentDecisionFirstUser,
            PerfOutcome::Success,
            0,
            Some(query_normalized.len() as u64),
            Some(1),
            Some(estimate_tokens(&query_normalized)),
        ),
        EnrichmentDecision::Forced => record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentDecisionForced,
            PerfOutcome::Success,
            0,
            Some(query_normalized.len() as u64),
            Some(1),
            Some(estimate_tokens(&query_normalized)),
        ),
        EnrichmentDecision::Signaled => record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentDecisionSignaled,
            PerfOutcome::Success,
            0,
            Some(query_normalized.len() as u64),
            Some(1),
            Some(estimate_tokens(&query_normalized)),
        ),
        EnrichmentDecision::SkipEmpty => record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentSkipEmptyQuery,
            PerfOutcome::Skipped,
            0,
            Some(query_normalized.len() as u64),
            Some(1),
            None,
        ),
        EnrichmentDecision::SkipCommand => record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentSkipCommand,
            PerfOutcome::Skipped,
            0,
            Some(query_normalized.len() as u64),
            Some(1),
            None,
        ),
        EnrichmentDecision::SkipThreshold => record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentSkipThreshold,
            PerfOutcome::Skipped,
            0,
            Some(query_normalized.len() as u64),
            Some(1),
            Some(estimate_tokens(&query_normalized)),
        ),
    }
    if !decision.should_enrich() {
        return;
    }

    record_enrichment(
        current_chat_id,
        PerfComponent::EnrichmentAttempt,
        PerfOutcome::Success,
        0,
        Some(query_normalized.len() as u64),
        Some(1),
        Some(estimate_tokens(&query_normalized)),
    );
    let context_scan_started = perf_diagnostics::is_enabled().then(Instant::now);
    let existing_paths = get_existing_context_file_paths(messages);
    record_enrichment(
        current_chat_id,
        PerfComponent::EnrichmentExistingContextScan,
        PerfOutcome::Success,
        elapsed_us(context_scan_started),
        None,
        Some(existing_paths.len() as u64),
        None,
    );

    let score_threshold = if force_enrichment {
        FORCED_KNOWLEDGE_SCORE_THRESHOLD
    } else {
        KNOWLEDGE_SCORE_THRESHOLD
    };

    if let Some(knowledge_context) = create_knowledge_context(
        gcx,
        &query_normalized,
        &existing_paths,
        current_chat_id,
        score_threshold,
    )
    .await
    {
        messages.insert(last_user_idx, knowledge_context);
        if perf_diagnostics::is_enabled() {
            let (file_count, char_count, estimated_tokens) =
                context_message_stats(&messages[last_user_idx]);
            record_enrichment(
                current_chat_id,
                PerfComponent::EnrichmentInsertion,
                PerfOutcome::Success,
                0,
                Some(char_count),
                Some(file_count),
                Some(estimated_tokens),
            );
        }
        info!(
            "Injected knowledge context before user message at position {}",
            last_user_idx
        );
    } else {
        record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentInsertion,
            PerfOutcome::Skipped,
            0,
            None,
            Some(0),
            Some(0),
        );
    }
}

pub async fn enrichment_cache_key(
    gcx: SharedGlobalContext,
    query_text: &str,
    current_chat_id: Option<&str>,
    score_threshold: f32,
) -> EnrichmentCacheKey {
    let settings = crate::runtime_settings::current();
    crate::privacy::load_privacy_if_needed(gcx.clone()).await;
    let project_dirs = get_project_dirs(gcx.clone()).await;
    let mut roots = project_dirs
        .iter()
        .map(|path| canonicalize_normalized_path(path.clone()))
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    let allowed_roots_fingerprint = fingerprint_strings(
        roots
            .iter()
            .map(|root| {
                root.join(KNOWLEDGE_FOLDER_NAME)
                    .to_string_lossy()
                    .into_owned()
            })
            .chain(std::iter::once(
                gcx.config_dir
                    .join("knowledge")
                    .to_string_lossy()
                    .into_owned(),
            )),
    );
    let workspace_scope_fingerprint = fingerprint_strings(
        roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .chain(std::iter::once(
                gcx.config_dir.to_string_lossy().into_owned(),
            )),
    );
    let current_root = gcx
        .memory_runtime
        .clone()
        .enrichment_current_root_id(gcx.clone(), current_chat_id)
        .await;
    let current_root_fingerprint =
        fingerprint_bytes(&[current_root.as_deref().unwrap_or_default().as_bytes()]);
    let (embedding_config_fingerprint, index_generation) = {
        let vecdb = gcx.vec_db.lock().await.clone();
        match vecdb {
            Some(vecdb) => {
                let (config, splitter_window_size) = vecdb.current_constants();
                let backend_identity = format!("{:p}", Arc::as_ptr(&vecdb));
                let model = fingerprint_strings([
                    backend_identity,
                    config.model_id,
                    config.endpoint,
                    config.endpoint_style,
                    config.embedding_endpoint_style,
                    config.model_name,
                    fingerprint_hex(&fingerprint_bytes(&[config.api_key.as_bytes()])),
                    config.embedding_size.to_string(),
                    config.dimensions.unwrap_or_default().to_string(),
                    config.query_prefix,
                    config.document_prefix,
                    config.rejection_threshold.to_bits().to_string(),
                    config.embedding_batch.to_string(),
                    config.n_ctx.to_string(),
                    splitter_window_size.to_string(),
                ]);
                (
                    model,
                    gcx.enrichment_generation
                        .load(std::sync::atomic::Ordering::Acquire),
                )
            }
            None => (
                fingerprint_bytes(&[b"fallback-enrichment-search"]),
                gcx.enrichment_generation
                    .load(std::sync::atomic::Ordering::Acquire),
            ),
        }
    };
    EnrichmentCacheKey {
        query_fingerprint: fingerprint_bytes(&[query_text.as_bytes()]),
        workspace_scope_fingerprint,
        allowed_roots_fingerprint,
        privacy_generation: gcx
            .tool_catalog_generations
            .privacy
            .load(std::sync::atomic::Ordering::Acquire),
        index_generation,
        embedding_config_fingerprint,
        current_root_fingerprint,
        top_n_memories: settings.auto_enrichment_knowledge_top_n,
        top_n_trajectories: settings.auto_enrichment_trajectory_top_n,
        score_threshold_bits: score_threshold.to_bits(),
    }
}

pub async fn cache_result_from_memories(
    gcx: SharedGlobalContext,
    memories: Vec<MemoRecord>,
) -> CachedEnrichmentResult {
    if memories.is_empty() {
        return CachedEnrichmentResult::empty();
    }
    let mut cached = Vec::with_capacity(memories.len());
    let mut bytes = 0usize;
    for mut memo in memories {
        if memo.kind.as_deref() == Some("trajectory") {
            return CachedEnrichmentResult {
                memories: Vec::new(),
                result_fingerprint: fingerprint_bytes(&[b"uncacheable-enrichment-result"]),
                bytes: 0,
                cacheable: false,
            };
        }
        let Some(path) = memo.file_path.as_ref() else {
            return CachedEnrichmentResult {
                memories: Vec::new(),
                result_fingerprint: fingerprint_bytes(&[b"uncacheable-enrichment-result"]),
                bytes: 0,
                cacheable: false,
            };
        };
        let Some(text) = get_file_text_from_memory_or_disk(gcx.clone(), path)
            .await
            .ok()
        else {
            return CachedEnrichmentResult {
                memories: Vec::new(),
                result_fingerprint: fingerprint_bytes(&[b"uncacheable-enrichment-result"]),
                bytes: 0,
                cacheable: false,
            };
        };
        let Some(source) = source_fingerprint(path, &text).await else {
            return CachedEnrichmentResult {
                memories: Vec::new(),
                result_fingerprint: fingerprint_bytes(&[b"uncacheable-enrichment-result"]),
                bytes: 0,
                cacheable: false,
            };
        };
        bytes = bytes
            .saturating_add(memo.tags.iter().map(String::len).sum::<usize>())
            .saturating_add(path.as_os_str().len());
        memo.content.clear();
        cached.push(CachedMemo { memo, source });
    }
    let result_fingerprint = fingerprint_strings(cached.iter().map(|cached| {
        format!(
            "{}:{}:{}",
            cached.memo.memid,
            cached.source.path.display(),
            fingerprint_hex(&cached.source.content_fingerprint)
        )
    }));
    CachedEnrichmentResult {
        memories: cached,
        result_fingerprint,
        bytes,
        cacheable: true,
    }
}

pub async fn revalidate_cached_memories(
    gcx: SharedGlobalContext,
    cached: &CachedEnrichmentResult,
    score_threshold: f32,
    current_chat_id: Option<&str>,
) -> Option<Vec<MemoRecord>> {
    let current_root = gcx
        .memory_runtime
        .clone()
        .enrichment_current_root_id(gcx.clone(), current_chat_id)
        .await;
    let mut memories = Vec::with_capacity(cached.memories.len());
    for cached_memo in &cached.memories {
        let path = &cached_memo.source.path;
        let text = get_file_text_from_memory_or_disk(gcx.clone(), path)
            .await
            .ok()?;
        let source = source_fingerprint(path, &text).await?;
        if source.size != cached_memo.source.size
            || source.modified_ns != cached_memo.source.modified_ns
            || source.content_fingerprint != cached_memo.source.content_fingerprint
        {
            return None;
        }
        if cached_memo.memo.score.unwrap_or_default() < score_threshold {
            return None;
        }
        let mut memo = cached_memo.memo.clone();
        let (frontmatter, content_start) = KnowledgeFrontmatter::parse(&text);
        if frontmatter.is_archived() || frontmatter.is_deprecated() {
            return None;
        }
        if matches!(
            (frontmatter.source_chat_id.as_deref(), current_root.as_deref()),
            (Some(source_root), Some(current_root)) if source_root == current_root
        ) {
            return None;
        }
        memo.content = text[content_start..].trim().to_string();
        memo.tags = frontmatter.tags;
        memo.title = frontmatter.title;
        memo.created = frontmatter.created;
        memo.kind = frontmatter.kind;
        memories.push(memo);
    }
    Some(memories)
}

fn record_enrichment(
    current_chat_id: Option<&str>,
    component: PerfComponent,
    outcome: PerfOutcome,
    elapsed_us: u64,
    size_bytes: Option<u64>,
    item_count: Option<u64>,
    estimated_tokens: Option<u64>,
) {
    if let Some(chat_id) = current_chat_id {
        perf_diagnostics::record_enrichment(
            component,
            chat_id,
            outcome,
            elapsed_us,
            size_bytes,
            item_count,
            estimated_tokens,
        );
    }
}

fn elapsed_us(started: Option<Instant>) -> u64 {
    started
        .map(|started| started.elapsed().as_micros().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

async fn create_knowledge_context(
    gcx: SharedGlobalContext,
    query_text: &str,
    existing_paths: &HashSet<String>,
    current_chat_id: Option<&str>,
    score_threshold: f32,
) -> Option<ChatMessage> {
    let cache_key =
        enrichment_cache_key(gcx.clone(), query_text, current_chat_id, score_threshold).await;
    let gcx_for_fetch = gcx.clone();
    let query_for_fetch = query_text.to_string();
    let current_chat_for_fetch = current_chat_id.map(str::to_string);
    let settings = crate::runtime_settings::current();
    let (cached, disposition) = gcx
        .enrichment_cache
        .get_or_fetch(cache_key, move || async move {
            let memories = gcx_for_fetch
                .memory_runtime
                .clone()
                .memories_search_for_enrichment(
                    gcx_for_fetch.clone(),
                    &query_for_fetch,
                    settings.auto_enrichment_knowledge_top_n,
                    settings.auto_enrichment_trajectory_top_n,
                    current_chat_for_fetch.as_deref(),
                    current_chat_for_fetch.as_deref(),
                )
                .await?;
            Ok(cache_result_from_memories(gcx_for_fetch, memories).await)
        })
        .await
        .ok()?;
    if enrichment_cache_key(gcx.clone(), query_text, current_chat_id, score_threshold).await
        != cache_key
    {
        return None;
    }
    let memories =
        match revalidate_cached_memories(gcx.clone(), &cached, score_threshold, current_chat_id)
            .await
        {
            Some(memories) => memories,
            None => {
                gcx.enrichment_cache.invalidate(&cache_key);
                return None;
            }
        };
    let cache_component = match disposition {
        EnrichmentCacheDisposition::Miss => PerfComponent::EnrichmentCacheMiss,
        EnrichmentCacheDisposition::Hit => PerfComponent::EnrichmentCacheHit,
        EnrichmentCacheDisposition::Coalesced => PerfComponent::EnrichmentCacheCoalesced,
    };
    record_enrichment(
        current_chat_id,
        cache_component,
        PerfOutcome::Success,
        0,
        Some(cached.bytes as u64),
        Some(cached.memories.len() as u64),
        None,
    );

    let high_score_memories: Vec<_> = memories
        .into_iter()
        .filter(|m| m.score.unwrap_or(0.0) >= score_threshold)
        .filter(|m| {
            if let Some(path) = &m.file_path {
                !existing_paths.contains(&path.to_string_lossy().to_string())
            } else {
                true
            }
        })
        .collect();

    if high_score_memories.is_empty() {
        return None;
    }

    info!(
        "Knowledge enrichment: {} memories passed threshold {}",
        high_score_memories.len(),
        score_threshold
    );

    let card_started = perf_diagnostics::is_enabled().then(Instant::now);
    let context_files = build_bounded_enrichment_context_files(
        high_score_memories,
        crate::runtime_settings::current().auto_enrichment_total_token_cap,
        crate::runtime_settings::current().auto_enrichment_card_token_cap,
    );

    if context_files.is_empty() {
        return None;
    }

    if perf_diagnostics::is_enabled() {
        let card_chars = context_files
            .iter()
            .map(|file| file.file_content.len() as u64)
            .sum::<u64>();
        record_enrichment(
            current_chat_id,
            PerfComponent::EnrichmentCardBuild,
            PerfOutcome::Success,
            elapsed_us(card_started),
            Some(card_chars),
            Some(context_files.len() as u64),
            Some((card_chars.saturating_add(3)) / 4),
        );
    }

    let query_fingerprint = fingerprint_hex(&cache_key.query_fingerprint);
    let result_fingerprint = fingerprint_hex(&cached.result_fingerprint);
    let index_fingerprint = fingerprint_hex(&fingerprint_bytes(&[
        cache_key.index_generation.to_string().as_bytes(),
        &cache_key.embedding_config_fingerprint,
    ]));
    let identity = format!("{query_fingerprint}:{result_fingerprint}:{index_fingerprint}");
    let mut context = ChatMessage {
        role: "context_file".to_string(),
        content: ChatContent::ContextFiles(context_files),
        tool_call_id: KNOWLEDGE_ENRICHMENT_MARKER.to_string(),
        ..Default::default()
    };
    context.extra.insert(
        "knowledge_enrichment".to_string(),
        serde_json::json!({
            "identity": identity,
            "query_fingerprint": query_fingerprint,
            "result_fingerprint": result_fingerprint,
            "index_fingerprint": index_fingerprint,
        }),
    );
    Some(context)
}

pub async fn get_allowed_enrichment_dirs(gcx: SharedGlobalContext) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let config_dir = gcx.config_dir.clone();
    let mut candidates = Vec::new();
    let project_dirs = get_project_dirs(gcx.clone()).await;
    for pd in project_dirs {
        candidates.push(pd.join(KNOWLEDGE_FOLDER_NAME));
    }
    candidates.push(config_dir.join("knowledge"));

    let mut seen = HashSet::new();
    for candidate in candidates {
        if let Some(canonical) = canonicalize_allowed_enrichment_root(&candidate).await {
            if seen.insert(canonical.clone()) {
                dirs.push(canonical);
            }
        }
    }

    dirs
}

async fn canonicalize_allowed_enrichment_root(root: &std::path::Path) -> Option<PathBuf> {
    let metadata = match tokio::fs::symlink_metadata(root).await {
        Ok(metadata) => metadata,
        Err(_) => return None,
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        warn!(
            "preview: skipping unsafe enrichment root: {}",
            root.display()
        );
        return None;
    }
    let canonical = match tokio::fs::canonicalize(root).await {
        Ok(canonical) => canonical,
        Err(_) => return None,
    };
    Some(dunce::simplified(&canonical).to_path_buf())
}
