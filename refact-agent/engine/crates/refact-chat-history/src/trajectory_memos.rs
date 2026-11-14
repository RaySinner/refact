use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::fs;

pub const TRAJECTORIES_FOLDER: &str = ".refact/trajectories";
pub const MAX_MEMOS_PER_TRAJECTORY: usize = 10;
pub const ABANDONED_THRESHOLD_HOURS: i64 = 2;

pub fn is_buddy_system_trajectory(trajectory: &Value) -> bool {
    trajectory
        .get("buddy_meta")
        .and_then(|meta| {
            let is_buddy = meta
                .get("is_buddy_chat")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let kind = meta
                .get("buddy_chat_kind")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            (is_buddy && kind == "system").then_some(())
        })
        .is_some()
}

pub fn trajectory_title_is_generated(trajectory: &Value) -> bool {
    trajectory
        .get("extra")
        .and_then(|e| e.get("isTitleGenerated"))
        .or_else(|| trajectory.get("isTitleGenerated"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub async fn write_trajectory_json(path: &Path, trajectory: &Value) -> Result<(), String> {
    let tmp_path = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(trajectory).map_err(|e| e.to_string())?;
    fs::write(&tmp_path, &json)
        .await
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, path).await.map_err(|e| e.to_string())
}

pub async fn mark_trajectory_skipped(
    path: &Path,
    trajectory: &mut Value,
    reason: &str,
) -> Result<(), String> {
    let traj_obj = trajectory.as_object_mut().ok_or("Invalid trajectory")?;
    traj_obj.insert("memo_extracted".to_string(), Value::Bool(true));
    traj_obj.insert(
        "memo_extraction_skip_reason".to_string(),
        Value::String(reason.to_string()),
    );
    write_trajectory_json(path, trajectory).await
}

pub struct ExtractedMemo {
    pub memo_type: String,
    pub content: String,
}

pub struct TrajectoryMeta {
    pub overview: String,
    pub title: String,
}

pub struct ExtractionResult {
    pub meta: Option<TrajectoryMeta>,
    pub memos: Vec<ExtractedMemo>,
}

pub fn normalized_memo_title(title: &str) -> String {
    let collapsed = title
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let stripped = if let Some(rest) = collapsed.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            rest[end + 1..].trim_start()
        } else {
            collapsed.as_str()
        }
    } else {
        collapsed.as_str()
    };
    stripped.to_lowercase()
}

pub fn trajectory_project_root(path: &Path) -> Option<PathBuf> {
    let mut parent = path.parent();
    while let Some(dir) = parent {
        if dir.file_name().and_then(|name| name.to_str()) == Some("trajectories") {
            let refact_dir = dir.parent()?;
            if refact_dir.file_name().and_then(|name| name.to_str()) == Some(".refact") {
                return refact_dir.parent().map(Path::to_path_buf);
            }
            return None;
        }
        parent = dir.parent();
    }
    None
}

pub fn normalize_memo_type(memo_type: &str) -> String {
    match memo_type.trim().to_lowercase().replace('-', "_").as_str() {
        "pattern" | "tool_pattern" | "tool_patterns" => "pattern".to_string(),
        "preference" | "preferences" => "preference".to_string(),
        "decision" | "decisions" => "decision".to_string(),
        "bug" | "bugs" | "bug_fix" | "bug_fixed" => "bug".to_string(),
        "lesson" | "lessons" | "learning" => "lesson".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "lesson".to_string(),
    }
}

pub fn memo_kind(memo_type: &str) -> &'static str {
    match memo_type {
        "preference" => "preference",
        "decision" => "decision",
        "pattern" => "pattern",
        "bug" => "bug_fix",
        _ => "lesson",
    }
}

pub fn memo_confidence(memo_type: &str) -> f32 {
    match memo_type {
        "preference" => 0.78,
        "decision" => 0.74,
        "pattern" => 0.72,
        "bug" => 0.70,
        _ => 0.68,
    }
}

pub fn trajectory_message_range(idx: usize, total: usize) -> String {
    let total = total.max(1);
    format!("memo:{}-{}", idx, total - 1)
}

pub fn deterministic_candidate_id(trajectory_id: &str, idx: usize, content_hash: &str) -> String {
    format!(
        "memcand_traj_{}",
        candidate_hash(&[trajectory_id, &idx.to_string(), content_hash])
    )
}

pub fn candidate_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.len().to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}

pub fn parse_extraction_response(response_text: &str) -> ExtractionResult {
    let mut meta: Option<TrajectoryMeta> = None;
    let mut memos: Vec<ExtractedMemo> = Vec::new();

    for line in response_text.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }

        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let (Some(overview), Some(title)) = (
            parsed.get("overview").and_then(|v| v.as_str()),
            parsed.get("title").and_then(|v| v.as_str()),
        ) {
            meta = Some(TrajectoryMeta {
                overview: overview.to_string(),
                title: title.to_string(),
            });
            continue;
        }

        if let (Some(memo_type), Some(content)) = (
            parsed.get("type").and_then(|v| v.as_str()),
            parsed.get("content").and_then(|v| v.as_str()),
        ) {
            if memos.len() < MAX_MEMOS_PER_TRAJECTORY {
                memos.push(ExtractedMemo {
                    memo_type: memo_type.to_string(),
                    content: content.to_string(),
                });
            }
        }
    }

    ExtractionResult { meta, memos }
}
