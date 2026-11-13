use std::collections::HashSet;
use std::path::{Component, Path as FilePath, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use regex::Regex;
use serde::Serialize;

use refact_core::chat_types::{ChatContent, ChatMessage, ContextFile};
use refact_core::json_utils::extract_json_object;
use refact_core::tokens_runtime::count_text_tokens_with_fallback;
use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;

use crate::enrichment_cache::{fingerprint_bytes, fingerprint_hex, MemoSourceFingerprint};
use crate::memo::MemoRecord;

static PATH_IN_CARD_RE: OnceLock<Regex> = OnceLock::new();
static TITLE_IN_CARD_RE: OnceLock<Regex> = OnceLock::new();
static CODE_FENCE_RE: OnceLock<Regex> = OnceLock::new();
static TOOL_PATH_LINE_RE: OnceLock<Regex> = OnceLock::new();
static TITLE_ICON_RE: OnceLock<Regex> = OnceLock::new();
static KIND_ICON_RE: OnceLock<Regex> = OnceLock::new();
static RELATED_BULLET_RE: OnceLock<Regex> = OnceLock::new();
static LINE_RANGE_SUFFIX_RE: OnceLock<Regex> = OnceLock::new();

fn path_in_card_re() -> &'static Regex {
    PATH_IN_CARD_RE.get_or_init(|| Regex::new(r"Memory file: (.+)").unwrap())
}

fn title_in_card_re() -> &'static Regex {
    TITLE_IN_CARD_RE.get_or_init(|| Regex::new(r"(?m)^Title: (.+)$").unwrap())
}

fn code_fence_re() -> &'static Regex {
    CODE_FENCE_RE.get_or_init(|| Regex::new(r"```[\s\S]*?```").unwrap())
}

fn tool_path_line_re() -> &'static Regex {
    TOOL_PATH_LINE_RE.get_or_init(|| Regex::new(r"(?m)^📄\s+(.+)$").unwrap())
}

fn title_icon_re() -> &'static Regex {
    TITLE_ICON_RE.get_or_init(|| Regex::new(r"(?m)^📌\s+(.+)$").unwrap())
}

fn kind_icon_re() -> &'static Regex {
    KIND_ICON_RE.get_or_init(|| Regex::new(r"(?m)^📦\s+(.+)$").unwrap())
}

fn related_bullet_re() -> &'static Regex {
    RELATED_BULLET_RE.get_or_init(|| Regex::new(r"(?m)^-\s+(.+?)\s+\(([^()\n]+)\)\s*$").unwrap())
}

fn line_range_suffix_re() -> &'static Regex {
    LINE_RANGE_SUFFIX_RE
        .get_or_init(|| Regex::new(r"^(?P<path>.+):(?P<line1>\d+)-(?P<line2>\d+)$").unwrap())
}

fn format_enrichment_card(m: &MemoRecord) -> String {
    let mut out = String::new();
    out.push_str("# Related memory (short form)\n");
    out.push_str("Note: this is a heuristic match and may be unrelated to the actual problem.\n\n");
    if let Some(title) = &m.title {
        out.push_str(&format!("Title: {}\n", bounded_card_field(title)));
    }
    if let Some(kind) = &m.kind {
        out.push_str(&format!("Kind: {}\n", bounded_card_field(kind)));
    }
    if let Some(score) = m.score {
        out.push_str(&format!("Relevance: {:.0}%\n", score * 100.0));
    }
    if !m.tags.is_empty() {
        let tags = m
            .tags
            .iter()
            .take(16)
            .map(|tag| bounded_card_field(tag))
            .collect::<Vec<_>>();
        out.push_str(&format!("Tags: {}\n", tags.join(", ")));
    }
    if let Some(path) = &m.file_path {
        out.push_str(&format!("Memory file: {}\n", path.display()));
        out.push_str(&format!(
            "To load full content: call `cat(paths=\"{}\")`\n\n",
            path.display()
        ));
    }
    out.push_str(&m.content);
    out
}

fn bounded_card_field(value: &str) -> String {
    value.chars().take(512).collect()
}

pub const KNOWLEDGE_SCORE_THRESHOLD: f32 = 0.75;
pub const FORCED_KNOWLEDGE_SCORE_THRESHOLD: f32 = 0.50;
pub const KNOWLEDGE_ENRICHMENT_MARKER: &str = "knowledge_enrichment";
pub const MAX_QUERY_LENGTH: usize = 2000;
pub const AUTO_ENRICHMENT_TOTAL_TOKEN_CAP: usize = 1600;
pub const AUTO_ENRICHMENT_CARD_TOKEN_CAP: usize = 480;
const MAX_ENRICHMENT_PREVIEW_ITEMS: usize = 5;
const MAX_ENRICHMENT_PREVIEW_CANDIDATES: usize = 64;

pub fn enrichment_query_fingerprint(query: &str) -> String {
    fingerprint_hex(&fingerprint_bytes(&[query.as_bytes()]))
}

pub fn enrichment_identity_from_context(message: &ChatMessage) -> Option<String> {
    (message.role == "context_file" && message.tool_call_id == KNOWLEDGE_ENRICHMENT_MARKER)
        .then(|| {
            message
                .extra
                .get("knowledge_enrichment")
                .and_then(|value| value.get("identity"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .flatten()
}

pub fn record_enrichment_identity_on_user_message(
    message: &mut ChatMessage,
    identity: &str,
    query_fingerprint: &str,
) {
    message.extra.insert(
        "knowledge_enrichment".to_string(),
        serde_json::json!({
            "identity": identity,
            "query_fingerprint": query_fingerprint,
        }),
    );
}

pub fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(count_text_tokens_with_fallback(None, text)).unwrap_or(u64::MAX)
}

pub fn context_message_stats(message: &ChatMessage) -> (u64, u64, u64) {
    let ChatContent::ContextFiles(files) = &message.content else {
        return (0, 0, 0);
    };
    let char_count = files
        .iter()
        .map(|file| file.file_content.len() as u64)
        .sum::<u64>();
    (
        files.len() as u64,
        char_count,
        files
            .iter()
            .map(|file| estimate_tokens(&file.file_content))
            .sum(),
    )
}

pub fn normalize_query(query: &str) -> String {
    let normalized = code_fence_re().replace_all(query, " [code] ").to_string();
    let normalized = normalized.trim();
    if normalized.len() > MAX_QUERY_LENGTH {
        normalized.chars().take(MAX_QUERY_LENGTH).collect()
    } else {
        normalized.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrichmentDecision {
    FirstUser,
    Forced,
    Signaled,
    SkipEmpty,
    SkipCommand,
    SkipThreshold,
}

impl EnrichmentDecision {
    pub fn should_enrich(self) -> bool {
        matches!(self, Self::FirstUser | Self::Forced | Self::Signaled)
    }
}

pub fn should_enrich(
    messages: &[ChatMessage],
    query_raw: &str,
    query_normalized: &str,
    force_enrichment: bool,
) -> EnrichmentDecision {
    let trimmed = query_raw.trim();
    if trimmed.is_empty() {
        return EnrichmentDecision::SkipEmpty;
    }
    if trimmed.starts_with('@') || trimmed.starts_with('/') {
        return EnrichmentDecision::SkipCommand;
    }
    if force_enrichment {
        tracing::info!("Knowledge enrichment: explicitly enabled for later turn");
        return EnrichmentDecision::Forced;
    }
    let user_message_count = messages.iter().filter(|m| m.role == "user").count();
    if user_message_count == 1 {
        tracing::info!("Knowledge enrichment: first user message");
        return EnrichmentDecision::FirstUser;
    }
    let strong = count_strong_signals(query_raw);
    let weak = count_weak_signals(query_raw, query_normalized);
    if strong >= 1 {
        tracing::info!("Knowledge enrichment: {} strong signal(s)", strong);
        return EnrichmentDecision::Signaled;
    }
    if weak >= 2 && query_normalized.len() >= 20 {
        tracing::info!("Knowledge enrichment: {} weak signal(s)", weak);
        return EnrichmentDecision::Signaled;
    }
    EnrichmentDecision::SkipThreshold
}

fn count_strong_signals(query: &str) -> usize {
    let query_lower = query.to_lowercase();
    let mut count = 0;
    let error_keywords = [
        "error",
        "panic",
        "exception",
        "traceback",
        "stack trace",
        "segfault",
        "failed",
        "unable to",
        "cannot",
        "doesn't work",
        "does not work",
        "broken",
        "bug",
        "crash",
    ];
    if error_keywords.iter().any(|kw| query_lower.contains(kw)) {
        count += 1;
    }
    let file_extensions = [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".cpp", ".c", ".h",
    ];
    let config_files = [
        "cargo.toml",
        "package.json",
        "tsconfig",
        "pyproject",
        ".yaml",
        ".yml",
        ".toml",
    ];
    if file_extensions.iter().any(|ext| query_lower.contains(ext))
        || config_files.iter().any(|f| query_lower.contains(f))
    {
        count += 1;
    }
    static PATH_RE: OnceLock<Regex> = OnceLock::new();
    let path_re = PATH_RE.get_or_init(|| Regex::new(r"\b[\w-]+/[\w-]+(?:/[\w.-]+)*\b").unwrap());
    if path_re.is_match(query) {
        count += 1;
    }
    if query.contains("::") || query.contains("->") || query.contains("`") {
        count += 1;
    }
    let retrieval_phrases = [
        "search",
        "find",
        "where is",
        "which file",
        "look up",
        "in this repo",
        "in the codebase",
        "in the project",
    ];
    if retrieval_phrases.iter().any(|p| query_lower.contains(p)) {
        count += 1;
    }
    count
}

fn count_weak_signals(query_raw: &str, query_normalized: &str) -> usize {
    let mut count = 0;
    if query_raw.contains('?') {
        count += 1;
    }
    let query_lower = query_raw.trim().to_lowercase();
    let question_starters = [
        "how",
        "why",
        "what",
        "where",
        "when",
        "can",
        "should",
        "could",
        "would",
        "is there",
        "are there",
    ];
    if question_starters.iter().any(|s| query_lower.starts_with(s)) {
        count += 1;
    }
    if query_normalized.len() >= 80 {
        count += 1;
    }
    count
}

pub fn build_bounded_enrichment_context_files(
    mut memories: Vec<MemoRecord>,
    total_token_cap: usize,
    card_token_cap: usize,
) -> Vec<ContextFile> {
    memories.sort_by(|a, b| {
        b.score
            .unwrap_or_default()
            .total_cmp(&a.score.unwrap_or_default())
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.memid.cmp(&b.memid))
    });

    let mut remaining_tokens = total_token_cap;
    let mut context_files = Vec::new();
    for memo in memories {
        let Some(file_path) = memo.file_path.as_ref() else {
            continue;
        };
        let card_cap = card_token_cap.min(remaining_tokens);
        let (content, content_truncated) = bounded_enrichment_content(&memo.content, card_cap);
        let mut bounded_memo = memo.clone();
        bounded_memo.content = content;
        let mut card = format_enrichment_card(&bounded_memo);
        if content_truncated {
            card.push_str("\nEnrichment metadata: truncated=true\n");
        }
        let Some(card) = truncate_enrichment_card(&card, card_cap) else {
            continue;
        };
        let card_tokens = estimate_tokens(&card);
        if card_tokens > remaining_tokens as u64 {
            continue;
        }
        remaining_tokens = remaining_tokens.saturating_sub(card_tokens as usize);
        let line_count = card.lines().count().max(1);
        context_files.push(ContextFile {
            file_name: file_path.to_string_lossy().to_string(),
            file_content: card,
            line1: 1,
            line2: line_count,
            file_rev: None,
            symbols: vec![],
            gradient_type: -1,
            usefulness: 80.0 + (memo.score.unwrap_or(0.75) * 20.0),
            skip_pp: true,
        });
        if remaining_tokens == 0 {
            break;
        }
    }
    context_files
}

fn bounded_enrichment_content(content: &str, token_cap: usize) -> (String, bool) {
    let max_chars = token_cap.saturating_mul(4).max(1);
    let mut chars = content.chars();
    let bounded = chars.by_ref().take(max_chars).collect::<String>();
    (bounded, chars.next().is_some())
}

pub fn truncate_enrichment_card(card: &str, token_cap: usize) -> Option<String> {
    if estimate_tokens(card) <= token_cap as u64 {
        return Some(card.to_string());
    }

    const TRUNCATION_METADATA: &str = "\nEnrichment metadata: truncated=true\n";
    let mut truncated = String::new();
    for line in card.lines() {
        let mut candidate = truncated.clone();
        candidate.push_str(line);
        candidate.push('\n');
        candidate.push_str(TRUNCATION_METADATA);
        if estimate_tokens(&candidate) > token_cap as u64 {
            break;
        }
        truncated.push_str(line);
        truncated.push('\n');
    }
    if truncated.trim().is_empty() {
        return None;
    }
    truncated.push_str(TRUNCATION_METADATA);
    Some(truncated)
}

pub fn has_knowledge_enrichment_near(messages: &[ChatMessage], user_idx: usize) -> bool {
    let search_start = user_idx.saturating_sub(2);
    let search_end = (user_idx + 2).min(messages.len());
    for i in search_start..search_end {
        if messages[i].role == "context_file"
            && messages[i].tool_call_id == KNOWLEDGE_ENRICHMENT_MARKER
        {
            tracing::info!("Skipping enrichment - already enriched at position {}", i);
            return true;
        }
    }
    false
}

pub fn get_existing_context_file_paths(messages: &[ChatMessage]) -> HashSet<String> {
    let mut paths = HashSet::new();
    for msg in messages {
        if msg.role == "context_file" {
            let files: Vec<ContextFile> = match &msg.content {
                ChatContent::ContextFiles(files) => files.clone(),
                ChatContent::SimpleText(text) => {
                    serde_json::from_str::<Vec<ContextFile>>(text).unwrap_or_default()
                }
                _ => vec![],
            };
            for file in files {
                paths.insert(file.file_name.clone());
            }
        }
    }
    paths
}

fn path_has_unsafe_component(path: &FilePath) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn path_has_markdown_extension(path: &FilePath) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("mdx"))
        .unwrap_or(false)
}

fn workspace_root_for_allowed_knowledge_dir(root: &FilePath) -> Option<PathBuf> {
    let knowledge_folder = FilePath::new(KNOWLEDGE_FOLDER_NAME);
    let knowledge_name = knowledge_folder.file_name()?;
    let refact_name = knowledge_folder.parent()?.file_name()?;
    if root.file_name()? == knowledge_name && root.parent()?.file_name()? == refact_name {
        root.parent()?.parent().map(|path| path.to_path_buf())
    } else {
        None
    }
}

fn candidate_paths_for_enrichment_path(path: &FilePath, allowed_dirs: &[PathBuf]) -> Vec<PathBuf> {
    if path.is_absolute() {
        return vec![path.to_path_buf()];
    }

    let mut candidates = Vec::new();
    for root in allowed_dirs {
        if path.starts_with(KNOWLEDGE_FOLDER_NAME) {
            if let Some(workspace_root) = workspace_root_for_allowed_knowledge_dir(root) {
                candidates.push(workspace_root.join(path));
            }
        } else {
            candidates.push(root.join(path));
        }
    }
    candidates
}

fn canonicalize_enrichment_candidate(raw_path: &str, allowed_dirs: &[PathBuf]) -> Option<PathBuf> {
    let path_str = strip_line_range_suffix(raw_path);
    let path_str = path_str.trim();
    if allowed_dirs.is_empty() || path_str.is_empty() || path_str.contains('\0') {
        return None;
    }

    let path = FilePath::new(path_str);
    if path_has_unsafe_component(path) || !path_has_markdown_extension(path) {
        return None;
    }

    for candidate in candidate_paths_for_enrichment_path(path, allowed_dirs) {
        let canonical = match std::fs::canonicalize(&candidate) {
            Ok(canonical) => dunce::simplified(&canonical).to_path_buf(),
            Err(_) => continue,
        };
        if !std::fs::metadata(&canonical)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        if !path_has_markdown_extension(&canonical) {
            continue;
        }
        if allowed_dirs.iter().any(|root| canonical.starts_with(root)) {
            return Some(canonical);
        }
    }

    None
}

fn strip_line_range_suffix(path: &str) -> String {
    let trimmed = path.trim();
    line_range_suffix_re()
        .captures(trimmed)
        .and_then(|caps| caps.name("path").map(|m| m.as_str().trim().to_string()))
        .unwrap_or_else(|| trimmed.to_string())
}

fn title_from_section(section: &str) -> Option<String> {
    title_in_card_re()
        .captures(section)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .or_else(|| {
            title_icon_re()
                .captures(section)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim().to_string())
        })
}

fn kind_from_section_or_path(section: &str, path_str: &str) -> String {
    let raw_kind = kind_icon_re()
        .captures(section)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_lowercase())
        .filter(|kind| !kind.is_empty())
        .unwrap_or_else(|| {
            if path_str.contains("trajectories") {
                "trajectory".to_string()
            } else {
                "memory".to_string()
            }
        });

    match raw_kind.as_str() {
        "trajectory" => "trajectory".to_string(),
        "file" => "file".to_string(),
        _ => "memory".to_string(),
    }
}

fn push_enrichment_item(
    items: &mut Vec<EnrichmentItem>,
    seen_paths: &mut HashSet<String>,
    candidate_attempts: &mut usize,
    allowed_dirs: &[PathBuf],
    raw_path: &str,
    label: Option<String>,
    kind: String,
    content: String,
) {
    if items.len() >= MAX_ENRICHMENT_PREVIEW_ITEMS
        || *candidate_attempts >= MAX_ENRICHMENT_PREVIEW_CANDIDATES
    {
        return;
    }
    *candidate_attempts += 1;

    let path = match canonicalize_enrichment_candidate(raw_path, allowed_dirs) {
        Some(path) => path,
        None => {
            tracing::warn!(
                "preview: skipping enrichment path outside allowed roots: {}",
                strip_line_range_suffix(raw_path)
            );
            return;
        }
    };
    let path_str = path.to_string_lossy().to_string();

    if seen_paths.contains(&path_str) {
        return;
    }

    let label = label
        .filter(|s| !s.trim().is_empty())
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| path_str.clone());
    let line_count = content.lines().count().max(1);

    seen_paths.insert(path_str.clone());

    items.push(EnrichmentItem {
        kind,
        label,
        context_file: ContextFile {
            file_name: path_str,
            file_content: content,
            line1: 1,
            line2: line_count,
            file_rev: None,
            symbols: vec![],
            gradient_type: -1,
            usefulness: 85.0,
            skip_pp: true,
        },
    });
}

/// Extract enrichment items from tool result messages produced by the knowledge tool.
/// Content comes directly from tool results (server-generated) — no re-reading from disk.
/// Paths are validated against allowed directories.
pub fn extract_items_from_tool_results(
    messages: &[ChatMessage],
    allowed_dirs: &[PathBuf],
) -> Vec<EnrichmentItem> {
    let path_re = path_in_card_re();
    let title_re = title_in_card_re();

    let mut items: Vec<EnrichmentItem> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut candidate_attempts = 0usize;

    for msg in messages {
        if items.len() >= MAX_ENRICHMENT_PREVIEW_ITEMS
            || candidate_attempts >= MAX_ENRICHMENT_PREVIEW_CANDIDATES
        {
            break;
        }
        if msg.role != "tool" {
            continue;
        }
        let text = match &msg.content {
            ChatContent::SimpleText(t) => t.as_str(),
            _ => continue,
        };

        for section in text.split("# Related memory").skip(1) {
            if items.len() >= MAX_ENRICHMENT_PREVIEW_ITEMS
                || candidate_attempts >= MAX_ENRICHMENT_PREVIEW_CANDIDATES
            {
                break;
            }
            let path_str = match path_re
                .captures(section)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim().to_string())
            {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };

            let label = title_re
                .captures(section)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim().to_string())
                .or_else(|| title_from_section(section));

            let card = format!("# Related memory{}", section);
            let kind = kind_from_section_or_path(section, &path_str);
            push_enrichment_item(
                &mut items,
                &mut seen_paths,
                &mut candidate_attempts,
                allowed_dirs,
                &path_str,
                label,
                kind,
                card,
            );
        }

        for section in text.split("\n---\n") {
            if items.len() >= MAX_ENRICHMENT_PREVIEW_ITEMS
                || candidate_attempts >= MAX_ENRICHMENT_PREVIEW_CANDIDATES
            {
                break;
            }
            let path_str = match tool_path_line_re()
                .captures(section)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim().to_string())
            {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };

            let label = title_from_section(section);
            let kind = kind_from_section_or_path(section, &path_str);
            push_enrichment_item(
                &mut items,
                &mut seen_paths,
                &mut candidate_attempts,
                allowed_dirs,
                &path_str,
                label,
                kind,
                section.trim().to_string(),
            );
        }

        for caps in related_bullet_re().captures_iter(text) {
            if items.len() >= MAX_ENRICHMENT_PREVIEW_ITEMS
                || candidate_attempts >= MAX_ENRICHMENT_PREVIEW_CANDIDATES
            {
                break;
            }
            let label = caps.get(1).map(|m| m.as_str().trim().to_string());
            let path_str = match caps.get(2).map(|m| m.as_str().trim().to_string()) {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };
            let content = label
                .as_ref()
                .map(|l| {
                    format!(
                        "# Related memory (short form)\nTitle: {}\nMemory file: {}",
                        l, path_str
                    )
                })
                .unwrap_or_else(|| {
                    format!("# Related memory (short form)\nMemory file: {}", path_str)
                });
            let kind = kind_from_section_or_path(text, &path_str);
            push_enrichment_item(
                &mut items,
                &mut seen_paths,
                &mut candidate_attempts,
                allowed_dirs,
                &path_str,
                label,
                kind,
                content,
            );
        }
    }

    items
}

/// A single enrichment item returned to the frontend for wand-preview chip rendering.
#[derive(Serialize)]
pub struct EnrichmentItem {
    pub kind: String,
    pub label: String,
    pub context_file: ContextFile,
}

pub async fn source_fingerprint(path: &FilePath, content: &str) -> Option<MemoSourceFingerprint> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    let modified_ns = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(MemoSourceFingerprint {
        path: path.to_path_buf(),
        size: metadata.len(),
        modified_ns,
        content_fingerprint: fingerprint_bytes(&[content.as_bytes()]),
    })
}

pub fn parse_rewritten_text(text: &str) -> String {
    let stripped = {
        let t = text.trim();
        if t.starts_with("```") {
            let inner: Vec<&str> = t.lines().skip(1).collect();
            let last = inner
                .iter()
                .rposition(|l| l.trim() == "```")
                .unwrap_or(inner.len());
            inner[..last].join("\n")
        } else {
            t.to_string()
        }
    };

    let val = serde_json::from_str::<serde_json::Value>(stripped.trim())
        .or_else(|_| extract_json_object(text));

    match val {
        Ok(v) => v
            .get("rewritten_text")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::memo::MemoRecord;

    fn write_file(path: &FilePath, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn create_file_symlink(target: &FilePath, link: &FilePath) -> bool {
        #[cfg(unix)]
        {
            return std::os::unix::fs::symlink(target, link).is_ok();
        }
        #[cfg(windows)]
        {
            return std::os::windows::fs::symlink_file(target, link).is_ok();
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    fn canonical(path: &FilePath) -> PathBuf {
        dunce::simplified(&fs::canonicalize(path).unwrap()).to_path_buf()
    }

    fn extract_from_single_path(path: &FilePath, allowed_dirs: &[PathBuf]) -> Vec<EnrichmentItem> {
        extract_from_path_str(&path.display().to_string(), allowed_dirs)
    }

    fn extract_from_path_str(path: &str, allowed_dirs: &[PathBuf]) -> Vec<EnrichmentItem> {
        let message = format!(
            "📄 {}:1-3\n📌 Memory Title\n📦 decision\nbody\n\n---\n",
            path
        );
        extract_items_from_tool_results(&[tool_message(&message)], allowed_dirs)
    }

    fn tool_message(content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(content.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn enrichment_decision_matrix_preserves_existing_trigger_rules() {
        let first_turn = vec![ChatMessage::new("user".to_string(), "hello".to_string())];
        assert_eq!(
            should_enrich(&first_turn, "hello", "hello", false),
            EnrichmentDecision::FirstUser
        );
        let later_turn = vec![
            ChatMessage::new("user".to_string(), "hello".to_string()),
            ChatMessage::new("assistant".to_string(), "reply".to_string()),
            ChatMessage::new("user".to_string(), "plain followup".to_string()),
        ];
        assert_eq!(
            should_enrich(&later_turn, "plain followup", "plain followup", false),
            EnrichmentDecision::SkipThreshold
        );
        assert_eq!(
            should_enrich(&later_turn, "", "", false),
            EnrichmentDecision::SkipEmpty
        );
        assert_eq!(
            should_enrich(&later_turn, "/help", "/help", true),
            EnrichmentDecision::SkipCommand
        );
        assert_eq!(
            should_enrich(&later_turn, "plain followup", "plain followup", true),
            EnrichmentDecision::Forced
        );
        assert_eq!(
            should_enrich(
                &later_turn,
                "find src/chat/generation.rs error",
                "find src/chat/generation.rs error",
                false
            ),
            EnrichmentDecision::Signaled
        );
    }

    #[test]
    fn enrichment_context_stats_measure_cards_without_retaining_text() {
        let message = ChatMessage {
            role: "context_file".to_string(),
            content: ChatContent::ContextFiles(vec![ContextFile {
                file_name: "private-memory.md".to_string(),
                file_content: "private enrichment content".to_string(),
                line1: 1,
                line2: 1,
                file_rev: None,
                symbols: Vec::new(),
                gradient_type: -1,
                usefulness: 80.0,
                skip_pp: true,
            }]),
            ..Default::default()
        };

        assert_eq!(
            context_message_stats(&message),
            (
                1,
                "private enrichment content".len() as u64,
                estimate_tokens("private enrichment content")
            )
        );
    }

    #[test]
    fn enrichment_identity_helpers_only_store_fingerprints() {
        let mut user = ChatMessage::new("user".to_string(), "sensitive query".to_string());
        let query_fingerprint = enrichment_query_fingerprint(&user.content.content_text_only());
        record_enrichment_identity_on_user_message(&mut user, "query:result", &query_fingerprint);

        assert!(!serde_json::Value::Object(user.extra.clone())
            .to_string()
            .contains("sensitive query"));
        assert_eq!(
            user.extra["knowledge_enrichment"]["query_fingerprint"],
            query_fingerprint
        );
    }

    #[test]
    fn bounded_enrichment_cards_preserve_order_and_mark_truncation() {
        let memories = vec![
            MemoRecord {
                memid: "lower".to_string(),
                tags: vec!["knowledge".to_string()],
                content: "lower relevance\n".repeat(1_000),
                file_path: Some(PathBuf::from("/knowledge/lower.md")),
                title: Some("Lower".to_string()),
                created: None,
                kind: Some("memory".to_string()),
                score: Some(0.80),
                line_range: None,
            },
            MemoRecord {
                memid: "higher".to_string(),
                tags: vec!["knowledge".to_string()],
                content: "higher relevance\n".repeat(1_000),
                file_path: Some(PathBuf::from("/knowledge/higher.md")),
                title: Some("Higher".to_string()),
                created: None,
                kind: Some("memory".to_string()),
                score: Some(0.95),
                line_range: None,
            },
        ];

        let files = build_bounded_enrichment_context_files(
            memories,
            AUTO_ENRICHMENT_TOTAL_TOKEN_CAP,
            AUTO_ENRICHMENT_CARD_TOKEN_CAP,
        );
        assert_eq!(
            files.first().map(|file| file.file_name.as_str()),
            Some("/knowledge/higher.md")
        );
        assert!(files.iter().all(
            |file| estimate_tokens(&file.file_content) <= AUTO_ENRICHMENT_CARD_TOKEN_CAP as u64
        ));
        assert!(files
            .iter()
            .any(|file| file.file_content.contains("truncated=true")));
        assert!(
            files
                .iter()
                .map(|file| estimate_tokens(&file.file_content))
                .sum::<u64>()
                <= AUTO_ENRICHMENT_TOTAL_TOKEN_CAP as u64
        );
    }

    #[test]
    fn enrichment_card_truncation_keeps_markdown_line_boundaries() {
        let card =
            "# Heading\n\n- café\n- résumé\n\n```rust\nlet value = \"🌻\";\n```\n".repeat(100);
        let truncated =
            truncate_enrichment_card(&card, 120).expect("card should retain a safe prefix");

        assert!(truncated.ends_with("Enrichment metadata: truncated=true\n"));
        assert!(estimate_tokens(&truncated) <= 120);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn extract_items_from_tool_results_parses_knowledge_tool_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        let path = knowledge_dir.join("memory.md");
        write_file(&path, "memory body");
        let items = extract_from_single_path(&path, &[canonical(&knowledge_dir)]);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Memory Title");
        assert_eq!(items[0].kind, "memory");
        assert_eq!(
            items[0].context_file.file_name,
            canonical(&path).display().to_string()
        );
    }

    #[test]
    fn extract_items_from_tool_results_parses_related_memory_bullets() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        let path = knowledge_dir.join("related.md");
        write_file(&path, "related body");
        let messages = vec![tool_message(&format!(
            "## Related memories (short form)\n\n- Related Title ({})\n  short desc\n",
            path.display()
        ))];
        let items = extract_items_from_tool_results(&messages, &[canonical(&knowledge_dir)]);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Related Title");
        assert_eq!(
            items[0].context_file.file_name,
            canonical(&path).display().to_string()
        );
    }

    #[test]
    fn extract_items_from_tool_results_rejects_parent_component_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        write_file(&knowledge_dir.join("allowed.md"), "allowed body");
        write_file(&dir.path().join(".refact/secrets.json"), "{}");
        let traversal = knowledge_dir.join("../secrets.json");

        let items = extract_from_single_path(&traversal, &[canonical(&knowledge_dir)]);

        assert!(items.is_empty());
    }

    #[test]
    fn extract_items_from_tool_results_rejects_config_dir_non_knowledge_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let knowledge_dir = config_dir.join("knowledge");
        let provider_file = config_dir.join("providers.d/provider.md");
        write_file(&knowledge_dir.join("memory.md"), "memory body");
        write_file(&provider_file, "provider body");

        let items = extract_from_single_path(&provider_file, &[canonical(&knowledge_dir)]);

        assert!(items.is_empty());
    }

    #[test]
    fn extract_items_from_tool_results_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        let outside = dir.path().join("outside.md");
        let link = knowledge_dir.join("link.md");
        write_file(&knowledge_dir.join("memory.md"), "memory body");
        write_file(&outside, "outside body");
        if !create_file_symlink(&outside, &link) {
            return;
        }

        let items = extract_from_single_path(&link, &[canonical(&knowledge_dir)]);

        assert!(items.is_empty());
    }

    #[test]
    fn extract_items_from_tool_results_accepts_valid_canonical_knowledge_doc() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        let path = knowledge_dir.join("valid.md");
        write_file(&path, "valid body");

        let items = extract_from_single_path(&path, &[canonical(&knowledge_dir)]);

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].context_file.file_name,
            canonical(&path).display().to_string()
        );
    }

    #[test]
    fn extract_items_from_tool_results_accepts_relative_refact_knowledge_doc() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        let path = knowledge_dir.join("relative.md");
        write_file(&path, "relative body");

        let items = extract_from_path_str(
            ".refact/knowledge/relative.md",
            &[canonical(&knowledge_dir)],
        );

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].context_file.file_name,
            canonical(&path).display().to_string()
        );
    }

    #[test]
    fn extract_items_from_tool_results_accepts_relative_memory_doc_name() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        let path = knowledge_dir.join("nested/relative.mdx");
        write_file(&path, "relative body");

        let items = extract_from_path_str("nested/relative.mdx", &[canonical(&knowledge_dir)]);

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].context_file.file_name,
            canonical(&path).display().to_string()
        );
    }

    #[test]
    fn extract_items_from_tool_results_rejects_relative_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        write_file(&knowledge_dir.join("allowed.md"), "allowed body");
        write_file(&dir.path().join("outside.md"), "outside body");

        let items = extract_from_path_str("../outside.md", &[canonical(&knowledge_dir)]);

        assert!(items.is_empty());
    }

    #[test]
    fn extract_items_from_tool_results_rejects_relative_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        let outside = dir.path().join("outside.md");
        let link = knowledge_dir.join("link.md");
        write_file(&knowledge_dir.join("memory.md"), "memory body");
        write_file(&outside, "outside body");
        if !create_file_symlink(&outside, &link) {
            return;
        }

        let items = extract_from_path_str("link.md", &[canonical(&knowledge_dir)]);

        assert!(items.is_empty());
    }

    #[test]
    fn extract_items_from_tool_results_is_bounded_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        for i in 0..10 {
            write_file(&knowledge_dir.join(format!("valid-{i}.md")), "valid body");
        }
        let mut message = String::new();
        for i in 0..70 {
            let path = if i < 60 {
                format!("missing-{i}.md")
            } else {
                format!("valid-{}.md", i - 60)
            };
            message.push_str(&format!("📄 {path}:1-3\n📌 Title {i}\nbody\n\n---\n"));
        }

        let items = extract_items_from_tool_results(
            &[tool_message(&message)],
            &[canonical(&knowledge_dir)],
        );

        assert_eq!(items.len(), 4);
        assert!(items
            .iter()
            .all(|item| item.context_file.file_name.contains("valid-")));

        let mut duplicate_message = String::new();
        for i in 0..10 {
            duplicate_message.push_str(&format!(
                "📄 valid-0.md:1-3\n📌 Duplicate {i}\nbody\n\n---\n"
            ));
        }
        for i in 1..10 {
            duplicate_message
                .push_str(&format!("📄 valid-{i}.md:1-3\n📌 Valid {i}\nbody\n\n---\n"));
        }

        let deduped = extract_items_from_tool_results(
            &[tool_message(&duplicate_message)],
            &[canonical(&knowledge_dir)],
        );

        assert_eq!(deduped.len(), MAX_ENRICHMENT_PREVIEW_ITEMS);
        let unique_paths = deduped
            .iter()
            .map(|item| item.context_file.file_name.clone())
            .collect::<HashSet<_>>();
        assert_eq!(unique_paths.len(), deduped.len());
    }

}
