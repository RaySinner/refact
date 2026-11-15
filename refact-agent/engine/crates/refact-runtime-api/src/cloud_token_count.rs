use std::time::Duration;

use reqwest::header::{HeaderMap, ACCEPT, CONTENT_TYPE};
use serde_json::Value;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use refact_core::chat_types::ChatUsage;
use refact_core::llm_types::BaseModelRecord;
use refact_core::model_caps::{ANTHROPIC_CLOUD_TOKENIZER, CLAUDE_CLOUD_TOKENIZER_ALIAS};
use refact_llm::adapter::{AdapterSettings, HttpParts, LlmWireAdapter};
use refact_llm::canonical::LlmRequest;
use refact_privacy::destinations::clear_for_model;

use crate::chat_ctx::ChatCtx;

const CLOUD_TOKEN_COUNT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloudTokenizerKind {
    Anthropic,
}

#[derive(Debug, Clone)]
pub struct CloudInputTokenCount {
    pub usage: ChatUsage,
    pub output_token_reserve: usize,
}

pub fn spawn_input_token_count_task(
    ctx: ChatCtx,
    client: reqwest::Client,
    req: &LlmRequest,
    model_rec: &BaseModelRecord,
    chat_id: Option<String>,
) -> Option<JoinHandle<Option<CloudInputTokenCount>>> {
    let cloud_tokenizer_kind = cloud_tokenizer_kind(&model_rec.tokenizer)?;
    let started_at = refact_perf::perf_diagnostics::is_enabled().then(std::time::Instant::now);
    let req = req.clone();
    let model_rec = model_rec.clone();
    Some(tokio::spawn(async move {
        let result = match cloud_tokenizer_kind {
            CloudTokenizerKind::Anthropic => {
                try_count_input_tokens(&ctx, &client, &req, &model_rec).await
            }
        };
        if let Some(started_at) = started_at {
            refact_perf::perf_diagnostics::record(
                refact_perf::perf_diagnostics::PerfComponent::StreamTokenCountRequest,
                chat_id.as_deref(),
                refact_perf::perf_diagnostics::PerfOutcome::Success,
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
        result
    }))
}

fn cloud_tokenizer_kind(tokenizer: &str) -> Option<CloudTokenizerKind> {
    match tokenizer.trim().to_ascii_lowercase().as_str() {
        ANTHROPIC_CLOUD_TOKENIZER | CLAUDE_CLOUD_TOKENIZER_ALIAS => {
            Some(CloudTokenizerKind::Anthropic)
        }
        _ => None,
    }
}

fn set_path_segments(url: &str, segments: &[&str]) -> Option<String> {
    let mut parsed = url::Url::parse(url).ok()?;
    {
        let mut path_segments = parsed.path_segments_mut().ok()?;
        path_segments.clear();
        path_segments.extend(segments.iter().copied());
    }
    Some(parsed.to_string())
}

fn path_segments(url: &str) -> Option<Vec<String>> {
    Some(
        url::Url::parse(url)
            .ok()?
            .path_segments()?
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

pub fn anthropic_count_url(endpoint: &str) -> Option<String> {
    let mut segments = path_segments(endpoint)?;
    if segments.last().map(String::as_str) == Some("count_tokens") {
        return set_path_segments(
            endpoint,
            &segments.iter().map(String::as_str).collect::<Vec<_>>(),
        );
    }
    if segments.last().map(String::as_str) == Some("messages") {
        segments.push("count_tokens".to_string());
        return set_path_segments(
            endpoint,
            &segments.iter().map(String::as_str).collect::<Vec<_>>(),
        );
    }
    None
}

pub fn strip_anthropic_count_unsupported_fields(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        for key in [
            "max_tokens",
            "temperature",
            "stop_sequences",
            "stream",
            "metadata",
            "service_tier",
            "container",
            "context_management",
            "mcp_servers",
        ] {
            obj.remove(key);
        }
    }
}

pub fn body_usize(body: &Value, key: &str) -> Option<usize> {
    body.get(key)?.as_u64().map(|value| value as usize)
}

fn anthropic_count_http_parts(
    ctx: &ChatCtx,
    req: &LlmRequest,
    model_rec: &BaseModelRecord,
    settings: &AdapterSettings,
) -> Result<(HttpParts, usize), String> {
    if settings.api_key.is_empty() && settings.auth_token.is_empty() {
        return Err("Anthropic cloud token count requires an API key or auth token".to_string());
    }
    let count_url = anthropic_count_url(&settings.endpoint)
        .ok_or_else(|| "Anthropic cloud token count requires a /messages endpoint".to_string())?;

    let cleared_req = clear_for_model(&ctx.privacy_policy_load, req.clone(), model_rec)
        .map_err(|refusal| refusal.model_facing().to_string())?;
    let mut http =
        refact_llm::adapters::anthropic::AnthropicAdapter.build_http(&cleared_req, settings)?;
    let output_token_reserve =
        body_usize(&http.body, "max_tokens").unwrap_or(req.params.max_tokens);
    http.url = count_url;
    strip_anthropic_count_unsupported_fields(&mut http.body);
    Ok((http, output_token_reserve))
}

fn cloud_count_http_parts(
    ctx: &ChatCtx,
    req: &LlmRequest,
    model_rec: &BaseModelRecord,
) -> Result<Option<(HttpParts, usize)>, String> {
    let Some(kind) = cloud_tokenizer_kind(&model_rec.tokenizer) else {
        return Ok(None);
    };

    let api_key = if model_rec.tokenizer_api_key.is_empty() {
        model_rec.api_key.clone()
    } else {
        model_rec.tokenizer_api_key.clone()
    };

    let settings = AdapterSettings {
        api_key,
        auth_token: model_rec.auth_token.clone(),
        endpoint: model_rec.endpoint.clone(),
        extra_headers: model_rec.extra_headers.clone(),
        model_name: model_rec.name.clone(),
        supports_tools: true,
        supports_reasoning: true,
        reasoning_type: None,
        supports_temperature: true,
        supports_max_completion_tokens: model_rec.supports_max_completion_tokens,
        eof_is_done: model_rec.eof_is_done,
        supports_web_search: model_rec.supports_web_search,
        supports_cache_control: model_rec.supports_cache_control,
    };

    let parts = match kind {
        CloudTokenizerKind::Anthropic => {
            anthropic_count_http_parts(ctx, req, model_rec, &settings)?
        }
    };
    Ok(Some(parts))
}

fn response_input_tokens(body: &Value) -> Option<usize> {
    body.get("input_tokens")?.as_u64().map(|n| n as usize)
}

fn header_names(headers: &HeaderMap) -> Vec<String> {
    headers.keys().map(|key| key.as_str().to_string()).collect()
}

async fn send_cloud_count_request(
    client: &reqwest::Client,
    http_parts: &HttpParts,
) -> Result<usize, String> {
    let mut request = client
        .post(&http_parts.url)
        .headers(http_parts.headers.clone())
        .header(ACCEPT, "application/json")
        .json(&http_parts.body)
        .timeout(CLOUD_TOKEN_COUNT_TIMEOUT);

    if !http_parts.headers.contains_key(CONTENT_TYPE) {
        request = request.header(CONTENT_TYPE, "application/json");
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("cloud token count request failed: {err}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| format!("cloud token count response read failed: {err}"))?;
    if !status.is_success() {
        return Err(format!(
            "cloud token count failed with status {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|err| format!("cloud token count response was not JSON: {err}"))?;
    response_input_tokens(&body)
        .ok_or_else(|| format!("cloud token count response missing input_tokens: {body}"))
}

pub async fn try_count_input_tokens(
    ctx: &ChatCtx,
    client: &reqwest::Client,
    req: &LlmRequest,
    model_rec: &BaseModelRecord,
) -> Option<CloudInputTokenCount> {
    let (parts, output_token_reserve) = match cloud_count_http_parts(ctx, req, model_rec) {
        Ok(Some(parts)) => parts,
        Ok(None) => return None,
        Err(err) => {
            debug!(model = %model_rec.id, error = %err, "cloud token count not available");
            return None;
        }
    };

    debug!(
        model = %model_rec.id,
        url = %parts.url,
        headers = ?header_names(&parts.headers),
        "cloud token count request"
    );

    match send_cloud_count_request(client, &parts).await {
        Ok(input_tokens) => Some(CloudInputTokenCount {
            usage: ChatUsage {
                prompt_tokens: input_tokens,
                completion_tokens: 0,
                total_tokens: input_tokens,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                metering_usd: None,
            },
            output_token_reserve,
        }),
        Err(err) => {
            warn!(model = %model_rec.id, error = %err, "cloud token count failed; falling back to local estimate/provider usage");
            None
        }
    }
}

pub fn cloud_input_exceeds_context(
    usage: &ChatUsage,
    context_limit: usize,
    output_token_reserve: usize,
) -> bool {
    context_limit > 0 && usage.prompt_tokens.saturating_add(output_token_reserve) > context_limit
}

pub fn cloud_context_limit_message(
    usage: &ChatUsage,
    model_rec: &BaseModelRecord,
    context_limit: usize,
    output_token_reserve: usize,
) -> String {
    format!(
        "Cloud token count for model '{}' is {} input tokens plus {} reserved output tokens, exceeding the configured context window of {} tokens",
        model_rec.id, usage.prompt_tokens, output_token_reserve, context_limit
    )
}
