use std::sync::Arc;

use crate::global_context::GlobalContext;
use refact_runtime_api::GlobalContextCtx;

pub use refact_runtime_api::trajectory_memos_runtime::{
    candidate_status, dedupe_memo_candidates, extract_memos_and_meta_once,
    load_existing_memo_docs, memory_candidates_from_extraction, memory_ops_from_candidates,
    persist_extraction_result, sanitize_candidate_text, CHECK_INTERVAL_SECS,
    MAX_TRAJECTORY_BYTES, SUBAGENT_ID,
};
use refact_chat_history::trajectory_memos::{
    is_buddy_system_trajectory, mark_trajectory_skipped, trajectory_title_is_generated,
    ExtractionResult, ABANDONED_THRESHOLD_HOURS, TRAJECTORIES_FOLDER,
};
use crate::call_validation::{ChatContent, ChatMessage};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use tokio::fs;
use tracing::warn;
use walkdir::WalkDir;

pub async fn trajectory_memos_background_task(gcx: Arc<GlobalContext>) {
    loop {
        let shutdown_flag = gcx.shutdown_flag.clone();
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(CHECK_INTERVAL_SECS)) => {}
            _ = async {
                while !shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                }
            } => {
                tracing::info!("Trajectory memos: shutdown detected, stopping");
                return;
            }
        }

        if let Err(e) = process_abandoned_trajectories(gcx.clone()).await {
            warn!("trajectory_memos: error processing trajectories: {}", e);
        }
    }
}

async fn process_abandoned_trajectories(gcx: Arc<GlobalContext>) -> Result<(), String> {
    let project_dirs = crate::files_correction::get_project_dirs(gcx.clone()).await;
    if project_dirs.is_empty() {
        return Ok(());
    }

    let now = Utc::now();
    let threshold = now - Duration::hours(ABANDONED_THRESHOLD_HOURS);

    for workspace_root in project_dirs {
        let trajectories_dir = workspace_root.join(TRAJECTORIES_FOLDER);
        if !trajectories_dir.exists() {
            continue;
        }

        for entry in WalkDir::new(&trajectories_dir)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() || path.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }

            match process_single_trajectory(gcx.clone(), path.to_path_buf(), &threshold).await {
                Ok(true) => tracing::info!("trajectory_memos: extracted memos from {}", path.display()),
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

async fn process_single_trajectory(
    gcx: Arc<GlobalContext>,
    path: std::path::PathBuf,
    threshold: &DateTime<Utc>,
) -> Result<bool, String> {
    if tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0) > MAX_TRAJECTORY_BYTES {
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
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
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

    let gcx2 = gcx.clone();
    let title2 = current_title.clone();
    let trajectory_id2 = trajectory_id.clone();
    let root_chat_id2 = root_chat_id.clone();
    let extraction = crate::buddy::workflows::buddy_wrap_workflow(
        crate::app_state::app_state_from_gcx(gcx.clone()).await,
        "memo_extraction",
        "🧠",
        8,
        |r: &ExtractionResult| format!("Memory extracted: {} memos", r.memos.len()),
        move || async move {
            extract_memos_and_meta(
                gcx2,
                chat_messages,
                &title2,
                is_title_generated,
                Some(trajectory_id2.as_str()),
                Some(root_chat_id2.as_str()),
            )
            .await
        },
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

async fn extract_memos_and_meta(
    gcx: Arc<GlobalContext>,
    messages: Vec<ChatMessage>,
    current_title: &str,
    is_title_generated: bool,
    parent_chat_id: Option<&str>,
    parent_root_chat_id: Option<&str>,
) -> Result<ExtractionResult, String> {
    extract_memos_and_meta_once(
        &gcx.chat_ctx(gcx.clone()),
        messages,
        current_title,
        is_title_generated,
        parent_chat_id,
        parent_root_chat_id,
    )
    .await
}

fn build_chat_messages(messages: &[Value]) -> Vec<ChatMessage> {
    let msgs: Vec<ChatMessage> = messages
        .iter()
        .filter_map(|msg| {
            let role = msg.get("role").and_then(|v| v.as_str())?;
            if role != "user" && role != "assistant" {
                return None;
            }

            let content = msg
                .get("content")
                .and_then(crate::chat::trajectories::extract_text_with_image_placeholders_from_json)?;

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
    match crate::chat::history_limit::validate_chat_history(&trimmed) {
        Ok(valid) => valid,
        Err(e) => {
            warn!("trajectory_memos: skipping invalid chat history: {}", e);
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_chat_history::trajectory_memos::{ExtractedMemo, TrajectoryMeta};
    use refact_runtime_api::trajectory_memos_runtime as runtime;

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

    fn trajectory_json(id: &str, messages: Vec<Value>) -> Value {
        serde_json::json!({
            "id": id,
            "title": "Untitled",
            "model": "test",
            "mode": "agent",
            "root_chat_id": id,
            "updated_at": "2026-05-01T00:00:00Z",
            "messages": messages
        })
    }

    fn enough_messages() -> Vec<Value> {
        (0..10)
            .map(|idx| {
                if idx % 2 == 0 {
                    serde_json::json!({"role": "user", "content": format!("Question {idx}")})
                } else {
                    serde_json::json!({"role": "assistant", "content": format!("Answer {idx}")})
                }
            })
            .collect()
    }

    #[test]
    fn trajectory_extraction_builds_proposed_candidate_ops_with_source_id() {
        let extraction = extraction_with_memo(
            "decision",
            "Use bounded queue persistence for trajectory memories in src/lib.rs.",
        );
        let candidates = runtime::memory_candidates_from_extraction(
            &extraction,
            "trajectory-a",
            "root-a",
            "Queue Persistence",
        );
        let ops = runtime::memory_ops_from_candidates(&candidates, stale_threshold());

        assert_eq!(ops.len(), 1);
        let op = &ops[0];
        assert_eq!(op.source, refact_runtime_api::buddy_memory_lifecycle::MemorySource::Trajectory);
        assert_eq!(
            op.op_type,
            refact_runtime_api::buddy_memory_lifecycle::MemoryOpType::CreateMemory
        );
        assert_eq!(
            op.status,
            refact_runtime_api::buddy_memory_lifecycle::MemoryOpStatus::Pending
        );
        assert!(!op.requires_approval);
        assert_eq!(
            op.payload.source_id.as_deref().unwrap().split(':').next(),
            Some("trajectory-a")
        );
        assert_eq!(op.payload.kind.as_deref(), Some("decision"));
        assert_eq!(op.payload.source_message_range.as_deref(), Some("memo:0-0"));
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

    #[tokio::test]
    async fn trajectory_json_persists_queued_memory_candidate_ops() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let trajectories_dir = root.join(TRAJECTORIES_FOLDER);
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        let path = trajectories_dir.join("trajectory-a.json");
        let mut trajectory = trajectory_json("trajectory-a", enough_messages());
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        let extraction =
            extraction_with_memo("lesson", "Use the memory ops queue for distilled lessons.");
        persist_extraction_result(
            &path,
            &mut trajectory,
            &extraction,
            "trajectory-a",
            "root-a",
            "Queue Persistence",
            false,
            stale_threshold(),
        )
        .await
        .unwrap();

        let state = crate::buddy::storage::load_memory_ops(root).await;
        assert_eq!(state.ops.len(), 1);
        assert_eq!(state.pending_count, 1);
        assert_eq!(
            state.ops[0].source,
            refact_runtime_api::buddy_memory_lifecycle::MemorySource::Trajectory
        );
        assert!(state.ops[0]
            .payload
            .source_id
            .as_deref()
            .unwrap()
            .starts_with("trajectory-a:memo:0-0:root-a:"));
        let updated: Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(updated["memo_extracted"].as_bool(), Some(true));
    }

    #[test]
    fn duplicate_trajectory_rerun_produces_same_idempotency_key() {
        let extraction = extraction_with_memo("pattern", "Use tokio::fs for async file writes.");
        let first = runtime::memory_ops_from_candidates(
            &runtime::memory_candidates_from_extraction(&extraction, "trajectory-a", "root-a", "Title"),
            stale_threshold(),
        );
        let second = runtime::memory_ops_from_candidates(
            &runtime::memory_candidates_from_extraction(&extraction, "trajectory-a", "root-a", "Title"),
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
            refact_chat_history::trajectory_memos::normalized_memo_title(" [decision]   Queue   Persistence \n Plan "),
            "queue persistence plan"
        );
        assert_eq!(
            refact_chat_history::trajectory_memos::normalized_memo_title("Queue Persistence"),
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
        let candidates = runtime::memory_candidates_from_extraction(
            &extraction,
            "trajectory-a",
            "root-a",
            "Queue Persistence",
        );
        use refact_runtime_api::buddy_memory_lifecycle::MemoryDocSnapshot;
        let existing = vec![MemoryDocSnapshot {
            title: "[decision] Queue Persistence".to_string(),
            content_hash: "different".to_string(),
            source_id: Some("trajectory-old:memo:0-0:root-a:hash".to_string()),
            ..Default::default()
        }];
        let deduped = runtime::dedupe_memo_candidates(candidates, &existing, "root-a");

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
        let candidates = runtime::memory_candidates_from_extraction(
            &extraction,
            "trajectory-a",
            "root-a",
            "Queue Persistence",
        );
        let deduped = runtime::dedupe_memo_candidates(candidates, &[], "other-root");

        assert_eq!(deduped.len(), 2);
        assert!(deduped
            .iter()
            .all(|candidate| refact_chat_history::trajectory_memos::normalized_memo_title(&candidate.title) == "queue persistence"));
    }

    #[tokio::test]
    async fn huge_and_malformed_trajectories_skip_safely() {
        let dir = tempfile::tempdir().unwrap();
        let huge = dir.path().join("huge.json");
        let malformed = dir.path().join("malformed.json");
        tokio::fs::write(
            &huge,
            format!(
                "{{\"pad\":\"{}\"}}",
                "x".repeat(MAX_TRAJECTORY_BYTES as usize + 1)
            ),
        )
        .await
        .unwrap();
        tokio::fs::write(&malformed, "{").await.unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;

        assert!(
            !process_single_trajectory(gcx.clone(), huge.clone(), &stale_threshold())
                .await
                .unwrap()
        );
        assert!(
            !process_single_trajectory(gcx, malformed.clone(), &stale_threshold())
                .await
                .unwrap()
        );
        assert!(!tokio::fs::read_to_string(huge)
            .await
            .unwrap()
            .contains("memo_extracted"));
        assert_eq!(tokio::fs::read_to_string(malformed).await.unwrap(), "{");
    }

    #[tokio::test]
    async fn buddy_system_trajectories_are_marked_skipped_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buddy.json");
        let mut trajectory = trajectory_json("buddy-chat", enough_messages());
        trajectory["buddy_meta"] = serde_json::json!({
            "is_buddy_chat": true,
            "buddy_chat_kind": "system",
            "workflow_id": "buddy_security_whisperer"
        });
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;

        assert!(
            !process_single_trajectory(gcx, path.clone(), &stale_threshold())
                .await
                .unwrap()
        );
        let updated: Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(updated["memo_extracted"].as_bool(), Some(true));
        assert_eq!(
            updated["memo_extraction_skip_reason"].as_str(),
            Some("skipped: buddy system trajectory")
        );
    }

    #[tokio::test]
    async fn memo_extracted_is_not_set_if_queueing_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.json");
        let mut trajectory = trajectory_json("chat", enough_messages());
        trajectory["overview"] = Value::String("overview".to_string());
        tokio::fs::write(&path, serde_json::to_string(&trajectory).unwrap())
            .await
            .unwrap();

        let extraction = extraction_with_memo("lesson", "Queue through memory ops.");
        let err = persist_extraction_result(
            &path,
            &mut trajectory,
            &extraction,
            "chat",
            "chat",
            "Chat",
            false,
            stale_threshold(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("trajectory path is not under a project .refact directory"));
        let unchanged: Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert!(unchanged.get("memo_extracted").is_none());
    }

    #[test]
    fn candidate_content_gets_normalized_tags_kind_review_after_status_proposed() {
        let extraction = extraction_with_memo(
            "preference",
            "User prefers concise answers and not token=secret in logs.",
        );
        let candidates = runtime::memory_candidates_from_extraction(&extraction, "chat", "chat", "Prefs");

        use refact_runtime_api::buddy_memory_lifecycle::MemoryCandidateStatus;
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
