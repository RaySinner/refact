use std::collections::HashSet;
use std::path::Path;

use chrono::{Duration, Local, Utc};
use regex::Regex;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::knowledge_frontmatter::KnowledgeFrontmatter;
use crate::string_utils::redact_sensitive;

pub const MAX_CONCURRENT_ENRICHMENT_SEARCHES: usize = 8;

const PREFERENCE_MIN_CONFIDENCE: f32 = 0.85;

fn generate_slug(content: &str) -> String {
    let first_line = content.lines().next().unwrap_or("knowledge");
    first_line
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase()
        .chars()
        .take(50)
        .collect()
}

pub fn generate_filename(content: &str) -> String {
    let timestamp = Local::now().format("%Y-%m-%d_%H%M%S").to_string();
    let slug = generate_slug(content);
    let short_uuid = &Uuid::new_v4().to_string()[..8];
    if slug.is_empty() {
        format!("{}_{}_knowledge.md", timestamp, short_uuid)
    } else {
        format!("{}_{}_{}.md", timestamp, short_uuid, slug)
    }
}

pub fn compute_content_hash_hex(content: &str) -> String {
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    hex::encode(h.finalize())
}

pub fn normalize_memory_tag(tag: &str) -> Option<String> {
    let raw = tag.trim().to_lowercase();
    if raw.is_empty() {
        return None;
    }

    let alias = match raw.as_str() {
        "rs" | "rust" => "language:rust",
        "py" | "python" => "language:python",
        "ts" | "typescript" => "language:typescript",
        "js" | "javascript" => "language:javascript",
        "tsx" => "language:typescript-react",
        "jsx" => "language:javascript-react",
        "c++" | "cpp" => "language:cpp",
        "c#" | "csharp" => "language:csharp",
        "behavior_learner" | "behavior-learner" => "workflow:behavior-learner",
        "knowledge_graph" | "knowledge-graph" => "domain:knowledge-graph",
        "playwright" | "playwright mcp" | "playwright-mcp" => "tool:playwright-mcp",
        "chrome" | "chrome tool" | "chrome-tool" => "tool:chrome",
        _ => raw.as_str(),
    };

    let preserve_underscores = alias.starts_with("entity:") || alias.starts_with("symbol:");
    let mut normalized = String::new();
    let mut last_was_dash = false;

    for ch in alias.chars() {
        match ch {
            'a'..='z' | '0'..='9' | ':' | '/' | '.' => {
                normalized.push(ch);
                last_was_dash = false;
            }
            '_' if preserve_underscores => {
                normalized.push(ch);
                last_was_dash = false;
            }
            '-' | '_' | ' ' | '\t' => {
                if !last_was_dash && !normalized.is_empty() {
                    normalized.push('-');
                    last_was_dash = true;
                }
            }
            _ => {}
        }
    }

    let normalized = normalized
        .trim_matches('-')
        .chars()
        .take(96)
        .collect::<String>();
    if normalized.len() >= 2 {
        Some(normalized)
    } else {
        None
    }
}

pub fn normalize_memory_tags(tags: &[String], max_tags: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for tag in tags {
        let Some(normalized) = normalize_memory_tag(tag) else {
            continue;
        };
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
        if out.len() >= max_tags {
            break;
        }
    }

    out
}

pub fn create_frontmatter(
    title: Option<&str>,
    tags: &[String],
    filenames: &[String],
    links: &[String],
    kind: &str,
) -> KnowledgeFrontmatter {
    let now = Local::now();
    let created = now.format("%Y-%m-%d").to_string();
    let review_days = match kind {
        "trajectory" => 90,
        "preference" => 365,
        _ => 90,
    };
    let review_after = (now + Duration::days(review_days))
        .format("%Y-%m-%d")
        .to_string();

    KnowledgeFrontmatter {
        id: Some(Uuid::new_v4().to_string()),
        title: title.map(|t| t.to_string()),
        tags: normalize_memory_tags(tags, 16),
        created: Some(created.clone()),
        updated: Some(created),
        filenames: filenames.to_vec(),
        links: links.to_vec(),
        kind: Some(kind.to_string()),
        status: Some("active".to_string()),
        superseded_by: None,
        deprecated_at: None,
        review_after: Some(review_after),
        source_chat_id: None,

        created_at: Some(Utc::now().to_rfc3339()),
        summary: None,
        description: None,
        entities: Vec::new(),
        related_files: Vec::new(),
        related_entities: Vec::new(),
        content_hash: None,
        source_tool: None,
        source_confidence: None,
        source_trajectory_id: None,
        source_message_range: None,
        source_commit: None,
        topic: None,
        last_used_at: None,
        use_count: 0,
        last_injected_at: None,
        dismissed_count: 0,
        source_id: None,
        source_content_hash: None,
        review_needed: None,
        occurrences: 0,
        signal_key: None,
        last_observed: None,
        extra: Default::default(),
    }
}

pub fn first_nonempty_line(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Some(trimmed.trim_start_matches('#').trim().to_string());
    }
    None
}

fn push_memory_tag(tags: &mut Vec<String>, tag: &str) {
    let Some(normalized) = normalize_memory_tag(tag) else {
        return;
    };
    if !tags.contains(&normalized) {
        tags.push(normalized);
    }
}

fn content_has_any(content_lower: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| content_lower.contains(term))
}

fn push_if_content_matches(tags: &mut Vec<String>, content_lower: &str, tag: &str, terms: &[&str]) {
    if content_has_any(content_lower, terms) {
        push_memory_tag(tags, tag);
    }
}

fn push_file_path_tags(tags: &mut Vec<String>, file: &str) {
    let lower = file.to_lowercase();

    match Path::new(file).extension().and_then(|ext| ext.to_str()) {
        Some("rs") => push_memory_tag(tags, "language:rust"),
        Some("py") => push_memory_tag(tags, "language:python"),
        Some("ts") => push_memory_tag(tags, "language:typescript"),
        Some("tsx") => push_memory_tag(tags, "language:typescript-react"),
        Some("js") => push_memory_tag(tags, "language:javascript"),
        Some("jsx") => push_memory_tag(tags, "language:javascript-react"),
        Some("kt") => push_memory_tag(tags, "language:kotlin"),
        Some("go") => push_memory_tag(tags, "language:go"),
        Some("java") => push_memory_tag(tags, "language:java"),
        Some("md") | Some("mdx") => push_memory_tag(tags, "format:markdown"),
        _ => {}
    }

    let mappings = [
        ("component:buddy", &["/buddy/", "src/buddy/"][..]),
        (
            "component:knowledge-graph",
            &["/knowledge_graph/", "src/knowledge_graph/"][..],
        ),
        ("component:knowledge-index", &["knowledge_index.rs"][..]),
        (
            "component:memory-system",
            &["memories.rs", "/knowledge/"][..],
        ),
        ("component:chat-engine", &["/chat/", "src/chat/"][..]),
        ("component:subchat", &["subchat.rs", "/subchat/"][..]),
        ("component:tools", &["/tools/", "src/tools/"][..]),
        (
            "component:gui-chat",
            &["gui/src/features/chat/", "gui/src/components/chat"][..],
        ),
        (
            "component:gui-state",
            &["gui/src/app/", "gui/src/features/"][..],
        ),
        (
            "component:integrations",
            &["/integrations/", "src/integrations/"][..],
        ),
        (
            "component:providers",
            &["/providers/", "src/providers/"][..],
        ),
        ("component:vecdb", &["/vecdb/", "src/vecdb/"][..]),
        ("component:ast", &["/ast/", "src/ast/"][..]),
        ("component:http-api", &["/http/", "src/http/"][..]),
    ];

    for (tag, terms) in mappings {
        if terms.iter().any(|term| lower.contains(term)) {
            push_memory_tag(tags, tag);
        }
    }
}

pub fn extract_fallback_tags(
    content: &str,
    detected_files: &[String],
    detected_entities: &[String],
) -> Vec<String> {
    let content_lower = content.to_lowercase();
    let mut tags = Vec::new();

    push_if_content_matches(
        &mut tags,
        &content_lower,
        "domain:buddy",
        &["buddy", "memory garden", "behavior learner"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "domain:knowledge-graph",
        &[
            "knowledge graph",
            "kg builder",
            "kg linking",
            "kg mechanism",
        ],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "domain:memory-system",
        &["memory", "memories", "knowledge entry", "knowledge content"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "domain:browser-automation",
        &[
            "browser automation",
            "playwright",
            "chrome tool",
            "accessibility snapshot",
        ],
    );

    push_if_content_matches(
        &mut tags,
        &content_lower,
        "workflow:background-chat",
        &[
            "background chat",
            "autonomous chat",
            "self-initiated",
            "proactive chat",
        ],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "workflow:behavior-learner",
        &[
            "behavior learner",
            "auto-write preferences",
            "auto write preferences",
        ],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "workflow:memory-enrichment",
        &[
            "memory enrichment",
            "kg enrich",
            "enrich knowledge",
            "tag generation",
        ],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "workflow:patch-verification",
        &[
            "patch",
            "apply_patch",
            "verify the actual file",
            "edits silently fail",
        ],
    );

    push_if_content_matches(
        &mut tags,
        &content_lower,
        "component:buddy-scheduler",
        &["scheduler", "buddy job", "buddy jobs", "job/subchat"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "component:subchat",
        &["subchat", "run_subchat", "run_subchat_once"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "component:trajectory-persistence",
        &["trajectory", "save_trajectory_as", "source_trajectory_id"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "component:notifications",
        &["notification", "notifications", "action button", "expire"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "component:buddy-animation",
        &[
            "animation",
            "roaming",
            "waypoint",
            "motion",
            "fireflies",
            "aurora",
            "garden growth",
        ],
    );

    push_if_content_matches(
        &mut tags,
        &content_lower,
        "tool:playwright-mcp",
        &[
            "playwright mcp",
            "playwright",
            "accessibility tree",
            "browser snapshot",
        ],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "tool:chrome",
        &["chrome tool", "chrome", "tab management"],
    );

    push_if_content_matches(
        &mut tags,
        &content_lower,
        "verification:cargo-check",
        &["cargo check"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "verification:rust-tests",
        &["cargo test", "rust unit test"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "verification:git-sanity",
        &["git status", "git diff", "git sanity"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "verification:eslint",
        &["eslint", "max-warnings 0"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "verification:prettier",
        &["prettier", "format:check"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "verification:vitest",
        &["vitest"],
    );

    push_if_content_matches(
        &mut tags,
        &content_lower,
        "language:rust",
        &["cargo", "rust"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "language:typescript",
        &["typescript", "tsc", "eslint", "react"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "framework:react",
        &["react", "redux"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "state:redux",
        &["redux", "rtk query"],
    );
    push_if_content_matches(
        &mut tags,
        &content_lower,
        "protocol:sse",
        &["sse", "stream_delta"],
    );

    for file in detected_files.iter().take(8) {
        push_file_path_tags(&mut tags, file);
    }

    for entity in detected_entities.iter().take(8) {
        if entity.len() >= 3 && entity.len() <= 80 {
            push_memory_tag(&mut tags, &format!("entity:{}", entity));
        }
    }

    tags.truncate(16);
    tags
}

pub fn normalize_preference_text_for_dedupe(text: &str) -> String {
    text.to_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn preference_statement_is_safe(statement: &str, confidence: f32) -> bool {
    if confidence < PREFERENCE_MIN_CONFIDENCE {
        return false;
    }
    let trimmed = statement.trim();
    if trimmed.chars().count() < 12 {
        return false;
    }
    let redacted = redact_sensitive(trimmed);
    if redacted != trimmed || redacted.contains("[REDACTED") {
        return false;
    }
    let normalized = normalize_preference_text_for_dedupe(trimmed);
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    let vague = [
        "remember this",
        "do this",
        "use this",
        "i like this",
        "i prefer this",
        "preference",
        "user preference",
    ];
    if vague.contains(&normalized.as_str()) {
        return false;
    }
    let sensitive_terms = [
        "password",
        "token",
        "secret",
        "credential",
        "api key",
        "apikey",
        "private key",
        "ssh key",
        "home address",
        "phone number",
        "email address",
        "ssn",
        "credit card",
    ];
    if sensitive_terms.iter().any(|term| normalized.contains(term)) {
        return false;
    }
    let lower = trimmed.to_lowercase();
    let preference_cues = [
        "prefer",
        "always",
        "never",
        "avoid",
        "don't",
        "do not",
        "i like",
        "i want",
        "please use",
        "please keep",
        "use ",
        "keep ",
        "format ",
        "respond ",
        "write ",
    ];
    preference_cues.iter().any(|cue| lower.contains(cue))
}

pub fn extract_entities(content: &str) -> Vec<String> {
    let backtick_re =
        Regex::new(r"`([a-zA-Z_][a-zA-Z0-9_:]*(?:::[a-zA-Z_][a-zA-Z0-9_]*)*)`").unwrap();
    backtick_re
        .captures_iter(content)
        .map(|c| c.get(1).unwrap().as_str().to_string())
        .filter(|e| e.len() >= 3 && e.len() <= 100)
        .collect()
}

pub fn extract_file_paths(content: &str) -> Vec<String> {
    let path_re =
        Regex::new(r"(?:^|[\s`])((?:[a-zA-Z0-9_-]+/)+[a-zA-Z0-9_-]+\.[a-zA-Z0-9]+)").unwrap();
    path_re
        .captures_iter(content)
        .map(|c| c.get(1).unwrap().as_str().to_string())
        .collect()
}
