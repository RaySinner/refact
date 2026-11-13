use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{AbVariantInfo, BoardCard};
use crate::tools::task_tool_helpers::{required_string, require_bound_planner_task};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_worktrees::service::WorktreeService;
use refact_worktrees::types::{WorktreeRecordView, WorktreeReference};

const DEFAULT_MAX_LINES: usize = 300;
pub const GIT_DIFF_TIMEOUT: Duration = Duration::from_secs(10);
const UNTRACKED_PER_FILE_CAP_BYTES: usize = 64 * 1024;
const UNTRACKED_TOTAL_CAP_BYTES: usize = 256 * 1024;
const UNTRACKED_BINARY_PROBE_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDiffMode {
    Stat,
    Unified,
    NameOnly,
}

impl AgentDiffMode {
    fn parse(value: Option<&Value>) -> Result<Self, String> {
        match value.and_then(|value| value.as_str()).unwrap_or("stat") {
            "stat" => Ok(Self::Stat),
            "unified" => Ok(Self::Unified),
            "name-only" => Ok(Self::NameOnly),
            other => Err(format!("Invalid mode: {}", other)),
        }
    }
}

pub struct ToolAgentDiff;

impl ToolAgentDiff {
    pub fn new() -> Self {
        Self
    }
}

fn parse_card_variant_id(input: &str) -> (&str, Option<&str>) {
    if let Some((card_id, variant)) = input.split_once('/') {
        return (card_id, Some(variant));
    }
    if let Some((card_id, variant)) = input.split_once(':') {
        return (card_id, Some(variant));
    }
    (input, None)
}

fn variant_key_from_request(value: Option<&str>) -> Result<Option<&'static str>, String> {
    match value {
        Some("a") | Some("A") => Ok(Some("a")),
        Some("b") | Some("B") => Ok(Some("b")),
        Some(other) => Err(format!(
            "Invalid A/B variant '{}', expected 'a' or 'b'",
            other
        )),
        None => Ok(None),
    }
}

fn card_for_diff_target(card: &BoardCard, requested_id: &str) -> Result<BoardCard, String> {
    let (_, requested_variant) = parse_card_variant_id(requested_id);
    let Some(variant_key) = variant_key_from_request(requested_variant)? else {
        return Ok(card.clone());
    };
    let variants = card
        .ab_variants
        .as_ref()
        .ok_or_else(|| format!("Card {} has no A/B variants", card.id))?;
    let variant = variants
        .variant(variant_key)
        .ok_or_else(|| format!("A/B variant {} not found", variant_key))?;
    Ok(card_with_variant_agent(card, variant_key, variant))
}

fn card_with_variant_agent(
    card: &BoardCard,
    variant_key: &str,
    variant: &AbVariantInfo,
) -> BoardCard {
    let mut card = card.clone();
    card.agent_chat_id = Some(variant.chat_id.clone());
    card.assignee = Some(variant.agent_id.clone());
    card.agent_worktree = Some(variant.worktree.clone());
    card.agent_worktree_name = variant.worktree_name.clone();
    card.agent_branch = variant.branch.clone();
    card.title = format!("{} [A/B {}]", card.title, variant_key.to_ascii_uppercase());
    card
}

fn parse_max_lines(args: &HashMap<String, Value>) -> Result<usize, String> {
    let Some(value) = args.get("max_lines") else {
        return Ok(DEFAULT_MAX_LINES);
    };
    if value.is_null() {
        return Ok(DEFAULT_MAX_LINES);
    }
    let Some(n) = value.as_u64() else {
        return Err("max_lines must be a non-negative number".to_string());
    };
    usize::try_from(n).map_err(|_| "max_lines is too large".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffBase {
    pub refish: String,
    pub label: String,
}

fn present(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn resolve_base(
    worktree_commit: Option<String>,
    worktree_branch: Option<String>,
    card_commit: Option<String>,
    card_branch: Option<String>,
    task_meta_commit: Option<String>,
    task_meta_branch: Option<String>,
) -> Result<DiffBase, String> {
    let card_pair_present =
        present(card_commit.clone()).is_some() || present(card_branch.clone()).is_some();
    let worktree_pair_present =
        present(worktree_commit.clone()).is_some() || present(worktree_branch.clone()).is_some();
    let (commit, branch) = if card_pair_present {
        (present(card_commit), present(card_branch))
    } else if worktree_pair_present {
        (present(worktree_commit), present(worktree_branch))
    } else {
        (present(task_meta_commit), present(task_meta_branch))
    };
    if let Some(commit) = commit {
        return Ok(DiffBase {
            refish: commit.clone(),
            label: format!("commit {}", commit),
        });
    }
    if let Some(branch) = branch {
        return Ok(DiffBase {
            refish: branch.clone(),
            label: format!("branch {}", branch),
        });
    }
    Err("Task has no base commit or base branch set".to_string())
}

fn reference_matches_card(task_id: &str, card: &BoardCard, reference: &WorktreeReference) -> bool {
    if reference.task_id.as_deref() == Some(task_id)
        && reference.card_id.as_deref() == Some(card.id.as_str())
    {
        return true;
    }
    if let Some(agent_chat_id) = card.agent_chat_id.as_deref() {
        if reference.chat_id.as_deref() == Some(agent_chat_id) {
            return true;
        }
    }
    card.assignee
        .as_deref()
        .map(|assignee| {
            reference.task_id.as_deref() == Some(task_id)
                && reference.agent_id.as_deref() == Some(assignee)
        })
        .unwrap_or(false)
}

fn registered_record_matches_card(
    view: &WorktreeRecordView,
    task_id: &str,
    card: &BoardCard,
) -> bool {
    if view.meta.task_id.as_deref() == Some(task_id)
        && view.meta.card_id.as_deref() == Some(card.id.as_str())
    {
        return true;
    }
    view.references
        .iter()
        .any(|reference| reference_matches_card(task_id, card, reference))
}

fn registered_error_allows_legacy_fallback(err: &str) -> bool {
    !err.contains("does not match task")
}

async fn base_from_worktree_meta(
    ctx: &refact_runtime_api::ChatCtx,
    task_id: &str,
    card: &BoardCard,
) -> (Option<String>, Option<String>) {
    let project_dirs = refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    for source_root in project_dirs {
        let Ok(service) = WorktreeService::new_async(ctx.paths.cache_dir.clone(), source_root).await
        else {
            continue;
        };
        let Ok(registry) = service.load_registry().await else {
            continue;
        };
        if let Some(record) = registry.records.iter().find(|record| {
            card.agent_worktree_name.as_deref() == Some(record.meta.id.as_str())
                || (record.meta.task_id.as_deref() == Some(task_id)
                    && record.meta.card_id.as_deref() == Some(card.id.as_str()))
                || record
                    .references
                    .iter()
                    .any(|reference| reference_matches_card(task_id, card, reference))
        }) {
            if record.meta.root.exists() {
                return (
                    record.meta.base_commit.clone(),
                    record.meta.base_branch.clone(),
                );
            }
        }
    }
    (None, None)
}

fn canonical_existing_path(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path)
        .map_err(|e| format!("Failed to canonicalize '{}': {}", path.display(), e))
}

fn validated_cache_root(cache_dir: &Path) -> Result<PathBuf, String> {
    if !cache_dir.exists() {
        std::fs::create_dir_all(cache_dir).map_err(|e| {
            format!(
                "Failed to create cache directory '{}': {}",
                cache_dir.display(),
                e
            )
        })?;
    }
    let cache_root = refact_worktrees::service::worktrees_base_dir(cache_dir)
        .unwrap_or_else(|_| cache_dir.join("worktrees"));
    if !cache_root.exists() {
        std::fs::create_dir_all(&cache_root).map_err(|e| {
            format!(
                "Failed to create worktree cache '{}': {}",
                cache_root.display(),
                e
            )
        })?;
    }
    canonical_existing_path(&cache_root)
}

fn validate_fallback_worktree_path(
    cache_dir: &Path,
    workspace_root: &Path,
    card_id: &str,
    worktree: &Path,
    branch: &str,
) -> Result<PathBuf, String> {
    if !worktree.exists() {
        return Err(format!(
            "Agent worktree '{}' for card {} does not exist",
            worktree.display(),
            card_id
        ));
    }
    let worktree = canonical_existing_path(worktree)?;
    let cache_root = validated_cache_root(cache_dir)?;
    let cache_dir = canonical_existing_path(cache_dir)?;
    if worktree == Path::new("/") || worktree == cache_dir || worktree == cache_root {
        return Err(format!(
            "Refusing to use unsafe agent worktree path '{}' for card {}.",
            worktree.display(),
            card_id
        ));
    }
    if !worktree.starts_with(&cache_root) {
        return Err(format!(
            "Agent worktree '{}' for card {} is outside worktree cache '{}'.",
            worktree.display(),
            card_id,
            cache_root.display()
        ));
    }
    let status = refact_worktrees::git::status_for_path(&worktree);
    if !status.is_git_worktree {
        return Err(format!(
            "Agent worktree '{}' for card {} is not a git worktree.",
            worktree.display(),
            card_id
        ));
    }
    let top_level = refact_worktrees::git::run_git(&worktree, &["rev-parse", "--show-toplevel"])?;
    let top_level = canonical_existing_path(Path::new(top_level.trim()))?;
    if top_level != worktree {
        return Err(format!(
            "Agent worktree '{}' for card {} is not the git worktree root.",
            worktree.display(),
            card_id
        ));
    }
    let workspace_root = canonical_existing_path(workspace_root)?;
    let common_dir = |root: &Path| -> Result<PathBuf, String> {
        let output = refact_worktrees::git::run_git(root, &["rev-parse", "--git-common-dir"])?;
        let path = Path::new(output.trim());
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        canonical_existing_path(&path)
    };
    if common_dir(&workspace_root)? != common_dir(&worktree)? {
        return Err(format!(
            "Agent worktree '{}' for card {} belongs to a different repository.",
            worktree.display(),
            card_id
        ));
    }
    refact_worktrees::git::ensure_checkout_on_branch(&worktree, "Agent worktree", branch)?;
    Ok(worktree)
}

async fn primary_workspace_root(ctx: &refact_runtime_api::ChatCtx) -> Result<PathBuf, String> {
    let project_dirs = refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    let workspace_root = project_dirs.first().ok_or("No workspace folder found")?;
    canonical_existing_path(workspace_root)
}

fn legacy_worktree_path(
    cache_dir: &Path,
    workspace_root: &Path,
    card: &BoardCard,
) -> Result<PathBuf, String> {
    let worktree = card
        .agent_worktree
        .as_ref()
        .ok_or_else(|| format!("Card {} has no agent worktree", card.id))?;
    let branch = card
        .agent_branch
        .as_ref()
        .ok_or_else(|| format!("Card {} has no agent branch", card.id))?;
    validate_fallback_worktree_path(
        cache_dir,
        workspace_root,
        &card.id,
        Path::new(worktree),
        branch,
    )
}

async fn registered_worktree_path(
    ctx: &refact_runtime_api::ChatCtx,
    task_id: &str,
    card: &BoardCard,
    worktree_name: &str,
) -> Result<PathBuf, String> {
    let project_dirs = refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    for source_root in project_dirs {
        let Ok(service) = WorktreeService::new_async(ctx.paths.cache_dir.clone(), source_root).await
        else {
            continue;
        };
        let Ok(view) = service.get_worktree(worktree_name).await else {
            continue;
        };
        if !registered_record_matches_card(&view, task_id, card) {
            return Err(format!(
                "Registered worktree '{}' does not match task {} card {}.",
                worktree_name, task_id, card.id
            ));
        }
        if !view.meta.root.exists() {
            return Err(format!(
                "Registered worktree '{}' path '{}' for card {} does not exist",
                worktree_name,
                view.meta.root.display(),
                card.id
            ));
        }
        service
            .validate_registered_worktree_checkout(worktree_name)
            .await?;
        return canonical_existing_path(&view.meta.root);
    }
    Err(format!(
        "Registered worktree '{}' for card {} was not found",
        worktree_name, card.id
    ))
}

async fn registered_worktree_path_for_card(
    ctx: &refact_runtime_api::ChatCtx,
    task_id: &str,
    card: &BoardCard,
    excluded_id: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let project_dirs = refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    for source_root in project_dirs {
        let Ok(service) = WorktreeService::new_async(ctx.paths.cache_dir.clone(), source_root).await
        else {
            continue;
        };
        let Ok(registry) = service.load_registry().await else {
            continue;
        };
        for record in &registry.records {
            if excluded_id == Some(record.meta.id.as_str()) {
                continue;
            }
            let Ok(view) = service.get_worktree(&record.meta.id).await else {
                continue;
            };
            let branch_matches = card
                .agent_branch
                .as_deref()
                .map(|branch| {
                    view.meta.branch.as_deref() == Some(branch)
                        && view
                            .meta
                            .task_id
                            .as_deref()
                            .map(|record_task_id| record_task_id == task_id)
                            .unwrap_or(true)
                })
                .unwrap_or(false);
            if !registered_record_matches_card(&view, task_id, card) && !branch_matches {
                continue;
            }
            if !view.meta.root.exists() {
                return Err(format!(
                    "Registered worktree '{}' path '{}' for card {} does not exist",
                    view.meta.id,
                    view.meta.root.display(),
                    card.id
                ));
            }
            service
                .validate_registered_worktree_checkout(&view.meta.id)
                .await?;
            return canonical_existing_path(&view.meta.root).map(Some);
        }
    }
    Ok(None)
}

async fn canonical_worktree(
    ctx: &refact_runtime_api::ChatCtx,
    task_id: &str,
    card: &BoardCard,
) -> Result<PathBuf, String> {
    if let Some(worktree_name) = card
        .agent_worktree_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return match registered_worktree_path(ctx, task_id, card, worktree_name).await {
            Ok(path) => Ok(path),
            Err(registered_err) => {
                if !registered_error_allows_legacy_fallback(&registered_err) {
                    return Err(registered_err);
                }
                let card_registered_err = match registered_worktree_path_for_card(
                    ctx,
                    task_id,
                    card,
                    Some(worktree_name),
                )
                .await
                {
                    Ok(Some(path)) => return Ok(path),
                    Ok(None) => None,
                    Err(err) => Some(err),
                };
                let workspace_root = primary_workspace_root(ctx).await?;
                legacy_worktree_path(ctx.paths.cache_dir.as_path(), &workspace_root, card).map_err(|legacy_err| {
                    if let Some(card_registered_err) = card_registered_err.as_deref() {
                        format!(
                            "{}; related registered lookup failed: {}; legacy fallback failed: {}",
                            registered_err, card_registered_err, legacy_err
                        )
                    } else {
                        format!("{}; legacy fallback failed: {}", registered_err, legacy_err)
                    }
                })
            }
        };
    }
    match registered_worktree_path_for_card(ctx, task_id, card, None).await {
        Ok(Some(path)) => return Ok(path),
        Ok(None) => {}
        Err(registered_err) => {
            if !registered_error_allows_legacy_fallback(&registered_err) {
                return Err(registered_err);
            }
            let workspace_root = primary_workspace_root(ctx).await?;
            return legacy_worktree_path(ctx.paths.cache_dir.as_path(), &workspace_root, card).map_err(|legacy_err| {
                format!("{}; legacy fallback failed: {}", registered_err, legacy_err)
            });
        }
    }
    let workspace_root = primary_workspace_root(ctx).await?;
    legacy_worktree_path(ctx.paths.cache_dir.as_path(), &workspace_root, card)
}

pub async fn run_with_timeout<F, T>(fut: F, timeout: Duration) -> Result<T, ()>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(timeout, fut).await.map_err(|_| ())
}

pub fn git_output_cap_notice(shown_bytes: usize, total_bytes: usize, limit: usize) -> String {
    format!(
        "\n... ⚠️ showing {} of {}+ git output bytes (limit: agent_diff_max_output_bytes = {}). 💡 Raise agent_diff_max_output_bytes in trajectory settings or narrow the diff with a path filter.\n",
        shown_bytes, total_bytes, limit
    )
}

pub async fn run_git(worktree: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    use tokio::io::AsyncReadExt;

    let mut child = tokio::process::Command::new("git")
        .args(args)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to run git {:?} in '{}': {}",
                args,
                worktree.display(),
                e
            )
        })?;

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let max_output_bytes = refact_core::trajectory_settings::current().agent_diff_max_output_bytes;
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 8192];
    let mut capped = false;
    let mut seen_bytes: usize = 0;

    let read_result = run_with_timeout(
        async {
            loop {
                let n = stdout
                    .read(&mut chunk)
                    .await
                    .map_err(|e| format!("git stdout read failed: {e}"))?;
                if n == 0 {
                    break;
                }
                seen_bytes = seen_bytes.saturating_add(n);
                if buf.len() + n > max_output_bytes {
                    let remaining = max_output_bytes.saturating_sub(buf.len());
                    buf.extend_from_slice(&chunk[..remaining]);
                    capped = true;
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Ok::<_, String>(())
        },
        timeout,
    )
    .await;

    let _ = child.kill().await;
    let status = child
        .wait()
        .await
        .map_err(|e| format!("git wait failed: {e}"))?;

    match read_result {
        Err(()) => {
            return Err(format!(
                "git {:?} timed out after {} seconds",
                args,
                timeout.as_secs()
            ));
        }
        Ok(Err(e)) => return Err(e),
        Ok(Ok(())) => {}
    }

    if capped {
        let shown = buf.len();
        buf.extend_from_slice(
            git_output_cap_notice(shown, seen_bytes.max(shown), max_output_bytes).as_bytes(),
        );
        return String::from_utf8(buf).map_err(|e| format!("git output not utf-8: {e}"));
    }

    if !status.success() {
        let mut stderr_buf = Vec::new();
        let _ = stderr.read_to_end(&mut stderr_buf).await;
        let stderr_text = String::from_utf8_lossy(&stderr_buf).trim().to_string();
        let msg = if stderr_text.is_empty() {
            "unknown git error".to_string()
        } else {
            stderr_text
        };
        return Err(format!(
            "git {:?} failed in '{}': {}",
            args,
            worktree.display(),
            msg
        ));
    }

    String::from_utf8(buf).map_err(|e| format!("git output not utf-8: {e}"))
}

async fn list_untracked(worktree: &Path, timeout: Duration) -> Result<Vec<String>, String> {
    Ok(run_git(
        worktree,
        &["ls-files", "--others", "--exclude-standard"],
        timeout,
    )
    .await?
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect())
}

fn append_section(output: &mut String, title: &str, body: &str) {
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str("## ");
    output.push_str(title);
    output.push_str("\n");
    if body.trim().is_empty() {
        output.push_str("(no changes)\n");
    } else {
        output.push_str(body.trim_end());
        output.push('\n');
    }
}

fn join_untracked(untracked: &[String]) -> String {
    if untracked.is_empty() {
        String::new()
    } else {
        let mut output = untracked.join("\n");
        output.push('\n');
        output
    }
}

fn usize_from_file_len(len: u64) -> usize {
    usize::try_from(len).unwrap_or(usize::MAX)
}

fn read_file_prefix(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open '{}': {}", path.display(), e))?;
    let mut bytes = Vec::new();
    file.take(limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read '{}': {}", path.display(), e))?;
    Ok(bytes)
}

fn render_untracked_text_diff(relative_path: &str, bytes: &[u8], more_bytes: u64) -> String {
    let mut lines = String::from_utf8_lossy(bytes)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if more_bytes > 0 {
        lines.push(format!("... (truncated, {} more bytes)", more_bytes));
    }
    let mut output = String::new();
    output.push_str("--- /dev/null\n");
    output.push_str(&format!("+++ b/{}\n", relative_path));
    output.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for line in lines {
        output.push('+');
        output.push_str(&line);
        output.push('\n');
    }
    output
}

fn render_untracked_unified_entry(
    worktree: &Path,
    relative_path: &str,
    remaining_budget: usize,
) -> (String, usize) {
    let path = worktree.join(relative_path);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) => return (format!("{} (failed to stat: {})\n", relative_path, e), 0),
    };
    if !metadata.is_file() {
        return (format!("{} (not a regular file)\n", relative_path), 0);
    }

    let file_len = metadata.len();
    let file_len_usize = usize_from_file_len(file_len);
    let content_limit = remaining_budget.min(UNTRACKED_PER_FILE_CAP_BYTES);
    let read_limit = file_len_usize
        .min(UNTRACKED_PER_FILE_CAP_BYTES)
        .min(content_limit.max(UNTRACKED_BINARY_PROBE_BYTES));
    let bytes = match read_file_prefix(&path, read_limit) {
        Ok(bytes) => bytes,
        Err(e) => return (format!("{} ({})\n", relative_path, e), 0),
    };
    let probe_len = bytes.len().min(UNTRACKED_BINARY_PROBE_BYTES);
    if bytes[..probe_len].contains(&0) {
        return (
            format!("{} (binary, {} bytes)\n", relative_path, file_len),
            0,
        );
    }

    let embedded_len = bytes.len().min(content_limit);
    let more_bytes = file_len.saturating_sub(embedded_len as u64);
    (
        render_untracked_text_diff(relative_path, &bytes[..embedded_len], more_bytes),
        embedded_len,
    )
}

fn render_untracked_unified(worktree: &Path, untracked: &[String]) -> String {
    let mut output = String::new();
    let mut used_bytes = 0usize;
    for (index, relative_path) in untracked.iter().enumerate() {
        let remaining_budget = UNTRACKED_TOTAL_CAP_BYTES.saturating_sub(used_bytes);
        if remaining_budget == 0 {
            for omitted_path in &untracked[index..] {
                output.push_str(omitted_path);
                output.push('\n');
            }
            output.push_str(&format!(
                "({} more untracked files omitted due to size cap)\n",
                untracked.len().saturating_sub(index)
            ));
            break;
        }
        let (entry, consumed_bytes) =
            render_untracked_unified_entry(worktree, relative_path, remaining_budget);
        output.push_str(&entry);
        if !entry.ends_with('\n') {
            output.push('\n');
        }
        used_bytes = used_bytes.saturating_add(consumed_bytes);
    }
    output
}

fn push_name_only(names: &mut Vec<String>, seen: &mut HashSet<String>, output: &str) {
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if seen.insert(line.to_string()) {
            names.push(line.to_string());
        }
    }
}

async fn run_git_diff(
    worktree: &Path,
    mode: AgentDiffMode,
    base: &DiffBase,
) -> Result<String, String> {
    let range = format!("{}...HEAD", base.refish);
    match mode {
        AgentDiffMode::Stat => {
            let committed =
                run_git(worktree, &["diff", "--stat", &range], GIT_DIFF_TIMEOUT).await?;
            let staged =
                run_git(worktree, &["diff", "--stat", "--cached"], GIT_DIFF_TIMEOUT).await?;
            let unstaged = run_git(worktree, &["diff", "--stat"], GIT_DIFF_TIMEOUT).await?;
            let untracked = list_untracked(worktree, GIT_DIFF_TIMEOUT).await?;
            if committed.trim().is_empty()
                && staged.trim().is_empty()
                && unstaged.trim().is_empty()
                && untracked.is_empty()
            {
                return Ok("(no changes detected)".to_string());
            }
            let mut output = String::new();
            append_section(&mut output, "Committed changes since base", &committed);
            append_section(&mut output, "Staged changes", &staged);
            append_section(&mut output, "Unstaged changes", &unstaged);
            append_section(&mut output, "Untracked files", &join_untracked(&untracked));
            Ok(output)
        }
        AgentDiffMode::Unified => {
            let committed = run_git(worktree, &["diff", &range], GIT_DIFF_TIMEOUT).await?;
            let staged = run_git(worktree, &["diff", "--cached"], GIT_DIFF_TIMEOUT).await?;
            let unstaged = run_git(worktree, &["diff"], GIT_DIFF_TIMEOUT).await?;
            let untracked = list_untracked(worktree, GIT_DIFF_TIMEOUT).await?;
            if committed.trim().is_empty()
                && staged.trim().is_empty()
                && unstaged.trim().is_empty()
                && untracked.is_empty()
            {
                return Ok("(no changes detected)".to_string());
            }
            let mut output = String::new();
            append_section(&mut output, "Committed changes since base", &committed);
            append_section(&mut output, "Staged changes", &staged);
            append_section(&mut output, "Unstaged changes", &unstaged);
            append_section(
                &mut output,
                "Untracked files",
                &render_untracked_unified(worktree, &untracked),
            );
            Ok(output)
        }
        AgentDiffMode::NameOnly => {
            let committed =
                run_git(worktree, &["diff", "--name-only", &range], GIT_DIFF_TIMEOUT).await?;
            let staged = run_git(
                worktree,
                &["diff", "--name-only", "--cached"],
                GIT_DIFF_TIMEOUT,
            )
            .await?;
            let unstaged = run_git(worktree, &["diff", "--name-only"], GIT_DIFF_TIMEOUT).await?;
            let untracked = list_untracked(worktree, GIT_DIFF_TIMEOUT).await?;
            let mut names = Vec::new();
            let mut seen = HashSet::new();
            push_name_only(&mut names, &mut seen, &committed);
            push_name_only(&mut names, &mut seen, &staged);
            push_name_only(&mut names, &mut seen, &unstaged);
            for path in untracked {
                if seen.insert(path.clone()) {
                    names.push(path);
                }
            }
            if names.is_empty() {
                Ok("(no changes detected)".to_string())
            } else {
                Ok(names.join("\n"))
            }
        }
    }
}

fn output_fence(mode: AgentDiffMode) -> &'static str {
    match mode {
        AgentDiffMode::Unified => "diff",
        AgentDiffMode::Stat | AgentDiffMode::NameOnly => "text",
    }
}

fn truncate_lines(output: &str, max_lines: usize) -> String {
    let lines = output.lines().collect::<Vec<_>>();
    if lines.len() <= max_lines {
        return output.to_string();
    }
    let omitted = lines.len().saturating_sub(max_lines);
    let mut result = lines
        .iter()
        .take(max_lines)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    result.push_str(&format!(
        "... ({} more lines, use mode='name-only' to see all files)",
        omitted
    ));
    result
}

pub fn render_agent_diff(
    card: &BoardCard,
    branch: &str,
    base: &DiffBase,
    mode: AgentDiffMode,
    output: &str,
    max_lines: usize,
) -> String {
    let rendered = truncate_lines(output, max_lines);
    let diff = if rendered.trim().is_empty() {
        "(no changes detected)".to_string()
    } else {
        rendered
    };
    format!(
        "# Agent Diff for {}\n\n**Card:** {}\n**Branch:** {}\n**Base:** {}\n\n```{}\n{}\n```",
        card.id,
        card.title,
        branch,
        base.label,
        output_fence(mode),
        diff
    )
}

#[async_trait]
impl Tool for ToolAgentDiff {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "agent_diff".to_string(),
            display_name: "Agent Diff".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Show the real git diff for a task agent worktree against the task base commit or branch, including committed, staged, unstaged, and untracked changes. Planner-only; use this to inspect actual agent changes instead of relying on final reports.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "card_id": {"type": "string", "description": "Card ID whose agent worktree diff to inspect"},
                    "mode": {"type": "string", "enum": ["stat", "unified", "name-only"], "description": "Diff mode. Default: stat"},
                    "max_lines": {"type": "number", "description": "Maximum output lines before truncation. Default: 300"},
                    "task_id": {"type": "string", "description": "Task ID (optional if chat is bound to a task)"}
                },
                "required": ["card_id"]
            }),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let is_planner = {
            let ccx_lock = ccx.lock().await;
            ccx_lock
                .task_meta
                .as_ref()
                .map(|meta| meta.role == "planner")
                .unwrap_or(false)
        };
        if !is_planner {
            return Err(
                "agent_diff can only be called by the task planner. Switch to the planner chat to inspect agent diffs."
                    .to_string(),
            );
        }

        let requested_card_id = required_string(args, "card_id")?;
        let (card_id, _) = parse_card_variant_id(&requested_card_id);
        let mode = AgentDiffMode::parse(args.get("mode"))?;
        let max_lines = parse_max_lines(args)?;
        let task_id = require_bound_planner_task(&ccx, args).await?;
        let app = ccx.lock().await.app.clone();
        let ctx = app.chat_ctx();

        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let card = board
            .get_card(card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        let card = card_for_diff_target(card, &requested_card_id)?;
        let worktree = canonical_worktree(&ctx, &task_id, &card).await?;
        let task_meta = storage::load_task_meta_for_ctx(&ctx, &task_id).await?;
        let (worktree_commit, worktree_branch) =
            base_from_worktree_meta(&ctx, &task_id, &card).await;
        let base = resolve_base(
            worktree_commit,
            worktree_branch,
            card.base_commit.clone(),
            card.base_branch.clone(),
            task_meta.base_commit,
            task_meta.base_branch,
        )?;
        let branch = card
            .agent_branch
            .as_ref()
            .ok_or_else(|| format!("Card {} has no agent branch", card.id))?;
        let output = run_git_diff(&worktree, mode, &base).await?;
        let result = render_agent_diff(&card, branch, &base, mode, &output, max_lines);
        let output_lines = output.lines().count();
        let truncated = output_lines > max_lines;

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                extra: serde_json::Map::from_iter([(
                    "git".to_string(),
                    json!({
                        "status": "available",
                        "path": "workspace",
                        "short_sha": short_sha(&base.refish),
                        "scope": format!("card:{}:{}", card.id, mode.as_str()),
                        "truncated": truncated,
                    }),
                )]),
                ..Default::default()
            })],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

impl AgentDiffMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stat => "stat",
            Self::Unified => "unified",
            Self::NameOnly => "name_only",
        }
    }
}

fn short_sha(value: &str) -> Option<&str> {
    let value = value.trim();
    value
        .get(..12)
        .filter(|value| value.len() >= 7 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
