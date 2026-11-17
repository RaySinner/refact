use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use eventsource_stream::Eventsource;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::connect_async_tls_with_config;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use refact_chat_api::diagnostics::safe_provider_error_diagnostic;
use refact_chat_api::openai_codex_ws::{
    OpenAICodexWebSocketResponseTracker, OpenAICodexWebSocketSession,
};
use refact_chat_api::types::{
    stream_heartbeat, stream_idle_timeout, stream_total_timeout, StreamIdleWatchdog,
};
use refact_chat_history::retry_policy::{classify_llm_error_for_retry, RetryDecision};
use refact_core::llm_types::BaseModelRecord;
use refact_llm::adapter::{AdapterSettings, HttpParts};
use refact_llm::{get_adapter, safe_truncate, LlmRequest, WireFormat};
use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};
use refact_privacy::destinations::clear_for_model;

use crate::app_state::AppState;
use crate::chat_ctx::ChatCtx;

pub use refact_llm::stream_core::*;

lazy_static::lazy_static! {
    static ref OPENAI_CODEX_UNBOUND_WEBSOCKET_SESSIONS:
        tokio::sync::Mutex<HashMap<String, OpenAICodexWebSocketSession>> =
        tokio::sync::Mutex::new(HashMap::new());
}

pub const DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES: usize = 32 * 1024 * 1024;
pub const MIN_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_MB: usize = 8;
pub const MAX_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_MB: usize = 512;

pub fn stack_mb_to_bytes_clamped(value: Option<&str>, default_bytes: usize) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|mb| {
            mb.clamp(
                MIN_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_MB,
                MAX_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_MB,
            ) * 1024
                * 1024
        })
        .unwrap_or(default_bytes)
}

pub fn openai_codex_websocket_thread_stack_bytes() -> usize {
    stack_mb_to_bytes_clamped(
        std::env::var("REFACT_OPENAI_CODEX_WS_STACK_MB")
            .ok()
            .as_deref(),
        DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES,
    )
}


pub async fn wait_for_abort_signal(
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
) {
    let Some(abort_flag) = abort_flag else {
        std::future::pending::<()>().await;
        return;
    };

    if abort_flag.load(Ordering::SeqCst) {
        return;
    }

    if let Some(notify) = abort_notify {
        let notified = notify.notified();
        tokio::pin!(notified);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_millis(50));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = &mut notified => return,
                _ = heartbeat.tick() => {
                    if abort_flag.load(Ordering::SeqCst) {
                        return;
                    }
                }
            }
        }
    } else {
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_millis(50));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            heartbeat.tick().await;
            if abort_flag.load(Ordering::SeqCst) {
                return;
            }
        }
    }
}

pub fn abort_requested(abort_flag: Option<&Arc<AtomicBool>>) -> bool {
    abort_flag.is_some_and(|flag| flag.load(Ordering::SeqCst))
}

pub const ABORT_ERROR_MESSAGE: &str = "Aborted";

pub fn is_abort_error(error: &str) -> bool {
    error == ABORT_ERROR_MESSAGE
}

pub async fn send_llm_http_request(
    client: &reqwest::Client,
    http_parts: &HttpParts,
    wire_format: WireFormat,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
) -> Result<reqwest::Response, String> {
    let accept = match wire_format {
        WireFormat::OllamaNative => "application/x-ndjson",
        _ => "text/event-stream",
    };

    let request = client
        .post(&http_parts.url)
        .headers(http_parts.headers.clone())
        .header(reqwest::header::ACCEPT, accept)
        .json(&http_parts.body)
        .send();

    tokio::select! {
        result = request => result.map_err(|e| format!("LLM request failed: {}", e)),
        _ = wait_for_abort_signal(abort_flag, abort_notify) => Err(ABORT_ERROR_MESSAGE.to_string()),
    }
}

pub fn llm_http_response_header_timeout_message(timeout: Duration) -> String {
    format!(
        "LLM HTTP SSE response header timeout after {}s",
        timeout.as_secs()
    )
}

pub async fn wait_before_llm_transport_retry(
    attempt: usize,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
) -> Result<(), String> {
    let shift = std::cmp::min(attempt, 3) as u32;
    let delay = Duration::from_millis(200 * 2u64.pow(shift));
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        _ = wait_for_abort_signal(abort_flag, abort_notify) => Err(ABORT_ERROR_MESSAGE.to_string()),
    }
}

pub async fn send_llm_http_request_with_header_timeout(
    client: &reqwest::Client,
    http_parts: &HttpParts,
    wire_format: WireFormat,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
    response_header_timeout: Duration,
    max_attempts: usize,
) -> Result<reqwest::Response, String> {
    let max_attempts = std::cmp::max(1, max_attempts);
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let request = send_llm_http_request(
            client,
            http_parts,
            wire_format,
            abort_flag.clone(),
            abort_notify.clone(),
        );
        let result = tokio::time::timeout(response_header_timeout, request).await;
        match result {
            Ok(Ok(response)) => {
                if attempt > 0 {
                    tracing::info!(
                        attempt = attempt + 1,
                        max_attempts,
                        "LLM HTTP SSE request recovered after retry"
                    );
                }
                return Ok(response);
            }
            Ok(Err(error)) if is_abort_error(&error) => return Err(error),
            Ok(Err(error)) => {
                last_error = error;
                if !classify_llm_error_for_retry(&last_error).is_retryable_transient() {
                    return Err(last_error);
                }
            }
            Err(_) => {
                last_error = llm_http_response_header_timeout_message(response_header_timeout);
            }
        }

        if attempt + 1 == max_attempts {
            break;
        }

        tracing::warn!(
            attempt = attempt + 1,
            max_attempts,
            error = %last_error,
            "LLM HTTP SSE request did not receive response headers; retrying"
        );
        wait_before_llm_transport_retry(attempt, abort_flag.clone(), abort_notify.clone()).await?;
    }

    Err(last_error)
}

pub fn should_commit_cache_guard_after_http_success(status: reqwest::StatusCode, text: &str) -> bool {
    if status.is_success() {
        return true;
    }
    !matches!(
        classify_llm_error_for_retry(&format_llm_error_body(&format!("{}", status), text, None)),
        RetryDecision::Retry { .. } | RetryDecision::ContextLimit { .. }
    )
}

pub fn openai_codex_instance_id(model_rec: &BaseModelRecord) -> Option<&str> {
    let (provider_name, _) = model_rec.id.split_once('/')?;
    (model_rec.endpoint.contains("chatgpt.com/backend-api")
        && (provider_name == "openai_codex"
            || model_rec
                .extra_headers
                .get(refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER)
                .is_some_and(|base_provider| base_provider == "openai_codex")))
    .then_some(provider_name)
}

pub fn claude_code_instance_id(model_rec: &BaseModelRecord) -> Option<&str> {
    let (provider_name, _) = model_rec.id.split_once('/')?;
    (model_rec.wire_format == refact_llm::WireFormat::AnthropicMessages
        && (provider_name == "claude_code"
            || model_rec
                .extra_headers
                .get(refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER)
                .is_some_and(|base_provider| base_provider == "claude_code")))
    .then_some(provider_name)
}

pub fn xai_oauth_instance_id(model_rec: &BaseModelRecord) -> Option<&str> {
    let (provider_name, _) = model_rec.id.split_once('/')?;
    (provider_name == "xai_oauth"
        || model_rec
            .extra_headers
            .get(refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER)
            .is_some_and(|base_provider| base_provider == "xai_oauth"))
    .then_some(provider_name)
}

pub fn is_litellm_model(model_rec: &BaseModelRecord) -> bool {
    model_rec
        .id
        .split_once('/')
        .is_some_and(|(provider_name, _)| provider_name == "litellm")
        || model_rec
            .extra_headers
            .get(refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER)
            .is_some_and(|base_provider| base_provider == "litellm")
}

pub fn xai_rate_limit_headers(headers: &reqwest::header::HeaderMap) -> HashMap<String, String> {
    refact_providers::xai_oauth::XAIOAuthProvider::filter_rate_limit_headers(
        headers
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str(), value))),
    )
}

pub async fn capture_xai_oauth_rate_limit_headers(
    app: &AppState,
    model_rec: &BaseModelRecord,
    headers: &reqwest::header::HeaderMap,
) {
    let Some(provider_instance_id) = xai_oauth_instance_id(model_rec) else {
        return;
    };
    let rate_limit_headers = xai_rate_limit_headers(headers);
    if rate_limit_headers.is_empty() {
        return;
    }

    let captured = {
        let mut registry = app.model.providers.write().await;
        registry
            .get_mut(provider_instance_id)
            .and_then(|provider| {
                provider
                    .as_any_mut()
                    .downcast_mut::<refact_providers::xai_oauth::XAIOAuthProvider>()
            })
            .is_some_and(|provider| provider.capture_rate_limit_headers(&rate_limit_headers))
    };
    if captured {
        // Keep follow-up work after the provider registry guard is dropped.
        // ProviderQuotaCache currently has no targeted invalidation API.
        tracing::debug!(
            provider = provider_instance_id,
            "captured xAI quota response headers"
        );
    }
}

pub fn claude_code_request_expires_at(model_rec: &BaseModelRecord) -> Option<i64> {
    model_rec
        .extra_headers
        .get(refact_providers::claude_code::CLAUDE_CODE_OAUTH_EXPIRES_AT_HEADER)
        .and_then(|expires_at| expires_at.parse::<i64>().ok())
}

pub fn is_openai_codex_chatgpt_backend(model_rec: &BaseModelRecord) -> bool {
    openai_codex_instance_id(model_rec).is_some()
}

pub fn llm_http_header_retry_config(model_rec: &BaseModelRecord) -> Option<(Duration, usize)> {
    let headers = &model_rec.extra_headers;
    let retry_enabled = headers
        .get(refact_providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_ENABLED_HEADER)
        .map(|value| value.eq_ignore_ascii_case("true") || value == "1");
    match retry_enabled {
        Some(true) => {}
        Some(false) => return None,
        None => return None,
    }
    let timeout = headers
        .get(refact_providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_TIMEOUT_SECONDS_HEADER)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| {
            Duration::from_secs(
                refact_providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_TIMEOUT_SECONDS_DEFAULT,
            )
        });
    let max_attempts = headers
        .get(refact_providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_MAX_ATTEMPTS_HEADER)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(refact_providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_MAX_ATTEMPTS_DEFAULT);
    Some((timeout, max_attempts))
}

pub fn openai_codex_websocket_auth_rejection_status(error: &str) -> Option<reqwest::StatusCode> {
    let lower = error.to_lowercase();
    if lower.contains("status=401")
        || lower.contains("status 401")
        || lower.contains("401 unauthorized")
    {
        return Some(reqwest::StatusCode::UNAUTHORIZED);
    }
    if lower.contains("status=403")
        || lower.contains("status 403")
        || lower.contains("403 forbidden")
    {
        return Some(reqwest::StatusCode::FORBIDDEN);
    }
    None
}

pub fn set_bearer_authorization_header(
    headers: &mut reqwest::header::HeaderMap,
    access_token: &str,
) -> Result<(), String> {
    let auth_value = reqwest::header::HeaderValue::from_str(&format!("Bearer {access_token}"))
        .map_err(|e| format!("Bearer credential cannot be used: {e}"))?;
    headers.insert(reqwest::header::AUTHORIZATION, auth_value);
    Ok(())
}

pub async fn force_refresh_openai_codex_for_retry(
    ctx: ChatCtx,
    http_client: &reqwest::Client,
    provider_instance_id: &str,
    status: reqwest::StatusCode,
    current_access_token: &str,
) -> Result<Option<String>, String> {
    let _guard = refact_providers::openai_codex::OpenAICodexProvider::lock_refresh_guard().await?;

    let (config_dir, provider) = {
        let registry = ctx.model.providers.read().await;
        let provider = registry
            .get(provider_instance_id)
            .and_then(|p| {
                p.as_any()
                    .downcast_ref::<refact_providers::openai_codex::OpenAICodexProvider>()
            })
            .cloned();
        let config_dir = ctx.paths.config_dir.clone();
        (config_dir, provider)
    };

    let Some(mut provider) = provider else {
        if let Some(message) =
            refact_providers::openai_codex::OpenAICodexProvider::codex_cli_unmanaged_refresh_message(
                current_access_token,
            )
        {
            return Err(message);
        }
        return Ok(None);
    };

    if let Some(access_token) = provider.access_token_changed_since_rejection(current_access_token)
    {
        return Ok(Some(access_token));
    }

    if provider.oauth_tokens.refresh_token.is_empty() {
        if let Some(message) =
            refact_providers::openai_codex::OpenAICodexProvider::codex_cli_unmanaged_refresh_message(
                current_access_token,
            )
        {
            return Err(message);
        }
    }

    if !refact_providers::openai_codex::OpenAICodexProvider::should_force_refresh_for_status(
        status,
        &provider.oauth_tokens.refresh_token,
        false,
    ) {
        return Ok(None);
    }

    let previous_tokens = provider.oauth_tokens.clone();
    let previous_session_id = provider.session_id.clone();
    let refresh_result = provider
        .force_refresh_after_auth_rejection(http_client, &config_dir, provider_instance_id)
        .await;

    if !provider.auth_state_matches(&previous_tokens, &previous_session_id) {
        let changed = {
            let mut registry = ctx.model.providers.write().await;
            registry
                .get_mut(provider_instance_id)
                .and_then(|p| {
                    p.as_any_mut()
                        .downcast_mut::<refact_providers::openai_codex::OpenAICodexProvider>()
                })
                .map(|current| {
                    current.update_auth_state_from_if_current(
                        &provider,
                        &previous_tokens,
                        &previous_session_id,
                    )
                })
                .unwrap_or(false)
        };

        if changed {
            let mut caps = ctx.model.caps.write().await;
            caps.caps = None;
            caps.last_attempted_ts = 0;
            ctx.tool_catalog_generations.advance_capabilities();
        }
    }

    refresh_result
}

pub async fn force_refresh_claude_code_for_retry(
    app: AppState,
    http_client: &reqwest::Client,
    provider_instance_id: &str,
    status: reqwest::StatusCode,
    current_access_token: &str,
    current_expires_at: Option<i64>,
) -> Result<Option<String>, String> {
    crate::providers_oauth_refresh::force_refresh_claude_code_for_retry(
        &app.gcx,
        http_client,
        provider_instance_id,
        current_access_token,
        current_expires_at,
        Some(status),
    )
    .await
    .map(|provider| provider.and_then(|provider| provider.resolve_auth().ok()))
}






pub fn openai_codex_websocket_endpoint(model_rec: &BaseModelRecord) -> Option<&str> {
    if !is_openai_codex_chatgpt_backend(model_rec) {
        return None;
    }
    model_rec
        .extra_headers
        .get(refact_providers::openai_codex::CODEX_WEBSOCKET_ENDPOINT_HEADER)
        .map(String::as_str)
        .filter(|endpoint| !endpoint.trim().is_empty())
}

pub fn build_openai_codex_websocket_message(body: &Value) -> Value {
    let mut message = body.clone();
    if let Some(obj) = message.as_object_mut() {
        obj.insert("type".to_string(), json!("response.create"));
        obj.entry("client_metadata".to_string())
            .or_insert_with(|| json!({}));
        if let Some(meta) = obj
            .get_mut("client_metadata")
            .and_then(|v| v.as_object_mut())
        {
            meta.insert(
                "x-codex-ws-stream-request-start-ms".to_string(),
                json!(chrono::Utc::now().timestamp_millis().to_string()),
            );
        }
        message
    } else {
        json!({"type": "response.create", "request": body})
    }
}

pub fn websocket_header_entries(
    headers: &reqwest::header::HeaderMap,
    turn_state: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();
    for (key, value) in headers {
        let key = key.as_str().to_ascii_lowercase();
        if key == "accept"
            || key == "content-type"
            || key == "content-length"
            || key == "connection"
            || key == "host"
            || key.starts_with("sec-websocket-")
            || key.starts_with("x-refact-internal-")
        {
            continue;
        }
        let value = value
            .to_str()
            .map_err(|e| format!("WebSocket header '{}' is not valid UTF-8: {}", key, e))?;
        entries.push((key, value.to_string()));
    }
    entries.push((
        "openai-beta".to_string(),
        "responses_websockets=2026-02-06".to_string(),
    ));
    if let Some(turn_state) = turn_state.filter(|s| !s.trim().is_empty()) {
        entries.push(("x-codex-turn-state".to_string(), turn_state.to_string()));
    }
    Ok(entries)
}

pub fn openai_codex_websocket_connection_key(
    websocket_endpoint: &str,
    http_parts: &HttpParts,
) -> String {
    let mut key = websocket_endpoint.to_string();
    for header in [
        reqwest::header::AUTHORIZATION.as_str(),
        "chatgpt-account-id",
        "originator",
        "session_id",
    ] {
        if let Some(value) = http_parts.headers.get(header).and_then(|v| v.to_str().ok()) {
            key.push('\n');
            key.push_str(header);
            key.push('=');
            key.push_str(value);
        }
    }
    key
}

pub fn openai_codex_body_without_stateful_fields(body: &Value) -> Value {
    let mut cloned = body.clone();
    if let Some(obj) = cloned.as_object_mut() {
        obj.remove("input");
        obj.remove("previous_response_id");
        obj.remove("generate");
        obj.remove("client_metadata");
    }
    cloned
}

pub fn openai_codex_input_items(body: &Value) -> Option<&Vec<Value>> {
    body.get("input").and_then(|v| v.as_array())
}

pub fn openai_codex_delta_input(
    current_body: &Value,
    last_request_body: &Value,
    session: &OpenAICodexWebSocketSession,
) -> Option<(String, Vec<Value>)> {
    let last_response = session.last_response.as_ref()?;
    if last_response.response_id.is_empty() {
        return None;
    }
    if openai_codex_body_without_stateful_fields(current_body)
        != openai_codex_body_without_stateful_fields(last_request_body)
    {
        return None;
    }

    let last_input = openai_codex_input_items(last_request_body)?;
    let current_input = openai_codex_input_items(current_body)?;
    let mut baseline = last_input.clone();
    baseline.extend(last_response.output_items.clone());
    if current_input.len() < baseline.len() {
        return None;
    }
    if !current_input
        .iter()
        .zip(baseline.iter())
        .all(|(a, b)| a == b)
    {
        return None;
    }

    Some((
        last_response.response_id.clone(),
        current_input[baseline.len()..].to_vec(),
    ))
}

pub fn prepare_openai_codex_websocket_body(
    body: &Value,
    session: &OpenAICodexWebSocketSession,
    prewarm: bool,
) -> Value {
    let mut prepared = body.clone();
    if let Some(obj) = prepared.as_object_mut() {
        if prewarm {
            obj.insert("generate".to_string(), json!(false));
        } else if let Some(last_request_body) = session.last_request_body.as_ref() {
            if let Some((previous_response_id, input_delta)) =
                openai_codex_delta_input(body, last_request_body, session)
            {
                obj.insert(
                    "previous_response_id".to_string(),
                    json!(previous_response_id),
                );
                obj.insert("input".to_string(), json!(input_delta));
            }
        }
    }
    build_openai_codex_websocket_message(&prepared)
}

pub fn clear_openai_codex_websocket_incremental_state(
    session: &mut OpenAICodexWebSocketSession,
    clear_turn_state: bool,
) {
    session.connection = None;
    session.last_request_body = None;
    session.last_response = None;
    if clear_turn_state {
        session.turn_state = None;
    }
}

pub async fn take_openai_codex_websocket_session(
    app: &AppState,
    chat_id: Option<&String>,
) -> OpenAICodexWebSocketSession {
    let Some(chat_id) = chat_id else {
        return OpenAICodexWebSocketSession::default();
    };
    let session_arc_opt = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    let Some(session_arc) = session_arc_opt else {
        return OPENAI_CODEX_UNBOUND_WEBSOCKET_SESSIONS
            .lock()
            .await
            .remove(chat_id)
            .unwrap_or_default();
    };
    let mut session = session_arc.lock().await;
    std::mem::take(&mut session.openai_codex_websocket)
}

pub async fn store_openai_codex_websocket_session(
    app: &AppState,
    chat_id: Option<&String>,
    websocket_session: OpenAICodexWebSocketSession,
) {
    let Some(chat_id) = chat_id else {
        return;
    };
    let session_arc_opt = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(chat_id).cloned()
    };
    if let Some(session_arc) = session_arc_opt {
        let mut session = session_arc.lock().await;
        session.openai_codex_websocket = websocket_session;
    } else {
        OPENAI_CODEX_UNBOUND_WEBSOCKET_SESSIONS
            .lock()
            .await
            .insert(chat_id.clone(), websocket_session);
    }
}

pub async fn clear_unbound_openai_codex_websocket_session(chat_id: &str) {
    OPENAI_CODEX_UNBOUND_WEBSOCKET_SESSIONS
        .lock()
        .await
        .remove(chat_id);
}

pub async fn commit_cache_guard_snapshot_if_needed(
    snapshot_for_commit: Option<(
        Arc<tokio::sync::Mutex<refact_chat_api::types::ChatSession>>,
        crate::chat_cache_guard::CacheGuardSnapshot,
    )>,
) {
    let Some((session_arc, snapshot)) = snapshot_for_commit else {
        return;
    };
    crate::chat_cache_guard::commit_cache_guard_snapshot(session_arc, snapshot).await;
}

pub async fn ensure_openai_codex_websocket_connected(
    websocket_endpoint: &str,
    http_parts: &HttpParts,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
    connect_timeout: Duration,
    session: &mut OpenAICodexWebSocketSession,
) -> Result<(), String> {
    let connection_key = openai_codex_websocket_connection_key(websocket_endpoint, http_parts);
    if session.connection_key.as_deref() != Some(connection_key.as_str()) {
        clear_openai_codex_websocket_incremental_state(session, true);
        session.connection_key = Some(connection_key);
    }
    if session.connection.is_some() {
        return Ok(());
    }

    let connect_started_at = Instant::now();
    let mut request = websocket_endpoint
        .into_client_request()
        .map_err(|e| format!("OpenAI Codex WebSocket request build failed: {}", e))?;
    {
        let headers = request.headers_mut();
        for (key, value) in
            websocket_header_entries(&http_parts.headers, session.turn_state.as_deref())?
        {
            let name = tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(key.as_bytes())
                .map_err(|e| {
                    format!("OpenAI Codex WebSocket header '{}' is invalid: {}", key, e)
                })?;
            let value = tokio_tungstenite::tungstenite::http::HeaderValue::from_str(&value)
                .map_err(|e| {
                    format!(
                        "OpenAI Codex WebSocket header '{}' value is invalid: {}",
                        key, e
                    )
                })?;
            headers.insert(name, value);
        }
    }

    let connect = tokio::time::timeout(
        connect_timeout,
        connect_async_tls_with_config(request, None, false, None),
    );
    let (websocket, response) = tokio::select! {
        result = connect => result
            .map_err(|_| "OpenAI Codex WebSocket connect timed out".to_string())?
            .map_err(|e| format!("OpenAI Codex WebSocket connect failed: {}", e))?,
        _ = wait_for_abort_signal(abort_flag.clone(), abort_notify.clone()) => return Err(ABORT_ERROR_MESSAGE.to_string()),
    };
    if let Some(turn_state) = response
        .headers()
        .get("x-codex-turn-state")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
    {
        session.turn_state = Some(turn_state.to_string());
    }
    session.connection = Some(websocket);
    tracing::info!(
        elapsed_ms = connect_started_at.elapsed().as_millis() as u64,
        has_turn_state = session.turn_state.is_some(),
        "OpenAI Codex WebSocket connected"
    );
    Ok(())
}

pub async fn run_openai_codex_websocket_exchange<C: StreamCollector>(
    websocket_endpoint: &str,
    http_parts: &HttpParts,
    adapter: &dyn refact_llm::adapter::LlmWireAdapter,
    auth_token: &str,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
    connect_timeout: Duration,
    session: &mut OpenAICodexWebSocketSession,
    prewarm: bool,
    collector: &mut C,
) -> Result<Vec<ChoiceFinal>, String> {
    ensure_openai_codex_websocket_connected(
        websocket_endpoint,
        http_parts,
        abort_flag.clone(),
        abort_notify.clone(),
        connect_timeout,
        session,
    )
    .await?;

    let request_body = prepare_openai_codex_websocket_body(&http_parts.body, session, prewarm);
    let request_kind = if prewarm {
        "prewarm"
    } else if request_body.get("previous_response_id").is_some() {
        "incremental"
    } else {
        "full"
    };
    let request_input_items = request_body
        .get("input")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let mut websocket = session
        .connection
        .take()
        .ok_or_else(|| "OpenAI Codex WebSocket connection is unavailable".to_string())?;
    let send = websocket.send(tokio_tungstenite::tungstenite::Message::Text(
        request_body.to_string(),
    ));
    tokio::select! {
        result = send => result.map_err(|e| format!("OpenAI Codex WebSocket send failed: {}", e))?,
        _ = wait_for_abort_signal(abort_flag.clone(), abort_notify.clone()) => return Err(ABORT_ERROR_MESSAGE.to_string()),
    };

    let mut accumulators: Vec<ChoiceAccumulator> = vec![ChoiceAccumulator::default()];
    let mut response_tracker = OpenAICodexWebSocketResponseTracker::default();
    let mut stream_done = false;
    let stream_started_at = Instant::now();
    let mut last_event_at = Instant::now();
    let mut heartbeat = tokio::time::interval(stream_heartbeat());
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if stream_done {
            break;
        }
        let message = tokio::select! {
            _ = heartbeat.tick() => {
                if let Some(ref flag) = abort_flag {
                    if flag.load(Ordering::SeqCst) {
                        return Err(ABORT_ERROR_MESSAGE.to_string());
                    }
                }
                if stream_started_at.elapsed() > stream_total_timeout() {
                    return Err("OpenAI Codex WebSocket stream timeout".to_string());
                }
                if last_event_at.elapsed() > stream_idle_timeout() {
                    return Err("OpenAI Codex WebSocket stream stalled".to_string());
                }
                continue;
            }
            _ = wait_for_abort_signal(abort_flag.clone(), abort_notify.clone()) => {
                return Err(ABORT_ERROR_MESSAGE.to_string());
            }
            maybe_message = websocket.next() => {
                match maybe_message {
                    Some(Ok(message)) => message,
                    Some(Err(e)) => {
                        return Err(format!("OpenAI Codex WebSocket stream error: {}", e));
                    }
                    None => {
                        if !stream_done {
                            return Err("OpenAI Codex WebSocket ended before completion".to_string());
                        }
                        break;
                    }
                }
            }
        };
        last_event_at = Instant::now();

        let data = match message {
            tokio_tungstenite::tungstenite::Message::Text(text) => text,
            tokio_tungstenite::tungstenite::Message::Binary(bytes) => String::from_utf8(bytes)
                .map_err(|e| {
                    format!("OpenAI Codex WebSocket binary message was not UTF-8: {}", e)
                })?,
            tokio_tungstenite::tungstenite::Message::Ping(_)
            | tokio_tungstenite::tungstenite::Message::Pong(_) => continue,
            tokio_tungstenite::tungstenite::Message::Close(_) => {
                if stream_done {
                    break;
                }
                return Err("OpenAI Codex WebSocket closed before completion".to_string());
            }
            tokio_tungstenite::tungstenite::Message::Frame(_) => continue,
        };

        if let Ok(event) = serde_json::from_str::<Value>(&data) {
            if let Some(error) = openai_codex_websocket_error_message(&event) {
                return Err(error);
            }
            response_tracker.observe_event(&event);
        }

        stream_done = process_stream_event_data(
            adapter,
            auth_token,
            &data,
            &mut accumulators,
            collector,
            true,
        )?;
    }

    session.last_request_body = Some(http_parts.body.clone());
    session.last_response = response_tracker.into_last_response();
    let response_id = session
        .last_response
        .as_ref()
        .map(|r| r.response_id.as_str())
        .unwrap_or("");
    let response_output_items = session
        .last_response
        .as_ref()
        .map(|r| r.output_items.len())
        .unwrap_or(0);
    session.connection = Some(websocket);
    tracing::info!(
        request_kind,
        elapsed_ms = stream_started_at.elapsed().as_millis() as u64,
        input_items = request_input_items,
        response_id_present = !response_id.is_empty(),
        output_items = response_output_items,
        "OpenAI Codex WebSocket exchange completed"
    );
    Ok(finalize_accumulators(accumulators, collector))
}

pub async fn close_openai_codex_websocket_connection(
    session: &mut OpenAICodexWebSocketSession,
    reason: &str,
) {
    let Some(mut websocket) = session.connection.take() else {
        return;
    };

    match tokio::time::timeout(Duration::from_millis(500), websocket.close(None)).await {
        Ok(Ok(())) => {
            tracing::debug!(reason, "OpenAI Codex WebSocket connection closed");
        }
        Ok(Err(error)) => {
            tracing::debug!(reason, %error, "OpenAI Codex WebSocket close failed");
        }
        Err(_) => {
            tracing::debug!(reason, "OpenAI Codex WebSocket close timed out");
        }
    }
}

pub async fn run_llm_websocket_request<C: StreamCollector>(
    websocket_endpoint: &str,
    http_parts: &HttpParts,
    adapter: &dyn refact_llm::adapter::LlmWireAdapter,
    auth_token: &str,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
    connect_timeout: Duration,
    session: &mut OpenAICodexWebSocketSession,
    collector: &mut C,
) -> Result<Vec<ChoiceFinal>, String> {
    session.turn_state = None;
    let should_prewarm = session.last_request_body.is_none();
    let current_thread = std::thread::current();
    let thread_name = current_thread.name().unwrap_or("unnamed");
    tracing::info!(
        prewarm = should_prewarm,
        has_previous_request = session.last_request_body.is_some(),
        body_bytes = json_value_size_estimate(&http_parts.body),
        thread_name,
        "OpenAI Codex WebSocket request starting"
    );
    if should_prewarm {
        let mut prewarm_collector = ReplayCollector::default();
        run_openai_codex_websocket_exchange(
            websocket_endpoint,
            http_parts,
            adapter,
            auth_token,
            abort_flag.clone(),
            abort_notify.clone(),
            connect_timeout,
            session,
            true,
            &mut prewarm_collector,
        )
        .await?;
    }

    let result = run_openai_codex_websocket_exchange(
        websocket_endpoint,
        http_parts,
        adapter,
        auth_token,
        abort_flag.clone(),
        abort_notify.clone(),
        connect_timeout,
        session,
        false,
        collector,
    )
    .await;

    match result {
        Err(error) if is_openai_codex_previous_response_missing_error(&error) => {
            tracing::warn!(
                "OpenAI Codex WebSocket previous_response_id was rejected; retrying this request without incremental state: {}",
                error
            );
            clear_openai_codex_websocket_incremental_state(session, true);
            run_openai_codex_websocket_exchange(
                websocket_endpoint,
                http_parts,
                adapter,
                auth_token,
                abort_flag,
                abort_notify,
                connect_timeout,
                session,
                false,
                collector,
            )
            .await
        }
        other => other,
    }
}

pub async fn refresh_openai_codex_after_websocket_auth_rejection(
    ctx: ChatCtx,
    http_client: &reqwest::Client,
    model_rec: &BaseModelRecord,
    status: reqwest::StatusCode,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
) -> Result<Option<String>, String> {
    let provider_instance_id = openai_codex_instance_id(model_rec).unwrap_or("openai_codex");
    tokio::select! {
        res = force_refresh_openai_codex_for_retry(
            ctx,
            http_client,
            provider_instance_id,
            status,
            &model_rec.api_key,
        ) => res,
        _ = wait_for_abort_signal(abort_flag, abort_notify) => Err(ABORT_ERROR_MESSAGE.to_string()),
    }
}

pub async fn run_llm_websocket_request_with_retry(
    wire_format: WireFormat,
    websocket_endpoint: String,
    http_parts: HttpParts,
    auth_token: String,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
    connect_timeout: Duration,
    max_attempts: usize,
    session: OpenAICodexWebSocketSession,
) -> OpenAICodexWebSocketThreadResult {
    let max_attempts = std::cmp::max(1, max_attempts);
    let mut session = session;
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let result = run_llm_websocket_request_on_large_stack(
            wire_format,
            websocket_endpoint.clone(),
            HttpParts {
                url: http_parts.url.clone(),
                headers: http_parts.headers.clone(),
                body: http_parts.body.clone(),
            },
            auth_token.clone(),
            abort_flag.clone(),
            abort_notify.clone(),
            connect_timeout,
            session,
        )
        .await;

        match result {
            Ok((results, replay_collector, returned_session)) => {
                if attempt > 0 {
                    tracing::info!(
                        attempt = attempt + 1,
                        max_attempts,
                        "OpenAI Codex WebSocket request recovered after retry"
                    );
                }
                return Ok((results, replay_collector, returned_session));
            }
            Err((error, returned_session)) => {
                if abort_requested(abort_flag.as_ref()) || is_abort_error(&error) {
                    return Err((error, returned_session));
                }
                last_error = error;
                session = returned_session;
                clear_openai_codex_websocket_incremental_state(&mut session, true);
                if !classify_llm_error_for_retry(&last_error).is_retryable_transient() {
                    return Err((last_error, session));
                }
            }
        }

        if attempt + 1 == max_attempts {
            break;
        }

        tracing::warn!(
            attempt = attempt + 1,
            max_attempts,
            error = %last_error,
            "OpenAI Codex WebSocket request failed; retrying"
        );
        if let Err(error) =
            wait_before_llm_transport_retry(attempt, abort_flag.clone(), abort_notify.clone()).await
        {
            return Err((error, session));
        }
    }

    Err((last_error, session))
}

pub type OpenAICodexWebSocketThreadResult = Result<
    (
        Vec<ChoiceFinal>,
        ReplayCollector,
        OpenAICodexWebSocketSession,
    ),
    (String, OpenAICodexWebSocketSession),
>;

pub async fn run_llm_websocket_request_on_large_stack(
    wire_format: WireFormat,
    websocket_endpoint: String,
    http_parts: HttpParts,
    auth_token: String,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
    connect_timeout: Duration,
    mut session: OpenAICodexWebSocketSession,
) -> OpenAICodexWebSocketThreadResult {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawn_result = std::thread::Builder::new()
        .name("openai-codex-websocket".to_string())
        .stack_size(openai_codex_websocket_thread_stack_bytes())
        .spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => {
                    let adapter = get_adapter(wire_format);
                    let mut replay_collector = ReplayCollector::default();
                    let outcome = runtime.block_on(async {
                        let outcome = run_llm_websocket_request(
                            &websocket_endpoint,
                            &http_parts,
                            adapter,
                            &auth_token,
                            abort_flag,
                            abort_notify,
                            connect_timeout,
                            &mut session,
                            &mut replay_collector,
                        )
                        .await;
                        let close_reason = if outcome.is_ok() {
                            "websocket_thread_success"
                        } else {
                            "websocket_thread_error"
                        };
                        close_openai_codex_websocket_connection(&mut session, close_reason).await;
                        outcome
                    });
                    match outcome {
                        Ok(results) => Ok((results, replay_collector, session)),
                        Err(error) => Err((error, session)),
                    }
                }
                Err(error) => Err((
                    format!("OpenAI Codex WebSocket runtime build failed: {}", error),
                    session,
                )),
            };
            let _ = tx.send(result);
        });

    if let Err(error) = spawn_result {
        return Err((
            format!("OpenAI Codex WebSocket thread spawn failed: {}", error),
            OpenAICodexWebSocketSession::default(),
        ));
    }

    rx.await.map_err(|error| {
        (
            format!(
                "OpenAI Codex WebSocket thread stopped before reporting result: {}",
                error
            ),
            OpenAICodexWebSocketSession::default(),
        )
    })?
}

pub fn openai_codex_websocket_error_message(event: &Value) -> Option<String> {
    if event.get("type").and_then(|t| t.as_str()) != Some("error") {
        return None;
    }
    let status = event
        .get("status")
        .or_else(|| event.get("status_code"))
        .and_then(|v| v.as_u64());
    let nested = event.get("error");
    let code = nested
        .and_then(|e| e.get("code"))
        .or_else(|| event.get("code"))
        .and_then(|v| v.as_str());
    let message = nested
        .and_then(|e| e.get("message"))
        .or_else(|| event.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("websocket error");
    Some(match (status, code) {
        (Some(status), Some(code)) => {
            format!("OpenAI Codex WebSocket error status={status} code={code}: {message}")
        }
        (Some(status), None) => {
            format!("OpenAI Codex WebSocket error status={status}: {message}")
        }
        (None, Some(code)) => format!("OpenAI Codex WebSocket error code={code}: {message}"),
        (None, None) => format!("OpenAI Codex WebSocket error: {message}"),
    })
}

pub fn is_openai_codex_previous_response_missing_error(error: &str) -> bool {
    error.contains("previous_response_not_found")
        || error.contains("Previous response with id")
        || error.contains("previous_response_id")
}

pub async fn run_llm_ndjson_request<C: StreamCollector>(
    response: reqwest::Response,
    adapter: &dyn refact_llm::adapter::LlmWireAdapter,
    auth_token: &str,
    abort_flag: Option<Arc<AtomicBool>>,
    abort_notify: Option<Arc<tokio::sync::Notify>>,
    collector: &mut C,
    request_sent_at: Option<Instant>,
    chat_id: Option<&str>,
    idle_watchdog: StreamIdleWatchdog,
) -> Result<Vec<ChoiceFinal>, String> {
    let mut stream = response.bytes_stream();
    let mut pending = Vec::new();
    let mut accumulators: Vec<ChoiceAccumulator> = vec![ChoiceAccumulator::default()];
    let mut stream_done = false;
    let mut provider_ttft_recorded = false;
    let stream_started_at = Instant::now();
    let mut last_event_at = Instant::now();
    let mut provider_progressed = false;
    let mut heartbeat = tokio::time::interval(stream_heartbeat());
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if stream_done {
            break;
        }
        let bytes = tokio::select! {
            _ = heartbeat.tick() => {
                if let Some(ref flag) = abort_flag {
                    if flag.load(Ordering::SeqCst) {
                        return Err(ABORT_ERROR_MESSAGE.to_string());
                    }
                }
                if stream_started_at.elapsed() > stream_total_timeout() {
                    return Err("LLM stream timeout".to_string());
                }
                if last_event_at.elapsed() > idle_watchdog.timeout(provider_progressed) {
                    return Err("LLM stream stalled".to_string());
                }
                continue;
            }
            _ = wait_for_abort_signal(abort_flag.clone(), abort_notify.clone()) => {
                return Err(ABORT_ERROR_MESSAGE.to_string());
            }
            maybe_bytes = stream.next() => {
                match maybe_bytes {
                    Some(Ok(bytes)) => bytes,
                    Some(Err(e)) => {
                        return Err(format!("Stream error: {}", e));
                    }
                    None => {
                        if process_ndjson_eof(
                            adapter,
                            auth_token,
                            &mut pending,
                            &mut accumulators,
                            collector,
                        )? {
                            break;
                        }
                        return Err("LLM stream ended unexpectedly without completion signal".to_string());
                    }
                }
            }
        };
        last_event_at = Instant::now();
        provider_progressed = true;
        let has_complete_event = request_sent_at.is_some() && !provider_ttft_recorded && {
            let mut line_has_content = false;
            pending.iter().chain(bytes.iter()).any(|byte| {
                if *byte == b'\n' {
                    let complete = line_has_content;
                    line_has_content = false;
                    complete
                } else {
                    line_has_content |= !byte.is_ascii_whitespace();
                    false
                }
            })
        };
        stream_done = process_ndjson_bytes(
            adapter,
            auth_token,
            &mut pending,
            &bytes,
            &mut accumulators,
            collector,
        )?;
        if !provider_ttft_recorded && has_complete_event {
            if let Some(started_at) = request_sent_at {
                perf_diagnostics::record(
                    PerfComponent::StreamProviderTtft,
                    chat_id,
                    PerfOutcome::Success,
                    started_at
                        .elapsed()
                        .as_micros()
                        .try_into()
                        .unwrap_or(u64::MAX),
                    None,
                    None,
                    None,
                );
            }
            provider_ttft_recorded = true;
        }
    }

    Ok(finalize_accumulators(accumulators, collector))
}

pub enum LlmStreamOutcome {
    Choices(Vec<ChoiceFinal>),
    PausedForCacheGuard,
}

pub fn build_http_for_attempt(
    app: &AppState,
    llm_request: &LlmRequest,
    model_rec: &BaseModelRecord,
    adapter: &dyn refact_llm::adapter::LlmWireAdapter,
    adapter_settings: &AdapterSettings,
) -> Result<HttpParts, String> {
    let cleared_request = clear_for_model(&app.ctx.privacy_policy_load, llm_request.clone(), model_rec)
        .map_err(|refusal| refusal.model_facing().to_string())?;
    adapter
        .build_http(&cleared_request, adapter_settings)
        .map_err(|error| format!("Failed to build LLM request: {error}"))
}

pub async fn run_llm_stream<C: StreamCollector>(
    app: AppState,
    params: StreamRunParams,
    collector: &mut C,
) -> Result<LlmStreamOutcome, LlmStreamError> {
    run_llm_stream_with_prepare_started(app, params, collector, None).await
}

pub async fn run_llm_stream_with_prepare_started<C: StreamCollector>(
    app: AppState,
    params: StreamRunParams,
    collector: &mut C,
    prepare_started_at: Option<Instant>,
) -> Result<LlmStreamOutcome, LlmStreamError> {
    let mut partial_output_emitted = false;

    if params.llm_request.params.n.unwrap_or(1) != 1 {
        return Err(LlmStreamError::new(
            "Streaming with n > 1 is not supported",
            partial_output_emitted,
        ));
    }

    let client = app.runtime.http_client.clone();

    let wire_format = params.model_rec.wire_format;
    let adapter = get_adapter(wire_format);

    let mut adapter_extra_headers = params.model_rec.extra_headers.clone();
    if is_litellm_model(&params.model_rec) {
        adapter_extra_headers.insert(
            refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER.to_string(),
            "litellm".to_string(),
        );
    }
    let adapter_settings = AdapterSettings {
        api_key: params.model_rec.api_key.clone(),
        auth_token: params.model_rec.auth_token.clone(),
        endpoint: params.model_rec.endpoint.clone(),
        extra_headers: adapter_extra_headers,
        model_name: params.model_rec.name.clone(),
        supports_tools: params.supports_tools,
        supports_reasoning: params.supports_reasoning,
        reasoning_type: params.reasoning_type.clone(),
        supports_temperature: params.supports_temperature,
        supports_max_completion_tokens: params.model_rec.supports_max_completion_tokens,
        eof_is_done: params.model_rec.eof_is_done,
        supports_web_search: params.model_rec.supports_web_search,
        supports_cache_control: params.model_rec.supports_cache_control,
    };

    let mut http_parts = build_http_for_attempt(
        &app,
        &params.llm_request,
        &params.model_rec,
        adapter,
        &adapter_settings,
    )
    .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?;

    let mut command_credential = tokio::select! {
        result = refact_providers::runtime_credential::resolve(&params.model_rec) => result,
        _ = wait_for_abort_signal(params.abort_flag.clone(), params.abort_notify.clone()) => {
            return Err(LlmStreamError::new(ABORT_ERROR_MESSAGE, partial_output_emitted));
        }
    }
    .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?;
    let mut rejected_command_credential: Option<String> = None;
    if let Some(value) = command_credential.as_deref() {
        set_bearer_authorization_header(&mut http_parts.headers, value)
            .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?;
    }

    refact_chat_api::cache_diagnostics::log_provider_request_hashes(
        params.chat_id.as_deref(),
        &params.llm_request.model_id,
        &http_parts.body,
    );

    let mut snapshot_for_commit = None;
    if let Some(chat_id) = &params.chat_id {
        let session_arc_opt = {
            let sessions = app.chat.sessions.read().await;
            sessions.get(chat_id).cloned()
        };
        if let Some(session_arc) = session_arc_opt {
            let outcome = tokio::select! {
                res = crate::chat_cache_guard::check_or_pause_cache_guard(
                    app.clone(),
                    session_arc.clone(),
                    &params.llm_request.model_id,
                    &http_parts.body,
                ) => res.map_err(|e| LlmStreamError::new(e, partial_output_emitted))?,
                _ = wait_for_abort_signal(params.abort_flag.clone(), params.abort_notify.clone()) => {
                    return Err(LlmStreamError::new(ABORT_ERROR_MESSAGE, partial_output_emitted));
                }
            };
            match outcome {
                crate::chat_cache_guard::CacheGuardOutcome::Pass(s) => {
                    snapshot_for_commit = s.map(|snapshot| (session_arc, snapshot));
                }
                crate::chat_cache_guard::CacheGuardOutcome::Paused { reason } => {
                    tracing::info!("Generation paused by cache guard: {}", reason);
                    return Ok(LlmStreamOutcome::PausedForCacheGuard);
                }
                crate::chat_cache_guard::CacheGuardOutcome::Error(e) => {
                    return Err(LlmStreamError::new(e, partial_output_emitted));
                }
            }
        }
    }

    if http_parts.url.is_empty() {
        return Err(LlmStreamError::new(
            "LLM endpoint URL is empty",
            partial_output_emitted,
        ));
    }

    tracing::debug!(
        url = %http_parts.url,
        model = %params.llm_request.model_id,
        messages_count = params.llm_request.messages.len(),
        "LLM streaming request"
    );

    // A large prompt takes proportionally longer to prefill, and the provider emits no
    // progress events while it does, so the idle watchdog gets a prompt-size-scaled
    // deadline for the prefill window only. Once the provider has actually delivered
    // something, generation speed — not prompt size — bounds a stall, so the watchdog
    // reverts to the base deadline.
    let prompt_tokens =
        refact_chat_history::trajectory_ops::approx_token_count(&params.llm_request.messages);
    let idle_watchdog =
        StreamIdleWatchdog::for_prompt_tokens((prompt_tokens > 0).then_some(prompt_tokens));

    if let Some(started_at) = prepare_started_at {
        perf_diagnostics::record(
            PerfComponent::StreamPrepare,
            params.chat_id.as_deref(),
            PerfOutcome::Success,
            started_at
                .elapsed()
                .as_micros()
                .try_into()
                .unwrap_or(u64::MAX),
            Some(json_value_size_estimate(&http_parts.body) as u64),
            Some(params.llm_request.messages.len() as u64),
            None,
        );
    }

    let header_retry_config = llm_http_header_retry_config(&params.model_rec);
    let mut codex_http_fallback_reason: Option<String> = None;
    if params.allow_websocket {
        if let Some(websocket_endpoint) = openai_codex_websocket_endpoint(&params.model_rec) {
            let websocket_session =
                take_openai_codex_websocket_session(&app, params.chat_id.as_ref()).await;
            let (connect_timeout, max_attempts) =
                header_retry_config.unwrap_or_else(|| (stream_idle_timeout(), 1));
            let mut websocket_session = websocket_session;
            let mut websocket_http_parts = HttpParts {
                url: http_parts.url.clone(),
                headers: http_parts.headers.clone(),
                body: http_parts.body.clone(),
            };
            loop {
                match run_llm_websocket_request_with_retry(
                    wire_format,
                    websocket_endpoint.to_string(),
                    HttpParts {
                        url: websocket_http_parts.url.clone(),
                        headers: websocket_http_parts.headers.clone(),
                        body: websocket_http_parts.body.clone(),
                    },
                    params.model_rec.auth_token.clone(),
                    params.abort_flag.clone(),
                    params.abort_notify.clone(),
                    connect_timeout,
                    max_attempts,
                    websocket_session,
                )
                .await
                {
                    Ok((results, replay_collector, returned_session)) => {
                        store_openai_codex_websocket_session(
                            &app,
                            params.chat_id.as_ref(),
                            returned_session,
                        )
                        .await;
                        commit_cache_guard_snapshot_if_needed(snapshot_for_commit.clone()).await;
                        replay_collector.replay(collector);
                        return Ok(LlmStreamOutcome::Choices(results));
                    }
                    Err((error, returned_session)) => {
                        if let Some(status) = openai_codex_websocket_auth_rejection_status(&error) {
                            match refresh_openai_codex_after_websocket_auth_rejection(
                                app.chat_ctx(),
                                &client,
                                &params.model_rec,
                                status,
                                params.abort_flag.clone(),
                                params.abort_notify.clone(),
                            )
                            .await
                            {
                                Ok(Some(access_token)) => {
                                    let mut retry_session = returned_session;
                                    clear_openai_codex_websocket_incremental_state(
                                        &mut retry_session,
                                        true,
                                    );
                                    if let Err(error) = set_bearer_authorization_header(
                                        &mut websocket_http_parts.headers,
                                        &access_token,
                                    ) {
                                        store_openai_codex_websocket_session(
                                            &app,
                                            params.chat_id.as_ref(),
                                            retry_session,
                                        )
                                        .await;
                                        return Err(LlmStreamError::new(
                                            error,
                                            partial_output_emitted,
                                        ));
                                    }
                                    websocket_session = retry_session;
                                    tracing::info!(
                                        status = status.as_u16(),
                                        "OpenAI Codex WebSocket auth rejected; refreshed token and retrying WebSocket"
                                    );
                                    continue;
                                }
                                Ok(None) => {}
                                Err(refresh_error) => {
                                    store_openai_codex_websocket_session(
                                        &app,
                                        params.chat_id.as_ref(),
                                        returned_session,
                                    )
                                    .await;
                                    return Err(LlmStreamError::new(
                                        refresh_error,
                                        partial_output_emitted,
                                    ));
                                }
                            }
                        }
                        store_openai_codex_websocket_session(
                            &app,
                            params.chat_id.as_ref(),
                            returned_session,
                        )
                        .await;
                        return Err(LlmStreamError::new(error, partial_output_emitted));
                    }
                }
            }
        } else if is_openai_codex_chatgpt_backend(&params.model_rec) {
            codex_http_fallback_reason = Some("websocket_endpoint_missing".to_string());
        }
    } else if is_openai_codex_chatgpt_backend(&params.model_rec) {
        codex_http_fallback_reason = Some("websocket_not_allowed".to_string());
    }

    if is_openai_codex_chatgpt_backend(&params.model_rec) {
        tracing::info!(
            reason = codex_http_fallback_reason
                .as_deref()
                .unwrap_or("websocket_not_attempted"),
            model = %params.llm_request.model_id,
            messages_count = params.llm_request.messages.len(),
            body_bytes = json_value_size_estimate(&http_parts.body),
            "OpenAI Codex HTTP SSE request starting"
        );
    }

    let request_sent_at = perf_diagnostics::is_enabled().then(Instant::now);
    let mut response = if let Some((response_header_timeout, max_attempts)) = header_retry_config {
        send_llm_http_request_with_header_timeout(
            &client,
            &http_parts,
            wire_format,
            params.abort_flag.clone(),
            params.abort_notify.clone(),
            response_header_timeout,
            max_attempts,
        )
        .await
    } else {
        send_llm_http_request(
            &client,
            &http_parts,
            wire_format,
            params.abort_flag.clone(),
            params.abort_notify.clone(),
        )
        .await
    }
    .map_err(|e| LlmStreamError::new(e, partial_output_emitted))?;
    let provider_ttft_started_at = request_sent_at;
    if let Some(started_at) = request_sent_at {
        perf_diagnostics::record(
            PerfComponent::StreamRequestSend,
            params.chat_id.as_deref(),
            PerfOutcome::Success,
            started_at
                .elapsed()
                .as_micros()
                .try_into()
                .unwrap_or(u64::MAX),
            None,
            None,
            None,
        );
    }
    capture_xai_oauth_rate_limit_headers(&app, &params.model_rec, response.headers()).await;
    let mut status = response.status();
    if !status.is_success()
        && matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        )
    {
        if let Some(rejected_value) = command_credential.as_deref() {
            let refreshed = tokio::select! {
                result = refact_providers::runtime_credential::refresh_after_rejection(&params.model_rec, rejected_value) => result,
                _ = wait_for_abort_signal(params.abort_flag.clone(), params.abort_notify.clone()) => {
                    return Err(LlmStreamError::new(ABORT_ERROR_MESSAGE, partial_output_emitted));
                }
            }
            .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?
            .ok_or_else(|| {
                LlmStreamError::new(
                    "Command credential refresh was unavailable",
                    partial_output_emitted,
                )
            })?;
            let mut retry_parts = HttpParts {
                url: http_parts.url.clone(),
                headers: http_parts.headers.clone(),
                body: http_parts.body.clone(),
            };
            set_bearer_authorization_header(&mut retry_parts.headers, &refreshed)
                .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?;
            rejected_command_credential = command_credential.replace(refreshed);
            response = if let Some((response_header_timeout, max_attempts)) = header_retry_config {
                send_llm_http_request_with_header_timeout(
                    &client,
                    &retry_parts,
                    wire_format,
                    params.abort_flag.clone(),
                    params.abort_notify.clone(),
                    response_header_timeout,
                    max_attempts,
                )
                .await
            } else {
                send_llm_http_request(
                    &client,
                    &retry_parts,
                    wire_format,
                    params.abort_flag.clone(),
                    params.abort_notify.clone(),
                )
                .await
            }
            .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?;
            capture_xai_oauth_rate_limit_headers(&app, &params.model_rec, response.headers()).await;
            status = response.status();
        }
        if let Some(provider_instance_id) = claude_code_instance_id(&params.model_rec) {
            let refresh_outcome = tokio::select! {
                result = force_refresh_claude_code_for_retry(
                    app.clone(),
                    &client,
                    provider_instance_id,
                    status,
                    &params.model_rec.auth_token,
                    claude_code_request_expires_at(&params.model_rec),
                ) => result,
                _ = wait_for_abort_signal(params.abort_flag.clone(), params.abort_notify.clone()) => {
                    return Err(LlmStreamError::new(ABORT_ERROR_MESSAGE, partial_output_emitted));
                }
            };
            if let Some(new_access_token) = refresh_outcome
                .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?
            {
                let mut retry_parts = HttpParts {
                    url: http_parts.url.clone(),
                    headers: http_parts.headers.clone(),
                    body: http_parts.body.clone(),
                };
                set_bearer_authorization_header(&mut retry_parts.headers, &new_access_token)
                    .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?;
                response =
                    if let Some((response_header_timeout, max_attempts)) = header_retry_config {
                        send_llm_http_request_with_header_timeout(
                            &client,
                            &retry_parts,
                            wire_format,
                            params.abort_flag.clone(),
                            params.abort_notify.clone(),
                            response_header_timeout,
                            max_attempts,
                        )
                        .await
                    } else {
                        send_llm_http_request(
                            &client,
                            &retry_parts,
                            wire_format,
                            params.abort_flag.clone(),
                            params.abort_notify.clone(),
                        )
                        .await
                    }
                    .map_err(|error| LlmStreamError::new(error, partial_output_emitted))?;
                capture_xai_oauth_rate_limit_headers(&app, &params.model_rec, response.headers())
                    .await;
                status = response.status();
            }
        }
    }
    if !status.is_success()
        && is_openai_codex_chatgpt_backend(&params.model_rec)
        && matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        )
    {
        let provider_instance_id =
            openai_codex_instance_id(&params.model_rec).unwrap_or("openai_codex");
        let refresh_outcome = tokio::select! {
            res = force_refresh_openai_codex_for_retry(
                app.chat_ctx(),
                &client,
                provider_instance_id,
                status,
                &params.model_rec.api_key,
            ) => res,
            _ = wait_for_abort_signal(params.abort_flag.clone(), params.abort_notify.clone()) => {
                return Err(LlmStreamError::new(ABORT_ERROR_MESSAGE, partial_output_emitted));
            }
        };
        match refresh_outcome.map_err(|e| LlmStreamError::new(e, partial_output_emitted))? {
            Some(new_access_token) => {
                let mut retry_parts = HttpParts {
                    url: http_parts.url.clone(),
                    headers: http_parts.headers.clone(),
                    body: http_parts.body.clone(),
                };
                set_bearer_authorization_header(&mut retry_parts.headers, &new_access_token)
                    .map_err(|e| LlmStreamError::new(e, partial_output_emitted))?;
                response =
                    if let Some((response_header_timeout, max_attempts)) = header_retry_config {
                        send_llm_http_request_with_header_timeout(
                            &client,
                            &retry_parts,
                            wire_format,
                            params.abort_flag.clone(),
                            params.abort_notify.clone(),
                            response_header_timeout,
                            max_attempts,
                        )
                        .await
                    } else {
                        send_llm_http_request(
                            &client,
                            &retry_parts,
                            wire_format,
                            params.abort_flag.clone(),
                            params.abort_notify.clone(),
                        )
                        .await
                    }
                    .map_err(|e| LlmStreamError::new(e, partial_output_emitted))?;
                capture_xai_oauth_rate_limit_headers(&app, &params.model_rec, response.headers())
                    .await;
                status = response.status();
            }
            None => {}
        }
    }

    if !status.is_success() {
        let text = tokio::select! {
            body = read_error_body_bounded(response) => body,
            _ = wait_for_abort_signal(params.abort_flag.clone(), params.abort_notify.clone()) => {
                return Err(LlmStreamError::new(ABORT_ERROR_MESSAGE, partial_output_emitted));
            }
        };
        if should_commit_cache_guard_after_http_success(status, &text) {
            commit_cache_guard_snapshot_if_needed(snapshot_for_commit).await;
        }
        let text = refact_providers::runtime_credential::redact(
            text,
            rejected_command_credential.as_deref(),
        );
        return Err(LlmStreamError::new(
            format_llm_error_body(&format!("{}", status), &text, command_credential.as_deref()),
            partial_output_emitted,
        ));
    }

    commit_cache_guard_snapshot_if_needed(snapshot_for_commit).await;

    let progress_events = Arc::new(AtomicU64::new(0));
    let mut tracking_collector = PartialOutputCollector {
        collector,
        partial_output_emitted: &mut partial_output_emitted,
        progress_events: progress_events.clone(),
    };

    if wire_format == WireFormat::OllamaNative {
        return run_llm_ndjson_request(
            response,
            adapter,
            &params.model_rec.auth_token,
            params.abort_flag.clone(),
            params.abort_notify.clone(),
            &mut tracking_collector,
            provider_ttft_started_at,
            params.chat_id.as_deref(),
            idle_watchdog,
        )
        .await
        .map_err(|e| LlmStreamError::new(e, *tracking_collector.partial_output_emitted))
        .map(LlmStreamOutcome::Choices);
    }

    let mut stream = response.bytes_stream().eventsource();

    let mut accumulators: Vec<ChoiceAccumulator> = vec![ChoiceAccumulator::default()];
    let mut stream_done = false;
    let mut provider_ttft_recorded = false;

    let stream_started_at = Instant::now();
    let mut last_progress_event_count = 0;
    let mut last_progress_at = Instant::now();
    let mut provider_progressed = false;
    let mut heartbeat = tokio::time::interval(stream_heartbeat());
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if stream_done {
            break;
        }
        let event = tokio::select! {
            _ = heartbeat.tick() => {
                if let Some(ref flag) = params.abort_flag {
                    if flag.load(Ordering::SeqCst) {
                        return Err(LlmStreamError::new(
                            ABORT_ERROR_MESSAGE,
                            *tracking_collector.partial_output_emitted,
                        ));
                    }
                }
                if stream_started_at.elapsed() > stream_total_timeout() {
                    return Err(LlmStreamError::new(
                        "LLM stream timeout",
                        *tracking_collector.partial_output_emitted,
                    ));
                }
                let progress_event_count = progress_events.load(Ordering::SeqCst);
                if progress_event_count != last_progress_event_count {
                    last_progress_event_count = progress_event_count;
                    last_progress_at = Instant::now();
                    provider_progressed = true;
                }
                if last_progress_at.elapsed() > idle_watchdog.timeout(provider_progressed) {
                    return Err(LlmStreamError::new(
                        "LLM stream stalled",
                        *tracking_collector.partial_output_emitted,
                    ));
                }
                continue;
            }
            _ = wait_for_abort_signal(params.abort_flag.clone(), params.abort_notify.clone()) => {
                return Err(LlmStreamError::new(
                    ABORT_ERROR_MESSAGE,
                    *tracking_collector.partial_output_emitted,
                ));
            }
            maybe_event = stream.next() => {
                match maybe_event {
                    Some(Ok(ev)) => ev,
                    Some(Err(e)) => {
                        return Err(LlmStreamError::new(
                            format!("Stream error: {}", e),
                            *tracking_collector.partial_output_emitted,
                        ));
                    }
                    None => {
                        if !stream_done && !adapter_settings.eof_is_done {
                            if !should_finish_on_anthropic_stop_reason_eof(wire_format, &accumulators) {
                                return Err(LlmStreamError::new(
                                    "LLM stream ended unexpectedly without completion signal",
                                    *tracking_collector.partial_output_emitted,
                                ));
                            }
                        }
                        break;
                    }
                }
            }
        };
        stream_done = process_stream_event_data(
            adapter,
            &params.model_rec.auth_token,
            &event.data,
            &mut accumulators,
            &mut tracking_collector,
            false,
        )
        .map_err(|e| LlmStreamError::new(e, *tracking_collector.partial_output_emitted))?;
        if !provider_ttft_recorded {
            if let Some(started_at) = provider_ttft_started_at {
                perf_diagnostics::record(
                    PerfComponent::StreamProviderTtft,
                    params.chat_id.as_deref(),
                    PerfOutcome::Success,
                    started_at
                        .elapsed()
                        .as_micros()
                        .try_into()
                        .unwrap_or(u64::MAX),
                    None,
                    None,
                    None,
                );
            }
            provider_ttft_recorded = true;
        }
    }

    let results = finalize_accumulators(accumulators, &mut tracking_collector);

    Ok(LlmStreamOutcome::Choices(results))
}


pub async fn read_error_body_bounded(response: reqwest::Response) -> String {
    const ERROR_BODY_MAX_BYTES: usize = 64 * 1024;
    const ERROR_BODY_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    let timeout = ERROR_BODY_READ_TIMEOUT;

    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut truncated = false;

    let read_fut = async {
        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(chunk) => {
                    let remaining = ERROR_BODY_MAX_BYTES.saturating_sub(buffer.len());
                    if remaining == 0 {
                        truncated = true;
                        break;
                    }
                    if chunk.len() > remaining {
                        buffer.extend_from_slice(&chunk[..remaining]);
                        truncated = true;
                        break;
                    }
                    buffer.extend_from_slice(&chunk);
                }
                Err(_) => break,
            }
        }
    };

    let timed_out = tokio::time::timeout(timeout, read_fut).await.is_err();

    let mut text = String::from_utf8_lossy(&buffer).into_owned();
    if truncated {
        text.push_str(" [error body truncated]");
    }
    if timed_out {
        text.push_str(" [error body read timeout]");
    }
    text
}

pub fn format_llm_error_body(status_label: &str, text: &str, secret: Option<&str>) -> String {
    let redacted = match secret.filter(|secret| !secret.is_empty()) {
        Some(secret) => text.replace(secret, "[REDACTED]"),
        None => text.to_string(),
    };
    let redacted = safe_provider_error_diagnostic(&redacted);
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&redacted) {
        if let Some(detail) = json.get("detail") {
            return format!("LLM error ({}): {}", status_label, detail);
        }
        if let Some(msg) = json.pointer("/error/message") {
            return format!("LLM error ({}): {}", status_label, msg);
        }
        if let Some(err_obj) = json.get("error") {
            return format!("LLM error ({}): {}", status_label, err_obj);
        }
    }
    let preview = safe_truncate(&redacted, 500);
    format!("LLM error ({}): {}", status_label, preview)
}

