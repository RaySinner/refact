use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use async_trait::async_trait;
use uuid::Uuid;
use chrono::{DateTime, Utc};

use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_runtime_api::GlobalContextCtx;
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum, ContextFile};
use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::{CapsProvider, ChatCtx};
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_runtime_api::trajectory_runtime::resolve_task_planner_controller_chat_id;
use refact_tasks::rooms;
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{BoardCard, StatusUpdate};
use refact_runtime_api::global_context::GlobalContext;
use refact_tasks::types::TaskMeta as StoredTaskMeta;
use refact_worktrees::git;
use refact_worktrees::service::WorktreeService;
use refact_chat_api::{ChatCommand, TaskMeta, ThreadParams};
use refact_worktrees::types::{CreateWorktreeRequest, WorktreeMeta};
use refact_runtime_api::CreateSessionRequest;

pub const FILES_TO_OPEN_PER_FILE_LIMIT: usize = 256 * 1024;
pub const FILES_TO_OPEN_TOTAL_LIMIT: usize = 1024 * 1024;
pub const MAX_ID_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct SpawnPreparationCorrelation {
    task_id: String,
    card_id: String,
    agent_id: String,
    agent_chat_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct SpawnPreparationEvent {
    event: &'static str,
    timestamp_ms: i64,
    elapsed_ms: u64,
    #[serde(flatten)]
    correlation: SpawnPreparationCorrelation,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files_bytes: Option<usize>,
}

impl SpawnPreparationEvent {
    fn at(
        event: &'static str,
        correlation: SpawnPreparationCorrelation,
        timestamp_ms: i64,
        elapsed: Duration,
    ) -> Self {
        Self {
            event,
            timestamp_ms,
            elapsed_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            correlation,
            error_stage: None,
            files_count: None,
            files_bytes: None,
        }
    }

    fn with_error_stage(mut self, stage: &'static str) -> Self {
        self.error_stage = Some(stage);
        self
    }

    fn with_file_metrics(mut self, count: usize, bytes: usize) -> Self {
        self.files_count = Some(count);
        self.files_bytes = Some(bytes);
        self
    }

    fn emit(self) {
        tracing::info!(
            target: "spawn_preparation",
            event = self.event,
            timestamp_ms = self.timestamp_ms,
            elapsed_ms = self.elapsed_ms,
            task_id = %self.correlation.task_id,
            card_id = %self.correlation.card_id,
            agent_id = %self.correlation.agent_id,
            agent_chat_id = %self.correlation.agent_chat_id,
            error_stage = ?self.error_stage,
            files_count = ?self.files_count,
            files_bytes = ?self.files_bytes,
            "spawn_preparation"
        );
    }
}

struct SpawnPreparationDiagnostics {
    correlation: SpawnPreparationCorrelation,
    started_at: Instant,
    stage: &'static str,
    terminal: bool,
    #[cfg(test)]
    events: Vec<SpawnPreparationEvent>,
}

impl SpawnPreparationDiagnostics {
    fn new(task_id: &str, card_id: &str, agent_id: &str, agent_chat_id: &str) -> Self {
        Self::new_at(
            SpawnPreparationCorrelation {
                task_id: task_id.to_string(),
                card_id: card_id.to_string(),
                agent_id: agent_id.to_string(),
                agent_chat_id: agent_chat_id.to_string(),
            },
            Utc::now().timestamp_millis(),
        )
    }

    fn new_at(correlation: SpawnPreparationCorrelation, timestamp_ms: i64) -> Self {
        let mut diagnostics = Self {
            correlation,
            started_at: Instant::now(),
            stage: "spawn_preflight",
            terminal: false,
            #[cfg(test)]
            events: Vec::new(),
        };
        diagnostics.transition_at(
            "spawn_preflight",
            "spawn_accepted",
            timestamp_ms,
            Duration::ZERO,
            None,
        );
        diagnostics
    }

    fn transition(&mut self, stage: &'static str, event: &'static str) {
        self.transition_at(
            stage,
            event,
            Utc::now().timestamp_millis(),
            self.started_at.elapsed(),
            None,
        );
    }

    fn transition_with_file_metrics(
        &mut self,
        stage: &'static str,
        event: &'static str,
        count: usize,
        bytes: usize,
    ) {
        self.transition_at(
            stage,
            event,
            Utc::now().timestamp_millis(),
            self.started_at.elapsed(),
            Some((count, bytes)),
        );
    }

    fn transition_at(
        &mut self,
        stage: &'static str,
        event: &'static str,
        timestamp_ms: i64,
        elapsed: Duration,
        file_metrics: Option<(usize, usize)>,
    ) {
        if self.terminal {
            return;
        }
        self.stage = stage;
        let mut event =
            SpawnPreparationEvent::at(event, self.correlation.clone(), timestamp_ms, elapsed);
        if let Some((count, bytes)) = file_metrics {
            event = event.with_file_metrics(count, bytes);
        }
        self.record(event);
    }

    fn set_stage(&mut self, stage: &'static str) {
        if !self.terminal {
            self.stage = stage;
        }
    }

    fn complete(&mut self, stage: &'static str, event: &'static str) {
        self.complete_at(
            stage,
            event,
            Utc::now().timestamp_millis(),
            self.started_at.elapsed(),
        );
    }

    fn complete_at(
        &mut self,
        stage: &'static str,
        event: &'static str,
        timestamp_ms: i64,
        elapsed: Duration,
    ) {
        self.transition_at(stage, event, timestamp_ms, elapsed, None);
        self.terminal = true;
    }

    fn fail(&mut self) {
        self.fail_at(Utc::now().timestamp_millis(), self.started_at.elapsed());
    }

    fn fail_at(&mut self, timestamp_ms: i64, elapsed: Duration) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        let event = SpawnPreparationEvent::at(
            "spawn_error",
            self.correlation.clone(),
            timestamp_ms,
            elapsed,
        )
        .with_error_stage(self.stage);
        self.record(event);
    }

    fn record(&mut self, event: SpawnPreparationEvent) {
        #[cfg(test)]
        self.events.push(event.clone());
        event.emit();
    }
}

impl Drop for SpawnPreparationDiagnostics {
    fn drop(&mut self) {
        self.fail();
    }
}

pub fn validate_id(id: &str, name: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if id.len() > MAX_ID_LEN {
        return Err(format!("ID '{id}' is too long (max {MAX_ID_LEN} chars)"));
    }
    if id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(format!(
            "{name} '{id}' contains invalid characters (only alphanumeric, '-', '_' allowed)"
        ))
    }
}

pub async fn get_task_id(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
) -> Result<String, String> {
    if let Some(id) = args.get("task_id").and_then(|v| v.as_str()) {
        return Ok(id.to_string());
    }
    let ccx_lock = ccx.lock().await;
    if let Some(ref meta) = ccx_lock.task_meta {
        return Ok(meta.task_id.clone());
    }
    storage::infer_task_id_from_chat_id(&ccx_lock.chat_id)
        .ok_or_else(|| "Missing 'task_id' (and chat is not bound to a task)".to_string())
}

pub async fn resolve_agent_model(
    caps_provider: &Arc<dyn CapsProvider>,
    current_model: &str,
) -> Result<String, String> {
    let caps = caps_provider
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| format!("Failed to load caps for model resolution: {}", e))?;

    let task_planner_model = caps.defaults.task_planner_agent_model.trim();
    if !task_planner_model.is_empty() {
        return Ok(task_planner_model.to_string());
    }

    if !current_model.is_empty() {
        return Ok(current_model.to_string());
    }

    let default_model = &caps.defaults.chat_default_model;
    if !default_model.is_empty() {
        return Ok(default_model.clone());
    }

    Err(
        "No model available: task_planner_agent_model, current_model, and global default are all empty"
            .to_string(),
    )
}

#[derive(Debug)]
pub struct PreparedWorktree {
    pub meta: WorktreeMeta,
    pub branch_was_created: bool,
    pub spawned_with_dirty_tree: bool,
    pub base_branch_mismatch_warning: Option<String>,
}

impl PreparedWorktree {
    pub fn branch_name(&self) -> Option<String> {
        self.meta.branch.clone()
    }

    pub fn worktree_name(&self) -> String {
        self.meta.id.clone()
    }

    pub fn worktree_path(&self) -> PathBuf {
        self.meta.root.clone()
    }

    pub fn source_workspace_root(&self) -> PathBuf {
        self.meta.source_workspace_root.clone()
    }

    pub fn worktree_meta(&self) -> WorktreeMeta {
        self.meta.clone()
    }

    pub async fn cleanup_unlinked(self, cache_dir: PathBuf) {
        if let Ok(service) =
            WorktreeService::new_async(cache_dir, self.meta.source_workspace_root.clone()).await
        {
            if service
                .delete_worktree(&self.meta.id, self.branch_was_created, true)
                .await
                .is_ok()
            {
                return;
            }
        }
        if self.meta.root.exists() {
            let _ = std::fs::remove_dir_all(&self.meta.root);
        }
        if self.branch_was_created {
            if let Some(branch) = self.meta.branch.as_deref() {
                if let Err(e) =
                    refact_worktrees::git::delete_branch(&self.meta.source_workspace_root, branch)
                {
                    tracing::warn!(
                        "Failed to delete leaked agent branch '{}' during cleanup_unlinked: {}",
                        branch,
                        e
                    );
                }
            }
        }
    }

    pub async fn retain(self) {
        tracing::info!(
            "Retaining worktree '{}' / branch '{}' after spawn failure for inspection.",
            self.worktree_name(),
            self.branch_name()
                .unwrap_or_else(|| "<unknown>".to_string())
        );
    }
}

pub fn current_branch_for_workspace(
    workspace_root: &std::path::Path,
) -> Result<Option<String>, String> {
    let repo = git::discover_repo(workspace_root)?;
    Ok(git::current_branch(&repo))
}

pub fn optional_nonempty_string_arg(
    args: &HashMap<String, Value>,
    name: &str,
) -> Result<Option<String>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.trim().to_string()))
        }
        Some(Value::String(_)) => Err(format!("'{}' must be a non-empty string", name)),
        Some(_) => Err(format!("'{}' must be a string", name)),
    }
}

pub fn git_base_at(path: &Path) -> Result<(Option<String>, String), String> {
    let repo = git::discover_repo(path)?;
    Ok((git::current_branch(&repo), git::head_commit(&repo)?))
}

pub fn resolve_spawn_base(
    explicit_branch: Option<String>,
    explicit_commit: Option<String>,
    planner_worktree: Option<&WorktreeMeta>,
    workspace_root: &Path,
    task_meta: &StoredTaskMeta,
) -> (Option<String>, Option<String>) {
    if explicit_branch.is_some() || explicit_commit.is_some() {
        let fallback = planner_worktree
            .and_then(|worktree| git_base_at(&worktree.root).ok())
            .or_else(|| git_base_at(workspace_root).ok());
        return (
            explicit_branch
                .or_else(|| fallback.as_ref().and_then(|(branch, _)| branch.clone()))
                .or_else(|| task_meta.base_branch.clone()),
            explicit_commit,
        );
    }
    if let Some(base) = planner_worktree.and_then(|worktree| git_base_at(&worktree.root).ok()) {
        return (base.0, Some(base.1));
    }
    if let Ok((branch, commit)) = git_base_at(workspace_root) {
        return (branch, Some(commit));
    }
    (task_meta.base_branch.clone(), task_meta.base_commit.clone())
}

pub fn task_base_branch_missing_error(branch: &str) -> String {
    format!(
        "Task base branch '{}' no longer exists. Update the task base branch or create a new task on the current branch.",
        branch
    )
}

pub fn map_task_base_branch_error(error: String, base_branch: Option<&str>) -> String {
    if let Some(branch) = base_branch {
        if error.contains(&format!("Base branch '{}' not found", branch)) {
            return task_base_branch_missing_error(branch);
        }
    }
    format!("Failed to create task-agent worktree: {}", error)
}

pub fn find_abandoned_worktrees(board: &refact_tasks::types::TaskBoard) -> Vec<String> {
    board
        .cards
        .iter()
        .filter(|card| card.column != "doing" && card.column != "failed")
        .filter_map(|card| {
            let worktree = card.agent_worktree.as_ref()?;
            if !std::path::Path::new(worktree).exists() {
                return None;
            }
            Some(format!(
                "- {} ({}) in column `{}`: `{}`",
                card.id, card.title, card.column, worktree
            ))
        })
        .collect()
}

pub fn abandoned_worktrees_error(action: &str, abandoned_worktrees: &[String]) -> String {
    format!(
        "Cannot {} while abandoned task worktrees exist. Resolve each one, then retry:\n\
         - to keep the work: `merge_agent(card_id=...)`\n\
         - to discard it without merging: remove the worktree directly with \
         `git worktree remove <path> --force` (add `git branch -D <branch>` to drop its branch)\n\n{}",
        action,
        abandoned_worktrees.join("\n")
    )
}

pub async fn prepare_agent_worktree(
    ctx: &ChatCtx,
    task_meta: &StoredTaskMeta,
    task_id: &str,
    agent_id: &str,
    card_id: &str,
    agent_chat_id: &str,
) -> Result<PreparedWorktree, String> {
    prepare_agent_worktree_with_suffix(
        ctx,
        task_meta,
        task_id,
        agent_id,
        card_id,
        agent_chat_id,
        None,
    )
    .await
}

pub async fn prepare_agent_worktree_with_suffix(
    ctx: &ChatCtx,
    task_meta: &StoredTaskMeta,
    task_id: &str,
    agent_id: &str,
    card_id: &str,
    agent_chat_id: &str,
    branch_suffix: Option<&str>,
) -> Result<PreparedWorktree, String> {
    prepare_agent_worktree_with_base_and_suffix(
        ctx,
        task_meta,
        task_id,
        agent_id,
        card_id,
        agent_chat_id,
        task_meta.base_branch.clone(),
        task_meta.base_commit.clone(),
        branch_suffix,
    )
    .await
}

pub async fn prepare_agent_worktree_with_base_and_suffix(
    ctx: &ChatCtx,
    task_meta: &StoredTaskMeta,
    task_id: &str,
    agent_id: &str,
    card_id: &str,
    agent_chat_id: &str,
    base_branch: Option<String>,
    base_commit: Option<String>,
    branch_suffix: Option<&str>,
) -> Result<PreparedWorktree, String> {
    let project_dirs =
        refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx).await;
    let workspace_root = project_dirs.first().cloned().ok_or_else(|| {
        "No workspace folder found; task agents require an isolated git worktree".to_string()
    })?;

    let agent_id_short = &agent_id[..agent_id.len().min(8)];
    let branch_name = format!(
        "refact/task/{}/card/{}/{}{}",
        task_id,
        card_id,
        agent_id_short,
        branch_suffix.unwrap_or("")
    );
    if branch_name.len() > 200 {
        return Err(format!(
            "Generated branch name '{branch_name}' exceeds 200 chars"
        ));
    }
    let cache_dir = ctx.paths.cache_dir.clone();
    let service = WorktreeService::new_async(cache_dir, workspace_root.clone()).await?;
    let task_base_branch = base_branch;
    let current_branch = if task_base_branch.is_some() {
        current_branch_for_workspace(&workspace_root)?
    } else {
        None
    };
    if let Some(branch) = task_base_branch.as_deref() {
        if !git::branch_exists(&workspace_root, branch)? {
            return Err(task_base_branch_missing_error(branch));
        }
    }
    let base_branch_mismatch_warning = match (task_base_branch.as_deref(), current_branch.as_deref()) {
        (Some(task_branch), Some(current_branch)) if task_branch != current_branch => Some(format!(
            "Current repo HEAD is on branch '{}' but this task was created from '{}'; spawning agent from the task base branch.",
            current_branch, task_branch
        )),
        (Some(task_branch), None) => Some(format!(
            "Current repo HEAD is detached but this task was created from '{}'; spawning agent from the task base branch.",
            task_branch
        )),
        _ => None,
    };
    let created = service
        .create_worktree(CreateWorktreeRequest {
            source_workspace_root: Some(workspace_root.to_string_lossy().to_string()),
            branch: Some(branch_name),
            base_branch: task_base_branch,
            base_commit,
            chat_id: Some(agent_chat_id.to_string()),
            kind: Some("task_agent".to_string()),
            task_id: Some(task_id.to_string()),
            card_id: Some(card_id.to_string()),
            agent_id: Some(agent_id.to_string()),
        })
        .await
        .map_err(|e| map_task_base_branch_error(e, task_meta.base_branch.as_deref()))?;

    let worktree_root = created.worktree.meta.root.clone();
    let worktree_meta = tokio::fs::metadata(&worktree_root).await.map_err(|_| {
        format!(
            "Agent worktree '{}' does not exist on disk",
            worktree_root.display()
        )
    })?;
    if !worktree_meta.is_dir() {
        return Err(format!(
            "Agent worktree '{}' is not a directory",
            worktree_root.display()
        ));
    }

    if created.dirty_source_warning {
        tracing::warn!(
            "Spawning agent from committed base — local uncommitted changes are excluded from the agent's worktree"
        );
    }
    if let Some(warning) = base_branch_mismatch_warning.as_deref() {
        tracing::warn!("spawn_agent: {}", warning);
    }

    Ok(PreparedWorktree {
        meta: created.worktree.meta,
        branch_was_created: created.branch_was_created,
        spawned_with_dirty_tree: created.dirty_source_warning,
        base_branch_mismatch_warning,
    })
}

pub fn parse_rfc3339_to_utc(ts: &str) -> Option<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

pub fn validate_files_to_open(args: &HashMap<String, Value>) -> Result<Vec<String>, String> {
    let raw = args
        .get("files_to_open")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    let mut paths = Vec::with_capacity(raw.len());
    for (idx, v) in raw.iter().enumerate() {
        match v.as_str() {
            None => {
                let type_name = match v {
                    Value::Null => "null",
                    Value::Bool(_) => "bool",
                    Value::Number(_) => "number",
                    Value::Array(_) => "array",
                    Value::Object(_) => "object",
                    Value::String(_) => unreachable!(),
                };
                return Err(format!(
                    "files_to_open[{idx}] must be a string, got {type_name}"
                ));
            }
            Some(s) if s.is_empty() => {
                return Err(format!("files_to_open[{idx}] is empty"));
            }
            Some(s) => paths.push(s.to_string()),
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut has_duplicates = false;
    let mut deduped = Vec::with_capacity(paths.len());
    for path in paths {
        if seen.insert(path.clone()) {
            deduped.push(path);
        } else {
            has_duplicates = true;
        }
    }
    if has_duplicates {
        tracing::warn!("files_to_open contains duplicate paths; deduplicating");
    }
    Ok(deduped)
}

pub struct ToolTaskSpawnAgent;

impl ToolTaskSpawnAgent {
    pub fn new() -> Self {
        Self
    }
}

pub fn build_agent_prompt(
    card_title: &str,
    instructions: &str,
    dependency_context: &str,
    suggested_steps: usize,
) -> String {
    let dep_section = if dependency_context.is_empty() {
        String::new()
    } else {
        format!("\n\n## Context from Dependencies\n{}", dependency_context)
    };

    format!(
        r#"# Card: {card_title}

## Instructions
{instructions}{dep_section}

## Guidelines
- Suggested step budget: ~{suggested_steps} steps
- Focus only on this specific card
- Report progress clearly
- **Remember to call `agent_finish()` when done!**"#
    )
}

/// Prompt for a member of a multi-agent room.
///
/// The roster is embedded because a room member cannot guess who else is on the card: its chat
/// has no shared history, exactly like the isolated contexts Claude Code agent teams rely on.
/// The named bus is what turns a set of isolated workers into a team.
pub fn build_room_member_prompt(
    card_title: &str,
    instructions: &str,
    dependency_context: &str,
    suggested_steps: usize,
    role: &str,
    mandate: Option<&str>,
    roster: &[String],
) -> String {
    let base = build_agent_prompt(
        card_title,
        instructions,
        dependency_context,
        suggested_steps,
    );
    let mandate_line = mandate
        .map(|mandate| format!("\n**Your mandate:** {mandate}"))
        .unwrap_or_default();
    let roster_section = if roster.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n## Room\nYou are the `{role}` on this card{mandate_line}.\n\n\
             Other members (each has its own context and its own git worktree):\n{}\n",
            roster
                .iter()
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let role_directive = if matches!(role, "reviewer" | "qa" | "architect" | "auditor") {
        "### Role Directive: Reviewer / Inspector\n\
         - Your task is to verify changes and inspect diffs or architecture.\n\
         - Coordinate with peers via `agent_message(to=\"<peer chat id>\", text=...)`.\n\
         - Do not edit source files if your mandate is review-only. When review is complete, record your findings in `agent_finish`.\n"
    } else {
        "### Role Directive: Implementer\n\
         - You MUST inspect the codebase and implement the requested changes directly in your worktree.\n\
         - DO NOT call `agent_finish` prematurely without writing code or verifying your changes.\n\
         - Once implementation and checks are done, summarise your work and call `agent_finish(success=true, report=...)` or inform your peers via `agent_message`.\n"
    };
    let critical_guard = "\
        ### Critical Rule on Completion:\n\
        - DO NOT give up or call `agent_finish(success=false)` without attempting to use your tools to inspect the files and perform work.\n\
        - Each member is responsible for their own mandate.\n";

    format!(
        "{base}{roster_section}\n\
         ## Working in a room\n\
         - Use `agent_message(to=\"<peer chat id>\", text=...)` to reach a peer; the address is the chat id above.\n\
         - An architect role is sequential: it cannot run at the same time as another member.\n\
         - Your report is recorded per member, so summarise what *you* did and what you verified.\n\
         - Merge and the card's final report are the architect's job, not yours.\n\
         {role_directive}\n\
         {critical_guard}"
    )
}

pub fn mark_card_agent_started(
    card: &mut BoardCard,
    agent_id: &str,
    agent_chat_id: &str,
    worktree_branch: Option<String>,
    worktree_path: Option<String>,
    worktree_name: Option<String>,
) {
    card.assignee = Some(agent_id.to_string());
    card.agent_chat_id = Some(agent_chat_id.to_string());
    card.started_at = Some(Utc::now().to_rfc3339());
    if card.column == "planned" {
        card.column = "doing".to_string();
    }
    card.agent_branch = worktree_branch;
    card.agent_worktree = worktree_path;
    card.agent_worktree_name = worktree_name;
    card.status_updates.push(StatusUpdate {
        timestamp: Utc::now().to_rfc3339(),
        message: "Agent started working on card".to_string(),
    });
}

/// Claim a room slot on `card` for a newly spawned member.
///
/// A card that already carries `team_members` is a room. The spawn must *claim the reserved
/// slot* the room's roster already named — not push a second member with the same role, which
/// would list one agent twice. Claiming fills the slot's identity (`agent_id`, `agent_chat_id`,
/// the worktree fields) and, because the spawn just created a real, working session, moves it
/// to `Running` — the status it had to stay `Pending` for while the roster was only a plan.
///
/// A member becomes `Running` only if its role may work in parallel: a sequential role (the
/// architect) stays `Pending` so `validate_team` can still refuse it once it actually starts,
/// and so a parallel peer is free to be `Running` beside it.
///
/// If the roster has no reserved slot for this role, the spawn is refused rather than quietly
/// appending a member the user never put in the room.
///
/// A card with no `team_members` is a single-agent card: the member is recorded and the card's
/// scalar fields are filled exactly as the historical path did.
pub fn claim_room_slot(
    card: &mut BoardCard,
    role: &str,
    agent_id: &str,
    agent_chat_id: &str,
    worktree_branch: Option<String>,
    worktree_path: Option<String>,
    worktree_name: Option<String>,
    mandate: Option<String>,
    tools: Vec<String>,
) -> Result<(), String> {
    let is_room = !card.team_members.is_empty();

    if is_room {
        rooms::validate_spawn_roster(card, role)?;
        let branch_for_promote = worktree_branch.clone();
        let path_for_promote = worktree_path.clone();
        // Claiming the slot borrows `card.team_members`, so the decision to promote (which only
        // needs the slot's behaviour) is computed inside this block and the borrow is dropped
        // before the card's scalars and status log are touched.
        let writes = {
            let Some(member) = rooms::claim_planned_slot(card, role) else {
                return Err(format!(
                    "Card {} has no reserved slot for role '{}'. The room roster was created \
                     with a different set of roles; create or edit the room with board_update \
                     before spawning it.",
                    card.id,
                    role.trim()
                ));
            };
            let writes = member.writes();
            let parallel = member.allows_parallel();
            member.agent_id = Some(agent_id.to_string());
            member.agent_chat_id = Some(agent_chat_id.to_string());
            member.agent_branch = worktree_branch;
            member.agent_worktree = worktree_path;
            if let Some(mandate) = mandate {
                member.mandate = Some(mandate);
            }
            member.tools = tools;
            member.member_status = Some(if parallel {
                refact_tasks::types::TeamStatus::Running
            } else {
                refact_tasks::types::TeamStatus::Pending
            });
            writes
        };
        card.status_updates.push(StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: format!("Agent {agent_id} joined the room as {role}"),
        });
        promote_to_primary_owner(
            card,
            agent_id,
            agent_chat_id,
            branch_for_promote,
            path_for_promote,
            writes,
        );
        return Ok(());
    }

    let mut member = rooms::new_room_member(
        role,
        agent_id,
        agent_chat_id,
        worktree_branch.clone(),
        worktree_path.clone(),
        mandate,
        tools,
    );
    member.member_status = Some(if member.allows_parallel() {
        refact_tasks::types::TeamStatus::Running
    } else {
        refact_tasks::types::TeamStatus::Pending
    });
    mark_card_agent_started(
        card,
        agent_id,
        agent_chat_id,
        worktree_branch,
        worktree_path,
        worktree_name,
    );
    card.team_members = vec![member];
    Ok(())
}

/// Primary Owner promotion: the first *writing* member of a room becomes the card's owner.
///
/// `agent_diff`, `merge_agent`, the spawn guard and the monitor all still read the card's scalar
/// fields, so the writing member's identity is mirrored onto them. A read-only member (a
/// reviewer, a decision maker) never owns the card: it has no worktree to diff or merge.
///
/// Only the first writer promotes, so the card keeps pointing at one stable agent even as more
/// read-only members join. The card moves to `doing` and records `started_at` the first time a
/// writer is promoted.
pub fn promote_to_primary_owner(
    card: &mut BoardCard,
    agent_id: &str,
    agent_chat_id: &str,
    worktree_branch: Option<String>,
    worktree_path: Option<String>,
    writes: bool,
) {
    if !writes {
        return;
    }
    // Only the first writer becomes the owner; a later writer must not steal the scalars that
    // the earlier owner's diff/merge paths already depend on.
    if card.agent_chat_id.is_some() {
        return;
    }
    mark_card_agent_started(
        card,
        agent_id,
        agent_chat_id,
        worktree_branch,
        worktree_path,
        None,
    );
    card.status_updates.push(StatusUpdate {
        timestamp: Utc::now().to_rfc3339(),
        message: format!("Agent {agent_id} is the primary owner of card {}", card.id),
    });
}

/// Default room role for a spawn: the honest name for a card an agent was asked to just "work on".
/// It is a label, not a contract — validation reads the agent definition, not this string.
pub const DEFAULT_ROOM_ROLE: &str = "coder";

pub fn spawn_role(args: &HashMap<String, Value>) -> Result<String, String> {
    // Roles are free text now. The user names a member for what it is on this card, so refusing
    // "designer" or "qa" would put the hardcoded job list back in the way. What a member may do is
    // decided by its agent definition (`writes`, `decision_maker`), not by its name.
    Ok(
        optional_nonempty_string_arg(args, "role")?
            .unwrap_or_else(|| DEFAULT_ROOM_ROLE.to_string()),
    )
}

/// Refuse a spawn that the card cannot accept, re-checking every rule the room contract has.
///
/// This runs both before the worktree is created (cheap failure) and inside the atomic board
/// update (so two concurrent spawns cannot both pass on a stale snapshot). `Err` messages name
/// the room limit, the terminal column, or `validate_team`'s own reason so the planner can act
/// without reading the source.
pub fn check_spawn_precondition(card: &BoardCard, role: &str) -> Result<(), String> {
    if let Some(error) = rooms::reject_spawn_into_card(card) {
        return Err(error);
    }
    rooms::validate_spawn_roster(card, role)?;
    card.validate_team()
}

pub fn restore_original_card_if_current_agent(
    board: &mut refact_tasks::types::TaskBoard,
    original_card: &BoardCard,
    guard_chat_id: &str,
) -> bool {
    if let Some(card) = board.get_card_mut(&original_card.id) {
        if card.agent_chat_id.as_deref() == Some(guard_chat_id) {
            *card = original_card.clone();
            return true;
        }
    }
    false
}

pub async fn rollback_retain(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    guard_chat_id: &str,
    error_msg: &str,
) {
    let card_id = card_id.to_string();
    let guard_chat_id = guard_chat_id.to_string();
    let truncated: String = error_msg.chars().take(300).collect();
    let msg = format!(
        "Spawn rolled back: {}. Worktree retained for inspection.",
        truncated
    );
    let _ = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        if let Some(card) = board.get_card_mut(&card_id) {
            if card.agent_chat_id.as_deref() == Some(&guard_chat_id) {
                card.column = "failed".to_string();
                card.status_updates.push(StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: msg,
                });
            }
        }
        Ok(Some(()))
    })
    .await;
    let _ = storage::update_task_stats_for_ctx(ctx, task_id).await;
}

pub fn build_agent_thread_params(
    agent_chat_id: &str,
    card_title: &str,
    model: &str,
    task_id: &str,
    agent_id: &str,
    card_id: &str,
    planner_chat_id: &str,
    worktree: WorktreeMeta,
) -> ThreadParams {
    ThreadParams {
        id: agent_chat_id.to_string(),
        title: format!("Agent: {}", card_title),
        model: model.to_string(),
        mode: "task_agent".to_string(),
        tool_use: "agent".to_string(),
        boost_reasoning: Some(false),
        context_tokens_cap: None,
        auto_compression_cap: None,
        auto_compression_cap_pending: true,
        include_project_info: true,
        checkpoints_enabled: false,
        is_title_generated: true,
        auto_approve_editing_tools: true,
        auto_approve_dangerous_commands: false,
        autonomous_no_confirm: false,
        task_meta: Some(TaskMeta {
            task_id: task_id.to_string(),
            role: "agents".to_string(),
            agent_id: Some(agent_id.to_string()),
            card_id: Some(card_id.to_string()),
            planner_chat_id: Some(planner_chat_id.to_string()),
        }),
        worktree: Some(worktree),
        parent_id: Some(planner_chat_id.to_string()),
        link_type: Some("task_agent".to_string()),
        root_chat_id: Some(planner_chat_id.to_string()),
        reasoning_effort: None,
        thinking_budget: None,
        temperature: None,
        frequency_penalty: None,
        max_tokens: None,
        parallel_tool_calls: None,
        previous_response_id: None,
        browser_meta: None,
        active_skill: None,
        auto_enrichment_enabled: None,
        buddy_meta: None,
        auto_compact_enabled: None,
        agent_nudge_enabled: None,
        strip_reasoning_from_prompt: None,
        frozen_request_prefix: None,
        claude_code_identity: None,
        reactive_compact_attempts: None,
    }
}

pub async fn resolve_invoking_planner_chat_id(
    gcx: Arc<GlobalContext>,
    chat_id: &str,
    root_chat_id: &str,
    task_meta: Option<&TaskMeta>,
) -> String {
    if let Some(task_meta) = task_meta {
        if !root_chat_id.is_empty()
            && root_chat_id != chat_id
            && root_chat_is_planner_for_task(gcx.clone(), root_chat_id, &task_meta.task_id).await
        {
            return root_chat_id.to_string();
        }
    }
    resolve_task_planner_controller_chat_id(
        &gcx.chat_ctx(gcx.clone()),
        chat_id,
        Some(root_chat_id),
        task_meta,
    )
    .await
}

pub async fn root_chat_is_planner_for_task(
    gcx: Arc<GlobalContext>,
    root_chat_id: &str,
    task_id: &str,
) -> bool {
    let session_arc = {
        let sessions = gcx.chat_sessions.read().await;
        sessions.get(root_chat_id).cloned()
    };
    if let Some(session_arc) = session_arc {
        let session = session_arc.lock().await;
        if session
            .thread
            .task_meta
            .as_ref()
            .is_some_and(|meta| meta.role == "planner" && meta.task_id == task_id)
        {
            return true;
        }
    }
    refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
        &gcx.chat_ctx(gcx.clone()),
        root_chat_id,
    )
    .await
    .is_some_and(|loaded| {
        loaded
            .thread
            .task_meta
            .as_ref()
            .is_some_and(|meta| meta.role == "planner" && meta.task_id == task_id)
    })
}

pub fn resolve_files_to_open_path(
    worktree_root: &Path,
    source_root: &Path,
    path_str: &str,
) -> PathBuf {
    let requested = Path::new(path_str);
    if requested.is_absolute() {
        if let Ok(relative) = requested.strip_prefix(source_root) {
            return worktree_root.join(relative);
        }
        return requested.to_path_buf();
    }
    worktree_root.join(path_str)
}

pub async fn context_files_from_files_to_open(
    gcx: Arc<GlobalContext>,
    worktree_root: &Path,
    source_root: &Path,
    files_to_open: &[String],
) -> Result<Vec<ContextFile>, String> {
    let worktree_canonical = dunce::canonicalize(worktree_root).map_err(|e| {
        format!(
            "Failed to canonicalize agent worktree '{}': {}",
            worktree_root.display(),
            e
        )
    })?;
    let mut context_files = Vec::new();
    let mut total_bytes = 0usize;
    for path_str in files_to_open {
        let resolved = resolve_files_to_open_path(worktree_root, source_root, path_str);
        let canonical_resolved = dunce::canonicalize(&resolved).map_err(|e| {
            format!(
                "files_to_open '{}' does not exist or cannot be resolved: {}",
                path_str, e
            )
        })?;
        if !canonical_resolved.starts_with(&worktree_canonical) {
            return Err(format!(
                "files_to_open '{}' resolves outside agent worktree: {}",
                path_str,
                canonical_resolved.display()
            ));
        }
        refact_runtime_api::files_in_workspace::check_file_privacy_for_model_context(
            gcx.clone(),
            &canonical_resolved,
        )
        .await
        .map_err(|e| {
            format!(
                "files_to_open '{}' is blocked by privacy settings: {}",
                path_str, e
            )
        })?;
        if total_bytes >= FILES_TO_OPEN_TOTAL_LIMIT {
            context_files.push(ContextFile {
                file_name: canonical_resolved.to_string_lossy().to_string(),
                ..Default::default()
            });
            continue;
        }
        let bytes = tokio::fs::read(&canonical_resolved).await.map_err(|e| {
            format!(
                "Could not read files_to_open '{}': {}",
                canonical_resolved.display(),
                e
            )
        })?;
        let remaining_total = FILES_TO_OPEN_TOTAL_LIMIT - total_bytes;
        let read_limit = FILES_TO_OPEN_PER_FILE_LIMIT.min(remaining_total);
        let bytes_to_include = bytes.len().min(read_limit);
        let omitted_bytes = bytes.len().saturating_sub(bytes_to_include);
        let mut content = String::from_utf8_lossy(&bytes[..bytes_to_include]).to_string();
        if omitted_bytes > 0 {
            content.push_str(&format!("\n... (truncated, {} more bytes)", omitted_bytes));
        }
        total_bytes += bytes_to_include;
        let line_count = content.lines().count().max(1);
        context_files.push(ContextFile {
            file_name: canonical_resolved.to_string_lossy().to_string(),
            file_content: content,
            line1: 1,
            line2: line_count,
            ..Default::default()
        });
    }
    Ok(context_files)
}

fn initial_file_metrics(context_files: &[ContextFile]) -> (usize, usize) {
    (
        context_files.len(),
        context_files
            .iter()
            .map(|file| file.file_content.len())
            .sum(),
    )
}

pub fn context_file_message(
    gcx: &Arc<GlobalContext>,
    context_files: Vec<ContextFile>,
) -> Result<ChatMessage, String> {
    let records = refact_runtime_api::gcx_helpers::declared_file_records(
        gcx,
        context_files
            .iter()
            .map(|file| PathBuf::from(&file.file_name)),
    )?;
    let mut message = ChatMessage {
        role: "context_file".to_string(),
        content: ChatContent::ContextFiles(context_files),
        tool_call_id: "initial_files".to_string(),
        ..Default::default()
    };
    refact_privacy::merge_records(&mut message, records);
    Ok(message)
}

#[async_trait]
impl Tool for ToolTaskSpawnAgent {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "spawn_agent".to_string(),
            display_name: "Task Spawn Agent".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Spawn an agent to work on a specific task card. The agent runs in the background as a real chat session. Returns immediately with a hyperlink to view the agent's progress. The agent will call agent_finish() when done. Spawning onto a card that already has members adds a room member instead of replacing the agent; pass `role` and `role_mandate` to staff the room.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "card_id": {
                        "type": "string",
                        "description": "Card ID to work on"
                    },
                    "role": {
                        "type": "string",
                        "description": "Free-text name for this member's part of the work (default: coder). Multi-member rooms require exactly one writer from the agent definition; spawn that writer role before any advisors. The architect label is sequential: it cannot run while another member is active."
                    },
                    "role_mandate": {
                        "type": "string",
                        "description": "What this specific member is responsible for, recorded on the room roster so peers can read it."
                    },
                    "suggested_steps": {
                        "type": "integer",
                        "description": "Suggested step budget for the agent (default: 30). This is a hint, not enforced."
                    },
                    "files_to_open": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of file paths to open immediately when the agent starts. The agent will see these files as context at the beginning of its session."
                    },
                    "base_branch": { "type": "string", "description": "Optional merge target branch for this card" },
                    "base_commit": { "type": "string", "description": "Optional exact commit from which to create this card's agent branch" }
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
        let ccx_lock = ccx.lock().await;

        let is_planner = ccx_lock
            .task_meta
            .as_ref()
            .map(|m| m.role == "planner")
            .unwrap_or(false);

        if !is_planner {
            return Err("spawn_agent can only be called by the task planner. \
                 Switch to the planner chat to spawn agents."
                .to_string());
        }

        drop(ccx_lock);

        let (app, current_model, task_id, chat_id, root_chat_id, task_meta, planner_worktree) = {
            let ccx_lock = ccx.lock().await;
            let task_id = ccx_lock
                .task_meta
                .as_ref()
                .map(|m| m.task_id.clone())
                .unwrap_or_else(|| {
                    args.get("task_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default()
                });
            (
                ccx_lock.app.clone(),
                ccx_lock.current_model.clone(),
                task_id,
                ccx_lock.chat_id.clone(),
                ccx_lock.root_chat_id.clone(),
                ccx_lock.task_meta.clone(),
                ccx_lock
                    .execution_scope
                    .as_ref()
                    .map(|scope| scope.worktree().clone()),
            )
        };
        let ctx = app.chat_ctx();
        let tool = app.tool.clone();
        let planner_chat_id = resolve_invoking_planner_chat_id(
            app.gcx.clone(),
            &chat_id,
            &root_chat_id,
            task_meta.as_ref(),
        )
        .await;
        let task_id = if task_id.is_empty() {
            get_task_id(&ccx, args).await?
        } else {
            task_id
        };
        let card_id = args
            .get("card_id")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'card_id'")?;
        let files_to_open = validate_files_to_open(args)?;
        let suggested_steps: usize = match args
            .get("suggested_steps")
            .or_else(|| args.get("max_steps"))
        {
            Some(Value::String(s)) => s.parse().unwrap_or(30),
            Some(Value::Number(n)) => n.as_u64().unwrap_or(30) as usize,
            _ => 30,
        };
        let suggested_steps = suggested_steps.min(50).max(1);

        let agent_id = Uuid::new_v4().to_string();
        let agent_chat_id = format!("agent-{}-{}", card_id, &agent_id[..8]);
        let mut diagnostics =
            SpawnPreparationDiagnostics::new(&task_id, card_id, &agent_id, &agent_chat_id);

        let task_meta = storage::load_task_meta_for_ctx(&ctx, &task_id).await?;
        let explicit_base_branch = optional_nonempty_string_arg(args, "base_branch")?;
        let explicit_base_commit = optional_nonempty_string_arg(args, "base_commit")?;

        let model = resolve_agent_model(&tool.caps, &current_model).await?;
        crate::tools::task_tool_helpers::preflight_agent_model(&app.chat_ctx(), &model).await?;

        validate_id(&task_id, "task_id")?;
        validate_id(card_id, "card_id")?;

        diagnostics.transition("abandoned_preflight", "abandoned_preflight_started");
        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let abandoned_worktrees = find_abandoned_worktrees(&board);
        if !abandoned_worktrees.is_empty() {
            return Err(abandoned_worktrees_error(
                "spawn a new task agent",
                &abandoned_worktrees,
            ));
        }
        diagnostics.transition("abandoned_preflight", "abandoned_preflight_done");
        diagnostics.set_stage("card_validation");

        let card = board
            .get_card(card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        if card.column == "done" {
            return Err(format!("Card {} is already done", card_id));
        }
        if card.column == "failed" {
            return Err(format!(
                "Card {} has failed. Reset it first if you want to retry.",
                card_id
            ));
        }
        if card.column != "planned" && card.column != "doing" {
            return Err(format!(
                "Card {} is in column '{}', expected 'planned' or 'doing'",
                card_id, card.column
            ));
        }
        let role = spawn_role(args)?;
        check_spawn_precondition(card, &role)?;
        let mandate = optional_nonempty_string_arg(args, "role_mandate")?;

        let project_dirs =
            refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(&ctx).await;
        let workspace_root = project_dirs.first().ok_or_else(|| {
            "No workspace folder found; task agents require an isolated git worktree".to_string()
        })?;
        let (base_branch, base_commit) = resolve_spawn_base(
            explicit_base_branch,
            explicit_base_commit,
            planner_worktree.as_ref(),
            workspace_root,
            &task_meta,
        );
        diagnostics.transition("worktree_prepare", "worktree_prepare_started");
        let prepared_worktree = prepare_agent_worktree_with_base_and_suffix(
            &ctx,
            &task_meta,
            &task_id,
            &agent_id,
            card_id,
            &agent_chat_id,
            base_branch,
            base_commit,
            None,
        )
        .await?;
        diagnostics.transition("worktree_prepare", "worktree_ready");

        let dirty_tree_warning = prepared_worktree.spawned_with_dirty_tree;
        let base_branch_mismatch_warning = prepared_worktree.base_branch_mismatch_warning.clone();

        diagnostics.transition("initial_files_read", "initial_files_read_started");
        let initial_context_files = if files_to_open.is_empty() {
            Vec::new()
        } else {
            let worktree_path = prepared_worktree.worktree_path();
            let source_workspace_root = prepared_worktree.source_workspace_root();
            match context_files_from_files_to_open(
                app.gcx.clone(),
                &worktree_path,
                &source_workspace_root,
                &files_to_open,
            )
            .await
            {
                Ok(context_files) => context_files,
                Err(e) => {
                    diagnostics.fail();
                    prepared_worktree
                        .cleanup_unlinked(app.paths.cache_dir.clone())
                        .await;
                    return Err(e);
                }
            }
        };
        let (initial_files_count, initial_files_bytes) =
            initial_file_metrics(&initial_context_files);
        diagnostics.transition_with_file_metrics(
            "initial_files_read",
            "initial_files_read_done",
            initial_files_count,
            initial_files_bytes,
        );
        let initial_context_message = if initial_context_files.is_empty() {
            None
        } else {
            match context_file_message(&app.gcx, initial_context_files) {
                Ok(message) => Some(message),
                Err(e) => {
                    diagnostics.fail();
                    prepared_worktree
                        .cleanup_unlinked(app.paths.cache_dir.clone())
                        .await;
                    return Err(e);
                }
            }
        };
        diagnostics.set_stage("card_session_preparation");

        let card_id_owned = card_id.to_string();
        let agent_id_clone = agent_id.clone();
        let agent_chat_id_clone = agent_chat_id.clone();
        let role_clone = role.clone();
        let mandate_clone = mandate.clone();
        // The tool names this member actually gets, recorded on the card so peers can see what
        // their neighbours are able to do. Resolved from the member's own mode, not from the
        // template it was spawned from: the allowed set is a per-spawn fact.
        let member_tools: Vec<String> = tool
            .tool_registry
            .get_tools_for_mode("task_agent", Some(&model))
            .await
            .into_iter()
            .map(|description| description.name)
            .collect();
        let worktree_branch = prepared_worktree.branch_name();
        let worktree_path_str = Some(
            prepared_worktree
                .worktree_path()
                .to_string_lossy()
                .to_string(),
        );
        let worktree_name = Some(prepared_worktree.worktree_name());
        let base_branch_from_prep = prepared_worktree.meta.base_branch.clone();
        let base_commit_from_prep = prepared_worktree.meta.base_commit.clone();

        let board_update_result =
            storage::update_board_atomic_for_ctx(&ctx, &task_id, move |board| {
                let agents_active_before = board
                    .cards
                    .iter()
                    .filter(|c| c.column == "doing" && c.assignee.is_some())
                    .count();

                let card = board
                    .get_card_mut(&card_id_owned)
                    .ok_or(format!("Card {} not found", card_id_owned))?;

                if card.column == "done" {
                    return Err(format!("Card {} is already done", card_id_owned));
                }
                if card.column == "failed" {
                    return Err(format!(
                        "Card {} has failed. Reset it first if you want to retry.",
                        card_id_owned
                    ));
                }
                if card.column != "planned" && card.column != "doing" {
                    return Err(format!(
                        "Card {} is in column '{}', expected 'planned' or 'doing'",
                        card_id_owned, card.column
                    ));
                }
                check_spawn_precondition(card, &role_clone)?;
                claim_room_slot(
                    card,
                    &role_clone,
                    &agent_id_clone,
                    &agent_chat_id_clone,
                    worktree_branch.clone(),
                    worktree_path_str.clone(),
                    worktree_name.clone(),
                    mandate_clone.clone(),
                    member_tools.clone(),
                )
                .map_err(|error| format!("Card {card_id_owned}: {error}"))?;
                card.validate_team()
                    .map_err(|error| format!("Card {card_id_owned}: {error}"))?;
                card.base_branch = base_branch_from_prep.clone();
                card.base_commit = base_commit_from_prep.clone();

                Ok(Some(agents_active_before == 0))
            })
            .await;
        let (board, commit_info) = match board_update_result {
            Ok(result) => result,
            Err(e) => {
                diagnostics.fail();
                prepared_worktree
                    .cleanup_unlinked(app.paths.cache_dir.clone())
                    .await;
                return Err(e);
            }
        };

        let starting_new_run = commit_info.unwrap();

        if let Err(e) = storage::update_task_stats_for_ctx(&ctx, &task_id).await {
            diagnostics.fail();
            rollback_retain(&ctx, &task_id, card_id, &agent_chat_id, &e).await;
            prepared_worktree.retain().await;
            return Err(e);
        }

        let mut meta = match storage::load_task_meta_for_ctx(&ctx, &task_id).await {
            Ok(m) => m,
            Err(e) => {
                diagnostics.fail();
                rollback_retain(&ctx, &task_id, card_id, &agent_chat_id, &e).await;
                prepared_worktree.retain().await;
                return Err(e);
            }
        };
        if starting_new_run {
            meta.last_agents_summary_at = Some(Utc::now().to_rfc3339());
        } else if meta.last_agents_summary_at.is_none() {
            let earliest = board
                .cards
                .iter()
                .filter(|c| c.column == "doing" && c.assignee.is_some())
                .filter_map(|c| c.started_at.as_deref())
                .filter_map(parse_rfc3339_to_utc)
                .min();
            meta.last_agents_summary_at = Some(earliest.unwrap_or_else(Utc::now).to_rfc3339());
        }
        if let Err(e) = storage::save_task_meta_for_ctx(&ctx, &task_id, &meta).await {
            diagnostics.fail();
            rollback_retain(&ctx, &task_id, card_id, &agent_chat_id, &e).await;
            prepared_worktree.retain().await;
            return Err(e);
        }

        let card_title = board
            .get_card(card_id)
            .map(|c| c.title.clone())
            .unwrap_or_default();
        let card_instructions = board
            .get_card(card_id)
            .map(|c| c.instructions.clone())
            .unwrap_or_default();
        let dependency_context = board
            .get_dependency_reports(card_id)
            .into_iter()
            .map(|(title, report)| format!("### {}\n{}", title, report))
            .collect::<Vec<_>>()
            .join("\n\n");

        let thread = build_agent_thread_params(
            &agent_chat_id,
            &card_title,
            &model,
            &task_id,
            &agent_id,
            card_id,
            &planner_chat_id,
            prepared_worktree.meta.clone(),
        );
        let user_prompt = match board
            .get_card(card_id)
            .map(|card| rooms::describe_room(card))
            .filter(|roster| !roster.is_empty() && roster.len() > 1)
        {
            Some(roster) => build_room_member_prompt(
                &card_title,
                &card_instructions,
                &dependency_context,
                suggested_steps,
                &role,
                mandate.as_deref(),
                &roster,
            ),
            None => build_agent_prompt(
                &card_title,
                &card_instructions,
                &dependency_context,
                suggested_steps,
            ),
        };
        let user_msg = event(
            EventSubkind::SystemNotice,
            "tool.task_spawn_agent",
            json!({
                "task_id": &task_id,
                "planner_chat_id": &planner_chat_id,
                "card_id": card_id,
                "card_title": &card_title,
                "agent_id": &agent_id,
                "agent_chat_id": &agent_chat_id,
                "model": &model,
                "suggested_steps": suggested_steps,
                "files_to_open": &files_to_open,
                "worktree": prepared_worktree.worktree_path().to_string_lossy().to_string(),
                "worktree_name": prepared_worktree.worktree_name(),
                "branch": prepared_worktree.branch_name(),
            }),
            user_prompt,
        );
        let mut messages = vec![user_msg];

        if let Some(context_message) = initial_context_message {
            messages.push(context_message);
        }

        diagnostics.transition("session_create", "session_create_started");
        let session_result = async {
            app.chat
                .facade
                .create_session(CreateSessionRequest {
                    chat_id: agent_chat_id.clone(),
                    thread,
                    messages,
                })
                .await?;
            app.chat.facade.maybe_save_session(&agent_chat_id).await?;
            diagnostics.transition("generation_queue", "session_ready");
            app.chat
                .facade
                .push_priority_command(&agent_chat_id, ChatCommand::Regenerate {})
                .await
        }
        .await;
        if let Err(e) = session_result {
            diagnostics.fail();
            rollback_retain(&ctx, &task_id, card_id, &agent_chat_id, &e).await;
            prepared_worktree.retain().await;
            return Err(e);
        }
        diagnostics.complete("generation_queue", "generation_queued");

        tracing::info!(
            "Spawned agent {} for card {}: {} (model: {})",
            agent_id,
            card_id,
            card_title,
            model
        );

        let dirty_note = if dirty_tree_warning {
            "\n\n⚠️ Note: agent works from the committed base. Your uncommitted local changes are not included in the agent's worktree."
        } else {
            ""
        };
        let branch_note = base_branch_mismatch_warning
            .as_ref()
            .map(|warning| format!("\n\n⚠️ Note: {}", warning))
            .unwrap_or_default();

        let result_message = format!(
            r#"# Agent Spawned: {}

**Card:** {}
**Agent ID:** {}
**Model:** {}
**Status:** Running in background

The agent will call `agent_finish()` when done. Use `check_agents` to monitor progress.{}{}"#,
            card_title, card_id, agent_id, model, dirty_note, branch_note
        );

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result_message),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod spawn_preparation_diagnostics_tests {
    use super::*;

    fn correlation() -> SpawnPreparationCorrelation {
        SpawnPreparationCorrelation {
            task_id: "task-1".to_string(),
            card_id: "BUG-005B".to_string(),
            agent_id: "agent-1".to_string(),
            agent_chat_id: "agent-chat-1".to_string(),
        }
    }

    #[test]
    fn timeline_has_fake_elapsed_correlation_and_file_metrics() {
        let mut diagnostics = SpawnPreparationDiagnostics::new_at(correlation(), 1_000);
        diagnostics.transition_at(
            "abandoned_preflight",
            "abandoned_preflight_started",
            1_100,
            Duration::from_millis(100),
            None,
        );
        diagnostics.transition_at(
            "abandoned_preflight",
            "abandoned_preflight_done",
            1_250,
            Duration::from_millis(250),
            None,
        );
        diagnostics.transition_at(
            "worktree_prepare",
            "worktree_prepare_started",
            1_300,
            Duration::from_millis(300),
            None,
        );
        diagnostics.transition_at(
            "worktree_prepare",
            "worktree_ready",
            1_800,
            Duration::from_millis(800),
            None,
        );
        diagnostics.transition_at(
            "initial_files_read",
            "initial_files_read_started",
            1_850,
            Duration::from_millis(850),
            None,
        );
        let files = vec![
            ContextFile {
                file_content: "abcd".to_string(),
                ..Default::default()
            },
            ContextFile {
                file_content: "ef".to_string(),
                ..Default::default()
            },
        ];
        let (count, bytes) = initial_file_metrics(&files);
        diagnostics.transition_at(
            "initial_files_read",
            "initial_files_read_done",
            1_950,
            Duration::from_millis(950),
            Some((count, bytes)),
        );
        diagnostics.transition_at(
            "session_create",
            "session_create_started",
            2_000,
            Duration::from_millis(1_000),
            None,
        );
        diagnostics.transition_at(
            "generation_queue",
            "session_ready",
            2_200,
            Duration::from_millis(1_200),
            None,
        );
        diagnostics.complete_at(
            "generation_queue",
            "generation_queued",
            2_250,
            Duration::from_millis(1_250),
        );

        let names = diagnostics
            .events
            .iter()
            .map(|event| event.event)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "spawn_accepted",
                "abandoned_preflight_started",
                "abandoned_preflight_done",
                "worktree_prepare_started",
                "worktree_ready",
                "initial_files_read_started",
                "initial_files_read_done",
                "session_create_started",
                "session_ready",
                "generation_queued",
            ]
        );
        assert_eq!(diagnostics.events[0].elapsed_ms, 0);
        assert_eq!(diagnostics.events[4].elapsed_ms, 800);
        assert_eq!(diagnostics.events[6].files_count, Some(2));
        assert_eq!(diagnostics.events[6].files_bytes, Some(6));
        assert_eq!(diagnostics.events[9].timestamp_ms, 2_250);
        assert_eq!(diagnostics.events[9].elapsed_ms, 1_250);
        assert!(diagnostics
            .events
            .iter()
            .all(|event| event.correlation == correlation()));
    }

    #[test]
    fn error_stage_is_terminal_and_suppresses_later_ready_events() {
        let mut diagnostics = SpawnPreparationDiagnostics::new_at(correlation(), 1_000);
        diagnostics.transition_at(
            "worktree_prepare",
            "worktree_prepare_started",
            1_100,
            Duration::from_millis(100),
            None,
        );
        diagnostics.fail_at(1_400, Duration::from_millis(400));
        diagnostics.transition_at(
            "worktree_prepare",
            "worktree_ready",
            1_500,
            Duration::from_millis(500),
            None,
        );

        assert_eq!(diagnostics.events.len(), 3);
        let error = diagnostics.events.last().unwrap();
        assert_eq!(error.event, "spawn_error");
        assert_eq!(error.error_stage, Some("worktree_prepare"));
        assert_eq!(error.elapsed_ms, 400);
        assert_eq!(error.timestamp_ms, 1_400);
    }

    #[test]
    fn events_do_not_serialize_instruction_content_or_worktree_paths() {
        let secret = "private card instructions and provider message content";
        let worktree_path = "C:/private/full/worktree/path";
        let mut diagnostics = SpawnPreparationDiagnostics::new_at(correlation(), 1_000);
        diagnostics.transition_at(
            "initial_files_read",
            "initial_files_read_done",
            1_100,
            Duration::from_millis(100),
            Some((0, 0)),
        );
        diagnostics.fail_at(1_200, Duration::from_millis(200));
        let rendered = serde_json::to_string(&diagnostics.events).unwrap();

        assert!(!rendered.contains(secret));
        assert!(!rendered.contains(worktree_path));
        assert!(!rendered.contains("instructions"));
        assert!(!rendered.contains("file_content"));
        assert!(!rendered.contains("worktree_path"));
        assert!(rendered.contains("initial_files_read"));
        assert!(rendered.contains("spawn_error"));
    }
}
