use std::path::PathBuf;
use std::sync::Arc;

use refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk;
use refact_runtime_api::global_context::GlobalContext;
use crate::tools::review_scope::{paths_match, ReviewScope};
use refact_tool_api::review_types::ReviewFinding;

const WINDOW_LINES: u32 = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvidenceOutcome {
    pub checked: usize,
    pub present: usize,
    pub relocated: usize,
    pub unreadable: usize,
}

fn strip_line_number_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 7 {
        return trimmed;
    }
    let rest = &trimmed[digits.len()..];
    for marker in [": ", ":", "| ", "|", " "] {
        if let Some(stripped) = rest.strip_prefix(marker) {
            return stripped;
        }
    }
    trimmed
}

pub fn normalize_snippet(text: &str) -> String {
    text.lines()
        .map(strip_line_number_prefix)
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn window_text(text: &str, line_start: u32, line_end: u32) -> String {
    let first = line_start.saturating_sub(WINDOW_LINES).max(1) as usize;
    let last = line_end.saturating_add(WINDOW_LINES) as usize;
    text.lines()
        .enumerate()
        .filter(|(index, _)| {
            let line = index + 1;
            line >= first && line <= last
        })
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn locate_snippet(text: &str, needle: &str) -> Option<(u32, u32)> {
    let needle_lines: Vec<String> = normalize_snippet(needle)
        .lines()
        .map(str::to_string)
        .collect();
    if needle_lines.is_empty() {
        return None;
    }
    let file_lines: Vec<String> = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let first = needle_lines.first()?;
    for (index, line) in file_lines.iter().enumerate() {
        if !line.contains(first.as_str()) {
            continue;
        }
        let mut cursor = index;
        let mut matched = 0;
        for needle_line in &needle_lines {
            let mut found = false;
            while cursor < file_lines.len() {
                if file_lines[cursor].contains(needle_line.as_str()) {
                    found = true;
                    cursor += 1;
                    break;
                }
                if !file_lines[cursor].trim().is_empty() {
                    break;
                }
                cursor += 1;
            }
            if !found {
                break;
            }
            matched += 1;
        }
        if matched == needle_lines.len() {
            return Some((index as u32 + 1, cursor as u32));
        }
    }
    None
}

pub fn resolve_scope_path(scope: &ReviewScope, candidate: &str) -> Option<PathBuf> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return None;
    }
    let direct = PathBuf::from(candidate);
    if direct.is_absolute() && direct.exists() {
        return Some(direct);
    }
    let known = scope
        .files
        .iter()
        .chain(scope.changed_files.iter())
        .find(|path| paths_match(&path.to_string_lossy(), candidate));
    if let Some(path) = known {
        return Some(path.clone());
    }
    if let Some(root) = scope.repo_root.as_ref() {
        let joined = root.join(candidate);
        if joined.exists() {
            return Some(joined);
        }
    }
    direct.exists().then_some(direct)
}

pub async fn verify_evidence(
    gcx: Arc<GlobalContext>,
    scope: &ReviewScope,
    findings: &mut [ReviewFinding],
) -> EvidenceOutcome {
    let mut outcome = EvidenceOutcome::default();
    for finding in findings.iter_mut() {
        outcome.checked += 1;
        if finding.evidence.trim().is_empty() {
            continue;
        }
        let Some(path) = resolve_scope_path(scope, &finding.file) else {
            outcome.unreadable += 1;
            continue;
        };
        let Ok(text) = get_file_text_from_memory_or_disk(gcx.clone(), &path).await else {
            outcome.unreadable += 1;
            continue;
        };
        finding.file = path.to_string_lossy().to_string();
        let window = window_text(&text, finding.line_start, finding.line_end);
        let needle = normalize_snippet(&finding.evidence);
        if !needle.is_empty() && normalize_snippet(&window).contains(&needle) {
            finding.evidence_present = true;
            outcome.present += 1;
            continue;
        }
        if let Some((start, end)) = locate_snippet(&text, &finding.evidence) {
            finding.line_start = start;
            finding.line_end = end.max(start);
            finding.evidence_present = true;
            outcome.present += 1;
            outcome.relocated += 1;
        }
    }
    outcome
}
