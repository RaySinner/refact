#[allow(unused_imports)]
use super::*;
use crate::tasks::types::BoardCard;
use std::path::PathBuf;
    use std::collections::VecDeque;

    use crate::tasks::types::{ScopeGuardMode, TaskBoard, TaskMeta, TaskStatus};

    #[derive(Default)]
    struct MockRunner {
        outputs: VecDeque<PostMergeCommandOutput>,
        calls: Vec<PostMergeCommand>,
    }

    struct MutatingRunner {
        outputs: VecDeque<PostMergeCommandOutput>,
        calls: Vec<PostMergeCommand>,
        gcx: Arc<GlobalContext>,
        mutated: bool,
    }

    #[async_trait]
    impl PostMergeCommandRunner for MockRunner {
        async fn run(
            &mut self,
            _workspace_root: &Path,
            command: PostMergeCommand,
            _timeout: Duration,
        ) -> PostMergeCommandOutput {
            self.calls.push(command);
            self.outputs
                .pop_front()
                .expect("mock output must be queued")
        }
    }

    #[async_trait]
    impl PostMergeCommandRunner for MutatingRunner {
        async fn run(
            &mut self,
            _workspace_root: &Path,
            command: PostMergeCommand,
            _timeout: Duration,
        ) -> PostMergeCommandOutput {
            let should_mutate = matches!(
                &command,
                PostMergeCommand::Git(args)
                    if args.first().map(String::as_str) == Some("rev-parse")
            );
            self.calls.push(command);
            if should_mutate && !self.mutated {
                storage::update_board_atomic(self.gcx.clone(), "task-1", |board| {
                    board.get_card_mut("T-1").unwrap().final_report =
                        Some("newer finish".to_string());
                    Ok(())
                })
                .await
                .unwrap();
                self.mutated = true;
            }
            self.outputs
                .pop_front()
                .expect("mock output must be queued")
        }
    }

    fn output(success: bool, exit_code: Option<i32>, text: &str) -> PostMergeCommandOutput {
        PostMergeCommandOutput {
            success,
            exit_code,
            output: text.to_string(),
            outcome: if success {
                VerificationOutcome::Passed
            } else if exit_code.is_some() {
                VerificationOutcome::CommandFailed
            } else {
                VerificationOutcome::InfrastructureFailed
            },
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn system_runner_timeout_kills_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("descendant-survived");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!("(sleep 1; touch '{}') & wait", marker.display()),
        ]);

        let result = run_command(command, "timeout process group", Duration::from_millis(50)).await;
        tokio::time::sleep(Duration::from_millis(1200)).await;

        assert_eq!(result.outcome, VerificationOutcome::InfrastructureFailed);
        assert!(result.output.contains("timed out"));
        assert!(!marker.exists(), "timed-out descendant survived");
    }

    fn card(instructions: &str) -> BoardCard {
        BoardCard {
            id: "T-1".to_string(),
            title: "Merge card".to_string(),
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
            target_files: vec!["src/lib.rs".to_string()],
            scope_guard_mode: ScopeGuardMode::Off,
            team_members: vec![],
        }
    }

    async fn write_task(gcx: Arc<GlobalContext>, root: &Path, card: BoardCard) {
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        let now = Utc::now().to_rfc3339();
        let meta = TaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
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
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
    }

    fn request(root: &Path) -> PostMergeCheckRequest {
        PostMergeCheckRequest {
            task_id: "task-1".to_string(),
            card_id: "T-1".to_string(),
            workspace_root: root.to_path_buf(),
            enabled: true,
            timeout: Duration::from_secs(5),
            expected_merge_commit: "mergehash".to_string(),
            expected_card_state: None,
        }
    }

    #[tokio::test]
    async fn post_merge_check_no_command_is_checked_failure_without_revert() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(gcx.clone(), temp.path(), card("No verify command")).await;
        let mut runner = MockRunner::default();

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.checked);
        assert!(!result.auto_reverted);
        assert_eq!(result.outcome, VerificationOutcome::NoCommands);
        assert_eq!(
            result.revert_skipped_reason.as_deref(),
            Some("verification was not a genuine command failure")
        );
        assert!(runner.calls.is_empty());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(board.get_card("T-1").unwrap().column, "done");
    }

    #[tokio::test]
    async fn post_merge_check_does_not_revert_when_verification_passes() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test --lib`"),
        )
        .await;
        let mut runner = MockRunner {
            outputs: VecDeque::from([output(true, Some(0), "ok")]),
            calls: Vec::new(),
        };

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.checked);
        assert!(!result.auto_reverted);
        assert_eq!(
            runner.calls,
            vec![PostMergeCommand::Verify {
                cwd: None,
                env: Vec::new(),
                argv: vec!["cargo".to_string(), "test".to_string(), "--lib".to_string()]
            }]
        );
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(board.get_card("T-1").unwrap().column, "done");
        assert!(board.get_card("T-1-fix").is_none());
    }

    #[tokio::test]
    async fn post_merge_runs_argv_not_shell() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cd refact-agent/engine && cargo check`"),
        )
        .await;
        let mut runner = MockRunner {
            outputs: VecDeque::from([output(true, Some(0), "ok")]),
            calls: Vec::new(),
        };

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.checked);
        assert_eq!(
            runner.calls,
            vec![PostMergeCommand::Verify {
                cwd: Some(PathBuf::from("refact-agent/engine")),
                env: Vec::new(),
                argv: vec!["cargo".to_string(), "check".to_string()]
            }]
        );
    }

    #[tokio::test]
    async fn post_merge_runs_previously_unsupported_binaries() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `dotnet test`"),
        )
        .await;
        let mut runner = MockRunner {
            outputs: VecDeque::from([output(true, Some(0), "ok")]),
            calls: Vec::new(),
        };

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.checked);
        assert_eq!(
            runner.calls,
            vec![PostMergeCommand::Verify {
                cwd: None,
                env: Vec::new(),
                argv: vec!["dotnet".to_string(), "test".to_string()]
            }]
        );
    }

    #[tokio::test]
    async fn post_merge_denies_command_matching_deny_rule() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `sudo rm -rf /`"),
        )
        .await;
        let mut runner = MockRunner::default();

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.checked);
        assert!(!result.auto_reverted);
        assert_eq!(result.outcome, VerificationOutcome::PolicyDenied);
        assert!(
            result.output_tail.starts_with("Denied by shell policy:"),
            "{}",
            result.output_tail
        );
        assert!(runner.calls.is_empty());
    }

    #[tokio::test]
    async fn post_merge_rejects_shell_syntax() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test | tee f`"),
        )
        .await;
        let mut runner = MockRunner::default();

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert_eq!(result.outcome, VerificationOutcome::Rejected);
        assert!(
            result.output_tail.starts_with("Cannot run as a command:"),
            "{}",
            result.output_tail
        );
        assert!(runner.calls.is_empty());
    }

    #[tokio::test]
    async fn post_merge_check_skips_revert_when_head_changed() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test --lib`"),
        )
        .await;
        let mut runner = MockRunner {
            outputs: VecDeque::from([
                output(false, Some(1), "running\nerror: regression failed\n"),
                output(true, Some(0), "newhead\n"),
            ]),
            calls: Vec::new(),
        };

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.checked);
        assert!(!result.auto_reverted);
        assert_eq!(result.merge_commit.as_deref(), Some("mergehash"));
        assert_eq!(
            result.revert_skipped_reason.as_deref(),
            Some("HEAD changed since merge")
        );
        assert_eq!(result.fix_card_id.as_deref(), Some("T-1-fix"));
        assert!(!runner.calls.iter().any(|call| matches!(
            call,
            PostMergeCommand::Git(args) if args.first().map(String::as_str) == Some("revert")
        )));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let original = board.get_card("T-1").unwrap();
        assert_eq!(original.column, "regressed");
        assert!(original
            .status_updates
            .iter()
            .any(|update| update.message == "Auto-revert skipped: HEAD changed since merge"));
        let fix = board.get_card("T-1-fix").unwrap();
        assert!(fix.instructions.contains("Current HEAD: newhead"));
    }

    #[tokio::test]
    async fn post_merge_check_skips_revert_when_dirty_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test --lib`"),
        )
        .await;
        let mut runner = MockRunner {
            outputs: VecDeque::from([
                output(false, Some(1), "running\nerror: regression failed\n"),
                output(true, Some(0), "mergehash\n"),
                output(true, Some(0), " M src/lib.rs\n"),
            ]),
            calls: Vec::new(),
        };

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.checked);
        assert!(!result.auto_reverted);
        assert_eq!(result.merge_commit.as_deref(), Some("mergehash"));
        assert_eq!(
            result.revert_skipped_reason.as_deref(),
            Some("workspace dirty since merge")
        );
        assert_eq!(result.fix_card_id.as_deref(), Some("T-1-fix"));
        assert!(!runner.calls.iter().any(|call| matches!(
            call,
            PostMergeCommand::Git(args) if args.first().map(String::as_str) == Some("revert")
        )));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let original = board.get_card("T-1").unwrap();
        assert_eq!(original.column, "regressed");
        assert!(original
            .status_updates
            .iter()
            .any(|update| update.message == "Auto-revert skipped: workspace dirty since merge"));
        let fix = board.get_card("T-1-fix").unwrap();
        assert!(fix.instructions.contains("Git status --porcelain"));
        assert!(fix.instructions.contains("M src/lib.rs"));
    }

    #[tokio::test]
    async fn post_merge_check_reverts_when_head_matches_and_clean() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test --lib`"),
        )
        .await;
        let mut runner = MockRunner {
            outputs: VecDeque::from([
                output(false, Some(1), "running\nerror: regression failed\n"),
                output(true, Some(0), "mergehash\n"),
                output(true, Some(0), ""),
                output(true, Some(0), "mergehash parenthash\n"),
                output(true, Some(0), "revert ok\n"),
                output(true, Some(0), "reverthash\n"),
            ]),
            calls: Vec::new(),
        };

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.auto_reverted);
        assert_eq!(result.merge_commit.as_deref(), Some("mergehash"));
        assert_eq!(result.revert_commit.as_deref(), Some("reverthash"));
        assert_eq!(result.fix_card_id.as_deref(), Some("T-1-fix"));
        assert!(runner.calls.contains(&PostMergeCommand::Git(vec![
            "revert".to_string(),
            "--no-edit".to_string(),
            "mergehash".to_string()
        ])));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let original = board.get_card("T-1").unwrap();
        assert_eq!(original.column, "regressed");
        assert!(original
            .status_updates
            .iter()
            .any(|update| update.message == "Auto-reverted: error: regression failed"));
        assert!(board.columns.iter().any(|column| column.id == "regressed"));
    }

    #[tokio::test]
    async fn post_merge_check_reverts_merge_commit_with_mainline() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test --lib`"),
        )
        .await;
        let mut runner = MockRunner {
            outputs: VecDeque::from([
                output(false, Some(1), "error: regression\n"),
                output(true, Some(0), "mergehash\n"),
                output(true, Some(0), ""),
                output(true, Some(0), "mergehash parent1 parent2\n"),
                output(true, Some(0), "revert ok\n"),
                output(true, Some(0), "reverthash\n"),
            ]),
            calls: Vec::new(),
        };

        let result = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap();

        assert!(result.auto_reverted);
        assert!(runner.calls.contains(&PostMergeCommand::Git(vec![
            "revert".to_string(),
            "-m".to_string(),
            "1".to_string(),
            "--no-edit".to_string(),
            "mergehash".to_string(),
        ])));
    }

    #[tokio::test]
    async fn post_merge_check_skips_stale_card_state_before_running_commands() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test --lib`"),
        )
        .await;
        let expected_card_state =
            crate::chat::verifier::load_expected_card_state(gcx.clone(), "task-1", "T-1")
                .await
                .unwrap();
        storage::update_board_atomic(gcx.clone(), "task-1", |board| {
            board.get_card_mut("T-1").unwrap().final_report = Some("newer finish".to_string());
            Ok(())
        })
        .await
        .unwrap();
        let mut req = request(temp.path());
        req.expected_card_state = Some(expected_card_state);
        let mut runner = MockRunner::default();

        let result = post_merge_check_with_runner(&gcx.chat_ctx(gcx.clone()), req, &mut runner)
            .await
            .unwrap();

        assert!(!result.checked);
        assert_eq!(result.skipped_reason.as_deref(), Some("stale card state"));
        assert!(runner.calls.is_empty());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(board.get_card("T-1").unwrap().column, "done");
        assert!(board.get_card("T-1-fix").is_none());
    }

    #[tokio::test]
    async fn post_merge_check_noops_regression_write_when_card_changed_after_failure() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test --lib`"),
        )
        .await;
        let expected_card_state =
            crate::chat::verifier::load_expected_card_state(gcx.clone(), "task-1", "T-1")
                .await
                .unwrap();
        let mut req = request(temp.path());
        req.expected_card_state = Some(expected_card_state);
        let mut runner = MutatingRunner {
            outputs: VecDeque::from([
                output(false, Some(1), "running\nerror: regression failed\n"),
                output(true, Some(0), "newhead\n"),
            ]),
            calls: Vec::new(),
            gcx: gcx.clone(),
            mutated: false,
        };

        let result = post_merge_check_with_runner(&gcx.chat_ctx(gcx.clone()), req, &mut runner)
            .await
            .unwrap();

        assert!(result.checked);
        assert_eq!(
            result.revert_skipped_reason.as_deref(),
            Some("HEAD changed since merge")
        );
        assert!(result.fix_card_id.is_none());
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let original = board.get_card("T-1").unwrap();
        assert_eq!(original.column, "done");
        assert_eq!(original.final_report.as_deref(), Some("newer finish"));
        assert!(board.get_card("T-1-fix").is_none());
    }

    #[tokio::test]
    async fn post_merge_check_persists_regression_when_revert_fails() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `cargo test --lib`"),
        )
        .await;
        let mut runner = MockRunner {
            outputs: VecDeque::from([
                output(false, Some(1), "running\nerror: regression failed\n"),
                output(true, Some(0), "mergehash\n"),
                output(true, Some(0), ""),
                output(true, Some(0), "mergehash parenthash\n"),
                output(false, Some(1), "CONFLICT in src/lib.rs\n"),
            ]),
            calls: Vec::new(),
        };

        let error = post_merge_check_with_runner(
            &gcx.chat_ctx(gcx.clone()),
            request(temp.path()),
            &mut runner,
        )
        .await
        .unwrap_err();

        assert!(
            error.contains("post-merge verification failed, and git revert failed"),
            "{error}"
        );
        assert!(runner.calls.contains(&PostMergeCommand::Git(vec![
            "revert".to_string(),
            "--no-edit".to_string(),
            "mergehash".to_string()
        ])));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let original = board.get_card("T-1").unwrap();
        assert_eq!(original.column, "regressed");
        assert!(original
            .status_updates
            .iter()
            .any(|update| update.message == "Auto-revert skipped: git revert failed"));
        let fix = board.get_card("T-1-fix").unwrap();
        assert!(fix
            .instructions
            .contains("Auto-revert was skipped: git revert failed"));
        assert!(fix.instructions.contains("Git revert output"));
        assert!(fix.instructions.contains("CONFLICT in src/lib.rs"));
    }

    #[tokio::test]
    async fn post_merge_check_fix_card_contains_revert_info() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(
            gcx.clone(),
            temp.path(),
            card("## Acceptance Criteria\n- Verify: `npm test`"),
        )
        .await;
        let mut req = request(temp.path());
        req.expected_merge_commit = "abc123".to_string();
        let mut runner = MockRunner {
            outputs: VecDeque::from([
                output(false, Some(1), "FAIL test_a\nexpected true\n"),
                output(true, Some(0), "abc123\n"),
                output(true, Some(0), ""),
                output(true, Some(0), "abc123 parenthash\n"),
                output(true, Some(0), "reverted\n"),
                output(true, Some(0), "def456\n"),
            ]),
            calls: Vec::new(),
        };

        post_merge_check_with_runner(&gcx.chat_ctx(gcx.clone()), req, &mut runner)
            .await
            .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let fix = board.get_card("T-1-fix").unwrap();
        assert_eq!(fix.title, "Fix regression in T-1");
        assert!(fix.instructions.contains("Merge commit: abc123"));
        assert!(fix.instructions.contains("Revert commit: def456"));
        assert!(fix.instructions.contains("Failing command: `npm test`"));
        assert!(fix.instructions.contains("FAIL test_a"));
        assert_eq!(fix.target_files, vec!["src/lib.rs".to_string()]);
    }
