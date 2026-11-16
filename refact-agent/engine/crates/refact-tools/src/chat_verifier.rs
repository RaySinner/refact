use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;

use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_runtime_api::GlobalContextCtx;
use refact_runtime_api::chat_ctx::ChatCtx;
use refact_worktrees::verifier_diff::{git_changed_files_summary, resolve_verifier_diff_base};
pub use refact_chat_api::verify_cmd::{
    parse_verification_argv, verification_commands, VerifyCommandPolicy,
};
use refact_exec::command_policy::{CommandKind, CommandPolicyInput, ExecSource};
use refact_runtime_api::command_policy::build_exec_request_for_ctx;
use refact_exec::{ExecOutputStream, ExecStatus};
use refact_runtime_api::global_context::GlobalContext;
use refact_worktrees::service::WorktreeService;
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{
    BoardCard, StatusUpdate, VerificationOutcome, VerificationResult, VerifierReport,
    VerifierReportClassification,
};

pub use refact_chat_api::verifier::{
    outcome_is_infrastructure, ExpectedCardState, VerifyCardRequest,
};
pub use refact_runtime_api::verifier::{
    load_expected_card_state_for_ctx, schedule_card_verifier_after_finish_for_ctx,
    verify_card_for_ctx,
};

const VERIFY_TIMEOUT: Duration = Duration::from_secs(600);
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_TAIL_CHARS: usize = 4000;
const MAX_OUTPUT_CAPTURE_BYTES: usize = 512 * 1024;
const MAX_DIFF_LINES: usize = 200;
const VERIFIER_SOURCE: &str = "chat.verifier";
const STALE_VERIFIER_WRITE: &str = "stale verifier write";


pub async fn load_expected_card_state(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    card_id: &str,
) -> Result<ExpectedCardState, String> {
    load_expected_card_state_for_ctx(&gcx.chat_ctx(gcx.clone()), task_id, card_id).await
}

pub async fn load_expected_card_state_for_ctx_impl(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
) -> Result<ExpectedCardState, String> {
    let board = storage::load_board_for_ctx(ctx, task_id).await?;
    let card = board
        .get_card(card_id)
        .ok_or_else(|| format!("Card {} not found", card_id))?;
    Ok(ExpectedCardState::from_card(board.rev, card))
}

fn stale_verifier_error(task_id: &str, card_id: &str) -> String {
    format!(
        "{} for task {} card {}: current card state no longer matches the verifier request",
        STALE_VERIFIER_WRITE, task_id, card_id
    )
}

fn is_stale_verifier_error(error: &str) -> bool {
    error.starts_with(STALE_VERIFIER_WRITE)
}

#[async_trait]
pub trait VerificationCommandRunner: Send {
    async fn run(
        &mut self,
        worktree: &Path,
        command: &str,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
        argv: Vec<String>,
    ) -> VerificationResult;
}

struct SystemVerificationCommandRunner {
    ctx: ChatCtx,
}

#[async_trait]
impl VerificationCommandRunner for SystemVerificationCommandRunner {
    async fn run(
        &mut self,
        worktree: &Path,
        command: &str,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
        argv: Vec<String>,
    ) -> VerificationResult {
        run_verification_argv(&self.ctx, worktree, command, cwd, env, argv).await
    }
}

async fn check_cwd_in_worktree(worktree: &Path, effective_cwd: &Path) -> Result<(), String> {
    let canonical_worktree = tokio::fs::canonicalize(worktree)
        .await
        .map_err(|e| format!("cannot access worktree '{}': {}", worktree.display(), e))?;
    if let Ok(canonical_cwd) = tokio::fs::canonicalize(effective_cwd).await {
        if !canonical_cwd.starts_with(&canonical_worktree) {
            return Err(format!(
                "cwd '{}' is outside the worktree",
                effective_cwd.display()
            ));
        }
    }
    Ok(())
}

pub fn append_verifier_status(card: &mut BoardCard, report: &VerifierReport) {
    let message = if report.passed {
        "Verifier: PASS".to_string()
    } else {
        let first = report
            .concerns
            .first()
            .map(|s| s.as_str())
            .unwrap_or("verification failed");
        format!("Verifier: FAIL — {}", first)
    };
    card.status_updates.push(StatusUpdate {
        timestamp: Utc::now().to_rfc3339(),
        message,
    });
}

pub async fn store_verifier_report(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    expected_state: Option<&ExpectedCardState>,
    report: VerifierReport,
) -> Result<bool, String> {
    let card_id = card_id.to_string();
    let task_id_owned = task_id.to_string();
    let expected_state = expected_state.cloned();
    match storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        let board_rev = board.rev;
        let Some(card) = board.get_card_mut(&card_id) else {
            if expected_state.is_some() {
                return Err(stale_verifier_error(&task_id_owned, &card_id));
            }
            return Err(format!("Card {} not found", card_id));
        };
        if let Some(expected) = expected_state.as_ref() {
            if !expected.matches_board_card(board_rev, card) {
                return Err(stale_verifier_error(&task_id_owned, &card_id));
            }
        }
        card.verifier_report = Some(report.clone());
        append_verifier_status(card, &report);
        Ok(())
    })
    .await
    {
        Ok(_) => Ok(true),
        Err(error) if is_stale_verifier_error(&error) => {
            tracing::info!("{}", error);
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub async fn schedule_card_verifier(ctx: &ChatCtx, request: VerifyCardRequest) {
    if ctx.runtime.shutdown_flag.load(Ordering::SeqCst) {
        return;
    }
    let task_ctx = ctx.clone();
    let mut tasks = ctx.card_verifier_tasks.lock().await;
    while let Some(result) = tasks.try_join_next() {
        if let Err(error) = result {
            tracing::warn!("card verifier task failed: {error}");
        }
    }
    if ctx.runtime.shutdown_flag.load(Ordering::SeqCst) {
        return;
    }
    tasks.spawn(async move {
        if let Err(error) = verify_card_for_ctx(&task_ctx, request.clone()).await {
            if is_stale_verifier_error(&error) {
                tracing::info!("{}", error);
                return;
            }
            let report = launch_failure_report(error);
            if let Err(store_error) = store_verifier_report(
                &task_ctx,
                &request.task_id,
                &request.card_id,
                request.expected_state.as_ref(),
                report,
            )
            .await
            {
                tracing::warn!(
                    "failed to store verifier launch-failure report for card {}: {}",
                    request.card_id,
                    store_error
                );
            }
        }
    });
}

pub async fn shutdown_card_verifiers(ctx: &ChatCtx) {
    let mut tasks = ctx.card_verifier_tasks.lock().await;
    tasks.abort_all();
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            if !error.is_cancelled() {
                tracing::warn!("card verifier task failed during shutdown: {error}");
            }
        }
    }
}

pub async fn schedule_card_verifier_after_finish(
    gcx: Arc<GlobalContext>,
    task_id: String,
    card_id: String,
    expected_state: ExpectedCardState,
) {
    schedule_card_verifier_after_finish_for_ctx(
        &gcx.chat_ctx(gcx.clone()),
        task_id,
        card_id,
        expected_state,
    )
    .await;
}

pub async fn schedule_card_verifier_after_finish_for_ctx_impl(
    ctx: &ChatCtx,
    task_id: String,
    card_id: String,
    expected_state: ExpectedCardState,
) {
    schedule_card_verifier(
        ctx,
        VerifyCardRequest {
            task_id,
            card_id,
            expected_state: Some(expected_state),
            custom_command: None,
        },
    )
    .await;
}

pub async fn verify_card(
    gcx: Arc<GlobalContext>,
    request: VerifyCardRequest,
) -> Result<VerifierReport, String> {
    verify_card_for_ctx(&gcx.chat_ctx(gcx.clone()), request).await
}

pub async fn verify_card_for_ctx_impl(
    ctx: &ChatCtx,
    request: VerifyCardRequest,
) -> Result<VerifierReport, String> {
    let task_meta = storage::load_task_meta_for_ctx(ctx, &request.task_id).await?;
    let board = storage::load_board_for_ctx(ctx, &request.task_id).await?;
    let card = board
        .get_card(&request.card_id)
        .ok_or_else(|| format!("Card {} not found", request.card_id))?
        .clone();
    if let Some(expected) = request.expected_state.as_ref() {
        if !expected.matches_board_card(board.rev, &card) {
            return Err(stale_verifier_error(&request.task_id, &request.card_id));
        }
    }
    let worktree = card
        .agent_worktree
        .as_ref()
        .map(PathBuf::from)
        .ok_or_else(|| format!("Card {} has no agent worktree", card.id))?;
    if !tokio::fs::metadata(&worktree)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(format!(
            "Card {} worktree '{}' does not exist",
            card.id,
            worktree.display()
        ));
    }

    let mut commands = verification_commands(&card);
    if let Some(ref custom) = request.custom_command {
        commands.insert(0, custom.clone());
    }
    let no_commands = commands.is_empty();
    let mut command_results = Vec::new();
    let mut concerns = Vec::new();

    if no_commands {
        concerns.push(
            "No verification commands found in card instructions or final report".to_string(),
        );
    }

    let command_policy = ctx.card_verifier.verification_command_policy(ctx).await;
    for command in commands {
        let result =
            run_verification_command_with_policy(ctx, &worktree, &command, &command_policy).await;
        if !result.passed {
            concerns.push(format!("Verification command failed: {}", result.command));
        }
        command_results.push(result);
    }

    let worktree_base = if let Some(worktree_id) = card.agent_worktree_name.as_deref() {
        let project_root = refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx(ctx)
            .await
            .into_iter()
            .next();
        if let Some(project_root) = project_root {
            match WorktreeService::new_async(ctx.paths.cache_dir.clone(), project_root).await {
                Ok(service) => service
                    .get_worktree(worktree_id)
                    .await
                    .ok()
                    .map(|view| (view.meta.base_commit, view.meta.base_branch)),
                Err(_) => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    let card_pair_present = card.base_commit.is_some() || card.base_branch.is_some();
    let (base_commit, base_branch) = if card_pair_present {
        (card.base_commit.clone(), card.base_branch.clone())
    } else if let Some((commit, branch)) = worktree_base {
        if commit.is_some() || branch.is_some() {
            (commit, branch)
        } else {
            (task_meta.base_commit, task_meta.base_branch)
        }
    } else {
        (task_meta.base_commit, task_meta.base_branch)
    };
    let diff_base = resolve_verifier_diff_base(base_commit, base_branch)?;
    let diff = git_changed_files_summary(&worktree, &diff_base, MAX_DIFF_LINES)
        .await
        .unwrap_or_else(|error| format!("diff unavailable: {}", error));
    let prompt = verifier_prompt(&card, &command_results, &diff);
    let review = run_verifier_review(ctx, prompt, card.agent_chat_id.as_deref()).await;
    let review_unavailable = review.is_err();
    let model_concerns = review.unwrap_or_else(|error| {
        vec![format!(
            "Verifier review subchat unavailable; human review recommended: {}",
            error
        )]
    });
    let review_has_concerns = !review_unavailable && !model_concerns.is_empty();
    concerns.extend(model_concerns);

    let classification = classify_verifier_report(
        no_commands,
        &command_results,
        review_unavailable,
        review_has_concerns,
    );
    let passed = classification == VerifierReportClassification::Passed;
    let recommendation = match classification {
        VerifierReportClassification::Passed => "merge",
        VerifierReportClassification::HumanReview => "human-review",
        VerifierReportClassification::VerificationFailed
        | VerifierReportClassification::Unknown => "fix-needed",
    }
    .to_string();

    let report = VerifierReport {
        passed,
        command_results,
        concerns,
        recommendation,
        classification,
    };
    let stored = store_verifier_report(
        ctx,
        &request.task_id,
        &request.card_id,
        request.expected_state.as_ref(),
        report.clone(),
    )
    .await?;
    if !stored {
        return Err(stale_verifier_error(&request.task_id, &request.card_id));
    }
    Ok(report)
}

pub fn classify_verifier_report(
    no_commands: bool,
    command_results: &[VerificationResult],
    review_unavailable: bool,
    review_has_concerns: bool,
) -> VerifierReportClassification {
    let hard_command_failed = command_results.iter().any(|result| {
        matches!(result.outcome, VerificationOutcome::CommandFailed)
            || (!result.passed
                && !outcome_is_infrastructure(&result.outcome)
                && !matches!(result.outcome, VerificationOutcome::NoCommands))
    });
    if hard_command_failed {
        return VerifierReportClassification::VerificationFailed;
    }
    let human_review_needed = no_commands
        || review_has_concerns
        || review_unavailable
        || command_results
            .iter()
            .any(|result| outcome_is_infrastructure(&result.outcome) || !result.passed);
    if human_review_needed {
        return VerifierReportClassification::HumanReview;
    }
    if !command_results.is_empty()
        && command_results.iter().all(|result| {
            result.passed
                && matches!(
                    result.outcome,
                    VerificationOutcome::Passed | VerificationOutcome::Unknown
                )
        })
    {
        VerifierReportClassification::Passed
    } else {
        VerifierReportClassification::VerificationFailed
    }
}

pub fn launch_failure_report(error: String) -> VerifierReport {
    VerifierReport {
        passed: false,
        command_results: Vec::new(),
        concerns: vec![format!(
            "Verifier failed to launch; human review recommended: {}",
            error
        )],
        recommendation: "human-review".to_string(),
        classification: VerifierReportClassification::HumanReview,
    }
}

pub async fn run_verification_command(
    ctx: &ChatCtx,
    worktree: &Path,
    command: &str,
) -> VerificationResult {
    let policy = ctx.card_verifier.verification_command_policy(ctx).await;
    run_verification_command_with_policy(ctx, worktree, command, &policy).await
}

pub async fn run_verification_command_with_policy(
    ctx: &ChatCtx,
    worktree: &Path,
    command: &str,
    policy: &VerifyCommandPolicy,
) -> VerificationResult {
    let mut runner = SystemVerificationCommandRunner { ctx: ctx.clone() };
    run_verification_command_with_runner(worktree, command, policy, &mut runner).await
}

pub async fn run_verification_command_with_runner<R: VerificationCommandRunner>(
    worktree: &Path,
    command: &str,
    policy: &VerifyCommandPolicy,
    runner: &mut R,
) -> VerificationResult {
    let parsed = match parse_verification_argv(command) {
        Ok(parsed) => parsed,
        Err(reason) => {
            return VerificationResult {
                command: command.to_string(),
                exit_code: None,
                passed: false,
                output_tail: format!("Cannot run as a command: {}", reason),
                outcome: VerificationOutcome::Rejected,
            };
        }
    };
    if let Err(reason) = policy.check(command) {
        return VerificationResult {
            command: command.to_string(),
            exit_code: None,
            passed: false,
            output_tail: format!("Denied by shell policy: {}", reason),
            outcome: VerificationOutcome::PolicyDenied,
        };
    }
    runner
        .run(worktree, command, parsed.cwd, parsed.env, parsed.argv)
        .await
}

async fn run_verification_argv(
    ctx: &ChatCtx,
    worktree: &Path,
    command: &str,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    argv: Vec<String>,
) -> VerificationResult {
    run_verification_argv_impl(
        ctx,
        worktree,
        command,
        cwd,
        env,
        argv,
        VERIFY_TIMEOUT,
        DRAIN_TIMEOUT,
    )
    .await
}

pub async fn run_verification_argv_impl(
    ctx: &ChatCtx,
    worktree: &Path,
    command: &str,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    argv: Vec<String>,
    timeout: Duration,
    drain_timeout: Duration,
) -> VerificationResult {
    if argv.is_empty() {
        return VerificationResult {
            command: command.to_string(),
            exit_code: None,
            passed: false,
            output_tail: "empty verification command".to_string(),
            outcome: VerificationOutcome::Rejected,
        };
    };
    let effective_cwd = cwd.map_or_else(|| worktree.to_path_buf(), |cwd| worktree.join(cwd));
    if let Err(reason) = check_cwd_in_worktree(worktree, &effective_cwd).await {
        return VerificationResult {
            command: command.to_string(),
            exit_code: None,
            passed: false,
            output_tail: reason,
            outcome: VerificationOutcome::InfrastructureFailed,
        };
    }
    let request = match build_exec_request_for_ctx(
        ctx,
        CommandPolicyInput {
            source: ExecSource::Verifier,
            command: CommandKind::Argv(&argv),
            cwd: Some(effective_cwd),
            env: env.into_iter().collect::<HashMap<String, String>>(),
            chat_mode: None,
            escalation: None,
        },
    )
    .await
    {
        Ok(policy) => policy
            .request
            .with_timeout(timeout)
            .with_output_drain_timeout(drain_timeout)
            .with_transcript_limit(MAX_OUTPUT_CAPTURE_BYTES * 2),
        Err(error) => {
            return VerificationResult {
                command: command.to_string(),
                exit_code: None,
                passed: false,
                output_tail: format!("failed to spawn command: {}", error.message),
                outcome: VerificationOutcome::PolicyDenied,
            };
        }
    };
    let result = match ctx.runtime.exec_registry.spawn(request).await {
        Ok(result) => result,
        Err(error) => {
            return VerificationResult {
                command: command.to_string(),
                exit_code: None,
                passed: false,
                output_tail: format!("failed to spawn command: {}", error),
                outcome: VerificationOutcome::InfrastructureFailed,
            };
        }
    };
    let read = ctx
        .runtime
        .exec_registry
        .read(&result.snapshot.meta.process_id, 0, None)
        .await;
    let mut stdout = String::new();
    let mut stderr = String::new();
    for chunk in read.chunks {
        match chunk.stream {
            ExecOutputStream::Stdout | ExecOutputStream::Combined => stdout.push_str(&chunk.text),
            ExecOutputStream::Stderr => stderr.push_str(&chunk.text),
        }
    }
    let mut output = format!("{stdout}{stderr}");
    let (exit_code, passed, outcome) = match &result.snapshot.status {
        ExecStatus::Exited { exit_code } => (
            *exit_code,
            exit_code == &Some(0),
            if exit_code == &Some(0) {
                VerificationOutcome::Passed
            } else {
                VerificationOutcome::CommandFailed
            },
        ),
        ExecStatus::SandboxLauncherFailed { exit_code } => {
            output.push_str(&format!(
                "sandbox launcher failed before command execution with exit code {exit_code}"
            ));
            (
                Some(*exit_code),
                false,
                VerificationOutcome::InfrastructureFailed,
            )
        }
        ExecStatus::TimedOut => {
            output.push_str(&format!(
                "command timed out after {} seconds",
                timeout.as_secs()
            ));
            (None, false, VerificationOutcome::InfrastructureFailed)
        }
        ExecStatus::Failed { message } => {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(message);
            (None, false, VerificationOutcome::InfrastructureFailed)
        }
        ExecStatus::Killed | ExecStatus::Starting | ExecStatus::Running => {
            (None, false, VerificationOutcome::InfrastructureFailed)
        }
    };
    VerificationResult {
        command: command.to_string(),
        exit_code,
        passed,
        output_tail: tail_chars(&output, MAX_OUTPUT_TAIL_CHARS),
        outcome,
    }
}

fn verifier_prompt(card: &BoardCard, commands: &[VerificationResult], diff: &str) -> String {
    format!(
        "Review this completed task card. Return concise concerns only. If the changed files look safe and commands passed, answer exactly PASS.\n\nCard: {} - {}\n\nInstructions:\n{}\n\nFinal report:\n{}\n\nCommand results:\n{}\n\nChanged files:\n{}",
        card.id,
        card.title,
        card.instructions,
        card.final_report.as_deref().unwrap_or(""),
        serde_json::to_string_pretty(commands).unwrap_or_default(),
        diff
    )
}

async fn run_verifier_review(
    ctx: &ChatCtx,
    prompt: String,
    parent_chat_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let model = resolve_verifier_model(ctx).await?;
    let config = refact_chat_api::SubchatConfig {
        tool_name: "verifier".to_string(),
        stateful: false,
        autonomous_no_confirm: true,
        auto_approve_editing_tools: false,
        auto_approve_dangerous_commands: false,
        chat_id: None,
        title: None,
        parent_id: None,
        link_type: None,
        root_chat_id: None,
        tools: refact_chat_api::ToolsPolicy::None,
        max_steps: 1,
        prepend_system_prompt: false,
        wrap_up: None,
        task_meta: None,
        worktree: None,
        model,
        mode: "agent".to_string(),
        n_ctx: 32_000,
        max_new_tokens: 1024,
        temperature: Some(0.0),
        reasoning_effort: None,
        cache_control: refact_core::chat_types::CacheControl::Ephemeral,
        parent_tool_call_id: None,
        parent_subchat_tx: None,
        abort_flag: None,
        soft_abort: false,
        activity_stamp: None,
        background_agent_id: None,
        subchat_depth: 1,
        final_step_force_answer: false,
        buddy_meta: None,
        step_progress: None,
        trace_parent: refact_chat_api::TraceParent::from_parts(parent_chat_id, None),
    };
    let messages = vec![event(
        EventSubkind::VerifierReport,
        VERIFIER_SOURCE,
        json!({ "kind": "verifier_review_prompt" }),
        prompt,
    )];
    let result = ctx.subchat.run_subchat(messages, config).await?;
    let answer = result
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .map(|message| message.content.content_text_only())
        .unwrap_or_default();
    Ok(parse_review_concerns(&answer))
}

async fn resolve_verifier_model(ctx: &ChatCtx) -> Result<String, String> {
    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| e.message.clone())?;
    if !caps.defaults.chat_light_model.is_empty() {
        return Ok(caps.defaults.chat_light_model.clone());
    }
    if !caps.defaults.chat_default_model.is_empty() {
        return Ok(caps.defaults.chat_default_model.clone());
    }
    Err("no light/default model configured for verifier".to_string())
}

fn parse_review_concerns(answer: &str) -> Vec<String> {
    let trimmed = answer.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("pass") {
        return Vec::new();
    }
    trimmed
        .lines()
        .map(|line| line.trim().trim_start_matches(['-', '*', ' ']).trim())
        .filter(|line| !line.is_empty() && !line.eq_ignore_ascii_case("pass"))
        .map(str::to_string)
        .collect()
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let len = text.chars().count();
    if len <= max_chars {
        return text.to_string();
    }
    text.chars().skip(len - max_chars).collect()
}
