use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, warn};

use refact_core::call_validation::ChatMessage;
use crate::chat_ctx::ChatCtx;
use refact_chat_api::types::{ThreadParams, TrajectoryCommitIntent, TrajectorySourceIdentity};
use crate::files_correction::get_unscoped_project_dirs_for_ctx;
use refact_worktrees::service::WorktreeService;
use refact_worktrees::types::WorktreeMeta;
use refact_chat_api::{GoalLedgerEntry, GoalSnapshot};

use super::model::{clamp_goal_snapshot_for_load, LoadedTrajectory, TrajectoryData};
use super::paths::{
    chat_folder_name, find_normal_trajectory_file, find_trajectory_file,
    fix_tool_call_indexes, get_all_task_roots, get_all_trajectories_dirs,
    get_buddy_conversations_dir, get_or_create_buddy_conversations_dir, get_trajectories_dir,
    list_task_trajectory_dirs, parse_stored_messages, read_existing_trajectory_object,
    read_valid_trajectory_candidate, trajectory_candidate_paths, validate_trajectory_id,
    ValidTrajectoryCandidate,
};
use super::{
    canonical_child_path_under_root, ensure_real_dir_tree, is_real_dir, is_real_file,
    safe_new_trajectory_file_in_dir,
};
use super::reload::{parsed_rfc3339_utc, raw_frozen_prefix_has_tools, repair_transition_frozen_prefix};
use super::watcher::trajectory_source_identity_from_path;

pub fn is_known_trajectory_top_level_key(key: &str) -> bool {
    matches!(
        key,
        "id" | "title"
            | "model"
            | "mode"
            | "tool_use"
            | "messages"
            | "created_at"
            | "updated_at"
            | "boost_reasoning"
            | "checkpoints_enabled"
            | "context_tokens_cap"
            | "auto_compression_cap"
            | "auto_compression_cap_pending"
            | "include_project_info"
            | "isTitleGenerated"
            | "auto_approve_editing_tools"
            | "auto_approve_dangerous_commands"
            | "autonomous_no_confirm"
            | "reasoning_effort"
            | "thinking_budget"
            | "temperature"
            | "frequency_penalty"
            | "max_tokens"
            | "previous_response_id"
            | "parallel_tool_calls"
            | "active_skill"
            | "auto_enrichment_enabled"
            | "buddy_meta"
            | "auto_compact_enabled"
            | "agent_nudge_enabled"
            | "frozen_request_prefix"
            | "claude_code_identity"
            | "reactive_compact_attempts"
            | "wake_up_at"
            | "waiting_for_card_ids"
            | "pending_deliveries"
            | "worktree"
            | "parent_id"
            | "link_type"
            | "root_chat_id"
            | "task_meta"
            | "goal"
            | "goal_ledger"
            | "goal_verification_blocked_until_ms"
            | "browser_meta"
    )
}

pub fn preserve_existing_trajectory_metadata(
    trajectory: &mut serde_json::Value,
    existing: Option<serde_json::Map<String, serde_json::Value>>,
) {
    let Some(existing) = existing else {
        return;
    };
    let Some(trajectory_object) = trajectory.as_object_mut() else {
        return;
    };
    for (key, value) in existing {
        let preserve_browser_meta = key == "browser_meta" && !trajectory_object.contains_key(&key);
        let preserve_unknown =
            !is_known_trajectory_top_level_key(&key) && !trajectory_object.contains_key(&key);
        if preserve_browser_meta || preserve_unknown {
            trajectory_object.insert(key, value);
        }
    }
}

pub fn validate_task_trajectory_role(role: &str) -> Result<(), String> {
    match role {
        "planner" | "agents" | "subchats" => Ok(()),
        _ => Err(format!("Invalid task trajectory role: {role}")),
    }
}

pub fn validate_task_agent_id(agent_id: Option<&str>) -> Result<(), String> {
    if let Some(agent_id) = agent_id {
        validate_trajectory_id(agent_id).map_err(|e| e.message)?;
    }
    Ok(())
}

pub async fn safe_task_dir(ctx: &ChatCtx, task_id: &str) -> Result<PathBuf, String> {
    crate::task_storage_runtime::validate_task_id(task_id)?;
    for tasks_dir in get_all_task_roots(ctx).await {
        let candidate = tasks_dir.join(task_id);
        if is_real_dir(&candidate).await {
            return Ok(candidate);
        }
    }
    Err(format!("Task not found: {task_id}"))
}

pub async fn safe_task_trajectory_dir(
    ctx: &ChatCtx,
    task_meta: &refact_chat_api::types::TaskMeta,
) -> Result<PathBuf, String> {
    validate_task_trajectory_role(&task_meta.role)?;
    validate_task_agent_id(task_meta.agent_id.as_deref())?;
    let task_dir = safe_task_dir(ctx, &task_meta.task_id).await?;
    let traj_dir = crate::task_storage_runtime::get_task_trajectory_dir(
        &task_dir,
        &task_meta.role,
        task_meta.agent_id.as_deref(),
    );
    ensure_real_dir_tree(&traj_dir).await?;
    Ok(traj_dir)
}

pub async fn safe_new_task_trajectory_file(
    ctx: &ChatCtx,
    task_meta: &refact_chat_api::types::TaskMeta,
    chat_id: &str,
) -> Result<PathBuf, String> {
    let traj_dir = safe_task_trajectory_dir(ctx, task_meta).await?;
    safe_new_trajectory_file_in_dir(&traj_dir, chat_id).await
}

pub async fn safe_new_buddy_trajectory_file(
    ctx: &ChatCtx,
    chat_id: &str,
) -> Result<PathBuf, String> {
    let buddy_dir = get_or_create_buddy_conversations_dir(ctx).await?;
    safe_new_trajectory_file_in_dir(&buddy_dir, chat_id).await
}

pub async fn ensure_legacy_flat_slot_is_safe(
    ctx: &ChatCtx,
    chat_id: &str,
) -> Result<(), String> {
    let trajectories_dir = get_trajectories_dir(ctx).await?;
    let legacy_path = safe_new_trajectory_file_in_dir(&trajectories_dir, chat_id).await?;
    if is_real_file(&legacy_path).await {
        read_existing_trajectory_object(&legacy_path, chat_id).await?;
    }
    Ok(())
}

pub async fn safe_new_nested_trajectory_file(
    ctx: &ChatCtx,
    chat_id: &str,
    root_chat_id: Option<&str>,
) -> Result<PathBuf, String> {
    validate_trajectory_id(chat_id).map_err(|e| e.message)?;
    ensure_legacy_flat_slot_is_safe(ctx, chat_id).await?;
    let folder = chat_folder_name(chat_id, root_chat_id);
    let base = get_trajectories_dir(ctx).await?;
    let dir = base.join(folder);
    ensure_real_dir_tree(&dir).await?;
    safe_new_trajectory_file_in_dir(&dir, chat_id).await
}

pub async fn trajectory_source_path_is_allowed(
    ctx: &ChatCtx,
    path: &Path,
) -> bool {
    if !is_real_file(path).await {
        return false;
    }

    for root in get_all_trajectories_dirs(ctx).await {
        if canonical_child_path_under_root(&root, path).await.is_ok() {
            return true;
        }
    }
    for root in list_task_trajectory_dirs(ctx).await {
        if canonical_child_path_under_root(&root, path).await.is_ok() {
            return true;
        }
    }
    if let Ok(root) = get_buddy_conversations_dir(ctx).await {
        if is_real_dir(&root).await && canonical_child_path_under_root(&root, path).await.is_ok() {
            return true;
        }
    }
    false
}

pub async fn resolve_trajectory_data_save_path(
    ctx: &ChatCtx,
    id: &str,
    data: &TrajectoryData,
) -> Result<PathBuf, String> {
    validate_trajectory_id(id).map_err(|e| e.message)?;
    if data.id != id {
        return Err("ID mismatch".to_string());
    }

    let task_meta_value = data.extra.get("task_meta").filter(|value| !value.is_null());
    let buddy_meta_present = data
        .extra
        .get("buddy_meta")
        .is_some_and(|value| !value.is_null());

    if task_meta_value.is_some() && buddy_meta_present {
        return Err("Trajectory cannot contain both task_meta and buddy_meta".to_string());
    }

    if let Some(value) = task_meta_value {
        let task_meta = serde_json::from_value::<refact_chat_api::types::TaskMeta>(value.clone())
            .map_err(|e| format!("Invalid task_meta: {}", e))?;
        return safe_new_task_trajectory_file(ctx, &task_meta, id).await;
    }

    if buddy_meta_present {
        return safe_new_buddy_trajectory_file(ctx, id).await;
    }

    if let Some(candidate) = find_normal_trajectory_file(ctx, id).await {
        return Ok(candidate.path);
    }
    let root_chat_id = data
        .extra
        .get("root_chat_id")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    safe_new_nested_trajectory_file(ctx, id, root_chat_id.as_deref()).await
}

pub async fn title_generation_backing_file_matches(
    file_path: &Path,
    chat_id: &str,
    source: &TrajectorySourceIdentity,
) -> bool {
    let content = match fs::read_to_string(file_path).await {
        Ok(content) => content,
        Err(e) => {
            warn!(
                "Skipping title update for {}: failed to read backing trajectory: {}",
                file_path.display(),
                e
            );
            return false;
        }
    };
    let parsed = tokio::task::spawn_blocking(move || serde_json::from_str(&content)).await;
    let json: serde_json::Value = match parsed {
        Ok(Ok(json)) => json,
        Ok(Err(e)) => {
            warn!(
                "Skipping title update for {}: failed to parse backing trajectory: {}",
                file_path.display(),
                e
            );
            return false;
        }
        Err(e) => {
            warn!(
                "Skipping title update for {}: parse task failed: {}",
                file_path.display(),
                e
            );
            return false;
        }
    };
    if !trajectory_root_id_matches(&json, chat_id, file_path) {
        return false;
    }
    let actual_source = match TrajectorySourceIdentity::from_json(&json) {
        Ok(source) => source,
        Err(e) => {
            warn!(
                "Skipping title update for {}: backing trajectory source is invalid: {}",
                file_path.display(),
                e
            );
            return false;
        }
    };
    if &actual_source != source {
        warn!(
            "Skipping title update for {}: backing trajectory source mismatch, expected {:?}, found {:?}",
            file_path.display(),
            source,
            actual_source
        );
        return false;
    }
    true
}

pub fn repaired_created_at(raw_created_at: Option<&str>, transition_identity_repaired: bool) -> String {
    if transition_identity_repaired {
        if let Some(created_at) = raw_created_at {
            if parsed_rfc3339_utc(created_at).is_some() {
                return created_at.to_string();
            }
        }
        return chrono::Utc::now().to_rfc3339();
    }

    raw_created_at
        .map(ToString::to_string)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
}

pub fn parse_worktree_meta(value: &serde_json::Value) -> Option<WorktreeMeta> {
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

pub fn trajectory_worktree_from_extra(
    extra: &serde_json::Map<String, serde_json::Value>,
) -> Option<WorktreeMeta> {
    extra.get("worktree").and_then(parse_worktree_meta)
}

pub fn sanitize_worktree_extra(
    extra: &mut serde_json::Map<String, serde_json::Value>,
) -> Option<WorktreeMeta> {
    let Some(value) = extra.get("worktree") else {
        return None;
    };
    if value.is_null() {
        extra.remove("worktree");
        return None;
    }
    match serde_json::from_value::<WorktreeMeta>(value.clone()) {
        Ok(worktree) => Some(worktree),
        Err(e) => {
            warn!("Ignoring invalid trajectory worktree metadata: {}", e);
            extra.remove("worktree");
            None
        }
    }
}

pub async fn worktree_service_for_ctx(
    ctx: &ChatCtx,
    requested_source_root: Option<&Path>,
) -> Result<WorktreeService, String> {
    let cache_dir = ctx.paths.cache_dir.clone();
    let project_dirs = get_unscoped_project_dirs_for_ctx(ctx).await;
    if project_dirs.is_empty() {
        return Err("No project root available".to_string());
    }
    let source_root = match requested_source_root {
        Some(requested) => {
            let requested = tokio::fs::canonicalize(requested).await.map_err(|e| {
                format!(
                    "Failed to resolve worktree source root '{}': {}",
                    requested.display(),
                    e
                )
            })?;
            let requested = dunce::simplified(&requested).to_path_buf();
            let mut matches = false;
            for dir in &project_dirs {
                if tokio::fs::canonicalize(dir)
                    .await
                    .map(|canonical| dunce::simplified(&canonical).to_path_buf() == requested)
                    .unwrap_or(false)
                {
                    matches = true;
                    break;
                }
            }
            if !matches {
                return Err("Worktree source root is not a current workspace directory".to_string());
            }
            requested
        }
        None => project_dirs[0].clone(),
    };
    WorktreeService::new_async(cache_dir, source_root).await
}

pub async fn validate_loaded_worktree_strict(
    ctx: &ChatCtx,
    chat_id: &str,
    worktree: WorktreeMeta,
) -> Option<WorktreeMeta> {
    let service =
        match worktree_service_for_ctx(ctx, Some(&worktree.source_workspace_root)).await {
            Ok(service) => service,
            Err(e) => {
                warn!(
                    "Ignoring trajectory worktree metadata for chat {}: {}",
                    chat_id, e
                );
                return None;
            }
        };
    match service.validate_worktree_meta_strict(&worktree).await {
        Ok(validated) => Some(validated),
        Err(e) => {
            debug!(
                "Ignoring untrusted trajectory worktree metadata for chat {}: {}",
                chat_id, e
            );
            None
        }
    }
}

pub async fn validate_loaded_legacy_task_agent_worktree(
    ctx: &ChatCtx,
    chat_id: &str,
    worktree: WorktreeMeta,
) -> Option<WorktreeMeta> {
    let service =
        match worktree_service_for_ctx(ctx, Some(&worktree.source_workspace_root)).await {
            Ok(service) => service,
            Err(e) => {
                warn!(
                    "Ignoring legacy task-agent worktree metadata for chat {}: {}",
                    chat_id, e
                );
                return None;
            }
        };
    match service
        .validate_legacy_task_agent_worktree_meta(&worktree)
        .await
    {
        Ok(validated) => Some(validated),
        Err(e) => {
            warn!(
                "Ignoring untrusted legacy task-agent worktree metadata for chat {}: {}",
                chat_id, e
            );
            None
        }
    }
}

pub async fn synthesize_legacy_task_agent_worktree(
    ctx: &ChatCtx,
    chat_id: &str,
    task_meta: Option<&refact_chat_api::types::TaskMeta>,
) -> Option<WorktreeMeta> {
    let task_meta = task_meta?;
    if task_meta.role != "agents" {
        return None;
    }

    let source_workspace_root = get_unscoped_project_dirs_for_ctx(ctx)
        .await
        .into_iter()
        .next();
    let task_record = crate::task_storage_runtime::load_task_meta_for_ctx(ctx, &task_meta.task_id)
        .await
        .ok();
    let board = crate::task_storage_runtime::load_board_for_ctx(ctx, &task_meta.task_id)
        .await
        .ok()?;

    let card = if let Some(card_id) = task_meta.card_id.as_deref() {
        board.cards.iter().find(|card| card.id == card_id)?
    } else {
        board
            .cards
            .iter()
            .find(|card| card.agent_chat_id.as_deref() == Some(chat_id))?
    };
    if card.agent_chat_id.as_deref() != Some(chat_id) {
        return None;
    }
    if let Some(agent_id) = task_meta.agent_id.as_deref() {
        if card.assignee.as_deref() != Some(agent_id) {
            return None;
        }
    }

    let root = PathBuf::from(card.agent_worktree.as_ref()?);
    let source_workspace_root = source_workspace_root.unwrap_or_else(|| root.clone());
    let id = card
        .agent_worktree_name
        .clone()
        .or_else(|| {
            root.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string())
        })
        .unwrap_or_else(|| format!("{}-{}", task_meta.task_id, card.id));

    Some(WorktreeMeta {
        id,
        kind: "task_agent".to_string(),
        root,
        source_workspace_root: source_workspace_root.clone(),
        repo_root: source_workspace_root,
        branch: card.agent_branch.clone(),
        base_branch: if card.base_branch.is_some() || card.base_commit.is_some() {
            card.base_branch.clone()
        } else {
            task_record
                .as_ref()
                .and_then(|meta| meta.base_branch.clone())
        },
        base_commit: if card.base_branch.is_some() || card.base_commit.is_some() {
            card.base_commit.clone()
        } else {
            task_record
                .as_ref()
                .and_then(|meta| meta.base_commit.clone())
        },
        task_id: Some(task_meta.task_id.clone()),
        card_id: Some(card.id.clone()),
        agent_id: task_meta.agent_id.clone().or_else(|| card.assignee.clone()),
        enforce: true,
    })
}

pub fn trajectory_root_id_matches(
    t: &serde_json::Value,
    chat_id: &str,
    path: &Path,
) -> bool {
    match t.get("id").and_then(|value| value.as_str()) {
        Some(id) if id == chat_id => true,
        Some(id) => {
            warn!(
                "Rejecting trajectory {}: JSON id mismatch, expected {}, found {}",
                path.display(),
                chat_id,
                id
            );
            false
        }
        None => {
            warn!(
                "Rejecting trajectory {}: missing or non-string JSON id for requested chat {}",
                path.display(),
                chat_id
            );
            false
        }
    }
}

pub fn trajectory_candidate_has_minimum_schema(
    t: &serde_json::Value,
    chat_id: &str,
    path: &Path,
) -> bool {
    let Some(root) = t.as_object() else {
        warn!(
            "Rejecting trajectory {}: JSON root must be an object for requested chat {}",
            path.display(),
            chat_id
        );
        return false;
    };
    if !trajectory_root_id_matches(t, chat_id, path) {
        return false;
    }
    if root
        .get("messages")
        .is_some_and(|messages| messages.is_array())
    {
        return true;
    }
    warn!(
        "Rejecting trajectory {}: missing or non-array JSON messages for requested chat {}",
        path.display(),
        chat_id
    );
    false
}

pub use refact_chat_history::trajectory_paths::trajectory_path_stem_matches_id;

pub async fn load_trajectory_candidate(
    ctx: &ChatCtx,
    chat_id: &str,
    candidate: ValidTrajectoryCandidate,
) -> Option<LoadedTrajectory> {
    let traj_path = candidate.path;
    let mut t = candidate.json;

    let messages_value = t
        .as_object_mut()
        .and_then(|object| object.remove("messages"))
        .unwrap_or_default();
    let persisted_goal_value = t.as_object_mut().and_then(|object| object.remove("goal"));
    let goal_ledger_value = t
        .as_object_mut()
        .and_then(|object| object.remove("goal_ledger"));
    let pending_deliveries = t
        .as_object_mut()
        .and_then(|object| object.remove("pending_deliveries"))
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let (messages, goal_ledger, goal): (
        Vec<ChatMessage>,
        Vec<GoalLedgerEntry>,
        Option<GoalSnapshot>,
    ) = tokio::task::spawn_blocking(move || {
        // Nested reconstruction payloads stay opaque in `extra` during load.
        // Invalid boundaries remain browsable and are rejected only by the send
        // projection. Never replace a failed transcript parse with empty history.
        let mut messages: Vec<ChatMessage> = match parse_stored_messages(messages_value) {
            Ok(messages) => messages,
            Err(error) => {
                tracing::warn!(%error, "Invalid stored trajectory messages; refusing empty-history fallback");
                return None;
            }
        };
        fix_tool_call_indexes(&mut messages);

        for (message_index, msg) in messages.iter_mut().enumerate() {
            if msg.message_id.is_empty() {
                let role = msg.role.clone();
                let content = msg.content.content_text_only();
                let source = msg
                    .extra
                    .get("event")
                    .and_then(|event| event.get("source"))
                    .and_then(|source| source.as_str())
                    .unwrap_or_default();
                msg.message_id = format!(
                    "legacy:{}:{:x}",
                    role,
                    md5::compute(
                        format!("{message_index}\n{role}\n{source}\n{content}").as_bytes()
                    )
                );
            }

            if let Some(tool_calls) = &msg.tool_calls {
                let filtered: Vec<_> = tool_calls
                    .iter()
                    .filter(|tc| !tc.function.name.is_empty())
                    .cloned()
                    .collect();

                if filtered.len() != tool_calls.len() {
                    tracing::warn!(
                        "Filtered out {} tool call(s) with empty names from message {}",
                        tool_calls.len() - filtered.len(),
                        msg.message_id
                    );
                }

                msg.tool_calls = if filtered.is_empty() {
                    None
                } else {
                    Some(filtered)
                };
            }
        }
        let persisted_goal: Option<GoalSnapshot> = persisted_goal_value
            .and_then(|value| serde_json::from_value(value).ok())
            .map(clamp_goal_snapshot_for_load);
        let goal_ledger: Vec<GoalLedgerEntry> = goal_ledger_value
            .and_then(|value| value.as_array().cloned())
            .map(|entries| {
                entries
                    .into_iter()
                    .filter_map(|entry| match serde_json::from_value(entry) {
                        Ok(parsed) => Some(parsed),
                        Err(error) => {
                            tracing::warn!("skipping unrecognized goal_ledger entry: {error}");
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ledger_prior = refact_chat_api::reduce_goal_ledger(&goal_ledger).map(|state| {
            let mut seed = persisted_goal.clone().unwrap_or_else(|| GoalSnapshot {
                version: state.version,
                budget: state.budget.clone(),
                ..Default::default()
            });
            state.apply_to_snapshot(&mut seed);
            seed
        });
        let goal_prior = persisted_goal.or(ledger_prior);
        let goal = refact_chat_api::session::goal_snapshot_from_messages(&messages, goal_prior.as_ref())
            .or(goal_prior)
            .map(clamp_goal_snapshot_for_load);
        Some((messages, goal_ledger, goal))
    })
    .await
    .map_err(|e| {
        warn!(
            "Failed to deserialize trajectory messages for chat {}: {}",
            chat_id, e
        );
    })
    .ok()??;

    let task_meta: Option<refact_chat_api::types::TaskMeta> = t
        .get("task_meta")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let worktree = if let Some(candidate) = t.get("worktree").and_then(parse_worktree_meta) {
        validate_loaded_worktree_strict(ctx, chat_id, candidate).await
    } else if let Some(candidate) =
        synthesize_legacy_task_agent_worktree(ctx, chat_id, task_meta.as_ref()).await
    {
        validate_loaded_legacy_task_agent_worktree(ctx, chat_id, candidate).await
    } else {
        None
    };

    let wake_up_at = t
        .get("wake_up_at")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let waiting_for_card_ids: Vec<String> = t
        .get("waiting_for_card_ids")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let goal_verification_blocked_until_ms = t
        .get("goal_verification_blocked_until_ms")
        .and_then(|v| v.as_u64());

    let parent_id = t
        .get("parent_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let link_type = t
        .get("link_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let root_chat_id = t
        .get("root_chat_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let previous_response_id = t
        .get("previous_response_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let frozen_request_prefix_value = t.get("frozen_request_prefix").filter(|v| !v.is_null());
    let loaded_frozen_request_prefix =
        frozen_request_prefix_value.and_then(|v| serde_json::from_value(v.clone()).ok());
    let claude_code_identity_value = t.get("claude_code_identity").filter(|v| !v.is_null());
    let provider_state_present =
        previous_response_id.is_some() || claude_code_identity_value.is_some();
    let (frozen_request_prefix, transition_prefix_repaired) = repair_transition_frozen_prefix(
        link_type.as_deref(),
        &messages,
        loaded_frozen_request_prefix,
        raw_frozen_prefix_has_tools(frozen_request_prefix_value),
        t.get("created_at").and_then(|v| v.as_str()),
        provider_state_present,
    );
    let claude_code_identity = if transition_prefix_repaired {
        None
    } else {
        claude_code_identity_value.and_then(|v| serde_json::from_value(v.clone()).ok())
    };
    let previous_response_id = if transition_prefix_repaired {
        None
    } else {
        previous_response_id
    };

    let thread = ThreadParams {
        id: chat_id.to_string(),
        title: t
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("New Chat")
            .to_string(),
        model: t
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        mode: refact_chat_api::setparams::map_legacy_mode_to_id(
            t.get("mode").and_then(|v| v.as_str()).unwrap_or("agent"),
        )
        .to_string(),
        tool_use: t
            .get("tool_use")
            .and_then(|v| v.as_str())
            .unwrap_or("agent")
            .to_string(),
        boost_reasoning: t.get("boost_reasoning").and_then(|v| v.as_bool()),
        context_tokens_cap: t
            .get("context_tokens_cap")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize),
        auto_compression_cap_pending: t
            .get("auto_compression_cap_pending")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        auto_compression_cap: t
            .get("auto_compression_cap")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize),
        include_project_info: t
            .get("include_project_info")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        checkpoints_enabled: t
            .get("checkpoints_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        is_title_generated: t
            .get("isTitleGenerated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        auto_approve_editing_tools: t
            .get("auto_approve_editing_tools")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        auto_approve_dangerous_commands: t
            .get("auto_approve_dangerous_commands")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        autonomous_no_confirm: t
            .get("autonomous_no_confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        task_meta,
        worktree,
        parent_id,
        link_type,
        root_chat_id,
        reasoning_effort: t
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        thinking_budget: t
            .get("thinking_budget")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize),
        temperature: t
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|n| n as f32),
        frequency_penalty: t
            .get("frequency_penalty")
            .and_then(|v| v.as_f64())
            .map(|n| n as f32),
        max_tokens: t
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize),
        parallel_tool_calls: t.get("parallel_tool_calls").and_then(|v| v.as_bool()),

        previous_response_id,

        browser_meta: t
            .get("browser_meta")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),

        active_skill: t
            .get("active_skill")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),

        auto_enrichment_enabled: t.get("auto_enrichment_enabled").and_then(|v| v.as_bool()),

        buddy_meta: t
            .get("buddy_meta")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),

        auto_compact_enabled: t.get("auto_compact_enabled").and_then(|v| v.as_bool()),
        agent_nudge_enabled: t.get("agent_nudge_enabled").and_then(|v| v.as_bool()),
        strip_reasoning_from_prompt: t
            .get("strip_reasoning_from_prompt")
            .and_then(|v| v.as_bool()),
        frozen_request_prefix,
        claude_code_identity,
        // Runtime-only: saves strip this key and loads ignore injected values
        // (pinned by `reactive_compact_attempts_is_runtime_only_not_persisted`),
        // so a stale in-flight retry counter can never survive a reload.
        reactive_compact_attempts: None,
    };

    let auto_approve_editing_tools_present = t
        .get("auto_approve_editing_tools")
        .and_then(|v| v.as_bool())
        .is_some();
    let auto_approve_dangerous_commands_present = t
        .get("auto_approve_dangerous_commands")
        .and_then(|v| v.as_bool())
        .is_some();

    let created_at = repaired_created_at(
        t.get("created_at").and_then(|v| v.as_str()),
        transition_prefix_repaired,
    );

    let updated_at = t
        .get("updated_at")
        .and_then(|v| v.as_str())
        .unwrap_or(&created_at)
        .to_string();

    let pending_deliveries =
        if let Some(agent_id) = ctx.agents.find_agent_id_by_child_chat_id(chat_id).await {
            if ctx.agents.has_runtime(&agent_id).await {
                Vec::new()
            } else {
                pending_deliveries
            }
        } else {
            pending_deliveries
        };

    Some(LoadedTrajectory {
        pending_deliveries,
        goal_ledger,
        goal_verification_blocked_until_ms,
        source_path: traj_path,
        messages,
        thread,
        goal,
        created_at,
        updated_at,
        wake_up_at,
        waiting_for_card_ids,
        auto_approve_editing_tools_present,
        auto_approve_dangerous_commands_present,
        transition_identity_repaired: transition_prefix_repaired,
    })
}

pub async fn load_generic_trajectory_for_chat(
    ctx: &ChatCtx,
    chat_id: &str,
) -> Option<LoadedTrajectory> {
    let candidate = find_trajectory_file(ctx, chat_id).await?;
    load_trajectory_candidate(ctx, chat_id, candidate).await
}

pub fn trajectory_source_matches_hint(
    actual: &TrajectorySourceIdentity,
    hint: &TrajectorySourceIdentity,
) -> bool {
    match (actual, hint) {
        (
            TrajectorySourceIdentity::Task {
                task_id,
                role,
                agent_id,
                card_id,
                planner_chat_id,
            },
            TrajectorySourceIdentity::Task {
                task_id: hint_task_id,
                role: hint_role,
                agent_id: hint_agent_id,
                card_id: hint_card_id,
                planner_chat_id: hint_planner_chat_id,
            },
        ) => {
            task_id == hint_task_id
                && role == hint_role
                && hint_agent_id
                    .as_ref()
                    .is_none_or(|hint_agent_id| Some(hint_agent_id) == agent_id.as_ref())
                && hint_card_id
                    .as_ref()
                    .is_none_or(|hint_card_id| Some(hint_card_id) == card_id.as_ref())
                && hint_planner_chat_id
                    .as_ref()
                    .is_none_or(|hint_planner_chat_id| {
                        Some(hint_planner_chat_id) == planner_chat_id.as_ref()
                    })
        }
        (left, right) => left == right,
    }
}

pub fn effective_trajectory_source_for_path(
    source: TrajectorySourceIdentity,
    path: &Path,
    task_roots: &[PathBuf],
) -> TrajectorySourceIdentity {
    let path_source = trajectory_source_identity_from_path(path, task_roots);
    if matches!(&source, TrajectorySourceIdentity::Normal)
        && !matches!(&path_source, TrajectorySourceIdentity::Normal)
    {
        path_source
    } else {
        source
    }
}

pub async fn load_generic_trajectory_for_chat_matching_source(
    ctx: &ChatCtx,
    chat_id: &str,
    source: &TrajectorySourceIdentity,
) -> Option<LoadedTrajectory> {
    validate_trajectory_id(chat_id).ok()?;
    let task_roots = get_all_task_roots(ctx).await;
    let paths = trajectory_candidate_paths(ctx, chat_id).await;
    for path in paths {
        let Some(candidate) = read_valid_trajectory_candidate(path, chat_id).await else {
            continue;
        };
        let candidate_source = match TrajectorySourceIdentity::from_json(&candidate.json) {
            Ok(candidate_source) => {
                effective_trajectory_source_for_path(candidate_source, &candidate.path, &task_roots)
            }
            Err(e) => {
                warn!(
                    "Skipping trajectory candidate {} for chat {}: invalid source: {}",
                    candidate.path.display(),
                    chat_id,
                    e
                );
                continue;
            }
        };
        if !trajectory_source_matches_hint(&candidate_source, source) {
            continue;
        }
        return load_trajectory_candidate(ctx, chat_id, candidate).await;
    }
    None
}

pub use crate::trajectory_runtime::save_initial_task_chat_trajectory;

pub async fn save_trajectory_as(
    ctx: &ChatCtx,
    thread: &ThreadParams,
    messages: &[ChatMessage],
) {
    crate::trajectory_runtime::save_trajectory_as_with_intent(
        ctx,
        thread,
        messages,
        TrajectoryCommitIntent::Required,
    )
    .await;
}
