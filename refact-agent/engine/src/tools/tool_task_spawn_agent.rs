pub use refact_tools::tools::tool_task_spawn_agent::*;


#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use refact_runtime_api::CapsProvider;
#[cfg(test)]
use refact_runtime_api::ChatCtx;
#[cfg(test)]
use refact_tasks::rooms;
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use refact_tasks::types::TaskMeta as StoredTaskMeta;
#[cfg(test)]
use refact_worktrees::git;
#[cfg(test)]
use refact_worktrees::service::WorktreeService;
#[cfg(test)]
use refact_worktrees::types::WorktreeMeta;


#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::privacy_types::{FilePrivacySettings, PrivacySettings};
    use refact_runtime_api::task_storage as storage;
    use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus};
    use std::path::Path;
    use std::process::Command;

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
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
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

    async fn set_workspace(gcx: Arc<GlobalContext>, root: &Path) {
        let root = root.canonicalize().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root];
    }

    async fn set_caps(gcx: &Arc<GlobalContext>, caps: crate::caps::CodeAssistantCaps) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut state = gcx.caps_state.write().await;
        state.caps = Some(Arc::new(caps));
        state.last_attempted_ts = now;
    }

    async fn test_chat_ctx(gcx: &Arc<GlobalContext>) -> ChatCtx {
        refact_runtime_api::app_state_from_gcx(gcx.clone())
            .await
            .chat_ctx()
    }

    async fn test_caps_provider(gcx: &Arc<GlobalContext>) -> Arc<dyn CapsProvider> {
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await.tool.caps
    }

    async fn test_gcx_with_privacy(blocked: Vec<String>) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: vec![],
                blocked,
            },
            loaded_ts: u64::MAX / 2,
        });
        gcx
    }

    fn sample_task_meta(base_branch: Option<&str>) -> StoredTaskMeta {
        let now = Utc::now().to_rfc3339();
        StoredTaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 0,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: base_branch.map(|branch| branch.to_string()),
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        }
    }

    fn sample_worktree_meta(temp: &Path) -> WorktreeMeta {
        let root = temp.join("agent-root");
        let source = temp.join("source-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        WorktreeMeta {
            id: "wt-task-agent".to_string(),
            kind: "task_agent".to_string(),
            root,
            source_workspace_root: source.clone(),
            repo_root: source,
            branch: Some("refact/task/task-1/card/T-1/agent".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: Some("base".to_string()),
            task_id: Some("task-1".to_string()),
            card_id: Some("T-1".to_string()),
            agent_id: Some("agent-1".to_string()),
            enforce: true,
        }
    }

    #[test]
    fn build_agent_thread_params_starts_with_fresh_compaction_and_cache_state() {
        let temp = tempfile::tempdir().unwrap();
        let params = build_agent_thread_params(
            "agent-chat-1",
            "Card Title",
            "provider/model",
            "task-1",
            "agent-1",
            "T-1",
            "planner-chat-1",
            sample_worktree_meta(temp.path()),
        );

        // Task-agent children must never inherit the planner's compaction or
        // provider-cache state; a stale cap/response id would corrupt the
        // child's first generation.
        assert_eq!(params.context_tokens_cap, None);
        assert_eq!(params.previous_response_id, None);
        assert_eq!(params.auto_compact_enabled, None);
        assert_eq!(params.reactive_compact_attempts, None);
        assert_eq!(params.frozen_request_prefix, None);
        assert_eq!(params.mode, "task_agent");
        assert_eq!(params.parent_id.as_deref(), Some("planner-chat-1"));
    }

    fn test_card(id: &str, column: &str, worktree: Option<String>) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: format!("Card {}", id),
            column: column.to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: worktree,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    #[test]
    fn validate_id_rejects_overlong_ids() {
        assert!(validate_id("short-id", "test_id").is_ok());
        assert!(validate_id(&"a".repeat(MAX_ID_LEN), "test_id").is_ok());
        let err = validate_id(&"a".repeat(MAX_ID_LEN + 1), "test_id").unwrap_err();
        assert!(
            err.contains("too long"),
            "expected 'too long' in error: {err}"
        );
    }

    #[test]
    fn validate_id_rejects_empty_string() {
        let err = validate_id("", "task_id").unwrap_err();
        assert!(err.contains("must not be empty"), "{err}");
    }

    #[tokio::test]
    async fn resolve_agent_model_uses_task_planner_agent_model_over_current() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut caps = crate::caps::CodeAssistantCaps::default();
        caps.defaults.task_planner_agent_model = "provider/task-planner".to_string();
        caps.defaults.chat_default_model = "provider/chat".to_string();
        set_caps(&gcx, caps).await;

        let model = resolve_agent_model(&test_caps_provider(&gcx).await, "provider/current").await.unwrap();

        assert_eq!(model, "provider/task-planner");
    }

    #[tokio::test]
    async fn resolve_agent_model_falls_back_without_task_planner_agent_model() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut caps = crate::caps::CodeAssistantCaps::default();
        caps.defaults.chat_default_model = "provider/chat".to_string();
        set_caps(&gcx, caps).await;

        let model = resolve_agent_model(&test_caps_provider(&gcx).await, "provider/current").await.unwrap();

        assert_eq!(model, "provider/current");
    }

    #[tokio::test]
    async fn resolve_agent_model_uses_global_default_when_current_is_empty() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut caps = crate::caps::CodeAssistantCaps::default();
        caps.defaults.chat_default_model = "provider/chat".to_string();
        set_caps(&gcx, caps).await;

        let model = resolve_agent_model(&test_caps_provider(&gcx).await, "").await.unwrap();

        assert_eq!(model, "provider/chat");
    }

    #[tokio::test]
    async fn spawn_agent_rejects_overlong_branch_combination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        // 100-char IDs: "refact/task/" (12) + 100 + "/card/" (6) + 100 + "/" (1) + 8 = 227 > 200
        let task_id = "a".repeat(100);
        let card_id = "b".repeat(100);
        let task_meta = sample_task_meta(None);
        let err = prepare_agent_worktree_with_suffix(&test_chat_ctx(&gcx).await,
            &task_meta,
            &task_id,
            "agent-12345678",
            &card_id,
            "agent-chat-id",
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("exceeds 200 chars"),
            "expected branch-too-long error: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_agent_non_git_workspace_fails_clearly() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;

        let task_meta = sample_task_meta(None);
        let err = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            "task-1",
            "agent-12345678",
            "T-1",
            "agent-T-1-12345678",
        )
        .await
        .unwrap_err();

        assert!(err.contains("not a git repository"), "{err}");
    }

    #[tokio::test]
    async fn spawn_agent_prepare_sets_worktree_meta_without_global_workspace_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let before = refact_runtime_api::files_correction::get_project_dirs(gcx.clone()).await;

        let task_meta = sample_task_meta(None);
        let prepared = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            "task-1",
            "agent-12345678",
            "T-1",
            "agent-T-1-12345678",
        )
        .await
        .unwrap();
        let after = refact_runtime_api::files_correction::get_project_dirs(gcx.clone()).await;

        assert_eq!(after, before);
        assert!(!after.contains(&prepared.meta.root));
        assert_eq!(prepared.meta.kind, "task_agent");
        assert!(prepared.meta.enforce);
        assert_eq!(prepared.meta.task_id.as_deref(), Some("task-1"));
        assert_eq!(prepared.meta.card_id.as_deref(), Some("T-1"));
        assert_eq!(prepared.meta.agent_id.as_deref(), Some("agent-12345678"));
        assert!(prepared.meta.root.is_dir());

        prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
    }

    #[tokio::test]
    async fn spawn_agent_rollback_cleanup_removes_worktree_branch_and_registry() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let task_meta = sample_task_meta(None);
        let prepared = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            "task-1",
            "agent-abcdef12",
            "T-1",
            "agent-T-1-abcdef12",
        )
        .await
        .unwrap();
        let id = prepared.meta.id.clone();
        let branch = prepared.meta.branch.clone().unwrap();
        let root = prepared.meta.root.clone();
        let cache_dir = gcx.cache_dir.clone();
        let service = WorktreeService::new(cache_dir, source.canonicalize().unwrap()).unwrap();
        assert!(service.get_worktree(&id).await.is_ok());

        prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;

        assert!(!root.exists());
        assert!(service.get_worktree(&id).await.is_err());
        let branches = run_git(&source, &["branch", "--list", &branch]);
        assert!(branches.trim().is_empty());
    }

    #[tokio::test]
    async fn spawn_agent_uses_stored_base_branch_not_current_head() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let main_head = commit_file(&source, "main_only.txt", "only main\n", "main-only");
        run_git(&source, &["checkout", "-b", "dev"]);
        let dev_head = commit_file(&source, "dev_only.txt", "only dev\n", "dev-only");
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let task_meta = sample_task_meta(Some("main"));

        let prepared = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            "task-1",
            "agent-11111111",
            "T-1",
            "agent-T-1-11111111",
        )
        .await
        .unwrap();

        assert_eq!(prepared.meta.base_branch.as_deref(), Some("main"));
        assert_eq!(
            prepared.meta.base_commit.as_deref(),
            Some(main_head.as_str())
        );
        assert!(prepared.meta.root.join("main_only.txt").is_file());
        assert!(!prepared.meta.root.join("dev_only.txt").exists());
        assert_eq!(run_git(&source, &["rev-parse", "dev"]).trim(), dev_head);

        prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
    }

    #[tokio::test]
    async fn spawn_agent_none_base_branch_falls_back_to_current_head() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        run_git(&source, &["checkout", "-b", "dev"]);
        let dev_head = commit_file(&source, "dev_only.txt", "only dev\n", "dev-only");
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let task_meta = sample_task_meta(None);

        let prepared = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            "task-1",
            "agent-22222222",
            "T-1",
            "agent-T-1-22222222",
        )
        .await
        .unwrap();

        assert_eq!(prepared.meta.base_branch.as_deref(), Some("dev"));
        assert_eq!(
            prepared.meta.base_commit.as_deref(),
            Some(dev_head.as_str())
        );
        assert!(prepared.meta.root.join("dev_only.txt").is_file());
        assert!(prepared.base_branch_mismatch_warning.is_none());

        prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
    }

    #[tokio::test]
    async fn spawn_agent_deleted_base_branch_returns_clear_error() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        run_git(&source, &["branch", "task-base"]);
        run_git(&source, &["branch", "-D", "task-base"]);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let task_meta = sample_task_meta(Some("task-base"));

        let err = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            "task-1",
            "agent-33333333",
            "T-1",
            "agent-T-1-33333333",
        )
        .await
        .unwrap_err();

        assert_eq!(
            err,
            "Task base branch 'task-base' no longer exists. Update the task base branch or create a new task on the current branch."
        );
    }

    #[tokio::test]
    async fn spawn_agent_warns_when_current_head_differs_from_task_base_branch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        run_git(&source, &["checkout", "-b", "dev"]);
        commit_file(&source, "dev_only.txt", "only dev\n", "dev-only");
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let task_meta = sample_task_meta(Some("main"));

        let prepared = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            "task-1",
            "agent-44444444",
            "T-1",
            "agent-T-1-44444444",
        )
        .await
        .unwrap();

        let warning = prepared.base_branch_mismatch_warning.as_deref().unwrap();
        assert!(warning.contains("Current repo HEAD is on branch 'dev'"));
        assert!(warning.contains("this task was created from 'main'"));

        prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;
    }

    #[test]
    fn spawn_agent_successful_spawn_sets_board_mirrors_and_thread_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = sample_worktree_meta(temp.path());
        let mut card = test_card("T-1", "planned", None);
        mark_card_agent_started(
            &mut card,
            "agent-1",
            "agent-chat-1",
            worktree.branch.clone(),
            Some(worktree.root.to_string_lossy().to_string()),
            Some(worktree.id.clone()),
        );

        assert_eq!(card.column, "doing");
        assert_eq!(card.assignee.as_deref(), Some("agent-1"));
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-chat-1"));
        assert_eq!(card.agent_branch, worktree.branch.clone());
        assert_eq!(
            card.agent_worktree.as_deref(),
            Some(worktree.root.to_str().unwrap())
        );
        assert_eq!(card.agent_worktree_name.as_deref(), Some("wt-task-agent"));

        let thread = build_agent_thread_params(
            "agent-chat-1",
            "Card T-1",
            "model-a",
            "task-1",
            "agent-1",
            "T-1",
            "planner-task-1-1",
            worktree.clone(),
        );
        assert_eq!(thread.mode, "task_agent");
        assert_eq!(thread.task_meta.as_ref().unwrap().role, "agents");
        assert_eq!(
            thread
                .task_meta
                .as_ref()
                .unwrap()
                .planner_chat_id
                .as_deref(),
            Some("planner-task-1-1")
        );
        assert_eq!(thread.parent_id.as_deref(), Some("planner-task-1-1"));
        assert_eq!(thread.root_chat_id.as_deref(), Some("planner-task-1-1"));
        assert_eq!(thread.worktree.as_ref(), Some(&worktree));
        assert!(thread.worktree.as_ref().unwrap().enforce);
    }

    #[tokio::test]
    async fn spawn_agent_planner_chat_id_uses_active_root_before_legacy_meta() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut session = refact_chat_api::ChatSession::new("controller-planner".to_string());
        session.thread.task_meta = Some(refact_chat_api::TaskMeta {
            task_id: "task-1".to_string(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: Some("controller-planner".to_string()),
        });
        gcx.chat_sessions.write().await.insert(
            "controller-planner".to_string(),
            Arc::new(tokio::sync::Mutex::new(session)),
        );
        let task_meta = refact_chat_api::TaskMeta {
            task_id: "task-1".to_string(),
            role: "subchats".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: Some("stale-planner".to_string()),
        };

        assert_eq!(
            resolve_invoking_planner_chat_id(
                gcx.clone(),
                "child-chat",
                "controller-planner",
                Some(&task_meta)
            )
            .await,
            "controller-planner"
        );
        assert_eq!(
            resolve_invoking_planner_chat_id(gcx, "child-chat", "", Some(&task_meta)).await,
            "stale-planner"
        );
    }

    #[tokio::test]
    async fn spawn_agent_planner_chat_id_ignores_non_planner_root() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_meta = refact_chat_api::TaskMeta {
            task_id: "task-1".to_string(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: Some("planner-chat".to_string()),
        };

        assert_eq!(
            resolve_invoking_planner_chat_id(
                gcx,
                "planner-chat",
                "normal-source-chat",
                Some(&task_meta)
            )
            .await,
            "planner-chat"
        );
    }

    #[test]
    fn spawn_agent_rollback_restore_original_card_after_session_failure() {
        let mut board = TaskBoard::default();
        let original = test_card("T-1", "planned", None);
        let mut spawned = original.clone();
        mark_card_agent_started(
            &mut spawned,
            "agent-1",
            "agent-chat-1",
            Some("branch-new".to_string()),
            Some("/tmp/new-worktree".to_string()),
            Some("wt-new".to_string()),
        );
        board.cards.push(spawned);

        assert!(restore_original_card_if_current_agent(
            &mut board,
            &original,
            "agent-chat-1"
        ));

        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "planned");
        assert!(card.assignee.is_none());
        assert!(card.agent_chat_id.is_none());
        assert!(card.agent_worktree.is_none());
        assert!(card.agent_branch.is_none());
    }

    #[test]
    fn spawn_agent_rollback_guard_skips_different_agent() {
        let mut board = TaskBoard::default();
        let original = test_card("T-1", "planned", None);
        let mut spawned = original.clone();
        mark_card_agent_started(&mut spawned, "agent-2", "agent-chat-2", None, None, None);
        board.cards.push(spawned.clone());

        assert!(!restore_original_card_if_current_agent(
            &mut board,
            &original,
            "agent-chat-1"
        ));

        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.agent_chat_id, spawned.agent_chat_id);
        assert_eq!(card.assignee, spawned.assignee);
        assert_eq!(card.column, "doing");
    }

    #[test]
    fn abandoned_worktree_detection_ignores_active_agents() {
        let tempdir = tempfile::tempdir().unwrap();
        let worktree_path = tempdir.path().join("agent-worktree");
        std::fs::create_dir_all(&worktree_path).unwrap();

        let mut board = TaskBoard::default();
        board.cards.push(test_card(
            "T-1",
            "done",
            Some(worktree_path.to_string_lossy().to_string()),
        ));
        board.cards.push(test_card(
            "T-2",
            "doing",
            Some(worktree_path.to_string_lossy().to_string()),
        ));
        board.cards.push(test_card(
            "T-3",
            "failed",
            Some(tempdir.path().join("missing").to_string_lossy().to_string()),
        ));

        let abandoned = find_abandoned_worktrees(&board);
        assert_eq!(abandoned.len(), 1);
        assert!(abandoned[0].contains("T-1"));
        assert!(!abandoned[0].contains("T-2"));
        assert!(!abandoned[0].contains("T-3"));
    }

    #[test]
    fn worktree_scope_files_to_open_path_traversal_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::fs::write(worktree.join("safe.txt"), "safe").unwrap();

        let worktree_canonical = dunce::canonicalize(&worktree).unwrap();

        let traversal = worktree.join("../outside/secret.txt");
        let traversal_canonical = dunce::canonicalize(&traversal).unwrap();
        assert!(
            !traversal_canonical.starts_with(&worktree_canonical),
            "traversal path should escape worktree: {:?} not under {:?}",
            traversal_canonical,
            worktree_canonical
        );

        let safe_canonical = dunce::canonicalize(worktree.join("safe.txt")).unwrap();
        assert!(
            safe_canonical.starts_with(&worktree_canonical),
            "in-worktree path should pass check: {:?} under {:?}",
            safe_canonical,
            worktree_canonical
        );
    }

    #[tokio::test]
    async fn spawn_agent_files_to_open_rejects_outside_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let source = temp.path().join("source");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        let gcx = test_gcx_with_privacy(vec![]).await;

        let err = context_files_from_files_to_open(
            gcx,
            &worktree,
            &source,
            &["../outside/secret.txt".to_string()],
        )
        .await
        .unwrap_err();

        assert!(err.contains("outside agent worktree"), "{err}");
    }

    #[tokio::test]
    async fn spawn_agent_files_to_open_rejects_privacy_blocked_file() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let source = temp.path().join("source");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(worktree.join(".env"), "SECRET=value\n").unwrap();
        let gcx = test_gcx_with_privacy(vec!["*.env".to_string()]).await;

        let err = context_files_from_files_to_open(gcx, &worktree, &source, &[".env".to_string()])
            .await
            .unwrap_err();

        assert!(err.contains("blocked by privacy settings"), "{err}");
        assert!(err.contains("Blocked"), "{err}");
    }

    #[tokio::test]
    async fn spawn_agent_files_to_open_valid_file_works() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let source = temp.path().join("source");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(worktree.join("safe.txt"), "safe\ncontent\n").unwrap();
        let gcx = test_gcx_with_privacy(vec![]).await;

        let files =
            context_files_from_files_to_open(gcx, &worktree, &source, &["safe.txt".to_string()])
                .await
                .unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_content, "safe\ncontent\n");
        assert_eq!(files[0].line1, 1);
        assert_eq!(files[0].line2, 2);
        assert!(files[0].file_name.ends_with("safe.txt"));
    }

    #[tokio::test]
    async fn spawn_agent_context_file_message_has_declared_record() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let source = temp.path().join("source");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(worktree.join("safe.txt"), "safe\n").unwrap();
        let gcx = test_gcx_with_privacy(vec![]).await;
        let files = context_files_from_files_to_open(
            gcx.clone(),
            &worktree,
            &source,
            &["safe.txt".to_string()],
        )
        .await
        .unwrap();

        let message = context_file_message(&gcx, files).unwrap();
        let privacy: refact_privacy::PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();

        assert_eq!(privacy.files.len(), 1);
        assert!(privacy.files[0].path.ends_with("safe.txt"));
        assert_eq!(
            privacy.files[0].attribution,
            refact_privacy::Attribution::Declared
        );
    }

    #[tokio::test]
    async fn files_to_open_truncates_oversized_file() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let source = temp.path().join("source");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(worktree.join("large.txt"), vec![b'a'; 1024 * 1024]).unwrap();
        let gcx = test_gcx_with_privacy(vec![]).await;

        let files =
            context_files_from_files_to_open(gcx, &worktree, &source, &["large.txt".to_string()])
                .await
                .unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0]
            .file_content
            .starts_with(&"a".repeat(FILES_TO_OPEN_PER_FILE_LIMIT)));
        assert!(files[0]
            .file_content
            .contains("... (truncated, 786432 more bytes)"));
        assert!(files[0].file_name.ends_with("large.txt"));
    }

    #[tokio::test]
    async fn files_to_open_drops_remaining_after_total_cap() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        let source = temp.path().join("source");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        let paths = (0..5)
            .map(|idx| {
                let name = format!("file-{idx}.txt");
                std::fs::write(worktree.join(&name), vec![b'a' + idx as u8; 256 * 1024]).unwrap();
                name
            })
            .collect::<Vec<_>>();
        let gcx = test_gcx_with_privacy(vec![]).await;

        let files = context_files_from_files_to_open(gcx, &worktree, &source, &paths)
            .await
            .unwrap();

        assert_eq!(files.len(), 5);
        for file in files.iter().take(4) {
            assert_eq!(file.file_content.len(), 256 * 1024);
            assert_eq!(file.line1, 1);
            assert_eq!(file.line2, 1);
        }
        assert!(files[4].file_name.ends_with("file-4.txt"));
        assert!(files[4].file_content.is_empty());
        assert_eq!(files[4].line1, 0);
        assert_eq!(files[4].line2, 0);
    }

    #[test]
    fn abandoned_worktree_detection_ignores_failed_cards_with_retained_worktrees() {
        let tempdir = tempfile::tempdir().unwrap();
        let worktree_path = tempdir.path().join("retained-failed-worktree");
        std::fs::create_dir_all(&worktree_path).unwrap();

        let mut board = TaskBoard::default();
        board.cards.push(test_card(
            "T-1",
            "failed",
            Some(worktree_path.to_string_lossy().to_string()),
        ));
        board.cards.push(test_card(
            "T-2",
            "done",
            Some(worktree_path.to_string_lossy().to_string()),
        ));

        let abandoned = find_abandoned_worktrees(&board);
        assert_eq!(abandoned.len(), 1, "only done card should be flagged");
        assert!(
            !abandoned[0].contains("T-1"),
            "failed card retained worktree should not block spawning"
        );
        assert!(
            abandoned[0].contains("T-2"),
            "done card with retained worktree should still be flagged"
        );
    }

    #[test]
    fn abandoned_worktrees_error_names_merge_and_merge_free_remedies() {
        let abandoned = vec!["- T-2 (Card) in column `done`: `/tmp/agent-worktree`".to_string()];

        let message = abandoned_worktrees_error("spawn a new task agent", &abandoned);

        assert!(
            message.contains("Cannot spawn a new task agent while abandoned task worktrees exist"),
            "{message}"
        );
        assert!(
            message.contains("merge_agent(card_id=...)"),
            "message must name the merge path: {message}"
        );
        assert!(
            message.contains("git worktree remove <path> --force"),
            "message must name a discard path that the verifier cannot block: {message}"
        );
        assert!(
            message.contains("without merging"),
            "message must make the merge-free path explicit: {message}"
        );
        assert!(
            message.contains("/tmp/agent-worktree"),
            "offending worktrees must still be listed: {message}"
        );
    }

    #[test]
    fn abandoned_worktrees_error_is_shared_between_spawn_agent_and_spawn_ab() {
        let abandoned = vec!["- T-2 (Card) in column `done`: `/tmp/agent-worktree`".to_string()];

        let spawn_agent_message = abandoned_worktrees_error("spawn a new task agent", &abandoned);
        let spawn_ab_message = abandoned_worktrees_error("spawn A/B agents", &abandoned);

        for message in [&spawn_agent_message, &spawn_ab_message] {
            assert!(message.contains("merge_agent(card_id=...)"), "{message}");
            assert!(
                message.contains("git worktree remove <path> --force"),
                "{message}"
            );
            assert!(message.contains("/tmp/agent-worktree"), "{message}");
        }
        assert!(
            spawn_ab_message.contains("Cannot spawn A/B agents"),
            "{spawn_ab_message}"
        );
    }

    async fn setup_task_storage(
        gcx: Arc<GlobalContext>,
        source: &Path,
        task_id: &str,
        card: BoardCard,
    ) {
        let task_dir = source.join(".refact").join("tasks").join(task_id);
        tokio::fs::create_dir_all(task_dir.join("trajectories").join("planner"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(task_dir.join("trajectories").join("agents"))
            .await
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let meta = TaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Test Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: None,
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
            gcx.clone(),
            task_id,
            &TaskBoard {
                cards: vec![card],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn spawn_rollback_before_board_write_cleans_up_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;
        let task_meta = sample_task_meta(None);
        let prepared = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            "task-1",
            "agent-abcdef12",
            "T-1",
            "agent-T-1-abcdef12",
        )
        .await
        .unwrap();
        let root = prepared.meta.root.clone();
        assert!(root.is_dir());

        prepared.cleanup_unlinked(gcx.cache_dir.clone()).await;

        assert!(!root.exists());
    }

    #[tokio::test]
    async fn spawn_rollback_after_board_write_retains_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;

        let task_id = "task-retain-test";
        let task_meta = sample_task_meta(None);
        let prepared = prepare_agent_worktree(&test_chat_ctx(&gcx).await,
            &task_meta,
            task_id,
            "agent-abcdef12",
            "T-1",
            "agent-T-1-abcdef12",
        )
        .await
        .unwrap();
        let root = prepared.meta.root.clone();
        let branch = prepared.meta.branch.clone().unwrap();

        let mut card = test_card("T-1", "doing", Some(root.to_string_lossy().to_string()));
        card.agent_chat_id = Some("agent-T-1-abcdef12".to_string());
        setup_task_storage(gcx.clone(), &source, task_id, card).await;

        rollback_retain(&test_chat_ctx(&gcx).await,
            task_id,
            "T-1",
            "agent-T-1-abcdef12",
            "model unavailable",
        )
        .await;
        prepared.retain().await;

        assert!(
            root.exists(),
            "worktree dir should still exist after retain"
        );
        let branches = run_git(&source, &["branch", "--list", &branch]);
        assert!(
            !branches.trim().is_empty(),
            "branch should still exist after retain"
        );

        let board = storage::load_board(gcx.clone(), task_id).await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "failed");
        assert!(
            card.status_updates
                .iter()
                .any(|u| u.message.contains("retained for inspection")),
            "status update should mention retained for inspection"
        );
    }

    #[tokio::test]
    async fn spawn_rollback_message_includes_error() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_workspace(gcx.clone(), &source).await;

        let task_id = "task-msg-test";
        let mut card = test_card("T-2", "doing", None);
        card.agent_chat_id = Some("agent-T-2-msgtest".to_string());
        setup_task_storage(gcx.clone(), &source, task_id, card).await;

        let long_error: String = "x".repeat(400);
        rollback_retain(&test_chat_ctx(&gcx).await,
            task_id,
            "T-2",
            "agent-T-2-msgtest",
            &long_error,
        )
        .await;

        let board = storage::load_board(gcx.clone(), task_id).await.unwrap();
        let card = board.get_card("T-2").unwrap();
        let msg = &card.status_updates.last().unwrap().message;
        assert!(
            msg.contains(&"x".repeat(300)),
            "message should include first 300 chars of error"
        );
        assert!(
            !msg.contains(&"x".repeat(301)),
            "message should not include more than 300 chars of error"
        );
        assert!(
            msg.contains("Worktree retained for inspection"),
            "message should contain retention notice"
        );
        assert!(
            msg.contains("Worktree retained for inspection"),
            "message should contain retention notice"
        );
    }

    #[test]
    fn files_to_open_rejects_non_string_entry() {
        use serde_json::json;
        let args = HashMap::from([(
            "files_to_open".to_string(),
            json!([{"path": "/foo"}, "/bar"]),
        )]);
        let err = validate_files_to_open(&args).unwrap_err();
        assert!(err.contains("files_to_open[0]"), "{err}");
        assert!(err.contains("must be a string"), "{err}");
        assert!(err.contains("object"), "{err}");
    }

    #[test]
    fn files_to_open_rejects_empty_string() {
        use serde_json::json;
        let args = HashMap::from([(
            "files_to_open".to_string(),
            json!(["valid.txt", "", "other.txt"]),
        )]);
        let err = validate_files_to_open(&args).unwrap_err();
        assert!(err.contains("files_to_open[1]"), "{err}");
        assert!(err.contains("is empty"), "{err}");
    }

    #[test]
    fn files_to_open_dedupes_with_warning() {
        use serde_json::json;
        let args = HashMap::from([(
            "files_to_open".to_string(),
            json!(["/foo/bar.txt", "/baz.txt", "/foo/bar.txt"]),
        )]);
        let paths = validate_files_to_open(&args).unwrap();
        assert_eq!(paths, vec!["/foo/bar.txt", "/baz.txt"]);
    }

    fn reserved_slot(role: &str, writes: Option<bool>) -> refact_tasks::types::TeamMember {
        rooms::planned_room_member(rooms::RoomSlot {
            role: role.to_string(),
            mandate: None,
            writes: writes.unwrap_or(true),
            decision_maker: false,
            room_role_hint: None,
            tools: vec![],
            target_files: vec![],
        })
    }

    #[test]
    fn second_agent_claims_reserved_slot_and_becomes_primary_owner() {
        let mut card = test_card("T-1", "planned", None);
        // The roster is reserved up front: a deciding (read-only) architect and a writing coder.
        card.team_members =
            vec![reserved_slot("architect", Some(false)), reserved_slot("coder", Some(true))];

        // The architect's spawn claims its slot. A sequential role stays Pending, so the room is
        // still valid, and it does not take the card (it will decide, not implement).
        claim_room_slot(
            &mut card,
            "architect",
            "agent-1",
            "agent-T-1-arch",
            Some("branch-1".into()),
            Some("/wt/1".into()),
            None,
            None,
            vec!["cat".into()],
        )
        .unwrap();
        assert_eq!(card.team_members.len(), 2);
        assert_eq!(
            card.team_members[0].member_status,
            Some(refact_tasks::types::TeamStatus::Pending)
        );
        assert!(
            card.agent_chat_id.is_none(),
            "a deciding/reading member must not own the card"
        );

        // The coder's spawn claims the second slot. It is the first writer, so it becomes the
        // primary owner: its scalars are promoted, it goes Running, and the card starts.
        claim_room_slot(
            &mut card,
            "coder",
            "agent-2",
            "agent-T-1-code",
            Some("branch-2".into()),
            Some("/wt/2".into()),
            None,
            Some("write the parser".into()),
            vec!["cat".into(), "shell".into()],
        )
        .unwrap();

        // The reserved slot was *filled in place* — not a second member appended for "coder".
        assert_eq!(card.team_members.len(), 2);
        assert_eq!(card.team_members[1].role, "coder");
        assert_eq!(
            card.team_members[1].agent_chat_id.as_deref(),
            Some("agent-T-1-code")
        );
        assert_eq!(card.team_members[1].agent_branch.as_deref(), Some("branch-2"));
        assert_eq!(
            card.team_members[1].agent_worktree.as_deref(),
            Some("/wt/2")
        );
        assert_eq!(
            card.team_members[1].mandate.as_deref(),
            Some("write the parser")
        );
        assert_eq!(
            card.team_members[1].tools,
            vec!["cat".to_string(), "shell".to_string()],
            "the roster must be able to tell peers what each member may use"
        );
        assert_eq!(
            card.team_members[1].member_status,
            Some(refact_tasks::types::TeamStatus::Running)
        );

        // Primary owner promotion: the card's scalars now point at the coder, and the card moved
        // to doing with a start time, so every legacy reader (monitor, diff, merge) sees it.
        assert_eq!(card.assignee.as_deref(), Some("agent-2"));
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-T-1-code"));
        assert_eq!(card.agent_branch.as_deref(), Some("branch-2"));
        assert_eq!(card.column, "doing");
        assert!(card.started_at.is_some());
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message.contains("primary owner")));
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message.contains("joined the room as coder")));
    }

    #[test]
    fn spawning_a_role_the_room_never_reserved_is_rejected() {
        let mut card = test_card("T-1", "planned", None);
        card.team_members = vec![reserved_slot("coder", Some(true))];

        let error = claim_room_slot(
            &mut card,
            "reviewer",
            "agent-2",
            "agent-T-1-rev",
            Some("branch-2".into()),
            Some("/wt/2".into()),
            None,
            None,
            vec!["cat".into()],
        )
        .unwrap_err();

        assert!(error.contains("no reserved slot"), "{error}");
        // Refusing a foreign role must not append it to the roster.
        assert_eq!(card.team_members.len(), 1);
        assert!(card.team_members[0].agent_chat_id.is_none());
    }

    #[test]
    fn read_only_member_joins_without_becoming_owner() {
        let mut card = test_card("T-1", "planned", None);
        card.team_members = vec![
            reserved_slot("coder", Some(true)),
            reserved_slot("reviewer", Some(false)),
        ];

        // The writer claims first and owns the card.
        claim_room_slot(
            &mut card,
            "coder",
            "agent-1",
            "agent-T-1-code",
            Some("branch-1".into()),
            Some("/wt/1".into()),
            None,
            None,
            vec![],
        )
        .unwrap();
        assert_eq!(card.assignee.as_deref(), Some("agent-1"));
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-T-1-code"));

        // A read-only member claims its slot: it goes Running, but must not take the scalars.
        claim_room_slot(
            &mut card,
            "reviewer",
            "agent-2",
            "agent-T-1-rev",
            Some("branch-2".into()),
            Some("/wt/2".into()),
            None,
            None,
            vec![],
        )
        .unwrap();
        assert_eq!(
            card.team_members[1].member_status,
            Some(refact_tasks::types::TeamStatus::Running)
        );
        assert_eq!(
            card.assignee.as_deref(),
            Some("agent-1"),
            "a read-only peer must not steal ownership"
        );
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-T-1-code"));
    }

    #[test]
    fn single_card_keeps_scalar_assignment_behavior() {
        let mut card = test_card("T-1", "planned", None);

        claim_room_slot(
            &mut card,
            "coder",
            "agent-1",
            "agent-T-1-1111",
            Some("branch-1".into()),
            Some("/wt/1".into()),
            Some("wt-1".into()),
            None,
            vec!["cat".into()],
        )
        .unwrap();

        // Everything the historical scalar path promised still holds.
        assert_eq!(card.column, "doing");
        assert_eq!(card.assignee.as_deref(), Some("agent-1"));
        assert_eq!(card.agent_chat_id.as_deref(), Some("agent-T-1-1111"));
        assert_eq!(card.agent_branch.as_deref(), Some("branch-1"));
        assert_eq!(card.agent_worktree.as_deref(), Some("/wt/1"));
        assert_eq!(card.agent_worktree_name.as_deref(), Some("wt-1"));
        assert!(card.started_at.is_some());
        assert!(card
            .status_updates
            .iter()
            .any(|update| update.message == "Agent started working on card"));

        // ...plus one roster entry, now the sole (and writing) member, so a later spawn of a
        // different role can claim a reserved slot beside it.
        assert_eq!(card.team_members.len(), 1);
        assert_eq!(card.team_members[0].role, "coder");
        assert_eq!(
            card.team_members[0].member_status,
            Some(refact_tasks::types::TeamStatus::Running)
        );
    }

    #[test]
    fn spawn_into_room_rejected_when_validate_team_fails() {
        // Two writing members that claim the same file: the roster is a valid room shape, but the
        // pair would overwrite each other, so `validate_team` must refuse it.
        let mut card = test_card("T-1", "doing", None);
        card.team_members = vec![
            refact_tasks::rooms::new_room_member("implementer", "a1", "agent-T-1-a1", None, None, None, vec![]),
            refact_tasks::rooms::new_room_member("refactorer", "a2", "agent-T-1-a2", None, None, None, vec![]),
        ];
        card.team_members[0].target_files = vec!["src/lib.rs".into()];
        card.team_members[1].target_files = vec!["src/lib.rs".into()];

        let error = check_spawn_precondition(&card).unwrap_err();

        assert!(error.contains("both write"), "{error}");
        assert!(error.contains("src/lib.rs"), "{error}");
    }

    #[test]
    fn spawn_accepts_a_room_of_readers_and_free_roles() {
        // The removed "a room must contain a coder" rule: what used to be an invalid room is now a
        // legitimate one, because roles are labels and nobody is required to write.
        let mut card = test_card("T-1", "doing", None);
        card.team_members = vec![
            refact_tasks::rooms::new_room_member("researcher", "a1", "agent-T-1-a1", None, None, None, vec![]),
            refact_tasks::rooms::new_room_member("дизайнер", "a2", "agent-T-1-a2", None, None, None, vec![]),
        ];

        assert!(check_spawn_precondition(&card).is_ok());
    }

    #[test]
    fn spawn_rejects_duplicate_room_identity() {
        let mut card = test_card("T-1", "doing", None);
        card.team_members = vec![
            refact_tasks::rooms::new_room_member("architect", "same", "agent-T-1-a", None, None, None, vec![]),
            refact_tasks::rooms::new_room_member("coder", "same", "agent-T-1-b", None, None, None, vec![]),
        ];

        let error = check_spawn_precondition(&card).unwrap_err();

        assert!(error.contains("duplicate team member identity"), "{error}");
        assert!(error.contains("same"), "{error}");
    }

    #[test]
    fn spawn_rejects_running_architect_beside_active_peer() {
        let mut card = test_card("T-1", "doing", None);
        card.team_members = vec![
            refact_tasks::rooms::new_room_member("architect", "a1", "agent-T-1-a1", None, None, None, vec![]),
            refact_tasks::rooms::new_room_member("coder", "a2", "agent-T-1-a2", None, None, None, vec![]),
        ];
        card.team_members[0].member_status = Some(refact_tasks::types::TeamStatus::Running);
        card.team_members[1].member_status = Some(refact_tasks::types::TeamStatus::Running);

        let error = check_spawn_precondition(&card).unwrap_err();

        assert!(error.contains("sequential"), "{error}");
    }

    #[test]
    fn spawn_role_defaults_to_coder_and_accepts_free_text() {
        assert_eq!(spawn_role(&HashMap::new()).unwrap(), DEFAULT_ROOM_ROLE);

        let args = HashMap::from([("role".to_string(), json!("architect"))]);
        assert_eq!(spawn_role(&args).unwrap(), "architect");

        // Roles are free text: the user names a member for this card, and refusing an unknown
        // word would reintroduce the hardcoded job list this card removed.
        for role in ["wizard", "designer", "qa-инженер"] {
            let args = HashMap::from([("role".to_string(), json!(role))]);
            assert_eq!(spawn_role(&args).unwrap(), role);
        }
    }

    #[test]
    fn concurrent_room_claims_do_not_lose_members() {
        // `update_board_atomic` gives each claim the whole board, so a claim can never read a
        // stale roster. Model the critical section: a reserved roster, then each spawn claims its
        // own slot in sequence and every member must be filled, none duplicated or lost.
        let mut card = test_card("T-1", "planned", None);
        card.team_members = vec![
            reserved_slot("coder", Some(true)),
            reserved_slot("reviewer", Some(false)),
            reserved_slot("designer", Some(true)),
        ];

        for index in 0..3 {
            let room = card.clone();
            // Each claimant validated against the snapshot it was handed...
            check_spawn_precondition(&room).unwrap();
            // ...then applied its own mutation on top of the current board.
            claim_room_slot(
                &mut card,
                ["coder", "reviewer", "designer"][index],
                &format!("agent-{index}"),
                &format!("agent-T-1-{index}"),
                None,
                None,
                None,
                None,
                vec![],
            )
            .unwrap();
        }

        assert_eq!(card.team_members.len(), 3);
        let chats: Vec<&str> = card
            .team_members
            .iter()
            .filter_map(|member| member.agent_chat_id.as_deref())
            .collect();
        assert_eq!(
            chats,
            vec!["agent-T-1-0", "agent-T-1-1", "agent-T-1-2"],
            "every claim must fill its slot, none duplicated"
        );
        assert_eq!(card.validate_team().unwrap(), ());
        // The first writer (the coder) is the primary owner.
        assert_eq!(card.assignee.as_deref(), Some("agent-0"));
    }

    #[test]
    fn room_prompt_names_the_roster_and_the_bus() {
        let prompt = build_room_member_prompt(
            "Card T-1",
            "do the thing",
            "",
            30,
            "coder",
            Some("write the parser"),
            &["architect [running] agent-T-1-arch".to_string()],
        );

        assert!(prompt.contains("You are the `coder`"), "{prompt}");
        assert!(prompt.contains("write the parser"), "{prompt}");
        assert!(prompt.contains("agent-T-1-arch"), "{prompt}");
        assert!(
            prompt.contains("agent_message(to=\"<peer chat id>\""),
            "the room must tell the member how to reach peers: {prompt}"
        );
        // The original single-agent guidance must survive.
        assert!(prompt.contains("agent_finish()"), "{prompt}");
        assert!(prompt.contains("## Instructions"), "{prompt}");
    }

}
