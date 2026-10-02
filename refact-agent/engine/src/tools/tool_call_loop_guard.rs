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
use std::collections::HashMap;
use std::hash::Hasher;
use std::sync::{LazyLock, RwLock};

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

/// Signature of a tool-specific identity extractor.
///
/// It receives the parsed arguments and returns the part of the identity that
/// distinguishes "the same call" for that tool. Anything it deliberately ignores
/// (e.g. `cat`'s line ranges) stops affecting the fingerprint.
pub type IdentityExtractor = fn(&serde_json::Value) -> String;

/// Per-tool identity overrides, keyed by canonical tool name.
///
/// `cat` is pre-registered here (it is a builtin) so no chat-layer code has to
/// remember to install it. Locking is fallible-tolerant: a poisoned or contended
/// lock degrades to "no override", never a panic.
static IDENTITY_OVERRIDES: LazyLock<RwLock<HashMap<String, IdentityExtractor>>> =
    LazyLock::new(|| {
        let mut overrides: HashMap<String, IdentityExtractor> = HashMap::new();
        overrides.insert(CAT_TOOL_NAME.to_string(), cat_identity_value);
        RwLock::new(overrides)
    });

/// Register (or, with `None`, clear) an identity override for a tool.
///
/// Intended for builtin registration and tests, not for per-request wiring: the
/// table is process-global, so calling it on every tool call would be wasteful
/// and would let a request change global identity semantics.
pub fn set_identity_override(canonical_tool_name: &str, extract: Option<IdentityExtractor>) {
    if let Ok(mut overrides) = IDENTITY_OVERRIDES.write() {
        match extract {
            Some(extract) => {
                overrides.insert(canonical_tool_name.to_string(), extract);
            }
            None => {
                overrides.remove(canonical_tool_name);
            }
        }
    }
}

/// Identity key for a tool call, honouring a registered override.
///
/// Falls back to [`call_identity_key`] (canonical tool name + normalized
/// arguments) when no override is registered or the arguments do not parse.
pub fn identity_key_for(canonical_tool_name: &str, args_json: &str) -> String {
    let override_fn = IDENTITY_OVERRIDES
        .read()
        .ok()
        .and_then(|overrides| overrides.get(canonical_tool_name).copied());
    if let Some(extract) = override_fn {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(args_json.trim()) {
            return extract(&value);
        }
    }
    call_identity_key(canonical_tool_name, args_json)
}

/// Canonical name of the file-reading tool, used to key its identity override.
pub const CAT_TOOL_NAME: &str = "cat";

/// Identity fingerprint for a `cat` call: the requested paths with any
/// `:START` / `:START-END` suffix stripped, each normalized through
/// `normalize_file_name`, sorted and de-duplicated.
///
/// Reading a file line-by-line is the loop this guard exists to catch, so
/// `cat("a.rs")` and `cat("a.rs:10-20")` MUST share a key. Path order inside one
/// call is ignored (models list paths in any order) and duplicate entries
/// collapse, so `cat("a.rs:1-20", "a.rs:30-40")` also equals `cat("a.rs")`.
///
/// Every argument other than `paths` is folded in with the generic canonical JSON
/// normalization, so `cat(paths="a.rs", symbols="foo")` and `symbols="bar"` stay
/// distinct calls. Malformed JSON, a missing `paths`, or a non-string `paths`
/// degrade to the raw argument string instead of panicking.
pub fn cat_identity(args_json: &str) -> String {
    let trimmed = args_json.trim();
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(trimmed)
    else {
        return format!("{CAT_TOOL_NAME}\u{1}{trimmed}");
    };

    let paths_key = match fields.get("paths") {
        Some(serde_json::Value::String(raw_paths)) => canonical_cat_paths(raw_paths),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| "\u{1}unrenderable".into()),
        None => String::new(),
    };

    let mut other_args: Vec<(&String, &serde_json::Value)> = fields
        .iter()
        .filter(|(key, _)| key.as_str() != "paths")
        .collect();
    other_args.sort_by(|left, right| left.0.cmp(right.0));
    let rendered: Vec<String> = other_args
        .into_iter()
        .map(|(key, value)| {
            let key_json = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string());
            format!("{key_json}:{}", canonical_json(value))
        })
        .collect();

    format!(
        "{CAT_TOOL_NAME}\u{1}paths={paths_key}\u{1}args={{{}}}",
        rendered.join(",")
    )
}

/// `cat` identity extractor in the shape [`IDENTITY_OVERRIDES`] stores.
///
/// Round-trips through JSON so the registered function and the directly callable
/// [`cat_identity`] share one implementation instead of drifting apart.
fn cat_identity_value(value: &serde_json::Value) -> String {
    let rendered = serde_json::to_string(value).unwrap_or_default();
    cat_identity(&rendered)
}

/// Split, range-strip, normalize, sort and de-duplicate a comma-separated `paths`.
///
/// The range suffix is parsed with the very same `try_parse_line_range` that
/// `tool_cat` itself uses (extracted to module scope for this purpose), so the
/// guard can never disagree with the tool about what `:1-20` means. A path whose
/// suffix is not a valid range (`C:/x/a.rs`) keeps it, exactly as the tool does.
fn canonical_cat_paths(raw_paths: &str) -> String {
    let mut normalized: Vec<String> = raw_paths
        .split(',')
        .map(|entry| {
            let entry = entry.trim();
            let stripped = match entry.rfind(':') {
                Some(colon) => match crate::tools::tool_cat::try_parse_line_range(&entry[colon + 1..])
                {
                    Ok(Some(_)) => entry[..colon].trim().to_string(),
                    // Not a range, or an invalid one: keep the entry verbatim.
                    _ => entry.to_string(),
                },
                None => entry.to_string(),
            };
            refact_core::chat_types::normalize_file_name(stripped)
        })
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized.join(",")
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

    #[test]
    fn cat_identity_ignores_line_ranges() {
        // The whole point of the override: reading a file line-by-line is the loop
        // the guard must see, so every range spelling collapses to the whole-file read.
        let whole = cat_identity(r#"{"paths":"a.rs"}"#);
        assert_eq!(whole, cat_identity(r#"{"paths":"a.rs:1"}"#));
        assert_eq!(whole, cat_identity(r#"{"paths":"a.rs:1-20"}"#));
        assert_eq!(whole, cat_identity(r#"{"paths":"a.rs:30-40"}"#));
        assert_eq!(whole, cat_identity(r#"{"paths":" a.rs : 5 - 7 "}"#));
        assert_ne!(whole, cat_identity(r#"{"paths":"b.rs"}"#));
    }

    #[test]
    fn cat_identity_strips_ranges_from_windows_paths() {
        let whole = cat_identity(r#"{"paths":"C:/x/a.rs"}"#);
        assert_eq!(whole, cat_identity(r#"{"paths":"C:/x/a.rs:5"}"#));
        assert_eq!(whole, cat_identity(r#"{"paths":"C:/x/a.rs:5-9"}"#));
        // Backslashes normalize to forward slashes, so both spellings agree.
        assert_eq!(whole, cat_identity(r#"{"paths":"C:\\x\\a.rs"}"#));
        // An invalid range (`start > end`) is NOT a range for the tool either, so
        // the suffix stays part of the path and the two differ.
        assert_ne!(whole, cat_identity(r#"{"paths":"C:/x/a.rs:9-5"}"#));
    }

    #[test]
    fn cat_identity_is_order_insensitive_and_shares_file_components() {
        let left = cat_identity(r#"{"paths":"a.rs:1-20,g.rs"}"#);
        let right = cat_identity(r#"{"paths":"g.rs,a.rs:30-40"}"#);
        assert_eq!(left, right);
        assert!(left.contains("a.rs") && left.contains("g.rs"), "{left}");
        // Duplicate entries collapse rather than growing the key.
        assert_eq!(left, cat_identity(r#"{"paths":"a.rs:1-20,g.rs,a.rs:7"}"#));
        assert_ne!(left, cat_identity(r#"{"paths":"a.rs:1-20"}"#));
    }

    #[test]
    fn cat_identity_keeps_other_arguments_significant() {
        let base = cat_identity(r#"{"paths":"a.rs"}"#);
        assert_ne!(base, cat_identity(r#"{"paths":"a.rs","symbols":"foo"}"#));
        assert_ne!(
            cat_identity(r#"{"paths":"a.rs","symbols":"foo"}"#),
            cat_identity(r#"{"paths":"a.rs","symbols":"bar"}"#)
        );
        // Key order inside the arguments object is still normalized away.
        assert_eq!(
            cat_identity(r#"{"paths":"a.rs:3-4","symbols":"foo"}"#),
            cat_identity(r#"{"symbols":"foo","paths":"a.rs"}"#)
        );
    }

    #[test]
    fn cat_identity_does_not_panic_on_malformed_input() {
        for bad in [
            "not json at all",
            "{}",
            r#"{"paths":123}"#,
            r#"{"paths":null}"#,
            r#"{"paths":""}"#,
            r#"{"paths":"a.rs:","other":"x"}"#,
            r#"{"paths":"::"}"#,
            r#"{"paths":"a.rs:-"}"#,
        ] {
            let key = cat_identity(bad);
            assert!(key.starts_with(CAT_TOOL_NAME), "{bad} => {key}");
        }
        // Missing `paths` still distinguishes tools/args rather than collapsing.
        assert_eq!(cat_identity("{}"), cat_identity("{}"));
    }

    #[test]
    fn identity_key_for_uses_the_registered_override_only_for_matching_tools() {
        assert_eq!(
            identity_key_for("cat", r#"{"paths":"a.rs:10-20"}"#),
            cat_identity(r#"{"paths":"a.rs:10-20"}"#)
        );
        assert_ne!(
            identity_key_for("cat", r#"{"paths":"a.rs:10-20"}"#),
            call_identity_key("cat", r#"{"paths":"a.rs:10-20"}"#)
        );
        // An unregistered tool keeps the default fingerprint.
        let unregistered = identity_key_for("process_read", r#"{"b":1,"a":2}"#);
        assert_eq!(unregistered, call_identity_key("process_read", r#"{"b":1,"a":2}"#));
        assert_eq!(unregistered, call_identity_key("process_read", r#"{"a":2,"b":1}"#));
        // Malformed args fall back to the default instead of panicking.
        assert_eq!(
            identity_key_for("cat", "not json at all"),
            call_identity_key("cat", "not json at all")
        );
    }

    #[test]
    fn set_identity_override_registers_and_clears() {
        // serial_test is not used here: the override table is process-global, so
        // restore it before the assertion rather than relying on test ordering.
        fn extract_only_path(value: &serde_json::Value) -> String {
            format!("probe\u{1}{}", value.get("p").and_then(|v| v.as_str()).unwrap_or(""))
        }
        set_identity_override("probe_tool", Some(extract_only_path));
        assert_eq!(
            identity_key_for("probe_tool", r#"{"p":"x","junk":"y"}"#),
            "probe\u{1}x"
        );

        set_identity_override("probe_tool", None);
        assert_eq!(
            identity_key_for("probe_tool", r#"{"p":"x","junk":"y"}"#),
            call_identity_key("probe_tool", r#"{"p":"x","junk":"y"}"#)
        );
    }
}
