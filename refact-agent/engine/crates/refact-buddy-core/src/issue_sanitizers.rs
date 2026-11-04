use crate::actor::redact_sensitive;

pub fn sanitize_title(raw: &str) -> String {
    let single: String = raw.chars().filter(|&c| c != '\n' && c != '\r').collect();
    if single.chars().count() > 120 {
        single.chars().take(120).collect()
    } else {
        single
    }
}

pub fn sanitize_body(raw: &str) -> String {
    let escaped = raw.replace("```", "'''");
    if escaped.chars().count() > 4000 {
        escaped.chars().take(4000).collect()
    } else {
        escaped
    }
}

fn redact_issue_text(text: &str) -> String {
    let mut result = redact_sensitive(text);
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            result = result.replace(&home, "~");
        }
    }
    result
}

pub fn sanitize_issue_title(raw: &str) -> String {
    sanitize_title(&redact_issue_text(raw))
}

pub fn sanitize_issue_body(raw: &str) -> String {
    sanitize_body(&redact_issue_text(raw))
}

pub fn sanitize_labels(raw: &[String]) -> Vec<String> {
    raw.iter()
        .map(|label| redact_sensitive(label))
        .map(|label| label.chars().take(50).collect::<String>())
        .filter(|label| !label.trim().is_empty())
        .take(5)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_title_strips_newlines() {
        let result = sanitize_title("Error:\nline 2\r\nline 3");
        assert!(!result.contains('\n'));
        assert!(!result.contains('\r'));
        assert_eq!(result, "Error:line 2line 3");
    }

    #[test]
    fn sanitize_title_caps_at_120_unicode_chars() {
        let raw = "ё".repeat(150);
        let result = sanitize_title(&raw);
        assert_eq!(result.chars().count(), 120);
        assert_eq!(result, "ё".repeat(120));
    }

    #[test]
    fn sanitize_title_keeps_short_input() {
        assert_eq!(sanitize_title("ok"), "ok");
        assert_eq!(sanitize_title(&"ё".repeat(120)), "ё".repeat(120));
    }

    #[test]
    fn sanitize_body_substitutes_code_fences() {
        assert_eq!(sanitize_body("a```b"), "a'''b");
        assert_eq!(sanitize_body("```"), "'''");
    }

    #[test]
    fn sanitize_body_caps_at_4000_unicode_chars() {
        let raw = "ж".repeat(4500);
        let result = sanitize_body(&raw);
        assert_eq!(result.chars().count(), 4000);
    }

    #[test]
    fn sanitize_body_keeps_short_input() {
        assert_eq!(sanitize_body("plain"), "plain");
    }

    #[test]
    fn sanitize_issue_title_redacts_credentials() {
        let result = sanitize_issue_title("failed token=supersecret123");
        assert!(!result.contains("supersecret123"));
        assert!(result.contains("token=[REDACTED]"));
    }

    #[test]
    fn sanitize_issue_body_redacts_bearer_tokens() {
        let result = sanitize_issue_body("auth Bearer abcsecret def");
        assert!(!result.contains("abcsecret"));
        assert!(result.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn sanitize_labels_redacts_caps_and_filters_empty() {
        let labels = vec![
            "bug".to_string(),
            "   ".to_string(),
            format!("ё{}", "x".repeat(60)),
            "token=leaky-secret".to_string(),
            String::new(),
        ];
        let result = sanitize_labels(&labels);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "bug");
        assert_eq!(result[1].chars().count(), 50);
        assert!(result[2].contains("token=[REDACTED]"));
        assert!(!result[2].contains("leaky-secret"));
    }

    #[test]
    fn sanitize_labels_keeps_first_five_in_order() {
        let labels: Vec<String> = ["one", "two", "three", "four", "five", "six", "seven"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = sanitize_labels(&labels);
        assert_eq!(
            result,
            vec![
                "one".to_string(),
                "two".to_string(),
                "three".to_string(),
                "four".to_string(),
                "five".to_string()
            ]
        );
    }

    #[test]
    fn sanitize_labels_empty_input() {
        assert!(sanitize_labels(&[]).is_empty());
    }
}
