use std::path::PathBuf;
use std::collections::HashSet;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryWriteOutcome {
    Created(PathBuf),
    DuplicateTouched(PathBuf),
}

impl MemoryWriteOutcome {
    pub fn path(&self) -> &PathBuf {
        match self {
            MemoryWriteOutcome::Created(path) => path,
            MemoryWriteOutcome::DuplicateTouched(path) => path,
        }
    }

    pub fn created(&self) -> bool {
        matches!(self, MemoryWriteOutcome::Created(_))
    }
}

pub struct EnrichmentParams {
    pub base_tags: Vec<String>,
    pub base_filenames: Vec<String>,
    pub base_kind: String,
    pub base_title: Option<String>,
    pub source_chat_id: Option<String>,
}
