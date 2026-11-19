use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use refact_core::knowledge_frontmatter::KnowledgeFrontmatter;
use refact_core::knowledge_index::KnowledgeSearchFilters;
use refact_core::memories_format::MAX_CONCURRENT_ENRICHMENT_SEARCHES;
use refact_core::vecdb_types::VecdbSearchScope;
use refact_knowledge_graph::memo::MemoRecord;
use tokio::sync::Semaphore;
use tracing::warn;

use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};
use crate::files_in_workspace::get_file_text_from_memory_or_disk;
use crate::global_context::GlobalContext;
use crate::GlobalContextCtx;
use crate::memories_index::{load_root_chat_id_from_trajectory, resolve_root_chat_id};
use crate::memories_runtime::{get_all_knowledge_dirs, path_contains_component};

static ENRICHMENT_SEARCH_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub async fn memories_search(
    gcx: Arc<GlobalContext>,
    query: &str,
    top_n_memories: usize,
    top_n_trajectories: usize,
    exclude_trajectory_id: Option<&str>,
) -> Result<Vec<MemoRecord>, String> {
    memories_search_inner(
        gcx,
        query,
        top_n_memories,
        top_n_trajectories,
        exclude_trajectory_id,
        None,
    )
    .await
}

pub async fn memories_search_for_enrichment(
    gcx: Arc<GlobalContext>,
    query: &str,
    top_n_memories: usize,
    top_n_trajectories: usize,
    exclude_trajectory_id: Option<&str>,
    diagnostic_chat_id: Option<&str>,
) -> Result<Vec<MemoRecord>, String> {
    memories_search_inner(
        gcx,
        query,
        top_n_memories,
        top_n_trajectories,
        exclude_trajectory_id,
        diagnostic_chat_id,
    )
    .await
}

async fn memories_search_inner(
    gcx: Arc<GlobalContext>,
    query: &str,
    top_n_memories: usize,
    top_n_trajectories: usize,
    exclude_trajectory_id: Option<&str>,
    diagnostic_chat_id: Option<&str>,
) -> Result<Vec<MemoRecord>, String> {
    let root_discovery_started = perf_diagnostics::is_enabled().then(Instant::now);
    let knowledge_dirs = get_all_knowledge_dirs(&gcx).await;
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentRootDiscovery,
        PerfOutcome::Success,
        elapsed_us(root_discovery_started),
        None,
        Some(knowledge_dirs.len() as u64),
    );

    let mut root_cache: HashMap<String, String> = HashMap::new();
    let root_resolve_started = perf_diagnostics::is_enabled().then(Instant::now);
    let exclude_root = match exclude_trajectory_id {
        Some(id) => Some(resolve_root_chat_id(gcx.clone(), id, &mut root_cache).await),
        None => None,
    };
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentCurrentRootResolve,
        PerfOutcome::Success,
        elapsed_us(root_resolve_started),
        None,
        Some(usize::from(exclude_root.is_some()) as u64),
    );

    // Resolve trajectory dirs before acquiring vec_db lock to avoid holding the
    // mutex across gcx.read() calls (which could deadlock with concurrent gcx.write()).
    let trajectory_dirs =
        crate::trajectories::paths::get_all_trajectories_dirs(&gcx.chat_ctx(gcx.clone())).await;

    let vecdb_arc = {
        let gcx_read = gcx.clone();
        gcx_read.vec_db.clone()
    };

    let vecdb_lock_started = perf_diagnostics::is_enabled().then(Instant::now);
    let vecdb = vecdb_arc.lock().await.clone();
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentVecdbLockWait,
        PerfOutcome::Success,
        elapsed_us(vecdb_lock_started),
        None,
        Some(1),
    );
    let vecdb_hold_started = perf_diagnostics::is_enabled().then(Instant::now);
    let Some(vecdb) = vecdb else {
        record_enrichment_metric(
            diagnostic_chat_id,
            PerfComponent::EnrichmentVecdbLockHold,
            PerfOutcome::Success,
            elapsed_us(vecdb_hold_started),
            None,
            Some(0),
        );
        return memories_search_fallback(
            gcx,
            query,
            top_n_memories,
            &knowledge_dirs,
            exclude_root.as_deref(),
            diagnostic_chat_id,
        )
        .await;
    };
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentVecdbLockHold,
        PerfOutcome::Success,
        elapsed_us(vecdb_hold_started),
        None,
        Some(0),
    );

    let _enrichment_permit = enrichment_search_semaphore()
        .acquire_owned()
        .await
        .map_err(|_| "knowledge enrichment search is shutting down".to_string())?;

    // Improve recall by doing two scoped searches:
    // - knowledge roots
    // - trajectory roots
    // This avoids code chunks dominating a global top-K.
    let embedding_started = perf_diagnostics::is_enabled().then(Instant::now);
    let embedding = vecdb.embed_query(query).await;
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentEmbedding,
        if embedding.is_ok() {
            PerfOutcome::Success
        } else {
            PerfOutcome::Failure
        },
        elapsed_us(embedding_started),
        None,
        Some(1),
    );
    let embedding = embedding?;

    let k_knowledge = ((top_n_memories.max(1) + top_n_trajectories.max(1)) * 50).min(400);
    let k_trajectories = ((top_n_memories.max(1) + top_n_trajectories.max(1)) * 50).min(400);

    let scoped_search_started = perf_diagnostics::is_enabled().then(Instant::now);
    let scopes = knowledge_dirs
        .iter()
        .map(|dir| VecdbSearchScope {
            path_prefix: scope_prefix(dir),
            top_n: k_knowledge,
        })
        .chain(trajectory_dirs.iter().map(|dir| VecdbSearchScope {
            path_prefix: scope_prefix(dir),
            top_n: k_trajectories,
        }))
        .collect::<Vec<_>>();
    let scoped_searches = u64::from(!scopes.is_empty());
    let mut combined_results = vecdb
        .vecdb_search_scopes_with_embedding(&embedding, &scopes)
        .await
        .unwrap_or_else(|error| {
            warn!("memories: scoped VecDB search failed: {}", error);
            vec![Vec::new(); scopes.len()]
        })
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentScopedSearch,
        PerfOutcome::Success,
        elapsed_us(scoped_search_started),
        None,
        Some(scoped_searches),
    );

    // De-dup identical segments; keep best usefulness.
    let merge_dedup_started = perf_diagnostics::is_enabled().then(Instant::now);
    combined_results.sort_by(|a, b| b.usefulness.total_cmp(&a.usefulness));
    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::<(PathBuf, u64, u64)>::new();
    for r in combined_results {
        let key = (r.file_path.clone(), r.start_line, r.end_line);
        if seen.insert(key) {
            deduped.push(r);
        }
        if deduped.len() >= (k_knowledge + k_trajectories) {
            break;
        }
    }

    let search_result = crate::vdb_structs::SearchResult {
        query_text: query.to_string(),
        results: deduped,
    };
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentMergeDedup,
        PerfOutcome::Success,
        elapsed_us(merge_dedup_started),
        None,
        Some(search_result.results.len() as u64),
    );
    struct KnowledgeMatch {
        best_score: f32,
    }
    struct TrajectoryMatch {
        best_score: f32,
        matched_ranges: Vec<(u64, u64)>,
    }

    let mut knowledge_matches: HashMap<PathBuf, KnowledgeMatch> = HashMap::new();
    let mut trajectory_matches: HashMap<PathBuf, TrajectoryMatch> = HashMap::new();

    for rec in search_result.results.iter() {
        // Prefer VecDB usefulness normalization over raw distance.
        let score = (rec.usefulness / 100.0).clamp(0.0, 1.0);
        let is_knowledge = knowledge_dirs.iter().any(|d| rec.file_path.starts_with(d))
            && !path_contains_component(&rec.file_path, "archive");
        let is_trajectory = trajectory_dirs.iter().any(|d| rec.file_path.starts_with(d))
            && rec
                .file_path
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false);

        if is_knowledge {
            knowledge_matches
                .entry(rec.file_path.clone())
                .and_modify(|m| {
                    if score > m.best_score {
                        m.best_score = score;
                    }
                })
                .or_insert(KnowledgeMatch { best_score: score });
        } else if is_trajectory {
            let traj_id = rec
                .file_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            if let Some(ref ex_root) = exclude_root {
                if !traj_id.is_empty() {
                    let candidate_root = if let Some(cached) = root_cache.get(&traj_id) {
                        cached.clone()
                    } else if let Some(stored_root) =
                        load_root_chat_id_from_trajectory(gcx.clone(), &rec.file_path).await
                    {
                        root_cache.insert(traj_id.clone(), stored_root.clone());
                        stored_root
                    } else {
                        resolve_root_chat_id(gcx.clone(), &traj_id, &mut root_cache).await
                    };
                    if candidate_root == *ex_root {
                        continue;
                    }
                }
            }

            trajectory_matches
                .entry(rec.file_path.clone())
                .and_modify(|m| {
                    if score > m.best_score {
                        m.best_score = score;
                    }
                    m.matched_ranges.push((rec.start_line, rec.end_line));
                })
                .or_insert(TrajectoryMatch {
                    best_score: score,
                    matched_ranges: vec![(rec.start_line, rec.end_line)],
                });
        }
    }

    let mut records = Vec::new();

    // Process knowledge files (whole content)
    let mut sorted_knowledge: Vec<_> = knowledge_matches.into_iter().collect();
    sorted_knowledge.sort_by(|a, b| {
        b.1.best_score
            .partial_cmp(&a.1.best_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let reread_started = perf_diagnostics::is_enabled().then(Instant::now);
    let mut reread_count = 0u64;
    let mut reread_bytes = 0u64;
    for (file_path, file_match) in sorted_knowledge.into_iter().take(top_n_memories) {
        let text = match get_file_text_from_memory_or_disk(gcx.clone(), &file_path).await {
            Ok(t) => {
                reread_count += 1;
                reread_bytes = reread_bytes.saturating_add(t.len() as u64);
                t
            }
            Err(_) => continue,
        };

        let (frontmatter, content_start) = KnowledgeFrontmatter::parse(&text);
        if frontmatter.is_archived() || frontmatter.is_deprecated() {
            continue;
        }

        if let (Some(ref ex_root), Some(ref source_id)) =
            (&exclude_root, &frontmatter.source_chat_id)
        {
            if source_id == ex_root {
                tracing::debug!(
                    "Excluding knowledge created by current chat: {:?}",
                    file_path
                );
                continue;
            }
        }

        let content = text[content_start..].trim().to_string();
        let line_count = content.lines().count();
        let id = frontmatter
            .id
            .clone()
            .unwrap_or_else(|| file_path.to_string_lossy().to_string());

        records.push(MemoRecord {
            memid: id,
            tags: frontmatter.tags,
            content,
            file_path: Some(file_path),
            line_range: Some((1, line_count as u64)),
            title: frontmatter.title,
            created: frontmatter.created,
            kind: frontmatter.kind,
            score: Some(file_match.best_score),
        });
    }

    // Process trajectories (matched parts only)
    let mut sorted_trajectories: Vec<_> = trajectory_matches.into_iter().collect();
    sorted_trajectories.sort_by(|a, b| {
        b.1.best_score
            .partial_cmp(&a.1.best_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for (file_path, traj_match) in sorted_trajectories.into_iter().take(top_n_trajectories) {
        let text = match get_file_text_from_memory_or_disk(gcx.clone(), &file_path).await {
            Ok(t) => {
                reread_count += 1;
                reread_bytes = reread_bytes.saturating_add(t.len() as u64);
                t
            }
            Err(_) => continue,
        };

        let traj_json: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let traj_id = file_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let traj_title = traj_json
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled")
            .to_string();

        let messages = match traj_json.get("messages").and_then(|v| v.as_array()) {
            Some(m) => m,
            None => continue,
        };

        // Extract matched message content
        let mut matched_content = Vec::new();
        for (start, end) in &traj_match.matched_ranges {
            let start_idx = *start as usize;
            let end_idx = (*end as usize).min(messages.len().saturating_sub(1));

            for idx in start_idx..=end_idx {
                if let Some(msg) = messages.get(idx) {
                    let role = msg
                        .get("role")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let content = msg
                        .get("content")
                        .map(|v| {
                            if let Some(s) = v.as_str() {
                                s.chars().take(500).collect::<String>()
                            } else {
                                v.to_string().chars().take(500).collect()
                            }
                        })
                        .unwrap_or_default();

                    if !content.is_empty() && role != "system" && role != "context_file" {
                        matched_content.push(format!("[msg {}] {}: {}", idx, role, content));
                    }
                }
            }
        }

        if matched_content.is_empty() {
            continue;
        }

        let content = format!(
            "Trajectory: {} ({})\n\n{}",
            traj_title,
            traj_id,
            matched_content.join("\n\n")
        );

        records.push(MemoRecord {
            memid: traj_id.clone(),
            tags: vec!["trajectory".to_string()],
            content,
            file_path: Some(file_path),
            line_range: None,
            title: Some(traj_title),
            created: None,
            kind: Some("trajectory".to_string()),
            score: Some(traj_match.best_score),
        });
    }

    tracing::info!(
        "memories_search: found {} knowledge + {} trajectories",
        records
            .iter()
            .filter(|r| r.kind.as_deref() != Some("trajectory"))
            .count(),
        records
            .iter()
            .filter(|r| r.kind.as_deref() == Some("trajectory"))
            .count()
    );
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentFileReread,
        PerfOutcome::Success,
        elapsed_us(reread_started),
        Some(reread_bytes),
        Some(reread_count),
    );

    if !records.is_empty() {
        return Ok(records);
    }

    memories_search_fallback(
        gcx,
        query,
        top_n_memories,
        &knowledge_dirs,
        exclude_root.as_deref(),
        diagnostic_chat_id,
    )
    .await
}

async fn memories_search_fallback(
    gcx: Arc<GlobalContext>,
    query: &str,
    top_n: usize,
    knowledge_dirs: &[PathBuf],
    exclude_root: Option<&str>,
    diagnostic_chat_id: Option<&str>,
) -> Result<Vec<MemoRecord>, String> {
    let fallback_started = perf_diagnostics::is_enabled().then(Instant::now);
    let query_lower = query.to_lowercase();
    const FALLBACK_STOP_WORDS: &[&str] = &[
        "the", "and", "for", "with", "that", "this", "from", "into", "about", "what", "where",
        "when", "why", "how", "can", "could", "should", "would", "please", "find", "search",
        "look", "need", "want", "have", "does", "doesn", "work",
    ];
    let mut seen_query_words = std::collections::HashSet::new();
    let query_words: Vec<&str> = query_lower
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .map(|w| w.trim_matches('-'))
        .filter(|w| w.len() >= 3)
        .filter(|w| !FALLBACK_STOP_WORDS.contains(w))
        .filter(|w| seen_query_words.insert((*w).to_string()))
        .collect();
    if knowledge_dirs.is_empty() || query_words.is_empty() {
        record_enrichment_metric(
            diagnostic_chat_id,
            PerfComponent::EnrichmentFallback,
            PerfOutcome::Skipped,
            elapsed_us(fallback_started),
            Some(0),
            Some(0),
        );
        return Ok(vec![]);
    }

    let candidate_cap = top_n.saturating_mul(4).clamp(1, 32);
    let (records, index_available) = {
        let index = gcx.knowledge_index.lock().await;
        let index_available = index.is_ready();
        let records = if index_available {
            index
                .search_bounded(
                    query,
                    &KnowledgeSearchFilters::default(),
                    candidate_cap,
                    candidate_cap,
                )
                .into_iter()
                .filter(|hit| {
                    knowledge_dirs
                        .iter()
                        .any(|dir| hit.card.file_path.starts_with(dir))
                })
                .filter(|hit| {
                    exclude_root.is_none_or(|root| {
                        index.source_chat_id_for_path(&hit.card.file_path) != Some(root)
                    })
                })
                .take(top_n)
                .map(|hit| {
                    let content = index
                        .content_for_path(&hit.card.file_path)
                        .unwrap_or_default()
                        .chars()
                        .take(500)
                        .collect();
                    MemoRecord {
                        memid: hit.card.id,
                        tags: hit.card.tags,
                        content,
                        file_path: Some(hit.card.file_path),
                        line_range: None,
                        title: Some(hit.card.title),
                        created: hit.card.created,
                        kind: hit.card.kind,
                        score: Some((hit.score / 2.0).clamp(0.0, 1.0)),
                    }
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        (records, index_available)
    };
    if !index_available {
        warn!("memories: fallback index unavailable; returning no knowledge candidates");
    }
    record_enrichment_metric(
        diagnostic_chat_id,
        PerfComponent::EnrichmentFallback,
        if index_available {
            PerfOutcome::Success
        } else {
            PerfOutcome::Skipped
        },
        elapsed_us(fallback_started),
        Some(0),
        Some(0),
    );
    Ok(records)
}

fn record_enrichment_metric(
    diagnostic_chat_id: Option<&str>,
    component: PerfComponent,
    outcome: PerfOutcome,
    elapsed_us: u64,
    size_bytes: Option<u64>,
    item_count: Option<u64>,
) {
    if let Some(chat_id) = diagnostic_chat_id {
        perf_diagnostics::record_enrichment(
            component, chat_id, outcome, elapsed_us, size_bytes, item_count, None,
        );
    }
}

fn elapsed_us(started: Option<Instant>) -> u64 {
    started
        .map(|started| started.elapsed().as_micros().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn enrichment_search_semaphore() -> Arc<Semaphore> {
    ENRICHMENT_SEARCH_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_ENRICHMENT_SEARCHES)))
        .clone()
}

fn scope_prefix(dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    if dir.ends_with(std::path::MAIN_SEPARATOR) {
        dir.to_string()
    } else {
        format!("{}{}", dir, std::path::MAIN_SEPARATOR)
    }
}
