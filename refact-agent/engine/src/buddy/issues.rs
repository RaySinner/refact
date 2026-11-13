pub use refact_buddy_core::issue_result::BuddyIssueCreateResult;
pub use refact_buddy_core::issue_sanitizers::{sanitize_body, sanitize_title};
pub use refact_runtime_api::buddy_issue_runtime::{
    check_issue_gate, check_manual_issue_gate, create_confirmed_issue,
    create_confirmed_issue_with_provider, create_issue, create_issue_via_mcp, detect_repo_from_git,
    has_github_mcp, investigation_internal_context, investigation_logs, issue_dedupe_text,
    issue_title_and_body, mcp_issue_args, parse_remote_url, prepare_issue_content,
    record_issue_success, redact_diagnostic_text, resolve_issue_context,
    resolve_or_synthesize_context, validate_issue_binary, IssueGate, IssueProvider, PreparedIssue,
    RepoHost, RepoInfo,
};
pub use refact_buddy_core::issue_sanitizers::{
    sanitize_issue_body, sanitize_issue_title, sanitize_labels,
};
