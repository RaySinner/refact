//! Universal repeated-tool-call guard.
//!
//! Models occasionally get stuck calling the same tool with the same arguments
//! and receive byte-identical output — polling `process_list` forever, re-reading
//! an unchanged file line by line, calling `process_read` instead of
//! `process_wait`. This module holds the *decision primitive* for breaking that
//! loop. It is deliberately tool-agnostic: no per-tool signatures, no per-file
//! maps, one fingerprint chain.
//!
//! The whole trick is that a repeat only counts when the call **and** its result
//! are identical. A changed result digest means the agent made real progress (the
//! file was edited, the process advanced, the cursor moved), so the counter
//! resets and the call is served normally.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use crate::call_validation::{ChatContent, ChatMessage, ContextFile};

/// What the guard decided about one observed tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopVerdict {
    /// First call in a chain, OR the previous call returned different output.
    FirstCall,
    /// Nth identical repeat, N <= threshold. Serve content AND warn.
    Warn { consecutive: u8 },
    /// Identical repeat beyond threshold. Block.
    Block { consecutive: u8 },
}

/// Per-chat loop detector. State is a single fingerprint chain, not a map.
#[derive(Debug, Clone, Default)]
pub struct ToolCallLoopGuard {
    last_key: Option<String>,
    last_result_digest: Option<u64>,
    consecutive: u8,
}

impl ToolCallLoopGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one completed tool call (its identity key and its result digest) and
    /// learn whether it is a fresh call, a warnable repeat, or a blocked loop.
    pub fn observe(&mut self, key: &str, result_digest: u64, threshold: usize) -> LoopVerdict {
        let effective_threshold = threshold.max(1);
        match self.last_key.as_deref() {
            // Same call, but the tool produced different output: real progress.
            Some(previous_key)
                if previous_key == key && self.last_result_digest == Some(result_digest) =>
            {
                self.consecutive = self.consecutive.saturating_add(1);
                let consecutive = self.consecutive;
                if (consecutive as usize) <= effective_threshold {
                    LoopVerdict::Warn { consecutive }
                } else {
                    LoopVerdict::Block { consecutive }
                }
            }
            _ => {
                self.last_key = Some(key.to_string());
                self.last_result_digest = Some(result_digest);
                self.consecutive = 1;
                LoopVerdict::FirstCall
            }
        }
    }

    /// Forget the chain. The next call is a `FirstCall`.
    pub fn clear(&mut self) {
        self.last_key = None;
        self.last_result_digest = None;
        self.consecutive = 0;
    }

    pub fn consecutive(&self) -> u8 {
        self.consecutive
    }

    pub fn last_key(&self) -> Option<&str> {
        self.last_key.as_deref()
    }
}

/// Identity fingerprint: canonical tool name + normalized arguments.
///
/// Callers may override the key for tools whose "same call" is not argument
/// equality (e.g. `cat` must ignore line ranges).
pub fn call_identity_key(canonical_tool_name: &str, args_json: &str) -> String {
    let normalized = normalize_args_json(args_json);
    format!("{canonical_tool_name}\u{1}{normalized}")
}

/// Re-serialize the arguments so key order and whitespace do not make two
/// semantically identical calls look different. Falls back to the raw string
/// when the payload is not a JSON object/array or does not parse at all.
///
/// Keys are sorted explicitly rather than relying on `serde_json::to_string`:
/// this workspace enables serde_json's `preserve_order` feature (its `Map` is
/// an insertion-ordered index map), so re-serialization alone preserves the
/// order the model happened to emit. Array order IS significant and is kept.
fn normalize_args_json(args_json: &str) -> String {
    let trimmed = args_json.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value @ serde_json::Value::Object(_)) | Ok(value @ serde_json::Value::Array(_)) => {
            canonical_json(&value)
        }
        _ => trimmed.to_string(),
    }
}

/// Deterministic JSON rendering with recursively sorted object keys.
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(fields) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = fields.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let rendered: Vec<String> = entries
                .into_iter()
                .map(|(key, field)| {
                    let key_json = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string());
                    format!("{key_json}:{}", canonical_json(field))
                })
                .collect();
            format!("{{{}}}", rendered.join(","))
        }
        serde_json::Value::Array(items) => {
            let rendered: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", rendered.join(","))
        }
        scalar => scalar.to_string(),
    }
}

/// Deterministic digest of a tool result. Hashes at most the first 64 KiB so a
/// huge result cannot make the guard itself expensive.
pub const RESULT_DIGEST_BYTE_BUDGET: usize = 64 * 1024;

/// Streaming hasher that stops absorbing input once the byte budget is spent.
struct BoundedDigest {
    hasher: DefaultHasher,
    remaining: usize,
}

impl BoundedDigest {
    fn new() -> Self {
        Self {
            hasher: DefaultHasher::new(),
            remaining: RESULT_DIGEST_BYTE_BUDGET,
        }
    }

    fn feed(&mut self, value: &str) {
        if self.remaining == 0 {
            return;
        }
        let bytes = value.as_bytes();
        let take = bytes.len().min(self.remaining);
        self.hasher.write(&bytes[..take]);
        self.remaining -= take;
    }

    fn finish(self) -> u64 {
        self.hasher.finish()
    }
}

/// Digest of a tool result. Cheap enough to run on every single tool call and
/// never panics: every arm falls back to a stable string, never an `unwrap`.
pub fn result_digest(messages: &[ChatMessage], context_files: &[ContextFile]) -> u64 {
    let mut digest = BoundedDigest::new();
    for message in messages {
        digest.feed(&message.role);
        match &message.content {
            ChatContent::SimpleText(text) => digest.feed(text),
            ChatContent::Multimodal(elements) => {
                for element in elements {
                    digest.feed(&element.m_type);
                    digest.feed(&element.m_content);
                }
            }
            ChatContent::ContextFiles(files) => {
                for file in files {
                    digest.feed(&file.file_name);
                    digest.feed(&file.file_content);
                }
            }
        }
        if let Some(tool_failed) = message.tool_failed {
            digest.feed(if tool_failed { "1" } else { "0" });
        }
    }
    for file in context_files {
        digest.feed(&file.file_name);
        digest.feed(&file.file_content);
    }
    digest.finish()
}

pub fn loop_warning_text(tool: &str, consecutive: u8, threshold: usize) -> String {
    format!(
        "Warning: `{tool}` returned identical output on {consecutive} consecutive identical calls \
(loop threshold {threshold}). If this call made no progress, change your approach: vary the \
arguments, use a different tool, or continue with the task instead of repeating it."
    )
}

pub fn loop_block_text(tool: &str, consecutive: u8, threshold: usize) -> String {
    format!(
        "Blocked: `{tool}` was called with identical arguments {consecutive} times in a row and \
returned identical output every time (loop threshold {threshold}). The result is withheld \
because nothing has changed. Do not call `{tool}` again with the same arguments — vary the \
arguments, use a different tool, or continue with the task."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: usize = 3;

    fn simple(text: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn different_key_is_a_first_call() {
        let mut guard = ToolCallLoopGuard::new();
        assert_eq!(guard.observe("cat:a", 1, THRESHOLD), LoopVerdict::FirstCall);
        assert_eq!(guard.consecutive(), 1);
        assert_eq!(
            guard.observe("cat:b", 1, THRESHOLD),
            LoopVerdict::FirstCall
        );
        assert_eq!(guard.consecutive(), 1);
        assert_eq!(guard.last_key(), Some("cat:b"));
    }

    #[test]
    fn identical_key_and_digest_warns_then_blocks() {
        let mut guard = ToolCallLoopGuard::new();
        assert_eq!(guard.observe("k", 7, THRESHOLD), LoopVerdict::FirstCall);
        assert_eq!(
            guard.observe("k", 7, THRESHOLD),
            LoopVerdict::Warn { consecutive: 2 }
        );
        assert_eq!(guard.consecutive(), 2);
        assert_eq!(
            guard.observe("k", 7, THRESHOLD),
            LoopVerdict::Warn { consecutive: 3 }
        );
        assert_eq!(
            guard.observe("k", 7, THRESHOLD),
            LoopVerdict::Block { consecutive: 4 }
        );
    }

    #[test]
    fn changed_digest_resets_the_counter() {
        let mut guard = ToolCallLoopGuard::new();
        guard.observe("k", 7, THRESHOLD);
        assert_eq!(guard.observe("k", 7, THRESHOLD), LoopVerdict::Warn { consecutive: 2 });
        // Same call, different output => the agent edited the file / the
        // process advanced. That is progress, not a loop.
        assert_eq!(guard.observe("k", 8, THRESHOLD), LoopVerdict::FirstCall);
        assert_eq!(guard.consecutive(), 1);
        assert_eq!(guard.observe("k", 8, THRESHOLD), LoopVerdict::Warn { consecutive: 2 });
    }

    #[test]
    fn threshold_zero_behaves_as_one() {
        let mut guard = ToolCallLoopGuard::new();
        assert_eq!(guard.observe("k", 1, 0), LoopVerdict::FirstCall);
        assert_eq!(
            guard.observe("k", 1, 0),
            LoopVerdict::Block { consecutive: 2 }
        );
    }

    #[test]
    fn threshold_one_blocks_on_the_second_identical_repeat() {
        let mut guard = ToolCallLoopGuard::new();
        assert_eq!(guard.observe("k", 1, 1), LoopVerdict::FirstCall);
        assert_eq!(
            guard.observe("k", 1, 1),
            LoopVerdict::Block { consecutive: 2 }
        );
    }

    #[test]
    fn clear_resets_to_first_call() {
        let mut guard = ToolCallLoopGuard::new();
        guard.observe("k", 1, THRESHOLD);
        guard.observe("k", 1, THRESHOLD);
        assert_eq!(guard.consecutive(), 2);
        guard.clear();
        assert_eq!(guard.consecutive(), 0);
        assert_eq!(guard.last_key(), None);
        assert_eq!(guard.observe("k", 1, THRESHOLD), LoopVerdict::FirstCall);
        assert_eq!(guard.consecutive(), 1);
    }

    #[test]
    fn messages_name_the_tool_and_the_repeat_count() {
        let warning = loop_warning_text("cat", 3, THRESHOLD);
        assert!(warning.contains("cat"), "{warning}");
        assert!(warning.contains('3'), "{warning}");
        let block = loop_block_text("process_list", 4, THRESHOLD);
        assert!(block.contains("process_list"), "{block}");
        assert!(block.contains('4'), "{block}");
    }

    #[test]
    fn digest_is_stable_for_identical_content_and_differs_otherwise() {
        let a = result_digest(&[simple("hello")], &[]);
        let b = result_digest(&[simple("hello")], &[]);
        let c = result_digest(&[simple("world")], &[]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, result_digest(&[], &[]));
    }

    #[test]
    fn digest_is_bounded_to_the_byte_budget() {
        // Two payloads that agree for the first 64 KiB and diverge afterwards
        // MUST digest identically: that is the bound, proven deterministically
        // rather than by timing a wall clock.
        let prefix = "x".repeat(RESULT_DIGEST_BYTE_BUDGET);
        let a = format!("{prefix}{}", "a".repeat(RESULT_DIGEST_BYTE_BUDGET));
        let b = format!("{prefix}{}", "b".repeat(RESULT_DIGEST_BYTE_BUDGET));
        assert_ne!(a, b);
        assert_eq!(
            result_digest(&[simple(&a)], &[]),
            result_digest(&[simple(&b)], &[])
        );
        // A difference inside the budget is still observed.
        let short_a = "a".repeat(RESULT_DIGEST_BYTE_BUDGET);
        let short_b = "b".repeat(RESULT_DIGEST_BYTE_BUDGET);
        assert_ne!(
            result_digest(&[simple(&short_a)], &[]),
            result_digest(&[simple(&short_b)], &[])
        );
    }

    #[test]
    fn digest_covers_context_files() {
        let file = ContextFile {
            file_name: "src/main.rs".to_string(),
            file_content: "fn main() {}".to_string(),
            ..Default::default()
        };
        let base = result_digest(&[], &[]);
        assert_ne!(base, result_digest(&[], &[file.clone()]));
        let mut other = file.clone();
        other.file_content = "fn main() { }".to_string();
        assert_ne!(result_digest(&[], &[file]), result_digest(&[], &[other]));
    }

    #[test]
    fn identity_key_ignores_argument_order_and_whitespace() {
        let a = call_identity_key("cat", r#"{"path":"a.rs","lines":[1,2]}"#);
        let b = call_identity_key("cat", "{ \"lines\" : [1,2], \"path\" : \"a.rs\" }");
        assert_eq!(a, b);
        let c = call_identity_key("cat", r#"{"path":"b.rs"}"#);
        assert_ne!(a, c);
        assert_ne!(a, call_identity_key("grep", r#"{"path":"a.rs"}"#));
        assert_eq!(
            call_identity_key("cat", "   "),
            call_identity_key("cat", "{}")
        );
    }

    #[test]
    fn identity_key_does_not_panic_on_malformed_json() {
        let key = call_identity_key("cat", "not json at all");
        assert!(key.starts_with("cat"));
        assert!(key.contains("not json at all"));
    }
}
