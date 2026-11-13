pub use refact_tools::tools::tool_task_agent_finish::*;


#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use refact_chat_api::verifier::ExpectedCardState;
#[cfg(test)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
use refact_tasks::types::FinalReport;
#[cfg(test)]
use refact_worktrees::types::WorktreeMeta;


#[cfg(test)]
mod tests {
    use super::*;
    use refact_tasks::types::{
        AbVariantInfo, AbVariants, BoardCard, TaskBoard, TaskMeta as StoredTaskMeta, TaskStatus,
        TeamStatus,
    };

    fn run_git(cwd: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
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
        run_git(root, &["checkout", "-B", "main"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        run_git(root, &["add", "file.txt"]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    fn test_card(worktree: Option<String>) -> BoardCard {
        BoardCard {
            id: "T-1".to_string(),
            title: "Card T-1".to_string(),
            column: "doing".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: Some("agent-1".to_string()),
            agent_chat_id: Some("agent-chat-1".to_string()),
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: Utc::now().to_rfc3339(),
            started_at: Some(Utc::now().to_rfc3339()),
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: Some("legacy-branch".to_string()),
            agent_worktree: worktree,
            agent_worktree_name: Some("legacy-id".to_string()),
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    fn ab_variant(key: &str) -> AbVariantInfo {
        AbVariantInfo {
            agent_id: format!("agent-{}", key),
            chat_id: format!("agent-chat-{}", key),
            worktree: format!("/tmp/agent-{}", key),
            worktree_name: Some(format!("worktree-{}", key)),
            branch: Some(format!("branch-{}", key)),
            model: Some(format!("model-{}", key)),
            finish: None,
        }
    }

    fn ab_card() -> BoardCard {
        let mut card = test_card(None);
        card.assignee = Some("ab".to_string());
        card.agent_chat_id = None;
        card.agent_branch = None;
        card.agent_worktree = None;
        card.agent_worktree_name = None;
        card.ab_variants = Some(AbVariants {
            a: ab_variant("a"),
            b: ab_variant("b"),
            winner: None,
        });
        card
    }

    fn parsed_report(summary: &str, success: bool) -> ParsedFinishReport {
        let structured = FinalReport {
            summary: summary.to_string(),
            success,
            ..Default::default()
        };
        ParsedFinishReport {
            markdown: structured.to_markdown(),
            structured: Some(structured),
        }
    }

    fn sample_worktree_meta(temp: &Path) -> WorktreeMeta {
        let root = temp.join("thread-worktree");
        let source = temp.join("source");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        WorktreeMeta {
            id: "thread-id".to_string(),
            kind: "task_agent".to_string(),
            root,
            source_workspace_root: source.clone(),
            repo_root: source,
            branch: Some("thread-branch".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: Some("base".to_string()),
            task_id: Some("task-1".to_string()),
            card_id: Some("T-1".to_string()),
            agent_id: Some("agent-1".to_string()),
            enforce: true,
        }
    }

    async fn write_task(
        root: &Path,
        gcx: Arc<crate::global_context::GlobalContext>,
        card: BoardCard,
    ) {
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        let now = Utc::now().to_rfc3339();
        let meta = StoredTaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: Some("main".to_string()),
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        storage::save_task_meta(gcx.clone(), "task-1", &meta)
            .await
            .unwrap();
        storage::save_board(
            gcx,
            "task-1",
            &TaskBoard {
                cards: vec![card],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn finish_locks_pruned_after_completion() {
        let key = "task-1:T-1".to_string();

        ensure_lock_for("task-1", "T-1", async {
            assert!(get_finish_locks().lock().await.contains_key(&key));
        })
        .await;

        assert!(!get_finish_locks().lock().await.contains_key(&key));
    }

    #[tokio::test]
    async fn finish_lock_not_pruned_while_external_holder_exists() {
        let task_id = "race-task";
        let card_id = "T-Race";
        let key = format!("{}:{}", task_id, card_id);

        let external = get_finish_lock(task_id, card_id).await;

        ensure_lock_for(task_id, card_id, async {}).await;

        assert!(
            get_finish_locks().lock().await.contains_key(&key),
            "lock entry was pruned while another caller still held an Arc; \
             a concurrent waiter would have proceeded against a stale lock \
             while a new caller created a fresh one and ran in parallel"
        );

        drop(external);
        ensure_lock_for(task_id, card_id, async {}).await;

        assert!(
            !get_finish_locks().lock().await.contains_key(&key),
            "lock entry must be pruned once no external holder remains"
        );
    }

    #[test]
    fn agents_active_uses_chat_id_not_assignee() {
        let mut card = test_card(None);
        card.agent_chat_id = None;

        assert_eq!(agents_active(&[card]), 0);
    }

    #[tokio::test]
    async fn finish_heartbeat_rejects_restarted_card_owner() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut card = test_card(None);
        card.assignee = Some("agent-new".to_string());
        card.agent_chat_id = Some("agent-chat-new".to_string());
        write_task(temp.path(), gcx.clone(), card).await;

        let error = refresh_finish_heartbeat_if_current(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            "T-1",
            "agent-chat-1",
            Some("agent-1"),
        )
        .await
        .unwrap_err();

        assert!(error.contains("agent-chat-new"), "{error}");
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.last_heartbeat_at.is_none());
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-chat-new"));
    }

    #[tokio::test]
    async fn finish_heartbeat_accepts_current_owner() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        write_task(temp.path(), gcx.clone(), test_card(None)).await;

        refresh_finish_heartbeat_if_current(
            &gcx.chat_ctx(gcx.clone()),
            "task-1",
            "T-1",
            "agent-chat-1",
            Some("agent-1"),
        )
        .await
        .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.last_heartbeat_at.is_some());
    }

    #[test]
    fn spawn_agent_finish_prefers_thread_worktree_over_board_mirror() {
        let temp = tempfile::tempdir().unwrap();
        let meta = sample_worktree_meta(temp.path());
        let legacy_root = temp.path().join("legacy-root");
        let card = test_card(Some(legacy_root.to_string_lossy().to_string()));

        let resolved = resolve_agent_worktree(Some(meta.clone()), &card).unwrap();
        assert_eq!(resolved.root, meta.root);
        assert_eq!(resolved.branch.as_deref(), Some("thread-branch"));
        assert_eq!(resolved.name.as_deref(), Some("thread-id"));

        let legacy = resolve_agent_worktree(None, &card).unwrap();
        assert_eq!(legacy.root, legacy_root);
        assert_eq!(legacy.branch.as_deref(), Some("legacy-branch"));
        assert_eq!(legacy.name.as_deref(), Some("legacy-id"));
    }

    #[test]
    fn spawn_agent_finish_failure_retains_worktree_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp
            .path()
            .join("retained-worktree")
            .to_string_lossy()
            .to_string();
        let mut card = test_card(Some(worktree.clone()));
        let branch = card.agent_branch.clone();
        let name = card.agent_worktree_name.clone();
        let report = ParsedFinishReport {
            markdown: "agent failed".to_string(),
            structured: None,
        };

        mark_finished_card(&mut card, false, &report, None);
        clear_finished_agent_session(&mut card);

        assert_eq!(card.column, "failed");
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert_eq!(card.agent_worktree.as_deref(), Some(worktree.as_str()));
        assert_eq!(card.agent_branch, branch);
        assert_eq!(card.agent_worktree_name, name);
    }

    #[test]
    fn finish_failure_cleanup_preserves_restarted_owner() {
        let mut card = test_card(None);
        let report = ParsedFinishReport {
            markdown: "agent failed".to_string(),
            structured: None,
        };
        mark_finished_card(&mut card, false, &report, None);
        let expected = ExpectedCardState::from_card(1, &card);
        card.column = "doing".to_string();
        card.assignee = Some("agent-2".to_string());
        card.agent_chat_id = Some("agent-chat-2".to_string());

        let error = clear_finished_agent_session_if_current(&mut card, &expected, 2).unwrap_err();

        assert_eq!(error, "stale finish cleanup");
        assert_eq!(card.assignee.as_deref(), Some("agent-2"));
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-chat-2"));
    }

    #[test]
    fn spawn_ab_first_variant_finish_does_not_terminalize_shared_card() {
        let mut card = ab_card();
        let report = parsed_report("variant A done", true);

        let all_finished = mark_ab_variant_finished(
            &mut card,
            "a",
            "agent-chat-a",
            Some("agent-a"),
            true,
            &report,
            Some("abc123"),
        )
        .unwrap();

        assert!(!all_finished);
        assert_eq!(card.column, "doing");
        assert!(card.final_report.is_none());
        assert!(card.final_report_structured.is_none());
        assert!(card.completed_at.is_none());
        let variants = card.ab_variants.as_ref().unwrap();
        let finish = variants.a.finish.as_ref().unwrap();
        assert!(finish.success);
        assert_eq!(finish.final_report, report.markdown);
        assert_eq!(finish.commit_hash.as_deref(), Some("abc123"));
        assert!(variants.b.finish.is_none());
    }

    #[test]
    fn spawn_ab_both_variant_finishes_persist_separately() {
        let mut card = ab_card();
        let report_a = parsed_report("variant A done", true);
        let report_b = parsed_report("variant B failed", false);

        assert!(!mark_ab_variant_finished(
            &mut card,
            "a",
            "agent-chat-a",
            Some("agent-a"),
            true,
            &report_a,
            None,
        )
        .unwrap());
        assert!(mark_ab_variant_finished(
            &mut card,
            "b",
            "agent-chat-b",
            Some("agent-b"),
            false,
            &report_b,
            None,
        )
        .unwrap());

        assert_eq!(card.column, "doing");
        assert!(card.final_report.is_none());
        let variants = card.ab_variants.as_ref().unwrap();
        assert_eq!(
            variants.a.finish.as_ref().unwrap().final_report,
            report_a.markdown
        );
        assert_eq!(
            variants.b.finish.as_ref().unwrap().final_report,
            report_b.markdown
        );
        assert!(variants.a.finish.as_ref().unwrap().success);
        assert!(!variants.b.finish.as_ref().unwrap().success);
    }

    #[test]
    fn spawn_ab_variant_finish_rejects_second_finish() {
        let mut card = ab_card();
        let report = parsed_report("variant A done", true);

        mark_ab_variant_finished(
            &mut card,
            "a",
            "agent-chat-a",
            Some("agent-a"),
            true,
            &report,
            None,
        )
        .unwrap();
        let error = mark_ab_variant_finished(
            &mut card,
            "a",
            "agent-chat-a",
            Some("agent-a"),
            true,
            &report,
            None,
        )
        .unwrap_err();

        assert!(error.contains("already finished"), "{error}");
    }

    #[test]
    fn deterministic_agent_commit_message_is_bounded_single_line() {
        let long_title = format!("  Fix   frog\n{}  ", "x".repeat(300));

        let message = deterministic_agent_commit_message("T-1", &long_title);

        assert!(message.starts_with("Card T-1: Fix frog "));
        assert!(!message.contains('\n'));
        assert!(message.chars().count() <= "Card T-1: ".chars().count() + 160);
    }

    #[test]
    fn deterministic_agent_commit_message_sanitizes_control_chars_in_id_and_title() {
        let message = deterministic_agent_commit_message("T-1\n\0\x1b[31m", "Fix\x07 title");

        assert_eq!(message, "Card T-1 [31m: Fix title");
        assert!(!message.chars().any(|ch| ch.is_control()));
    }

    #[test]
    fn sanitize_commit_component_bounds_override_messages() {
        let message = sanitize_commit_component(&format!("Fix\n{}", "x".repeat(300)), 12);

        assert_eq!(message, "Fix xxxxxxxx");
        assert!(!message.chars().any(|ch| ch.is_control()));
    }

    #[test]
    fn tool_agent_finish_structured_object_populates_both_report_shapes() {
        let args = HashMap::from_iter([
            ("success".to_string(), json!(true)),
            (
                "report".to_string(),
                json!({
                    "summary": "Implemented structured finish reports.",
                    "success": false,
                    "files_changed": ["refact-agent/engine/src/tools/tool_agent_finish.rs"],
                    "tests_added_or_updated": ["tool_agent_finish_structured_object_populates_both_report_shapes"],
                    "verification": [{
                        "command": "cargo test --lib -p refact-lsp -- tool_agent_finish",
                        "exit_code": 0,
                        "passed": true,
                        "output_tail": "ok"
                    }],
                    "followup_cards": [{
                        "title": "GUI structured report rendering",
                        "instructions": "Render final_report_structured when present.",
                        "priority": "P2",
                        "target_files": ["refact-agent/gui/src/features/Tasks"]
                    }],
                    "risks": ["Planner still reads legacy markdown."],
                    "assumptions": ["Markdown fallback remains populated."]
                }),
            ),
        ]);
        let parsed = parse_finish_report(&args, true).unwrap();
        let mut card = test_card(None);

        mark_finished_card(&mut card, true, &parsed, None);

        let structured = card.final_report_structured.unwrap();
        assert!(structured.success);
        assert_eq!(structured.summary, "Implemented structured finish reports.");
        assert_eq!(
            structured.files_changed,
            vec!["refact-agent/engine/src/tools/tool_agent_finish.rs"]
        );
        let markdown = card.final_report.unwrap();
        assert!(markdown.contains("## Summary\nImplemented structured finish reports."));
        assert!(markdown.contains("## Files Changed"));
        assert!(markdown.contains("## Tests Added or Updated"));
        assert!(markdown.contains("## Verification"));
        assert!(markdown.contains("## Follow-up Cards"));
        assert!(markdown.contains("## Risks"));
        assert!(markdown.contains("## Assumptions"));
    }

    #[test]
    fn tool_agent_finish_string_with_optional_fields_builds_structured_report() {
        let args = HashMap::from_iter([
            ("success".to_string(), json!(true)),
            ("report".to_string(), json!("Summary from legacy field")),
            ("files_changed".to_string(), json!(["src/lib.rs"])),
            ("tests_added".to_string(), json!(["unit test"])),
            (
                "verification".to_string(),
                json!([{
                    "command": "cargo test",
                    "exit_code": 0,
                    "passed": true,
                    "output_tail": "ok"
                }]),
            ),
        ]);

        let parsed = parse_finish_report(&args, true).unwrap();
        let structured = parsed.structured.unwrap();

        assert_eq!(structured.summary, "Summary from legacy field");
        assert_eq!(structured.files_changed, vec!["src/lib.rs"]);
        assert_eq!(structured.tests_added_or_updated, vec!["unit test"]);
        assert!(parsed.markdown.contains("## Verification"));
    }

    #[test]
    fn agent_finish_accepts_yes_and_rejects_garbage_success_string() {
        let tru_args = HashMap::from_iter([
            ("success".to_string(), json!("tru")),
            ("report".to_string(), json!("invalid success")),
        ]);
        let yes_args = HashMap::from_iter([
            ("success".to_string(), json!("yes")),
            ("report".to_string(), json!("invalid success")),
        ]);

        let tru_error = parse_success_arg(&tru_args).unwrap_err();

        assert_eq!(
            tru_error,
            "Invalid 'success' string; expected true or false"
        );
        assert_eq!(parse_success_arg(&yes_args), Ok(true));
    }

    #[test]
    fn agent_finish_accepts_trimmed_boolean_strings() {
        let true_args = HashMap::from_iter([
            ("success".to_string(), json!(" true ")),
            ("report".to_string(), json!("success")),
        ]);
        let false_args = HashMap::from_iter([
            ("success".to_string(), json!(" false ")),
            ("report".to_string(), json!("failure")),
        ]);
        let bool_args = HashMap::from_iter([
            ("success".to_string(), json!(true)),
            ("report".to_string(), json!("boolean success")),
        ]);

        assert!(parse_success_arg(&true_args).unwrap());
        assert!(!parse_success_arg(&false_args).unwrap());
        assert!(parse_success_arg(&bool_args).unwrap());
    }

    #[tokio::test]
    async fn spawn_agent_finish_missing_worktree_returns_error() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-worktree");
        let result = auto_commit_worktree_with_message(
            &missing,
            "T-1",
            "Card T-1",
            Some("test commit".to_string()),
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[tokio::test]
    async fn spawn_agent_finish_non_git_worktree_returns_error() {
        let temp = tempfile::tempdir().unwrap();
        let non_git = temp.path().join("non-git");
        std::fs::create_dir_all(&non_git).unwrap();
        let result = auto_commit_worktree_with_message(
            &non_git,
            "T-1",
            "Card T-1",
            Some("test commit".to_string()),
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a git worktree/repo"));
    }

    #[tokio::test]
    async fn spawn_agent_finish_clean_worktree_returns_no_commit() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);

        let commit = auto_commit_worktree_with_message(
            &repo,
            "T-1",
            "Card T-1",
            Some("test commit".to_string()),
        )
        .await
        .unwrap();

        assert!(commit.is_none());
        assert!(run_git(&repo, &["status", "--porcelain"]).trim().is_empty());
    }

    #[tokio::test]
    async fn spawn_agent_finish_auto_commits_from_worktree_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let worktree = temp.path().join("agent-worktree");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "refact/task/task-1/card/T-1/agent",
                worktree.to_str().unwrap(),
            ],
        );
        std::fs::write(worktree.join("file.txt"), "changed in worktree\n").unwrap();
        let commit = auto_commit_worktree_with_message(
            &worktree,
            "T-1",
            "Card T-1",
            Some("test commit".to_string()),
        )
        .await
        .unwrap();

        assert!(commit.is_some());
        assert!(run_git(&worktree, &["status", "--porcelain"])
            .trim()
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "changed in worktree\n"
        );
    }

    #[tokio::test]
    async fn spawn_agent_finish_default_commit_message_does_not_need_subchat() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let worktree = temp.path().join("agent-worktree-default-message");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "refact/task/task-1/card/T-2/agent",
                worktree.to_str().unwrap(),
            ],
        );
        std::fs::write(
            worktree.join("file.txt"),
            "changed without generated message\n",
        )
        .unwrap();

        let commit = auto_commit_worktree_with_message(
            &worktree,
            "T-2",
            "  Implement    stable finish\nwithout subchat  ",
            None,
        )
        .await
        .unwrap();

        assert!(commit.is_some());
        let subject = run_git(&worktree, &["log", "-1", "--pretty=%s"]);
        assert_eq!(
            subject.trim(),
            "Card T-2: Implement stable finish without subchat"
        );
    }

    fn room_member(role: &str, chat: &str, status: TeamStatus) -> refact_tasks::types::TeamMember {
        refact_tasks::types::TeamMember {
            role: role.to_string(),
            agent_chat_id: Some(chat.to_string()),
            agent_id: Some(format!("agent-{}", chat)),
            member_status: Some(status),
            ..Default::default()
        }
    }

    fn room_card() -> BoardCard {
        let mut card = test_card(None);
        card.assignee = Some("agent-agent-coder-chat".to_string());
        card.agent_chat_id = Some("agent-coder-chat".to_string());
        card.team_members = vec![
            room_member("coder", "agent-coder-chat", TeamStatus::Running),
            room_member("reviewer", "agent-reviewer-chat", TeamStatus::Running),
        ];
        card
    }

    #[test]
    fn is_room_finisher_accepts_owner_and_any_room_member() {
        let card = room_card();

        assert!(is_room_finisher(&card, "agent-coder-chat"), "scalar owner");
        assert!(is_room_finisher(&card, "agent-reviewer-chat"), "room member");
        assert!(!is_room_finisher(&card, "agent-stranger"), "stranger");
    }

    #[test]
    fn mark_room_member_finished_records_only_that_member() {
        let mut card = room_card();
        let report = parsed_report("coder done", true);

        mark_room_member_finished(&mut card, "agent-coder-chat", true, &report, "2026-01-01T01:00:00Z").unwrap();

        assert_eq!(card.column, "doing", "one member finishing must not finish the card");
        assert!(card.final_report.is_none());
        assert!(card.completed_at.is_none());

        let coder = card.team_member_by_chat_id("agent-coder-chat").unwrap();
        assert_eq!(coder.typed_status(), TeamStatus::Done);
        assert!(coder.report.is_some());

        let reviewer = card.team_member_by_chat_id("agent-reviewer-chat").unwrap();
        assert_eq!(reviewer.typed_status(), TeamStatus::Running, "peer is untouched");
        assert!(reviewer.report.is_none());
    }

    #[test]
    fn mark_room_member_finished_rejects_second_finish() {
        let mut card = room_card();
        let report = parsed_report("done", true);

        mark_room_member_finished(&mut card, "agent-coder-chat", true, &report, "2026-01-01T01:00:00Z").unwrap();
        let error = mark_room_member_finished(&mut card, "agent-coder-chat", true, &report, "2026-01-01T01:00:00Z").unwrap_err();

        assert!(error.contains("already finished"), "{error}");
    }

    #[test]
    fn mark_room_member_finished_rejects_unknown_chat() {
        let mut card = room_card();
        let report = parsed_report("done", true);

        let error = mark_room_member_finished(&mut card, "agent-stranger", true, &report, "2026-01-01T01:00:00Z").unwrap_err();

        assert!(error.contains("no room member"), "{error}");
    }

    #[test]
    fn room_is_incomplete_while_a_member_is_running() {
        let mut card = room_card();
        let report = parsed_report("done", true);

        mark_room_member_finished(&mut card, "agent-coder-chat", true, &report, "2026-01-01T01:00:00Z").unwrap();

        let reports = refact_tasks::rooms::collect_room_reports(&card);
        assert!(!reports.room_complete());
        assert_eq!(reports.outstanding, vec!["reviewer".to_string()]);
        assert_eq!(card.column, "doing", "card is not finalized while incomplete");
    }

    #[test]
    fn finalize_room_card_is_done_when_every_member_succeeded() {
        let mut card = room_card();
        let report = parsed_report("done", true);

        mark_room_member_finished(&mut card, "agent-coder-chat", true, &report, "2026-01-01T01:00:00Z").unwrap();
        mark_room_member_finished(&mut card, "agent-reviewer-chat", true, &report, "2026-01-01T02:00:00Z").unwrap();

        let reports = refact_tasks::rooms::collect_room_reports(&card);
        assert!(reports.room_complete());
        finalize_room_card(&mut card, &reports, Some("abc123"));

        assert_eq!(card.column, "done");
        assert!(card.completed_at.is_some());
        let final_report = card.final_report.unwrap();
        assert!(final_report.contains("Room Report"));
        assert!(final_report.contains("team/agent-coder-chat"));
        assert!(final_report.contains("team/agent-reviewer-chat"));
    }

    #[test]
    fn finalize_room_card_is_failed_when_any_member_failed() {
        let mut card = room_card();
        let good = parsed_report("done", true);
        let bad = parsed_report("broke", false);

        mark_room_member_finished(&mut card, "agent-coder-chat", true, &good, "2026-01-01T01:00:00Z").unwrap();
        mark_room_member_finished(&mut card, "agent-reviewer-chat", false, &bad, "2026-01-01T02:00:00Z").unwrap();

        let reports = refact_tasks::rooms::collect_room_reports(&card);
        assert!(reports.room_complete());
        finalize_room_card(&mut card, &reports, None);

        assert_eq!(card.column, "failed", "one failed member fails the whole room");
        assert!(card.final_report.as_deref().unwrap().contains("reviewer"));
    }
}
