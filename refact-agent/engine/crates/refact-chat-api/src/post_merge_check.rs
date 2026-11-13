use std::path::PathBuf;
use std::time::Duration;

use refact_tasks::types::{BoardCard, BoardColumn, StatusUpdate, VerificationOutcome};

use crate::verifier::ExpectedCardState;

pub const MAX_OUTPUT_TAIL_CHARS: usize = 6000;

#[derive(Clone, Debug)]
pub struct PostMergeCheckRequest {
    pub task_id: String,
    pub card_id: String,
    pub workspace_root: PathBuf,
    pub enabled: bool,
    pub timeout: Duration,
    pub expected_merge_commit: String,
    pub expected_card_state: Option<ExpectedCardState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostMergeCheckResult {
    pub checked: bool,
    pub auto_reverted: bool,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub output_tail: String,
    pub merge_commit: Option<String>,
    pub revert_commit: Option<String>,
    pub fix_card_id: Option<String>,
    pub skipped_reason: Option<String>,
    pub revert_skipped_reason: Option<String>,
    pub outcome: VerificationOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostMergeCommand {
    Verify {
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
        argv: Vec<String>,
    },
    Git(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostMergeCommandOutput {
    pub exit_code: Option<i32>,
    pub success: bool,
    pub output: String,
    pub outcome: VerificationOutcome,
}

pub fn skipped_result(reason: &str) -> PostMergeCheckResult {
    PostMergeCheckResult {
        checked: false,
        auto_reverted: false,
        command: None,
        exit_code: None,
        output_tail: String::new(),
        merge_commit: None,
        revert_commit: None,
        fix_card_id: None,
        skipped_reason: Some(reason.to_string()),
        revert_skipped_reason: None,

        outcome: VerificationOutcome::Unknown,
    }
}

pub fn failed_without_revert(
    command: Option<String>,
    outcome: VerificationOutcome,
    output_tail: String,
) -> PostMergeCheckResult {
    PostMergeCheckResult {
        checked: true,
        auto_reverted: false,
        command,
        exit_code: None,
        output_tail,
        merge_commit: None,
        revert_commit: None,
        fix_card_id: None,
        skipped_reason: None,
        revert_skipped_reason: Some("verification was not a genuine command failure".to_string()),
        outcome,
    }
}

pub fn git_text(output: PostMergeCommandOutput) -> Option<String> {
    output
        .success
        .then(|| output.output.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn ensure_regressed_column(board: &mut refact_tasks::types::TaskBoard) {
    if !board.columns.iter().any(|column| column.id == "regressed") {
        board.columns.push(BoardColumn {
            id: "regressed".to_string(),
            title: "Regressed".to_string(),
        });
    }
}

pub fn next_fix_card_id(board: &refact_tasks::types::TaskBoard, card_id: &str) -> String {
    let base = format!("{}-fix", card_id);
    if board.get_card(&base).is_none() {
        return base;
    }
    for index in 2.. {
        let candidate = format!("{}-{}", base, index);
        if board.get_card(&candidate).is_none() {
            return candidate;
        }
    }
    unreachable!()
}

pub fn build_fix_card(
    source_card: &BoardCard,
    fix_card_id: &str,
    command: &str,
    output_tail: &str,
    merge_commit: &str,
    revert_commit: &str,
    revert_skipped_reason: Option<&str>,
    diagnostic: Option<&str>,
    now: &str,
) -> BoardCard {
    let fence = markdown_code_fence(output_tail);
    let revert_summary = match revert_skipped_reason {
        Some(reason) => format!(
            "Auto-revert was skipped: {}. A human should inspect the current branch before reverting or fixing.",
            reason
        ),
        None => "The merge was automatically reverted after post-merge verification failed.".to_string(),
    };
    let diagnostic_section = diagnostic
        .map(|diagnostic| {
            format!(
                "\n## Revert diagnostic\n{}text\n{}\n{}\n",
                fence,
                diagnostic.trim(),
                fence
            )
        })
        .unwrap_or_default();
    BoardCard {
        id: fix_card_id.to_string(),
        title: format!("Fix regression in {}", source_card.id),
        column: "planned".to_string(),
        priority: source_card.priority.clone(),
        depends_on: Vec::new(),
        instructions: format!(
            "# Fix regression in {}\n\n{}\n\n- Original card: {} — {}\n- Merge commit: {}\n- Revert commit: {}\n- Failing command: `{}`\n{}\n## Failing output\n{}text\n{}\n{}\n\nFix the regression and run the verification command before merging again.\n",
            source_card.id,
            revert_summary,
            source_card.id,
            source_card.title,
            merge_commit,
            revert_commit,
            command,
            diagnostic_section,
            fence,
            output_tail.trim(),
            fence
        ),
        assignee: None,
        agent_chat_id: None,
        retry_count: 0,
        status_updates: vec![StatusUpdate {
            timestamp: now.to_string(),
            message: format!("Created after post-merge regression in {}", source_card.id),
        }],
        comments: Vec::new(),
        final_report: None,
        final_report_structured: None,
        verifier_report: None,
        created_at: now.to_string(),
        started_at: None,
        last_heartbeat_at: None,
        completed_at: None,
        agent_branch: None,
        agent_worktree: None,
        agent_worktree_name: None,
        base_branch: source_card.base_branch.clone(),
        base_commit: source_card.base_commit.clone(),
        ab_variants: None,
        target_files: source_card.target_files.clone(),
        scope_guard_mode: source_card.scope_guard_mode,
        team_members: Vec::new(),
    }
}

pub fn markdown_code_fence(text: &str) -> String {
    "`".repeat(max_backtick_run(text).max(2) + 1)
}

pub fn max_backtick_run(text: &str) -> usize {
    let mut max_run = 0;
    let mut current = 0;
    for c in text.chars() {
        if c == '`' {
            current += 1;
            max_run = max_run.max(current);
        } else {
            current = 0;
        }
    }
    max_run
}

pub fn first_error_line(output: &str) -> String {
    let mut fallback = None;
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if fallback.is_none() {
            fallback = Some(line.to_string());
        }
        let lower = line.to_lowercase();
        if lower.contains("error") || lower.contains("failed") || lower.contains("failure") {
            return truncate_line(line, 240);
        }
    }
    fallback
        .map(|line| truncate_line(&line, 240))
        .unwrap_or_else(|| "verification failed".to_string())
}

pub fn truncate_line(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let mut truncated = line.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

pub fn tail_chars(text: &str, max_chars: usize) -> String {
    let len = text.chars().count();
    if len <= max_chars {
        return text.to_string();
    }
    text.chars().skip(len - max_chars).collect()
}
