use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

pub const WORKTREE_ROOT_ENV: &str = "REFACT_WORKTREE_ROOT";

static ROOT_LENGTH_WARNING_SHOWN: OnceLock<()> = OnceLock::new();

pub fn validate_worktree_root_override(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("REFACT_WORKTREE_ROOT must not be empty".to_string());
    }
    let length = value.chars().count();
    if length > 120 {
        return Err(format!(
            "REFACT_WORKTREE_ROOT '{}' is {} characters long, which would overflow the \
             Windows 32767-char rustc command line once sccache wraps it (the refact-lsp argv \
             grows ~176 chars per character of repo-root length). Use a short root such as \
             C:\\wt so sccache can share cache entries across worktrees.",
            value, length
        ));
    }
    if length > 90 && ROOT_LENGTH_WARNING_SHOWN.set(()).is_ok() {
        eprintln!(
            "warning: REFACT_WORKTREE_ROOT '{}' is {} characters long; above ~90 characters \
             sccache starts failing with os error 206 on Windows. A short root such as C:\\wt \
             is recommended.",
            value, length
        );
    }
    Ok(())
}

pub fn worktrees_base_dir(cache_dir: &Path) -> Result<PathBuf, String> {
    worktrees_base_dir_impl(std::env::var(WORKTREE_ROOT_ENV).ok().as_deref(), cache_dir)
}

pub fn worktrees_base_dir_impl(
    override_root: Option<&str>,
    cache_dir: &Path,
) -> Result<PathBuf, String> {
    match override_root {
        Some(value) => {
            validate_worktree_root_override(value)?;
            let root = PathBuf::from(value.trim());
            std::fs::create_dir_all(&root).map_err(|e| {
                format!(
                    "Failed to create REFACT_WORKTREE_ROOT '{}': {}",
                    root.display(),
                    e
                )
            })?;
            let probe = root.join(".refact-worktree-root-probe");
            std::fs::write(&probe, b"").map_err(|e| {
                format!(
                    "REFACT_WORKTREE_ROOT '{}' is not writable: {}",
                    root.display(),
                    e
                )
            })?;
            let _ = std::fs::remove_file(&probe);
            Ok(root)
        }
        None => Ok(cache_dir.join("worktrees")),
    }
}

use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AMutex;
use uuid::Uuid;

use crate::git;
use crate::types::{
    CreateWorktreeRequest, CreateWorktreeResponse, DeleteWorktreeResponse, MergeWorktreeRequest,
    MergeWorktreeResponse, OpenWorktreeResponse, WorktreeCleanupDeleted, WorktreeCleanupPlan,
    WorktreeCleanupRequest, WorktreeCleanupResult, WorktreeCleanupSkipped, WorktreeCleanupTarget,
    WorktreeConflictState, WorktreeDiffResponse, WorktreeInspection, WorktreeInventory,
    WorktreeInventorySummary, WorktreeListResponse, WorktreeMeta, WorktreeRecordView,
    WorktreeReference, WorktreeRegistry, WorktreeRegistryRecord, WorktreeRemovalResult,
    WorktreeStatus,
};

const DEFAULT_MAX_PATCH_BYTES: usize = 200_000;

static REGISTRY_WRITE_LOCK: OnceLock<AMutex<()>> = OnceLock::new();
static MERGE_LOCK: OnceLock<AMutex<()>> = OnceLock::new();

fn registry_write_lock() -> &'static AMutex<()> {
    REGISTRY_WRITE_LOCK.get_or_init(|| AMutex::new(()))
}

pub fn worktree_merge_lock() -> &'static AMutex<()> {
    MERGE_LOCK.get_or_init(|| AMutex::new(()))
}

#[derive(Debug, Clone)]
pub struct WorktreeService {
    worktrees_base: PathBuf,
    source_workspace_root: PathBuf,
    project_hash: String,
}

pub fn project_hash_for_path(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    hash.chars().take(16).collect()
}

pub fn validate_worktree_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("Worktree ID cannot be empty".to_string());
    }
    if id.len() > 128 {
        return Err("Worktree ID is too long".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Worktree ID must contain only ASCII alphanumeric characters, hyphens, or underscores"
                .to_string(),
        );
    }
    Ok(())
}

pub fn validate_branch_name(branch: &str) -> Result<(), String> {
    if branch.is_empty() {
        return Err("Branch name cannot be empty".to_string());
    }
    if branch.len() > 240 {
        return Err("Branch name is too long".to_string());
    }
    if branch.starts_with('/') || branch.ends_with('/') || branch.starts_with('-') {
        return Err(format!("Invalid branch name '{}'", branch));
    }
    if branch.contains("..")
        || branch.contains("//")
        || branch.contains("@{")
        || branch.ends_with('.')
        || branch.ends_with(".lock")
    {
        return Err(format!("Invalid branch name '{}'", branch));
    }
    if branch.chars().any(|c| {
        c.is_control() || c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    }) {
        return Err(format!("Invalid branch name '{}'", branch));
    }
    Ok(())
}

fn validate_kind(kind: &str) -> Result<String, String> {
    if kind.is_empty() || kind.len() > 64 {
        return Err("Worktree kind must be 1-64 characters".to_string());
    }
    if !kind
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Worktree kind contains invalid characters".to_string());
    }
    Ok(kind.to_string())
}

async fn validate_registry_root_async(
    registry_dir: &Path,
    id: &str,
    root: &Path,
) -> Result<(), String> {
    let registry_dir = normalize_lexical(registry_dir)?;
    let root = normalize_lexical(root)?;
    let expected = normalize_lexical(&registry_dir.join(id))?;
    if root != expected {
        return Err(format!(
            "Worktree root '{}' must exactly match registry path '{}'",
            root.display(),
            expected.display()
        ));
    }
    if let Ok(metadata) = tokio::fs::symlink_metadata(&expected).await {
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Worktree root '{}' must not be a symlink",
                expected.display()
            ));
        }
    }
    Ok(())
}

fn canonicalize_existing_dir(path: &Path) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(path).map_err(|e| {
        format!(
            "Failed to resolve source workspace root '{}': {}",
            path.display(),
            e
        )
    })?;
    if !canonical.is_dir() {
        return Err(format!(
            "Source workspace root '{}' is not a directory",
            canonical.display()
        ));
    }
    Ok(dunce::simplified(&canonical).to_path_buf())
}

async fn canonicalize_existing_dir_async(path: &Path) -> Result<PathBuf, String> {
    let canonical = tokio::fs::canonicalize(path).await.map_err(|e| {
        format!(
            "Failed to resolve source workspace root '{}': {}",
            path.display(),
            e
        )
    })?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|e| format!("Failed to inspect source workspace root: {}", e))?;
    if !metadata.is_dir() {
        return Err(format!(
            "Source workspace root '{}' is not a directory",
            canonical.display()
        ));
    }
    Ok(dunce::simplified(&canonical).to_path_buf())
}

fn normalize_existing_or_parent(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return dunce::simplified(&canonical).to_path_buf();
    }
    let Some(parent) = path.parent() else {
        return normalize_lexical(path).unwrap_or_else(|_| path.to_path_buf());
    };
    let Some(file_name) = path.file_name() else {
        return normalize_lexical(path).unwrap_or_else(|_| path.to_path_buf());
    };
    match std::fs::canonicalize(parent) {
        Ok(canonical_parent) => dunce::simplified(&canonical_parent).join(file_name),
        Err(_) => normalize_lexical(path).unwrap_or_else(|_| path.to_path_buf()),
    }
}

async fn normalize_existing_or_parent_async(path: &Path) -> PathBuf {
    if let Ok(canonical) = tokio::fs::canonicalize(path).await {
        return dunce::simplified(&canonical).to_path_buf();
    }
    let Some(parent) = path.parent() else {
        return normalize_lexical(path).unwrap_or_else(|_| path.to_path_buf());
    };
    let Some(file_name) = path.file_name() else {
        return normalize_lexical(path).unwrap_or_else(|_| path.to_path_buf());
    };
    match tokio::fs::canonicalize(parent).await {
        Ok(canonical_parent) => dunce::simplified(&canonical_parent).join(file_name),
        Err(_) => normalize_lexical(path).unwrap_or_else(|_| path.to_path_buf()),
    }
}

fn normalized_path_key(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path)
        .map(|path| dunce::simplified(&path).to_path_buf())
        .or_else(|_| normalize_lexical(path).map(|path| dunce::simplified(&path).to_path_buf()))
}

async fn normalized_path_key_async(path: &Path) -> Result<PathBuf, String> {
    match tokio::fs::canonicalize(path).await {
        Ok(path) => Ok(dunce::simplified(&path).to_path_buf()),
        Err(_) => normalize_lexical(path).map(|path| dunce::simplified(&path).to_path_buf()),
    }
}

fn normalize_lexical(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().map_err(|e| format!("Failed to read current dir: {}", e))?
    };
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalized)
}

fn sanitize_branch_component(value: &str) -> String {
    let component = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if component.is_empty() {
        "chat".to_string()
    } else {
        component.chars().take(32).collect()
    }
}

fn default_branch_name(kind: &str, chat_id: Option<&str>) -> String {
    let seed = chat_id
        .map(sanitize_branch_component)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let short = seed.chars().take(12).collect::<String>();
    format!("refact/{}/{}", sanitize_branch_component(kind), short)
}

fn request_to_reference(kind: &str, request: &CreateWorktreeRequest) -> Option<WorktreeReference> {
    let reference = WorktreeReference {
        kind: kind.to_string(),
        chat_id: request.chat_id.clone(),
        task_id: request.task_id.clone(),
        card_id: request.card_id.clone(),
        agent_id: request.agent_id.clone(),
    };
    reference.has_identity().then_some(reference)
}

fn reference_key(reference: &WorktreeReference) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        reference.kind,
        reference.chat_id.as_deref().unwrap_or_default(),
        reference.task_id.as_deref().unwrap_or_default(),
        reference.card_id.as_deref().unwrap_or_default(),
        reference.agent_id.as_deref().unwrap_or_default()
    )
}

fn canonical_git_path(root: &Path, git_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(git_path.trim());
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    path.canonicalize()
        .map_err(|e| format!("Failed to canonicalize git path: {}", e))
}

fn validate_registered_record_checkout(record: &WorktreeRegistryRecord) -> Result<(), String> {
    validate_worktree_meta_checkout(&record.meta)
}

fn validate_registered_record_diff(record: &WorktreeRegistryRecord) -> Result<(), String> {
    validate_worktree_meta_repository(&record.meta).map(|_| ())
}

fn validate_worktree_meta_checkout(meta: &WorktreeMeta) -> Result<(), String> {
    let registered_root = validate_worktree_meta_repository(meta)?;
    let source_branch = meta
        .branch
        .clone()
        .ok_or_else(|| format!("Worktree '{}' has no source branch", meta.id))?;
    git::ensure_checkout_on_branch(&registered_root, "Source worktree", &source_branch)
}

fn validate_worktree_meta_repository(meta: &WorktreeMeta) -> Result<PathBuf, String> {
    if !meta.root.exists() {
        return Err(format!(
            "Worktree '{}' path '{}' does not exist",
            meta.id,
            meta.root.display()
        ));
    }
    let registered_root = meta
        .root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize worktree root: {}", e))?;
    let top_level = git::run_git(&registered_root, &["rev-parse", "--show-toplevel"])?;
    let top_level = canonical_git_path(&registered_root, &top_level)?;
    if top_level != registered_root {
        return Err(format!(
            "Worktree '{}' path '{}' is not the git worktree root",
            meta.id,
            meta.root.display()
        ));
    }
    let source_common_dir = git::run_git(
        &meta.source_workspace_root,
        &["rev-parse", "--git-common-dir"],
    )?;
    let registered_common_dir = git::run_git(&registered_root, &["rev-parse", "--git-common-dir"])?;
    if canonical_git_path(&meta.source_workspace_root, &source_common_dir)?
        != canonical_git_path(&registered_root, &registered_common_dir)?
    {
        return Err(format!(
            "Worktree '{}' path '{}' belongs to a different repository",
            meta.id,
            meta.root.display()
        ));
    }
    Ok(registered_root)
}

fn fallback_merge_message(record: &WorktreeRegistryRecord, branch: &str) -> String {
    if let Some(card_id) = record.meta.card_id.as_deref() {
        format!("Merge worktree {} for card {}", branch, card_id)
    } else if let Some(task_id) = record.meta.task_id.as_deref() {
        format!("Merge worktree {} for task {}", branch, task_id)
    } else {
        format!("Merge worktree {}", branch)
    }
}

fn restore_target_stash(
    source_root: &Path,
    stash: &git::DirtyWorktreeStash,
    warnings: &mut Vec<String>,
) -> bool {
    let restore_result = git::restore_stashed_changes(source_root, stash);
    let restored = restore_result.critical_ok
        && git::conflict_files_for_path(source_root).is_empty()
        && !git::has_merge_in_progress(source_root);
    warnings.extend(restore_result.warnings);
    restored
}

fn post_cleanup_conflict_state(
    source_root: &Path,
    original_files: Vec<String>,
    cleanup_ok: bool,
) -> WorktreeConflictState {
    let current_files = git::conflict_files_for_path(source_root);
    let merge_in_progress = git::has_merge_in_progress(source_root);
    let current_files_empty = current_files.is_empty();
    let files = if current_files.is_empty() {
        original_files
    } else {
        current_files
    };
    conflict_state(
        files,
        cleanup_ok && !merge_in_progress && current_files_empty,
        merge_in_progress,
    )
}

fn format_merge_failure(prefix: &str, error: &str, cleanup_warnings: Vec<String>) -> String {
    if cleanup_warnings.is_empty() {
        format!("{}: {}", prefix, error)
    } else {
        format!(
            "{}: {}; cleanup warnings: {}",
            prefix,
            error,
            cleanup_warnings.join("; ")
        )
    }
}

fn conflict_state(
    files: Vec<String>,
    aborted: bool,
    merge_in_progress: bool,
) -> WorktreeConflictState {
    let instructions = if aborted {
        "Merge conflicts were detected during preflight or the merge was aborted and cleaned up; no conflict markers were left behind. Merge the source branch changes against the target branch yourself, then retry."
    } else if merge_in_progress {
        "Merge conflicts remain in the target workspace; resolve or abort the merge before retrying."
    } else {
        "The merge was stopped, but cleanup warnings were reported. Inspect the target workspace and warnings before retrying."
    };
    WorktreeConflictState {
        files,
        aborted,
        merge_in_progress,
        instructions: instructions.to_string(),
    }
}

fn validate_cleanup_request(request: &WorktreeCleanupRequest) -> Result<(), String> {
    if request.ids.is_empty() {
        return Err("Cleanup requires explicit worktree ids".to_string());
    }
    for id in &request.ids {
        validate_worktree_id(id)?;
    }
    Ok(())
}

fn cleanup_blockers_for_item(
    item: &WorktreeInspection,
    request: &WorktreeCleanupRequest,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if !item.status.path_exists {
        blockers.push("missing_path".to_string());
    }
    if !item.status.is_git_worktree {
        blockers.push("not_git_worktree".to_string());
    }
    if item.conflicted {
        blockers.push("conflicted".to_string());
    }
    if item.base_commit.is_none() && item.base_branch.is_none() {
        blockers.push("base_unknown".to_string());
    }
    if item.status.error.is_some() {
        blockers.push("diff_error".to_string());
    }
    if request.clean_only && (item.changed_files > 0 || item.status.dirty) {
        blockers.push("dirty".to_string());
    }
    if !request.allow_shared && item.shared {
        blockers.push("shared".to_string());
    }
    if item.reference_count > 0 {
        blockers.push("referenced".to_string());
    }
    match item.age_hours {
        Some(age) if age < request.min_age_hours => blockers.push("too_recent".to_string()),
        None if request.min_age_hours > 0 => blockers.push("age_unknown".to_string()),
        _ => {}
    }
    if request.delete_branches {
        match (&item.branch, item.branch_merged) {
            (Some(_), Some(true)) => {}
            (Some(_), Some(false)) => blockers.push("branch_not_merged".to_string()),
            (Some(_), None) => blockers.push("branch_safety_unknown".to_string()),
            (None, _) => {}
        }
    }
    blockers
}

fn summarize_inventory(worktrees: &[WorktreeInspection]) -> WorktreeInventorySummary {
    let mut summary = WorktreeInventorySummary::default();
    let mut disk_usage = 0u64;
    let mut has_disk_usage = false;
    for item in worktrees {
        if item.source == "registered" {
            summary.total_registered += 1;
        } else {
            summary.total_discovered += 1;
        }
        if item.stale {
            summary.stale += 1;
        }
        if item.conflicted {
            summary.conflicted += 1;
        }
        if item.shared {
            summary.shared += 1;
        }
        let unknown = item.status.error.is_some()
            || (item.base_commit.is_none() && item.base_branch.is_none());
        if unknown {
            summary.unknown += 1;
        }
        if item.changed_files > 0 || item.status.dirty {
            summary.dirty += 1;
        } else if !item.stale && !item.conflicted && !unknown {
            summary.clean += 1;
        }
        if item.cleanup_candidate {
            summary.abandoned_clean += 1;
        }
        if item.source == "registered" && !item.status.path_exists {
            summary.missing_registry_paths += 1;
        }
        if item.cache_dir_missing_from_registry {
            summary.unregistered_cache_dirs += 1;
        }
        if item.branch_merged == Some(true) {
            summary.merged_branches += 1;
        }
        summary.changed_files += item.changed_files;
        summary.additions += item.additions;
        summary.deletions += item.deletions;
        if let Some(bytes) = item.disk_usage_bytes {
            has_disk_usage = true;
            disk_usage = disk_usage.saturating_add(bytes);
        }
        if let Some(age) = item.age_hours {
            summary.newest_age_hours = Some(
                summary
                    .newest_age_hours
                    .map(|current| current.min(age))
                    .unwrap_or(age),
            );
            summary.oldest_age_hours = Some(
                summary
                    .oldest_age_hours
                    .map(|current| current.max(age))
                    .unwrap_or(age),
            );
        }
    }
    summary.total = worktrees.len();
    if has_disk_usage {
        summary.disk_usage_bytes = Some(disk_usage);
    }
    summary
}

fn worktree_id_from_path(path: &Path) -> String {
    if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
        if validate_worktree_id(name).is_ok() {
            return name.to_string();
        }
    }
    format!("discovered_{}", project_hash_for_path(path))
}

fn age_hours_since(timestamp: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|dt| Utc::now().signed_duration_since(dt.with_timezone(&Utc)))
        .map(|duration| duration.num_hours().max(0) as u64)
}

fn filesystem_age_hours(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    std::time::SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|duration| duration.as_secs() / 3600)
}

fn cheap_disk_usage(path: &Path) -> Option<u64> {
    if !path.exists() {
        return None;
    }
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(path) = stack.pop() {
        seen += 1;
        if seen > 5000 {
            return None;
        }
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(&path).ok()? {
                stack.push(entry.ok()?.path());
            }
        }
    }
    Some(total)
}

fn attached_chat_ids(references: &[WorktreeReference]) -> Vec<String> {
    let mut ids = references
        .iter()
        .filter_map(|reference| reference.chat_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn attached_task_ids(references: &[WorktreeReference]) -> Vec<String> {
    let mut ids = references
        .iter()
        .filter_map(|reference| reference.task_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

mod cleanup;
mod create;
mod merge;
mod registry;
mod remove;
mod validation;

#[cfg(test)]
mod worktree_registry_tests {
    include!("tests_registry.rs");
    include!("tests_merge.rs");
}
