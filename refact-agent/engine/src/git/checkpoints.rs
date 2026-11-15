pub use refact_runtime_api::git_runtime::{
    abort_init_shadow_repos, create_workspace_checkpoint, create_workspace_checkpoint_for_root,
    enqueue_init_shadow_repos, init_shadow_repos_if_needed,
    preview_changes_for_workspace_checkpoint, preview_changes_for_workspace_checkpoint_for_root,
    restore_workspace_checkpoint, restore_workspace_checkpoint_for_root, Checkpoint,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileChangeStatus;
    use crate::global_context::GlobalContext;
    use crate::worktrees::types::WorktreeMeta;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct Fixture {
        _temp: tempfile::TempDir,
        source: PathBuf,
        worktree: PathBuf,
        gcx: Arc<GlobalContext>,
        worktree_meta: WorktreeMeta,
    }

    fn write_file(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    async fn fixture() -> Fixture {
        let temp = tempfile::Builder::new()
            .prefix("refact-checkpoint-worktree")
            .tempdir()
            .unwrap();
        let source = temp.path().join("source");
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(source.join("src")).unwrap();
        fs::create_dir_all(worktree.join("src")).unwrap();
        write_file(&source.join("src").join("file.txt"), "source\n");
        write_file(&worktree.join("src").join("file.txt"), "before\n");
        let source = dunce::simplified(&fs::canonicalize(source).unwrap()).to_path_buf();
        let worktree = dunce::simplified(&fs::canonicalize(worktree).unwrap()).to_path_buf();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let worktree_meta = WorktreeMeta {
            id: "wt-checkpoint".to_string(),
            kind: "task_agent".to_string(),
            root: worktree.clone(),
            source_workspace_root: source.clone(),
            repo_root: source.clone(),
            branch: Some("refact/checkpoint".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: None,
            task_id: Some("task".to_string()),
            card_id: Some("card".to_string()),
            agent_id: Some("agent".to_string()),
            enforce: true,
        };
        Fixture {
            _temp: temp,
            source,
            worktree,
            gcx,
            worktree_meta,
        }
    }

    #[tokio::test]
    async fn checkpoint_worktree_create_uses_worktree_root() {
        let f = fixture().await;
        let (checkpoint, repo) = create_workspace_checkpoint_for_root(
            f.gcx.clone(),
            &f.worktree_meta.root,
            None,
            "checkpoint_worktree_create",
        )
        .await
        .unwrap();
        assert_eq!(checkpoint.workspace_folder, f.worktree);
        assert_eq!(repo.workdir().unwrap(), f.worktree.as_path());
        assert!(!checkpoint.commit_hash.is_empty());
    }

    #[tokio::test]
    async fn checkpoint_worktree_preview_reports_worktree_changes() {
        let f = fixture().await;
        let (checkpoint, _) = create_workspace_checkpoint_for_root(
            f.gcx.clone(),
            &f.worktree,
            None,
            "checkpoint_worktree_preview",
        )
        .await
        .unwrap();
        write_file(&f.worktree.join("src").join("file.txt"), "after\n");
        let (files_changed, _reverted_to, checkpoint_for_undo) =
            preview_changes_for_workspace_checkpoint_for_root(
                f.gcx.clone(),
                &f.worktree,
                &checkpoint,
                "checkpoint_worktree_preview",
            )
            .await
            .unwrap();
        assert_eq!(checkpoint_for_undo.workspace_folder, f.worktree);
        let changed_file = files_changed
            .iter()
            .find(|change| change.relative_path == PathBuf::from("src/file.txt"))
            .unwrap();
        assert_eq!(changed_file.status, FileChangeStatus::MODIFIED);
        assert_eq!(
            changed_file.absolute_path,
            f.worktree.join("src").join("file.txt")
        );
        assert!(files_changed
            .iter()
            .all(|change| change.absolute_path.starts_with(&f.worktree)));
    }

    #[tokio::test]
    async fn checkpoint_worktree_restore_reverts_only_worktree() {
        let f = fixture().await;
        let (checkpoint, _) = create_workspace_checkpoint_for_root(
            f.gcx.clone(),
            &f.worktree,
            None,
            "checkpoint_worktree_restore",
        )
        .await
        .unwrap();
        write_file(&f.worktree.join("src").join("file.txt"), "after\n");
        restore_workspace_checkpoint_for_root(
            f.gcx.clone(),
            &f.worktree,
            &checkpoint,
            "checkpoint_worktree_restore",
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(f.worktree.join("src").join("file.txt")).unwrap(),
            "before\n"
        );
        assert_eq!(
            fs::read_to_string(f.source.join("src").join("file.txt")).unwrap(),
            "source\n"
        );
    }

    #[tokio::test]
    async fn checkpoint_worktree_root_mismatch_is_rejected() {
        let f = fixture().await;
        let (checkpoint, _) = create_workspace_checkpoint_for_root(
            f.gcx.clone(),
            &f.worktree,
            None,
            "checkpoint_worktree_mismatch",
        )
        .await
        .unwrap();
        let other = f.source.parent().unwrap().join("other-worktree");
        fs::create_dir_all(&other).unwrap();
        let other = dunce::simplified(&fs::canonicalize(other).unwrap()).to_path_buf();
        let error = restore_workspace_checkpoint_for_root(
            f.gcx.clone(),
            &other,
            &checkpoint,
            "checkpoint_worktree_mismatch",
        )
        .await
        .unwrap_err();
        assert!(error.contains("different workspace folder"));
    }

    #[tokio::test]
    async fn checkpoint_worktree_stale_root_is_rejected() {
        let f = fixture().await;
        let error = match create_workspace_checkpoint_for_root(
            f.gcx.clone(),
            &f.worktree.join("deleted-root"),
            None,
            "checkpoint_worktree_stale",
        )
        .await
        {
            Ok(_) => panic!("stale worktree root unexpectedly created a checkpoint"),
            Err(e) => e,
        };
        assert!(error.contains("does not exist or cannot be resolved"));
    }

    #[tokio::test]
    async fn checkpoint_worktree_legacy_no_worktree_uses_active_workspace() {
        let f = fixture().await;
        write_file(&f.source.join("src").join("legacy.txt"), "legacy before\n");
        let (checkpoint, _) =
            create_workspace_checkpoint(f.gcx.clone(), None, "checkpoint_worktree_legacy")
                .await
                .unwrap();
        assert_eq!(checkpoint.workspace_folder, f.source);
        write_file(&f.source.join("src").join("legacy.txt"), "legacy after\n");
        restore_workspace_checkpoint(f.gcx.clone(), &checkpoint, "checkpoint_worktree_legacy")
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(f.source.join("src").join("legacy.txt")).unwrap(),
            "legacy before\n"
        );
    }
}
