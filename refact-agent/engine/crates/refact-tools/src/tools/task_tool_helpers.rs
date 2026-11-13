use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_chat_api::TaskMeta;
use refact_runtime_api::ChatCtx;
use refact_runtime_api::task_storage as storage;

pub fn required_string(args: &HashMap<String, Value>, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Missing '{}'", key))
}

#[allow(dead_code)]
pub fn optional_string(args: &HashMap<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let take = max.saturating_sub(1);
    format!("{}…", s.chars().take(take).collect::<String>())
}

pub fn human_age_at(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = now.signed_duration_since(ts).num_seconds().max(0);
    if seconds == 0 {
        "now".to_string()
    } else if seconds < 60 {
        format!("{}s ago", seconds)
    } else if seconds < 60 * 60 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h ago", seconds / (60 * 60))
    } else {
        format!("{}d ago", seconds / (60 * 60 * 24))
    }
}

pub fn human_age(ts: DateTime<Utc>) -> String {
    human_age_at(ts, Utc::now())
}

pub async fn require_bound_planner_task(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
) -> Result<String, String> {
    let requested_task_id = match args.get("task_id") {
        Some(value) if value.is_null() => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| "task_id must be a string".to_string())?
                .trim()
                .to_string(),
        ),
        None => None,
    };

    let ccx_lock = ccx.lock().await;
    if let Some(meta) = &ccx_lock.task_meta {
        if meta.role != "planner" {
            return Err(
                "task observability tools can only be called by the task planner.".to_string(),
            );
        }
        if requested_task_id
            .as_deref()
            .is_some_and(|task_id| task_id != meta.task_id)
        {
            return Err("task_id override is not allowed from this planner chat".to_string());
        }
        return Ok(meta.task_id.clone());
    }

    let inferred_task_id = storage::infer_task_id_from_chat_id(&ccx_lock.chat_id);
    match (requested_task_id, inferred_task_id) {
        (Some(requested), Some(inferred)) if requested == inferred => Ok(inferred),
        (Some(_), _) => Err("task_id override is not allowed from this planner chat".to_string()),
        (None, Some(inferred)) => Ok(inferred),
        (None, None) => Err("Missing 'task_id' (and chat is not bound to a task)".to_string()),
    }
}

#[allow(dead_code)]
pub async fn require_agent_task_meta(
    ccx: &Arc<AMutex<AtCommandsContext>>,
) -> Result<TaskMeta, String> {
    let ccx_lock = ccx.lock().await;
    let meta = ccx_lock
        .task_meta
        .clone()
        .ok_or_else(|| "task agent context is required".to_string())?;
    if !matches!(meta.role.as_str(), "agent" | "agents") {
        return Err("task agent context is required".to_string());
    }
    Ok(meta)
}

pub async fn preflight_agent_model(
    ctx: &ChatCtx,
    model_name: &str,
) -> Result<(), String> {
    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| format!("Cannot spawn agent: failed to check model availability: {e}"))?;
    if caps.chat_models.contains_key(model_name) {
        return Ok(());
    }
    let available: Vec<&str> = caps
        .chat_models
        .keys()
        .map(|s| s.as_str())
        .take(5)
        .collect();
    let alternatives = if available.is_empty() {
        "no chat models configured".to_string()
    } else {
        available.join(", ")
    };
    Err(format!(
        "Cannot spawn agent: model '{model_name}' is not configured for chat. \
        Available chat models: {alternatives}. \
        Configure `task_planner_agent_model` in default models or pass a different model."
    ))
}

pub async fn wait_for_agent_abort(
    ctx: &ChatCtx,
    chat_id: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let session_arc = {
        let sessions = ctx.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    let Some(session_arc) = session_arc else {
        return Ok(());
    };
    let processor_running = {
        let mut session = session_arc.lock().await;
        session.abort_stream();
        session.close_event_channel();
        session.queue_notify.notify_waiters();
        session.queue_processor_running.clone()
    };

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let state = {
            let session = session_arc.lock().await;
            session.runtime.state
        };
        let state_stopped = !matches!(
            state,
            refact_runtime_api::SessionState::Generating
                | refact_runtime_api::SessionState::ExecutingTools
                | refact_runtime_api::SessionState::Paused
                | refact_runtime_api::SessionState::WaitingIde
                | refact_runtime_api::SessionState::WaitingUserInput
        );
        if state_stopped && !processor_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for agent {} to stop after abort",
                chat_id
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

pub const AGENT_ABORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

