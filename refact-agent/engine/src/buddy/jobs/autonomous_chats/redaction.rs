pub use refact_buddy_core::autonomous_redaction::*;

#[cfg_attr(not(test), allow(dead_code))]
pub const AUTONOMOUS_PROMPT_CAP_CHARS: usize = 8_000;
#[cfg_attr(not(test), allow(dead_code))]
pub const AUTONOMOUS_EVIDENCE_CAP_CHARS: usize = 24_000;

#[cfg_attr(not(test), allow(dead_code))]
pub fn redact_and_cap_prompt(text: &str) -> String {
    redact_and_cap_text(text, AUTONOMOUS_PROMPT_CAP_CHARS)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn redact_and_cap_evidence(text: &str) -> String {
    redact_and_cap_text(text, AUTONOMOUS_EVIDENCE_CAP_CHARS)
}

pub(super) fn clean_evidence_value(value: &str) -> String {
    preview_text(value, 240)
}

pub(super) fn preview_text(value: &str, max_chars: usize) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    redact_and_cap_text(&single_line, max_chars)
}
