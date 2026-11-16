use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex as AMutex;

pub use refact_chat_api::cache_guard::*;

use crate::app_state::AppState;
use refact_chat_api::types::{ChatSession, TaskMeta};
use crate::chat_tokens::cached_tokenizer;
use refact_core::tokens_runtime::count_text_tokens_with_fallback;

pub enum CacheGuardOutcome {
    Pass(Option<CacheGuardSnapshot>),
    Paused { reason: String },
    Error(String),
}

#[derive(Clone)]
pub struct CacheGuardSnapshot {
    pub body: Value,
    pub request_generation: u64,
    pub reset_generation: u64,
}

pub async fn is_guard_enabled(app: AppState, model_id: &str, session: &ChatSession) -> bool {
    if is_non_interactive_thread(&session.thread) {
        return false;
    }

    is_guard_enabled_for_task_meta(app, model_id, session.thread.task_meta.as_ref()).await
}

async fn is_guard_enabled_for_task_meta(
    app: AppState,
    model_id: &str,
    task_meta: Option<&TaskMeta>,
) -> bool {
    if task_meta.is_some_and(|meta| is_task_management_role(&meta.role)) {
        return false;
    }

    model_supports_cache_guard(app, model_id).await
}

async fn model_supports_cache_guard(app: AppState, model_id: &str) -> bool {
    crate::caps_loader::try_load_caps_quickly_if_not_present(app.gcx.clone(), 0)
        .await
        .ok()
        .and_then(|caps| crate::caps_load_runtime::resolve_chat_model(caps, model_id).ok())
        .is_some_and(|record| chat_model_supports_cache_guard(&record))
}

pub async fn estimate_extra_cache_miss_usd(
    app: AppState,
    model_id: &str,
    previous_sanitized: &Value,
) -> Option<f64> {
    let pricing = crate::providers_pricing_runtime::lookup_model_pricing(&app.gcx, model_id).await?;
    let cache_read_rate = pricing.cache_read?;
    if pricing.prompt <= cache_read_rate {
        return Some(0.0);
    }

    let previous_pretty = serde_json::to_string_pretty(previous_sanitized).ok()?;
    let model_rec = {
        let caps = crate::caps_loader::try_load_caps_quickly_if_not_present(app.gcx.clone(), 0)
            .await
            .ok()?;
        crate::caps_load_runtime::resolve_chat_model(caps, model_id).ok()?
    };
    let tokenizer = cached_tokenizer(app.gcx, &model_rec.base)
        .await
        .ok()
        .flatten();
    let cached_tokens = count_text_tokens_with_fallback(tokenizer, &previous_pretty);
    let delta_rate = pricing.prompt - cache_read_rate;
    Some((cached_tokens as f64) * delta_rate / 1_000_000.0)
}

pub async fn check_or_pause_cache_guard(
    app: AppState,
    session_arc: Arc<AMutex<refact_chat_api::types::ChatSession>>,
    model_id: &str,
    request_body: &Value,
) -> Result<CacheGuardOutcome, String> {
    let (task_meta, non_interactive) = {
        let session = session_arc.lock().await;
        (
            session.thread.task_meta.clone(),
            is_non_interactive_thread(&session.thread),
        )
    };
    let has_explicit_markers = has_anthropic_explicit_cache_markers(request_body);
    if !has_explicit_markers {
        return Ok(CacheGuardOutcome::Pass(None));
    }

    if non_interactive {
        return Ok(CacheGuardOutcome::Pass(None));
    }

    if !is_guard_enabled_for_task_meta(app.clone(), model_id, task_meta.as_ref()).await {
        return Ok(CacheGuardOutcome::Pass(None));
    }

    // OpenAI Responses API stateful mode: when previous_response_id is present,
    // the server handles caching via response chaining. The request body intentionally
    // sends only tail items (not the full conversation), so the append-only prefix
    // check does not apply.
    if request_body
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
    {
        return Ok(CacheGuardOutcome::Pass(None));
    }

    let sanitized = body_for_cache_guard_with_boundary(
        request_body,
        anthropic_cache_prefix_boundary(request_body),
    );

    let (request_generation, reset_generation, maybe_violation_prev) = {
        let mut session = session_arc.lock().await;
        session.cache_guard_request_generation = session
            .cache_guard_request_generation
            .checked_add(1)
            .expect("cache guard request generation overflow");
        let request_generation = session.cache_guard_request_generation;
        let reset_generation = session.cache_guard_reset_generation;
        if session.cache_guard_force_next {
            (request_generation, reset_generation, None)
        } else if let Some(prev) = session.cache_guard_snapshot.as_ref() {
            if cache_prefix_compatible(prev, &sanitized, has_explicit_markers) {
                (request_generation, reset_generation, None)
            } else {
                (request_generation, reset_generation, Some(prev.clone()))
            }
        } else {
            (request_generation, reset_generation, None)
        }
    };

    let Some(previous) = maybe_violation_prev else {
        return Ok(CacheGuardOutcome::Pass(Some(CacheGuardSnapshot {
            body: sanitized,
            request_generation,
            reset_generation,
        })));
    };

    let mut diff = unified_json_diff(
        &preview_body_for_cache_guard_diff(&previous),
        &preview_body_for_cache_guard_diff(&sanitized),
    );
    diff.push_str(CACHE_GUARD_DIFF_PREVIEW_NOTICE);
    let diff = cap_cache_guard_diff(&diff);
    let estimated_extra_usd = estimate_extra_cache_miss_usd(app.clone(), model_id, &previous).await;

    let reason = {
        let mut session = session_arc.lock().await;
        if session.cache_guard_force_next {
            return Ok(CacheGuardOutcome::Pass(Some(CacheGuardSnapshot {
                body: sanitized,
                request_generation,
                reset_generation,
            })));
        }
        if let Some(outcome) = cache_guard_outcome_if_snapshot_changed(
            session.cache_guard_snapshot.as_ref(),
            &previous,
            &sanitized,
            has_explicit_markers,
            request_generation,
            reset_generation,
        ) {
            return Ok(outcome);
        }
        session.discard_draft_for_pause();
        session
            .abort_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        session.abort_notify.notify_waiters();

        let mut summary =
            format!("Prompt cache append-only prefix check failed for model `{model_id}`.\n\n");
        if let Some(extra) = estimated_extra_usd {
            summary.push_str(&format!(
                "Estimated extra cost if cache miss occurs: `${extra:.6}` USD.\n\n"
            ));
        }
        summary.push_str("Unified diff (sanitized provider request body):\n\n");
        summary.push_str("```diff\n");
        summary.push_str(&diff);
        summary.push_str("\n```\n");

        let reasons = vec![refact_chat_api::types::PauseReason {
            reason_type: "confirmation".to_string(),
            tool_name: CACHE_GUARD_TOOL_NAME.to_string(),
            command: summary.clone(),
            rule: CACHE_GUARD_RULE.to_string(),
            tool_call_id: CACHE_GUARD_TOOL_CALL_ID.to_string(),
            integr_config_path: None,
        }];
        session.set_paused_with_reasons_and_auto_approved(reasons, Vec::new(), None);
        summary
    };

    Ok(CacheGuardOutcome::Paused { reason })
}

pub fn cache_guard_outcome_if_snapshot_changed(
    current: Option<&Value>,
    captured_previous: &Value,
    sanitized: &Value,
    has_explicit_markers: bool,
    request_generation: u64,
    reset_generation: u64,
) -> Option<CacheGuardOutcome> {
    if current == Some(captured_previous) {
        return None;
    }
    let Some(current) = current else {
        return Some(CacheGuardOutcome::Pass(Some(CacheGuardSnapshot {
            body: sanitized.clone(),
            request_generation,
            reset_generation,
        })));
    };
    if cache_prefix_compatible(current, sanitized, has_explicit_markers) {
        Some(CacheGuardOutcome::Pass(Some(CacheGuardSnapshot {
            body: sanitized.clone(),
            request_generation,
            reset_generation,
        })))
    } else {
        None
    }
}

// `cache_guard_snapshot` is an in-memory-only canonical provider request body for
// cache-prefix comparison; trajectory persistence intentionally omits it.
pub async fn commit_cache_guard_snapshot(
    session_arc: Arc<AMutex<refact_chat_api::types::ChatSession>>,
    snapshot: CacheGuardSnapshot,
) {
    let mut session = session_arc.lock().await;
    if snapshot.request_generation > session.cache_guard_snapshot_generation
        && snapshot.reset_generation == session.cache_guard_reset_generation
    {
        session.cache_guard_snapshot = Some(snapshot.body);
        session.cache_guard_snapshot_generation = snapshot.request_generation;
        session.cache_guard_force_next = false;
    }
}
