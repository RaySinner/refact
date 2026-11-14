pub use refact_tools::tools::tool_task_merge_agent::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;

#[cfg(test)]
#[allow(unused_imports)]
use std::time::Duration;

#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;

#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;

#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{

    Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params,
};

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;

#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum};

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;

#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::verifier::outcome_is_infrastructure;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::verifier::load_expected_card_state_for_ctx;

#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage as storage;

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::task_tool_helpers::require_bound_planner_task;

#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::service::{worktree_merge_lock, WorktreeService};

#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::types::{
    MergeWorktreeRequest, MergeWorktreeResponse, WorktreeMergeStrategy, WorktreeRecordView,
    WorktreeReference,
};

#[cfg(test)]
use std::process::Command;
#[cfg(test)]
mod worktree_merge_tool_tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus, VerifierReport};
    use refact_worktrees::types::CreateWorktreeRequest;
    use serde_json::json;
    use std::path::Path;

    fn run_git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {:?}: {}", args, e));
        if !output.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn init_repo(root: &Path) {
        run_git(root, &["init"]);
        run_git(root, &["checkout", "-b", "main"]);
        run_git(root, &["config", "core.autocrlf", "false"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        std::fs::write(root.join(".gitignore"), ".refact/\n").unwrap();
        run_git(root, &["add", "file.txt", ".gitignore"]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    fn commit_file(root: &Path, file: &str, content: &str, message: &str) {
        std::fs::write(root.join(file), content).unwrap();
        run_git(root, &["add", file]);
        run_git(root, &["commit", "-m", message]);
    }

    fn branch_exists(root: &Path, branch: &str) -> bool {
        Command::new("git")
            .args(["rev-parse", "--verify", branch])
            .current_dir(root)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn create_legacy_agent_worktree(source: &Path, worktree: &Path, branch: &str) {
        run_git(source, &["branch", branch]);
        run_git(
            source,
            &["worktree", "add", worktree.to_str().unwrap(), branch],
        );
    }

    fn tool_text(result: &(bool, Vec<ContextEnum>)) -> String {
        let ContextEnum::ChatMessage(message) = &result.1[0] else {
            panic!("expected chat message")
        };
        let ChatContent::SimpleText(text) = &message.content else {
            panic!("expected simple text")
        };
        text.clone()
    }

    async fn set_workspace(gcx: Arc<GlobalContext>, root: &Path) {
        let root = root.canonicalize().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root];
    }

    async fn planner_ccx(gcx: Arc<GlobalContext>) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                "planner-chat".to_string(),
                None,
                "model".to_string(),
                Some(ThreadTaskMeta {
                    task_id: "task-1".to_string(),
                    role: "planner".to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: None,
                }),
                None,
            )
            .await,
        ))
    }

    fn test_card_with_branch(worktree_id: Option<&str>, branch: &str, root: &Path) -> BoardCard {
        BoardCard {
            id: "T-1".to_string(),
            title: "Card T-1".to_string(),
            column: "done".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: Some("agent-1".to_string()),
            agent_chat_id: Some("agent-chat-1".to_string()),
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: Some("done".to_string()),
            final_report_structured: None,
            verifier_report: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: Some(chrono::Utc::now().to_rfc3339()),
            agent_branch: Some(branch.to_string()),
            agent_worktree: Some(root.to_string_lossy().to_string()),
            agent_worktree_name: worktree_id.map(str::to_string),
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    fn test_card(worktree_id: &str, branch: &str, root: &Path) -> BoardCard {
        test_card_with_branch(Some(worktree_id), branch, root)
    }

    fn launch_failure_verifier_report() -> VerifierReport {
        VerifierReport {
            passed: false,
            concerns: vec![
                "Verifier failed to launch; human review recommended: no verifier model"
                    .to_string(),
            ],
            recommendation: "human-review".to_string(),
            ..Default::default()
        }
    }

    fn merged_response_with_warning() -> MergeWorktreeResponse {
        MergeWorktreeResponse {
            id: "wt-1".to_string(),
            status: "merged".to_string(),
            merged: true,
            strategy: "squash".to_string(),
            source_branch: "refact/task/T-1".to_string(),
            target_branch: "main".to_string(),
            committed_uncommitted: None,
            merge_commit: Some("abc123".to_string()),
            cleanup: None,
            conflict: None,
            affected_references: Vec::new(),
            affected_reference_count: 0,
            warnings: vec!["stash cleanup needs inspection".to_string()],
        }
    }

    fn merged_response_with_cleanup_warning() -> MergeWorktreeResponse {
        MergeWorktreeResponse {
            cleanup: Some(refact_worktrees::types::WorktreeRemovalResult {
                worktree_deleted: true,
                branch_deleted: false,
                registry_deleted: true,
                stale_path: false,
                warnings: vec!["branch deletion failed".to_string()],
            }),
            warnings: Vec::new(),
            ..merged_response_with_warning()
        }
    }

    #[test]
    fn merge_agent_response_message_includes_warnings() {
        let message = merge_response_message("T-1", &merged_response_with_warning(), None);

        assert!(message.contains("## Warnings"));
        assert!(message.contains("stash cleanup needs inspection"));
    }

    #[test]
    fn merge_agent_response_message_includes_cleanup_warnings() {
        let message = merge_response_message("T-1", &merged_response_with_cleanup_warning(), None);

        assert!(message.contains("## Warnings"));
        assert!(message.contains("branch deletion failed"));
    }

    #[test]
    fn post_merge_message_only_labels_command_failures_as_regressions() {
        use refact_runtime_api::post_merge_check::PostMergeCheckResult;
        use refact_tasks::types::VerificationOutcome;

        for (outcome, expected) in [
            (
                VerificationOutcome::Rejected,
                "could not be run as a command",
            ),
            (
                VerificationOutcome::PolicyDenied,
                "denied by command policy",
            ),
            (
                VerificationOutcome::InfrastructureFailed,
                "infrastructure failure",
            ),
            (VerificationOutcome::NoCommands, "no verification commands"),
            (VerificationOutcome::Unknown, "unknown result"),
        ] {
            let result = PostMergeCheckResult {
                checked: true,
                auto_reverted: false,
                command: Some("cargo test".to_string()),
                exit_code: None,
                output_tail: "details".to_string(),
                merge_commit: None,
                revert_commit: None,
                fix_card_id: None,
                skipped_reason: None,
                revert_skipped_reason: None,
                outcome,
            };
            let mut message = String::new();
            append_post_merge_check_message(&mut message, &result);
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains("regression"), "{message}");
            assert!(!message.contains("Fix card"), "{message}");
        }
    }

    #[test]
    fn post_merge_message_does_not_invent_fix_card() {
        use refact_runtime_api::post_merge_check::PostMergeCheckResult;
        use refact_tasks::types::VerificationOutcome;

        let result = PostMergeCheckResult {
            checked: true,
            auto_reverted: false,
            command: Some("cargo test".to_string()),
            exit_code: Some(1),
            output_tail: "test failed".to_string(),
            merge_commit: Some("abc".to_string()),
            revert_commit: None,
            fix_card_id: None,
            skipped_reason: None,
            revert_skipped_reason: Some("HEAD changed".to_string()),
            outcome: VerificationOutcome::CommandFailed,
        };
        let mut message = String::new();
        append_post_merge_check_message(&mut message, &result);

        assert!(message.contains("Post-merge regression"));
        assert!(!message.contains("Fix card"));
    }

    #[test]
    fn merge_agent_strategy_arg_defaults_to_squash() {
        assert_eq!(strategy_arg(&HashMap::new()).unwrap(), "squash");
    }

    #[test]
    fn merge_agent_strategy_arg_rejects_non_string_values() {
        let args = HashMap::from([("strategy".to_string(), Value::Bool(true))]);

        assert!(strategy_arg(&args)
            .unwrap_err()
            .contains("must be a string"));
    }

    #[test]
    fn merge_agent_bool_args_accept_string_spellings_and_reject_garbage() {
        let args = HashMap::from([("force".to_string(), Value::String("false".to_string()))]);

        assert_eq!(parse_force(&args), Ok(false));

        let args = HashMap::from([("auto_revert".to_string(), Value::String("True".to_string()))]);

        assert_eq!(parse_auto_revert(&args), Ok(true));

        let args = HashMap::from([(
            "auto_revert".to_string(),
            Value::String("sometimes".to_string()),
        )]);

        assert!(parse_auto_revert(&args)
            .unwrap_err()
            .contains("must be a boolean"));
    }

    #[test]
    fn merge_agent_timeout_rejects_string_values() {
        let args = HashMap::from([(
            "auto_revert_timeout_secs".to_string(),
            Value::String("120".to_string()),
        )]);

        assert!(parse_auto_revert_timeout(&args)
            .unwrap_err()
            .contains("positive integer"));
    }

    #[test]
    fn merge_agent_card_id_rejects_non_string_values() {
        let args = HashMap::from([("card_id".to_string(), Value::Bool(true))]);

        assert!(required_string_arg(&args, "card_id")
            .unwrap_err()
            .contains("must be a string"));
    }

    #[test]
    fn merge_agent_card_id_rejects_surrounding_whitespace() {
        let args = HashMap::from([("card_id".to_string(), Value::String(" T-1 ".to_string()))]);

        assert!(required_string_arg(&args, "card_id")
            .unwrap_err()
            .contains("leading or trailing whitespace"));
    }

    #[test]
    fn merge_agent_refuses_when_verifier_failed_without_force() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: false,
            concerns: vec!["cargo test failed".to_string()],
            recommendation: "fix-needed".to_string(),
            ..Default::default()
        });

        let err = ensure_verifier_allows_merge(&card, false, false).unwrap_err();

        assert!(err.contains("verifier failed"));
        assert!(err.contains("cargo test failed"));
        assert!(err.contains("force=true"));
    }

    #[test]
    fn merge_agent_force_cannot_bypass_genuine_failure() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: false,
            concerns: vec!["cargo test failed".to_string()],
            recommendation: "fix-needed".to_string(),
            ..Default::default()
        });

        assert!(ensure_verifier_allows_merge(&card, true, false).is_err());
    }

    #[test]
    fn merge_agent_blocks_contradictory_passed_report() {
        use refact_tasks::types::{
            VerificationOutcome, VerificationResult, VerifierReportClassification,
        };

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: true,
            classification: VerifierReportClassification::Passed,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                passed: false,
                outcome: VerificationOutcome::CommandFailed,
                ..Default::default()
            }],
            ..Default::default()
        });

        assert!(ensure_verifier_allows_merge(&card, false, false).is_err());
        assert!(ensure_verifier_allows_merge(&card, true, false).is_err());
    }

    #[test]
    fn merge_agent_fails_closed_for_ambiguous_legacy_unknown_result() {
        use refact_tasks::types::{VerificationResult, VerifierReportClassification};

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: true,
            classification: VerifierReportClassification::Unknown,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                passed: true,
                ..Default::default()
            }],
            ..Default::default()
        });

        assert!(ensure_verifier_allows_merge(&card, true, false).is_err());
    }

    #[test]
    fn merge_agent_blocks_when_verifier_launch_failed() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(launch_failure_verifier_report());

        let err = ensure_verifier_allows_merge(&card, false, false).unwrap_err();

        assert!(err.contains("verifier failed"));
        assert!(err.contains("Verifier failed to launch"));
        assert!(err.contains("force=true"));
    }

    #[test]
    fn merge_agent_force_allows_legacy_launch_failure_only() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(launch_failure_verifier_report());
        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());

        card.verifier_report = Some(VerifierReport {
            passed: false,
            recommendation: "human-review".to_string(),
            concerns: vec!["unknown legacy failure".to_string()],
            ..Default::default()
        });
        assert!(ensure_verifier_allows_merge(&card, true, false).is_err());
    }

    #[test]
    fn merge_agent_force_allows_human_review_with_review_concerns() {
        use refact_tasks::types::{
            VerificationOutcome, VerificationResult, VerifierReportClassification,
        };

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: false,
            concerns: vec!["Implementation does not satisfy the requested behavior".to_string()],
            recommendation: "human-review".to_string(),
            classification: VerifierReportClassification::HumanReview,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                passed: true,
                outcome: VerificationOutcome::Passed,
                ..Default::default()
            }],
        });

        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());
        assert!(ensure_verifier_allows_merge(&card, false, false).is_err());
    }

    #[test]
    fn merge_agent_force_allows_human_review_with_no_commands() {
        use refact_tasks::types::VerifierReportClassification;

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: false,
            concerns: vec![
                "No verification commands found in card instructions or final report".to_string(),
            ],
            recommendation: "human-review".to_string(),
            classification: VerifierReportClassification::HumanReview,
            command_results: vec![],
        });

        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());
        assert!(ensure_verifier_allows_merge(&card, false, false).is_err());
    }

    #[test]
    fn merge_agent_bypass_verifier_allows_genuine_command_failure() {
        use refact_tasks::types::{
            VerificationOutcome, VerificationResult, VerifierReportClassification,
        };

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: false,
            concerns: vec!["Verification command failed: cargo test".to_string()],
            recommendation: "fix-needed".to_string(),
            classification: VerifierReportClassification::VerificationFailed,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                passed: false,
                outcome: VerificationOutcome::CommandFailed,
                ..Default::default()
            }],
        });

        assert!(ensure_verifier_allows_merge(&card, true, false).is_err());
        let bypassed = ensure_verifier_allows_merge(&card, false, true).unwrap();
        assert_eq!(
            bypassed.as_deref(),
            Some("Verifier Warning: Bypassed by bypass_verifier=true")
        );
    }

    #[test]
    fn merge_agent_force_allows_typed_launch_failure_without_command_results() {
        use refact_tasks::types::VerifierReportClassification;

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        let mut report = launch_failure_verifier_report();
        report.classification = VerifierReportClassification::HumanReview;
        card.verifier_report = Some(report);

        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());
    }

    #[test]
    fn merge_agent_force_allows_mixed_passed_and_infrastructure_human_review() {
        use refact_tasks::types::{
            VerificationOutcome, VerificationResult, VerifierReportClassification,
        };

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: false,
            concerns: vec![
                "Verification command failed: cargo clippy".to_string(),
                "Verifier review subchat unavailable; human review recommended: no model configured"
                    .to_string(),
            ],
            recommendation: "human-review".to_string(),
            classification: VerifierReportClassification::HumanReview,
            command_results: vec![
                VerificationResult {
                    command: "cargo test".to_string(),
                    passed: true,
                    outcome: VerificationOutcome::Passed,
                    ..Default::default()
                },
                VerificationResult {
                    command: "cargo clippy".to_string(),
                    passed: false,
                    outcome: VerificationOutcome::InfrastructureFailed,
                    ..Default::default()
                },
            ],
        });

        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());
    }

    fn infrastructure_human_review_report(
        outcome: refact_tasks::types::VerificationOutcome,
    ) -> VerifierReport {
        use refact_tasks::types::{VerificationResult, VerifierReportClassification};

        VerifierReport {
            passed: false,
            concerns: vec!["Verification command failed: cargo test".to_string()],
            recommendation: "human-review".to_string(),
            classification: VerifierReportClassification::HumanReview,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                passed: false,
                outcome,
                ..Default::default()
            }],
        }
    }

    #[test]
    fn merge_agent_force_allows_rejected_command_human_review() {
        use refact_tasks::types::VerificationOutcome;

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(infrastructure_human_review_report(
            VerificationOutcome::Rejected,
        ));

        let warning = ensure_verifier_allows_merge(&card, true, false).unwrap();

        assert!(warning.is_some());
        assert!(warning.unwrap().contains("force=true"));
        assert!(ensure_verifier_allows_merge(&card, false, false).is_err());
    }

    #[test]
    fn merge_agent_force_allows_policy_denied_command_human_review() {
        use refact_tasks::types::VerificationOutcome;

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(infrastructure_human_review_report(
            VerificationOutcome::PolicyDenied,
        ));

        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());
        assert!(ensure_verifier_allows_merge(&card, false, false).is_err());
    }

    #[test]
    fn merge_agent_force_still_blocks_command_failed_result() {
        use refact_tasks::types::VerificationOutcome;

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(infrastructure_human_review_report(
            VerificationOutcome::CommandFailed,
        ));

        let err = ensure_verifier_allows_merge(&card, true, false).unwrap_err();

        assert!(err.contains("force=true cannot bypass verification failures"));
        assert!(err.contains("Verification command failed: cargo test"));
    }

    #[test]
    fn merge_agent_force_allows_no_commands_result() {
        use refact_tasks::types::{
            VerificationOutcome, VerificationResult, VerifierReportClassification,
        };

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: false,
            concerns: vec![
                "No verification commands found in card instructions or final report".to_string(),
            ],
            recommendation: "human-review".to_string(),
            classification: VerifierReportClassification::HumanReview,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                passed: false,
                outcome: VerificationOutcome::NoCommands,
                ..Default::default()
            }],
        });

        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());
        assert!(ensure_verifier_allows_merge(&card, false, false).is_err());
    }

    #[test]
    fn merge_agent_force_allows_mixed_passed_and_rejected_human_review() {
        use refact_tasks::types::{
            VerificationOutcome, VerificationResult, VerifierReportClassification,
        };

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: false,
            concerns: vec!["Verification command failed: cargo clippy".to_string()],
            recommendation: "human-review".to_string(),
            classification: VerifierReportClassification::HumanReview,
            command_results: vec![
                VerificationResult {
                    command: "cargo test".to_string(),
                    passed: true,
                    outcome: VerificationOutcome::Passed,
                    ..Default::default()
                },
                VerificationResult {
                    command: "cargo clippy".to_string(),
                    passed: false,
                    outcome: VerificationOutcome::Rejected,
                    ..Default::default()
                },
            ],
        });

        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());
    }

    #[test]
    fn merge_agent_force_allows_rejected_command_with_semantic_concern() {
        use refact_tasks::types::VerificationOutcome;

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        let mut report = infrastructure_human_review_report(VerificationOutcome::Rejected);
        report
            .concerns
            .push("Implementation does not satisfy the requested behavior".to_string());
        card.verifier_report = Some(report);

        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_some());
        assert!(ensure_verifier_allows_merge(&card, false, false).is_err());
    }

    #[test]
    fn merge_agent_strict_consistent_pass_is_unchanged() {
        use refact_tasks::types::{
            VerificationOutcome, VerificationResult, VerifierReportClassification,
        };

        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.verifier_report = Some(VerifierReport {
            passed: true,
            concerns: Vec::new(),
            recommendation: "merge".to_string(),
            classification: VerifierReportClassification::Passed,
            command_results: vec![VerificationResult {
                command: "cargo test".to_string(),
                passed: true,
                outcome: VerificationOutcome::Passed,
                ..Default::default()
            }],
        });

        assert!(ensure_verifier_allows_merge(&card, false, false)
            .unwrap()
            .is_none());
        assert!(ensure_verifier_allows_merge(&card, true, false).unwrap().is_none());
    }

    #[test]
    fn merge_agent_allows_card_without_verifier_report() {
        let card = test_card("wt", "agent", Path::new("/tmp/worktree"));

        assert!(ensure_verifier_allows_merge(&card, false, false).is_ok());
    }

    #[test]
    fn merge_agent_auto_revert_schema_and_args_are_opt_in() {
        let schema = ToolTaskMergeAgent::new().tool_description().input_schema;
        assert!(schema["properties"].get("auto_revert").is_some());
        assert!(schema["properties"]
            .get("auto_revert_timeout_secs")
            .is_some());
        let empty = HashMap::new();
        assert!(!parse_auto_revert(&empty).unwrap());
        assert_eq!(
            parse_auto_revert_timeout(&empty).unwrap(),
            Duration::from_secs(
                refact_runtime_api::post_merge_check::DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS
            )
        );
        let args = HashMap::from([
            ("auto_revert".to_string(), Value::Bool(true)),
            (
                "auto_revert_timeout_secs".to_string(),
                Value::Number(120.into()),
            ),
        ]);
        assert!(parse_auto_revert(&args).unwrap());
        assert_eq!(
            parse_auto_revert_timeout(&args).unwrap(),
            Duration::from_secs(120)
        );
    }

    #[tokio::test]
    async fn merge_agent_rejects_mismatched_task_id() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx).await;
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("task_id".to_string(), json!("task-2")),
        ]);

        let err = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(
            err,
            "task_id override is not allowed from this planner chat"
        );
    }

    #[tokio::test]
    async fn merge_agent_uses_bound_task_when_no_args() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(gcx.clone(), &source, test_card("wt", "agent", &source)).await;
        let ccx = planner_ccx(gcx).await;
        let args = HashMap::from([("card_id".to_string(), json!("missing-card"))]);

        let err = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(err, "Card missing-card not found");
    }

    #[tokio::test]
    async fn merge_agent_allows_with_force_when_launch_failed() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let cache_dir = gcx.cache_dir.clone();
        let service = WorktreeService::new(cache_dir, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-1/agent".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();
        commit_file(
            &root,
            "file.txt",
            "merged after human review\n",
            "agent change",
        );
        let mut card = test_card(&created.worktree.meta.id, &branch, &root);
        card.verifier_report = Some(launch_failure_verifier_report());
        write_task(gcx.clone(), &source, card).await;

        let result = merge_registered_task_worktree(
            gcx.clone(),
            &source.canonicalize().unwrap(),
            "task-1",
            "T-1",
            "squash",
            "tool-call",
            Some("task merge".to_string()),
            true,
            false,
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "merged after human review\n"
        );
        let ContextEnum::ChatMessage(message) = &result.1[0] else {
            panic!("expected chat message")
        };
        let ChatContent::SimpleText(text) = &message.content else {
            panic!("expected simple text")
        };
        assert!(text.contains("Verifier Warning"));
        assert!(text.contains("requires human review"));
        assert!(text.contains("force=true"));
        assert!(text.contains("Verifier failed to launch"));
    }

    #[tokio::test]
    async fn merge_agent_refuses_legacy_worktree_pointing_at_main_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        std::fs::write(source.join("main-dirty.txt"), "main dirty\n").unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, "main", &source),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let err = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(err.contains("main workspace"), "{err}");
        assert_eq!(
            run_git(&source, &["rev-list", "--count", "HEAD"]).trim(),
            "1"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("main-dirty.txt")).unwrap(),
            "main dirty\n"
        );
    }

    #[tokio::test]
    async fn merge_agent_refuses_legacy_worktree_pointing_inside_main_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(source.join("src")).unwrap();
        init_repo(&source);
        std::fs::write(source.join("src/main-dirty.txt"), "main dirty\n").unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, "main", &source.join("src")),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let err = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(err.contains("not the git worktree root"), "{err}");
        assert_eq!(
            run_git(&source, &["rev-list", "--count", "HEAD"]).trim(),
            "1"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("src/main-dirty.txt")).unwrap(),
            "main dirty\n"
        );
    }

    #[test]
    fn merge_agent_legacy_validation_rejects_detached_and_wrong_branch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-validate");
        let branch = "refact/task/task-1/card/T-1/agent-validate";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);

        run_git(&agent_worktree, &["checkout", "-b", "other-agent-branch"]);
        let err = ensure_legacy_agent_worktree_checkout(
            &source,
            agent_worktree.to_str().unwrap(),
            branch,
        )
        .unwrap_err();
        assert!(err.contains("different branch"), "{err}");

        let head = run_git(&agent_worktree, &["rev-parse", "HEAD"]);
        run_git(&agent_worktree, &["checkout", "--detach", head.trim()]);
        let err = ensure_legacy_agent_worktree_checkout(
            &source,
            agent_worktree.to_str().unwrap(),
            branch,
        )
        .unwrap_err();
        assert!(err.contains("detached HEAD"), "{err}");
    }

    #[tokio::test]
    async fn merge_agent_with_commits_ahead_and_dirty_worktree_auto_commits_and_cleans_up() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-committed-dirty");
        let branch = "refact/task/task-1/card/T-1/agent-committed-dirty";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        run_git(
            &agent_worktree,
            &["commit", "--allow-empty", "-m", "agent empty change"],
        );
        std::fs::write(agent_worktree.join("dirty.txt"), "uncommitted\n").unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, branch, &agent_worktree),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let result = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(source.join("dirty.txt")).unwrap(),
            "uncommitted\n"
        );
        assert!(!agent_worktree.exists());
        assert!(!branch_exists(&source, branch));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(tool_text(&result).contains("cleaned up"));
    }

    #[tokio::test]
    async fn merge_agent_with_commits_ahead_and_clean_worktree_cleans_up() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-committed-clean");
        let branch = "refact/task/task-1/card/T-1/agent-committed-clean";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        run_git(
            &agent_worktree,
            &["commit", "--allow-empty", "-m", "agent empty change"],
        );

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, branch, &agent_worktree),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let result = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert!(!agent_worktree.exists());
        assert!(!branch_exists(&source, branch));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(tool_text(&result).contains("cleaned up"));
    }

    #[tokio::test]
    async fn merge_agent_legacy_regular_merge_ignores_gpgsign_config() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-regular-gpg");
        let branch = "refact/task/task-1/card/T-1/agent-regular-gpg";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        run_git(
            &agent_worktree,
            &["commit", "--allow-empty", "-m", "agent empty change"],
        );
        run_git(&source, &["config", "commit.gpgsign", "true"]);

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, branch, &agent_worktree),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("strategy".to_string(), json!("merge")),
        ]);

        let result = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert!(tool_text(&result).contains("merged"));
        assert!(!agent_worktree.exists());
        assert!(!branch_exists(&source, branch));
    }

    #[tokio::test]
    async fn merge_agent_legacy_squash_uses_refact_author_without_repo_identity() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-squash-no-identity");
        let branch = "refact/task/task-1/card/T-1/agent-squash-no-identity";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        commit_file(
            &agent_worktree,
            "squash-no-identity.txt",
            "agent change\n",
            "agent file change",
        );
        run_git(&source, &["config", "--unset", "user.email"]);
        run_git(&source, &["config", "--unset", "user.name"]);

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, branch, &agent_worktree),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let result = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert!(tool_text(&result).contains("merged"));
        assert_eq!(
            run_git(&source, &["log", "-1", "--format=%an <%ae>"]).trim(),
            "Refact Agent <agent@refact.ai>"
        );
        assert!(!agent_worktree.exists());
        assert!(!branch_exists(&source, branch));
    }

    #[tokio::test]
    async fn merge_agent_auto_commits_dirty_zero_commit_legacy_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree");
        let branch = "refact/task/task-1/card/T-1/agent";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        std::fs::write(agent_worktree.join("dirty.txt"), "uncommitted\n").unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, branch, &agent_worktree),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let result = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(source.join("dirty.txt")).unwrap(),
            "uncommitted\n"
        );
        assert!(!agent_worktree.exists());
        assert!(!branch_exists(&source, branch));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        let text = tool_text(&result);
        assert!(text.contains("Merged") || text.contains("merged"));
    }

    #[tokio::test]
    async fn merge_agent_legacy_dirty_main_blocks_before_agent_auto_commit() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-main-dirty");
        let branch = "refact/task/task-1/card/T-1/agent-main-dirty";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        std::fs::write(agent_worktree.join("dirty.txt"), "agent dirty\n").unwrap();
        std::fs::write(source.join("main-dirty.txt"), "main dirty\n").unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, branch, &agent_worktree),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let err = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(
            err.contains("Main workspace has uncommitted changes"),
            "{err}"
        );
        assert_eq!(
            run_git(&agent_worktree, &["rev-list", "--count", "HEAD"]).trim(),
            "1"
        );
        assert!(run_git(&agent_worktree, &["status", "--porcelain"]).contains("dirty.txt"));
    }

    #[tokio::test]
    async fn merge_agent_cleans_empty_zero_commit_legacy_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-clean");
        let branch = "refact/task/task-1/card/T-1/agent-clean";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, branch, &agent_worktree),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let result = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert!(!agent_worktree.exists());
        assert!(!branch_exists(&source, branch));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(tool_text(&result).contains("Cleanup:"));
    }

    async fn write_task(gcx: Arc<GlobalContext>, source: &Path, card: BoardCard) {
        let task_dir = source.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let cards_done = if card.column == "done" { 1 } else { 0 };
        let cards_failed = if card.column == "failed" || card.column == "regressed" {
            1
        } else {
            0
        };
        let agents_active = if card.column == "doing" && card.assignee.is_some() {
            1
        } else {
            0
        };
        let meta = TaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done,
            cards_failed,
            agents_active,
            base_branch: Some("main".to_string()),
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        let mut board = TaskBoard::default();
        board.cards.push(card);
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&meta).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(
            task_dir.join("board.yaml"),
            serde_yaml::to_string(&board).unwrap(),
        )
        .await
        .unwrap();
        set_workspace(gcx, source).await;
    }

    #[tokio::test]
    async fn worktree_merge_merge_agent_uses_service_and_clears_board_mirrors() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let cache_dir = gcx.cache_dir.clone();
        let service = WorktreeService::new(cache_dir, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-1/agent".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();
        commit_file(&root, "file.txt", "merged by task tool\n", "agent change");
        write_task(
            gcx.clone(),
            &source,
            test_card(&created.worktree.meta.id, &branch, &root),
        )
        .await;

        let result = merge_registered_task_worktree(
            gcx.clone(),
            &source.canonicalize().unwrap(),
            "task-1",
            "T-1",
            "squash",
            "tool-call",
            Some("task merge".to_string()),
            false,
            false,
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap();

        assert!(result.is_some());
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "merged by task tool\n"
        );
        assert!(!root.exists());
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_err());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(card.target_files.contains(&"file.txt".to_string()));
    }

    #[tokio::test]
    async fn merge_agent_registered_path_auto_commits_dirty_worktree_and_cleans_up() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let cache_dir = gcx.cache_dir.clone();
        let service = WorktreeService::new(cache_dir, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-1/agent-dirty".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();
        commit_file(&root, "file.txt", "registered committed\n", "agent change");
        std::fs::write(root.join("dirty.txt"), "registered dirty\n").unwrap();
        write_task(
            gcx.clone(),
            &source,
            test_card(&created.worktree.meta.id, &branch, &root),
        )
        .await;

        let result = merge_registered_task_worktree(
            gcx.clone(),
            &source.canonicalize().unwrap(),
            "task-1",
            "T-1",
            "squash",
            "tool-call",
            Some("task merge".to_string()),
            false,
            false,
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap();

        assert!(result.is_some());
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "registered committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("dirty.txt")).unwrap(),
            "registered dirty\n"
        );
        assert!(!root.exists());
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_err());
        assert!(!branch_exists(&source, &branch));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(card.target_files.contains(&"dirty.txt".to_string()));
    }

    #[tokio::test]
    async fn merge_agent_registered_path_validates_checkout_before_diff_generation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let cache_dir = gcx.cache_dir.clone();
        let service = WorktreeService::new(cache_dir, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-1/agent-wrong-checkout".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();
        run_git(&root, &["checkout", "-b", "manual-task-branch"]);
        std::fs::write(root.join("wrong-branch.txt"), "do not diff\n").unwrap();
        write_task(
            gcx.clone(),
            &source,
            test_card(&created.worktree.meta.id, &branch, &root),
        )
        .await;

        let err = merge_registered_task_worktree(
            gcx.clone(),
            &source.canonicalize().unwrap(),
            "task-1",
            "T-1",
            "squash",
            "tool-call",
            None,
            false,
            false,
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();

        assert!(err.contains("different branch"), "{err}");
        assert_eq!(
            run_git(&root, &["branch", "--show-current"]).trim(),
            "manual-task-branch"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("wrong-branch.txt")).unwrap(),
            "do not diff\n"
        );
    }

    #[tokio::test]
    async fn worktree_merge_registered_error_does_not_fall_back_to_legacy_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp
            .path()
            .join("cache")
            .join("worktrees")
            .join("legacy-agent");
        std::fs::create_dir_all(agent_worktree.parent().unwrap()).unwrap();
        let branch = "refact/task/task-1/card/T-1/legacy-fallback";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        commit_file(
            &agent_worktree,
            "file.txt",
            "legacy fallback\n",
            "legacy change",
        );

        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            temp.path().join("cache"),
            temp.path().join("config"),
        )
        .await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(Some("missing-registry-id"), branch, &agent_worktree),
        )
        .await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let head_before = run_git(&source, &["rev-parse", "HEAD"]);
        let err = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(!err.is_empty());
        assert_eq!(run_git(&source, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "hello\n"
        );
        assert!(agent_worktree.exists());
        assert!(branch_exists(&source, branch));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.agent_branch.as_deref(), Some(branch));
        assert!(card.agent_worktree.is_some());
        assert_eq!(
            card.agent_worktree_name.as_deref(),
            Some("missing-registry-id")
        );
    }

    #[tokio::test]
    async fn worktree_merge_registry_reference_match_is_accepted_without_card_meta() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let cache_dir = gcx.cache_dir.clone();
        let service = WorktreeService::new(cache_dir, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-1/reference-match".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut registry = service.load_registry().await.unwrap();
        registry.records[0].meta.task_id = None;
        registry.records[0].meta.card_id = None;
        service.save_registry(&registry).await.unwrap();
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();
        commit_file(&root, "file.txt", "reference match\n", "agent change");
        write_task(
            gcx.clone(),
            &source,
            test_card(&created.worktree.meta.id, &branch, &root),
        )
        .await;

        let result = merge_registered_task_worktree(
            gcx.clone(),
            &source.canonicalize().unwrap(),
            "task-1",
            "T-1",
            "squash",
            "tool-call",
            Some("reference merge".to_string()),
            false,
            false,
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "reference match\n"
        );
        assert!(tool_text(&result).contains("merged"));
    }
    #[test]
    fn merge_agent_refuses_doing_card_without_force() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "doing".to_string();

        let err = ensure_card_done_allows_merge(&card, false).unwrap_err();

        assert!(err.contains("not 'done'"), "{err}");
        assert!(err.contains("doing"), "{err}");
        assert!(err.contains("force=true"), "{err}");
    }

    #[test]
    fn merge_agent_refuses_failed_card_without_force() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "failed".to_string();

        let err = ensure_card_done_allows_merge(&card, false).unwrap_err();

        assert!(err.contains("not 'done'"), "{err}");
        assert!(err.contains("failed"), "{err}");
        assert!(err.contains("force=true"), "{err}");
    }

    #[test]
    fn merge_agent_refuses_done_card_with_empty_final_report_without_force() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.final_report = None;

        let err = ensure_card_done_allows_merge(&card, false).unwrap_err();

        assert!(err.contains("no final report"), "{err}");
        assert!(err.contains("force=true"), "{err}");
    }

    #[test]
    fn merge_agent_allows_done_card_with_final_report() {
        let card = test_card("wt", "agent", Path::new("/tmp/worktree"));

        assert!(ensure_card_done_allows_merge(&card, false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn merge_agent_force_finalize_clears_current_ownership() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));

        let changed =
            finalize_force_merge_card_if_current(&mut card, Some("agent-1"), Some("agent-chat-1"));

        assert!(changed);
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert_eq!(card.column, "done");
    }

    #[test]
    fn merge_agent_force_finalize_preserves_newer_ownership() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "doing".to_string();
        card.completed_at = None;
        card.assignee = Some("agent-2".to_string());
        card.agent_chat_id = Some("agent-chat-2".to_string());

        let changed =
            finalize_force_merge_card_if_current(&mut card, Some("agent-1"), Some("agent-chat-1"));

        assert!(!changed);
        assert_eq!(card.assignee.as_deref(), Some("agent-2"));
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-chat-2"));
        assert_eq!(card.column, "doing");
        assert!(card.completed_at.is_none());
        assert!(card.status_updates.is_empty());
    }

    #[test]
    fn merge_agent_force_finalize_marks_active_card_done_with_audit() {
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "doing".to_string();
        card.final_report = None;
        card.completed_at = None;

        let changed =
            finalize_force_merge_card_if_current(&mut card, Some("agent-1"), Some("agent-chat-1"));

        assert!(changed);
        assert_eq!(card.column, "done");
        assert!(card.completed_at.is_some());
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.status_updates.iter().any(|update| update
            .message
            .contains("Force-merged by planner without the normal agent_finish path")));
    }

    #[tokio::test]
    async fn registered_force_merge_finalizes_after_post_merge_check_passes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "doing".to_string();
        card.completed_at = None;
        write_task(gcx.clone(), &source, card).await;
        let mut response = merged_response_with_cleanup_warning();
        response.cleanup.as_mut().unwrap().branch_deleted = true;

        clear_board_mirrors_after_registered_merge(
            gcx.clone(),
            "task-1",
            "T-1",
            &response,
            true,
            true,
            false,
            None,
            None,
            Some("agent-1".to_string()),
            Some("agent-chat-1".to_string()),
        )
        .await
        .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "done");
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
    }

    #[tokio::test]
    async fn registered_force_merge_auto_revert_clears_stale_refs_without_finishing_card() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "doing".to_string();
        card.completed_at = None;
        write_task(gcx.clone(), &source, card).await;
        let mut response = merged_response_with_cleanup_warning();
        response.cleanup.as_mut().unwrap().branch_deleted = true;

        clear_board_mirrors_after_registered_merge(
            gcx.clone(),
            "task-1",
            "T-1",
            &response,
            true,
            false,
            true,
            None,
            None,
            Some("agent-1".to_string()),
            Some("agent-chat-1".to_string()),
        )
        .await
        .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "doing");
        assert!(card.completed_at.is_none());
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message.contains("Forced merge was auto-reverted")));
        assert!(!card
            .status_updates
            .iter()
            .any(|update| update.message.contains("marked done for manual review")));
    }

    #[tokio::test]
    async fn registered_force_merge_retains_typed_failure_without_finishing_card() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "doing".to_string();
        card.completed_at = None;
        write_task(gcx.clone(), &source, card).await;
        let mut response = merged_response_with_cleanup_warning();
        response.cleanup.as_mut().unwrap().branch_deleted = true;

        clear_board_mirrors_after_registered_merge(
            gcx.clone(),
            "task-1",
            "T-1",
            &response,
            true,
            false,
            false,
            Some("Forced merge remains applied, but post-merge verification did not pass: verification was denied by policy. Stale agent ownership was cleared; the merged work is retained and the card was not marked done.".to_string()),
            None,
            Some("agent-1".to_string()),
            Some("agent-chat-1".to_string()),
        )
        .await
        .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "doing");
        assert!(card.completed_at.is_none());
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message.contains("merged work is retained")));
    }

    #[test]
    fn force_merge_finalization_requires_typed_pass_or_absent_check() {
        use refact_runtime_api::post_merge_check::PostMergeCheckResult;
        use refact_tasks::types::VerificationOutcome;

        let result = |checked, outcome| PostMergeCheckResult {
            checked,
            outcome,
            auto_reverted: false,
            command: Some("cargo test".to_string()),
            exit_code: None,
            output_tail: String::new(),
            merge_commit: Some("abc".to_string()),
            revert_commit: None,
            fix_card_id: None,
            skipped_reason: None,
            revert_skipped_reason: None,
        };

        assert!(force_merge_post_check_passed_or_absent(&Ok(None)));
        assert!(force_merge_post_check_passed_or_absent(&Ok(Some(result(
            true,
            VerificationOutcome::Passed,
        )))));
        for outcome in [
            VerificationOutcome::CommandFailed,
            VerificationOutcome::Rejected,
            VerificationOutcome::PolicyDenied,
            VerificationOutcome::InfrastructureFailed,
            VerificationOutcome::NoCommands,
        ] {
            let checked_failure = Ok(Some(result(true, outcome)));
            assert!(!force_merge_post_check_passed_or_absent(&checked_failure));
            let audit = retained_post_merge_verification_failure(&checked_failure).unwrap();
            assert!(audit.contains("merged work is retained"), "{audit}");
        }
        assert!(!force_merge_post_check_passed_or_absent(&Ok(Some(result(
            false,
            VerificationOutcome::Passed,
        )))));
        assert!(!force_merge_post_check_passed_or_absent(&Err(
            "verification unavailable".to_string()
        )));
    }

    #[test]
    fn legacy_typed_failure_without_revert_clears_ownership_but_does_not_finalize() {
        use refact_runtime_api::post_merge_check::PostMergeCheckResult;
        use refact_tasks::types::VerificationOutcome;

        let result = Ok(Some(PostMergeCheckResult {
            checked: true,
            outcome: VerificationOutcome::InfrastructureFailed,
            auto_reverted: false,
            command: Some("cargo test".to_string()),
            exit_code: None,
            output_tail: "runner unavailable".to_string(),
            merge_commit: Some("abc".to_string()),
            revert_commit: None,
            fix_card_id: None,
            skipped_reason: None,
            revert_skipped_reason: Some("not deterministic".to_string()),
        }));
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "doing".to_string();
        card.completed_at = None;

        let message = retained_post_merge_verification_failure(&result).unwrap();
        assert!(!force_merge_post_check_passed_or_absent(&result));
        assert!(retain_force_merge_without_finalizing(&mut card, &message));
        assert_eq!(card.column, "doing");
        assert!(card.completed_at.is_none());
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.status_updates[0]
            .message
            .contains("merged work is retained"));
    }

    #[tokio::test]
    async fn registered_post_merge_check_error_still_reconciles_cleanup_without_finalizing() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = test_card("wt", "agent", Path::new("/tmp/worktree"));
        card.column = "doing".to_string();
        card.completed_at = None;
        write_task(gcx.clone(), &source, card).await;
        let mut response = merged_response_with_cleanup_warning();
        response.cleanup.as_mut().unwrap().branch_deleted = true;

        // finalize_force_merge=false models an errored verification result: cleanup has
        // already happened in git, so its mirrors must be reconciled before returning it.
        clear_board_mirrors_after_registered_merge(
            gcx.clone(),
            "task-1",
            "T-1",
            &response,
            true,
            false,
            false,
            None,
            Some("verification service unavailable".to_string()),
            Some("agent-1".to_string()),
            Some("agent-chat-1".to_string()),
        )
        .await
        .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "doing");
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(card.status_updates.iter().any(|update| update
            .message
            .contains("post-merge verification failed to run")));
    }

    #[tokio::test]
    async fn registered_non_force_cleanup_preserves_rebound_card_pointers() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = test_card("wt-new", "agent-new", Path::new("/tmp/worktree-new"));
        card.assignee = Some("agent-2".to_string());
        card.agent_chat_id = Some("agent-chat-2".to_string());
        write_task(gcx.clone(), &source, card).await;
        let mut response = merged_response_with_cleanup_warning();
        response.cleanup.as_mut().unwrap().branch_deleted = true;

        clear_board_mirrors_after_registered_merge(
            gcx.clone(),
            "task-1",
            "T-1",
            &response,
            false,
            false,
            false,
            None,
            None,
            Some("agent-1".to_string()),
            Some("agent-chat-1".to_string()),
        )
        .await
        .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.assignee.as_deref(), Some("agent-2"));
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-chat-2"));
        assert_eq!(card.agent_branch.as_deref(), Some("agent-new"));
        assert_eq!(card.agent_worktree.as_deref(), Some("/tmp/worktree-new"));
        assert_eq!(card.agent_worktree_name.as_deref(), Some("wt-new"));
    }

    #[tokio::test]
    async fn legacy_squash_nothing_to_commit_does_not_verify_or_revert_existing_head() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-squash-noop");
        let branch = "refact/task/task-1/card/T-1/squash-noop";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        commit_file(
            &agent_worktree,
            "file.txt",
            "temporary\n",
            "temporary change",
        );
        commit_file(
            &agent_worktree,
            "file.txt",
            "hello\n",
            "restore base content",
        );

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        write_task(
            gcx.clone(),
            &source,
            test_card_with_branch(None, branch, &agent_worktree),
        )
        .await;
        let head_before = run_git(&source, &["rev-parse", "HEAD"]);
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("strategy".to_string(), json!("squash")),
            ("auto_revert".to_string(), json!(true)),
        ]);

        let result = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert_eq!(run_git(&source, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "hello\n"
        );
        assert!(tool_text(&result).contains("Nothing to Merge"));
        assert!(!tool_text(&result).contains("Post-merge check"));
        assert!(!agent_worktree.exists());
        assert!(!branch_exists(&source, branch));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
    }

    #[tokio::test]
    async fn merge_agent_force_true_allows_doing_card_with_warning() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let cache_dir = gcx.cache_dir.clone();
        let service = WorktreeService::new(cache_dir, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-1/agent-force-doing".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();
        commit_file(&root, "file.txt", "force doing\n", "agent change");
        let mut card = test_card(&created.worktree.meta.id, &branch, &root);
        card.column = "doing".to_string();
        card.final_report = None;
        write_task(gcx.clone(), &source, card).await;

        let result = merge_registered_task_worktree(
            gcx.clone(),
            &source.canonicalize().unwrap(),
            "task-1",
            "T-1",
            "squash",
            "tool-call",
            Some("force merge doing".to_string()),
            true,
            false,
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "force doing\n"
        );
        let text = tool_text(&result);
        assert!(text.contains("Force-merged"), "{text}");

        assert!(!root.exists());
        assert!(!branch_exists(&source, &branch));
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_err());

        let board = storage::load_board(gcx.clone(), "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "done");
        assert!(card.completed_at.is_some());
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(card.status_updates.iter().any(|update| update
            .message
            .contains("Force-merged by planner without the normal agent_finish path")));
        let ready = board.get_ready_cards();
        assert!(ready.in_progress.is_empty());
        assert_eq!(ready.completed, vec!["T-1".to_string()]);
        let meta = storage::load_task_meta(gcx, "task-1").await.unwrap();
        assert_eq!(meta.cards_done, 1);
        assert_eq!(meta.agents_active, 0);
    }

    #[tokio::test]
    async fn merge_agent_legacy_force_true_finishes_doing_card() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let agent_worktree = temp.path().join("agent-worktree-force-doing");
        let branch = "refact/task/task-1/card/T-1/agent-force-doing-legacy";
        create_legacy_agent_worktree(&source, &agent_worktree, branch);
        commit_file(
            &agent_worktree,
            "file.txt",
            "legacy force doing\n",
            "agent change",
        );

        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let mut card = test_card_with_branch(None, branch, &agent_worktree);
        card.column = "doing".to_string();
        card.final_report = None;
        card.completed_at = None;
        write_task(gcx.clone(), &source, card).await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("force".to_string(), json!(true)),
        ]);

        let result = ToolTaskMergeAgent::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "legacy force doing\n"
        );
        assert!(tool_text(&result).contains("Force-merged"));
        assert!(!agent_worktree.exists());
        assert!(!branch_exists(&source, branch));

        let board = storage::load_board(gcx.clone(), "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "done");
        assert!(card.completed_at.is_some());
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.agent_branch.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_worktree_name.is_none());
        assert!(card.status_updates.iter().any(|update| update
            .message
            .contains("Force-merged by planner without the normal agent_finish path")));
        let ready = board.get_ready_cards();
        assert!(ready.in_progress.is_empty());
        assert_eq!(ready.completed, vec!["T-1".to_string()]);
        let meta = storage::load_task_meta(gcx, "task-1").await.unwrap();
        assert_eq!(meta.cards_done, 1);
        assert_eq!(meta.agents_active, 0);
    }

    #[tokio::test]
    async fn merge_agent_force_true_allows_failed_card_with_warning() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let cache_dir = gcx.cache_dir.clone();
        let service = WorktreeService::new(cache_dir, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/task/task-1/card/T-1/agent-force-failed".to_string()),
                kind: Some("task_agent".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();
        commit_file(&root, "file.txt", "force failed\n", "agent change");
        let mut card = test_card(&created.worktree.meta.id, &branch, &root);
        card.column = "failed".to_string();
        card.final_report = None;
        write_task(gcx.clone(), &source, card).await;

        let result = merge_registered_task_worktree(
            gcx,
            &source.canonicalize().unwrap(),
            "task-1",
            "T-1",
            "squash",
            "tool-call",
            Some("force merge failed".to_string()),
            true,
            false,
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "force failed\n"
        );
        let text = tool_text(&result);
        assert!(text.contains("Force-merged"), "{text}");
    }
}
