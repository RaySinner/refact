use std::collections::HashSet;

pub const TRAJECTORY_META_TITLE_MAX_CHARS: usize = 120;
pub const TRAJECTORY_LABEL_MAX_CHARS: usize = TRAJECTORY_META_TITLE_MAX_CHARS;

pub fn truncate_text_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    text.chars().take(max_chars - 3).collect::<String>() + "..."
}

pub fn trajectory_meta_title(title: &str) -> String {
    let cleaned = title.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_text_chars(&cleaned, TRAJECTORY_META_TITLE_MAX_CHARS)
}

pub fn calculate_line_changes_from_messages(messages: &[serde_json::Value]) -> (i64, i64) {
    let mut total_added: i64 = 0;
    let mut total_removed: i64 = 0;

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "diff" {
            continue;
        }

        let content = match msg.get("content") {
            Some(serde_json::Value::String(s)) => s.as_str(),
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

pub fn calculate_task_progress_from_messages(messages: &[serde_json::Value]) -> (i32, i32, i32) {
    // Build a set of successful tool call IDs (tool messages without tool_failed=true)
    let mut successful_tool_ids: HashSet<String> = HashSet::new();
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "tool" {
            continue;
        }
        let tool_failed = msg
            .get("tool_failed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !tool_failed {
            if let Some(tool_call_id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                successful_tool_ids.insert(tool_call_id.to_string());
            }
        }
    }

    // Find the last successful tasks_set tool call (iterate in reverse)
    for msg in messages.iter().rev() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "assistant" {
            continue;
        }

        let tool_calls = match msg.get("tool_calls").and_then(|v| v.as_array()) {
            Some(tc) => tc,
            None => continue,
        };

        // Iterate tool_calls in reverse to find the last tasks_set
        for tc in tool_calls.iter().rev() {
            let function = match tc.get("function") {
                Some(f) => f,
                None => continue,
            };

            let name = function.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name != "tasks_set" {
                continue;
            }

            let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if tc_id.is_empty() || !successful_tool_ids.contains(tc_id) {
                continue;
            }

            // Parse the arguments
            let args_str = function
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("");
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_str) {
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

#[derive(Default, Debug, Clone, PartialEq)]
pub struct TokenTotals {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_usd: Option<f64>,
}

pub fn calculate_token_totals_from_messages(messages: &[serde_json::Value]) -> TokenTotals {
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;
    let mut total_tokens: u64 = 0;
    let mut cache_read_tokens: u64 = 0;
    let mut cache_creation_tokens: u64 = 0;
    let mut cost_usd: Option<f64> = None;

    for msg in messages {
        let usage = match msg.get("usage") {
            Some(u) if !u.is_null() => u,
            _ => continue,
        };
        if let Some(v) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
            prompt_tokens += v;
        }
        if let Some(v) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
            completion_tokens += v;
        }
        if let Some(v) = usage.get("total_tokens").and_then(|v| v.as_u64()) {
            total_tokens += v;
        }
        for key in &["cache_read_input_tokens", "cache_read_tokens"] {
            if let Some(v) = usage.get(key).and_then(|v| v.as_u64()) {
                cache_read_tokens += v;
                break;
            }
        }
        for key in &["cache_creation_input_tokens", "cache_creation_tokens"] {
            if let Some(v) = usage.get(key).and_then(|v| v.as_u64()) {
                cache_creation_tokens += v;
                break;
            }
        }
        if let Some(total) = usage
            .get("metering_usd")
            .and_then(|m| m.get("total_usd"))
            .and_then(|v| v.as_f64())
        {
            *cost_usd.get_or_insert(0.0) += total;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_trajectory_meta_title_truncation() {
        let short = "Short Title";
        assert_eq!(trajectory_meta_title(short), "Short Title");

        let long = "A".repeat(200);
        let result = trajectory_meta_title(&long);
        assert_eq!(result.chars().count(), TRAJECTORY_META_TITLE_MAX_CHARS);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_calculate_line_changes() {
        let messages = vec![json!({
            "role": "diff",
            "content": serde_json::to_string(&vec![
                json!({
                    "lines_add": "line1\nline2",
                    "lines_remove": "old_line"
                })
            ]).unwrap()
        })];
        let (added, removed) = calculate_line_changes_from_messages(&messages);
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
    }

    #[test]
    fn test_calculate_token_totals() {
        let messages = vec![json!({
            "role": "assistant",
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "cache_read_input_tokens": 3,
                "cache_creation_input_tokens": 2,
                "metering_usd": {
                    "total_usd": 0.003
                }
            }
        })];
        let totals = calculate_token_totals_from_messages(&messages);
        assert_eq!(totals.prompt_tokens, 10);
        assert_eq!(totals.completion_tokens, 5);
        assert_eq!(totals.total_tokens, 15);
        assert_eq!(totals.cache_read_tokens, 3);
        assert_eq!(totals.cache_creation_tokens, 2);
        assert_eq!(totals.cost_usd, Some(0.003));
    }
}
