use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use axum::http::{Response, StatusCode};
use axum::extract::State;
use hyper::Body;
use regex::Regex;
use serde::{Deserialize, Serialize};
use chrono::Local;

use refact_runtime_api::app_state::AppState;
use refact_runtime_api::global_context::GlobalContext;
use refact_buddy_core::memory_lifecycle_model::parse_memory_lifecycle_status;
use refact_core::custom_error::ScratchError;
use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
use refact_core::knowledge_frontmatter::KnowledgeFrontmatter;
use refact_runtime_api::knowledge_graph::build_knowledge_graph;
use refact_core::memories_format::normalize_memory_tags;
use refact_runtime_api::memories_runtime::rewrite_memory_document;

pub const AUTO_LINK_MAX_LINKS: usize = 5;
pub const AUTO_LINK_MIN_SCORE: f64 = 3.0;
pub const AUTO_LINK_MAX_TOTAL: usize = 10;
const VALID_STATUSES: &[&str] = &["active", "proposed", "pinned", "archived", "deprecated"];

fn extract_entities(content: &str) -> Vec<String> {
    let backtick_re =
        Regex::new(r"`([a-zA-Z_][a-zA-Z0-9_:]*(?:::[a-zA-Z_][a-zA-Z0-9_]*)*)`").unwrap();
    let mut entities: HashSet<String> = HashSet::new();

    for caps in backtick_re.captures_iter(content) {
        let entity = caps.get(1).unwrap().as_str().to_string();
        if entity.len() >= 3 && entity.len() <= 100 {
            entities.insert(entity);
        }
    }

    entities.into_iter().collect()
}

fn sanitize_string(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .collect::<String>()
        .trim()
        .to_string()
}

fn sanitize_and_dedupe_strings(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .map(|s| sanitize_string(&s))
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}

pub fn apply_lifecycle_status(frontmatter: &mut KnowledgeFrontmatter, status: &str) {
    frontmatter.status = Some(status.to_string());
    if matches!(status, "archived" | "deprecated") {
        if frontmatter.deprecated_at.is_none() {
            frontmatter.deprecated_at = Some(Local::now().format("%Y-%m-%d").to_string());
        }
    } else {
        frontmatter.deprecated_at = None;
        frontmatter.superseded_by = None;
    }
}

#[derive(Deserialize)]
pub struct UpdateMemoryPost {
    pub file_path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub filenames: Option<Vec<String>>,
    #[serde(default)]
    pub links: Option<Vec<String>>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub auto_link: Option<bool>,
}

#[derive(Deserialize)]
pub struct DeleteMemoryPost {
    pub file_path: String,
    #[serde(default)]
    pub archive: bool,
}

#[derive(Serialize)]
pub struct MemoryOperationResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Default)]
pub struct RelinkStats {
    pub docs_scanned: usize,
    pub docs_updated: usize,
    pub links_added: usize,
}

pub async fn auto_link_memory(
    gcx: Arc<GlobalContext>,
    frontmatter: &mut KnowledgeFrontmatter,
    content: &str,
    doc_path: &Path,
) -> Result<(), String> {
    let entities = extract_entities(content);
    let kg = build_knowledge_graph(gcx.clone()).await;
    let similar_docs = kg.find_similar_docs(&frontmatter.tags, &frontmatter.filenames, &entities);

    let doc_id = frontmatter
        .id
        .clone()
        .unwrap_or_else(|| doc_path.to_string_lossy().to_string());

    let suggested_links: Vec<String> = similar_docs
        .into_iter()
        .filter(|(id, score)| {
            if *score < AUTO_LINK_MIN_SCORE || *id == doc_id {
                return false;
            }
            if let Some(doc) = kg.docs.get(id) {
                if !doc.frontmatter.is_active() {
                    return false;
                }
            }
            true
        })
        .take(AUTO_LINK_MAX_LINKS)
        .map(|(id, _)| id)
        .collect();

    for link in suggested_links {
        if frontmatter.links.len() >= AUTO_LINK_MAX_TOTAL {
            break;
        }
        if !frontmatter.links.contains(&link) {
            frontmatter.links.push(link);
        }
    }

    frontmatter.links.retain(|link| link != &doc_id);

    Ok(())
}

pub async fn auto_link_all_memories(gcx: Arc<GlobalContext>) -> Result<RelinkStats, String> {
    let kg = build_knowledge_graph(gcx.clone()).await;
    let mut candidates: Vec<_> = kg
        .docs
        .iter()
        .filter(|(_, doc)| {
            doc.frontmatter.is_active() && doc.frontmatter.kind.as_deref() != Some("trajectory")
        })
        .map(|(id, doc)| {
            (
                id.clone(),
                doc.path.clone(),
                doc.frontmatter.tags.clone(),
                doc.frontmatter.filenames.clone(),
                doc.entities.clone(),
            )
        })
        .collect();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));

    let mut stats = RelinkStats {
        docs_scanned: candidates.len(),
        ..Default::default()
    };

    for (doc_id, path, tags, filenames, entities) in candidates {
        let similar = kg.find_similar_docs(&tags, &filenames, &entities);
        let suggested: Vec<String> = similar
            .into_iter()
            .filter(|(target_id, score)| {
                if *score < AUTO_LINK_MIN_SCORE || *target_id == doc_id {
                    return false;
                }
                if let Some(doc) = kg.docs.get(target_id) {
                    return doc.frontmatter.is_active()
                        && doc.frontmatter.kind.as_deref() != Some("trajectory");
                }
                false
            })
            .take(AUTO_LINK_MAX_LINKS)
            .map(|(id, _)| id)
            .collect();

        if suggested.is_empty() {
            continue;
        }

        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read memory file {}: {}", path.display(), e))?;
        let (mut fm, content_start) = KnowledgeFrontmatter::parse(&text);
        let body = text.get(content_start..).unwrap_or("").to_string();
        let self_id = fm
            .id
            .clone()
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        let mut added = 0usize;
        for link in suggested {
            if fm.links.len() >= AUTO_LINK_MAX_TOTAL {
                break;
            }
            if link != self_id && link != doc_id && !fm.links.contains(&link) {
                fm.links.push(link);
                added += 1;
            }
        }
        fm.links.retain(|link| link != &self_id && link != &doc_id);

        if added == 0 {
            continue;
        }

        rewrite_memory_document(&gcx, &path, &fm, &body).await?;
        stats.docs_updated += 1;
        stats.links_added += added;
    }

    tracing::info!(
        "auto_link_all_memories: scanned {} docs, updated {}, added {} links",
        stats.docs_scanned,
        stats.docs_updated,
        stats.links_added
    );
    Ok(stats)
}

pub async fn get_knowledge_root(gcx: &Arc<GlobalContext>) -> Result<PathBuf, ScratchError> {
    let workspace_folders = gcx.documents_state.workspace_folders.clone();
    let folders = workspace_folders.lock().unwrap();

    if folders.is_empty() {
        return Err(ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "No workspace folder configured".to_string(),
        ));
    }

    Ok(folders[0].join(KNOWLEDGE_FOLDER_NAME))
}

pub async fn validate_knowledge_path(
    file_path: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, ScratchError> {
    let root_metadata = tokio::fs::symlink_metadata(workspace_root)
        .await
        .map_err(|_| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot access workspace".to_string(),
            )
        })?;
    if root_metadata.file_type().is_symlink() {
        return Err(ScratchError::new(
            StatusCode::FORBIDDEN,
            "Knowledge directory cannot be a symlink".to_string(),
        ));
    }
    if !root_metadata.is_dir() {
        return Err(ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Knowledge directory is not a directory".to_string(),
        ));
    }

    let file_metadata = tokio::fs::symlink_metadata(file_path)
        .await
        .map_err(|_| ScratchError::new(StatusCode::NOT_FOUND, "File not found".to_string()))?;
    if file_metadata.file_type().is_symlink() {
        return Err(ScratchError::new(
            StatusCode::FORBIDDEN,
            "Memory file cannot be a symlink".to_string(),
        ));
    }
    if !file_metadata.is_file() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Memory path must be a file".to_string(),
        ));
    }

    let canonical = tokio::fs::canonicalize(file_path)
        .await
        .map(|path| dunce::simplified(&path).to_path_buf())
        .map_err(|_| ScratchError::new(StatusCode::NOT_FOUND, "File not found".to_string()))?;

    let root_canonical = tokio::fs::canonicalize(workspace_root).await.map_err(|_| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot access workspace".to_string(),
        )
    })?;
    let root_canonical = dunce::simplified(&root_canonical).to_path_buf();

    if !canonical.starts_with(&root_canonical) {
        return Err(ScratchError::new(
            StatusCode::FORBIDDEN,
            "Path outside knowledge directory".to_string(),
        ));
    }

    let ext = canonical.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "md" && ext != "mdx" {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Only .md and .mdx files allowed".to_string(),
        ));
    }

    Ok(canonical)
}

pub async fn handle_v1_knowledge_update_memory(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let post = serde_json::from_slice::<UpdateMemoryPost>(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("JSON problem: {}", e),
        )
    })?;

    let knowledge_root = get_knowledge_root(&gcx).await?;
    let file_path = validate_knowledge_path(Path::new(&post.file_path), &knowledge_root).await?;

    let existing_text = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read memory file: {}", e),
        )
    })?;

    let (mut frontmatter, content_start) = KnowledgeFrontmatter::parse(&existing_text);

    if let Some(title) = post.title {
        frontmatter.title = Some(sanitize_string(&title));
    }
    if let Some(tags) = post.tags {
        let tags = sanitize_and_dedupe_strings(tags);
        frontmatter.tags = normalize_memory_tags(&tags, 16);
    }
    if let Some(kind) = post.kind {
        let kind = sanitize_string(&kind);
        if !kind.is_empty() {
            frontmatter.kind = Some(kind);
        }
    }
    if let Some(filenames) = post.filenames {
        frontmatter.filenames = sanitize_and_dedupe_strings(filenames);
    }
    if let Some(links) = post.links {
        frontmatter.links = sanitize_and_dedupe_strings(links);
    }
    if let Some(status) = post.status {
        let status = sanitize_string(&status);
        let Some(status) = parse_memory_lifecycle_status(&status) else {
            return Err(ScratchError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "Invalid status '{}'. Must be one of: {}",
                    status,
                    VALID_STATUSES.join(", ")
                ),
            ));
        };
        apply_lifecycle_status(&mut frontmatter, &status);
    }
    frontmatter.updated = Some(Local::now().format("%Y-%m-%d").to_string());

    let existing_body = existing_text.get(content_start..).unwrap_or("").to_string();
    let content_to_write = post.content.unwrap_or(existing_body);

    let auto_link_enabled = post.auto_link.unwrap_or(true);
    if auto_link_enabled && frontmatter.is_active() {
        if let Err(e) =
            auto_link_memory(gcx.clone(), &mut frontmatter, &content_to_write, &file_path).await
        {
            tracing::warn!("Auto-linking failed: {}", e);
        }
    }

    rewrite_memory_document(&gcx, &file_path, &frontmatter, &content_to_write)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    tracing::info!("Updated memory: {}", file_path.display());

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_string(&MemoryOperationResponse {
                success: true,
                error: None,
            })
            .unwrap(),
        ))
        .unwrap())
}

pub async fn handle_v1_knowledge_relink_memories(
    State(app): State<AppState>,
    _body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let stats = auto_link_all_memories(gcx)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_string(&stats).unwrap()))
        .unwrap())
}


pub async fn handle_v1_knowledge_delete_memory(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    let post = serde_json::from_slice::<DeleteMemoryPost>(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("JSON problem: {}", e),
        )
    })?;

    let knowledge_root = get_knowledge_root(&gcx).await?;
    let file_path = validate_knowledge_path(Path::new(&post.file_path), &knowledge_root).await?;

    if post.archive {
        refact_runtime_api::memories::update_memory_document_frontmatter_for_ctx(
            &app.chat_ctx(),
            &file_path,
            |frontmatter| {
                if frontmatter.is_archived() || frontmatter.is_deprecated() {
                    return Ok(false);
                }
                apply_lifecycle_status(frontmatter, "archived");
                frontmatter.updated = Some(Local::now().format("%Y-%m-%d").to_string());
                Ok(true)
            },
        )
        .await
        .map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to archive memory: {}", e),
            )
        })?;
        tracing::info!("Archived memory: {}", file_path.display());
    } else {
        refact_runtime_api::memories_runtime::delete_document_from_disk(&gcx, &file_path)
            .await
            .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        tracing::info!("Deleted memory: {}", file_path.display());
    }

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_string(&MemoryOperationResponse {
                success: true,
                error: None,
            })
            .unwrap(),
        ))
        .unwrap())
}
