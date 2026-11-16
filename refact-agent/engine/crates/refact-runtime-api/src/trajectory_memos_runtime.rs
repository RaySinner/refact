//! Trajectory-memo orchestration through the `ChatCtx` seams: the subagent
//! config comes from `mode_config`, the memo-extraction call from `subchat`,
//! and the persistence helpers from `refact-chat-history`. The buddy workflow
//! wrapper and the background loop stay in the engine (they need `AppState` /
//! `GlobalContext`); the work performed inside them lives here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use tokio::fs;
use tracing::{info, warn};
use walkdir::WalkDir;

use refact_buddy_core::storage::enqueue_memory_op;
use refact_chat_api::{ChatMessage, TraceParent};
use refact_core::chat_types::ChatContent;
use refact_chat_history::history_limit::validate_chat_history;
use refact_chat_history::trajectory_memos::{
    candidate_hash, deterministic_candidate_id, is_buddy_system_trajectory, mark_trajectory_skipped,
    memo_confidence, memo_kind, normalize_memo_type, normalized_memo_title,
    parse_extraction_response, trajectory_message_range, trajectory_project_root,
    trajectory_title_is_generated, write_trajectory_json, ExtractionResult,
    ABANDONED_THRESHOLD_HOURS, TRAJECTORIES_FOLDER,
};
use refact_core::memories_format::extract_file_paths;
use refact_core::string_utils::redact_sensitive;
use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
use refact_llm::logging::safe_truncate;

use crate::buddy_memory_lifecycle::{
    compute_content_hash, compute_idempotency_key, load_memory_doc_snapshots_from_knowledge_dirs,
    MemoryCandidate, MemoryCandidateStatus, MemoryDocSnapshot, MemoryLifecycleOp, MemoryOpType,
    MemorySource,
};
use crate::chat_ctx::ChatCtx;

pub fn extract_text_from_json_message(content_value: &Value) -> Option<String> {
    crate::trajectories::queries::extract_text_with_image_placeholders_from_json(content_value)
}


pub const CHECK_INTERVAL_SECS: u64 = 300;
pub const SUBAGENT_ID: &str = "memo_extraction";
pub const MAX_TRAJECTORY_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_MEMO_CONTENT_CHARS: usize = 4_000;
pub const MAX_MEMO_EVIDENCE_CHARS: usize = 1_000;

pub async fn process_abandoned_trajectories_once(ctx: &ChatCtx) -> Result<(), String> {
    let project_dirs = ctx.workspace.workspace_folders.lock().unwrap().clone();
    if project_dirs.is_empty() {
        return Ok(());
    }

    let now = Utc::now();
    let threshold = now - Duration::hours(ABANDONED_THRESHOLD_HOURS);

    for workspace_root in project_dirs {
        let trajectories_dir = workspace_root.join(TRAJECTORIES_FOLDER);

        for entry in WalkDir::new(&trajectories_dir)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() || path.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }

            match process_single_trajectory_once(ctx, path.to_path_buf(), &threshold).await {
                Ok(true) => info!("trajectory_memos: extracted memos from {}", path.display()),
                Ok(false) => {}
                Err(e) => warn!(
                    "trajectory_memos: failed to process {}: {}",
                    path.display(),
                    e
                ),
            }
        }
    }

    Ok(())
}

pub async fn process_single_trajectory_once(
    ctx: &ChatCtx,
    path: PathBuf,
    threshold: &DateTime<Utc>,
) -> Result<bool, String> {
    let metadata = fs::metadata(&path).await.map_err(|e| e.to_string())?;
    if metadata.len() > MAX_TRAJECTORY_BYTES {
        return Ok(false);
    }

    let content = fs::read_to_string(&path).await.map_err(|e| e.to_string())?;
    let mut trajectory: Value = match serde_json::from_str(&content) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };

    if trajectory
        .get("memo_extracted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(false);
    }

    if is_buddy_system_trajectory(&trajectory) {
        mark_trajectory_skipped(&path, &mut trajectory, "skipped: buddy system trajectory").await?;
        return Ok(false);
    }

    let updated_at = trajectory
        .get("updated_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let is_abandoned = match updated_at {
        Some(dt) => dt < *threshold,
        None => false,
    };

    if !is_abandoned {
        return Ok(false);
    }

    let messages = trajectory
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or("No messages")?;

    if messages.len() < 10 {
        return Ok(false);
    }

    let trajectory_id = trajectory
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let root_chat_id = trajectory
        .get("root_chat_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| trajectory_id.clone());
    let current_title = trajectory
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled")
        .to_string();

    let is_title_generated = trajectory_title_is_generated(&trajectory);

    let chat_messages = build_chat_messages(messages);
    if chat_messages.is_empty() {
        mark_trajectory_skipped(&path, &mut trajectory, "skipped: invalid chat history").await?;
        return Ok(false);
    }

    let extraction = extract_memos_and_meta_once(
        ctx,
        chat_messages,
        &current_title,
        is_title_generated,
        Some(trajectory_id.as_str()),
        Some(root_chat_id.as_str()),
    )
    .await?;

    persist_extraction_result(
        &path,
        &mut trajectory,
        &extraction,
        &trajectory_id,
        &root_chat_id,
        &current_title,
        is_title_generated,
        Utc::now(),
    )
    .await?;

    Ok(true)
}

pub async fn persist_extraction_result(
    path: &Path,
    trajectory: &mut Value,
    extraction: &ExtractionResult,
    trajectory_id: &str,
    root_chat_id: &str,
    current_title: &str,
    is_title_generated: bool,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let memo_title = extraction
        .meta
        .as_ref()
        .filter(|_| is_title_generated)
        .map(|m| m.title.clone())
        .unwrap_or_else(|| current_title.to_string());
    let mut candidates =
        memory_candidates_from_extraction(extraction, trajectory_id, root_chat_id, &memo_title);
    let existing_docs = load_existing_memo_docs(path).await;
    candidates = dedupe_memo_candidates(candidates, &existing_docs, root_chat_id);
    let ops = memory_ops_from_candidates(&candidates, now);
    if !ops.is_empty() {
        enqueue_trajectory_memory_ops(path, &ops).await?;
    }

    let traj_obj = trajectory.as_object_mut().ok_or("Invalid trajectory")?;
    if let Some(ref meta) = extraction.meta {
        traj_obj.insert("overview".to_string(), Value::String(meta.overview.clone()));
        if is_title_generated && !meta.title.is_empty() {
            traj_obj.insert("title".to_string(), Value::String(meta.title.clone()));
            info!(
                "trajectory_memos: updated title '{}' -> '{}' for {}",
                current_title, meta.title, trajectory_id
            );
        }
    }
    if ops.is_empty() {
        traj_obj.insert(
            "memo_extraction_skip_reason".to_string(),
            Value::String("skipped: no reusable memory candidates".to_string()),
        );
    }
    traj_obj.insert("memo_extracted".to_string(), Value::Bool(true));
    write_trajectory_json(path, trajectory).await
}

pub fn build_chat_messages(messages: &[Value]) -> Vec<ChatMessage> {
    let msgs: Vec<ChatMessage> = messages
        .iter()
        .filter_map(|msg| {
            let role = msg.get("role").and_then(|v| v.as_str())?;
            if role != "user" && role != "assistant" {
                return None;
            }

            let content = msg.get("content").and_then(extract_text_from_json_message)?;

            if content.trim().is_empty() {
                return None;
            }

            Some(ChatMessage {
                role: role.to_string(),
                content: ChatContent::SimpleText(content.chars().take(3000).collect()),
                ..Default::default()
            })
        })
        .collect();

    let start = msgs
        .iter()
        .position(|m| m.role == "user")
        .unwrap_or(msgs.len());
    let trimmed = msgs[start..].to_vec();
    match validate_chat_history(&trimmed) {
        Ok(valid) => valid,
        Err(e) => {
            warn!("trajectory_memos: skipping invalid chat history: {}", e);
            vec![]
        }
    }
}

pub async fn extract_memos_and_meta_once(
    ctx: &ChatCtx,
    mut messages: Vec<ChatMessage>,
    current_title: &str,
    is_title_generated: bool,
    parent_chat_id: Option<&str>,
    parent_root_chat_id: Option<&str>,
) -> Result<ExtractionResult, String> {
    let subagent_config = ctx
        .mode_config
        .get_subagent_config(SUBAGENT_ID, None)
        .await
        .ok_or_else(|| format!("subagent config '{}' not found", SUBAGENT_ID))?;

    let extraction_prompt = subagent_config
        .messages
        .user_template
        .as_ref()
        .ok_or_else(|| {
            format!(
                "messages.user_template not defined for subagent '{}'",
                SUBAGENT_ID
            )
        })?;

    let title_hint = if is_title_generated {
        format!("\n\nNote: The current title \"{}\" was auto-generated. Please provide a better descriptive title.", current_title)
    } else {
        String::new()
    };

    messages.push(ChatMessage {
        role: "user".to_string(),
        content: ChatContent::SimpleText(format!("{}{}", extraction_prompt, title_hint)),
        ..Default::default()
    });

    let result = ctx
        .subchat
        .run_subchat_once(
            SUBAGENT_ID,
            messages,
            TraceParent::from_parts(parent_chat_id, parent_root_chat_id),
        )
        .await
        .map_err(|e| e.to_string())?;

    let response_text = result
        .messages
        .last()
        .and_then(|m| match &m.content {
            ChatContent::SimpleText(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default();

    Ok(parse_extraction_response(&response_text))
}

pub fn memory_candidates_from_extraction(
    extraction: &ExtractionResult,
    trajectory_id: &str,
    root_chat_id: &str,
    memo_title: &str,
) -> Vec<MemoryCandidate> {
    let mut candidates = Vec::new();
    for (idx, memo) in extraction.memos.iter().enumerate() {
        let content = sanitize_candidate_text(
            &format!(
                "{}\n\n---\nSource: trajectory `{}`",
                memo.content, trajectory_id
            ),
            MAX_MEMO_CONTENT_CHARS,
        );
        if content.is_empty() {
            continue;
        }
        let memo_type = normalize_memo_type(&memo.memo_type);
        let mut tags = vec![
            memo_type.clone(),
            "trajectory".to_string(),
            "memory".to_string(),
        ];
        if memo_type == "preference" {
            tags.push("preference".to_string());
        }
        let related_files = extract_file_paths(&content);
        let content_hash = compute_content_hash(&content);
        let source_message_range = trajectory_message_range(idx, extraction.memos.len());
        let source_id = format!(
            "{}:{}:{}:{}",
            trajectory_id,
            source_message_range,
            root_chat_id,
            candidate_hash(&[trajectory_id, &source_message_range, &content_hash])
        );
        candidates.push(
            MemoryCandidate {
                candidate_id: deterministic_candidate_id(trajectory_id, idx, &content_hash),
                source: MemorySource::Trajectory,
                title: format!("[{}] {}", memo_type, memo_title),
                content,
                tags,
                kind: memo_kind(&memo_type).to_string(),
                related_files,
                source_id: Some(source_id),
                source_message_range: Some(source_message_range),
                confidence: memo_confidence(&memo_type),
                status: candidate_status(MemorySource::Trajectory, memo_confidence(&memo_type)),
                content_hash,
                review_after_days: 0,
                ..Default::default()
            }
            .normalized(),
        );
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| {
            seen.insert(compute_idempotency_key(
                &candidate.idempotency_input(MemoryOpType::CreateMemory),
            ))
        })
        .collect()
}

pub fn memory_ops_from_candidates(
    candidates: &[MemoryCandidate],
    now: DateTime<Utc>,
) -> Vec<MemoryLifecycleOp> {
    candidates
        .iter()
        .map(|candidate| {
            let evidence = sanitize_candidate_text(
                &format!(
                    "trajectory memo candidate source={} type={} confidence={:.2}",
                    candidate.source_id.as_deref().unwrap_or_default(),
                    candidate.kind,
                    candidate.confidence
                ),
                MAX_MEMO_EVIDENCE_CHARS,
            );
            candidate.clone().into_create_memory_op(
                candidate.candidate_id.clone(),
                evidence,
                now.to_rfc3339(),
            )
        })
        .collect()
}

pub async fn load_existing_memo_docs(path: &Path) -> Vec<MemoryDocSnapshot> {
    let Some(project_root) = trajectory_project_root(path) else {
        return Vec::new();
    };
    load_memory_doc_snapshots_from_knowledge_dirs(&[project_root.join(KNOWLEDGE_FOLDER_NAME)]).await
}

pub fn dedupe_memo_candidates(
    candidates: Vec<MemoryCandidate>,
    existing_docs: &[MemoryDocSnapshot],
    root_chat_id: &str,
) -> Vec<MemoryCandidate> {
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| {
            let title = normalized_memo_title(&candidate.title);
            if !seen.insert((title.clone(), candidate.content_hash.clone())) {
                return false;
            }
            !existing_docs.iter().any(|doc| {
                normalized_memo_title(&doc.title) == title
                    && (doc.content_hash == candidate.content_hash
                        || doc
                            .source_id
                            .as_deref()
                            .map(|source_id| source_id.contains(root_chat_id))
                            .unwrap_or(false))
            })
        })
        .collect()
}

pub async fn enqueue_trajectory_memory_ops(
    path: &Path,
    ops: &[MemoryLifecycleOp],
) -> Result<(), String> {
    let project_root = trajectory_project_root(path).ok_or_else(|| {
        format!(
            "trajectory path is not under a project .refact directory: {}",
            path.display()
        )
    })?;
    for op in ops {
        enqueue_memory_op(&project_root, op.clone()).await?;
    }
    Ok(())
}

pub fn candidate_status(source: MemorySource, confidence: f32) -> MemoryCandidateStatus {
    if !source.is_autonomous() && confidence >= 0.85 {
        MemoryCandidateStatus::Promoted
    } else {
        MemoryCandidateStatus::Proposed
    }
}

pub fn sanitize_candidate_text(text: &str, max_chars: usize) -> String {
    let redacted = redact_sensitive(text);
    let collapsed = redacted
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    safe_truncate(&collapsed, max_chars)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_chat_history::trajectory_memos::{ExtractedMemo, TrajectoryMeta};

    fn extraction_with_memo(memo_type: &str, content: &str) -> ExtractionResult {
        ExtractionResult {
            meta: Some(TrajectoryMeta {
                overview: "Implemented a fix".to_string(),
                title: "Fix Summary".to_string(),
            }),
            memos: vec![ExtractedMemo {
                memo_type: memo_type.to_string(),
                content: content.to_string(),
            }],
        }
    }

    fn stale_threshold() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn trajectory_extraction_builds_proposed_candidate_ops_with_source_id() {
        let extraction = extraction_with_memo(
            "decision",
            "Use bounded queue persistence for trajectory memories in src/lib.rs.",
        );
        let candidates = memory_candidates_from_extraction(
            &extraction,
            "trajectory-a",
            "root-a",
            "Queue Persistence",
        );
        let ops = memory_ops_from_candidates(&candidates, stale_threshold());

        assert_eq!(ops.len(), 1);
        let op = &ops[0];
        assert_eq!(op.source, MemorySource::Trajectory);
        assert_eq!(op.op_type, MemoryOpType::CreateMemory);
        assert!(!op.requires_approval);
        assert_eq!(
            op.payload.source_id.as_deref().unwrap().split(':').next(),
            Some("trajectory-a")
        );
        assert_eq!(op.payload.kind.as_deref(), Some("decision"));
        assert_eq!(
            op.payload.source_message_range.as_deref(),
            Some("memo:0-0")
        );
        assert_eq!(op.payload.review_after.as_deref(), Some("2026-06-01"));
        assert!(op
            .payload
            .tags
            .as_ref()
            .unwrap()
            .contains(&"trajectory".to_string()));
        assert!(op
            .payload
            .tags
            .as_ref()
            .unwrap()
            .contains(&"memory".to_string()));
        assert_eq!(
            op.payload.related_files.as_ref().unwrap(),
            &vec!["src/lib.rs".to_string()]
        );
    }

    #[test]
    fn duplicate_trajectory_rerun_produces_same_idempotency_key() {
        let extraction = extraction_with_memo("pattern", "Use tokio::fs for async file writes.");
        let first = memory_ops_from_candidates(
            &memory_candidates_from_extraction(&extraction, "trajectory-a", "root-a", "Title"),
            stale_threshold(),
        );
        let second = memory_ops_from_candidates(
            &memory_candidates_from_extraction(&extraction, "trajectory-a", "root-a", "Title"),
            stale_threshold(),
        );

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].idempotency_key, second[0].idempotency_key);
        assert_eq!(first[0].op_id, second[0].op_id);
    }

    #[test]
    fn normalized_memo_title_strips_type_prefix_and_whitespace() {
        assert_eq!(
            normalized_memo_title(" [decision]   Queue   Persistence \n Plan "),
            "queue persistence plan"
        );
        assert_eq!(
            normalized_memo_title("Queue Persistence"),
            "queue persistence"
        );
    }

    #[test]
    fn memo_title_dedupe_skips_existing_and_batch_duplicates() {
        let extraction = ExtractionResult {
            meta: None,
            memos: vec![
                ExtractedMemo {
                    memo_type: "decision".to_string(),
                    content: "Use bounded queues.".to_string(),
                },
                ExtractedMemo {
                    memo_type: "lesson".to_string(),
                    content: "Prefer bounded queues.".to_string(),
                },
                ExtractedMemo {
                    memo_type: "pattern".to_string(),
                    content: "Ship idempotent writes.".to_string(),
                },
            ],
        };
        let candidates = memory_candidates_from_extraction(
            &extraction,
            "trajectory-a",
            "root-a",
            "Queue Persistence",
        );
        let existing = vec![MemoryDocSnapshot {
            title: "[decision] Queue Persistence".to_string(),
            content_hash: "different".to_string(),
            source_id: Some("trajectory-old:memo:0-0:root-a:hash".to_string()),
            ..Default::default()
        }];
        let deduped = dedupe_memo_candidates(candidates, &existing, "root-a");

        assert_eq!(deduped.len(), 0);

        let extraction = ExtractionResult {
            meta: None,
            memos: vec![
                ExtractedMemo {
                    memo_type: "decision".to_string(),
                    content: "Use bounded queues.".to_string(),
                },
                ExtractedMemo {
                    memo_type: "lesson".to_string(),
                    content: "Use bounded queues.".to_string(),
                },
                ExtractedMemo {
                    memo_type: "pattern".to_string(),
                    content: "Prefer bounded queues.".to_string(),
                },
            ],
        };
        let candidates = memory_candidates_from_extraction(
            &extraction,
            "trajectory-a",
            "root-a",
            "Queue Persistence",
        );
        let deduped = dedupe_memo_candidates(candidates, &[], "other-root");

        assert_eq!(deduped.len(), 2);
        assert!(deduped
            .iter()
            .all(|candidate| normalized_memo_title(&candidate.title) == "queue persistence"));
    }

    #[test]
    fn candidate_content_gets_normalized_tags_kind_review_after_status_proposed() {
        let extraction = extraction_with_memo(
            "preference",
            "User prefers concise answers and not token=secret in logs.",
        );
        let candidates = memory_candidates_from_extraction(&extraction, "chat", "chat", "Prefs");

        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.status, MemoryCandidateStatus::Proposed);
        assert_eq!(candidate.kind, "preference");
        assert_eq!(candidate.review_after_days, 30);
        assert!(candidate.tags.contains(&"memory".to_string()));
        assert!(candidate.tags.contains(&"preference".to_string()));
        assert!(!candidate.content.contains("secret"));
    }
}
