use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Local, Utc};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AMutex;
use tracing::{info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

use refact_buddy_core::memory_dedup;
use refact_core::knowledge_frontmatter::KnowledgeFrontmatter;
use refact_core::knowledge_index::KnowledgeIndex;
use refact_core::memory_plane::MemoryPlaneRoots;
use refact_core::memories_format::{compute_content_hash_hex, generate_filename};
use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
use refact_knowledge_graph::memo::MemoRecord;
use refact_knowledge_graph::memory_types::MemoryWriteOutcome;

use crate::app_state::AppState;
use crate::chat_ctx::ChatCtx;
use crate::files_correction::{get_project_dirs, get_unscoped_project_dirs_for_ctx};
use crate::files_in_workspace::get_file_text_from_memory_or_disk;
use crate::global_context::{GlobalContextCtx, SharedGlobalContext};

const NEAR_DUPLICATE_CANDIDATE_CAP: usize = 64;

pub fn path_contains_component(path: &Path, component: &str) -> bool {
    path.components().any(|c| c.as_os_str() == component)
}


pub async fn memory_plane_roots(gcx: SharedGlobalContext) -> MemoryPlaneRoots {
    let trajectories_dir = gcx.config_dir.join("trajectories");
    MemoryPlaneRoots::new(
        get_project_dirs(gcx.clone()).await,
        Some(get_global_knowledge_dir(&gcx).await),
        Some(trajectories_dir),
    )
}
pub async fn memory_plane_roots_for_ctx(ctx: &ChatCtx) -> MemoryPlaneRoots {
    MemoryPlaneRoots::new(
        get_unscoped_project_dirs_for_ctx(ctx).await,
        Some(crate::memories::get_global_knowledge_dir_for_ctx(ctx)),
        Some(ctx.paths.config_dir.join("trajectories")),
    )
}

pub async fn get_global_knowledge_dir(gcx: &SharedGlobalContext) -> PathBuf {
    crate::memories::get_global_knowledge_dir_for_ctx(&gcx.chat_ctx(gcx.clone()))
}

pub async fn get_all_knowledge_dirs(gcx: &SharedGlobalContext) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = get_project_dirs(gcx.clone())
        .await
        .into_iter()
        .map(|p| p.join(KNOWLEDGE_FOLDER_NAME))
        .filter(|p| p.exists())
        .collect();

    let global_dir = get_global_knowledge_dir(gcx).await;
    if global_dir.exists() {
        dirs.push(global_dir);
    }

    dirs
}

pub async fn get_first_knowledge_dir_for_ctx(ctx: &ChatCtx) -> Result<PathBuf, String> {
    let project_dirs = get_unscoped_project_dirs_for_ctx(ctx).await;
    let workspace_root = project_dirs.first().ok_or("No workspace folder found")?;
    Ok(workspace_root.join(KNOWLEDGE_FOLDER_NAME))
}

pub async fn memories_add(
    gcx: &SharedGlobalContext,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Result<PathBuf, String> {
    memories_add_for_ctx(&gcx.chat_ctx(gcx.clone()), frontmatter, content).await
}

pub async fn memories_add_for_ctx(
    ctx: &ChatCtx,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Result<PathBuf, String> {
    let knowledge_dir = get_first_knowledge_dir_for_ctx(ctx).await?;
    fs::create_dir_all(&knowledge_dir)
        .await
        .map_err(|e| format!("Failed to create knowledge dir: {}", e))?;

    let filename = generate_filename(content);
    let file_path = knowledge_dir.join(&filename);

    if file_path.exists() {
        return Err(format!("File already exists: {}", file_path.display()));
    }

    let md_content = format!("{}\n\n{}", frontmatter.to_yaml(), content);
    atomic_write_text(&file_path, &md_content).await?;
    ctx.workspace
        .enrichment_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    info!("Created knowledge entry: {}", file_path.display());

    // Update fast in-memory knowledge index (best-effort, new docs going forward).
    {
        let mut idx = ctx.workspace.knowledge_index.lock().await;
        idx.add_signature(memory_dedup::content_signature(content), file_path.clone());
        idx.add_from_frontmatter(file_path.clone(), frontmatter, Some(content));
    }

    let roots = memory_plane_roots_for_ctx(ctx).await;
    let vec_db = ctx.workspace.vec_db.clone();
    if let Some(vecdb) = vec_db.lock().await.clone() {
        vecdb
            .vectorizer_enqueue_files(&vec![file_path.to_string_lossy().to_string()], true, roots)
            .await;
    }

    Ok(file_path)
}

pub async fn touch_memory_observation(
    gcx: &SharedGlobalContext,
    doc_path: &Path,
) -> Result<(), String> {
    touch_memory_observation_for_ctx(&gcx.chat_ctx(gcx.clone()), doc_path).await
}

pub async fn touch_memory_observation_for_ctx(
    ctx: &ChatCtx,
    doc_path: &Path,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    crate::memories::update_memory_document_frontmatter_for_ctx(ctx, doc_path, |frontmatter| {
        frontmatter.occurrences = frontmatter.occurrences.saturating_add(1).max(2);
        frontmatter.last_observed = Some(now);
        Ok(true)
    })
    .await
    .map(|_| ())
}

pub async fn evict_stale_index_path(gcx: &SharedGlobalContext, path: &Path) {
    evict_stale_index_path_for_ctx(&gcx.chat_ctx(gcx.clone()), path).await
}

pub async fn evict_stale_index_path_for_ctx(ctx: &ChatCtx, path: &Path) {
    warn!(
        "memories: knowledge index pointed at missing file {}, evicting stale entry",
        path.display()
    );
    let mut idx = ctx.workspace.knowledge_index.lock().await;
    idx.remove_path(path);
}

pub async fn find_duplicate_memory_for_ctx(
    ctx: &ChatCtx,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Option<PathBuf> {
    loop {
        let candidate = {
            let idx = ctx.workspace.knowledge_index.lock().await;
            find_duplicate_in_index(&idx, frontmatter, content)
        };
        let path = candidate?;
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Some(path);
        }
        evict_stale_index_path_for_ctx(ctx, &path).await;
    }
}

fn find_duplicate_in_index(
    idx: &KnowledgeIndex,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Option<PathBuf> {
    let signature = memory_dedup::content_signature(content);
    if let Some(path) = idx.first_path_for_signature(&signature) {
        return Some(path.clone());
    }

    let mut seen = HashSet::new();
    let mut checked = 0usize;
    for tag in &frontmatter.tags {
        for card in idx.related_for_tags(std::slice::from_ref(tag), NEAR_DUPLICATE_CANDIDATE_CAP) {
            if checked >= NEAR_DUPLICATE_CANDIDATE_CAP {
                return None;
            }
            if !seen.insert(card.file_path.clone()) {
                continue;
            }
            let Some(existing_content) = idx.content_for_path(&card.file_path) else {
                continue;
            };
            checked += 1;
            if memory_dedup::near_duplicate(content, existing_content) {
                return Some(card.file_path.clone());
            }
        }
    }
    None
}

pub async fn memories_add_dedup(
    gcx: &SharedGlobalContext,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Result<MemoryWriteOutcome, String> {
    memories_add_dedup_for_ctx(&gcx.chat_ctx(gcx.clone()), frontmatter, content).await
}

pub async fn memories_add_dedup_for_ctx(
    ctx: &ChatCtx,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Result<MemoryWriteOutcome, String> {
    if let Some(existing) = find_duplicate_memory_for_ctx(ctx, frontmatter, content).await {
        info!(
            "memories: duplicate content detected, touching {} instead of creating a new file",
            existing.display()
        );
        match touch_memory_observation_for_ctx(ctx, &existing).await {
            Ok(()) => return Ok(MemoryWriteOutcome::DuplicateTouched(existing)),
            Err(err) => {
                warn!(
                    "memories: failed to touch duplicate {}: {}, creating instead",
                    existing.display(),
                    err
                );
                evict_stale_index_path_for_ctx(ctx, &existing).await;
            }
        }
    }
    memories_add_for_ctx(ctx, frontmatter, content)
        .await
        .map(MemoryWriteOutcome::Created)
}

pub async fn memories_upsert_by_signal_key(
    gcx: &SharedGlobalContext,
    signal_key: &str,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Result<MemoryWriteOutcome, String> {
    memories_upsert_by_signal_key_for_ctx(
        &gcx.chat_ctx(gcx.clone()),
        signal_key,
        frontmatter,
        content,
    )
    .await
}

pub async fn memories_upsert_by_signal_key_for_ctx(
    ctx: &ChatCtx,
    signal_key: &str,
    frontmatter: &KnowledgeFrontmatter,
    content: &str,
) -> Result<MemoryWriteOutcome, String> {
    let signal_key = signal_key.trim();
    if signal_key.is_empty() {
        return memories_add_dedup_for_ctx(ctx, frontmatter, content).await;
    }
    loop {
        let existing = {
            let idx = ctx.workspace.knowledge_index.lock().await;
            idx.first_path_for_signal_key(signal_key).cloned()
        };
        let Some(doc_path) = existing else {
            break;
        };
        let existing_text = match fs::read_to_string(&doc_path).await {
            Ok(text) => text,
            Err(err) => {
                warn!(
                    "memories: failed to read signal doc {}: {}",
                    doc_path.display(),
                    err
                );
                evict_stale_index_path_for_ctx(ctx, &doc_path).await;
                continue;
            }
        };
        let (mut existing_frontmatter, _) = KnowledgeFrontmatter::parse(&existing_text);
        existing_frontmatter.occurrences =
            existing_frontmatter.occurrences.saturating_add(1).max(2);
        existing_frontmatter.last_observed = Some(Utc::now().to_rfc3339());
        existing_frontmatter.updated = Some(Local::now().format("%Y-%m-%d").to_string());
        existing_frontmatter.signal_key = Some(signal_key.to_string());
        if let Some(title) = &frontmatter.title {
            existing_frontmatter.title = Some(title.clone());
        }
        if let Some(kind) = &frontmatter.kind {
            existing_frontmatter.kind = Some(kind.clone());
        }
        for tag in &frontmatter.tags {
            if !existing_frontmatter.tags.contains(tag) {
                existing_frontmatter.tags.push(tag.clone());
            }
        }
        for filename in &frontmatter.filenames {
            if !existing_frontmatter.filenames.contains(filename) {
                existing_frontmatter.filenames.push(filename.clone());
            }
        }
        existing_frontmatter.content_hash = Some(compute_content_hash_hex(content));
        rewrite_memory_document_for_ctx(ctx, &doc_path, &existing_frontmatter, content).await?;
        info!(
            "memories: upserted signal '{}' in place at {}",
            signal_key,
            doc_path.display()
        );
        return Ok(MemoryWriteOutcome::DuplicateTouched(doc_path));
    }
    let mut stamped = frontmatter.clone();
    stamped.signal_key = Some(signal_key.to_string());
    stamped.occurrences = stamped.occurrences.max(1);
    stamped.last_observed = Some(Utc::now().to_rfc3339());
    let outcome = memories_add_dedup_for_ctx(ctx, &stamped, content).await?;
    if let MemoryWriteOutcome::DuplicateTouched(path) = &outcome {
        let key = signal_key.to_string();
        let stamp = crate::memories::update_memory_document_frontmatter_for_ctx(
            ctx,
            path,
            move |fm| {
                if fm.signal_key.as_deref() == Some(key.as_str()) {
                    return Ok(false);
                }
                fm.signal_key = Some(key);
                Ok(true)
            },
        )
        .await;
        if let Err(err) = stamp {
            warn!(
                "memories: failed to stamp signal key on deduped doc {}: {}",
                path.display(),
                err
            );
        }
    }
    Ok(outcome)
}

pub async fn load_memories_by_tags(
    gcx: &SharedGlobalContext,
    allowed_tags: &[&str],
    max_items: usize,
) -> Result<Vec<MemoRecord>, String> {
    let knowledge_dirs = get_all_knowledge_dirs(gcx).await;

    if knowledge_dirs.is_empty() {
        return Ok(vec![]);
    }

    let mut records = Vec::new();

    for knowledge_dir in &knowledge_dirs {
        if !knowledge_dir.exists() {
            continue;
        }

        for entry in WalkDir::new(knowledge_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path_contains_component(path, "archive") {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "md" && ext != "mdx" {
                continue;
            }

            let text =
                match get_file_text_from_memory_or_disk(gcx.clone(), &path.to_path_buf()).await {
                    Ok(t) => t,
                    Err(_) => continue,
                };

            let (frontmatter, content_start) = KnowledgeFrontmatter::parse(&text);
            if frontmatter.is_archived() || frontmatter.is_deprecated() {
                continue;
            }

            let has_matching_tag = frontmatter.tags.iter().any(|tag| {
                let tag_lower = tag.to_lowercase();
                allowed_tags
                    .iter()
                    .any(|allowed| tag_lower.contains(&allowed.to_lowercase()))
            });

            let kind_matches = frontmatter.kind.as_ref().map_or(false, |k| {
                let kind_lower = k.to_lowercase();
                allowed_tags
                    .iter()
                    .any(|allowed| kind_lower.contains(&allowed.to_lowercase()))
            });

            if !has_matching_tag && !kind_matches {
                continue;
            }

            let content = text[content_start..].trim().to_string();
            let id = frontmatter
                .id
                .clone()
                .unwrap_or_else(|| path.to_string_lossy().to_string());

            records.push(MemoRecord {
                memid: id,
                tags: frontmatter.tags,
                content,
                file_path: Some(path.to_path_buf()),
                line_range: None,
                title: frontmatter.title,
                created: frontmatter.created,
                kind: frontmatter.kind,
                score: None,
            });
        }
    }

    records.sort_by(|a, b| {
        let date_a = a.created.as_deref().unwrap_or("");
        let date_b = b.created.as_deref().unwrap_or("");
        date_b.cmp(date_a)
    });

    records.truncate(max_items);

    tracing::info!(
        "load_memories_by_tags: found {} memories with tags {:?}",
        records.len(),
        allowed_tags
    );

    Ok(records)
}

pub async fn load_memories_by_tags_from_index(
    knowledge_index: Arc<AMutex<KnowledgeIndex>>,
    allowed_tags: &[&str],
    max_items: usize,
) -> Vec<MemoRecord> {
    let idx_arc = knowledge_index;
    let matched = {
        let idx = idx_arc.lock().await;
        idx.memos_by_tag_or_kind_substring(allowed_tags, max_items)
    };
    matched
        .into_iter()
        .map(|(card, content)| MemoRecord {
            memid: card.id,
            tags: card.tags,
            content: content.trim().to_string(),
            file_path: Some(card.file_path),
            line_range: None,
            title: Some(card.title),
            created: card.created,
            kind: card.kind,
            score: None,
        })
        .collect()
}

pub async fn memories_search_for_app(
    app: &AppState,
    query: &str,
    top_n_memories: usize,
    top_n_trajectories: usize,
    exclude_trajectory_id: Option<&str>,
) -> Result<Vec<MemoRecord>, String> {
    app.gcx
        .memory_runtime
        .clone()
        .memories_search(
            app.gcx.clone(),
            query,
            top_n_memories,
            top_n_trajectories,
            exclude_trajectory_id,
        )
        .await
}

pub async fn rewrite_memory_document(
    gcx: &SharedGlobalContext,
    doc_path: &Path,
    frontmatter: &KnowledgeFrontmatter,
    body: &str,
) -> Result<(), String> {
    rewrite_memory_document_for_ctx(&gcx.chat_ctx(gcx.clone()), doc_path, frontmatter, body).await
}

pub async fn rewrite_memory_document_for_ctx(
    ctx: &ChatCtx,
    doc_path: &Path,
    frontmatter: &KnowledgeFrontmatter,
    body: &str,
) -> Result<(), String> {
    let content = format!("{}\n\n{}", frontmatter.to_yaml(), body.trim());
    atomic_write_text(doc_path, &content).await?;
    ctx.workspace
        .enrichment_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let path_buf = doc_path.to_path_buf();
    {
        let mut idx = ctx.workspace.knowledge_index.lock().await;
        idx.remove_path(&path_buf);
        if !frontmatter.is_archived() && !frontmatter.is_deprecated() {
            idx.add_signature(memory_dedup::content_signature(body), path_buf.clone());
            idx.add_from_frontmatter(path_buf.clone(), frontmatter, Some(body));
        }
    }

    let roots = memory_plane_roots_for_ctx(ctx).await;
    let vec_db = ctx.workspace.vec_db.clone();
    if let Some(vecdb) = vec_db.lock().await.clone() {
        if frontmatter.is_archived() || frontmatter.is_deprecated() {
            let _ = vecdb.remove_file(&path_buf).await;
        } else {
            vecdb
                .vectorizer_enqueue_files(
                    &vec![path_buf.to_string_lossy().to_string()],
                    true,
                    roots,
                )
                .await;
        }
    }

    ctx.workspace
        .memory_document_map
        .lock()
        .await
        .remove(&path_buf);

    Ok(())
}

async fn atomic_write_text(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Invalid memory path: missing parent".to_string())?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Invalid memory path: missing file name".to_string())?;
    let tmp_path = parent.join(format!(".{}.tmp-{}", file_name, Uuid::new_v4()));
    let write_result = async {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)
            .await
            .map_err(|e| format!("Failed to create temporary memory file: {}", e))?;
        file.write_all(content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write temporary memory file: {}", e))?;
        file.flush()
            .await
            .map_err(|e| format!("Failed to flush temporary memory file: {}", e))?;
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)
                .await
                .map_err(|e| format!("Failed to replace memory file: {}", e))?;
        }
        fs::rename(&tmp_path, path)
            .await
            .map_err(|e| format!("Failed to replace memory file: {}", e))
    }
    .await;

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path).await;
    }
    write_result
}

pub async fn delete_document_from_disk(
    gcx: &SharedGlobalContext,
    doc_path: &PathBuf,
) -> Result<(), String> {
    delete_document_from_disk_for_ctx(&gcx.chat_ctx(gcx.clone()), doc_path).await
}

pub async fn delete_document_from_disk_for_ctx(
    ctx: &ChatCtx,
    doc_path: &PathBuf,
) -> Result<(), String> {
    match fs::remove_file(doc_path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Failed to delete document: {}", e)),
    }
    ctx.workspace
        .enrichment_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    info!("Deleted document from disk: {}", doc_path.display());

    ctx.workspace.knowledge_index.lock().await.remove_path(doc_path);

    let vec_db = ctx.workspace.vec_db.clone();
    if let Some(vecdb) = vec_db.lock().await.clone() {
        let _ = vecdb.remove_file(doc_path).await;
    }

    ctx.workspace
        .memory_document_map
        .lock()
        .await
        .remove(doc_path);

    Ok(())
}

pub async fn deprecate_document(
    gcx: &SharedGlobalContext,
    doc_path: &PathBuf,
    superseded_by: Option<&str>,
    _reason: &str,
) -> Result<(), String> {
    crate::memories::update_memory_document_frontmatter_for_ctx(
        &gcx.chat_ctx(gcx.clone()),
        doc_path,
        |frontmatter| {
            frontmatter.status = Some("deprecated".to_string());
            frontmatter.deprecated_at = Some(Local::now().format("%Y-%m-%d").to_string());
            frontmatter.updated = Some(Local::now().format("%Y-%m-%d").to_string());
            if let Some(superseded_by) = superseded_by {
                frontmatter.superseded_by = Some(superseded_by.to_string());
            }
            Ok(true)
        },
    )
    .await
    .map(|_| ())
}
