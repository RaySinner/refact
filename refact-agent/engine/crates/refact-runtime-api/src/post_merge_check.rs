use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use tokio::process::Command;

use refact_chat_api::verifier::ExpectedCardState;
use refact_chat_api::verify_cmd::{parse_verification_argv, verification_commands};
use refact_tasks::types::{StatusUpdate, VerificationOutcome};

use crate::chat_ctx::ChatCtx;
use crate::global_context::{GlobalContext, GlobalContextCtx};
use crate::task_storage;

pub use refact_chat_api::post_merge_check::*;
pub use refact_chat_api::tools_facade::DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS;

const STALE_POST_MERGE_WRITE: &str = "stale post-merge write";

pub(crate) async fn store_regression_result(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    expected_card_state: Option<&ExpectedCardState>,
    command: &str,
    output_tail: &str,
    merge_commit: &str,
    revert_commit: &str,
    first_error: &str,
    revert_skipped_reason: Option<&str>,
    diagnostic: Option<&str>,
) -> Result<Option<String>, String> {
    let card_id_owned = card_id.to_string();
    let task_id_owned = task_id.to_string();
    let expected_card_state = expected_card_state.cloned();
    let command_owned = command.to_string();
    let output_owned = output_tail.to_string();
    let merge_commit_owned = merge_commit.to_string();
    let revert_commit_owned = revert_commit.to_string();
    let first_error_owned = first_error.to_string();
    let revert_skipped_reason_owned = revert_skipped_reason.map(str::to_string);
    let diagnostic_owned = diagnostic.map(str::to_string);
    let result = task_storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        let Some(source_card_ref) = board.get_card(&card_id_owned) else {
            if expected_card_state.is_some() {
                return Err(format!(
                    "{} for task {} card {}: card no longer exists",
                    STALE_POST_MERGE_WRITE, task_id_owned, card_id_owned
                ));
            }
            return Err(format!("Card {} not found", card_id_owned));
        };
        if let Some(expected) = expected_card_state.as_ref() {
            if !expected.matches_board_card(board.rev, source_card_ref) {
                return Err(format!(
                    "{} for task {} card {}: current card state no longer matches post-merge request",
                    STALE_POST_MERGE_WRITE, task_id_owned, card_id_owned
                ));
            }
        }
        let source_card = source_card_ref.clone();
        ensure_regressed_column(board);
        let fix_card_id = next_fix_card_id(board, &card_id_owned);
        let now = Utc::now().to_rfc3339();
        let original = board
            .get_card_mut(&card_id_owned)
            .ok_or_else(|| format!("Card {} not found", card_id_owned))?;
        original.column = "regressed".to_string();
        original.status_updates.push(StatusUpdate {
            timestamp: now.clone(),
            message: match revert_skipped_reason_owned.as_deref() {
                Some(reason) => format!("Auto-revert skipped: {}", reason),
                None => format!("Auto-reverted: {}", first_error_owned),
            },
        });
        board.cards.push(build_fix_card(
            &source_card,
            &fix_card_id,
            &command_owned,
            &output_owned,
            &merge_commit_owned,
            &revert_commit_owned,
            revert_skipped_reason_owned.as_deref(),
            diagnostic_owned.as_deref(),
            &now,
        ));
        Ok(fix_card_id)
    })
    .await;
    let fix_card_id = match result {
        Ok((_, fix_card_id)) => fix_card_id,
        Err(error) if error.starts_with(STALE_POST_MERGE_WRITE) => {
            tracing::info!("{}", error);
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let _ = task_storage::update_task_stats_for_ctx(ctx, task_id).await;
    Ok(Some(fix_card_id))
}

#[async_trait]
pub trait PostMergeCommandRunner: Send {
    async fn run(
        &mut self,
        workspace_root: &Path,
        command: PostMergeCommand,
        timeout: Duration,
    ) -> PostMergeCommandOutput;
}

pub(crate) struct SystemPostMergeCommandRunner {
    ctx: ChatCtx,
}

#[async_trait]
impl PostMergeCommandRunner for SystemPostMergeCommandRunner {
    async fn run(
        &mut self,
        workspace_root: &Path,
        command: PostMergeCommand,
        timeout: Duration,
    ) -> PostMergeCommandOutput {
        match command {
            PostMergeCommand::Verify { cwd, env, argv } => {
                let label = argv.join(" ");
                if argv.is_empty() {
                    return PostMergeCommandOutput {
                        exit_code: None,
                        success: false,
                        output: "empty verification command".to_string(),
                        outcome: VerificationOutcome::Rejected,
                    };
                }
                let result = self
                    .ctx
                    .card_verifier
                    .run_verification_argv(
                        &self.ctx,
                        workspace_root,
                        &label,
                        cwd,
                        env,
                        argv,
                        timeout,
                        Duration::from_secs(10),
                    )
                    .await;
                PostMergeCommandOutput {
                    exit_code: result.exit_code,
                    success: result.passed,
                    output: result.output_tail,
                    outcome: result.outcome,
                }
            }
            PostMergeCommand::Git(args) => {
                let label = format!("git {}", args.join(" "));
                let mut cmd = Command::new("git");
                cmd.args(&args).current_dir(workspace_root);
                run_command(cmd, &label, timeout).await
            }
        }
    }
}

pub async fn run_command(
    mut command: Command,
    command_label: &str,
    timeout: Duration,
) -> PostMergeCommandOutput {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return PostMergeCommandOutput {
                exit_code: None,
                success: false,
                output: format!("failed to execute command '{}': {}", command_label, error),
                outcome: VerificationOutcome::InfrastructureFailed,
            };
        }
    };
    #[cfg(unix)]
    let child_id = child.id();
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            PostMergeCommandOutput {
                exit_code: output.status.code(),
                success: output.status.success(),
                output: text,
                outcome: if output.status.success() {
                    VerificationOutcome::Passed
                } else {
                    VerificationOutcome::CommandFailed
                },
            }
        }
        Ok(Err(error)) => PostMergeCommandOutput {
            exit_code: None,
            success: false,
            output: format!("failed to execute command '{}': {}", command_label, error),
            outcome: VerificationOutcome::InfrastructureFailed,
        },
        Err(_) => {
            #[cfg(unix)]
            if let Some(child_id) = child_id {
                unsafe {
                    libc::kill(-(child_id as i32), libc::SIGKILL);
                }
            }
            PostMergeCommandOutput {
                exit_code: None,
                success: false,
                output: format!(
                    "command '{}' timed out after {} seconds",
                    command_label,
                    timeout.as_secs()
                ),
                outcome: VerificationOutcome::InfrastructureFailed,
            }
        }
    }
}

pub async fn post_merge_check(
    gcx: Arc<GlobalContext>,
    request: PostMergeCheckRequest,
) -> Result<PostMergeCheckResult, String> {
    post_merge_check_for_ctx(&gcx.chat_ctx(gcx.clone()), request).await
}

pub async fn post_merge_check_for_ctx(
    ctx: &ChatCtx,
    request: PostMergeCheckRequest,
) -> Result<PostMergeCheckResult, String> {
    let mut runner = SystemPostMergeCommandRunner { ctx: ctx.clone() };
    post_merge_check_with_runner(ctx, request, &mut runner).await
}

pub async fn post_merge_check_with_runner<R: PostMergeCommandRunner>(
    ctx: &ChatCtx,
    request: PostMergeCheckRequest,
    runner: &mut R,
) -> Result<PostMergeCheckResult, String> {
    if !request.enabled {
        return Ok(skipped_result("auto_revert disabled"));
    }

    let board = task_storage::load_board_for_ctx(ctx, &request.task_id).await?;
    let Some(card) = board.get_card(&request.card_id) else {
        if request.expected_card_state.is_some() {
            return Ok(skipped_result("stale card state"));
        }
        return Err(format!("Card {} not found", request.card_id));
    };
    if let Some(expected) = request.expected_card_state.as_ref() {
        if !expected.matches_board_card(board.rev, card) {
            return Ok(skipped_result("stale card state"));
        }
    }
    let card = card.clone();
    let commands = verification_commands(&card);
    if commands.is_empty() {
        return Ok(failed_without_revert(
            None,
            VerificationOutcome::NoCommands,
            "no verification command found".to_string(),
        ));
    }
    let command_policy = ctx.card_verifier.verification_command_policy(ctx).await;
    let mut parsed = Vec::new();
    for command in commands {
        match parse_verification_argv(&command) {
            Ok(verification) => {
                if let Err(reason) = command_policy.check(&command) {
                    return Ok(failed_without_revert(
                        Some(command),
                        VerificationOutcome::PolicyDenied,
                        format!("Denied by shell policy: {}", reason),
                    ));
                }
                parsed.push((command, verification));
            }
            Err(reason) => {
                return Ok(failed_without_revert(
                    Some(command),
                    VerificationOutcome::Rejected,
                    format!("Cannot run as a command: {}", reason),
                ));
            }
        }
    }
    let mut command = String::new();
    let mut verification = PostMergeCommandOutput {
        exit_code: Some(0),
        success: true,
        output: String::new(),
        outcome: VerificationOutcome::Passed,
    };
    for (next_command, verified) in parsed {
        command = next_command;
        verification = runner
            .run(
                &request.workspace_root,
                PostMergeCommand::Verify {
                    cwd: verified.cwd,
                    env: verified.env,
                    argv: verified.argv,
                },
                request.timeout,
            )
            .await;
        if !verification.success {
            break;
        }
    }
    let exit_code = verification.exit_code;
    let verification_success = verification.success;
    let output_tail = tail_chars(&verification.output, MAX_OUTPUT_TAIL_CHARS);
    let verification_outcome = verification.outcome;
    if verification_success {
        return Ok(PostMergeCheckResult {
            checked: true,
            auto_reverted: false,
            command: Some(command),
            exit_code,
            output_tail,
            merge_commit: None,
            revert_commit: None,
            fix_card_id: None,
            skipped_reason: None,
            revert_skipped_reason: None,
            outcome: VerificationOutcome::Passed,
        });
    }
    if verification_outcome != VerificationOutcome::CommandFailed {
        return Ok(failed_without_revert(
            Some(command),
            verification_outcome,
            output_tail,
        ));
    }

    let expected_merge_commit = request.expected_merge_commit.trim().to_string();
    if expected_merge_commit.is_empty() {
        return Ok(skipped_result("missing expected merge commit"));
    }

    let current_head = git_text(
        runner
            .run(
                &request.workspace_root,
                PostMergeCommand::Git(vec!["rev-parse".to_string(), "HEAD".to_string()]),
                request.timeout,
            )
            .await,
    );
    let Some(current_head) = current_head else {
        let reason = "unable to verify HEAD before revert";
        let diagnostic = format!("Expected merge commit: {}", expected_merge_commit);
        let fix_card_id = store_regression_result(
            ctx,
            &request.task_id,
            &request.card_id,
            request.expected_card_state.as_ref(),
            &command,
            &output_tail,
            &expected_merge_commit,
            "not reverted",
            &first_error_line(&output_tail),
            Some(reason),
            Some(&diagnostic),
        )
        .await?;
        return Ok(PostMergeCheckResult {
            checked: true,
            auto_reverted: false,
            command: Some(command),
            exit_code,
            output_tail,
            merge_commit: Some(expected_merge_commit),
            revert_commit: None,
            fix_card_id,
            skipped_reason: None,
            revert_skipped_reason: Some(reason.to_string()),
            outcome: VerificationOutcome::CommandFailed,
        });
    };
    if current_head != expected_merge_commit {
        let reason = "HEAD changed since merge";
        let diagnostic = format!(
            "Expected merge commit: {}\nCurrent HEAD: {}",
            expected_merge_commit, current_head
        );
        let fix_card_id = store_regression_result(
            ctx,
            &request.task_id,
            &request.card_id,
            request.expected_card_state.as_ref(),
            &command,
            &output_tail,
            &expected_merge_commit,
            "not reverted",
            &first_error_line(&output_tail),
            Some(reason),
            Some(&diagnostic),
        )
        .await?;
        return Ok(PostMergeCheckResult {
            checked: true,
            auto_reverted: false,
            command: Some(command),
            exit_code,
            output_tail,
            merge_commit: Some(expected_merge_commit),
            revert_commit: None,
            fix_card_id,
            skipped_reason: None,
            revert_skipped_reason: Some(reason.to_string()),
            outcome: VerificationOutcome::CommandFailed,
        });
    }

    let status_output = runner
        .run(
            &request.workspace_root,
            PostMergeCommand::Git(vec!["status".to_string(), "--porcelain".to_string()]),
            request.timeout,
        )
        .await;
    if !status_output.success || !status_output.output.trim().is_empty() {
        let reason = if status_output.success {
            "workspace dirty since merge"
        } else {
            "unable to verify clean workspace before revert"
        };
        let diagnostic = format!(
            "Expected merge commit: {}\nCurrent HEAD: {}\nGit status --porcelain:\n{}",
            expected_merge_commit,
            current_head,
            status_output.output.trim()
        );
        let fix_card_id = store_regression_result(
            ctx,
            &request.task_id,
            &request.card_id,
            request.expected_card_state.as_ref(),
            &command,
            &output_tail,
            &expected_merge_commit,
            "not reverted",
            &first_error_line(&output_tail),
            Some(reason),
            Some(&diagnostic),
        )
        .await?;
        return Ok(PostMergeCheckResult {
            checked: true,
            auto_reverted: false,
            command: Some(command),
            exit_code,
            output_tail,
            merge_commit: Some(expected_merge_commit),
            revert_commit: None,
            fix_card_id,
            skipped_reason: None,
            revert_skipped_reason: Some(reason.to_string()),
            outcome: VerificationOutcome::CommandFailed,
        });
    }

    let parents = runner
        .run(
            &request.workspace_root,
            PostMergeCommand::Git(vec![
                "rev-list".to_string(),
                "--parents".to_string(),
                "-n".to_string(),
                "1".to_string(),
                expected_merge_commit.clone(),
            ]),
            request.timeout,
        )
        .await;
    let parent_count = if parents.success {
        parents.output.split_whitespace().count().checked_sub(1)
    } else {
        None
    };
    let Some(parent_count) = parent_count else {
        return Ok(failed_without_revert(
            Some(command),
            VerificationOutcome::InfrastructureFailed,
            format!(
                "unable to determine merge commit parents: {}",
                parents.output.trim()
            ),
        ));
    };
    let mut revert_args = vec!["revert".to_string()];
    if parent_count > 1 {
        revert_args.extend(["-m".to_string(), "1".to_string()]);
    }
    revert_args.extend(["--no-edit".to_string(), expected_merge_commit.clone()]);
    let revert_output = runner
        .run(
            &request.workspace_root,
            PostMergeCommand::Git(revert_args),
            request.timeout,
        )
        .await;
    if !revert_output.success {
        let reason = "git revert failed";
        let diagnostic = format!(
            "Expected merge commit: {}\nCurrent HEAD: {}\nGit revert output:\n{}",
            expected_merge_commit,
            current_head,
            revert_output.output.trim()
        );
        store_regression_result(
            ctx,
            &request.task_id,
            &request.card_id,
            request.expected_card_state.as_ref(),
            &command,
            &output_tail,
            &expected_merge_commit,
            "not reverted",
            &first_error_line(&output_tail),
            Some(reason),
            Some(&diagnostic),
        )
        .await?;
        return Err(format!(
            "post-merge verification failed, and git revert failed: {}",
            first_error_line(&revert_output.output)
        ));
    }
    let revert_commit = git_text(
        runner
            .run(
                &request.workspace_root,
                PostMergeCommand::Git(vec!["rev-parse".to_string(), "HEAD".to_string()]),
                request.timeout,
            )
            .await,
    );
    let merge_commit = Some(expected_merge_commit);
    let first_error = first_error_line(&output_tail);
    let fix_card_id = store_regression_result(
        ctx,
        &request.task_id,
        &request.card_id,
        request.expected_card_state.as_ref(),
        &command,
        &output_tail,
        merge_commit.as_deref().unwrap_or("unknown"),
        revert_commit.as_deref().unwrap_or("unknown"),
        &first_error,
        None,
        None,
    )
    .await?;

    Ok(PostMergeCheckResult {
        checked: true,
        auto_reverted: true,
        command: Some(command),
        exit_code,
        output_tail,
        merge_commit,
        revert_commit,
        fix_card_id,
        skipped_reason: None,
        revert_skipped_reason: None,
        outcome: VerificationOutcome::CommandFailed,
    })
}
