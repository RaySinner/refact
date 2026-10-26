    use std::process::Command;

    use super::*;
    use crate::types::WorktreeMergeStrategy;

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
        run_git(root, &["add", "file.txt"]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    fn unset_user_config(root: &Path) {
        let _ = Command::new("git")
            .args(["config", "--unset", "user.email"])
            .current_dir(root)
            .output();
        let _ = Command::new("git")
            .args(["config", "--unset", "user.name"])
            .current_dir(root)
            .output();
    }

    fn commit_file(root: &Path, file: &str, content: &str, message: &str) {
        std::fs::write(root.join(file), content).unwrap();
        run_git(root, &["add", file]);
        run_git(root, &["commit", "-m", message]);
    }

    fn branch_exists(root: &Path, branch: &str) -> bool {
        !run_git(root, &["branch", "--list", branch])
            .trim()
            .is_empty()
    }

    fn head_parent_count(root: &Path) -> usize {
        run_git(root, &["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count()
            .saturating_sub(1)
    }

    fn sample_record(service: &WorktreeService, id: &str) -> WorktreeRegistryRecord {
        let now = Utc::now().to_rfc3339();
        WorktreeRegistryRecord {
            meta: WorktreeMeta {
                id: id.to_string(),
                kind: "chat".to_string(),
                root: service.worktree_path_for_id(id).unwrap(),
                source_workspace_root: service.source_workspace_root().to_path_buf(),
                repo_root: service.source_workspace_root().to_path_buf(),
                branch: Some(format!("refact/chat/{}", id)),
                base_branch: Some("main".to_string()),
                base_commit: Some("abc".to_string()),
                task_id: None,
                card_id: None,
                agent_id: None,
                enforce: true,
            },
            created_at: now.clone(),
            updated_at: now,
            last_seen_at: None,
            references: Vec::new(),
            last_known_status: None,
        }
    }

    #[tokio::test]
    async fn worktree_registry_default_base_without_env_override() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        let service =
            WorktreeService::new_with_worktree_root_override(cache.clone(), source, None).unwrap();
        assert_eq!(
            service.registry_dir(),
            cache.join("worktrees").join(service.project_hash())
        );
        assert_eq!(
            service.worktree_path_for_id("wt_1").unwrap(),
            service.registry_dir().join("wt_1")
        );
    }

    #[tokio::test]
    async fn worktree_registry_env_override_moves_registry_and_creates_root() {
        let temp = tempfile::tempdir().unwrap();
        let override_root = temp.path().join("short-wt");
        let source = temp.path().join("source");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        assert!(!override_root.exists());
        let service = WorktreeService::new_with_worktree_root_override(
            cache,
            source,
            Some(override_root.to_str().unwrap()),
        )
        .unwrap();
        assert!(override_root.is_dir());
        assert_eq!(
            service.registry_dir(),
            override_root.join(service.project_hash())
        );
        assert_eq!(
            service.worktree_path_for_id("wt_1").unwrap(),
            override_root.join(service.project_hash()).join("wt_1")
        );
    }

    #[tokio::test]
    async fn worktree_registry_env_override_creates_usable_worktree_with_same_project_hash() {
        let temp = tempfile::tempdir().unwrap();
        let override_root = temp.path().join("short-wt");
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new_with_worktree_root_override(
            cache,
            source,
            Some(override_root.to_str().unwrap()),
        )
        .unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/env-override".to_string()),
                chat_id: Some("chat-1".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let root = created.worktree.meta.root.clone();
        let id = created.worktree.meta.id.clone();
        assert!(root.starts_with(&override_root));
        let registry = service.load_registry().await.unwrap();
        assert_eq!(registry.project_hash, service.project_hash());
        assert!(service.get_worktree(&id).await.is_ok());
        assert!(
            service
                .delete_worktree(&id, true, true)
                .await
                .unwrap()
                .deleted
        );
    }

    #[tokio::test]
    async fn worktree_registry_env_override_rejects_too_long_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        let error = WorktreeService::new_with_worktree_root_override(
            cache,
            source,
            Some(&"a".repeat(121)),
        )
        .unwrap_err();
        assert!(error.contains("32767"), "{error}");
        assert!(error.contains("sccache"), "{error}");
    }

    #[test]
    fn validate_worktree_root_override_boundaries() {
        assert!(validate_worktree_root_override("C:\\wt").is_ok());
        assert!(validate_worktree_root_override(&"a".repeat(90)).is_ok());
        assert!(validate_worktree_root_override(&"a".repeat(121)).is_err());
        assert!(worktrees_base_dir_impl(Some("  "), Path::new("c:/cache")).is_err());
    }

    #[tokio::test]
    async fn worktree_registry_load_save_update_with_temp_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        let service = WorktreeService::new(cache, source).unwrap();
        let mut registry = service.load_registry().await.unwrap();
        assert!(registry.records.is_empty());
        registry.records.push(sample_record(&service, "wt_1"));
        service.save_registry(&registry).await.unwrap();
        let loaded = service.load_registry().await.unwrap();
        assert_eq!(loaded.records.len(), 1);
        let reference = WorktreeReference {
            kind: "chat".to_string(),
            chat_id: Some("chat-1".to_string()),
            task_id: None,
            card_id: None,
            agent_id: None,
        };
        let view = service.add_reference("wt_1", reference).await.unwrap();
        assert_eq!(view.reference_count, 1);
        let loaded = service.load_registry().await.unwrap();
        assert_eq!(loaded.records[0].references.len(), 1);
    }

    #[tokio::test]
    async fn worktree_registry_multiple_references_share_metadata_without_duplication() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        let service = WorktreeService::new(cache, source).unwrap();
        let mut registry = service.load_registry().await.unwrap();
        registry.records.push(sample_record(&service, "wt_1"));
        service.save_registry(&registry).await.unwrap();
        let first = WorktreeReference {
            kind: "chat".to_string(),
            chat_id: Some("chat-1".to_string()),
            task_id: None,
            card_id: None,
            agent_id: None,
        };
        let second = WorktreeReference {
            kind: "chat".to_string(),
            chat_id: Some("chat-2".to_string()),
            task_id: None,
            card_id: None,
            agent_id: None,
        };
        service.add_reference("wt_1", first.clone()).await.unwrap();
        service.add_reference("wt_1", first).await.unwrap();
        let view = service.add_reference("wt_1", second).await.unwrap();
        assert_eq!(view.meta.id, "wt_1");
        assert_eq!(view.reference_count, 2);
        assert_eq!(service.load_registry().await.unwrap().records.len(), 1);
    }

    #[tokio::test]
    async fn worktree_registry_create_list_get_delete_normal_and_stale_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                chat_id: Some("chat-1".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(created.worktree.meta.root.is_dir());
        assert_eq!(service.list_worktrees().await.unwrap().worktrees.len(), 1);
        let got = service
            .get_worktree(&created.worktree.meta.id)
            .await
            .unwrap();
        assert_eq!(got.meta.id, created.worktree.meta.id);
        let deleted = service
            .delete_worktree(&created.worktree.meta.id, true, true)
            .await
            .unwrap();
        assert!(deleted.deleted);
        assert!(!created.worktree.meta.root.exists());
        assert_eq!(service.list_worktrees().await.unwrap().worktrees.len(), 0);

        let mut registry = service.load_registry().await.unwrap();
        registry.records.push(sample_record(&service, "stale_1"));
        service.save_registry(&registry).await.unwrap();
        let deleted = service
            .delete_worktree("stale_1", false, true)
            .await
            .unwrap();
        assert!(deleted.deleted);
        assert!(deleted.stale_path);
        assert_eq!(service.list_worktrees().await.unwrap().worktrees.len(), 0);
    }

    #[tokio::test]
    async fn worktree_registry_create_without_identity_has_no_reference() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/unreferenced-create".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        let view = service
            .get_worktree(&created.worktree.meta.id)
            .await
            .unwrap();

        assert_eq!(view.reference_count, 0);
        assert!(view.references.is_empty());
    }

    #[tokio::test]
    async fn worktree_registry_create_warns_for_dirty_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        std::fs::write(source.join("dirty-source.txt"), "dirty\n").unwrap();
        let service = WorktreeService::new(cache, source).unwrap();

        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-source-warning".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(created.dirty_source_warning);
        assert!(created
            .warnings
            .iter()
            .any(|warning| warning.contains("Source checkout has uncommitted changes")));
    }

    #[tokio::test]
    async fn worktree_registry_list_prunes_missing_records() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/list-prune-existing".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut registry = service.load_registry().await.unwrap();
        registry.records.push(sample_record(&service, "missing_1"));
        service.save_registry(&registry).await.unwrap();

        let list = service.list_worktrees().await.unwrap();

        assert_eq!(list.worktrees.len(), 1);
        assert_eq!(list.worktrees[0].meta.id, created.worktree.meta.id);
        let registry = service.load_registry().await.unwrap();
        assert_eq!(registry.records.len(), 1);
        assert_eq!(registry.records[0].meta.id, created.worktree.meta.id);
    }

    #[tokio::test]
    async fn worktree_registry_list_keeps_referenced_missing_records() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let mut registry = service.load_registry().await.unwrap();
        let mut record = sample_record(&service, "missing_referenced");
        record.references.push(WorktreeReference {
            kind: "chat".to_string(),
            chat_id: Some("chat-1".to_string()),
            ..Default::default()
        });
        registry.records.push(record);
        service.save_registry(&registry).await.unwrap();

        let list = service.list_worktrees().await.unwrap();

        assert!(list.worktrees.is_empty());
        let registry = service.load_registry().await.unwrap();
        assert!(registry.records.is_empty());
    }

    #[tokio::test]
    async fn list_worktrees_prunes_missing_record_even_with_stale_references() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                kind: Some("task_agent".to_string()),
                chat_id: Some("chat-1".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                branch: Some("refact/task/task-1/card/T-1/agent-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let id = created.worktree.meta.id.clone();
        let root = created.worktree.meta.root.clone();

        std::fs::remove_dir_all(&root).unwrap();

        let list = service.list_worktrees().await.unwrap();
        assert!(list.worktrees.is_empty());
        assert!(service.get_worktree(&id).await.is_err());

        let registry = service.load_registry().await.unwrap();
        assert!(registry.records.is_empty());
    }

    #[tokio::test]
    async fn worktree_registry_create_without_base_uses_current_branch_head() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        run_git(&source, &["checkout", "-b", "dev"]);
        commit_file(&source, "dev_only.txt", "only on dev\n", "dev-only");
        let dev_head = run_git(&source, &["rev-parse", "HEAD"]).trim().to_string();
        let service = WorktreeService::new(cache, source).unwrap();

        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/current-branch".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(created.worktree.meta.base_branch.as_deref(), Some("dev"));
        assert_eq!(
            created.worktree.meta.base_commit.as_deref(),
            Some(dev_head.as_str())
        );
        assert!(created.worktree.meta.root.join("dev_only.txt").is_file());
    }

    #[tokio::test]
    async fn worktree_create_uses_explicit_commit_but_keeps_merge_target_branch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let exact = run_git(&source, &["rev-parse", "HEAD"]).trim().to_string();
        commit_file(&source, "later.txt", "later\n", "later");
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/exact-base".to_string()),
                base_branch: Some("main".to_string()),
                base_commit: Some(exact.clone()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(created.worktree.meta.base_branch.as_deref(), Some("main"));
        assert_eq!(
            created.worktree.meta.base_commit.as_deref(),
            Some(exact.as_str())
        );
        assert!(!created.worktree.meta.root.join("later.txt").exists());
    }

    #[tokio::test]
    async fn worktree_registry_create_rejects_existing_branch_reuse() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        run_git(&source, &["branch", "refact/chat/reused"]);
        run_git(&source, &["checkout", "-b", "dev"]);
        commit_file(&source, "dev_only.txt", "only on dev\n", "dev-only");
        let service = WorktreeService::new(cache, source).unwrap();

        let error = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/reused".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap_err();

        assert!(error.contains("already exists"));
    }

    #[tokio::test]
    async fn worktree_registry_create_cleans_branch_after_worktree_failure() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let invalid_worktree_path = source.join("file.txt");

        let error = match git::create_worktree(
            &source,
            &invalid_worktree_path,
            "cleanup-failure",
            "refact/chat/cleanup-failure",
            None,
            None,
        ) {
            Ok(_) => panic!("worktree creation unexpectedly succeeded"),
            Err(error) => error,
        };

        assert!(error.contains("Failed to create worktree"));
        assert!(!branch_exists(&source, "refact/chat/cleanup-failure"));
    }

    #[tokio::test]
    async fn worktree_registry_list_detached_head_has_no_source_branch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let head = run_git(&source, &["rev-parse", "HEAD"]);
        run_git(&source, &["checkout", "--detach", head.trim()]);
        let service = WorktreeService::new(cache, source).unwrap();

        let list = service.list_worktrees().await.unwrap();

        assert!(list.source_current_branch.is_none());
    }

    #[tokio::test]
    async fn worktree_registry_diff_returns_changed_files_and_patch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/diff-test".to_string()),
                chat_id: Some("diff-chat".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let root = created.worktree.meta.root.clone();
        std::fs::write(root.join("file.txt"), "hello committed\n").unwrap();
        run_git(&root, &["add", "file.txt"]);
        run_git(&root, &["commit", "-m", "change tracked"]);
        std::fs::write(root.join("untracked.txt"), "new content\n").unwrap();
        let diff = service
            .diff_worktree_with_limit(&created.worktree.meta.id, 50_000)
            .await
            .unwrap();
        assert!(diff.files.iter().any(|file| file.path == "file.txt"));
        assert!(diff.files.iter().any(|file| file.path == "untracked.txt"));
        assert!(diff.patch.contains("hello committed"));
        assert!(diff.patch.contains("new content"));
        assert!(!diff.patch_truncated);
    }

    #[tokio::test]
    async fn worktree_merge_diff_returns_committed_branch_patch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/merge-diff-committed".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "committed change\n",
            "committed change",
        );

        let diff = service
            .diff_worktree_with_limit(&created.worktree.meta.id, 50_000)
            .await
            .unwrap();

        assert!(diff.files.iter().any(|file| {
            file.path == "file.txt" && file.source == "committed" && file.additions == Some(1)
        }));
        assert!(diff.patch.contains("committed change"));
        assert!(!diff.status.dirty);
    }

    #[tokio::test]
    async fn worktree_diff_reports_base_relative_counts_without_upstream() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/base-relative-counts".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let root = &created.worktree.meta.root;
        let repo = git2::Repository::discover(root).unwrap();
        let branch = repo
            .find_branch("refact/chat/base-relative-counts", git2::BranchType::Local)
            .unwrap();
        assert!(branch.upstream().is_err());
        commit_file(root, "file.txt", "first change\n", "first change");
        commit_file(root, "file.txt", "second change\n", "second change");
        commit_file(root, "file.txt", "third change\n", "third change");

        let diff = service
            .diff_worktree(&created.worktree.meta.id)
            .await
            .unwrap();

        assert_eq!(diff.ahead, Some(3));
        assert_eq!(diff.behind, Some(0));
    }

    #[tokio::test]
    async fn worktree_diff_reports_zero_counts_when_level_with_base() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/base-relative-level".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        let diff = service
            .diff_worktree(&created.worktree.meta.id)
            .await
            .unwrap();

        assert_eq!(diff.ahead, Some(0));
        assert_eq!(diff.behind, Some(0));
    }

    #[tokio::test]
    async fn worktree_diff_omits_counts_for_detached_head_or_missing_base() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let missing_base = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/missing-base-counts".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut registry = service.load_registry().await.unwrap();
        let record = registry
            .records
            .iter_mut()
            .find(|record| record.meta.id == missing_base.worktree.meta.id)
            .unwrap();
        record.meta.base_commit = None;
        record.meta.base_branch = None;
        service.save_registry(&registry).await.unwrap();

        let missing_base_diff = service
            .diff_worktree(&missing_base.worktree.meta.id)
            .await
            .unwrap();

        assert_eq!(missing_base_diff.ahead, None);
        assert_eq!(missing_base_diff.behind, None);

        let detached = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/detached-counts".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let detached_root = &detached.worktree.meta.root;
        let head = run_git(detached_root, &["rev-parse", "HEAD"]);
        run_git(detached_root, &["checkout", "--detach", head.trim()]);

        let detached_diff = service
            .diff_worktree(&detached.worktree.meta.id)
            .await
            .unwrap();

        assert_eq!(detached_diff.ahead, None);
        assert_eq!(detached_diff.behind, None);

        let wrong_branch = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/wrong-branch-counts".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        run_git(
            &wrong_branch.worktree.meta.root,
            &["checkout", "-b", "manual-branch"],
        );

        let wrong_branch_diff = service
            .diff_worktree(&wrong_branch.worktree.meta.id)
            .await
            .unwrap();

        assert_eq!(wrong_branch_diff.ahead, Some(0));
        assert_eq!(wrong_branch_diff.behind, Some(0));
    }

    #[tokio::test]
    async fn worktree_merge_diff_includes_uncommitted_change() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/merge-diff-uncommitted".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        std::fs::write(
            created.worktree.meta.root.join("file.txt"),
            "dirty change\n",
        )
        .unwrap();
        std::fs::write(created.worktree.meta.root.join("new.txt"), "new dirty\n").unwrap();

        let diff = service
            .diff_worktree_with_limit(&created.worktree.meta.id, 50_000)
            .await
            .unwrap();

        assert!(diff.status.dirty);
        assert!(diff
            .files
            .iter()
            .any(|file| file.path == "file.txt" && file.source == "unstaged"));
        assert!(diff
            .files
            .iter()
            .any(|file| file.path == "new.txt" && file.source == "untracked"));
        assert!(diff.patch.contains("dirty change"));
        assert!(diff.patch.contains("new dirty"));
    }

    #[tokio::test]
    async fn worktree_merge_diff_falls_back_to_base_branch_when_base_commit_missing() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/missing-base-commit".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "branch change\n",
            "branch change",
        );
        let mut registry = service.load_registry().await.unwrap();
        registry.records[0].meta.base_commit = None;
        service.save_registry(&registry).await.unwrap();

        let diff = service
            .diff_worktree_with_limit(&created.worktree.meta.id, 50_000)
            .await
            .unwrap();

        assert!(diff.patch.contains("branch change"));
        assert_eq!(diff.stats.committed_files, 1);
    }

    #[tokio::test]
    async fn worktree_merge_diff_invalid_base_blocks_diff_and_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/invalid-base".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &created.worktree.meta.id, 48).await;
        let mut registry = service.load_registry().await.unwrap();
        registry.records[0].meta.base_commit = Some("not-a-commit".to_string());
        service.save_registry(&registry).await.unwrap();

        assert!(service
            .diff_worktree_with_limit(&created.worktree.meta.id, 50_000)
            .await
            .is_err());
        let plan = service
            .cleanup_worktrees_dry_run(WorktreeCleanupRequest {
                ids: vec![created.worktree.meta.id.clone()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(plan.candidates.is_empty());
        assert!(plan.skipped.iter().any(|item| item.reason == "diff_error"));
        let inventory = service.inspect_worktrees_with_min_age(24).await.unwrap();
        assert_eq!(inventory.summary.clean, 0);
        assert_eq!(inventory.summary.unknown, 1);
    }

    #[tokio::test]
    async fn worktree_merge_diff_large_untracked_file_truncates_safely() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/large-untracked".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        std::fs::write(
            created.worktree.meta.root.join("large.txt"),
            "x".repeat(300_000),
        )
        .unwrap();

        let diff = service
            .diff_worktree_with_limit(&created.worktree.meta.id, 1_000)
            .await
            .unwrap();

        assert!(diff.files.iter().any(|file| file.path == "large.txt"));
        assert!(diff.patch_truncated);
        assert!(diff.patch.len() <= 1_128);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_merge_diff_untracked_symlink_does_not_read_external_target() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        let outside = temp.path().join("outside-secret.txt");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(&outside, "secret outside repo\n").unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/symlink-untracked".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        std::os::unix::fs::symlink(&outside, created.worktree.meta.root.join("leak.txt")).unwrap();

        let diff = service
            .diff_worktree(&created.worktree.meta.id)
            .await
            .unwrap();

        assert!(diff.patch.contains("new file mode 120000"));
        assert!(!diff.patch.contains("secret outside repo"));
    }

    #[tokio::test]
    async fn worktree_merge_squash_creates_single_parent_change_on_base() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/squash-merge".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "squash change\n",
            "agent change",
        );

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("squash merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(merged.merged);
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "squash change\n"
        );
        assert_eq!(head_parent_count(&source), 1);
        assert!(created.worktree.meta.root.exists());
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn worktree_merge_auto_commits_dirty_source_worktree_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-source-default".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        std::fs::write(
            created.worktree.meta.root.join("file.txt"),
            "dirty source\n",
        )
        .unwrap();

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("dirty source merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(merged.merged);
        assert!(merged.committed_uncommitted.is_some());
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "dirty source\n"
        );
        assert!(!git::status_for_path(&created.worktree.meta.root).dirty);
    }

    #[tokio::test]
    async fn worktree_merge_refuses_dirty_registered_source_on_different_branch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/source-branch-guard".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        run_git(
            &created.worktree.meta.root,
            &["checkout", "-b", "manual-branch"],
        );
        std::fs::write(
            created.worktree.meta.root.join("file.txt"),
            "dirty wrong branch\n",
        )
        .unwrap();

        let error = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("should not commit wrong branch".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(error.contains("different branch"), "{error}");
        assert_eq!(
            run_git(&created.worktree.meta.root, &["branch", "--show-current"]).trim(),
            "manual-branch"
        );
        assert_eq!(
            std::fs::read_to_string(created.worktree.meta.root.join("file.txt")).unwrap(),
            "dirty wrong branch\n"
        );
    }

    #[tokio::test]
    async fn worktree_merge_refuses_dirty_registered_source_on_detached_head() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/source-detached-guard".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let head = run_git(&created.worktree.meta.root, &["rev-parse", "HEAD"]);
        run_git(
            &created.worktree.meta.root,
            &["checkout", "--detach", head.trim()],
        );
        std::fs::write(
            created.worktree.meta.root.join("file.txt"),
            "dirty detached\n",
        )
        .unwrap();

        let error = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("should not commit detached".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(error.contains("detached HEAD"), "{error}");
        assert_eq!(
            std::fs::read_to_string(created.worktree.meta.root.join("file.txt")).unwrap(),
            "dirty detached\n"
        );
    }

    #[tokio::test]
    async fn worktree_merge_refuses_registered_path_replaced_by_unrelated_repo() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/replaced-root".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let source_branch = created.worktree.meta.branch.clone().unwrap();
        std::fs::remove_dir_all(&created.worktree.meta.root).unwrap();
        std::fs::create_dir_all(&created.worktree.meta.root).unwrap();
        init_repo(&created.worktree.meta.root);
        run_git(
            &created.worktree.meta.root,
            &["checkout", "-b", &source_branch],
        );
        std::fs::write(created.worktree.meta.root.join("file.txt"), "unrelated\n").unwrap();

        let error = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("should reject unrelated repo".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(error.contains("different repository"), "{error}");
    }

    #[tokio::test]
    async fn worktree_reads_refuse_registered_path_replaced_by_unrelated_repo() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/replaced-read-root".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let id = created.worktree.meta.id.clone();
        let root = created.worktree.meta.root.clone();
        let branch = created.worktree.meta.branch.clone().unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        init_repo(&root);
        run_git(&root, &["checkout", "-b", &branch]);

        for error in [
            service.list_worktrees().await.unwrap_err(),
            service.get_worktree(&id).await.unwrap_err(),
            service.open_worktree(&id).await.unwrap_err(),
            service.diff_worktree(&id).await.unwrap_err(),
        ] {
            assert!(error.contains("different repository"), "{error}");
        }
    }

    #[tokio::test]
    async fn worktree_merge_refuses_dirty_target_before_source_auto_commit() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/target-guard-before-source-commit".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        std::fs::write(
            created.worktree.meta.root.join("source-dirty.txt"),
            "source dirty\n",
        )
        .unwrap();
        run_git(&source, &["checkout", "-b", "target-user-dirty"]);
        std::fs::write(source.join("target-dirty.txt"), "target dirty\n").unwrap();

        let error = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("should not auto commit source".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(error.contains("different branch"), "{error}");
        assert_eq!(
            run_git(
                &created.worktree.meta.root,
                &["rev-list", "--count", "HEAD"]
            )
            .trim(),
            "1"
        );
        assert!(
            run_git(&created.worktree.meta.root, &["status", "--porcelain"])
                .contains("source-dirty.txt")
        );
    }

    #[tokio::test]
    async fn worktree_merge_refuses_dirty_target_before_noop_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/target-guard-before-noop-cleanup".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let source_branch = created.worktree.meta.branch.clone().unwrap();
        run_git(&source, &["checkout", "-b", "target-user-dirty-noop"]);
        std::fs::write(source.join("target-dirty.txt"), "target dirty\n").unwrap();

        let error = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: true,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("should not cleanup".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(error.contains("different branch"), "{error}");
        assert!(created.worktree.meta.root.exists());
        assert!(branch_exists(&source, &source_branch));
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn worktree_merge_auto_commit_failure_restores_source_index() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-source-hook-failure".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "agent.txt",
            "agent change\n",
            "agent change",
        );
        std::fs::write(created.worktree.meta.root.join("staged.txt"), "staged\n").unwrap();
        run_git(&created.worktree.meta.root, &["add", "staged.txt"]);
        std::fs::write(
            created.worktree.meta.root.join("unstaged.txt"),
            "unstaged\n",
        )
        .unwrap();
        let git_common_dir = run_git(
            &created.worktree.meta.root,
            &["rev-parse", "--git-common-dir"],
        );
        let git_common_dir = PathBuf::from(git_common_dir.trim());
        let hook = git_common_dir.join("hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&hook).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&hook, perms).unwrap();
        }

        let error = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("source hook failure".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(error.contains("Failed to auto-commit source worktree changes"));
        assert!(run_git(
            &created.worktree.meta.root,
            &["diff", "--cached", "--name-only"]
        )
        .lines()
        .any(|line| line == "staged.txt"));
        assert!(!run_git(
            &created.worktree.meta.root,
            &["diff", "--cached", "--name-only"]
        )
        .lines()
        .any(|line| line == "unstaged.txt"));
        assert!(
            run_git(&created.worktree.meta.root, &["status", "--porcelain"])
                .lines()
                .any(|line| line == "?? unstaged.txt")
        );
    }

    #[tokio::test]
    async fn worktree_merge_refuses_dirty_source_with_merge_in_progress() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/source-merge-in-progress".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "agent side\n",
            "agent side",
        );
        commit_file(&source, "file.txt", "main side\n", "main side");
        let merge_output = Command::new("git")
            .args(["merge", "main"])
            .current_dir(&created.worktree.meta.root)
            .output()
            .unwrap();
        assert!(!merge_output.status.success());
        assert!(git::has_merge_in_progress(&created.worktree.meta.root));

        let error = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("should not commit source merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(error.contains("Source worktree has a merge in progress"));
        assert!(git::has_merge_in_progress(&created.worktree.meta.root));
    }

    #[tokio::test]
    async fn worktree_merge_regular_no_ff_merge_works() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/regular-merge".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "regular change\n",
            "agent change",
        );

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Merge,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("regular merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(merged.merged);
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "regular change\n"
        );
        assert_eq!(head_parent_count(&source), 2);
    }
