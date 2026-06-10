use regex::Regex;
use std::sync::OnceLock;

pub fn redact_sensitive(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            (
                Regex::new(r#"(?i)Bearer\s+[^\s"',]+"#).unwrap(),
                "Bearer [REDACTED]",
            ),
            (
                Regex::new(r"sk-[A-Za-z0-9]{8,}").unwrap(),
                "[REDACTED_SK_TOKEN]",
            ),
            (
                Regex::new(r#"(?i)\bghp_[A-Za-z0-9]{10,}\b"#).unwrap(),
                "[REDACTED_GH_TOKEN]",
            ),
            (
                Regex::new(r#"(?i)\bglpat-[A-Za-z0-9_-]{10,}\b"#).unwrap(),
                "[REDACTED_GL_TOKEN]",
            ),
            (
                Regex::new(
                    r#"(?i)\b(api[_-]?key|apikey|token|secret|password)\s*[:=]\s*[^\s"',;]+"#,
                )
                .unwrap(),
                "$1=[REDACTED]",
            ),
            (
                Regex::new(r#"(?i)Authorization:\s*[^\s"',]+"#).unwrap(),
                "Authorization: [REDACTED]",
            ),
            (
                Regex::new(r#"(?i)(https?://[^\s?#]+)\?[^\s)\]]+"#).unwrap(),
                "$1?[REDACTED]",
            ),
            (
                Regex::new(r#"file://[^\s)\]]+"#).unwrap(),
                "file://[REDACTED_PATH]",
            ),
            (
                Regex::new(r#"[A-Za-z]:\\[^\s)\]]+"#).unwrap(),
                "[REDACTED_PATH]",
            ),
            (
                Regex::new(r#"/(?:Users|home)/[^\s)]+"#).unwrap(),
                "[REDACTED_PATH]",
            ),
        ]
    });

    let mut out = text.to_string();
    for (re, replacement) in patterns {
        out = re.replace_all(&out, *replacement).into_owned();
    }
    out
}

pub fn safe_truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut end = max_len.min(s.len());
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}
