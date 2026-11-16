// Engine seam implementation of `CardVerifier`. The verification logic lives in
// `refact_tools::chat_verifier`; only the shell-policy loader (an engine-owned
// config file) stays here, so the struct delegates the rest into the crate.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;

use refact_runtime_api::verifier::CardVerifier;
use refact_tasks::types::{VerificationResult, VerifierReport};

use crate::chat::ctx::ChatCtx;

pub use refact_tools::chat_verifier::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::Ordering;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::Utc;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tasks::storage;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tasks::types::{
    BoardCard, StatusUpdate, VerificationOutcome, VerifierReportClassification,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::shell_gate::{ApprovalMode, ShellGatePolicy};
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::verify_cmd::{from_shell_policy, permissive};

pub struct EngineCardVerifier;

#[async_trait]
impl CardVerifier for EngineCardVerifier {
    async fn load_expected_card_state(
        &self,
        ctx: &ChatCtx,
        task_id: &str,
        card_id: &str,
    ) -> Result<ExpectedCardState, String> {
        refact_tools::chat_verifier::load_expected_card_state_for_ctx_impl(ctx, task_id, card_id)
            .await
    }

    async fn verify_card(
        &self,
        ctx: &ChatCtx,
        request: VerifyCardRequest,
    ) -> Result<VerifierReport, String> {
        refact_tools::chat_verifier::verify_card_for_ctx_impl(ctx, request).await
    }

    async fn schedule_card_verifier_after_finish(
        &self,
        ctx: &ChatCtx,
        task_id: String,
        card_id: String,
        expected_state: ExpectedCardState,
    ) {
        refact_tools::chat_verifier::schedule_card_verifier_after_finish_for_ctx_impl(
            ctx,
            task_id,
            card_id,
            expected_state,
        )
        .await;
    }

    async fn run_verification_argv(
        &self,
        ctx: &ChatCtx,
        worktree: &Path,
        command: &str,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
        argv: Vec<String>,
        timeout: Duration,
        drain_timeout: Duration,
    ) -> VerificationResult {
        refact_tools::chat_verifier::run_verification_argv_impl(
            ctx,
            worktree,
            command,
            cwd,
            env,
            argv,
            timeout,
            drain_timeout,
        )
        .await
    }

    async fn verification_command_policy(&self, ctx: &ChatCtx) -> VerifyCommandPolicy {
        crate::chat::verify_cmd::load_for_ctx(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::types::{FinalReport, ScopeGuardMode, TaskBoard, TaskMeta, TaskStatus};

    #[derive(Default)]
    struct MockVerificationRunner {
        calls: Vec<(
            PathBuf,
            String,
            Option<PathBuf>,
            Vec<(String, String)>,
            Vec<String>,
        )>,
    }

    #[async_trait]
    impl VerificationCommandRunner for MockVerificationRunner {
        async fn run(
            &mut self,
            worktree: &Path,
            command: &str,
            cwd: Option<PathBuf>,
            env: Vec<(String, String)>,
            argv: Vec<String>,
        ) -> VerificationResult {
            self.calls
                .push((worktree.to_path_buf(), command.to_string(), cwd, env, argv));
            VerificationResult {
                command: command.to_string(),
                exit_code: Some(0),
                passed: true,
                output_tail: "ok".to_string(),
                outcome: VerificationOutcome::Unknown,
            }
        }
    }

    fn card(instructions: &str) -> BoardCard {
        BoardCard {
            id: "T-verify".to_string(),
            title: "Verifier card".to_string(),
            column: "done".to_string(),
            priority: "P1".to_string(),
            depends_on: Vec::new(),
            instructions: instructions.to_string(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: Vec::new(),
            comments: vec![],
            final_report: Some("done".to_string()),
            final_report_structured: None,
            verifier_report: None,
            created_at: Utc::now().to_rfc3339(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: Some(Utc::now().to_rfc3339()),
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: Vec::new(),
            scope_guard_mode: ScopeGuardMode::Off,
        }
    }

    fn verifier_report(passed: bool) -> VerifierReport {
        VerifierReport {
            passed,
            command_results: vec![VerificationResult {
                command: "cargo test verifier".to_string(),
                exit_code: Some(if passed { 0 } else { 1 }),
                passed,
                output_tail: "ok".to_string(),
                outcome: VerificationOutcome::Unknown,
            }],
            concerns: if passed {
                Vec::new()
            } else {
                vec!["failed".to_string()]
            },
            recommendation: if passed { "merge" } else { "fix-needed" }.to_string(),
            classification: if passed {
                VerifierReportClassification::Passed
            } else {
                VerifierReportClassification::VerificationFailed
            },
        }
    }

    async fn write_task(root: &Path, gcx: Arc<GlobalContext>, card: BoardCard) {
        let task_id = "task-1";
        let task_dir = root.join(".refact").join("tasks").join(task_id);
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        let now = Utc::now().to_rfc3339();
        let meta = TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 1,
            cards_failed: 0,
            agents_active: 0,
            base_branch: Some("main".to_string()),
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        storage::save_task_meta(gcx.clone(), task_id, &meta)
            .await
            .unwrap();
        storage::save_board(
            gcx,
            task_id,
            &TaskBoard {
                cards: vec![card],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[test]
    fn verifier_commands_include_acceptance_verify_lines() {
        let card = card(
            "## Acceptance Criteria\n- verifier.rs created\n- Verify: `cargo test --lib -p refact-lsp -- verifier merge_agent`",
        );

        assert_eq!(
            verification_commands(&card),
            vec!["cargo test --lib -p refact-lsp -- verifier merge_agent".to_string()]
        );
    }

    #[test]
    fn verifier_commands_include_structured_final_report_commands() {
        let mut card = card("## Acceptance Criteria\n- [ ] done");
        card.final_report_structured = Some(FinalReport {
            verification: vec![VerificationResult {
                command: "cargo test --lib -p refact-lsp -- verifier".to_string(),
                exit_code: Some(0),
                passed: true,
                output_tail: "ok".to_string(),
                outcome: VerificationOutcome::Unknown,
            }],
            ..Default::default()
        });

        assert_eq!(
            verification_commands(&card),
            vec!["cargo test --lib -p refact-lsp -- verifier".to_string()]
        );
    }

    #[test]
    fn verifier_status_records_pass_and_fail() {
        let mut pass_card = card("");
        let pass = VerifierReport {
            passed: true,
            recommendation: "merge".to_string(),
            classification: VerifierReportClassification::Passed,
            ..Default::default()
        };
        append_verifier_status(&mut pass_card, &pass);
        assert_eq!(pass_card.status_updates[0].message, "Verifier: PASS");

        let mut fail_card = card("");
        let fail = VerifierReport {
            passed: false,
            concerns: vec!["command failed".to_string()],
            recommendation: "fix-needed".to_string(),
            classification: VerifierReportClassification::VerificationFailed,
            ..Default::default()
        };
        append_verifier_status(&mut fail_card, &fail);
        assert_eq!(
            fail_card.status_updates[0].message,
            "Verifier: FAIL — command failed"
        );
    }

    #[test]
    fn launch_failure_report_returns_passed_false() {
        let report = launch_failure_report("no model configured".to_string());

        assert!(!report.passed);
        assert_eq!(report.recommendation, "human-review");
        assert!(report.command_results.is_empty());
        assert!(report.concerns[0].contains("Verifier failed to launch"));
        assert!(report.concerns[0].contains("no model configured"));
    }

    #[test]
    fn typed_classification_does_not_infer_state_from_concern_text() {
        let infrastructure = VerificationResult {
            command: "cargo test".to_string(),
            passed: false,
            outcome: VerificationOutcome::InfrastructureFailed,
            ..Default::default()
        };
        assert_eq!(
            classify_verifier_report(false, &[infrastructure], false, false),
            VerifierReportClassification::HumanReview
        );
        let passed = VerificationResult {
            command: "cargo test".to_string(),
            passed: true,
            outcome: VerificationOutcome::Passed,
            ..Default::default()
        };
        assert_eq!(
            classify_verifier_report(false, &[passed.clone()], true, false),
            VerifierReportClassification::HumanReview
        );
        assert_eq!(
            classify_verifier_report(false, &[passed], false, true),
            VerifierReportClassification::HumanReview
        );
    }

    #[test]
    fn typed_classification_requires_commands_and_real_success() {
        assert_eq!(
            classify_verifier_report(true, &[], false, false),
            VerifierReportClassification::HumanReview
        );
        let result = VerificationResult {
            command: "cargo test".to_string(),
            passed: false,
            outcome: VerificationOutcome::CommandFailed,
            ..Default::default()
        };
        assert_eq!(
            classify_verifier_report(false, &[result], false, false),
            VerifierReportClassification::VerificationFailed
        );
    }

    #[test]
    fn review_concerns_with_passing_commands_classify_as_human_review() {
        let passed = command_result("cargo test", true, VerificationOutcome::Passed);
        assert_eq!(
            classify_verifier_report(false, &[passed], false, true),
            VerifierReportClassification::HumanReview
        );
    }

    #[test]
    fn no_commands_with_passing_review_classify_as_human_review() {
        assert_eq!(
            classify_verifier_report(true, &[], false, false),
            VerifierReportClassification::HumanReview
        );
    }

    #[test]
    fn mock_verifier_passed_case_recommends_merge() {
        let report = VerifierReport {
            passed: true,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                exit_code: Some(0),
                passed: true,
                output_tail: "ok".to_string(),
                outcome: VerificationOutcome::Unknown,
            }],
            concerns: Vec::new(),
            recommendation: "merge".to_string(),
            classification: VerifierReportClassification::Passed,
        };

        assert!(report.passed);
        assert_eq!(report.recommendation, "merge");
    }

    #[test]
    fn mock_verifier_failed_case_recommends_fix_needed() {
        let report = VerifierReport {
            passed: false,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                exit_code: Some(1),
                passed: false,
                output_tail: "failed".to_string(),
                outcome: VerificationOutcome::Unknown,
            }],
            concerns: vec!["Verification command failed: cargo test".to_string()],
            recommendation: "fix-needed".to_string(),
            classification: VerifierReportClassification::VerificationFailed,
        };

        assert!(!report.passed);
        assert_eq!(report.recommendation, "fix-needed");
    }

    #[tokio::test]
    async fn store_verifier_report_lands_when_expected_card_state_matches() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = card("## Acceptance Criteria\n- Verify: `cargo test verifier`");
        card.id = "T-1".to_string();
        write_task(temp.path(), gcx.clone(), card).await;
        let expected = load_expected_card_state(gcx.clone(), "task-1", "T-1")
            .await
            .unwrap();

        let stored = store_verifier_report(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            "T-1",
            Some(&expected),
            verifier_report(true),
        )
        .await
        .unwrap();

        assert!(stored);
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.verifier_report.as_ref().unwrap().passed);
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message == "Verifier: PASS"));
    }

    #[tokio::test]
    async fn store_verifier_report_noops_when_card_restarted() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = card("## Acceptance Criteria\n- Verify: `cargo test verifier`");
        card.id = "T-1".to_string();
        card.agent_chat_id = Some("agent-old".to_string());
        write_task(temp.path(), gcx.clone(), card).await;
        let expected = load_expected_card_state(gcx.clone(), "task-1", "T-1")
            .await
            .unwrap();

        storage::update_board_atomic(gcx.clone(), "task-1", |board| {
            let card = board.get_card_mut("T-1").unwrap();
            card.column = "doing".to_string();
            card.agent_chat_id = Some("agent-new".to_string());
            card.assignee = Some("agent-new".to_string());
            card.final_report = None;
            card.final_report_structured = None;
            Ok(())
        })
        .await
        .unwrap();

        let stored = store_verifier_report(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            "T-1",
            Some(&expected),
            verifier_report(false),
        )
        .await
        .unwrap();

        assert!(!stored);
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.verifier_report.is_none());
        assert_eq!(card.column, "doing");
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-new"));
    }

    #[tokio::test]
    async fn store_verifier_report_noops_when_card_cleaned_up_after_merge() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = card("## Acceptance Criteria\n- Verify: `cargo test verifier`");
        card.id = "T-1".to_string();
        card.agent_branch = Some("refact/task/task-1/card/T-1/agent".to_string());
        card.agent_worktree = Some(temp.path().join("agent").to_string_lossy().to_string());
        card.agent_worktree_name = Some("wt-1".to_string());
        write_task(temp.path(), gcx.clone(), card).await;
        let expected = load_expected_card_state(gcx.clone(), "task-1", "T-1")
            .await
            .unwrap();

        storage::update_board_atomic(gcx.clone(), "task-1", |board| {
            let card = board.get_card_mut("T-1").unwrap();
            card.agent_branch = None;
            card.agent_worktree = None;
            card.agent_worktree_name = None;
            Ok(())
        })
        .await
        .unwrap();

        let stored = store_verifier_report(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            "T-1",
            Some(&expected),
            verifier_report(false),
        )
        .await
        .unwrap();

        assert!(!stored);
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.verifier_report.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
    }

    #[tokio::test]
    async fn store_verifier_report_noops_when_report_already_changed() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = card("## Acceptance Criteria\n- Verify: `cargo test verifier`");
        card.id = "T-1".to_string();
        write_task(temp.path(), gcx.clone(), card).await;
        let expected = load_expected_card_state(gcx.clone(), "task-1", "T-1")
            .await
            .unwrap();

        store_verifier_report(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            "T-1",
            None,
            verifier_report(true),
        )
        .await
        .unwrap();

        let stored = store_verifier_report(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            "T-1",
            Some(&expected),
            verifier_report(false),
        )
        .await
        .unwrap();

        assert!(!stored);
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.verifier_report.as_ref().unwrap().passed);
    }

    #[tokio::test]
    async fn verifier_runs_argv_not_shell() {
        let temp = tempfile::tempdir().unwrap();
        let mut runner = MockVerificationRunner::default();

        let result = run_verification_command_with_runner(
            temp.path(),
            "cd refact-agent/engine && cargo check",
            &crate::chat::verify_cmd::permissive(),
            &mut runner,
        )
        .await;

        assert!(result.passed);
        assert_eq!(runner.calls.len(), 1);
        assert_eq!(runner.calls[0].0, temp.path());
        assert_eq!(runner.calls[0].1, "cd refact-agent/engine && cargo check");
        assert_eq!(
            runner.calls[0].2,
            Some(PathBuf::from("refact-agent/engine"))
        );
        assert!(runner.calls[0].3.is_empty());
        assert_eq!(runner.calls[0].4, vec!["cargo", "check"]);
    }

    #[tokio::test]
    async fn verifier_threads_env_prefix_to_the_runner() {
        let temp = tempfile::tempdir().unwrap();
        let mut runner = MockVerificationRunner::default();

        let result = run_verification_command_with_runner(
            temp.path(),
            "cd ui && FLEXUS_PYTHON=/abs/py pnpm typecheck",
            &crate::chat::verify_cmd::permissive(),
            &mut runner,
        )
        .await;

        assert!(result.passed, "{}", result.output_tail);
        assert_eq!(runner.calls[0].2, Some(PathBuf::from("ui")));
        assert_eq!(
            runner.calls[0].3,
            vec![("FLEXUS_PYTHON".to_string(), "/abs/py".to_string())]
        );
        assert_eq!(runner.calls[0].4, vec!["pnpm", "typecheck"]);
    }

    #[tokio::test]
    async fn verifier_runs_previously_unsupported_binaries() {
        let temp = tempfile::tempdir().unwrap();
        let mut runner = MockVerificationRunner::default();

        for command in [
            "dotnet test",
            "go test ./...",
            "make check",
            "gradlew build",
        ] {
            let result = run_verification_command_with_runner(
                temp.path(),
                command,
                &crate::chat::verify_cmd::permissive(),
                &mut runner,
            )
            .await;
            assert!(result.passed, "{command}: {}", result.output_tail);
        }
        assert_eq!(runner.calls.len(), 4);
    }

    #[tokio::test]
    async fn verifier_denies_commands_matching_shell_policy_deny_rules() {
        let temp = tempfile::tempdir().unwrap();
        let mut runner = MockVerificationRunner::default();
        let policy = crate::chat::verify_cmd::from_shell_policy(
            &crate::tools::shell_gate::ShellGatePolicy::default(),
        );

        let result = run_verification_command_with_runner(
            temp.path(),
            "sudo rm -rf /",
            &policy,
            &mut runner,
        )
        .await;

        assert!(!result.passed);
        assert_eq!(result.outcome, VerificationOutcome::PolicyDenied);
        assert!(
            result.output_tail.starts_with("Denied by shell policy:"),
            "{}",
            result.output_tail
        );
        assert!(runner.calls.is_empty());
    }

    fn command_result(
        command: &str,
        passed: bool,
        outcome: VerificationOutcome,
    ) -> VerificationResult {
        VerificationResult {
            command: command.to_string(),
            exit_code: None,
            passed,
            output_tail: String::new(),
            outcome,
        }
    }

    #[test]
    fn rejected_and_denied_commands_classify_as_human_review_not_verification_failure() {
        for outcome in [
            VerificationOutcome::Rejected,
            VerificationOutcome::PolicyDenied,
            VerificationOutcome::InfrastructureFailed,
        ] {
            let results = vec![command_result("dotnet test", false, outcome.clone())];
            assert_eq!(
                classify_verifier_report(false, &results, false, false),
                VerifierReportClassification::HumanReview,
                "{outcome:?} must be infrastructure, not a verification failure"
            );
        }
    }

    #[test]
    fn genuine_command_failure_still_classifies_as_verification_failure() {
        let results = vec![command_result(
            "cargo test",
            false,
            VerificationOutcome::CommandFailed,
        )];
        assert_eq!(
            classify_verifier_report(false, &results, false, false),
            VerifierReportClassification::VerificationFailed
        );

        let mixed = vec![
            command_result("cargo build", true, VerificationOutcome::Passed),
            command_result("cargo test", false, VerificationOutcome::CommandFailed),
        ];
        assert_eq!(
            classify_verifier_report(false, &mixed, false, false),
            VerifierReportClassification::VerificationFailed
        );
    }

    #[test]
    fn passed_and_rejected_mix_is_human_review() {
        let results = vec![
            command_result("cargo build", true, VerificationOutcome::Passed),
            command_result("dotnet test", false, VerificationOutcome::Rejected),
        ];
        assert_eq!(
            classify_verifier_report(false, &results, false, false),
            VerifierReportClassification::HumanReview
        );
    }

    #[tokio::test]
    async fn verifier_yolo_mode_allows_arbitrary_binary() {
        let temp = tempfile::tempdir().unwrap();
        let mut runner = MockVerificationRunner::default();
        let policy =
            crate::chat::verify_cmd::from_shell_policy(&crate::tools::shell_gate::ShellGatePolicy {
                mode: crate::tools::shell_gate::ApprovalMode::Yolo,
                ..crate::tools::shell_gate::ShellGatePolicy::default()
            });

        let result = run_verification_command_with_runner(
            temp.path(),
            "mycompany-verify --all",
            &policy,
            &mut runner,
        )
        .await;

        assert!(result.passed, "{}", result.output_tail);
        assert_eq!(runner.calls[0].4, vec!["mycompany-verify", "--all"]);
    }

    #[tokio::test]
    async fn verifier_rejects_shell_syntax() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;

        let result = run_verification_command(
            &gcx.chat_ctx(gcx.clone()),
            temp.path(),
            "cargo test | tee f",
        )
        .await;

        assert!(!result.passed);
        assert_eq!(result.outcome, VerificationOutcome::Rejected);
        assert!(result.output_tail.starts_with("Cannot run as a command:"));
    }

    #[tokio::test]
    async fn agent_finish_spawns_verifier_through_helper() {
        let mut card = card("");
        card.id = "T-missing".to_string();
        let expected_state = ExpectedCardState::from_card(0, &card);
        let gcx = crate::global_context::tests::make_test_gcx().await;

        schedule_card_verifier_after_finish(
            gcx.clone(),
            "missing-task".to_string(),
            "T-missing".to_string(),
            expected_state,
        )
        .await;

        assert_eq!(gcx.card_verifier_tasks.lock().await.len(), 1);
        shutdown_card_verifiers(&gcx.chat_ctx(gcx.clone())).await;
        assert!(gcx.card_verifier_tasks.lock().await.is_empty());
    }

    #[tokio::test]
    async fn verifier_is_not_scheduled_after_shutdown_starts() {
        let mut card = card("");
        card.id = "T-stopped".to_string();
        let expected_state = ExpectedCardState::from_card(0, &card);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        gcx.shutdown_flag.store(true, Ordering::SeqCst);

        schedule_card_verifier_after_finish(
            gcx.clone(),
            "missing-task".to_string(),
            "T-stopped".to_string(),
            expected_state,
        )
        .await;

        assert!(gcx.card_verifier_tasks.lock().await.is_empty());
    }

    #[tokio::test]
    async fn cwd_outside_worktree_is_rejected() {
        let worktree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;

        let result = run_verification_argv_impl(
            &gcx.chat_ctx(gcx.clone()),
            worktree.path(),
            "cargo check",
            Some(outside.path().to_path_buf()),
            Vec::new(),
            vec!["cargo".to_string(), "check".to_string()],
            Duration::from_secs(30),
            DRAIN_TIMEOUT,
        )
        .await;

        assert!(!result.passed);
        assert!(result.output_tail.contains("outside the worktree"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_null_does_not_hang() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_verification_argv_impl(
                &gcx.chat_ctx(gcx.clone()),
                temp.path(),
                "cat",
                None,
                Vec::new(),
                vec!["cat".to_string()],
                Duration::from_secs(30),
                DRAIN_TIMEOUT,
            ),
        )
        .await
        .expect("should not hang with stdin=null");

        assert!(result.passed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;

        let result = run_verification_argv_impl(
            &gcx.chat_ctx(gcx.clone()),
            temp.path(),
            "sleep 30",
            None,
            Vec::new(),
            vec!["sleep".to_string(), "30".to_string()],
            Duration::from_millis(200),
            DRAIN_TIMEOUT,
        )
        .await;

        assert!(!result.passed);
        assert!(result.output_tail.contains("timed out"));
        assert!(result.exit_code.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn drain_times_out_when_descendant_holds_pipe() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;

        // bash spawns a background sleep that inherits the stdout/stderr pipes and then exits;
        // without the drain timeout the verifier hangs waiting for EOF from the background sleep
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_verification_argv_impl(
                &gcx.chat_ctx(gcx.clone()),
                temp.path(),
                "bash -c 'sleep 60 &'",
                None,
                Vec::new(),
                vec![
                    "bash".to_string(),
                    "-c".to_string(),
                    "sleep 60 &".to_string(),
                ],
                Duration::from_secs(30),
                Duration::from_millis(500),
            ),
        )
        .await
        .expect("verifier must not hang when descendant holds pipe open");

        assert!(result.output_tail.contains("drain timed out"));
        assert!(!result.passed);
    }
}
