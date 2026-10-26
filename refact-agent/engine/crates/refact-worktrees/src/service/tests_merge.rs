    #[tokio::test]
    async fn worktree_merge_regular_no_ff_ignores_gpgsign_config() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/regular-gpgsign".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "regular signed config change\n",
            "agent change",
        );
        run_git(&source, &["config", "commit.gpgsign", "true"]);

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Merge,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("regular merge without gpg signing".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(merged.merged);
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "regular signed config change\n"
        );
        assert_eq!(head_parent_count(&source), 2);
    }

    #[tokio::test]
    async fn worktree_merge_squash_uses_refact_author_without_repo_identity() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/no-identity".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "identity change\n",
            "agent change",
        );
        unset_user_config(&source);

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("identity merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(merged.merged);
        assert_eq!(
            run_git(&source, &["log", "-1", "--format=%an <%ae>"]).trim(),
            "Refact Agent <agent@refact.ai>"
        );
    }

    #[tokio::test]
    async fn worktree_merge_squash_commit_failure_resets_target_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hook-failure".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "hook change\n",
            "agent change",
        );
        let hook = source.join(".git/hooks/pre-commit");
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
                    commit_message: Some("hook failure".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(error.contains("Failed to commit squash merge"));
        git::ensure_clean_worktree(&source, "Target workspace").unwrap();
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn worktree_merge_defaults_to_delete_after_merge_and_reports_references() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/delete-after-merge".to_string()),
                kind: Some("task_agent".to_string()),
                chat_id: Some("chat-1".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        service
            .add_reference(
                &created.worktree.meta.id,
                WorktreeReference {
                    kind: "chat".to_string(),
                    chat_id: Some("chat-2".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "delete after merge\n",
            "agent change",
        );
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("delete after merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let cleanup = merged.cleanup.unwrap();
        assert!(cleanup.worktree_deleted);
        assert!(cleanup.registry_deleted);
        assert!(cleanup.branch_deleted);
        assert_eq!(merged.affected_reference_count, 2);
        assert!(!root.exists());
        assert!(!branch_exists(&source, &branch));
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn worktree_merge_discard_delete_unmerged_worktree_removes_path_and_branch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/delete-unmerged".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "unmerged change\n",
            "agent change",
        );
        let branch = created.worktree.meta.branch.clone().unwrap();
        let root = created.worktree.meta.root.clone();

        let deleted = service
            .delete_worktree(&created.worktree.meta.id, true, true)
            .await
            .unwrap();

        assert!(deleted.deleted);
        assert!(deleted.branch_deleted);
        assert!(!root.exists());
        assert!(!branch_exists(&source, &branch));
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn worktree_merge_preserves_dirty_target_workspace_changes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-target".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "agent change\n",
            "agent change",
        );
        std::fs::write(source.join("target-notes.txt"), "dirty target\n").unwrap();

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("dirty target merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(merged.merged);
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "agent change\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("target-notes.txt")).unwrap(),
            "dirty target\n"
        );
        assert!(git::status_for_path(&source).dirty);
        assert!(run_git(&source, &["stash", "list"]).trim().is_empty());
    }

    #[tokio::test]
    async fn worktree_merge_preserves_dirty_target_without_repo_identity() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-target-no-identity".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "agent change\n",
            "agent change",
        );
        std::fs::write(source.join("target-notes.txt"), "dirty target\n").unwrap();
        unset_user_config(&source);

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("dirty target merge without identity".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(merged.merged);
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "agent change\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("target-notes.txt")).unwrap(),
            "dirty target\n"
        );
        assert!(git::status_for_path(&source).dirty);
        assert!(run_git(&source, &["stash", "list"]).trim().is_empty());
    }

    #[tokio::test]
    async fn worktree_merge_preserves_dirty_target_staged_and_unstaged_state() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        commit_file(&source, "target-base.txt", "target base\n", "target base");
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-target-index".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "agent change\n",
            "agent change",
        );
        std::fs::write(source.join("staged-target.txt"), "staged target\n").unwrap();
        run_git(&source, &["add", "staged-target.txt"]);
        std::fs::write(source.join("target-base.txt"), "unstaged target\n").unwrap();

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("dirty target index merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(merged.merged);
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "agent change\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("staged-target.txt")).unwrap(),
            "staged target\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("target-base.txt")).unwrap(),
            "unstaged target\n"
        );
        assert!(run_git(&source, &["diff", "--cached", "--name-only"])
            .lines()
            .any(|line| line == "staged-target.txt"));
        assert!(run_git(&source, &["diff", "--name-only"])
            .lines()
            .any(|line| line == "target-base.txt"));
        assert!(run_git(&source, &["stash", "list"]).trim().is_empty());
    }

    #[tokio::test]
    async fn worktree_merge_blocks_dirty_target_changes_from_different_branch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-target-cross-branch".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "agent change\n",
            "agent change",
        );
        run_git(&source, &["checkout", "-b", "user-dirty"]);
        std::fs::write(source.join("target-notes.txt"), "dirty target\n").unwrap();

        let err = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("dirty target cross branch".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(err.contains("different branch"));
        assert!(err.contains("user-dirty"));
        assert_eq!(
            run_git(&source, &["branch", "--show-current"]).trim(),
            "user-dirty"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("target-notes.txt")).unwrap(),
            "dirty target\n"
        );
        assert!(git::status_for_path(&source).dirty);
        assert!(run_git(&source, &["stash", "list"]).trim().is_empty());
    }

    #[tokio::test]
    async fn worktree_merge_blocks_dirty_target_changes_from_detached_head() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-target-detached".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "agent change\n",
            "agent change",
        );
        run_git(&source, &["checkout", "--detach", "HEAD"]);
        let detached_head = run_git(&source, &["rev-parse", "HEAD"]).trim().to_string();
        std::fs::write(source.join("target-notes.txt"), "dirty detached target\n").unwrap();

        let err = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("dirty target detached".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(err.contains("detached HEAD"));
        assert_eq!(run_git(&source, &["branch", "--show-current"]).trim(), "");
        assert_eq!(
            run_git(&source, &["rev-parse", "HEAD"]).trim(),
            detached_head
        );
        assert_eq!(
            std::fs::read_to_string(source.join("target-notes.txt")).unwrap(),
            "dirty detached target\n"
        );
        assert!(git::status_for_path(&source).dirty);
        assert!(run_git(&source, &["stash", "list"]).trim().is_empty());
    }

    #[tokio::test]
    async fn worktree_merge_conflicting_dirty_target_change_rolls_back_and_restores() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-target-conflict".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "agent change\n",
            "agent change",
        );
        std::fs::write(source.join("file.txt"), "dirty target\n").unwrap();

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("dirty target conflict".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(merged.status, "conflict");
        assert!(!merged.merged);
        let conflict = merged.conflict.unwrap();
        assert!(conflict.aborted);
        assert!(!conflict.merge_in_progress);
        assert!(conflict.files.contains(&"file.txt".to_string()));
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "dirty target\n"
        );
        assert!(git::status_for_path(&source).dirty);
        assert!(run_git(&source, &["stash", "list"]).trim().is_empty());
    }

    #[tokio::test]
    async fn worktree_merge_untracked_dirty_target_conflict_rolls_back_and_restores() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/dirty-target-untracked-conflict".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(
            &created.worktree.meta.root,
            "new-file.txt",
            "agent file\n",
            "agent adds file",
        );
        std::fs::write(source.join("new-file.txt"), "dirty target file\n").unwrap();

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Squash,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("dirty target untracked conflict".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(merged.status, "conflict");
        assert!(!merged.merged);
        let conflict = merged.conflict.unwrap();
        assert!(conflict.aborted);
        assert!(!conflict.merge_in_progress);
        assert_eq!(
            std::fs::read_to_string(source.join("new-file.txt")).unwrap(),
            "dirty target file\n"
        );
        assert!(merged
            .warnings
            .iter()
            .any(|warning| warning.contains("merge was rolled back")));
        assert!(git::status_for_path(&source).dirty);
        assert!(run_git(&source, &["stash", "list"]).trim().is_empty());
    }

    #[tokio::test]
    async fn worktree_merge_conflict_returns_files_and_keeps_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/conflict-merge".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        commit_file(&source, "file.txt", "target change\n", "target change");
        commit_file(
            &created.worktree.meta.root,
            "file.txt",
            "source change\n",
            "source change",
        );

        let merged = service
            .merge_worktree(
                &created.worktree.meta.id,
                MergeWorktreeRequest {
                    strategy: WorktreeMergeStrategy::Merge,
                    delete_after_merge: false,
                    target_branch: Some("main".to_string()),
                    commit_message: Some("conflict merge".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(merged.status, "conflict");
        assert!(!merged.merged);
        assert!(merged
            .conflict
            .unwrap()
            .files
            .contains(&"file.txt".to_string()));
        assert!(created.worktree.meta.root.exists());
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "target change\n"
        );
        assert!(service
            .get_worktree(&created.worktree.meta.id)
            .await
            .is_ok());
    }

    async fn mark_worktree_old(service: &WorktreeService, id: &str, hours: i64) {
        let mut registry = service.load_registry().await.unwrap();
        let record = registry
            .records
            .iter_mut()
            .find(|record| record.meta.id == id)
            .unwrap();
        let ts = (Utc::now() - chrono::Duration::hours(hours)).to_rfc3339();
        record.created_at = ts.clone();
        record.updated_at = ts.clone();
        record.last_seen_at = Some(ts);
        service.save_registry(&registry).await.unwrap();
    }

    fn replace_registered_root_with_unrelated_repo(source: &Path, root: &Path, branch: &str) {
        std::fs::remove_dir_all(root).unwrap();
        std::fs::create_dir_all(root).unwrap();
        run_git(root, &["init"]);
        run_git(root, &["config", "core.autocrlf", "false"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        run_git(root, &["fetch", source.to_str().unwrap(), "main"]);
        run_git(root, &["checkout", "-b", branch, "FETCH_HEAD"]);
        std::fs::write(root.join("replacement-only.txt"), "replacement contents\n").unwrap();
        run_git(root, &["add", "replacement-only.txt"]);
        run_git(root, &["commit", "-m", "replacement contents"]);
    }

    #[tokio::test]
    async fn worktree_summary_marks_replaced_registered_root_invalid_without_analyzing_it() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/replaced-summary-root".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let id = created.worktree.meta.id.clone();
        let root = created.worktree.meta.root.clone();
        let branch = created.worktree.meta.branch.clone().unwrap();
        replace_registered_root_with_unrelated_repo(&source, &root, &branch);

        let inventory = service.inspect_worktrees_with_min_age(24).await.unwrap();

        let item = inventory
            .worktrees
            .iter()
            .find(|item| item.id == id)
            .unwrap();
        assert!(item.stale);
        assert!(!item.status.is_git_worktree);
        assert_eq!(item.changed_files, 0);
        assert_eq!(item.additions, 0);
        assert_eq!(item.deletions, 0);
        assert!(item.disk_usage_bytes.is_none());
        assert!(item.branch_merged.is_none());
        assert!(item
            .status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("different repository")));
        assert!(!item.cleanup_candidate);
        assert!(!inventory.cleanup_candidates.contains(&id));
        assert_eq!(inventory.summary.dirty, 0);
        assert_eq!(inventory.summary.changed_files, 0);
        assert_eq!(inventory.summary.additions, 0);
        assert_eq!(inventory.summary.deletions, 0);
        assert!(root.join("replacement-only.txt").is_file());
    }

    #[tokio::test]
    async fn worktree_cleanup_dry_run_skips_replaced_registered_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/replaced-cleanup-dry-run".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let id = created.worktree.meta.id.clone();
        let root = created.worktree.meta.root.clone();
        let branch = created.worktree.meta.branch.clone().unwrap();
        mark_worktree_old(&service, &id, 48).await;
        replace_registered_root_with_unrelated_repo(&source, &root, &branch);

        let plan = service
            .cleanup_worktrees_dry_run(WorktreeCleanupRequest {
                ids: vec![id.clone()],
                clean_only: false,
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(plan.candidates.is_empty());
        assert!(plan.skipped.iter().any(|item| {
            item.id == id
                && item.reason == "not_git_worktree"
                && item.details.contains(&"diff_error".to_string())
        }));
        assert!(root.join("replacement-only.txt").is_file());
    }

    #[tokio::test]
    async fn worktree_cleanup_execution_skips_replaced_registered_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/replaced-cleanup-execute".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let id = created.worktree.meta.id.clone();
        let root = created.worktree.meta.root.clone();
        let branch = created.worktree.meta.branch.clone().unwrap();
        mark_worktree_old(&service, &id, 48).await;
        replace_registered_root_with_unrelated_repo(&source, &root, &branch);

        let result = service
            .cleanup_worktrees(WorktreeCleanupRequest {
                ids: vec![id.clone()],
                clean_only: false,
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(result.deleted.is_empty());
        assert!(result.skipped.iter().any(|item| {
            item.id == id
                && item.reason == "not_git_worktree"
                && item.details.contains(&"diff_error".to_string())
        }));
        assert!(root.join("replacement-only.txt").is_file());
        assert!(service
            .load_registry()
            .await
            .unwrap()
            .records
            .iter()
            .any(|record| record.meta.id == id));
    }

    #[tokio::test]
    async fn worktree_hygiene_classifies_clean_dirty_stale_shared_and_conflicted() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let clean = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-clean".to_string()),
                chat_id: Some("chat-clean".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &clean.worktree.meta.id, 48).await;
        service
            .add_reference(
                &clean.worktree.meta.id,
                WorktreeReference {
                    kind: "chat".to_string(),
                    chat_id: Some("chat-clean-2".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let dirty = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-dirty".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &dirty.worktree.meta.id, 48).await;
        std::fs::write(dirty.worktree.meta.root.join("dirty.txt"), "dirty\n").unwrap();
        let conflicted = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-conflict".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &conflicted.worktree.meta.id, 48).await;
        commit_file(&source, "file.txt", "target\n", "target");
        commit_file(
            &conflicted.worktree.meta.root,
            "file.txt",
            "source\n",
            "source",
        );
        let _ = Command::new("git")
            .args(["merge", "main"])
            .current_dir(&conflicted.worktree.meta.root)
            .output()
            .unwrap();
        let mut registry = service.load_registry().await.unwrap();
        registry.records.push(sample_record(&service, "stale_1"));
        service.save_registry(&registry).await.unwrap();

        let inventory = service.inspect_worktrees_with_min_age(24).await.unwrap();

        assert_eq!(inventory.summary.total_registered, 4);
        assert!(inventory.summary.dirty >= 1);
        assert!(inventory.summary.stale >= 1);
        assert!(inventory.summary.conflicted >= 1);
        assert!(inventory.summary.shared >= 1);
        assert!(inventory
            .worktrees
            .iter()
            .any(|item| item.id == dirty.worktree.meta.id && item.changed_files > 0));
    }

    #[tokio::test]
    async fn worktree_hygiene_changed_file_count_aggregation_and_candidate_selection() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let clean = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-candidate".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &clean.worktree.meta.id, 48).await;
        let dirty = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-aggregate".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &dirty.worktree.meta.id, 48).await;
        std::fs::write(dirty.worktree.meta.root.join("a.txt"), "a\n").unwrap();
        std::fs::write(dirty.worktree.meta.root.join("b.txt"), "b\n").unwrap();

        let inventory = service.inspect_worktrees_with_min_age(24).await.unwrap();

        assert!(inventory.summary.changed_files >= 2);
        assert!(inventory.summary.additions >= 2);
        assert!(inventory
            .cleanup_candidates
            .contains(&clean.worktree.meta.id));
        assert!(!inventory
            .cleanup_candidates
            .contains(&dirty.worktree.meta.id));
    }

    #[tokio::test]
    async fn worktree_hygiene_cleanup_dry_run_does_not_delete_and_blocks_shared_dirty() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let clean = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-dry-clean".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &clean.worktree.meta.id, 48).await;
        let shared = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-dry-shared".to_string()),
                chat_id: Some("chat-shared".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &shared.worktree.meta.id, 48).await;
        service
            .add_reference(
                &shared.worktree.meta.id,
                WorktreeReference {
                    kind: "chat".to_string(),
                    chat_id: Some("chat-shared-2".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let dirty = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-dry-dirty".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &dirty.worktree.meta.id, 48).await;
        std::fs::write(dirty.worktree.meta.root.join("dirty.txt"), "dirty\n").unwrap();

        let plan = service
            .cleanup_worktrees_dry_run(WorktreeCleanupRequest {
                ids: vec![
                    clean.worktree.meta.id.clone(),
                    shared.worktree.meta.id.clone(),
                    dirty.worktree.meta.id.clone(),
                ],
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.candidates[0].id, clean.worktree.meta.id);
        assert!(plan.skipped.iter().any(|item| item.reason == "shared"));
        assert!(plan.skipped.iter().any(|item| item.reason == "dirty"));
        assert!(clean.worktree.meta.root.exists());
        assert!(service.get_worktree(&clean.worktree.meta.id).await.is_ok());
    }

    #[tokio::test]
    async fn worktree_hygiene_cleanup_actual_deletes_only_selected_safe_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let clean = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-delete-clean".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &clean.worktree.meta.id, 48).await;
        let dirty = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/hygiene-delete-dirty".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &dirty.worktree.meta.id, 48).await;
        std::fs::write(dirty.worktree.meta.root.join("dirty.txt"), "dirty\n").unwrap();
        let clean_root = clean.worktree.meta.root.clone();
        let dirty_root = dirty.worktree.meta.root.clone();
        let branch = clean.worktree.meta.branch.clone().unwrap();

        let result = service
            .cleanup_worktrees(WorktreeCleanupRequest {
                ids: vec![
                    clean.worktree.meta.id.clone(),
                    dirty.worktree.meta.id.clone(),
                ],
                delete_branches: true,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.deleted.len(), 1);
        assert_eq!(result.deleted[0].id, clean.worktree.meta.id);
        assert!(result.deleted[0].worktree_deleted);
        assert!(result.deleted[0].branch_deleted);
        assert!(result.skipped.iter().any(|item| item.reason == "dirty"));
        assert!(!clean_root.exists());
        assert!(dirty_root.exists());
        assert!(!branch_exists(&source, &branch));
        assert!(service.get_worktree(&clean.worktree.meta.id).await.is_err());
    }

    #[tokio::test]
    async fn worktree_hygiene_cleanup_blocks_single_referenced_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let referenced = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/referenced-clean".to_string()),
                chat_id: Some("chat-referenced".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &referenced.worktree.meta.id, 48).await;

        let plan = service
            .cleanup_worktrees_dry_run(WorktreeCleanupRequest {
                ids: vec![referenced.worktree.meta.id.clone()],
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(plan.candidates.is_empty());
        assert!(plan.skipped.iter().any(|item| item.reason == "referenced"));
    }

    #[tokio::test]
    async fn worktree_hygiene_allow_shared_still_blocks_single_reference() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let referenced = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/allow-shared-single".to_string()),
                chat_id: Some("chat-referenced".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &referenced.worktree.meta.id, 48).await;

        let plan = service
            .cleanup_worktrees_dry_run(WorktreeCleanupRequest {
                ids: vec![referenced.worktree.meta.id.clone()],
                allow_shared: true,
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(plan.candidates.is_empty());
        assert!(plan.skipped.iter().any(|item| item.reason == "referenced"));
    }

    #[tokio::test]
    async fn worktree_hygiene_allow_shared_still_blocks_shared_references() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let referenced = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/allow-shared-many".to_string()),
                chat_id: Some("chat-referenced".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        service
            .add_reference(
                &referenced.worktree.meta.id,
                WorktreeReference {
                    kind: "chat".to_string(),
                    chat_id: Some("chat-referenced-2".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        mark_worktree_old(&service, &referenced.worktree.meta.id, 48).await;

        let plan = service
            .cleanup_worktrees_dry_run(WorktreeCleanupRequest {
                ids: vec![referenced.worktree.meta.id.clone()],
                allow_shared: true,
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(plan.candidates.is_empty());
        assert!(plan.skipped.iter().any(|item| item.reason == "referenced"));
    }

    #[tokio::test]
    async fn worktree_hygiene_clean_only_blocks_status_dirty_even_without_diff_stats() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source).unwrap();
        let dirty = service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/status-dirty".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        mark_worktree_old(&service, &dirty.worktree.meta.id, 48).await;
        std::fs::write(dirty.worktree.meta.root.join("dirty.txt"), "dirty\n").unwrap();
        let mut registry = service.load_registry().await.unwrap();
        registry.records[0].meta.base_commit = None;
        registry.records[0].meta.base_branch = None;
        service.save_registry(&registry).await.unwrap();

        let plan = service
            .cleanup_worktrees_dry_run(WorktreeCleanupRequest {
                ids: vec![dirty.worktree.meta.id.clone()],
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(plan.candidates.is_empty());
        assert!(plan
            .skipped
            .iter()
            .any(|item| item.details.contains(&"dirty".to_string())));
    }

    #[tokio::test]
    async fn worktree_registry_invalid_ids_paths_and_non_git_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        let service = WorktreeService::new(cache, source).unwrap();
        assert!(service.get_worktree("../bad").await.is_err());
        assert!(service.worktree_path_for_id("../bad").is_err());
        assert!(service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("../bad".to_string()),
                ..Default::default()
            })
            .await
            .is_err());
        assert!(service
            .create_worktree(CreateWorktreeRequest {
                branch: Some("refact/chat/non-git".to_string()),
                ..Default::default()
            })
            .await
            .unwrap_err()
            .contains("not a git repository"));
        let mut registry = service.load_registry().await.unwrap();
        let mut record = sample_record(&service, "wt_1");
        record.meta.root = temp.path().join("outside");
        registry.records.push(record);
        assert!(service.save_registry(&registry).await.is_err());
        let mut registry = service.load_registry().await.unwrap();
        let mut record = sample_record(&service, "wt_2");
        record.meta.root = service.registry_dir();
        registry.records.push(record);
        let error = service.save_registry(&registry).await.unwrap_err();
        assert!(error.contains("must exactly match registry path"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_registry_rejects_symlink_escape_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let cache = temp.path().join("cache");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let service = WorktreeService::new(cache, source).unwrap();
        std::fs::create_dir_all(service.registry_dir()).unwrap();
        std::os::unix::fs::symlink(&outside, service.registry_dir().join("wt_1")).unwrap();
        let mut registry = service.load_registry().await.unwrap();
        let mut record = sample_record(&service, "wt_1");
        record.meta.root = outside;
        registry.records.push(record);

        let error = service.save_registry(&registry).await.unwrap_err();

        assert!(error.contains("must exactly match registry path"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_registry_rejects_expected_root_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let cache = temp.path().join("cache");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let service = WorktreeService::new(cache, source).unwrap();
        std::fs::create_dir_all(service.registry_dir()).unwrap();
        std::os::unix::fs::symlink(&outside, service.registry_dir().join("wt_1")).unwrap();
        let mut registry = service.load_registry().await.unwrap();
        registry.records.push(sample_record(&service, "wt_1"));

        let error = service.save_registry(&registry).await.unwrap_err();

        assert!(error.contains("must not be a symlink"));
    }

    #[tokio::test]
    async fn delete_worktree_refuses_referenced_unless_forced() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                kind: Some("task_agent".to_string()),
                chat_id: Some("agent-chat-1".to_string()),
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

        let err = service.delete_worktree(&id, true, false).await.unwrap_err();
        assert!(
            err.contains("active reference") && err.contains("force_referenced=true"),
            "{err}"
        );
        assert!(root.exists(), "worktree must not be deleted on refusal");
        assert!(service.get_worktree(&id).await.is_ok());

        let deleted = service.delete_worktree(&id, true, true).await.unwrap();
        assert!(deleted.deleted);
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn validate_worktree_meta_strict_rejects_missing_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let service = WorktreeService::new(cache, source.canonicalize().unwrap()).unwrap();
        let created = service
            .create_worktree(CreateWorktreeRequest {
                kind: Some("task_agent".to_string()),
                chat_id: Some("agent-chat-2".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("T-2".to_string()),
                agent_id: Some("agent-2".to_string()),
                branch: Some("refact/task/task-1/card/T-2/agent-2".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let meta = created.worktree.meta.clone();
        std::fs::remove_dir_all(&meta.root).unwrap();

        let err = service
            .validate_worktree_meta_strict(&meta)
            .await
            .unwrap_err();
        assert!(
            err.contains("no longer exists on disk"),
            "expected missing-root error, got: {err}"
        );
    }

    #[tokio::test]
    async fn validate_legacy_task_agent_worktree_meta_rejects_source_workspace_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let source = source.canonicalize().unwrap();
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let meta = WorktreeMeta {
            id: "00000000-0000-0000-0000-000000000001".to_string(),
            kind: "task_agent".to_string(),
            root: source.clone(),
            source_workspace_root: source.clone(),
            repo_root: source.clone(),
            branch: Some("refact/task/task-1/card/T-1/legacy".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: None,
            task_id: Some("task-1".to_string()),
            card_id: Some("T-1".to_string()),
            agent_id: Some("legacy".to_string()),
            enforce: true,
        };

        let err = service
            .validate_legacy_task_agent_worktree_meta(&meta)
            .await
            .unwrap_err();
        assert!(
            err.contains("equals the source workspace root"),
            "expected source-workspace rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn validate_legacy_task_agent_worktree_meta_rejects_root_outside_cache() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        let outside = temp.path().join("outside-worktree");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        init_repo(&source);
        let source = source.canonicalize().unwrap();
        let outside = outside.canonicalize().unwrap();
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let meta = WorktreeMeta {
            id: "00000000-0000-0000-0000-000000000002".to_string(),
            kind: "task_agent".to_string(),
            root: outside,
            source_workspace_root: source.clone(),
            repo_root: source,
            branch: Some("refact/task/task-1/card/T-1/legacy".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: None,
            task_id: Some("task-1".to_string()),
            card_id: Some("T-1".to_string()),
            agent_id: Some("legacy".to_string()),
            enforce: true,
        };

        let err = service
            .validate_legacy_task_agent_worktree_meta(&meta)
            .await
            .unwrap_err();
        assert!(
            err.contains("outside the Refact worktree cache"),
            "expected outside-cache rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn validate_legacy_task_agent_worktree_meta_rejects_wrong_branch() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("repo");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let source = source.canonicalize().unwrap();
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let id = "00000000-0000-0000-0000-000000000003";
        let root = service.worktree_path_for_id(id).unwrap();
        let expected_branch = "refact/task/task-1/card/T-1/legacy";
        std::fs::create_dir_all(service.registry_dir()).unwrap();
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                expected_branch,
                root.to_str().unwrap(),
                "main",
            ],
        );
        let meta = WorktreeMeta {
            id: id.to_string(),
            kind: "task_agent".to_string(),
            root: root.clone(),
            source_workspace_root: source.clone(),
            repo_root: source,
            branch: Some(expected_branch.to_string()),
            base_branch: Some("main".to_string()),
            base_commit: None,
            task_id: Some("task-1".to_string()),
            card_id: Some("T-1".to_string()),
            agent_id: Some("legacy".to_string()),
            enforce: true,
        };
        assert_eq!(
            service
                .validate_legacy_task_agent_worktree_meta(&meta)
                .await
                .unwrap(),
            meta
        );
        run_git(&root, &["checkout", "-b", "legacy-wrong-branch"]);

        let err = service
            .validate_legacy_task_agent_worktree_meta(&meta)
            .await
            .unwrap_err();

        assert!(err.contains("different branch"), "{err}");
        assert!(err.contains("legacy-wrong-branch"), "{err}");
        assert!(err.contains(expected_branch), "{err}");
    }
