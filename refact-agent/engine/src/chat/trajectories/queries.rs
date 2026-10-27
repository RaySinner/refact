use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use axum::extract::Path as AxumPath;
use axum::extract::State;
use axum::http::{Response, StatusCode};
use hyper::Body;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::fs;
use tokio::sync::{Mutex as AMutex, broadcast};
use tracing::{info, warn};
use crate::chat::types::TrajectorySourceIdentitySessionExt;

use refact_chat_history::trajectory_event::TrajectoryEvent;
pub use refact_chat_history::trajectory_metrics::*;
use crate::app_state::AppState;
use crate::call_validation::{ChatContent, ChatMessage};
use crate::chat::ctx::ChatCtx;
use crate::chat::trajectory_index;
use crate::chat::types::{ChatSession, TrajectoryCommitIntent, TrajectorySourceIdentity};
use crate::custom_error::ScratchError;
use crate::worktrees::types::WorktreeMeta;

use super::load::{
    effective_trajectory_source_for_path, sanitize_worktree_extra,
    title_generation_backing_file_matches, trajectory_path_stem_matches_id,
    trajectory_worktree_from_extra, validate_loaded_worktree_strict,
};
use super::model::{
    get_session_runtime_for_trajectory_source, LoadedTrajectory, TrajectoryData,
    TrajectoryListCandidate, TrajectoryMeta,
};
use super::paths::{
    ensure_existing_trajectory_file_matches, is_internal_trace_link_type,
    is_trajectory_id_mismatch_error, validate_trajectory_id,
};
use super::reload::{loaded_trajectory_source, persist_loaded_trajectory_repair_raw};
use super::watcher::task_trajectory_context_from_path;
use super::writer::atomic_write_json;

pub const TITLE_GENERATION_SUBAGENT_ID: &str = "title_generation";

#[cfg(test)]
pub const TITLE_GENERATION_LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(not(test))]
pub const TITLE_GENERATION_LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) fn is_placeholder_title(title: &str) -> bool {
    let normalized = title.trim().to_lowercase();
    normalized.is_empty() || normalized == "new chat" || normalized == "untitled"
}

pub(super) fn is_placeholder_task_name(name: &str) -> bool {
    let normalized = name.trim().to_lowercase();
    normalized.is_empty() || normalized == "new task" || normalized == "untitled"
}

pub(super) fn count_user_messages(messages: &[serde_json::Value]) -> usize {
    messages
        .iter()
        .filter(|msg| {
            msg.get("role")
                .and_then(|r| r.as_str())
                .map(|r| r == "user")
                .unwrap_or(false)
        })
        .count()
}

pub(super) fn json_message_is_ui_only(msg: &serde_json::Value) -> bool {
    msg.get("_ui_only").and_then(|v| v.as_bool()) == Some(true)
        || msg
            .get("extra")
            .and_then(|extra| extra.get("_ui_only"))
            .and_then(|v| v.as_bool())
            == Some(true)
}

pub(super) fn extract_first_user_message(messages: &[serde_json::Value]) -> Option<String> {
    for msg in messages {
        if json_message_is_ui_only(msg) {
            continue;
        }
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        if let Some(content) = msg
            .get("content")
            .and_then(extract_text_with_image_placeholders_from_json)
        {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.chars().take(200).collect());
            }
        }
    }
    None
}

pub fn extract_text_with_image_placeholders_from_json(
    content_value: &serde_json::Value,
) -> Option<String> {
    if let Some(content) = content_value.as_str() {
        return Some(content.to_string());
    }
    if let Some(content_arr) = content_value.as_array() {
        let parts: Vec<String> = content_arr
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("image_url") {
                    return Some("[image]".to_string());
                }
                if let Some(m_type) = item.get("m_type").and_then(|t| t.as_str()) {
                    if m_type.starts_with("image/") {
                        return Some("[image]".to_string());
                    }
                }
                item.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| item.get("m_content").and_then(|t| t.as_str()))
                    .map(|s| s.to_string())
            })
            .collect();
        if !parts.is_empty() {
            return Some(parts.join("\n\n"));
        }
    }
    None
}

pub(super) fn build_title_generation_context(messages: &[serde_json::Value]) -> String {
    let mut context = String::new();
    let max_messages = 6;
    let max_chars_per_message = 500;
    let mut included_count = 0;

    for msg in messages.iter() {
        if included_count >= max_messages {
            break;
        }
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown");
        if json_message_is_ui_only(msg) {
            continue;
        }
        if role == "error" {
            continue;
        }
        if role == refact_chat_history::trajectory_ops::COMPRESSION_REPORT_ROLE
            || matches!(
                role,
                "system" | "tool" | "context_file" | "cd_instruction" | "plan" | "goal" | "event"
            )
        {
            continue;
        }
        let content_text = match msg
            .get("content")
            .and_then(extract_text_with_image_placeholders_from_json)
        {
            Some(text) => text,
            None => continue,
        };
        let truncated: String = content_text.chars().take(max_chars_per_message).collect();
        if !truncated.trim().is_empty() {
            context.push_str(&format!("{}: {}\n\n", role, truncated));
            included_count += 1;
        }
    }
    context
}

pub(super) fn clean_generated_title(raw_title: &str) -> String {
    let cleaned = raw_title
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim_matches('*')
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_text_chars(&cleaned, 60)
}

pub(crate) fn task_context_from_task_meta(
    task_meta: Option<&crate::chat::types::TaskMeta>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    task_meta
        .map(|meta| {
            (
                Some(meta.task_id.clone()),
                Some(meta.role.clone()),
                meta.agent_id.clone(),
                meta.card_id.clone(),
            )
        })
        .unwrap_or((None, None, None, None))
}

pub(super) async fn generate_title_llm(
    ctx: &ChatCtx,
    messages: &[serde_json::Value],
    parent_chat_id: Option<&str>,
) -> Option<String> {
    let context = build_title_generation_context(messages);
    if context.trim().is_empty() {
        return None;
    }

    let subagent_config = match ctx
        .mode_config
        .get_subagent_config(TITLE_GENERATION_SUBAGENT_ID, None)
        .await
    {
        Some(config) => config,
        None => {
            warn!(
                "subagent config '{}' not found",
                TITLE_GENERATION_SUBAGENT_ID
            );
            return None;
        }
    };

    let title_prompt = match subagent_config.messages.user_template.as_ref() {
        Some(prompt) => prompt,
        None => {
            warn!(
                "messages.user_template not defined for subagent '{}'",
                TITLE_GENERATION_SUBAGENT_ID
            );
            return None;
        }
    };

    let prompt = format!("Chat conversation:\n{}\n\n{}", context, title_prompt);
    let chat_messages = vec![ChatMessage::new("user".to_string(), prompt)];

    match ctx
        .subchat
        .run_subchat_once(
            TITLE_GENERATION_SUBAGENT_ID,
            chat_messages,
            crate::subchat::TraceParent::from_parts(parent_chat_id, None),
        )
        .await
    {
        Ok(result) => {
            if let Some(last_msg) = result.messages.last() {
                let raw_title = last_msg.content.content_text_only();
                let cleaned = clean_generated_title(&raw_title);
                if !cleaned.is_empty() && cleaned.to_lowercase() != "new chat" {
                    info!("Generated title: {}", cleaned);
                    return Some(cleaned);
                }
            }
            None
        }
        Err(e) => {
            warn!("Title generation failed: {}", e);
            None
        }
    }
}

pub(super) fn spawn_title_generation_task(
    ctx: &ChatCtx,
    id: String,
    messages: Vec<serde_json::Value>,
    file_path: PathBuf,
    source: TrajectorySourceIdentity,
) {
    let ctx: ChatCtx = ctx.clone();
    tokio::spawn(async move {
        let generated_title = match tokio::time::timeout(
            TITLE_GENERATION_LLM_TIMEOUT,
            generate_title_llm(&ctx, &messages, Some(id.as_str())),
        )
        .await
        {
            Ok(title) => title,
            Err(_) => {
                warn!("Title generation timed out for {}", id);
                None
            }
        };
        let title = match generated_title {
            Some(t) => t,
            None => match extract_first_user_message(&messages) {
                Some(first_msg) => {
                    let truncated: String = first_msg.chars().take(60).collect();
                    if truncated.len() < first_msg.len() {
                        format!("{}...", truncated.trim_end())
                    } else {
                        truncated
                    }
                }
                None => return,
            },
        };
        let sessions = ctx.chat.sessions.clone();
        if !title_generation_backing_file_matches(&file_path, &id, &source).await {
            return;
        }
        let maybe_session_arc = {
            let sessions_read = sessions.read().await;
            sessions_read.get(&id).cloned()
        };
        if let Some(session_arc) = maybe_session_arc {
            let mut session = session_arc.lock().await;
            if TrajectorySourceIdentity::from_session(&session) != source {
                drop(session);
            } else {
                if session.thread.is_title_generated {
                    info!("Title already generated for {}, skipping", id);
                    return;
                }
                let suppressed_trajectory_events_tx =
                    if matches!(source, TrajectorySourceIdentity::Buddy) {
                        session.trajectory_events_tx.take()
                    } else {
                        None
                    };
                session.set_title(title.clone(), true);
                if let Some(tx) = suppressed_trajectory_events_tx {
                    session.trajectory_events_tx = Some(tx);
                }
                drop(session);
                ctx.trajectories
                    .maybe_save_trajectory_with_intent(
                        session_arc,
                        TrajectoryCommitIntent::Checkpoint,
                    )
                    .await;
                info!("Updated session {} with generated title: {}", id, title);
                return;
            }
        }
        let content = match fs::read_to_string(&file_path).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read trajectory for title update: {}", e);
                return;
            }
        };
        let mut data: TrajectoryData = match serde_json::from_str(&content) {
            Ok(d) => d,
            Err(e) => {
                warn!("Failed to parse trajectory for title update: {}", e);
                return;
            }
        };
        if data.id != id {
            warn!(
                "Skipping title update for {}: JSON id mismatch, found {}",
                file_path.display(),
                data.id
            );
            return;
        }
        let actual_source = match TrajectorySourceIdentity::from_extra(&data.extra) {
            Ok(actual_source) => actual_source,
            Err(e) => {
                warn!(
                    "Skipping title update for {}: trajectory source is invalid before write: {}",
                    file_path.display(),
                    e
                );
                return;
            }
        };
        if actual_source != source {
            warn!(
                "Skipping title update for {}: trajectory source changed before write",
                file_path.display()
            );
            return;
        }
        let already_generated = data
            .extra
            .get("isTitleGenerated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if already_generated {
            info!("Title already generated for {}, skipping", id);
            return;
        }
        let updated_at = chrono::Utc::now().to_rfc3339();
        data.title = title.clone();
        data.updated_at = updated_at.clone();
        data.extra
            .insert("isTitleGenerated".to_string(), serde_json::json!(true));
        if let Err(e) = atomic_write_json(&file_path, &data, source.clone()).await {
            warn!("Failed to write trajectory with generated title: {}", e);
            return;
        }
        if let Some(dir) = ctx.trajectories.index_dir_for_trajectory_file(&file_path).await {
            let value = match serde_json::to_value(&data) {
                Ok(value) => value,
                Err(e) => {
                    warn!(
                        "Failed to serialize trajectory title update for index: {}",
                        e
                    );
                    return;
                }
            };
            if let Err(e) =
                trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
                    &ctx.chat.trajectory_index_coordinator,
                    &dir,
                    &file_path,
                    value,
                    Some(source.clone()),
                )
                .await
            {
                warn!(
                    "Failed to update trajectory index for generated title: {}",
                    e
                );
            }
        }
        info!("Updated trajectory {} with generated title: {}", id, title);
        if !source.emits_generic_event() {
            return;
        }
        let content = match fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                warn!("Failed to read updated trajectory for title SSE: {}", e);
                return;
            }
        };
        let updated_data: TrajectoryData = match serde_json::from_str(&content) {
            Ok(data) => data,
            Err(e) => {
                warn!("Failed to parse updated trajectory for title SSE: {}", e);
                return;
            }
        };
        let task_roots = ctx.trajectories.get_all_task_roots().await;
        let mut meta = trajectory_data_to_meta_validated(&ctx, &updated_data).await;
        apply_task_trajectory_context(&file_path, &task_roots, &mut meta);
        if meta.root_chat_id.is_none() {
            meta.root_chat_id = Some(id.clone());
        }
        let (session_state, session_error, session_worktree) =
            get_session_runtime_for_trajectory_source(&sessions, &id, &meta.source).await;
        let event = updated_trajectory_event_from_meta_with_worktree(
            meta,
            Some(true),
            session_state,
            session_error,
            session_worktree,
        );
        let tx = &ctx.chat.trajectory_events_tx;
        {
            let _ = tx.send(event);
        }
    });
}

pub(super) fn spawn_task_name_generation_task(
    ctx: &ChatCtx,
    task_id: String,
    messages: Vec<serde_json::Value>,
) {
    let ctx: ChatCtx = ctx.clone();
    tokio::spawn(async move {
        let task_meta =
            match crate::tasks::storage::load_task_meta_for_ctx(&ctx, &task_id).await {
                Ok(meta) => meta,
                Err(e) => {
                    warn!("Failed to load task meta for name generation: {}", e);
                    return;
                }
            };

        if task_meta.is_name_generated {
            return;
        }

        if !is_placeholder_task_name(&task_meta.name) {
            return;
        }

        let generated_name = generate_title_llm(&ctx, &messages, None).await;
        let name = match generated_name {
            Some(n) => n,
            None => match extract_first_user_message(&messages) {
                Some(first_msg) => {
                    let truncated: String = first_msg.chars().take(60).collect();
                    if truncated.len() < first_msg.len() {
                        format!("{}...", truncated.trim_end())
                    } else {
                        truncated
                    }
                }
                None => return,
            },
        };

        match crate::tasks::storage::update_task_name_for_ctx(&ctx, &task_id, &name).await {
            Ok(_) => {
                info!("Updated task {} with generated name: {}", task_id, name);
            }
            Err(e) => {
                warn!("Failed to update task name: {}", e);
            }
        }
    });
}

pub(super) fn calculate_line_changes_from_chat_messages(messages: &[ChatMessage]) -> (i64, i64) {
    let mut total_added: i64 = 0;
    let mut total_removed: i64 = 0;

    for msg in messages {
        if msg.role != "diff" {
            continue;
        }

        let content = match &msg.content {
            ChatContent::SimpleText(s) => s.as_str(),
            _ => continue,
        };

        if let Ok(chunks) = serde_json::from_str::<Vec<serde_json::Value>>(content) {
            for chunk in chunks {
                if let Some(lines_add) = chunk.get("lines_add").and_then(|v| v.as_str()) {
                    if !lines_add.is_empty() {
                        total_added += lines_add.lines().count() as i64;
                    }
                }
                if let Some(lines_remove) = chunk.get("lines_remove").and_then(|v| v.as_str()) {
                    if !lines_remove.is_empty() {
                        total_removed += lines_remove.lines().count() as i64;
                    }
                }
            }
        }
    }

    (total_added, total_removed)
}

pub(super) fn calculate_task_progress_from_chat_messages(
    messages: &[ChatMessage],
) -> (i32, i32, i32) {
    // Build a set of successful tool call IDs (tool messages without tool_failed=true)
    let mut successful_tool_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for msg in messages {
        if msg.role != "tool" {
            continue;
        }
        let tool_failed = msg.tool_failed.unwrap_or(false);
        if !tool_failed && !msg.tool_call_id.is_empty() {
            successful_tool_ids.insert(msg.tool_call_id.clone());
        }
    }

    // Find the last successful tasks_set tool call (iterate in reverse)
    for msg in messages.iter().rev() {
        if msg.role != "assistant" {
            continue;
        }

        let tool_calls = match &msg.tool_calls {
            Some(tc) => tc,
            None => continue,
        };

        // Iterate tool_calls in reverse to find the last tasks_set
        for tc in tool_calls.iter().rev() {
            if tc.function.name != "tasks_set" {
                continue;
            }

            if tc.id.is_empty() || !successful_tool_ids.contains(&tc.id) {
                continue;
            }

            // Parse the arguments
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                if let Some(tasks) = args.get("tasks").and_then(|t| t.as_array()) {
                    let mut total = 0i32;
                    let mut done = 0i32;
                    let mut failed = 0i32;

                    for task in tasks {
                        total += 1;
                        let status = task.get("status").and_then(|s| s.as_str()).unwrap_or("");
                        match status.to_lowercase().as_str() {
                            "completed" | "done" | "complete" => done += 1,
                            "failed" | "error" => failed += 1,
                            _ => {}
                        }
                    }

                    return (total, done, failed);
                }
            }
        }
    }

    (0, 0, 0)
}

pub(super) fn calculate_token_totals_from_chat_messages(messages: &[ChatMessage]) -> TokenTotals {
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    let mut total_tokens: u64 = 0;
    let mut cache_read_tokens: u64 = 0;
    let mut cache_creation_tokens: u64 = 0;
    let mut cost_usd: Option<f64> = None;

    for msg in messages {
        let usage = match &msg.usage {
            Some(u) => u,
            None => continue,
        };
        prompt_tokens += usage.prompt_tokens as u64;
        completion_tokens += usage.completion_tokens as u64;
        total_tokens += usage.total_tokens as u64;
        if let Some(v) = usage.cache_read_tokens {
            cache_read_tokens += v as u64;
        }
        if let Some(v) = usage.cache_creation_tokens {
            cache_creation_tokens += v as u64;
        }
        if let Some(ref m) = usage.metering_usd {
            *cost_usd.get_or_insert(0.0) += m.total_usd;
        }
    }

    TokenTotals {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        cost_usd,
    }
}

pub(super) fn trajectory_data_to_meta(data: &TrajectoryData) -> TrajectoryMeta {
    let task_meta_json = data.extra.get("task_meta");
    let task_id = task_meta_json
        .and_then(|v| v.get("task_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let task_role = task_meta_json
        .and_then(|v| v.get("role"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let agent_id = task_meta_json
        .and_then(|v| v.get("agent_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let card_id = task_meta_json
        .and_then(|v| v.get("card_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let parent_id = data
        .extra
        .get("parent_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let link_type = data
        .extra
        .get("link_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let root_chat_id = data
        .extra
        .get("root_chat_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let worktree = None;

    let (total_lines_added, total_lines_removed) =
        calculate_line_changes_from_messages(&data.messages);
    let (tasks_total, tasks_done, tasks_failed) =
        calculate_task_progress_from_messages(&data.messages);
    let token_totals = calculate_token_totals_from_messages(&data.messages);

    TrajectoryMeta {
        id: data.id.clone(),
        title: trajectory_meta_title(&data.title),
        created_at: data.created_at.clone(),
        updated_at: data.updated_at.clone(),
        model: data.model.clone(),
        mode: data.mode.clone(),
        message_count: data.messages.len(),
        parent_id,
        link_type,
        task_id,
        task_role,
        agent_id,
        card_id,
        session_state: None,
        root_chat_id,
        worktree,
        total_lines_added,
        total_lines_removed,
        tasks_total,
        tasks_done,
        tasks_failed,
        total_prompt_tokens: token_totals.prompt_tokens,
        total_completion_tokens: token_totals.completion_tokens,
        total_tokens: token_totals.total_tokens,
        total_cache_read_tokens: token_totals.cache_read_tokens,
        total_cache_creation_tokens: token_totals.cache_creation_tokens,
        total_cost_usd: token_totals.cost_usd,
        source: TrajectorySourceIdentity::from_extra(&data.extra).unwrap_or_default(),
    }
}

#[derive(Debug, Deserialize)]
pub struct TrajectoriesListQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub displayable_only: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TrajectoriesAllQuery {
    #[serde(default)]
    pub displayable_only: bool,
}

#[derive(Debug, Serialize)]
pub struct PaginatedTrajectories {
    pub items: Vec<TrajectoryMeta>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub total_count: usize,
}

pub(super) fn encode_cursor(updated_at: &str, id: &str) -> String {
    use base64::Engine;
    let cursor_data = format!("{}|{}", updated_at, id);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(cursor_data.as_bytes())
}

pub(super) fn decode_cursor(cursor: &str) -> Option<(String, String)> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()?;
    let cursor_str = String::from_utf8(decoded).ok()?;
    let parts: Vec<&str> = cursor_str.splitn(2, '|').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

pub(super) async fn trajectory_data_to_meta_validated(
    ctx: &ChatCtx,
    data: &TrajectoryData,
) -> TrajectoryMeta {
    let mut meta = trajectory_data_to_meta(data);
    if let Some(worktree) = trajectory_worktree_from_extra(&data.extra) {
        meta.worktree = ctx
            .trajectories
            .validate_loaded_worktree_strict(&data.id, worktree)
            .await;
    }
    meta
}

pub(super) fn loaded_trajectory_to_meta(
    loaded: &LoadedTrajectory,
    task_roots: &[PathBuf],
) -> TrajectoryMeta {
    let effective_root = loaded
        .thread
        .root_chat_id
        .clone()
        .unwrap_or_else(|| loaded.thread.id.clone());
    let source = loaded_trajectory_source(loaded, task_roots);
    let (task_id, task_role, agent_id, card_id) =
        task_context_from_task_meta(loaded.thread.task_meta.as_ref());
    let (total_lines_added, total_lines_removed) =
        calculate_line_changes_from_chat_messages(&loaded.messages);
    let (tasks_total, tasks_done, tasks_failed) =
        calculate_task_progress_from_chat_messages(&loaded.messages);
    let token_totals = calculate_token_totals_from_chat_messages(&loaded.messages);

    let mut meta = TrajectoryMeta {
        id: loaded.thread.id.clone(),
        title: trajectory_meta_title(&loaded.thread.title),
        created_at: loaded.created_at.clone(),
        updated_at: loaded.updated_at.clone(),
        model: loaded.thread.model.clone(),
        mode: loaded.thread.mode.clone(),
        message_count: loaded.messages.len(),
        parent_id: loaded.thread.parent_id.clone(),
        link_type: loaded.thread.link_type.clone(),
        task_id,
        task_role,
        agent_id,
        card_id,
        session_state: None,
        root_chat_id: Some(effective_root),
        worktree: loaded.thread.worktree.clone(),
        total_lines_added,
        total_lines_removed,
        tasks_total,
        tasks_done,
        tasks_failed,
        total_prompt_tokens: token_totals.prompt_tokens,
        total_completion_tokens: token_totals.completion_tokens,
        total_tokens: token_totals.total_tokens,
        total_cache_read_tokens: token_totals.cache_read_tokens,
        total_cache_creation_tokens: token_totals.cache_creation_tokens,
        total_cost_usd: token_totals.cost_usd,
        source,
    };
    apply_task_trajectory_context(&loaded.source_path, task_roots, &mut meta);
    meta
}

pub(super) fn deleted_trajectory_event(id: String) -> TrajectoryEvent {
    TrajectoryEvent {
        event_type: "deleted".to_string(),
        id,
        updated_at: None,
        title: None,
        is_title_generated: None,
        session_state: None,
        error: None,
        message_count: None,
        parent_id: None,
        link_type: None,
        root_chat_id: None,
        task_id: None,
        task_role: None,
        agent_id: None,
        card_id: None,
        model: None,
        mode: None,
        worktree: None,
        total_lines_added: None,
        total_lines_removed: None,
        tasks_total: None,
        tasks_done: None,
        tasks_failed: None,
        total_prompt_tokens: None,
        total_completion_tokens: None,
        total_tokens: None,
        total_cache_read_tokens: None,
        total_cache_creation_tokens: None,
        total_cost_usd: None,
    }
}

pub(super) fn updated_trajectory_event_from_meta(
    meta: TrajectoryMeta,
    is_title_generated: Option<bool>,
    session_state: String,
    session_error: Option<String>,
) -> TrajectoryEvent {
    updated_trajectory_event_from_meta_with_worktree(
        meta,
        is_title_generated,
        session_state,
        session_error,
        None,
    )
}

pub(super) fn updated_trajectory_event_from_meta_with_worktree(
    meta: TrajectoryMeta,
    is_title_generated: Option<bool>,
    session_state: String,
    session_error: Option<String>,
    session_worktree: Option<WorktreeMeta>,
) -> TrajectoryEvent {
    TrajectoryEvent {
        event_type: "updated".to_string(),
        id: meta.id,
        updated_at: Some(meta.updated_at),
        title: Some(meta.title),
        is_title_generated,
        session_state: Some(session_state),
        error: session_error,
        message_count: Some(meta.message_count),
        parent_id: meta.parent_id,
        link_type: meta.link_type,
        root_chat_id: meta.root_chat_id,
        task_id: meta.task_id,
        task_role: meta.task_role,
        agent_id: meta.agent_id,
        card_id: meta.card_id,
        model: Some(meta.model),
        mode: Some(meta.mode),
        worktree: meta.worktree.or(session_worktree),
        total_lines_added: Some(meta.total_lines_added),
        total_lines_removed: Some(meta.total_lines_removed),
        tasks_total: Some(meta.tasks_total),
        tasks_done: Some(meta.tasks_done),
        tasks_failed: Some(meta.tasks_failed),
        total_prompt_tokens: Some(meta.total_prompt_tokens),
        total_completion_tokens: Some(meta.total_completion_tokens),
        total_tokens: Some(meta.total_tokens),
        total_cache_read_tokens: Some(meta.total_cache_read_tokens),
        total_cache_creation_tokens: Some(meta.total_cache_creation_tokens),
        total_cost_usd: meta.total_cost_usd,
    }
}

pub(super) fn apply_task_trajectory_context(path: &Path, task_roots: &[PathBuf], meta: &mut TrajectoryMeta) {
    if let Some((task_id, role, agent_id)) = task_trajectory_context_from_path(path, task_roots) {
        meta.source = effective_trajectory_source_for_path(meta.source.clone(), path, task_roots);
        if meta.task_id.is_none() {
            meta.task_id = Some(task_id);
        }
        if meta.task_role.is_none() {
            meta.task_role = Some(role);
        }
        if meta.agent_id.is_none() {
            meta.agent_id = agent_id;
        }
    }
}

pub(super) fn trajectory_list_candidate_matches_hydrated_data(
    candidate: &TrajectoryListCandidate,
    data: &TrajectoryData,
) -> bool {
    if data.id == candidate.id && trajectory_path_stem_matches_id(&candidate.path, &data.id) {
        return true;
    }
    warn!(
        "Ignoring trajectory {} during hydration: expected id {}, found {}",
        candidate.path.display(),
        candidate.id,
        data.id
    );
    false
}

pub(super) async fn hydrate_trajectory_list_candidate(
    app: AppState,
    candidate: &TrajectoryListCandidate,
    task_roots: &[PathBuf],
) -> Option<TrajectoryMeta> {
    if let (Some(indexed_len), Some(indexed_modified_ms)) = (
        candidate.indexed_file_len,
        candidate.indexed_file_modified_unix_ms,
    ) {
        let fresh = fs::symlink_metadata(&candidate.path)
            .await
            .ok()
            .and_then(|metadata| {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return None;
                }
                let modified = metadata.modified().ok()?;
                let ms = modified.duration_since(UNIX_EPOCH).ok()?.as_millis();
                let modified_ms = i64::try_from(ms).ok()?;
                Some(metadata.len() == indexed_len && modified_ms == indexed_modified_ms)
            })
            .unwrap_or(false);
        if !fresh {
            return None;
        }
    }
    if let Some(mut meta) = candidate.indexed_meta.clone() {
        if let Some(worktree) = meta.worktree.clone() {
            meta.worktree = validate_loaded_worktree_strict(app.clone(), &meta.id, worktree).await;
        }
        apply_task_trajectory_context(&candidate.path, task_roots, &mut meta);
        return Some(meta);
    }
    let content = fs::read_to_string(&candidate.path).await.ok()?;
    let data = serde_json::from_str::<TrajectoryData>(&content).ok()?;
    if !trajectory_list_candidate_matches_hydrated_data(candidate, &data) {
        return None;
    }
    let mut meta = trajectory_data_to_meta_validated(&app.chat_ctx(), &data).await;
    apply_task_trajectory_context(&candidate.path, task_roots, &mut meta);
    Some(meta)
}

pub(super) async fn hydrate_trajectory_list_page(
    app: AppState,
    candidates: Vec<TrajectoryListCandidate>,
    limit: usize,
    task_roots: &[PathBuf],
) -> (Vec<TrajectoryMeta>, bool) {
    let mut items = Vec::with_capacity(limit);
    let mut has_more = false;
    for candidate in candidates {
        let Some(meta) =
            hydrate_trajectory_list_candidate(app.clone(), &candidate, task_roots).await
        else {
            continue;
        };
        if items.len() == limit {
            has_more = true;
            break;
        }
        items.push(meta);
    }
    (items, has_more)
}

pub(super) async fn collect_trajectory_list_candidates(
    ctx: &ChatCtx,
    cursor_filter: Option<&(String, String)>,
    displayable_only: bool,
    caller: trajectory_index::TrajectoryIndexListingCaller,
) -> Vec<TrajectoryListCandidate> {
    let mut candidates = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for trajectories_dir in ctx.trajectories.list_trajectory_dirs().await {
        if !is_real_dir(&trajectories_dir).await {
            continue;
        }
        let entries = match trajectory_index::list_trajectory_entries_with_rollout_for(
            &ctx.chat.trajectory_index_coordinator,
            &trajectories_dir,
            None,
            caller,
        )
        .await
        {
            Ok(entries) => entries,
            Err(e) => {
                warn!(
                    "Failed to list trajectory index {:?}: {}",
                    trajectories_dir, e
                );
                continue;
            }
        };
        for entry in entries {
            if matches!(entry.source, trajectory_index::TrajectoryIndexSource::Buddy) {
                continue;
            }
            if displayable_only && !entry.displayable_chat {
                continue;
            }
            if !seen_ids.insert(entry.id.clone()) {
                continue;
            }
            if let Some((cursor_updated_at, cursor_id)) = cursor_filter {
                if !cursor_precedes_item(
                    (entry.updated_at.as_str(), entry.id.as_str()),
                    (cursor_updated_at.as_str(), cursor_id.as_str()),
                ) {
                    continue;
                }
            }
            candidates.push(trajectory_index::list_candidate_from_entry(
                &trajectories_dir,
                &entry,
            ));
        }
    }

    candidates
}

pub(super) async fn is_real_dir(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path).await, Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink())
}

pub(super) fn is_real_dir_sync(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink())
}

pub(super) async fn is_real_file(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path).await, Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink())
}

#[cfg(target_os = "macos")]
pub(super) async fn allowed_real_dir_symlink_target(path: &Path) -> Option<PathBuf> {
    let expected_target = match path.to_str()? {
        "/tmp" => Path::new("/private/tmp"),
        "/var" => Path::new("/private/var"),
        _ => return None,
    };
    let resolved = fs::canonicalize(path).await.ok()?;
    (resolved == expected_target).then_some(resolved)
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn allowed_real_dir_symlink_target(_path: &Path) -> Option<PathBuf> {
    None
}

pub(super) async fn ensure_real_dir_tree(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::ParentDir => {
                return Err(format!(
                    "Refusing to create directory with parent component: {}",
                    path.display()
                ));
            }
        }
        if current.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(&current).await {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    if let Some(resolved) = allowed_real_dir_symlink_target(&current).await {
                        current = resolved;
                        continue;
                    }
                    return Err(format!(
                        "Refusing to use non-real directory component: {}",
                        current.display()
                    ));
                }
                if !metadata.is_dir() {
                    return Err(format!(
                        "Refusing to use non-real directory component: {}",
                        current.display()
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&current).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => {
                        return Err(format!(
                            "Failed to create directory {}: {}",
                            current.display(),
                            e
                        ))
                    }
                }
                let metadata = fs::symlink_metadata(&current).await.map_err(|e| {
                    format!(
                        "Failed to inspect created directory {}: {}",
                        current.display(),
                        e
                    )
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "Created directory is not a real directory: {}",
                        current.display()
                    ));
                }
            }
            Err(e) => {
                return Err(format!(
                    "Failed to inspect directory component {}: {}",
                    current.display(),
                    e
                ));
            }
        }
    }
    if is_real_dir(path).await {
        Ok(())
    } else {
        Err(format!(
            "Directory is not a real directory: {}",
            path.display()
        ))
    }
}

pub(super) async fn canonical_child_path_under_root(
    root: &Path,
    child: &Path,
) -> Result<PathBuf, String> {
    let root_canonical = fs::canonicalize(root)
        .await
        .map_err(|e| format!("Failed to resolve root {}: {}", root.display(), e))?;
    if !is_real_dir(root).await {
        return Err(format!("Root is not a real directory: {}", root.display()));
    }

    if is_real_file(child).await {
        let child_canonical = fs::canonicalize(child)
            .await
            .map_err(|e| format!("Failed to resolve child {}: {}", child.display(), e))?;
        if child_canonical.starts_with(&root_canonical) {
            return Ok(child.to_path_buf());
        }
        return Err(format!(
            "Child {} is outside root {}",
            child.display(),
            root.display()
        ));
    }

    if let Ok(metadata) = fs::symlink_metadata(child).await {
        if metadata.file_type().is_symlink() {
            return Err(format!("Refusing symlink child file: {}", child.display()));
        }
        return Err(format!("Child is not a real file: {}", child.display()));
    }

    let parent = child
        .parent()
        .ok_or_else(|| format!("Child has no parent: {}", child.display()))?;
    let parent_canonical = fs::canonicalize(parent)
        .await
        .map_err(|e| format!("Failed to resolve child parent {}: {}", parent.display(), e))?;
    if parent_canonical.starts_with(&root_canonical) {
        Ok(child.to_path_buf())
    } else {
        Err(format!(
            "Child parent {} is outside root {}",
            parent.display(),
            root.display()
        ))
    }
}

pub(super) async fn safe_trajectory_file_in_dir(dir: &Path, chat_id: &str) -> Option<PathBuf> {
    validate_trajectory_id(chat_id).ok()?;
    ensure_real_dir_tree(dir).await.ok()?;
    let child = dir.join(format!("{}.json", chat_id));
    if !is_real_file(&child).await {
        return None;
    }
    canonical_child_path_under_root(dir, &child).await.ok()
}

pub(super) async fn safe_new_trajectory_file_in_dir(
    dir: &Path,
    chat_id: &str,
) -> Result<PathBuf, String> {
    validate_trajectory_id(chat_id).map_err(|e| e.message)?;
    ensure_real_dir_tree(dir).await?;
    canonical_child_path_under_root(dir, &dir.join(format!("{}.json", chat_id))).await
}

pub(super) fn cursor_precedes_item(item: (&str, &str), cursor: (&str, &str)) -> bool {
    item < cursor
}

pub async fn list_trajectories_page(
    app: AppState,
    limit: usize,
    cursor: Option<String>,
    displayable_only: bool,
) -> Result<PaginatedTrajectories, String> {
    list_trajectories_page_for(
        app,
        limit,
        cursor,
        displayable_only,
        trajectory_index::TrajectoryIndexListingCaller::ChatApi,
    )
    .await
}

pub async fn list_trajectories_page_for(
    app: AppState,
    limit: usize,
    cursor: Option<String>,
    displayable_only: bool,
    caller: trajectory_index::TrajectoryIndexListingCaller,
) -> Result<PaginatedTrajectories, String> {
    let ctx = app.chat_ctx();
    let limit = limit.clamp(1, 200);
    let cursor_filter = match cursor.as_deref() {
        Some(cursor) => {
            Some(decode_cursor(cursor).ok_or_else(|| "Invalid cursor format".to_string())?)
        }
        None => None,
    };

    let task_roots = ctx.trajectories.get_all_task_roots().await;
    let mut candidates =
        collect_trajectory_list_candidates(&ctx, cursor_filter.as_ref(), displayable_only, caller)
            .await;
    let total_count = if cursor_filter.is_none() {
        candidates.len()
    } else {
        collect_trajectory_list_candidates(&ctx, None, displayable_only, caller)
            .await
            .len()
    };
    candidates.sort_by(|a, b| match b.updated_at.cmp(&a.updated_at) {
        std::cmp::Ordering::Equal => b.id.cmp(&a.id),
        other => other,
    });

    let (mut items, has_more) =
        hydrate_trajectory_list_page(app.clone(), candidates, limit, &task_roots).await;
    enrich_with_session_state(app, &mut items).await;

    let next_cursor = if has_more {
        items
            .last()
            .map(|last| encode_cursor(&last.updated_at, &last.id))
    } else {
        None
    };

    Ok(PaginatedTrajectories {
        items,
        next_cursor,
        has_more,
        total_count,
    })
}

pub async fn handle_v1_trajectories_list(
    State(app): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<TrajectoriesListQuery>,
) -> Result<Response<Body>, ScratchError> {
    let response = list_trajectories_page(
        app,
        params.limit.unwrap_or(50),
        params.cursor,
        params.displayable_only.unwrap_or(false),
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;

    let json = serde_json::to_string(&response).map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Serialization error: {}", e),
        )
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(json))
        .unwrap())
}

pub async fn list_all_trajectories_meta(
    app: AppState,
    displayable_only: bool,
) -> Result<Vec<TrajectoryMeta>, String> {
    let ctx = app.chat_ctx();
    let mut result: Vec<TrajectoryMeta> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let task_roots = ctx.trajectories.get_all_task_roots().await;

    for trajectories_dir in ctx.trajectories.list_trajectory_dirs().await {
        if !is_real_dir(&trajectories_dir).await {
            continue;
        }
        let entries = match trajectory_index::list_trajectory_entries_with_rollout_for(
            &app.chat.trajectory_index_coordinator,
            &trajectories_dir,
            None,
            trajectory_index::TrajectoryIndexListingCaller::AllTrajectoriesMetadata,
        )
        .await
        {
            Ok(entries) => entries,
            Err(e) => {
                warn!(
                    "Failed to list trajectory index {:?}: {}",
                    trajectories_dir, e
                );
                continue;
            }
        };
        for entry in entries {
            if matches!(entry.source, trajectory_index::TrajectoryIndexSource::Buddy) {
                continue;
            }
            if displayable_only && !entry.displayable_chat {
                continue;
            }
            if seen_ids.insert(entry.id.clone()) {
                let mut meta = trajectory_index::meta_from_entry(&trajectories_dir, &entry);
                if let Some(worktree) = meta.worktree.clone() {
                    meta.worktree =
                        validate_loaded_worktree_strict(app.clone(), &meta.id, worktree).await;
                }
                apply_task_trajectory_context(
                    &trajectory_index::trajectory_file_path_for_entry(&trajectories_dir, &entry),
                    &task_roots,
                    &mut meta,
                );
                result.push(meta);
            }
        }
    }

    enrich_with_session_state(app, &mut result).await;
    result.sort_by(|a, b| match b.updated_at.cmp(&a.updated_at) {
        std::cmp::Ordering::Equal => b.id.cmp(&a.id),
        other => other,
    });

    Ok(result)
}

pub async fn handle_v1_trajectories_all(
    State(app): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<TrajectoriesAllQuery>,
) -> Result<Response<Body>, ScratchError> {
    let result = list_all_trajectories_meta(app, params.displayable_only)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_string(&result).unwrap()))
        .unwrap())
}

pub(super) async fn enrich_with_session_state(
    app: AppState,
    trajectories: &mut Vec<TrajectoryMeta>,
) {
    let session_arcs: Vec<(usize, Arc<AMutex<ChatSession>>)> = {
        let sessions = app.chat.sessions.read().await;
        trajectories
            .iter()
            .enumerate()
            .filter_map(|(idx, traj)| sessions.get(&traj.id).map(|arc| (idx, arc.clone())))
            .collect()
    };

    for (idx, session_arc) in session_arcs {
        let session = session_arc.lock().await;
        if !trajectories[idx].source.matches_session(&session) {
            continue;
        }
        trajectories[idx].session_state = Some(session.runtime.state.to_string());
        if trajectories[idx].worktree.is_none() {
            trajectories[idx].worktree = session.thread.worktree.clone();
        }
    }
}

pub async fn handle_v1_trajectories_get(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response<Body>, ScratchError> {
    validate_trajectory_id(&id)?;
    let content = app.chat_ctx()
        .trajectories
        .read_trajectory_or_buddy_content(&id)
        .await
        .ok_or_else(|| {
            ScratchError::new(StatusCode::NOT_FOUND, "Trajectory not found".to_string())
        })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(content))
        .unwrap())
}
pub async fn handle_v1_trajectory_path(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response<Body>, ScratchError> {
    let sessions = app.chat.sessions.clone();
    validate_trajectory_id(&id)?;
    let path = app.chat_ctx()
        .trajectories
        .find_trajectory_path_for_active_chat(&sessions, &id)
        .await
        .ok_or_else(|| {
            ScratchError::new(StatusCode::NOT_FOUND, "Trajectory not found".to_string())
        })?;
    let body = serde_json::json!({ "path": path.to_string_lossy() });
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap())
}

pub async fn handle_v1_trajectories_save(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    validate_trajectory_id(&id)?;
    let data: TrajectoryData = serde_json::from_slice(&body_bytes)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut data = data;
    data.updated_at = now.clone();
    if data.created_at.is_empty() {
        data.created_at = now.clone();
    }
    if data.id != id {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "ID mismatch".to_string(),
        ));
    }
    let emits_generic_event = data
        .extra
        .get("buddy_meta")
        .map_or(true, |value| value.is_null());
    let ctx = app.chat_ctx();
    let file_path = ctx
        .trajectories
        .resolve_trajectory_data_save_path(&id, &data)
        .await
        .map_err(|e| {
            let status = if is_trajectory_id_mismatch_error(&e) {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            ScratchError::new(status, e)
        })?;
    ensure_existing_trajectory_file_matches(&file_path, &id)
        .await
        .map_err(|e| ScratchError::new(StatusCode::CONFLICT, e))?;
    let is_new = !is_real_file(&file_path).await;
    let is_title_generated = data
        .extra
        .get("isTitleGenerated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let should_generate_title =
        is_placeholder_title(&data.title) && !is_title_generated && !data.messages.is_empty();
    let worktree = if let Some(candidate) = sanitize_worktree_extra(&mut data.extra) {
        match validate_loaded_worktree_strict(app.clone(), &id, candidate).await {
            Some(validated) => {
                data.extra.insert(
                    "worktree".to_string(),
                    serde_json::to_value(&validated).unwrap_or_default(),
                );
                Some(validated)
            }
            None => {
                data.extra.remove("worktree");
                None
            }
        }
    } else {
        None
    };
    atomic_write_json(
        &file_path,
        &data,
        TrajectorySourceIdentity::from_extra(&data.extra).map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Invalid trajectory source for watcher suppression: {}", e),
            )
        })?,
    )
    .await
    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if !is_internal_trace_link_type(data.extra.get("link_type").and_then(|value| value.as_str())) {
        let source_hint = Some(
            TrajectorySourceIdentity::from_extra(&data.extra).map_err(|e| {
                ScratchError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Invalid trajectory source for index update: {}", e),
                )
            })?,
        );
        if let Some(dir) = ctx.trajectories.index_dir_for_trajectory_file(&file_path).await {
            trajectory_index::upsert_trajectory_index_entry_from_owned_value_with_rollout(
                &app.chat.trajectory_index_coordinator,
                &dir,
                &file_path,
                serde_json::to_value(&data).map_err(|e| {
                    ScratchError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to serialize trajectory for index update: {}", e),
                    )
                })?,
                source_hint,
            )
            .await
            .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
    }
    let parent_id = data
        .extra
        .get("parent_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let link_type = data
        .extra
        .get("link_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let effective_root = data
        .extra
        .get("root_chat_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| id.clone());
    let sessions = app.chat.sessions.clone();
    let source = TrajectorySourceIdentity::from_extra(&data.extra).map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Invalid trajectory source for session enrichment: {}", e),
        )
    })?;
    let (session_state, session_error, session_worktree) =
        get_session_runtime_for_trajectory_source(&sessions, &id, &source).await;
    let (total_lines_added, total_lines_removed) =
        calculate_line_changes_from_messages(&data.messages);
    let (tasks_total, tasks_done, tasks_failed) =
        calculate_task_progress_from_messages(&data.messages);
    let token_totals = calculate_token_totals_from_messages(&data.messages);
    let (task_id, task_role, agent_id, card_id) = data
        .extra
        .get("task_meta")
        .and_then(|value| {
            serde_json::from_value::<crate::chat::types::TaskMeta>(value.clone()).ok()
        })
        .map(|meta| task_context_from_task_meta(Some(&meta)))
        .unwrap_or((None, None, None, None));
    if emits_generic_event {
        let event = TrajectoryEvent {
            event_type: if is_new {
                "created".to_string()
            } else {
                "updated".to_string()
            },
            id: id.clone(),
            updated_at: Some(data.updated_at.clone()),
            title: Some(trajectory_meta_title(&data.title)),
            is_title_generated: Some(is_title_generated),
            session_state: Some(session_state),
            error: session_error,
            message_count: Some(data.messages.len()),
            parent_id,
            link_type,
            root_chat_id: Some(effective_root),
            task_id,
            task_role,
            agent_id,
            card_id,
            model: Some(data.model.clone()),
            mode: Some(data.mode.clone()),
            worktree: worktree.or(session_worktree),
            total_lines_added: Some(total_lines_added),
            total_lines_removed: Some(total_lines_removed),
            tasks_total: Some(tasks_total),
            tasks_done: Some(tasks_done),
            tasks_failed: Some(tasks_failed),
            total_prompt_tokens: Some(token_totals.prompt_tokens),
            total_completion_tokens: Some(token_totals.completion_tokens),
            total_tokens: Some(token_totals.total_tokens),
            total_cache_read_tokens: Some(token_totals.cache_read_tokens),
            total_cache_creation_tokens: Some(token_totals.cache_creation_tokens),
            total_cost_usd: token_totals.cost_usd,
        };
        let _ = app.chat.trajectory_events_tx.send(event);
    }
    if should_generate_title {
        let source = TrajectorySourceIdentity::from_extra(&data.extra).map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Invalid trajectory source for title generation: {}", e),
            )
        })?;
        spawn_title_generation_task(
            &ctx,
            id.clone(),
            data.messages.clone(),
            file_path.clone(),
            source,
        );
    }
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"status":"ok"}"#))
        .unwrap())
}

pub async fn handle_v1_trajectories_delete(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_trajectory_id(&id)?;
    let file_path = ctx
        .trajectories
        .find_trajectory_path(&id)
        .await
        .ok_or_else(|| {
            ScratchError::new(StatusCode::NOT_FOUND, "Trajectory not found".to_string())
        })?;
    fs::remove_file(&file_path)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(dir) = ctx.trajectories.index_dir_for_trajectory_file(&file_path).await {
        if let Err(e) = trajectory_index::remove_trajectory_index_entry_with_rollout(
            &app.chat.trajectory_index_coordinator,
            &dir,
            &id,
        )
        .await
        {
            warn!(
                "Failed to remove trajectory {} from index {:?}: {}",
                id, dir, e
            );
        }
    }

    let sessions = app.chat.sessions.clone();
    let fallback = ctx.trajectories.load_trajectory_by_chat_id(&id).await;
    let event = if let Some(mut fallback) = fallback {
        if fallback.transition_identity_repaired {
            ctx.trajectories
                .apply_mode_defaults_to_thread(
                    &mut fallback.thread,
                    fallback.auto_approve_editing_tools_present,
                    fallback.auto_approve_dangerous_commands_present,
                )
                .await;
            match persist_loaded_trajectory_repair_raw(&ctx, &fallback.repair_patch()).await {
                Ok(updated_at) => fallback.updated_at = updated_at,
                Err(e) => warn!(
                    "Failed to persist repaired trajectory for {}: {}",
                    fallback.thread.id, e
                ),
            }
        }
        let task_roots = ctx.trajectories.get_all_task_roots().await;
        let meta = loaded_trajectory_to_meta(&fallback, &task_roots);
        let (session_state, session_error, session_worktree) =
            get_session_runtime_for_trajectory_source(&sessions, &id, &meta.source).await;
        updated_trajectory_event_from_meta_with_worktree(
            meta,
            Some(fallback.thread.is_title_generated),
            session_state,
            session_error,
            session_worktree,
        )
    } else {
        deleted_trajectory_event(id.clone())
    };
    let _ = app.chat.trajectory_events_tx.send(event);
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"status":"ok"}"#))
        .unwrap())
}

pub async fn handle_v1_trajectories_subscribe(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let rx = app.chat.trajectory_events_tx.subscribe();
    let stream = async_stream::stream! {
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    match serde_json::to_string(&event) {
                        Ok(json) => yield Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", json)),
                        Err(e) => {
                            tracing::error!("Failed to serialize trajectory SSE event: {}", e);
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(Body::wrap_stream(stream))
        .unwrap())
}
