use super::*;

#[cfg_attr(not(test), allow(dead_code))]
pub const AUTONOMOUS_PROMPT_CAP_CHARS: usize = 8_000;
#[cfg_attr(not(test), allow(dead_code))]
pub const AUTONOMOUS_EVIDENCE_CAP_CHARS: usize = 24_000;

#[cfg_attr(not(test), allow(dead_code))]
pub(super) const AUTONOMOUS_REDACTION_SCAN_MULTIPLIER: usize = 4;
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const AUTONOMOUS_REDACTION_SCAN_EXTRA_CHARS: usize = 4_096;
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const TRUNCATED_MARKER: &str = "\n...[truncated]";
#[cfg_attr(not(test), allow(dead_code))]
pub fn redact_and_cap_prompt(text: &str) -> String {
    redact_and_cap_text(text, AUTONOMOUS_PROMPT_CAP_CHARS)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn redact_and_cap_evidence(text: &str) -> String {
    redact_and_cap_text(text, AUTONOMOUS_EVIDENCE_CAP_CHARS)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn redact_and_cap_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let scan_cap = redaction_scan_cap(max_chars);
    let (scan_text, truncated) = bounded_redaction_window(text, scan_cap);
    let mut redacted = redact_sensitive(scan_text);
    if truncated {
        redacted.push_str(TRUNCATED_MARKER);
    }
    cap_text(&redacted, max_chars)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn redaction_scan_cap(max_chars: usize) -> usize {
    max_chars
        .saturating_mul(AUTONOMOUS_REDACTION_SCAN_MULTIPLIER)
        .max(max_chars.saturating_add(AUTONOMOUS_REDACTION_SCAN_EXTRA_CHARS))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn bounded_redaction_window(text: &str, scan_cap: usize) -> (&str, bool) {
    if text.len() <= scan_cap {
        return (text, false);
    }

    let prefix = crate::llm::safe_truncate(text, scan_cap);
    if prefix
        .chars()
        .last()
        .map(is_redaction_boundary)
        .unwrap_or(true)
        || text[prefix.len()..]
            .chars()
            .next()
            .map(is_redaction_boundary)
            .unwrap_or(false)
    {
        return (prefix, true);
    }

    let end = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| is_redaction_boundary(*ch))
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);

    (&prefix[..end], true)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn is_redaction_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            ',' | ';' | ')' | ']' | '}' | '"' | '\'' | '`' | '<' | '>'
        )
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn cap_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    if max_chars <= TRUNCATED_MARKER.len() {
        return crate::llm::safe_truncate(TRUNCATED_MARKER, max_chars).to_string();
    }
    let keep = max_chars - TRUNCATED_MARKER.len();
    let prefix = crate::llm::safe_truncate(text, keep).trim_end().to_string();
    format!("{}{}", prefix, TRUNCATED_MARKER)
}

pub(super) fn clean_evidence_value(value: &str) -> String {
    preview_text(value, 240)
}

pub(super) fn preview_text(value: &str, max_chars: usize) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    redact_and_cap_text(&single_line, max_chars)
}
