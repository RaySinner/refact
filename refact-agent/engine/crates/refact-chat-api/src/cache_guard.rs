use std::sync::LazyLock;

use refact_caps_core::model_records::ChatModelRecord;
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use similar::{Algorithm, TextDiff};

use crate::internal_roles::{event, EventSubkind};
use crate::{ChatMessage, PauseReason, ThreadParams};

pub const CACHE_GUARD_TOOL_NAME: &str = "cache_guard";
pub const CACHE_GUARD_TOOL_CALL_ID: &str = "cacheguard_force";
pub const CACHE_GUARD_RULE: &str = "Prompt cache prefix violation";
pub const CACHE_GUARD_PREVIEW_STRING_MAX_CHARS: usize = 240;
pub const CACHE_GUARD_PREVIEW_ARRAY_MAX_ITEMS: usize = 24;
pub const CACHE_GUARD_DIFF_MAX_CHARS: usize = 32 * 1024;
pub const CACHE_GUARD_DIFF_TRUNCATION_NOTICE: &str = concat!(
    "\n...[cache guard diff truncated: omitted {omitted} chars; ",
    "sanitized provider request body preview exceeded limit]\n"
);
pub const CACHE_GUARD_DIFF_PREVIEW_NOTICE: &str = concat!(
    "\n[cache guard diff generated from bounded structural preview: ",
    "large arrays use count summaries and text payloads are redacted/truncated]\n"
);

pub const RECURSIVELY_IGNORED_KEYS: &[&str] = &[
    "cache_control",
    "request_attempt_id",
    "usage",
    "finish_reason",
    "checkpoints",
];

pub const TOP_LEVEL_IGNORED_KEYS: &[&str] = &[
    "temperature",
    "max_tokens",
    "max_completion_tokens",
    "max_output_tokens",
    "frequency_penalty",
    "presence_penalty",
    "stop",
    "stop_sequences",
    "n",
];

pub static CACHE_GUARD_REDACTION_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> =
    LazyLock::new(|| {
        vec![
            (
                Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._~+/=-]+")
                    .expect("cache guard bearer redaction regex must compile"),
                "Bearer [REDACTED]",
            ),
            (
                Regex::new(r"sk-[A-Za-z0-9_-]{6,}")
                    .expect("cache guard secret redaction regex must compile"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r#"(?i)([A-Za-z0-9_-]*api[_-]?key\s*[:=]\s*["']?)[^\s&"'`]+"#)
                    .expect("cache guard API key redaction regex must compile"),
                "${1}[REDACTED]",
            ),
        ]
    });

pub fn is_cache_guard_pause_id(tool_call_id: &str) -> bool {
    tool_call_id.starts_with("cacheguard_")
}

pub fn is_cache_guard_pause_reason(reason: &PauseReason) -> bool {
    reason.tool_name == CACHE_GUARD_TOOL_NAME || is_cache_guard_pause_id(&reason.tool_call_id)
}

pub fn cache_guard_event_message(payload: Value, summary: impl Into<String>) -> ChatMessage {
    event(
        EventSubkind::SystemNotice,
        "chat.cache_guard",
        payload,
        summary,
    )
}

pub fn is_task_management_role(role: &str) -> bool {
    matches!(
        role,
        "planner" | "agent" | "agents" | "task_agent" | "subchats"
    )
}

pub fn is_non_interactive_thread(thread: &ThreadParams) -> bool {
    if thread
        .task_meta
        .as_ref()
        .is_some_and(|meta| is_task_management_role(&meta.role))
    {
        return true;
    }

    if thread.buddy_meta.is_some() {
        return true;
    }

    thread
        .link_type
        .as_deref()
        .is_some_and(is_subagentic_link_type)
}

fn is_subagentic_link_type(link_type: &str) -> bool {
    !matches!(link_type, "handoff" | "mode_transition" | "branch")
}

pub fn chat_model_supports_cache_guard(record: &ChatModelRecord) -> bool {
    record.base.supports_cache_control
}

pub fn sanitize_body_for_cache_guard(value: &Value) -> Value {
    sanitize_value_with_parent(value, true, None)
}

pub fn body_for_cache_guard(value: &Value) -> Value {
    body_for_cache_guard_with_boundary(value, anthropic_cache_prefix_boundary(value))
}

pub fn body_for_cache_guard_with_boundary(
    value: &Value,
    boundary: Option<AnthropicCachePrefixBoundary>,
) -> Value {
    match boundary {
        Some(boundary) => sanitize_anthropic_cache_prefix(value, boundary),
        None => sanitize_body_for_cache_guard(value),
    }
}

pub fn is_append_only_prefix(prev: &Value, next: &Value) -> bool {
    is_append_only_prefix_inner(prev, next, None, true)
}

pub fn has_anthropic_explicit_cache_markers(value: &Value) -> bool {
    count_anthropic_cache_control_markers(value) > 0
}

fn count_anthropic_cache_control_markers(value: &Value) -> usize {
    count_cache_control_markers(value.get("system"))
        + count_cache_control_markers(value.get("messages"))
        + count_cache_control_markers(value.get("tools"))
}

fn count_cache_control_markers(value: Option<&Value>) -> usize {
    let Some(value) = value else {
        return 0;
    };
    match value {
        Value::Array(values) => values
            .iter()
            .map(|item| {
                item.get("cache_control").map_or(0, |_| 1)
                    + count_cache_control_markers(item.get("content"))
            })
            .sum(),
        _ => 0,
    }
}

#[derive(Clone, Copy)]
pub enum AnthropicCachePrefixBoundary {
    Messages {
        message_index: usize,
        content_index: usize,
    },
    System(usize),
    Tools(usize),
}

pub fn anthropic_cache_prefix_boundary(value: &Value) -> Option<AnthropicCachePrefixBoundary> {
    if let Some((message_index, content_index)) = value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| last_message_cache_marker_index(messages))
    {
        return Some(AnthropicCachePrefixBoundary::Messages {
            message_index,
            content_index,
        });
    }

    if let Some(index) = value
        .get("system")
        .and_then(Value::as_array)
        .and_then(|values| last_direct_cache_marker_index(values))
    {
        return Some(AnthropicCachePrefixBoundary::System(index));
    }

    value
        .get("tools")
        .and_then(Value::as_array)
        .and_then(|values| last_direct_cache_marker_index(values))
        .map(AnthropicCachePrefixBoundary::Tools)
}

fn sanitize_anthropic_cache_prefix(value: &Value, boundary: AnthropicCachePrefixBoundary) -> Value {
    let Some(object) = value.as_object() else {
        return sanitize_body_for_cache_guard(value);
    };

    let mut out = Map::new();
    for (key, value) in object {
        if is_ignored_key(key, true, None) {
            continue;
        }
        let sanitized = match boundary {
            AnthropicCachePrefixBoundary::Messages {
                message_index,
                content_index,
            } => {
                if key == "messages" {
                    Some(sanitize_messages_through_cache_marker(
                        value,
                        message_index,
                        content_index,
                    ))
                } else {
                    Some(sanitize_value_with_parent(value, false, Some(key)))
                }
            }
            AnthropicCachePrefixBoundary::System(index) => match key.as_str() {
                "messages" => None,
                "system" => Some(sanitize_array_through_cache_marker(value, index, "system")),
                _ => Some(sanitize_value_with_parent(value, false, Some(key))),
            },
            AnthropicCachePrefixBoundary::Tools(index) => match key.as_str() {
                "messages" | "system" => None,
                "tools" => Some(sanitize_array_through_cache_marker(value, index, "tools")),
                _ => Some(sanitize_value_with_parent(value, false, Some(key))),
            },
        };
        if let Some(sanitized) = sanitized {
            out.insert(key.clone(), sanitized);
        }
    }
    Value::Object(out)
}

fn sanitize_array_through_cache_marker(value: &Value, index: usize, parent_key: &str) -> Value {
    let Some(values) = value.as_array() else {
        return sanitize_value_with_parent(value, false, Some(parent_key));
    };
    let truncated = Value::Array(
        values
            .iter()
            .take(index + 1)
            .map(|value| sanitize_value_with_parent(value, false, Some(parent_key)))
            .collect(),
    );
    canonicalize_content_shape(truncated, Some(parent_key))
}

fn sanitize_messages_through_cache_marker(
    value: &Value,
    message_index: usize,
    content_index: usize,
) -> Value {
    let Some(messages) = value.as_array() else {
        return sanitize_value_with_parent(value, false, Some("messages"));
    };
    Value::Array(
        messages
            .iter()
            .enumerate()
            .take(message_index + 1)
            .map(|(index, message)| {
                if index == message_index {
                    sanitize_message_through_cache_marker(message, content_index)
                } else {
                    sanitize_value_with_parent(message, false, Some("messages"))
                }
            })
            .collect(),
    )
}

fn sanitize_message_through_cache_marker(value: &Value, content_index: usize) -> Value {
    let Some(message) = value.as_object() else {
        return sanitize_value_with_parent(value, false, Some("messages"));
    };
    let mut out = Map::new();
    for (key, value) in message {
        if is_ignored_key(key, false, None) {
            continue;
        }
        let sanitized = if key == "content" {
            sanitize_array_through_cache_marker(value, content_index, "content")
        } else {
            sanitize_value_with_parent(value, false, Some(key))
        };
        out.insert(key.clone(), sanitized);
    }
    Value::Object(out)
}

fn last_direct_cache_marker_index(values: &[Value]) -> Option<usize> {
    values
        .iter()
        .rposition(|value| value.get("cache_control").is_some())
}

fn last_message_cache_marker_index(messages: &[Value]) -> Option<(usize, usize)> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            let content_index = message
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| last_direct_cache_marker_index(content))?;
            Some((index, content_index))
        })
}

fn is_append_only_prefix_inner(
    prev: &Value,
    next: &Value,
    parent_key: Option<&str>,
    top_level: bool,
) -> bool {
    match (prev, next) {
        (Value::Null, Value::Null)
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_)) => prev == next,
        (Value::Array(a), Value::Array(b)) => {
            if matches!(parent_key, Some("messages" | "input")) {
                return a.len() <= b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(old_item, new_item)| old_item == new_item);
            }
            a == b
        }
        (Value::Object(a), Value::Object(b)) => {
            let a_keys = a
                .keys()
                .filter(|key| !is_ignored_key(key, top_level, parent_key))
                .count();
            let b_keys = b
                .keys()
                .filter(|key| !is_ignored_key(key, top_level, parent_key))
                .count();
            if a_keys != b_keys {
                return false;
            }
            a.iter()
                .filter(|(key, _)| !is_ignored_key(key, top_level, parent_key))
                .all(|(key, old_v)| {
                    b.get(key)
                        .map(|new_v| is_append_only_prefix_inner(old_v, new_v, Some(key), false))
                        .unwrap_or(false)
                })
        }
        _ => false,
    }
}

fn is_anthropic_cache_prefix_compatible(prev: &Value, next: &Value) -> bool {
    is_anthropic_cache_prefix_inner(prev, next, None, true, false)
}

fn is_anthropic_cache_prefix_inner(
    prev: &Value,
    next: &Value,
    parent_key: Option<&str>,
    top_level: bool,
    content_append_ok: bool,
) -> bool {
    match (prev, next) {
        (Value::Null, Value::Null)
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_)) => prev == next,
        (Value::Array(a), Value::Array(b)) => {
            if matches!(parent_key, Some("messages")) || content_append_ok {
                return a.len() <= b.len()
                    && a.iter().zip(b.iter()).all(|(old_item, new_item)| {
                        is_anthropic_cache_prefix_inner(old_item, new_item, None, false, false)
                    });
            }
            a == b
        }
        (Value::Object(a), Value::Object(b)) => {
            let a_keys = a
                .keys()
                .filter(|key| !is_ignored_key(key, top_level, parent_key))
                .count();
            let b_keys = b
                .keys()
                .filter(|key| !is_ignored_key(key, top_level, parent_key))
                .count();
            if a_keys != b_keys {
                return false;
            }
            a.iter()
                .filter(|(key, _)| !is_ignored_key(key, top_level, parent_key))
                .all(|(key, old_v)| {
                    let allow_content_append = key == "content" && a.get("role") == b.get("role");
                    b.get(key)
                        .map(|new_v| {
                            is_anthropic_cache_prefix_inner(
                                old_v,
                                new_v,
                                Some(key),
                                false,
                                allow_content_append,
                            )
                        })
                        .unwrap_or(false)
                })
        }
        _ => false,
    }
}

pub fn unified_json_diff(prev: &Value, next: &Value) -> String {
    let prev_pretty = serde_json::to_string_pretty(prev).unwrap_or_else(|_| prev.to_string());
    let next_pretty = serde_json::to_string_pretty(next).unwrap_or_else(|_| next.to_string());

    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_lines(&prev_pretty, &next_pretty);

    diff.unified_diff()
        .context_radius(6)
        .header("previous", "current")
        .to_string()
}

pub fn cap_cache_guard_diff(diff: &str) -> String {
    let char_count = diff.chars().count();
    if char_count <= CACHE_GUARD_DIFF_MAX_CHARS {
        return diff.to_string();
    }

    let kept: String = diff.chars().take(CACHE_GUARD_DIFF_MAX_CHARS).collect();
    let omitted = char_count - CACHE_GUARD_DIFF_MAX_CHARS;
    let notice = CACHE_GUARD_DIFF_TRUNCATION_NOTICE.replace("{omitted}", &omitted.to_string());
    format!("{kept}{notice}")
}

pub fn preview_body_for_cache_guard_diff(value: &Value) -> Value {
    preview_value_for_cache_guard_diff(value, None)
}

fn preview_value_for_cache_guard_diff(value: &Value, parent_key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                out.insert(
                    key.clone(),
                    preview_value_for_cache_guard_diff(value, Some(key.as_str())),
                );
            }
            Value::Object(out)
        }
        Value::Array(values) => {
            let omitted = values
                .len()
                .saturating_sub(CACHE_GUARD_PREVIEW_ARRAY_MAX_ITEMS);
            let mut preview_values: Vec<Value> = values
                .iter()
                .take(CACHE_GUARD_PREVIEW_ARRAY_MAX_ITEMS)
                .map(|value| preview_value_for_cache_guard_diff(value, parent_key))
                .collect();
            if omitted > 0 {
                preview_values.push(json_summary_cache_guard_preview(format!(
                    "[cache guard preview truncated: total_items={} retained_items={} omitted_items={}]",
                    values.len(),
                    values.len() - omitted,
                    omitted
                )));
            }
            Value::Array(preview_values)
        }
        Value::String(text) => {
            Value::String(redact_and_truncate_cache_guard_preview(text, parent_key))
        }
        scalar => scalar.clone(),
    }
}

fn json_summary_cache_guard_preview(summary: String) -> Value {
    let mut out = Map::new();
    out.insert(
        "__cache_guard_preview__".to_string(),
        Value::String(summary),
    );
    Value::Object(out)
}

fn redact_and_truncate_cache_guard_preview(text: &str, parent_key: Option<&str>) -> String {
    let redacted = if parent_key.is_some_and(is_sensitive_key) {
        "[REDACTED]".to_string()
    } else {
        redact_common_secret_patterns(text)
    };
    if parent_key.is_some_and(is_payload_text_key) {
        return fingerprint_cache_guard_payload(&redacted);
    }
    truncate_cache_guard_preview(&redacted)
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized.contains("auth_token")
        || normalized.contains("authorization")
        || normalized.contains("bearer")
        || normalized.contains("password")
        || normalized.contains("secret")
        || normalized == "token"
        || normalized.ends_with("_token")
}

fn is_payload_text_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "arguments"
            | "content"
            | "description"
            | "instructions"
            | "m_content"
            | "output"
            | "prompt"
            | "system"
            | "text"
    )
}

fn fingerprint_cache_guard_payload(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let digest_hex = hex::encode(digest);
    format!(
        "[redacted text chars={} sha256={}]",
        text.chars().count(),
        &digest_hex[..16]
    )
}

fn redact_common_secret_patterns(text: &str) -> String {
    let mut redacted = text.to_string();
    for (regex, replacement) in CACHE_GUARD_REDACTION_PATTERNS.iter() {
        redacted = regex.replace_all(&redacted, *replacement).to_string();
    }
    redacted
}

fn truncate_cache_guard_preview(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= CACHE_GUARD_PREVIEW_STRING_MAX_CHARS {
        return text.to_string();
    }
    let kept: String = text
        .chars()
        .take(CACHE_GUARD_PREVIEW_STRING_MAX_CHARS)
        .collect();
    format!(
        "{kept}…[truncated {} chars]",
        char_count - CACHE_GUARD_PREVIEW_STRING_MAX_CHARS
    )
}

pub fn cache_prefix_compatible(prev: &Value, next: &Value, has_explicit_markers: bool) -> bool {
    is_append_only_prefix(prev, next)
        || has_explicit_markers && is_anthropic_cache_prefix_compatible(prev, next)
}

fn is_ignored_key(key: &str, top_level: bool, parent_key: Option<&str>) -> bool {
    RECURSIVELY_IGNORED_KEYS.contains(&key)
        || (matches!(key, "started_at_ms" | "completed_at_ms") && parent_key == Some("tool_calls"))
        || top_level && TOP_LEVEL_IGNORED_KEYS.contains(&key)
}

fn sanitize_value_with_parent(value: &Value, top_level: bool, parent_key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                if is_ignored_key(key, top_level, parent_key) {
                    continue;
                }
                out.insert(
                    key.clone(),
                    sanitize_value_with_parent(value, false, Some(key)),
                );
            }
            Value::Object(out)
        }
        Value::Array(arr) => canonicalize_content_shape(
            Value::Array(
                arr.iter()
                    .map(|value| sanitize_value_with_parent(value, false, parent_key))
                    .collect(),
            ),
            parent_key,
        ),
        _ => value.clone(),
    }
}

fn canonicalize_content_shape(value: Value, parent_key: Option<&str>) -> Value {
    if parent_key != Some("content") {
        return value;
    }
    let Value::Array(items) = &value else {
        return value;
    };
    let [Value::Object(block)] = items.as_slice() else {
        return value;
    };
    if block.len() != 2 || block.get("type").and_then(Value::as_str) != Some("text") {
        return value;
    }
    match block.get("text") {
        Some(Value::String(text)) => Value::String(text.clone()),
        _ => value,
    }
}

#[cfg(test)]
fn legacy_body_for_cache_guard(value: &Value) -> Value {
    let mut sanitized = sanitize_body_for_cache_guard(value);
    if let Some((message_index, content_index)) = value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| last_message_cache_marker_index(messages))
    {
        truncate_messages_at(&mut sanitized, message_index, content_index);
        return sanitized;
    }
    if let Some(index) = value
        .get("system")
        .and_then(Value::as_array)
        .and_then(|values| last_direct_cache_marker_index(values))
    {
        truncate_top_level_array_at(&mut sanitized, "system", index);
        remove_top_level_key(&mut sanitized, "messages");
        return sanitized;
    }
    if let Some(index) = value
        .get("tools")
        .and_then(Value::as_array)
        .and_then(|values| last_direct_cache_marker_index(values))
    {
        truncate_top_level_array_at(&mut sanitized, "tools", index);
        remove_top_level_key(&mut sanitized, "system");
        remove_top_level_key(&mut sanitized, "messages");
    }
    sanitized
}

#[cfg(test)]
fn truncate_top_level_array_at(sanitized: &mut Value, key: &str, index: usize) {
    if let Some(values) = sanitized.get_mut(key).and_then(Value::as_array_mut) {
        values.truncate(index + 1);
    }
}

#[cfg(test)]
fn truncate_messages_at(sanitized: &mut Value, message_index: usize, content_index: usize) {
    let Some(messages) = sanitized.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    messages.truncate(message_index + 1);
    let Some(content) = messages
        .get_mut(message_index)
        .and_then(|message| message.get_mut("content"))
    else {
        return;
    };
    if let Some(items) = content.as_array_mut() {
        items.truncate(content_index + 1);
    }
    *content = canonicalize_content_shape(content.take(), Some("content"));
}

#[cfg(test)]
fn remove_top_level_key(sanitized: &mut Value, key: &str) {
    if let Some(object) = sanitized.as_object_mut() {
        object.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::llm_types::BaseModelRecord;
    use serde_json::json;

    #[test]
    fn test_sanitize_removes_ignored_fields_recursively() {
        let input = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}},
                    {"type": "text", "text": "world"}
                ]},
                {"role": "assistant", "content": "ok", "provider_specific_fields": {"x": 1, "cache_control": {"type": "ephemeral"}}}
            ],
            "temperature": 0.3,
            "max_tokens": 1024,
            "meta": {"request_attempt_id": "abc", "chat_id": "x"},
            "reasoning_effort": "high"
        });
        let out = sanitize_body_for_cache_guard(&input);
        assert!(out.get("temperature").is_none());
        assert!(out.get("max_tokens").is_none());
        assert_eq!(out["reasoning_effort"], "high");
        assert!(out["meta"].get("request_attempt_id").is_none());
        assert_eq!(out["meta"]["chat_id"], "x");

        let first_content = out["messages"][0]["content"].as_array().unwrap();
        assert!(first_content[0].get("cache_control").is_none());
        assert_eq!(out["messages"][1]["provider_specific_fields"]["x"], 1);
        assert!(out["messages"][1]["provider_specific_fields"]
            .get("cache_control")
            .is_none());
    }

    #[test]
    fn tool_call_timing_metadata_is_ignored_recursively() {
        let prev = sanitize_body_for_cache_guard(&json!({
            "messages": [{
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "cat", "arguments": "{}"}
                }]
            }]
        }));
        let next = sanitize_body_for_cache_guard(&json!({
            "messages": [{
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "cat", "arguments": "{}"},
                    "started_at_ms": 1_700_000_000_000u64,
                    "completed_at_ms": 1_700_000_000_123u64
                }]
            }]
        }));

        assert!(is_append_only_prefix(&prev, &next));
        assert!(next["messages"][0]["tool_calls"][0]
            .get("started_at_ms")
            .is_none());
        assert!(next["messages"][0]["tool_calls"][0]
            .get("completed_at_ms")
            .is_none());
    }

    #[test]
    fn tool_schema_timing_fields_remain_semantic() {
        let prev = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "name": "schedule",
                "input_schema": {"properties": {"started_at_ms": {"type": "integer"}}}
            }]
        }));
        let next = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "name": "schedule",
                "input_schema": {"properties": {"started_at_ms": {"type": "string"}}}
            }]
        }));

        assert!(next["tools"][0]["input_schema"]["properties"]
            .get("started_at_ms")
            .is_some());
        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_top_level_generation_options_are_ignored() {
        let prev = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.1,
            "max_tokens": 1024,
            "stop": ["old"],
            "presence_penalty": 0.0
        }));
        let next = sanitize_body_for_cache_guard(&json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"}
            ],
            "temperature": 0.9,
            "max_tokens": 2048,
            "stop": ["new"],
            "presence_penalty": 1.0
        }));

        assert!(prev.get("temperature").is_none());
        assert!(prev.get("max_tokens").is_none());
        assert!(prev.get("stop").is_none());
        assert!(prev.get("presence_penalty").is_none());
        assert!(is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_nested_generation_named_keys_remain_semantic() {
        let prev = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "configure",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "max_tokens": {"type": "integer"},
                            "stop": {"type": "string"}
                        }
                    }
                }
            }],
            "provider_specific_fields": {
                "generation_limits": {"max_tokens": 100, "stop": "END"}
            }
        }));
        let next = sanitize_body_for_cache_guard(&json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "configure",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "max_tokens": {"type": "string"},
                            "stop": {"type": "boolean"}
                        }
                    }
                }
            }],
            "provider_specific_fields": {
                "generation_limits": {"max_tokens": 200, "stop": "DONE"}
            }
        }));

        assert!(prev["tools"][0]["function"]["parameters"]["properties"]
            .get("max_tokens")
            .is_some());
        assert!(prev["tools"][0]["function"]["parameters"]["properties"]
            .get("stop")
            .is_some());
        assert_eq!(
            prev["provider_specific_fields"]["generation_limits"]["max_tokens"],
            100
        );
        assert_eq!(
            prev["provider_specific_fields"]["generation_limits"]["stop"],
            "END"
        );
        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_provider_specific_fields_semantic_mutation_breaks_prefix() {
        let prev = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "hi"}],
            "provider_specific_fields": {
                "reasoning": {"effort": "medium"},
                "cache_control": {"type": "ephemeral"}
            }
        }));
        let next = sanitize_body_for_cache_guard(&json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"}
            ],
            "provider_specific_fields": {
                "reasoning": {"effort": "high"},
                "cache_control": {"type": "ephemeral"}
            }
        }));

        assert!(prev["provider_specific_fields"]
            .get("cache_control")
            .is_none());
        assert!(!is_append_only_prefix(&prev, &next));
    }

    fn drifting_cache_marker_body(marked_assistant: bool) -> Value {
        let cc = json!({"type": "ephemeral", "ttl": "1h"});
        let assistant_content = if marked_assistant {
            json!([{"type": "text", "text": "", "cache_control": cc}])
        } else {
            json!("")
        };
        let tool_content = if marked_assistant {
            json!("old output")
        } else {
            json!([{"type": "text", "text": "old output", "cache_control": cc}])
        };
        json!({
            "model": "test",
            "messages": [
                {"role": "system", "content": [{"type": "text", "text": "sys", "cache_control": cc}]},
                {"role": "user", "content": "start"},
                {
                    "role": "assistant",
                    "content": assistant_content,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "cat", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "content": tool_content, "tool_call_id": "call_1"}
            ]
        })
    }

    #[test]
    fn drifting_cache_markers_do_not_break_the_prefix() {
        let prev = sanitize_body_for_cache_guard(&drifting_cache_marker_body(true));
        let mut next_body = drifting_cache_marker_body(false);
        next_body["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "assistant", "content": "next turn"}));
        let next = sanitize_body_for_cache_guard(&next_body);

        assert_eq!(prev["messages"][2]["content"], json!(""));
        assert_eq!(prev["messages"][3]["content"], json!("old output"));
        assert!(is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn drifting_cache_markers_survive_anthropic_boundary_truncation() {
        let prev = body_for_cache_guard(&json!({
            "model": "test",
            "messages": [
                {"role": "user", "content": "start"},
                {
                    "role": "tool",
                    "content": [{"type": "text", "text": "output", "cache_control": {"type": "ephemeral"}}],
                    "tool_call_id": "call_1"
                }
            ]
        }));
        let next = body_for_cache_guard(&json!({
            "model": "test",
            "messages": [
                {"role": "user", "content": "start"},
                {"role": "tool", "content": "output", "tool_call_id": "call_1"},
                {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "later", "cache_control": {"type": "ephemeral"}}]
                }
            ]
        }));

        assert_eq!(prev["messages"][1]["content"], json!("output"));
        assert!(is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn canonicalized_content_still_detects_real_text_changes() {
        let prev = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "tool", "content": "old output", "tool_call_id": "call_1"}]
        }));
        let next = sanitize_body_for_cache_guard(&json!({
            "messages": [{
                "role": "tool",
                "content": [{"type": "text", "text": "new output", "cache_control": {"type": "ephemeral"}}],
                "tool_call_id": "call_1"
            }]
        }));

        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn canonicalization_only_collapses_a_lone_plain_text_block() {
        let cases = [
            json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]),
            json!([{"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]),
            json!([{"type": "text", "text": "a", "extra": true}]),
            json!([{"type": "tool_result", "tool_use_id": "call_1", "content": "ok"}]),
        ];
        for content in cases {
            let sanitized = sanitize_body_for_cache_guard(&json!({
                "messages": [{"role": "user", "content": content.clone()}]
            }));
            assert!(
                sanitized["messages"][0]["content"].is_array(),
                "content must stay an array: {content}"
            );
        }

        let collapsed = sanitize_body_for_cache_guard(&json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "a", "cache_control": {"type": "ephemeral"}}]
            }]
        }));
        assert_eq!(collapsed["messages"][0]["content"], json!("a"));
    }

    #[test]
    fn test_append_only_prefix_objects_and_arrays() {
        let prev = json!({"messages": [1, 2], "meta": {"a": 1}});
        let next = json!({"messages": [1, 2, 3], "meta": {"a": 1}});
        assert!(is_append_only_prefix(&prev, &next));

        let bad_message = json!({"messages": [1, 99, 3], "meta": {"a": 1}});
        assert!(!is_append_only_prefix(&prev, &bad_message));

        let bad_meta = json!({"messages": [1, 2, 3], "meta": {"a": 1, "b": 2}});
        assert!(!is_append_only_prefix(&prev, &bad_meta));
    }

    #[test]
    fn test_append_only_prefix_strict_top_level_keys() {
        let prev = json!({"messages": [1, 2], "meta": {"a": 1}});
        let next = json!({"messages": [1, 2, 3], "meta": {"a": 1}, "extra": true});
        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_tools_array_strict_equality() {
        let tool_a =
            json!({"type": "function", "function": {"name": "tool_a", "description": "A"}});
        let tool_b =
            json!({"type": "function", "function": {"name": "tool_b", "description": "B"}});

        // Identical tools → OK
        let prev = json!({"messages": [1], "tools": [tool_a.clone()]});
        let next = json!({"messages": [1, 2], "tools": [tool_a.clone()]});
        assert!(is_append_only_prefix(&prev, &next));

        // New tool appended mid-session → violation (breaks LLM cache prefix)
        let next_extra = json!({"messages": [1, 2], "tools": [tool_a.clone(), tool_b.clone()]});
        assert!(!is_append_only_prefix(&prev, &next_extra));

        // Tool removed mid-session → violation
        let prev2 = json!({"messages": [1], "tools": [tool_a.clone(), tool_b.clone()]});
        let next_removed = json!({"messages": [1, 2], "tools": [tool_a.clone()]});
        assert!(!is_append_only_prefix(&prev2, &next_removed));

        // Tool description changed mid-session → violation
        let tool_a_changed =
            json!({"type": "function", "function": {"name": "tool_a", "description": "Changed"}});
        let next_changed = json!({"messages": [1, 2], "tools": [tool_a_changed]});
        assert!(!is_append_only_prefix(&prev, &next_changed));
    }

    #[test]
    fn test_non_conversation_arrays_are_strict() {
        let prev = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "system": [{"type": "text", "text": "stable"}]
        });
        let appended = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"}
            ],
            "system": [
                {"type": "text", "text": "stable"},
                {"type": "text", "text": "new prefix"}
            ]
        });
        let nested_key = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "system": [{"type": "text", "text": "stable", "name": "new"}]
        });

        assert!(!is_append_only_prefix(&prev, &appended));
        assert!(!is_append_only_prefix(&prev, &nested_key));
    }

    #[test]
    fn test_non_conversation_scalars_are_strict() {
        let prev = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "instructions": "stable prefix"
        });
        let next = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"}
            ],
            "instructions": "changed prefix"
        });

        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_append_only_prefix_messages_keys_strict() {
        let prev = json!({
            "messages": [
                {"role": "user", "content": "hi"}
            ]
        });
        let next = json!({
            "messages": [
                {"role": "user", "content": "hi", "extra": true}
            ]
        });
        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn detects_anthropic_explicit_cache_markers() {
        let body = json!({
            "system": [{"type": "text", "text": "stable", "cache_control": {"type": "ephemeral"}}],
            "tools": [{"name": "search", "input_schema": {"type": "object"}}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
        });

        assert!(has_anthropic_explicit_cache_markers(&body));
        assert_eq!(count_anthropic_cache_control_markers(&body), 1);
    }

    #[test]
    fn counts_nested_anthropic_message_cache_markers() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "ok"},
                    {"type": "text", "text": "continue", "cache_control": {"type": "ephemeral"}}
                ]
            }]
        });

        assert!(has_anthropic_explicit_cache_markers(&body));
        assert_eq!(count_anthropic_cache_control_markers(&body), 1);
    }

    #[test]
    fn anthropic_cache_guard_body_keeps_only_explicit_cache_prefix() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "cached", "cache_control": {"type": "ephemeral"}}]},
                {"role": "assistant", "content": [{"type": "text", "text": "uncached suffix"}]}
            ]
        });

        let sanitized = body_for_cache_guard(&body);

        assert_eq!(sanitized["messages"].as_array().unwrap().len(), 1);
        assert_eq!(sanitized["messages"][0]["role"], "user");
        assert!(sanitized.to_string().contains("cached"));
        assert!(!sanitized.to_string().contains("uncached suffix"));
        assert!(!sanitized.to_string().contains("cache_control"));
    }

    #[test]
    fn marker_first_sanitization_matches_legacy_prefix_truncation() {
        let message_marker_body = json!({
            "model": "test",
            "temperature": 0.7,
            "system": [{"type": "text", "text": "system"}],
            "tools": [{"name": "cat", "input_schema": {"type": "object"}}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "cached", "cache_control": {"type": "ephemeral"}},
                    {"type": "text", "text": "uncached"}
                ]},
                {"role": "assistant", "content": "suffix"}
            ]
        });
        let system_marker_body = json!({
            "system": [
                {"type": "text", "text": "cached", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "suffix"}
            ],
            "messages": [{"role": "user", "content": "suffix"}]
        });
        let tools_marker_body = json!({
            "system": [{"type": "text", "text": "suffix"}],
            "tools": [
                {"name": "cat", "cache_control": {"type": "ephemeral"}},
                {"name": "write"}
            ],
            "messages": [{"role": "user", "content": "suffix"}]
        });

        for body in [message_marker_body, system_marker_body, tools_marker_body] {
            assert_eq!(
                body_for_cache_guard(&body),
                legacy_body_for_cache_guard(&body)
            );
        }
    }

    #[test]
    fn explicit_marker_without_truncation_boundary_still_enables_guard() {
        let body = json!({
            "messages": [{
                "role": "user",
                "cache_control": {"type": "ephemeral"},
                "content": "stable"
            }]
        });

        assert!(has_anthropic_explicit_cache_markers(&body));
        assert!(anthropic_cache_prefix_boundary(&body).is_none());
        assert_eq!(
            body_for_cache_guard(&body),
            sanitize_body_for_cache_guard(&body)
        );
    }

    #[test]
    fn comparable_anthropic_prefix_allows_marker_moving_forward() {
        let prev = sanitize_body_for_cache_guard(&json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}]
            }]
        }));
        let next = sanitize_body_for_cache_guard(&json!({
            "messages": [
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "hello"}]
                },
                {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "changed suffix"}]
                },
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "continue", "cache_control": {"type": "ephemeral"}}]
                }
            ]
        }));

        assert!(is_anthropic_cache_prefix_compatible(&prev, &next));
    }

    #[test]
    fn comparable_anthropic_prefix_allows_completed_tool_event_turn_before_new_marker() {
        let prev = sanitize_body_for_cache_guard(&json!({
            "messages": [
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "start"}]
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "think", "signature": "sig"},
                        {"type": "text", "text": "calling shell"},
                        {"type": "tool_use", "id": "toolu_1", "name": "t_shell", "input": {"command": "echo hi"}}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "old output"},
                        {"type": "text", "text": "old event", "cache_control": {"type": "ephemeral"}}
                    ]
                }
            ]
        }));
        let next = sanitize_body_for_cache_guard(&json!({
            "messages": [
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "start"}]
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "think", "signature": "sig"},
                        {"type": "text", "text": "calling shell"},
                        {"type": "tool_use", "id": "toolu_1", "name": "t_shell", "input": {"command": "echo hi"}}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "old output"},
                        {"type": "text", "text": "old event"}
                    ]
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "next think", "signature": "sig2"},
                        {"type": "text", "text": "next call"},
                        {"type": "tool_use", "id": "toolu_2", "name": "t_shell", "input": {"command": "echo next"}}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_2", "content": "next output"},
                        {"type": "text", "text": "latest user text", "cache_control": {"type": "ephemeral"}}
                    ]
                }
            ]
        }));

        assert!(is_anthropic_cache_prefix_compatible(&prev, &next));
    }

    #[test]
    fn test_append_only_prefix_input_keys_strict() {
        let prev = json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        let next = json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}], "extra": true}
            ]
        });
        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_append_only_prefix_messages_nested_content_keys_strict() {
        let prev = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]}
            ]
        });
        let next = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi", "extra": true}]}
            ]
        });
        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_append_only_prefix_input_nested_content_keys_strict() {
        let prev = json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        let next = json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi", "extra": true}]}
            ]
        });
        assert!(!is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_append_only_prefix_allows_appended_messages_and_input_items() {
        let prev = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]}
            ],
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        let next = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
            ],
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]}
            ]
        });
        assert!(is_append_only_prefix(&prev, &next));
    }

    #[test]
    fn test_cache_guard_pause_reason_detection() {
        let reason = PauseReason {
            reason_type: "confirmation".to_string(),
            tool_name: "cache_guard".to_string(),
            command: String::new(),
            rule: String::new(),
            tool_call_id: "cacheguard_force".to_string(),
            integr_config_path: None,
        };
        assert!(is_cache_guard_pause_reason(&reason));
        assert!(is_cache_guard_pause_id("cacheguard_force_once"));
        assert!(!is_cache_guard_pause_id("call_123"));
    }

    #[test]
    fn cache_guard_diff_preview_redacts_secrets_and_truncates_large_strings() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": format!("Bearer sk-secret-123 api_key=abcd {}", "x".repeat(400))
            }],
            "metadata": {
                "auth_token": "raw-token-value",
                "safe_preview": "x".repeat(400)
            }
        });

        let preview = preview_body_for_cache_guard_diff(&body);
        let preview_text = serde_json::to_string(&preview).unwrap();

        assert!(!preview_text.contains("sk-secret-123"));
        assert!(!preview_text.contains("api_key=abcd"));
        assert!(!preview_text.contains("raw-token-value"));
        assert!(preview_text.contains("[redacted text chars="));
        assert!(preview_text.contains("sha256="));
        assert!(preview_text.contains("[truncated"));
    }

    #[test]
    fn cache_guard_redaction_patterns_are_cached_and_preserve_output() {
        assert_eq!(
            redact_common_secret_patterns("Bearer abc.def sk-secret-123 api_key=abcd"),
            "Bearer [REDACTED] [REDACTED_SECRET] api_key=[REDACTED]"
        );
    }

    #[test]
    fn cache_guard_diff_preview_omitted_arrays_use_count_summary() {
        let secret_tail = (0..64)
            .map(|i| json!({"api_key": format!("sk-omitted-secret-{i}")}))
            .collect::<Vec<_>>();
        let raw_digest = {
            let digest = Sha256::digest(serde_json::to_vec(&secret_tail).unwrap());
            let digest_hex = hex::encode(digest);
            digest_hex[..16].to_string()
        };
        let mut values = vec![json!({"safe": "kept"}); CACHE_GUARD_PREVIEW_ARRAY_MAX_ITEMS];
        values.extend(secret_tail);
        let body = json!({"metadata": values});

        let preview = preview_body_for_cache_guard_diff(&body);
        let preview_text = serde_json::to_string(&preview).unwrap();

        assert!(preview_text.contains("total_items=88"));
        assert!(preview_text.contains("retained_items=24"));
        assert!(preview_text.contains("omitted_items=64"));
        assert!(!preview_text.contains("omitted_sha256"));
        assert!(!preview_text.contains(&raw_digest));
        assert!(!preview_text.contains("sk-omitted-secret"));
    }

    #[test]
    fn cache_guard_diff_preview_huge_omitted_arrays_are_bounded() {
        let mut values = vec![json!({"safe": "kept"}); CACHE_GUARD_PREVIEW_ARRAY_MAX_ITEMS];
        values.extend((0..10_000).map(
            |i| json!({"secret": format!("sk-huge-omitted-secret-{i}-{}", "x".repeat(1024))}),
        ));
        let body = json!({"metadata": values});
        let started = std::time::Instant::now();

        let preview = preview_body_for_cache_guard_diff(&body);
        let elapsed = started.elapsed();
        let preview_text = serde_json::to_string(&preview).unwrap();

        assert!(elapsed < std::time::Duration::from_secs(2));
        assert!(preview_text.contains("total_items=10024"));
        assert!(preview_text.contains("omitted_items=10000"));
        assert!(!preview_text.contains("sk-huge-omitted-secret"));
        assert!(preview_text.len() < 2_000);
    }

    #[test]
    fn cache_guard_emits_event_not_user_message() {
        let message = cache_guard_event_message(
            json!({"model": "test/model", "reason": "prefix_changed"}),
            "cache guard probe",
        );

        assert_eq!(message.role, "event");
        assert_ne!(message.role, "user");
        let event = message.extra.get("event").unwrap();
        assert_eq!(event["subkind"], "system_notice");
        assert_eq!(event["source"], "chat.cache_guard");
        assert_eq!(event["payload"]["reason"], "prefix_changed");
        assert_eq!(message.content.content_text_only(), "cache guard probe");
    }

    #[test]
    fn cache_guard_disabled_for_cache_priced_model_without_cache_support() {
        let record = ChatModelRecord {
            base: BaseModelRecord {
                supports_cache_control: false,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(
            !chat_model_supports_cache_guard(&record),
            "cache pricing metadata must not enable cache guard without model cache support"
        );
    }
}
