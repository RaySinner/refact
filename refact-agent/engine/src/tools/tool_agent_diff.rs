pub use refact_tools::tools::tool_agent_diff::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_tasks::types::BoardCard;
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_worktrees::service::WorktreeService;
#[cfg(test)]
use refact_worktrees::types::WorktreeRecordView;
#[cfg(test)]
use refact_worktrees::types::WorktreeReference;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;

    #[test]
    fn downstream_base_resolution_prefers_card_over_task_legacy_fallback() {
        let base = resolve_base(
            None,
            None,
            Some("card-commit".to_string()),
            Some("card-branch".to_string()),
            Some("task-commit".to_string()),
            Some("task-branch".to_string()),
        )
        .unwrap();
        assert_eq!(base.refish, "card-commit");

        let legacy = resolve_base(
            None,
            None,
            None,
            None,
            Some("task-commit".to_string()),
            Some("task-branch".to_string()),
        )
        .unwrap();
        assert_eq!(legacy.refish, "task-commit");
    }
    use refact_tasks::types::{TaskBoard, TaskMeta, TaskStatus};
    use crate::tools::settings_guard::SettingsGuard;
    use crate::tools::tools_description::Tool;
    use serial_test::serial;
    use refact_worktrees::types::CreateWorktreeRequest;
    use std::process::Command as StdCommand;

    fn run_git(cwd: &Path, args: &[&str]) -> String {
        let output = StdCommand::new("git")
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
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        std::fs::write(root.join(".git").join("info").join("exclude"), ".refact/\n").unwrap();
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        run_git(root, &["add", "file.txt"]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    fn commit_file(root: &Path, name: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(name), content).unwrap();
        run_git(root, &["add", name]);
        run_git(root, &["commit", "-m", message]);
        run_git(root, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn legacy_repo_path(root: &Path) -> PathBuf {
        root.join("cache").join("worktrees").join("repo")
    }

    async fn make_gcx_for_root(root: &Path) -> Arc<crate::global_context::GlobalContext> {
        if let Some(worktrees_dir) = root.parent() {
            if worktrees_dir.file_name().and_then(|value| value.to_str()) == Some("worktrees") {
                if let Some(cache_dir) = worktrees_dir.parent() {
                    return crate::global_context::tests::make_test_gcx_with_dirs(
                        cache_dir.to_path_buf(),
                        cache_dir.join("config"),
                    )
                    .await;
                }
            }
        }
        crate::global_context::tests::make_test_gcx().await
    }

    fn test_card(branch: Option<String>, worktree: Option<String>) -> BoardCard {
        test_card_with_id("T-1", branch, worktree, None)
    }

    fn test_card_with_worktree_name(
        branch: Option<String>,
        worktree: Option<String>,
        worktree_name: Option<String>,
    ) -> BoardCard {
        test_card_with_id("T-1", branch, worktree, worktree_name)
    }

    fn test_card_with_id(
        id: &str,
        branch: Option<String>,
        worktree: Option<String>,
        worktree_name: Option<String>,
    ) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: "Diff card".to_string(),
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
            agent_branch: branch,
            agent_worktree: worktree,
            agent_worktree_name: worktree_name,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    fn write_file(root: &Path, path: &str, content: &str) {
        let full_path = root.join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full_path, content).unwrap();
    }

    fn assert_name_only_has(output: &str, expected: &[&str]) {
        let paths = output.lines().map(str::trim).collect::<Vec<_>>();
        for path in expected {
            assert!(paths.contains(path), "missing {path} in {output}");
        }
    }

    fn count_lines_with_prefix(output: &str, prefix: &str) -> usize {
        output
            .lines()
            .filter(|line| line.starts_with(prefix))
            .count()
    }

    async fn execute_diff(
        gcx: Arc<crate::global_context::GlobalContext>,
        mode: &str,
    ) -> Result<String, String> {
        execute_diff_with_max_lines(gcx, mode, 2000).await
    }

    async fn execute_diff_with_max_lines(
        gcx: Arc<crate::global_context::GlobalContext>,
        mode: &str,
        max_lines: usize,
    ) -> Result<String, String> {
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();
        let output = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!(mode)),
                    ("max_lines".to_string(), json!(max_lines)),
                ]),
            )
            .await
            .map(tool_output_text)?;
        Ok(output)
    }

    async fn create_registered_worktree(
        temp: &tempfile::TempDir,
        task_id: &str,
        card_id: &str,
    ) -> (
        Arc<crate::global_context::GlobalContext>,
        PathBuf,
        refact_worktrees::types::WorktreeRecordView,
    ) {
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let cache_dir = temp.path().join("cache");
        let config_dir = temp.path().join("config");
        let gcx =
            crate::global_context::tests::make_test_gcx_with_dirs(cache_dir, config_dir).await;
        let service =
            WorktreeService::new(gcx.cache_dir.clone(), source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some(format!("refact/task/{}/card/{}/agent", task_id, card_id)),
                kind: Some("task_agent".to_string()),
                task_id: Some(task_id.to_string()),
                card_id: Some(card_id.to_string()),
                agent_id: Some("agent-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        (gcx, source, created.worktree)
    }

    fn task_meta() -> TaskMeta {
        task_meta_with_base(Some("main"), None)
    }

    fn task_meta_with_base(base_branch: Option<&str>, base_commit: Option<&str>) -> TaskMeta {
        let now = chrono::Utc::now().to_rfc3339();
        TaskMeta {
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
            base_branch: base_branch.map(str::to_string),
            base_commit: base_commit.map(str::to_string),
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        }
    }

    async fn write_task(root: &Path, card: BoardCard) -> Arc<crate::global_context::GlobalContext> {
        write_task_with_meta(root, card, task_meta()).await
    }

    async fn write_task_with_meta(
        root: &Path,
        card: BoardCard,
        meta: TaskMeta,
    ) -> Arc<crate::global_context::GlobalContext> {
        let gcx = make_gcx_for_root(root).await;
        write_task_with_gcx(gcx, root, card, meta).await
    }

    async fn write_task_with_gcx(
        gcx: Arc<crate::global_context::GlobalContext>,
        root: &Path,
        card: BoardCard,
        meta: TaskMeta,
    ) -> Arc<crate::global_context::GlobalContext> {
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
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
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.canonicalize().unwrap()];
        gcx
    }

    async fn planner_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
        role: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
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
                    role: role.to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: None,
                }),
                None,
            )
            .await,
        ))
    }

    fn tool_output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    #[test]
    fn tool_agent_diff_description_is_correct() {
        let desc = ToolAgentDiff::new().tool_description();

        assert_eq!(desc.name, "agent_diff");
        assert_eq!(desc.input_schema["required"], json!(["card_id"]));
        assert_eq!(
            desc.input_schema["properties"]["mode"]["enum"],
            json!(["stat", "unified", "name-only"])
        );
        assert!(desc.description.contains("real git diff"));
    }

    #[tokio::test]
    async fn tool_agent_diff_rejects_non_planner_role() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), test_card(None, None)).await;
        let ccx = planner_ccx(gcx, "agents").await;
        let mut tool = ToolAgentDiff::new();
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by the task planner"));
    }

    #[tokio::test]
    async fn tool_agent_diff_missing_card_id_returns_error() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), test_card(None, None)).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();
        let args = HashMap::new();

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(err, "Missing 'card_id'");
    }

    #[tokio::test]
    async fn tool_agent_diff_rejects_mismatched_task_id() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), test_card(None, None)).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("task_id".to_string(), json!("task-2")),
        ]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(
            err,
            "task_id override is not allowed from this planner chat"
        );
    }

    #[tokio::test]
    async fn tool_agent_diff_git_diff_between_branches_works() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        commit_file(&repo, "file.txt", "hello\nagent\n", "agent change");
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task(&repo, card).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();

        let stat = tool_output_text(
            tool.tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("task_id".to_string(), json!("task-1")),
                    ("mode".to_string(), json!("stat")),
                ]),
            )
            .await
            .unwrap(),
        );
        assert!(stat.contains("# Agent Diff for T-1"));
        assert!(stat.contains("**Branch:** agent-branch"));
        assert!(stat.contains("**Base:** branch main"));
        assert!(stat.contains("file.txt"));

        let unified = tool_output_text(
            tool.tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!("unified")),
                ]),
            )
            .await
            .unwrap(),
        );
        assert!(unified.contains("+agent"));

        let name_only = tool_output_text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!("name-only")),
                ]),
            )
            .await
            .unwrap(),
        );
        assert!(name_only.contains("file.txt"));
    }

    #[tokio::test]
    async fn tool_agent_diff_name_only_includes_all_worktree_states() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        commit_file(&repo, "unstaged.txt", "base\n", "add tracked unstaged file");
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        commit_file(&repo, "committed.txt", "committed\n", "committed change");
        write_file(&repo, "staged.txt", "staged\n");
        run_git(&repo, &["add", "staged.txt"]);
        write_file(&repo, "unstaged.txt", "base\nunstaged\n");
        write_file(&repo, "untracked.txt", "untracked\n");
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task(&repo, card).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();

        let name_only = tool_output_text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!("name-only")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert_name_only_has(
            &name_only,
            &[
                "committed.txt",
                "staged.txt",
                "unstaged.txt",
                "untracked.txt",
            ],
        );
    }

    #[tokio::test]
    async fn tool_agent_diff_unified_sections_include_dirty_state() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        commit_file(&repo, "committed.txt", "committed\n", "committed change");
        write_file(&repo, "staged.txt", "staged\n");
        run_git(&repo, &["add", "staged.txt"]);
        std::fs::write(repo.join("file.txt"), "hello\nunstaged\n").unwrap();
        write_file(&repo, "untracked.txt", "untracked\n");
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task(&repo, card).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();

        let unified = tool_output_text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!("unified")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(unified.contains("## Committed changes since base"));
        assert!(unified.contains("## Staged changes"));
        assert!(unified.contains("## Unstaged changes"));
        assert!(unified.contains("## Untracked files"));
        assert!(unified.contains("+committed"));
        assert!(unified.contains("+staged"));
        assert!(unified.contains("+unstaged"));
        assert!(unified.contains("untracked.txt"));
    }

    #[tokio::test]
    async fn agent_diff_unified_includes_untracked_file_content() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        write_file(&repo, "new.txt", "alpha\nbeta\n");
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task(&repo, card).await;

        let unified = execute_diff(gcx, "unified").await.unwrap();

        assert!(unified.contains("--- /dev/null"));
        assert!(unified.contains("+++ b/new.txt"));
        assert!(unified.contains("+alpha"));
        assert!(unified.contains("+beta"));
    }

    #[tokio::test]
    async fn agent_diff_unified_truncates_large_untracked_files() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        write_file(&repo, "big.txt", &"a".repeat(200 * 1024));
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task(&repo, card).await;

        let unified = execute_diff_with_max_lines(gcx, "unified", 20)
            .await
            .unwrap();

        assert!(unified.contains("+++ b/big.txt"));
        assert!(unified.contains("... (truncated, 139264 more bytes)"));
        assert!(count_lines_with_prefix(&unified, "+a") <= 1);
    }

    #[tokio::test]
    async fn agent_diff_unified_skips_binary_untracked_files() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        std::fs::write(repo.join("binary.bin"), b"abc\0def").unwrap();
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task(&repo, card).await;

        let unified = execute_diff(gcx, "unified").await.unwrap();

        assert!(unified.contains("binary.bin (binary, 7 bytes)"));
        assert!(!unified.contains("+++ b/binary.bin"));
        assert!(!unified.contains("+abc"));
    }

    #[tokio::test]
    async fn agent_diff_unified_respects_total_size_budget() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        for index in 0..6 {
            write_file(&repo, &format!("file-{index}.txt"), &"x".repeat(64 * 1024));
        }
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task(&repo, card).await;

        let unified = execute_diff(gcx, "unified").await.unwrap();

        assert_eq!(
            unified
                .matches("more untracked files omitted due to size cap")
                .count(),
            1
        );
        assert!(unified.contains("(2 more untracked files omitted due to size cap)"));
        assert!(unified.contains("file-4.txt"));
        assert!(unified.contains("file-5.txt"));
    }

    #[tokio::test]
    async fn tool_agent_diff_prefers_original_base_commit_after_base_branch_advances() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let base_commit = run_git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        commit_file(&repo, "agent.txt", "agent\n", "agent change");
        run_git(&repo, &["checkout", "main"]);
        commit_file(&repo, "main.txt", "main advanced\n", "advance main");
        run_git(&repo, &["checkout", "agent-branch"]);
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task_with_meta(
            &repo,
            card,
            task_meta_with_base(Some("main"), Some(&base_commit)),
        )
        .await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();

        let name_only = tool_output_text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!("name-only")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(name_only.contains(&format!("**Base:** commit {}", base_commit)));
        assert!(name_only.contains("agent.txt"));
        assert!(!name_only.contains("main.txt"));
    }

    #[tokio::test]
    async fn tool_agent_diff_reports_no_changes() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
        );
        let gcx = write_task(&repo, card).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();

        let output = tool_output_text(
            tool.tool_execute(
                ccx,
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!("stat")),
                ]),
            )
            .await
            .unwrap(),
        );

        assert!(output.contains("(no changes detected)"));
    }

    #[tokio::test]
    async fn agent_diff_rejects_path_outside_worktree_cache() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let outside = temp.path().join("random-repo");
        std::fs::create_dir_all(&outside).unwrap();
        init_repo(&outside);
        run_git(&outside, &["checkout", "-b", "agent-branch"]);
        write_file(&outside, "evil.txt", "nope\n");
        let cache_dir = temp.path().join("cache");
        let config_dir = temp.path().join("config");
        let gcx =
            crate::global_context::tests::make_test_gcx_with_dirs(cache_dir, config_dir).await;
        let card = test_card(
            Some("agent-branch".to_string()),
            Some(outside.to_string_lossy().to_string()),
        );
        let gcx = write_task_with_gcx(gcx, &source, card, task_meta()).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();

        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!("unified")),
                ]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("outside worktree cache"), "{err}");
    }

    #[tokio::test]
    async fn agent_diff_uses_registry_when_worktree_name_set() {
        let temp = tempfile::tempdir().unwrap();
        let (gcx, source, registered) = create_registered_worktree(&temp, "task-1", "T-1").await;
        write_file(&registered.meta.root, "trusted.txt", "trusted\n");
        let stale = temp.path().join("stale");
        std::fs::create_dir_all(&stale).unwrap();
        init_repo(&stale);
        run_git(&stale, &["checkout", "-b", "agent-branch"]);
        write_file(&stale, "stale.txt", "stale\n");
        let card = test_card_with_worktree_name(
            registered.meta.branch.clone(),
            Some(stale.to_string_lossy().to_string()),
            Some(registered.meta.id.clone()),
        );
        let gcx = write_task_with_gcx(gcx, &source, card, task_meta()).await;

        let unified = execute_diff(gcx, "unified").await.unwrap();

        assert!(unified.contains("trusted.txt"));
        assert!(unified.contains("+trusted"));
        assert!(!unified.contains("stale.txt"));
    }

    #[tokio::test]
    async fn agent_diff_falls_back_to_valid_legacy_path_when_registry_id_is_stale() {
        let temp = tempfile::tempdir().unwrap();
        let repo = legacy_repo_path(temp.path());
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        run_git(&repo, &["checkout", "-b", "agent-branch"]);
        commit_file(&repo, "legacy.txt", "legacy\n", "legacy change");
        let card = test_card_with_worktree_name(
            Some("agent-branch".to_string()),
            Some(repo.to_string_lossy().to_string()),
            Some("missing-registry-id".to_string()),
        );
        let gcx = write_task(&repo, card).await;

        let name_only = execute_diff(gcx, "name-only").await.unwrap();

        assert!(name_only.contains("legacy.txt"), "{name_only}");
    }

    #[tokio::test]
    async fn agent_diff_resolves_registered_worktree_by_reference_when_meta_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let (gcx, source, registered) = create_registered_worktree(&temp, "task-1", "T-1").await;
        let service =
            WorktreeService::new(gcx.cache_dir.clone(), source.canonicalize().unwrap()).unwrap();
        let mut registry = service.load_registry().await.unwrap();
        registry.records[0].meta.task_id = None;
        registry.records[0].meta.card_id = None;
        registry.records[0].references = vec![refact_worktrees::types::WorktreeReference {
            kind: "task_agent".to_string(),
            task_id: Some("task-1".to_string()),
            card_id: Some("T-1".to_string()),
            chat_id: Some("agent-chat-1".to_string()),
            agent_id: Some("agent-1".to_string()),
        }];
        service.save_registry(&registry).await.unwrap();
        write_file(&registered.meta.root, "referenced.txt", "referenced\n");
        let card = test_card_with_worktree_name(registered.meta.branch.clone(), None, None);
        let gcx = write_task_with_gcx(gcx, &source, card, task_meta()).await;

        let unified = execute_diff(gcx, "unified").await.unwrap();

        assert!(unified.contains("referenced.txt"), "{unified}");
        assert!(unified.contains("+referenced"), "{unified}");
    }

    #[tokio::test]
    async fn agent_diff_registry_metadata_mismatch_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let (gcx, source, registered) =
            create_registered_worktree(&temp, "other-task", "T-1").await;
        let card = test_card_with_worktree_name(
            registered.meta.branch.clone(),
            Some(registered.meta.root.to_string_lossy().to_string()),
            Some(registered.meta.id.clone()),
        );
        let gcx = write_task_with_gcx(gcx, &source, card, task_meta()).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let mut tool = ToolAgentDiff::new();

        let err = tool
            .tool_execute(
                ccx,
                &"call".to_string(),
                &HashMap::from([
                    ("card_id".to_string(), json!("T-1")),
                    ("mode".to_string(), json!("unified")),
                ]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("does not match task task-1 card T-1"), "{err}");
    }

    #[test]
    fn tool_agent_diff_truncates_output() {
        let card = test_card(Some("agent".to_string()), Some("/tmp/wt".to_string()));
        let output = "a\nb\nc\nd\n";

        let rendered = render_agent_diff(
            &card,
            "agent",
            &DiffBase {
                refish: "main".to_string(),
                label: "branch main".to_string(),
            },
            AgentDiffMode::Unified,
            output,
            2,
        );

        assert!(
            rendered.contains("a\nb\n... (2 more lines, use mode='name-only' to see all files)")
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn agent_diff_output_byte_cap_honours_installed_setting_and_is_loud() {
        let _guard = SettingsGuard::install(|settings| {
            settings.agent_diff_max_output_bytes = 8_192;
        });
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        init_repo(repo);
        std::fs::write(repo.join("big.txt"), "a".repeat(200_000)).unwrap();
        run_git(repo, &["add", "big.txt"]);
        run_git(repo, &["commit", "-m", "add big"]);
        std::fs::write(repo.join("big.txt"), "b".repeat(200_000)).unwrap();

        let output = super::run_git(repo, &["diff", "HEAD", "--text"], GIT_DIFF_TIMEOUT)
            .await
            .unwrap();

        assert!(
            output.len() <= 8_192 + 400,
            "output {} bytes exceeds the installed cap plus notice overhead",
            output.len()
        );
        assert!(
            output.contains("agent_diff_max_output_bytes = 8192")
                && output.contains("git output bytes"),
            "expected a quantified byte-cap notice naming the setting, got tail: {}",
            &output[output.len().saturating_sub(400)..]
        );
    }

    #[test]
    fn git_output_cap_notice_quantifies_shown_and_total() {
        let notice = git_output_cap_notice(1_048_576, 3_000_000, 1_048_576);
        assert!(
            notice.contains("showing 1048576 of 3000000+ git output bytes"),
            "{}",
            notice
        );
        assert!(
            notice.contains("agent_diff_max_output_bytes = 1048576"),
            "{}",
            notice
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn run_git_truncates_output_at_byte_cap() {
        let _guard = SettingsGuard::install(|settings| {
            settings.agent_diff_max_output_bytes = 200_000;
        });
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        init_repo(repo);
        let content_a = "a".repeat(600_000);
        std::fs::write(repo.join("big.txt"), &content_a).unwrap();
        run_git(repo, &["add", "big.txt"]);
        run_git(repo, &["commit", "-m", "add big"]);
        let content_b = "b".repeat(600_000);
        std::fs::write(repo.join("big.txt"), &content_b).unwrap();

        let output = super::run_git(repo, &["diff", "HEAD", "--text"], GIT_DIFF_TIMEOUT)
            .await
            .unwrap();

        let limit = refact_core::trajectory_settings::current().agent_diff_max_output_bytes;
        assert!(
            output.contains("agent_diff_max_output_bytes ="),
            "expected a quantified truncation footer, got tail: {}",
            &output[output.len().saturating_sub(400)..]
        );
        assert!(
            output.len() <= limit + 400,
            "output {} bytes exceeds cap + overhead",
            output.len()
        );
    }

    #[tokio::test]
    async fn run_git_respects_timeout() {
        let result =
            super::run_with_timeout(std::future::pending::<()>(), Duration::from_nanos(1)).await;

        assert!(result.is_err(), "expected timeout");
    }

    #[tokio::test]
    async fn agent_diff_is_async_compatible() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        init_repo(repo);

        let result = super::run_git(
            repo,
            &["rev-parse", "--is-inside-work-tree"],
            GIT_DIFF_TIMEOUT,
        )
        .await
        .unwrap();

        assert_eq!(result.trim(), "true");
    }
}
