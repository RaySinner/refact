use crate::diagnostics::{collect_diagnostics_from_error, DiagnosticContext};

pub fn synthesize_issue_context(
    error: Option<String>,
    source_file: Option<String>,
    tool_name: Option<String>,
    title: &str,
    body: &str,
) -> Result<DiagnosticContext, String> {
    let has_error = error
        .as_deref()
        .map(|e| !e.trim().is_empty())
        .unwrap_or(false);
    if !has_error && source_file.is_none() && tool_name.is_none() {
        return Err(
            "provide a diagnostic reference (diagnostic_id, diagnostic_index, or collected_at), \
             OR reproduction context (source_file or tool_name) together with either an `error` \
             or the issue title/body"
                .to_string(),
        );
    }

    let message = match error {
        Some(err) if !err.trim().is_empty() => err,
        _ => format!("{title}\n\n{body}"),
    };
    let mut ctx = collect_diagnostics_from_error(&message);
    ctx.source_file = source_file;
    ctx.tool_name = tool_name;
    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{classify_error, DiagnosticSeverity};

    #[test]
    fn missing_reproduction_context_is_rejected() {
        let err = synthesize_issue_context(None, None, None, "title", "body").unwrap_err();
        assert!(err.contains("provide a diagnostic reference"));
    }

    #[test]
    fn error_context_is_synthesized_from_the_error_message() {
        let ctx =
            synthesize_issue_context(Some("error: boom".into()), None, None, "title", "body")
                .unwrap();
        assert_eq!(ctx.error_message, "error: boom");
        assert_eq!(ctx.severity, DiagnosticSeverity::High);
    }

    #[test]
    fn source_file_context_falls_back_to_title_and_body() {
        let ctx = synthesize_issue_context(
            None,
            Some("src/main.rs".into()),
            Some("tool_cat".into()),
            "title",
            "body",
        )
        .unwrap();
        assert_eq!(ctx.source_file.as_deref(), Some("src/main.rs"));
        assert_eq!(ctx.tool_name.as_deref(), Some("tool_cat"));
        assert_eq!(ctx.error_message, "title\n\nbody");
        assert_eq!(ctx.error_type, classify_error("title\n\nbody"));
    }
}
