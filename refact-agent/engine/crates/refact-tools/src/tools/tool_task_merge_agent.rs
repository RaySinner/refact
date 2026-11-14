use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex as AMutex;

use crate::tools::tools_description::{

    Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params,
};
use refact_runtime_api::GlobalContextCtx;
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum};
use refact_runtime_api::AtCommandsContext;
use refact_chat_api::verifier::outcome_is_infrastructure;
use refact_runtime_api::verifier::load_expected_card_state_for_ctx;
use refact_runtime_api::global_context::GlobalContext;
use refact_runtime_api::task_storage as storage;
use crate::tools::task_tool_helpers::require_bound_planner_task;
use refact_worktrees::service::{worktree_merge_lock, WorktreeService};
use refact_worktrees::types::{
    MergeWorktreeRequest, MergeWorktreeResponse, WorktreeMergeStrategy, WorktreeRecordView,
    WorktreeReference,
};

fn git_merge_lock() -> &'static AMutex<()> {
    worktree_merge_lock()
}

fn strategy_from_str(strategy: &str) -> Result<WorktreeMergeStrategy, String> {
    match strategy {
        "merge" => Ok(WorktreeMergeStrategy::Merge),
        "squash" => Ok(WorktreeMergeStrategy::Squash),
        _ => Err(format!(
            "Invalid strategy '{}', must be 'merge' or 'squash'",
            strategy
        )),
    }
}

pub fn strategy_arg(args: &HashMap<String, Value>) -> Result<&str, String> {
    match args.get("strategy") {
        None => Ok("squash"),
        Some(Value::String(strategy)) => Ok(strategy.as_str()),
        Some(_) => Err("'strategy' must be a string".to_string()),
    }
}

fn cleanup_summary(response: &MergeWorktreeResponse) -> String {
    match response.cleanup.as_ref() {
        Some(cleanup) => format!(
            "worktree_deleted={}, branch_deleted={}, registry_deleted={}, affected_references={}",
            cleanup.worktree_deleted,
            cleanup.branch_deleted,
            cleanup.registry_deleted,
            response.affected_reference_count
        ),
        None => format!(
            "no cleanup requested, affected_references={}",
            response.affected_reference_count
        ),
    }
}

pub fn append_post_merge_check_message(
    message: &mut String,
    result: &refact_runtime_api::post_merge_check::PostMergeCheckResult,
) {
    use refact_tasks::types::VerificationOutcome;

    if !result.checked {
        if let Some(reason) = result.skipped_reason.as_deref() {
            message.push_str(&format!("\n\n**Post-merge check:** skipped ({})", reason));
        }
        return;
    }
    let command = result.command.as_deref().unwrap_or("unknown");
    if result.outcome == VerificationOutcome::Passed {
        message.push_str(&format!("\n\n**Post-merge check:** `{}` passed", command));
        return;
    }
    if result.outcome != VerificationOutcome::CommandFailed {
        let label = match result.outcome {
            VerificationOutcome::Rejected => "could not be run as a command",
            VerificationOutcome::PolicyDenied => "denied by command policy",
            VerificationOutcome::InfrastructureFailed => {
                "could not run due to infrastructure failure"
            }
            VerificationOutcome::NoCommands => "found no verification commands",
            VerificationOutcome::Unknown => "returned an unknown result",
            VerificationOutcome::Passed | VerificationOutcome::CommandFailed => unreachable!(),
        };
        message.push_str(&format!(
            "\n\n**Post-merge check:** `{}` {}\n\n{}",
            command,
            label,
            refact_runtime_api::post_merge_check::first_error_line(&result.output_tail)
        ));
        return;
    }
    message.push_str(&format!(
        "\n\n## Post-merge regression detected\n\n**Verification:** `{}` failed{}\n**Auto-reverted:** {}\n**Merge commit:** {}",
        command,
        result.exit_code.map(|code| format!(" with exit code {}", code)).unwrap_or_default(),
        if result.auto_reverted {
            "true".to_string()
        } else {
            format!("false ({})", result.revert_skipped_reason.as_deref().unwrap_or("not attempted"))
        },
        result.merge_commit.as_deref().unwrap_or("unknown"),
    ));
    if let Some(revert_commit) = result.revert_commit.as_deref() {
        message.push_str(&format!("\n**Revert commit:** {}", revert_commit));
    }
    if let Some(fix_card_id) = result.fix_card_id.as_deref() {
        message.push_str(&format!("\n**Fix card:** {}", fix_card_id));
    }
    message.push_str(&format!(
        "\n\n{}",
        refact_runtime_api::post_merge_check::first_error_line(&result.output_tail)
    ));
}

fn append_merge_warnings(message: &mut String, warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }
    message.push_str("\n\n## Warnings");
    for warning in warnings {
        message.push_str(&format!("\n- {}", warning));
    }
}

fn response_warnings(response: &MergeWorktreeResponse) -> Vec<String> {
    let mut warnings = response.warnings.clone();
    if let Some(cleanup) = response.cleanup.as_ref() {
        warnings.extend(cleanup.warnings.clone());
    }
    warnings
}

pub fn merge_response_message(
    card_id: &str,
    response: &MergeWorktreeResponse,
    post_merge_check: Option<&refact_runtime_api::post_merge_check::PostMergeCheckResult>,
) -> String {
    if let Some(conflict) = response.conflict.as_ref() {
        let mut message = format!(
            "# Merge Conflicts Detected\n\n**Card:** {}\n**Branch:** {} → {}\n**Strategy:** {}\n**Aborted:** {}\n\n## Conflicting Files\n{}\n\n{}",
            card_id,
            response.source_branch,
            response.target_branch,
            response.strategy,
            conflict.aborted,
            if conflict.files.is_empty() {
                "None detected".to_string()
            } else {
                conflict
                    .files
                    .iter()
                    .map(|file| format!("- {}", file))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            conflict.instructions
        );
        append_merge_warnings(&mut message, &response_warnings(response));
        return message;
    }
    if response.status == "nothing_to_merge" {
        let mut message = format!(
            "# Nothing to Merge\n\n**Card:** {}\n**Branch:** {} → {}\n**Commits ahead of base:** 0\n\nCleanup: {}.",
            card_id,
            response.source_branch,
            response.target_branch,
            cleanup_summary(response)
        );
        append_merge_warnings(&mut message, &response_warnings(response));
        return message;
    }
    let mut message = format!(
        "# Agent Work Merged\n\n**Card:** {}\n**Strategy:** {}\n**Branch:** {} → {}\n**Cleanup:** {}\n\nThe agent's work has been successfully merged back to the target branch.",
        card_id,
        response.strategy,
        response.source_branch,
        response.target_branch,
        cleanup_summary(response)
    );
    if let Some(result) = post_merge_check {
        append_post_merge_check_message(&mut message, result);
    }
    append_merge_warnings(&mut message, &response_warnings(response));
    message
}

pub fn parse_force(args: &HashMap<String, Value>) -> Result<bool, String> {
    parse_schema_bool_arg(args, "force", false)
}

fn parse_bypass_verifier(args: &HashMap<String, Value>) -> Result<bool, String> {
    parse_schema_bool_arg(args, "bypass_verifier", false)
}

pub fn parse_auto_revert(args: &HashMap<String, Value>) -> Result<bool, String> {
    parse_schema_bool_arg(args, "auto_revert", false)
}

fn parse_schema_bool_arg(
    args: &HashMap<String, Value>,
    name: &str,
    default: bool,
) -> Result<bool, String> {
    match args.get(name) {
        None => Ok(default),
        Some(value) => refact_tool_api::coerce_bool(value)
            .ok_or_else(|| format!("'{}' must be a boolean", name)),
    }
}

pub fn parse_auto_revert_timeout(args: &HashMap<String, Value>) -> Result<Duration, String> {
    let Some(value) = args.get("auto_revert_timeout_secs") else {
        return Ok(Duration::from_secs(
            refact_runtime_api::post_merge_check::DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS,
        ));
    };
    let secs = match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| "auto_revert_timeout_secs must be a positive integer".to_string())?,
        _ => return Err("auto_revert_timeout_secs must be a positive integer".to_string()),
    };
    if secs == 0 {
        return Err("auto_revert_timeout_secs must be greater than zero".to_string());
    }
    Ok(Duration::from_secs(secs))
}

pub fn required_string_arg<'a>(
    args: &'a HashMap<String, Value>,
    name: &str,
) -> Result<&'a str, String> {
    match args.get(name) {
        Some(Value::String(value)) if value.trim().is_empty() => Err(format!("Missing '{}'", name)),
        Some(Value::String(value)) if value.trim() == value.as_str() => Ok(value.as_str()),
        Some(Value::String(_)) => Err(format!(
            "'{}' must not have leading or trailing whitespace",
            name
        )),
        Some(Value::Null) | None => Err(format!("Missing '{}'", name)),
        Some(_) => Err(format!("'{}' must be a string", name)),
    }
}

pub fn ensure_legacy_agent_worktree_checkout(
    workspace_root: &std::path::Path,
    agent_worktree: &str,
    agent_branch: &str,
) -> Result<(), String> {
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize workspace root: {}", e))?;
    let agent_worktree_path = std::path::Path::new(agent_worktree);
    let agent_worktree_root = agent_worktree_path
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize agent worktree: {}", e))?;
    if agent_worktree_root == workspace_root {
        return Err("Agent worktree path points at the main workspace; refusing to auto-commit main workspace changes".to_string());
    }
    let status = refact_worktrees::git::status_for_path(&agent_worktree_root);
    if !status.is_git_worktree {
        return Err(format!(
            "Agent worktree path '{}' is not a git worktree",
            agent_worktree
        ));
    }
    let agent_top_level =
        refact_worktrees::git::run_git(&agent_worktree_root, &["rev-parse", "--show-toplevel"])?;
    let agent_top_level = std::path::Path::new(agent_top_level.trim())
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize agent worktree root: {}", e))?;
    if agent_top_level != agent_worktree_root {
        return Err(format!(
            "Agent worktree path '{}' is not the git worktree root; refusing to auto-commit it",
            agent_worktree
        ));
    }
    let workspace_common_dir =
        refact_worktrees::git::run_git(&workspace_root, &["rev-parse", "--git-common-dir"])?;
    let agent_common_dir =
        refact_worktrees::git::run_git(&agent_worktree_root, &["rev-parse", "--git-common-dir"])?;
    let canonical_git_dir =
        |root: &std::path::Path, common_dir: &str| -> Result<std::path::PathBuf, String> {
            let path = std::path::Path::new(common_dir.trim());
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            path.canonicalize()
                .map_err(|e| format!("Failed to canonicalize git common dir: {}", e))
        };
    if canonical_git_dir(&workspace_root, &workspace_common_dir)?
        != canonical_git_dir(&agent_worktree_root, &agent_common_dir)?
    {
        return Err(
            "Agent worktree belongs to a different repository; refusing to auto-commit it"
                .to_string(),
        );
    }
    refact_worktrees::git::ensure_checkout_on_branch(
        &agent_worktree_root,
        "Agent worktree",
        agent_branch,
    )
}

fn reference_matches_card(
    task_id: &str,
    card: &refact_tasks::types::BoardCard,
    reference: &WorktreeReference,
) -> bool {
    if reference.task_id.as_deref() == Some(task_id)
        && reference.card_id.as_deref() == Some(card.id.as_str())
    {
        return true;
    }
    if let Some(agent_chat_id) = card.agent_chat_id.as_deref() {
        if reference.chat_id.as_deref() == Some(agent_chat_id) {
            return true;
        }
    }
    card.assignee
        .as_deref()
        .map(|assignee| {
            reference.task_id.as_deref() == Some(task_id)
                && reference.agent_id.as_deref() == Some(assignee)
        })
        .unwrap_or(false)
}

fn registered_record_matches_card(
    view: &WorktreeRecordView,
    task_id: &str,
    card: &refact_tasks::types::BoardCard,
) -> bool {
    if view.meta.task_id.as_deref() == Some(task_id)
        && view.meta.card_id.as_deref() == Some(card.id.as_str())
    {
        return true;
    }
    view.references
        .iter()
        .any(|reference| reference_matches_card(task_id, card, reference))
}

async fn worktree_id_is_valid_for_card(
    service: &WorktreeService,
    worktree_id: &str,
    task_id: &str,
    card: &refact_tasks::types::BoardCard,
) -> Result<WorktreeRecordView, String> {
    let view = service.get_worktree(worktree_id).await?;
    if !registered_record_matches_card(&view, task_id, card) {
        return Err(format!(
            "Registered worktree '{}' does not match task {} card {}",
            worktree_id, task_id, card.id
        ));
    }
    service
        .validate_registered_worktree_checkout(worktree_id)
        .await?;
    Ok(view)
}

fn render_concern_list(concerns: &[String]) -> String {
    if concerns.is_empty() {
        "- verifier failed without concerns".to_string()
    } else {
        concerns
            .iter()
            .map(|concern| format!("- {}", concern))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn verifier_merge_block_message(
    card_id: &str,
    classification: refact_tasks::types::VerifierReportClassification,
    concerns: &[String],
    failing_commands: &[&str],
) -> String {
    use refact_tasks::types::VerifierReportClassification;
    let rendered = render_concern_list(concerns);

    let is_verification_failed = classification == VerifierReportClassification::VerificationFailed
        || !failing_commands.is_empty();

    if is_verification_failed {
        let failing_str = if failing_commands.is_empty() {
            "none identified".to_string()
        } else {
            failing_commands.join(", ")
        };
        format!(
            "Card {} verifier failed with a genuine verification failure. Refusing merge; force=true cannot bypass verification failures.\nFailing command(s): {}\nRerun verification with task_verify_card after fixing the issue.\nVerification failed; bypass_verifier=true is required for emergency override.\nConcerns:\n{}",
            card_id, failing_str, rendered
        )
    } else if classification == VerifierReportClassification::HumanReview {
        format!(
            "Card {} verifier requires human review. Refusing merge without force=true.\nRerun verification with task_verify_card to refresh the report.\nPass force=true to proceed with merge if the concerns are acceptable.\nConcerns:\n{}",
            card_id, rendered
        )
    } else {
        format!(
            "Card {} verifier failed and merge is refused.\nRerun verification with task_verify_card to refresh the report.\nIf every concern is infrastructure-only, retry with force=true.\nConcerns:\n{}",
            card_id, rendered
        )
    }
}

fn verifier_force_warning(card_id: &str, concerns: &[String]) -> String {
    format!(
        "Card {} verifier failed and requires human review. Merge is continuing only because force=true. Concerns:\n{}",
        card_id,
        render_concern_list(concerns)
    )
}

fn append_verifier_warning(message: &mut String, warning: Option<&str>) {
    if let Some(warning) = warning {
        message.push_str("\n\n## Verifier Warning\n\n");
        message.push_str(warning);
    }
}

fn concern_is_launch_failure(concern: &str) -> bool {
    concern.starts_with("Verifier failed to launch; human review recommended:")
}

fn concern_is_infrastructure_only(concern: &str, infrastructure_commands: &[&str]) -> bool {
    concern_is_launch_failure(concern)
        || concern.starts_with("Verifier review subchat unavailable; human review recommended:")
        || infrastructure_commands
            .iter()
            .any(|command| concern.strip_prefix("Verification command failed: ") == Some(*command))
}

pub fn ensure_verifier_allows_merge(
    card: &refact_tasks::types::BoardCard,
    force: bool,
    bypass_verifier: bool,
) -> Result<Option<String>, String> {
    if bypass_verifier {
        return Ok(Some(
            "Verifier Warning: Bypassed by bypass_verifier=true".to_string(),
        ));
    }
    let Some(report) = card.verifier_report.as_ref() else {
        return Ok(None);
    };
    use refact_tasks::types::{VerificationOutcome, VerifierReportClassification};

    let command_failed = report.command_results.iter().any(|result| {
        matches!(result.outcome, VerificationOutcome::CommandFailed)
            || (!result.passed
                && !outcome_is_infrastructure(&result.outcome)
                && !matches!(result.outcome, VerificationOutcome::NoCommands))
    });

    let explicit_infrastructure_concerns = !report.concerns.is_empty()
        && report
            .concerns
            .iter()
            .all(|concern| concern_is_infrastructure_only(concern, &[]));
    let legacy_launch_failure = !report.passed
        && report.classification == VerifierReportClassification::Unknown
        && report.recommendation == "human-review"
        && report.command_results.is_empty()
        && report
            .concerns
            .iter()
            .any(|concern| concern_is_launch_failure(concern))
        && explicit_infrastructure_concerns;
    if force && legacy_launch_failure {
        return Ok(Some(verifier_force_warning(&card.id, &report.concerns)));
    }

    let forceable_human_review = !report.passed
        && report.classification == VerifierReportClassification::HumanReview
        && !command_failed;
    if force && forceable_human_review {
        return Ok(Some(verifier_force_warning(&card.id, &report.concerns)));
    }

    let consistent_pass = report.passed
        && report.classification == VerifierReportClassification::Passed
        && !report.command_results.is_empty()
        && report
            .command_results
            .iter()
            .all(|result| result.passed && result.outcome == VerificationOutcome::Passed);
    if consistent_pass {
        return Ok(None);
    }

    let failing_commands: Vec<&str> = report
        .command_results
        .iter()
        .filter(|result| {
            matches!(result.outcome, VerificationOutcome::CommandFailed)
                || (!result.passed
                    && !outcome_is_infrastructure(&result.outcome)
                    && !matches!(result.outcome, VerificationOutcome::NoCommands))
        })
        .map(|result| result.command.as_str())
        .collect();

    Err(verifier_merge_block_message(
        &card.id,
        report.classification,
        &report.concerns,
        &failing_commands,
    ))
}

pub fn ensure_card_done_allows_merge(
    card: &refact_tasks::types::BoardCard,
    force: bool,
) -> Result<Option<String>, String> {
    let column_ok = card.column == "done";
    let report_present = card
        .final_report
        .as_deref()
        .map(|r| !r.trim().is_empty())
        .unwrap_or(false);
    if !force {
        if !column_ok {
            return Err(format!(
                "Card {} is in column '{}', not 'done'. \
                 Wait for the agent to finish, or pass force=true to merge anyway.",
                card.id, card.column
            ));
        }
        if !report_present {
            return Err(format!(
                "Card {} has no final report. \
                 Wait for the agent to call agent_finish, or pass force=true to merge anyway.",
                card.id
            ));
        }
    }
    if force && (!column_ok || !report_present) {
        return Ok(Some(format!(
            "⚠️ Force-merged card {} in column '{}'. This bypasses normal completion checks; verify the merged code carefully.",
            card.id, card.column
        )));
    }
    Ok(None)
}

fn append_completion_warning(message: &mut String, warning: Option<&str>) {
    if let Some(warning) = warning {
        message.push_str("\n\n");
        message.push_str(warning);
    }
}

fn terminal_card_column(column: &str) -> bool {
    matches!(column, "done" | "failed" | "regressed")
}

fn force_merge_card_matches_current(
    card: &refact_tasks::types::BoardCard,
    expected_assignee: Option<&str>,
    expected_agent_chat_id: Option<&str>,
) -> bool {
    card.assignee.as_deref() == expected_assignee
        && card.agent_chat_id.as_deref() == expected_agent_chat_id
}

pub fn force_merge_post_check_passed_or_absent(
    result: &Result<Option<refact_runtime_api::post_merge_check::PostMergeCheckResult>, String>,
) -> bool {
    use refact_tasks::types::VerificationOutcome;

    match result {
        Ok(None) => true,
        Ok(Some(check)) => check.checked && check.outcome == VerificationOutcome::Passed,
        Err(_) => false,
    }
}

pub fn retained_post_merge_verification_failure(
    result: &Result<Option<refact_runtime_api::post_merge_check::PostMergeCheckResult>, String>,
) -> Option<String> {
    use refact_tasks::types::VerificationOutcome;

    let check = result.as_ref().ok()?.as_ref()?;
    if check.auto_reverted || (check.checked && check.outcome == VerificationOutcome::Passed) {
        return None;
    }
    let outcome = match check.outcome {
        VerificationOutcome::CommandFailed => "verification command failed",
        VerificationOutcome::Rejected => "verification command was rejected",
        VerificationOutcome::PolicyDenied => "verification was denied by policy",
        VerificationOutcome::InfrastructureFailed => "verification infrastructure failed",
        VerificationOutcome::NoCommands => "no verification commands were available",
        VerificationOutcome::Unknown => "verification returned an unknown outcome",
        VerificationOutcome::Passed => "verification was not explicitly checked",
    };
    Some(format!(
        "Forced merge remains applied, but post-merge verification did not pass: {}. Stale agent ownership was cleared; the merged work is retained and the card was not marked done.",
        outcome
    ))
}

pub fn retain_force_merge_without_finalizing(
    card: &mut refact_tasks::types::BoardCard,
    message: &str,
) -> bool {
    card.assignee = None;
    card.agent_chat_id = None;
    card.status_updates.push(refact_tasks::types::StatusUpdate {
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: message.to_string(),
    });
    true
}

pub fn finalize_force_merge_card_if_current(
    card: &mut refact_tasks::types::BoardCard,
    expected_assignee: Option<&str>,
    expected_agent_chat_id: Option<&str>,
) -> bool {
    if !force_merge_card_matches_current(card, expected_assignee, expected_agent_chat_id) {
        return false;
    }
    let mut changed = card.assignee.is_some() || card.agent_chat_id.is_some();
    if !terminal_card_column(&card.column) {
        let now = chrono::Utc::now().to_rfc3339();
        let previous_column = card.column.clone();
        card.column = "done".to_string();
        card.completed_at = Some(now.clone());
        card.status_updates.push(refact_tasks::types::StatusUpdate {
            timestamp: now,
            message: format!(
                "Force-merged by planner without the normal agent_finish path; marked done for manual review from column '{}'.",
                previous_column
            ),
        });
        changed = true;
    }
    card.assignee = None;
    card.agent_chat_id = None;
    changed
}

async fn finalize_force_merge_card_after_merge(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    card_id: &str,
    expected_assignee: Option<String>,
    expected_agent_chat_id: Option<String>,
) -> Result<(), String> {
    let card_id_owned = card_id.to_string();
    let (_, changed) = storage::update_board_atomic(gcx.clone(), task_id, move |board| {
        let Some(card) = board.get_card_mut(&card_id_owned) else {
            return Ok(false);
        };
        Ok(finalize_force_merge_card_if_current(
            card,
            expected_assignee.as_deref(),
            expected_agent_chat_id.as_deref(),
        ))
    })
    .await?;
    if changed {
        storage::update_task_stats(gcx, task_id).await?;
    }
    Ok(())
}

pub async fn clear_board_mirrors_after_registered_merge(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    card_id: &str,
    response: &MergeWorktreeResponse,
    force: bool,
    finalize_force_merge: bool,
    force_merge_auto_reverted: bool,
    retained_verification_failure: Option<String>,
    post_merge_check_error: Option<String>,
    expected_assignee: Option<String>,
    expected_agent_chat_id: Option<String>,
) -> Result<(), String> {
    let clear_ownership = force
        && (response.merged || response.status == "nothing_to_merge")
        && (finalize_force_merge
            || force_merge_auto_reverted
            || retained_verification_failure.is_some()
            || post_merge_check_error.is_some());
    let Some(cleanup) = response.cleanup.as_ref() else {
        if clear_ownership {
            if force_merge_auto_reverted
                || retained_verification_failure.is_some()
                || post_merge_check_error.is_some()
            {
                let card_id_owned = card_id.to_string();
                let retained_verification_failure = retained_verification_failure.clone();
                let post_merge_check_error = post_merge_check_error.clone();
                let (_, changed) =
                    storage::update_board_atomic(gcx.clone(), task_id, move |board| {
                        let Some(card) = board.get_card_mut(&card_id_owned) else {
                            return Ok(false);
                        };
                        if !force_merge_card_matches_current(
                            card,
                            expected_assignee.as_deref(),
                            expected_agent_chat_id.as_deref(),
                        ) {
                            return Ok(false);
                        }
                        let message = retained_verification_failure
                            .clone()
                            .or_else(|| post_merge_check_error.as_deref().map(|error| format!("Forced merge remains applied, but post-merge verification failed to run: {}. Stale agent ownership was cleared; merge success was not inferred from verification.", error)))
                            .unwrap_or_else(|| "Forced merge was auto-reverted after post-merge verification failed; the original card was not marked done and stale agent ownership was cleared.".to_string());
                        Ok(retain_force_merge_without_finalizing(card, &message))
                    })
                    .await?;
                if changed {
                    storage::update_task_stats(gcx, task_id).await?;
                }
            } else {
                finalize_force_merge_card_after_merge(
                    gcx,
                    task_id,
                    card_id,
                    expected_assignee,
                    expected_agent_chat_id,
                )
                .await?;
            }
        }
        return Ok(());
    };
    if !cleanup.worktree_deleted
        && !cleanup.branch_deleted
        && !cleanup.registry_deleted
        && !clear_ownership
    {
        return Ok(());
    }
    let card_id_owned = card_id.to_string();
    let clear_worktree = cleanup.worktree_deleted || cleanup.registry_deleted;
    let clear_branch = cleanup.branch_deleted;
    let retained_verification_failure_for_update = retained_verification_failure.clone();
    let post_merge_check_error_for_update = post_merge_check_error.clone();
    let (_, stats_changed) = storage::update_board_atomic(gcx.clone(), task_id, move |board| {
        if let Some(card) = board.get_card_mut(&card_id_owned) {
            if !force_merge_card_matches_current(
                card,
                expected_assignee.as_deref(),
                expected_agent_chat_id.as_deref(),
            ) {
                return Ok(false);
            }
            if clear_branch {
                card.agent_branch = None;
            }
            if clear_worktree {
                card.agent_worktree = None;
                card.agent_worktree_name = None;
            }
            if clear_ownership {
                if force_merge_auto_reverted
                    || retained_verification_failure_for_update.is_some()
                    || post_merge_check_error_for_update.is_some()
                {
                    let message = retained_verification_failure_for_update
                        .clone()
                        .or_else(|| post_merge_check_error_for_update.as_deref().map(|error| format!("Forced merge remains applied, but post-merge verification failed to run: {}. Stale worktree, branch, and agent ownership references were cleared; merge success was not inferred from verification.", error)))
                        .unwrap_or_else(|| "Forced merge was auto-reverted after post-merge verification failed; the original card was not marked done and stale worktree, branch, and agent ownership references were cleared.".to_string());
                    return Ok(retain_force_merge_without_finalizing(card, &message));
                }
                return Ok(finalize_force_merge_card_if_current(
                    card,
                    expected_assignee.as_deref(),
                    expected_agent_chat_id.as_deref(),
                ));
            }
        }
        Ok(false)
    })
    .await?;
    if stats_changed {
        storage::update_task_stats(gcx, task_id).await?;
    }
    Ok(())
}

pub async fn merge_registered_task_worktree(
    gcx: Arc<GlobalContext>,
    workspace_root: &std::path::Path,
    task_id: &str,
    card_id: &str,
    strategy: &str,
    tool_call_id: &str,
    commit_message_override: Option<String>,
    force: bool,
    bypass_verifier: bool,
    auto_revert: bool,
    auto_revert_timeout: Duration,
) -> Result<Option<(bool, Vec<ContextEnum>)>, String> {
    let board = storage::load_board(gcx.clone(), task_id).await?;
    let Some(card) = board.get_card(card_id) else {
        return Err(format!("Card {} not found", card_id));
    };
    let verifier_warning = ensure_verifier_allows_merge(card, force, bypass_verifier)?;
    let completion_warning = ensure_card_done_allows_merge(card, force)?;
    let expected_assignee = card.assignee.clone();
    let expected_agent_chat_id = card.agent_chat_id.clone();
    let Some(worktree_id) = card.agent_worktree_name.clone() else {
        return Ok(None);
    };
    let task_meta = storage::load_task_meta(gcx.clone(), task_id).await?;
    let cache_dir = gcx.cache_dir.clone();
    let service = WorktreeService::new_async(cache_dir, workspace_root.to_path_buf()).await?;
    let registered = worktree_id_is_valid_for_card(&service, &worktree_id, task_id, card).await?;
    let card_pair_present = card.base_branch.is_some() || card.base_commit.is_some();
    let registered_pair_present =
        registered.meta.base_branch.is_some() || registered.meta.base_commit.is_some();
    let (base_branch, base_commit) = if card_pair_present {
        (card.base_branch.clone(), card.base_commit.clone())
    } else if registered_pair_present {
        (
            registered.meta.base_branch.clone(),
            registered.meta.base_commit.clone(),
        )
    } else {
        (task_meta.base_branch, task_meta.base_commit)
    };
    let target_branch = base_branch.clone().ok_or("Task has no base branch set")?;
    let diff = refact_worktrees::git::diff_for_path(
        &registered.meta.root,
        base_commit.as_deref(),
        base_branch.as_deref(),
        1024 * 1024,
    )
    .ok();
    let changed_files = diff
        .as_ref()
        .map(|diff| {
            diff.files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let commit_message = match commit_message_override {
        Some(message) if !message.trim().is_empty() => Some(message),
        _ if changed_files.is_empty() => None,
        _ => {
            let diff_text = diff
                .as_ref()
                .map(|diff| diff.patch.clone())
                .unwrap_or_default();
            match refact_runtime_api::commit_message_runtime::generate_commit_message_by_diff(
                gcx.chat_ctx(gcx.clone()),
                &diff_text,
                &Some(card.title.clone()),
                None,
            )
            .await
            {
                Ok(message) if !message.trim().is_empty() => Some(message),
                _ => Some(format!("Card {}: {}", card_id, card.title)),
            }
        }
    };
    let post_merge_expected_state = if auto_revert {
        Some(load_expected_card_state_for_ctx(&gcx.chat_ctx(gcx.clone()), task_id, card_id).await?)
    } else {
        None
    };
    let response = service
        .merge_worktree(
            &worktree_id,
            MergeWorktreeRequest {
                strategy: strategy_from_str(strategy)?,
                delete_after_merge: true,
                include_uncommitted: true,
                target_branch: Some(target_branch),
                commit_message,
                generate_commit_message: false,
            },
        )
        .await?;
    let post_merge_check_result = match (response.merge_commit.as_ref(), post_merge_expected_state)
    {
        (Some(merge_commit), Some(expected_card_state)) if response.merged && auto_revert => {
            refact_runtime_api::post_merge_check::post_merge_check(
                gcx.clone(),
                refact_runtime_api::post_merge_check::PostMergeCheckRequest {
                    task_id: task_id.to_string(),
                    card_id: card_id.to_string(),
                    workspace_root: workspace_root.to_path_buf(),
                    enabled: true,
                    timeout: auto_revert_timeout,
                    expected_merge_commit: merge_commit.clone(),
                    expected_card_state: Some(expected_card_state),
                },
            )
            .await
            .map(Some)
        }
        _ => Ok(None),
    };
    let force_merge_auto_reverted = post_merge_check_result
        .as_ref()
        .ok()
        .and_then(|result| result.as_ref())
        .map(|result| result.auto_reverted)
        .unwrap_or(false);
    let finalize_force_merge = force_merge_post_check_passed_or_absent(&post_merge_check_result);
    let retained_verification_failure =
        retained_post_merge_verification_failure(&post_merge_check_result);
    let post_merge_check_error = post_merge_check_result.as_ref().err().cloned();
    clear_board_mirrors_after_registered_merge(
        gcx.clone(),
        task_id,
        card_id,
        &response,
        force,
        finalize_force_merge,
        force_merge_auto_reverted,
        retained_verification_failure,
        post_merge_check_error,
        expected_assignee,
        expected_agent_chat_id,
    )
    .await?;
    let post_merge_check = post_merge_check_result?;
    if response.merged && !force_merge_auto_reverted && !changed_files.is_empty() {
        let _ = refact_runtime_api::task_agent_monitor::append_card_target_files(
            gcx.chat_ctx(gcx.clone()),
            task_id,
            card_id,
            changed_files,
        )
        .await;
    }
    let mut message = merge_response_message(card_id, &response, post_merge_check.as_ref());
    append_verifier_warning(&mut message, verifier_warning.as_deref());
    append_completion_warning(&mut message, completion_warning.as_deref());
    Ok(Some((
        false,
        vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(message),
            tool_calls: None,
            tool_call_id: tool_call_id.to_string(),
            ..Default::default()
        })],
    )))
}

pub struct ToolTaskMergeAgent;

impl ToolTaskMergeAgent {
    pub fn new() -> Self {
        Self
    }
}

async fn run_git_in(cwd: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut command = tokio::process::Command::new("git");
    command.args(args).current_dir(cwd);
    let output = refact_worktrees::git::run_git_command(
        command,
        &format!("{:?}", args),
        refact_worktrees::git::GIT_COMMAND_TIMEOUT,
    )
    .await?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

#[async_trait]
impl Tool for ToolTaskMergeAgent {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "merge_agent".to_string(),
            display_name: "Task Merge Agent".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Merge an agent's work back to the main branch and always cleanup the worktree after a successful merge or no-op merged state. The agent must have completed work on a card with an associated git branch and worktree.".to_string(),
            input_schema: json_schema_from_params(&[("card_id", "string", "Card ID whose agent branch to merge"), ("strategy", "string", "Merge strategy: 'squash' (default) or 'merge'"), ("force", "boolean", "Bypass verifier human-review infrastructure failures only; genuine verification failures remain blocked"), ("bypass_verifier", "boolean", "Emergency bypass for all verifier checks, including genuine command failures"), ("auto_revert", "boolean", "Opt in to post-merge verification and automatic git revert on deterministic regression"), ("auto_revert_timeout_secs", "integer", "Timeout for post-merge verification and revert commands, default 300 seconds")], &["card_id"]),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let ccx_lock = ccx.lock().await;

        let is_planner = ccx_lock
            .task_meta
            .as_ref()
            .map(|m| m.role == "planner")
            .unwrap_or(false);

        if !is_planner {
            return Err("merge_agent can only be called by the task planner. \
                 Switch to the planner chat to merge agent work."
                .to_string());
        }

        let gcx = ccx_lock.app.gcx.clone();
        drop(ccx_lock);

        let task_id = require_bound_planner_task(&ccx, args).await?;

        let card_id = required_string_arg(args, "card_id")?;

        let strategy = strategy_arg(args)?;
        let force = parse_force(args)?;
        let bypass_verifier = parse_bypass_verifier(args)?;
        let auto_revert = parse_auto_revert(args)?;
        let auto_revert_timeout = parse_auto_revert_timeout(args)?;

        if strategy != "merge" && strategy != "squash" {
            return Err(format!(
                "Invalid strategy '{}', must be 'merge' or 'squash'",
                strategy
            ));
        }

        let project_dirs = refact_runtime_api::files_correction::get_project_dirs(gcx.clone()).await;
        let workspace_root = project_dirs.first().ok_or("No workspace folder found")?;

        let is_git_repo = {
            let mut command = tokio::process::Command::new("git");
            command
                .args(["rev-parse", "--is-inside-work-tree"])
                .current_dir(workspace_root);
            refact_worktrees::git::run_git_command(
                command,
                "rev-parse --is-inside-work-tree",
                refact_worktrees::git::GIT_COMMAND_TIMEOUT,
            )
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
        };
        if !is_git_repo {
            return Err("Workspace is not a git repository".to_string());
        }

        let board = storage::load_board(gcx.clone(), &task_id).await?;
        let card = board
            .get_card(card_id)
            .ok_or(format!("Card {} not found", card_id))?;
        let verifier_warning = ensure_verifier_allows_merge(card, force, bypass_verifier)?;
        let completion_warning = ensure_card_done_allows_merge(card, force)?;
        let expected_assignee = card.assignee.clone();
        let expected_agent_chat_id = card.agent_chat_id.clone();

        let task_meta = storage::load_task_meta(gcx.clone(), &task_id).await?;
        let base_branch = card
            .base_branch
            .as_ref()
            .or(task_meta.base_branch.as_ref())
            .ok_or("Task has no base branch set")?;
        let comparison_base = card.base_commit.as_deref().unwrap_or(base_branch);

        if card.agent_worktree_name.is_some() {
            match merge_registered_task_worktree(
                gcx.clone(),
                workspace_root,
                &task_id,
                card_id,
                strategy,
                tool_call_id,
                None,
                force,
                bypass_verifier,
                auto_revert,
                auto_revert_timeout,
            )
            .await
            {
                Ok(Some(result)) => return Ok(result),
                Ok(None) => {}
                Err(err) => return Err(err),
            }
        }

        let agent_branch = card
            .agent_branch
            .as_ref()
            .ok_or(format!("Card {} has no agent branch", card_id))?;
        let agent_worktree = card
            .agent_worktree
            .as_ref()
            .ok_or(format!("Card {} has no agent worktree", card_id))?;

        ensure_legacy_agent_worktree_checkout(workspace_root, agent_worktree, agent_branch)?;

        let merge_in_progress = run_git_in(
            workspace_root,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .await
        .is_ok();
        if merge_in_progress {
            let status = run_git_in(workspace_root, &["status", "--porcelain"])
                .await
                .unwrap_or_default();
            let conflict_files: Vec<String> = status
                .lines()
                .filter(|l| {
                    let bytes = l.as_bytes();
                    bytes.len() >= 2
                        && (bytes[0] == b'U'
                            || bytes[1] == b'U'
                            || (bytes[0] == b'A' && bytes[1] == b'A')
                            || (bytes[0] == b'D' && bytes[1] == b'D'))
                })
                .filter_map(|l| l.get(3..).map(|s| s.to_string()))
                .collect();

            let conflict_msg = format!(
                r#"# Merge Already In Progress

A previous merge is still in progress with unresolved conflicts.

## Conflicting Files
{}

## How to Resolve

### Option 1: Resolve conflicts
1. Use `cat <file>` to see conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`)
2. Use `update_textdoc` to resolve each conflict
3. Stage and commit: `git add -A && git commit -m "Resolved conflicts"`

### Option 2: Abort and retry
```
git merge --abort
```
Then call `merge_agent` again."#,
                if conflict_files.is_empty() {
                    "None detected (check `git status`)".to_string()
                } else {
                    conflict_files
                        .iter()
                        .map(|f| format!("- {}", f))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            );

            return Ok((
                false,
                vec![ContextEnum::ChatMessage(ChatMessage {
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(conflict_msg),
                    tool_calls: None,
                    tool_call_id: tool_call_id.clone(),
                    ..Default::default()
                })],
            ));
        }

        let main_status = run_git_in(workspace_root, &["status", "--porcelain"])
            .await
            .unwrap_or_default();
        if !main_status.trim().is_empty() {
            return Err(
                "Main workspace has uncommitted changes. Please commit or stash before merging."
                    .to_string(),
            );
        }

        let _auto_committed_legacy_worktree = refact_worktrees::git::commit_all(
            std::path::Path::new(agent_worktree),
            &format!("Card {}: {}", card_id, card.title),
        )?;

        let changed_files = refact_worktrees::git::git_diff_name_only(
            std::path::Path::new(agent_worktree),
            comparison_base,
            agent_branch,
        );

        let commits_ahead_result = run_git_in(
            workspace_root,
            &[
                "rev-list",
                "--count",
                &format!("{}..{}", comparison_base, agent_branch),
            ],
        )
        .await;
        let commits_ahead = match commits_ahead_result {
            Ok(output) => output
                .trim()
                .parse::<u32>()
                .map_err(|e| format!("Failed to parse commits ahead count: {}", e))?,
            Err(e) => {
                return Err(format!(
                    "Failed to count commits ahead (base: {}, agent: {}): {}. \
                    Check that both branches exist and are valid.",
                    comparison_base, agent_branch, e
                ));
            }
        };

        if commits_ahead == 0 {
            let worktree_status = if let Some(wt) = card.agent_worktree.as_ref() {
                let mut command = tokio::process::Command::new("git");
                command.args(["status", "--porcelain"]).current_dir(wt);
                refact_worktrees::git::run_git_command(
                    command,
                    "status --porcelain",
                    refact_worktrees::git::GIT_COMMAND_TIMEOUT,
                )
                .await
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
            } else {
                String::new()
            };

            let main_status = run_git_in(workspace_root, &["status", "--porcelain"])
                .await
                .unwrap_or_default();
            let main_dirty = !main_status.trim().is_empty();
            let worktree_dirty = !worktree_status.is_empty();

            let diagnostic = if main_dirty && !worktree_dirty {
                "Main workspace has uncommitted changes but agent worktree is clean. Agent likely edited files in the wrong directory."
            } else if worktree_dirty {
                "Agent worktree has uncommitted changes. The worktree and branch are preserved; inspect the worktree or use restart_agent(mode=resume)."
            } else {
                "Both main workspace and agent worktree are clean. Agent may not have made any changes."
            };

            let mut cleanup_status = Vec::new();
            let mut worktree_removed = false;
            let mut branch_deleted = false;
            if worktree_dirty {
                cleanup_status.push(
                    "cleanup skipped; dirty agent worktree preserved for inspection or restart_agent(mode=resume)",
                );
            } else if agent_branch != base_branch {
                let _guard = git_merge_lock().lock().await;
                worktree_removed = run_git_in(
                    workspace_root,
                    &["worktree", "remove", agent_worktree, "--force"],
                )
                .await
                .is_ok();
                branch_deleted = run_git_in(workspace_root, &["branch", "-D", agent_branch])
                    .await
                    .is_ok();
                drop(_guard);

                if worktree_removed {
                    cleanup_status.push("worktree removed");
                }
                if branch_deleted {
                    cleanup_status.push("branch deleted");
                }
            }

            if worktree_removed || branch_deleted || force {
                let card_id_owned = card_id.to_string();
                let clear_worktree = worktree_removed;
                let clear_branch = branch_deleted;
                let expected_assignee = expected_assignee.clone();
                let expected_agent_chat_id = expected_agent_chat_id.clone();
                let (_, stats_changed) =
                    storage::update_board_atomic(gcx.clone(), &task_id, move |board| {
                        let Some(card) = board.get_card_mut(&card_id_owned) else {
                            return Ok(false);
                        };
                        if !force_merge_card_matches_current(
                            card,
                            expected_assignee.as_deref(),
                            expected_agent_chat_id.as_deref(),
                        ) {
                            return Ok(false);
                        }
                        if clear_branch {
                            card.agent_branch = None;
                        }
                        if clear_worktree {
                            card.agent_worktree = None;
                            card.agent_worktree_name = None;
                        }
                        Ok(force
                            && finalize_force_merge_card_if_current(
                                card,
                                expected_assignee.as_deref(),
                                expected_agent_chat_id.as_deref(),
                            ))
                    })
                    .await?;
                if stats_changed {
                    storage::update_task_stats(gcx.clone(), &task_id).await?;
                }
            }

            let cleanup_msg = if cleanup_status.is_empty() {
                "No cleanup performed.".to_string()
            } else {
                format!("Cleanup: {}.", cleanup_status.join(", "))
            };

            let mut message = format!(
                "# Nothing to Merge\n\n**Card:** {}\n**Branch:** {}\n**Commits ahead of base:** 0\n\n**Diagnostic:** {}\n\n{}",
                card_id, agent_branch, diagnostic, cleanup_msg
            );
            append_verifier_warning(&mut message, verifier_warning.as_deref());

            return Ok((
                false,
                vec![ContextEnum::ChatMessage(ChatMessage {
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(message),
                    tool_calls: None,
                    tool_call_id: tool_call_id.clone(),
                    ..Default::default()
                })],
            ));
        }

        // Generate commit message before acquiring the lock: git diff base...agent is read-only
        // and produces the same content as git diff --cached after a squash merge.
        let diff = run_git_in(
            workspace_root,
            &["diff", &format!("{}...{}", comparison_base, agent_branch)],
        )
        .await
        .unwrap_or_default();
        let commit_msg =
            match refact_runtime_api::commit_message_runtime::generate_commit_message_by_diff(
                gcx.chat_ctx(gcx.clone()),
                &diff,
                &Some(card.title.clone()),
                None,
            )
            .await
            {
                Ok(msg) if !msg.trim().is_empty() => msg,
                _ => format!("Card {}: {}", card_id, card.title),
            };

        let post_merge_expected_state = if auto_revert {
            Some(load_expected_card_state_for_ctx(&gcx.chat_ctx(gcx.clone()), &task_id, card_id).await?)
        } else {
            None
        };

        let _guard = git_merge_lock().lock().await;

        run_git_in(workspace_root, &["checkout", base_branch])
            .await
            .map_err(|e| format!("Failed to checkout base branch: {}", e))?;

        let merge_result = if strategy == "squash" {
            run_git_in(workspace_root, &["merge", "--squash", agent_branch]).await
        } else {
            refact_worktrees::git::run_git_with_refact_author(
                workspace_root,
                &[
                    "merge",
                    "--no-ff",
                    "--no-gpg-sign",
                    agent_branch,
                    "-m",
                    &commit_msg,
                ],
            )
        };

        if let Err(e) = merge_result {
            let status = run_git_in(workspace_root, &["status", "--porcelain"])
                .await
                .unwrap_or_default();
            let is_conflict_line = |l: &str| {
                let bytes = l.as_bytes();
                bytes.len() >= 2
                    && (bytes[0] == b'U'
                        || bytes[1] == b'U'
                        || (bytes[0] == b'A' && bytes[1] == b'A')
                        || (bytes[0] == b'D' && bytes[1] == b'D'))
            };
            let has_conflicts = status.lines().any(is_conflict_line);

            if has_conflicts {
                let conflict_files: Vec<String> = status
                    .lines()
                    .filter(|l| is_conflict_line(l))
                    .filter_map(|l| l.get(3..).map(|s| s.to_string()))
                    .collect();

                let conflict_msg = format!(
                    r#"# Merge Conflicts Detected

**Card:** {}
**Branch:** {} → {}
**Strategy:** {}

## Conflicting Files
{}

## Current State
The merge is **in progress** with conflict markers in the files above.

## How to Resolve

### Option 1: Resolve manually
1. Open each conflicting file
2. Look for conflict markers: `<<<<<<<`, `=======`, `>>>>>>>`
3. Edit to keep the correct code (remove markers)
4. Stage resolved files: `git add <file>`
5. Complete merge: `git commit -m "Resolved conflicts from {}"`

### Option 2: Accept one side entirely
- Keep base branch version: `git checkout --ours <file>`
- Keep agent branch version: `git checkout --theirs <file>`
- Then: `git add <file>` and `git commit`

### Option 3: Abort and retry
```
git merge --abort
```
Then investigate why conflicts occurred and create a fix card.

## Conflict Details
Use `cat <file>` to see conflict markers in each file."#,
                    card_id,
                    agent_branch,
                    base_branch,
                    strategy,
                    conflict_files
                        .iter()
                        .map(|f| format!("- {}", f))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    agent_branch
                );

                return Ok((
                    false,
                    vec![ContextEnum::ChatMessage(ChatMessage {
                        role: "tool".to_string(),
                        content: ChatContent::SimpleText(conflict_msg),
                        tool_calls: None,
                        tool_call_id: tool_call_id.clone(),
                        ..Default::default()
                    })],
                ));
            }
            return Err(format!("Merge failed: {}", e));
        }

        let mut squash_nothing_to_commit = false;
        if strategy == "squash" {
            let commit_result = refact_worktrees::git::run_git_with_refact_author(
                workspace_root,
                &["commit", "--no-gpg-sign", "-m", &commit_msg],
            );
            if let Err(e) = commit_result {
                if !e.contains("nothing to commit") {
                    return Err(format!("Failed to commit squash merge: {}", e));
                }
                squash_nothing_to_commit = true;
            }
        }

        let merge_commit = if squash_nothing_to_commit {
            None
        } else {
            run_git_in(workspace_root, &["rev-parse", "HEAD"])
                .await
                .map(|head| head.trim().to_string())
                .ok()
                .filter(|head| !head.is_empty())
        };

        let agent_worktree_dirty = if agent_branch != base_branch {
            let mut status_command = tokio::process::Command::new("git");
            status_command
                .args(["status", "--porcelain"])
                .current_dir(agent_worktree);
            match refact_worktrees::git::run_git_command(
                status_command,
                "status --porcelain",
                refact_worktrees::git::GIT_COMMAND_TIMEOUT,
            )
            .await
            {
                Ok(output) if output.status.success() => {
                    !String::from_utf8_lossy(&output.stdout).trim().is_empty()
                }
                _ => true,
            }
        } else {
            false
        };

        let (worktree_removed, branch_deleted) =
            if agent_branch != base_branch && !agent_worktree_dirty {
                let wr = run_git_in(
                    workspace_root,
                    &["worktree", "remove", agent_worktree, "--force"],
                )
                .await
                .is_ok();
                let bd = run_git_in(workspace_root, &["branch", "-D", agent_branch])
                    .await
                    .is_ok();
                (wr, bd)
            } else {
                (false, false)
            };

        drop(_guard);

        let post_merge_check_result = if auto_revert {
            match merge_commit {
                Some(merge_commit) => {
                    let expected_card_state = post_merge_expected_state
                        .expect("post-merge state is captured whenever auto-revert is enabled");
                    refact_runtime_api::post_merge_check::post_merge_check(
                        gcx.clone(),
                        refact_runtime_api::post_merge_check::PostMergeCheckRequest {
                            task_id: task_id.clone(),
                            card_id: card_id.to_string(),
                            workspace_root: workspace_root.to_path_buf(),
                            enabled: true,
                            timeout: auto_revert_timeout,
                            expected_merge_commit: merge_commit,
                            expected_card_state: Some(expected_card_state),
                        },
                    )
                    .await
                    .map(Some)
                }
                None => Ok(None),
            }
        } else {
            Ok(None)
        };
        let merge_auto_reverted = post_merge_check_result
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref())
            .map(|result| result.auto_reverted)
            .unwrap_or(false);
        let post_merge_check_error = post_merge_check_result.as_ref().err().cloned();
        let retained_verification_failure =
            retained_post_merge_verification_failure(&post_merge_check_result);

        if !merge_auto_reverted && !squash_nothing_to_commit {
            let _ = refact_runtime_api::task_agent_monitor::append_card_target_files(
                gcx.chat_ctx(gcx.clone()),
                &task_id,
                card_id,
                changed_files,
            )
            .await;
        }

        if worktree_removed || branch_deleted || force {
            let card_id_owned = card_id.to_string();
            let clear_worktree = worktree_removed;
            let clear_branch = branch_deleted;
            let expected_assignee = expected_assignee.clone();
            let expected_agent_chat_id = expected_agent_chat_id.clone();
            let finalize_force = force_merge_post_check_passed_or_absent(&post_merge_check_result);
            let (_, stats_changed) =
                storage::update_board_atomic(gcx.clone(), &task_id, move |board| {
                    let Some(card) = board.get_card_mut(&card_id_owned) else {
                        return Ok(false);
                    };
                    if !force_merge_card_matches_current(
                        card,
                        expected_assignee.as_deref(),
                        expected_agent_chat_id.as_deref(),
                    ) {
                        return Ok(false);
                    }
                    if clear_branch {
                        card.agent_branch = None;
                    }
                    if clear_worktree {
                        card.agent_worktree = None;
                        card.agent_worktree_name = None;
                    }
                    if force && merge_auto_reverted {
                        card.assignee = None;
                        card.agent_chat_id = None;
                        card.status_updates.push(refact_tasks::types::StatusUpdate {
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            message: "Forced merge was auto-reverted after post-merge verification failed; the original card was not marked done and stale worktree, branch, and agent ownership references were cleared.".to_string(),
                        });
                        return Ok(true);
                    }
                    if force {
                        if let Some(message) = retained_verification_failure.as_deref() {
                            return Ok(retain_force_merge_without_finalizing(card, message));
                        }
                        if let Some(error) = post_merge_check_error.as_deref() {
                            card.assignee = None;
                            card.agent_chat_id = None;
                            card.status_updates.push(refact_tasks::types::StatusUpdate {
                                timestamp: chrono::Utc::now().to_rfc3339(),
                                message: format!("Forced merge remains applied, but post-merge verification failed to run: {}. Stale worktree, branch, and agent ownership references were cleared; merge success was not inferred from verification.", error),
                            });
                            return Ok(true);
                        }
                    }
                    Ok(force
                        && finalize_force
                        && finalize_force_merge_card_if_current(
                            card,
                            expected_assignee.as_deref(),
                            expected_agent_chat_id.as_deref(),
                        ))
                })
                .await?;
            if stats_changed {
                storage::update_task_stats(gcx.clone(), &task_id).await?;
            }
        }

        let post_merge_check = post_merge_check_result?;

        let cleanup_info = if agent_branch != base_branch {
            if agent_worktree_dirty {
                "Worktree retained (uncommitted changes detected; use restart_agent or inspect manually)"
                    .to_string()
            } else {
                match (worktree_removed, branch_deleted) {
                    (true, true) => "Worktree and branch cleaned up.".to_string(),
                    (true, false) => "Worktree removed, branch deletion failed.".to_string(),
                    (false, true) => "Worktree removal failed, branch deleted.".to_string(),
                    (false, false) => {
                        "Cleanup failed (worktree and branch still exist).".to_string()
                    }
                }
            }
        } else {
            "Cleanup skipped because agent branch matches base branch.".to_string()
        };

        let mut result_message = if squash_nothing_to_commit {
            format!(
                "# Nothing to Merge\n\n**Card:** {}\n**Branch:** {} → {}\n\n**Cleanup:** {}",
                card_id, agent_branch, base_branch, cleanup_info
            )
        } else {
            format!(
                r#"# Agent Work Merged

**Card:** {}
**Strategy:** {}
**Branch:** {} → {}
**Cleanup:** {}

The agent's work has been successfully merged back to the main branch."#,
                card_id, strategy, agent_branch, base_branch, cleanup_info
            )
        };
        append_verifier_warning(&mut result_message, verifier_warning.as_deref());
        append_completion_warning(&mut result_message, completion_warning.as_deref());
        if let Some(post_merge_check) = post_merge_check.as_ref() {
            append_post_merge_check_message(&mut result_message, post_merge_check);
        }

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result_message),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

