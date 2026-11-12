use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use git2::{Oid, Repository};
use tokio::sync::Mutex as AMutex;
use tokio::time::Instant;

use refact_core::custom_error::MapErrToString;

use crate::background_tasks::BackgroundTasksHolder;
use crate::files_blocklist::reload_indexing_everywhere_if_needed;
use crate::files_correction::{get_active_workspace_folder, get_project_dirs};
use crate::git::operations::{
    checkout_head_and_branch_to_commit, get_commit_datetime, get_diff_statuses_index_to_commit,
    get_or_create_branch,
};
use crate::git::cleanup::RECENT_COMMITS_DURATION;
use crate::global_context::GlobalContext;

pub use refact_core::chat_types::Checkpoint;
use refact_git::checkpoints::{
    normalize_shadow_path, repo_has_commits, resolve_checkpoint_workspace_folder,
    reverse_file_change_status,
};

async fn open_shadow_repo_and_nested_repos(
    gcx: Arc<GlobalContext>,
    workspace_folder: &Path,
    allow_init_main_repo: bool,
) -> Result<(Repository, Vec<Repository>, String), String> {
    let indexing_everywhere = reload_indexing_everywhere_if_needed(gcx.clone()).await;
    let (cache_dir, vcs_roots) = {
        (
            gcx.cache_dir.clone(),
            gcx.documents_state.workspace_vcs_roots.clone(),
        )
    };
    let nested_vcs_roots: Vec<PathBuf> = {
        let vcs_roots_locked = vcs_roots.lock().unwrap();
        refact_git::checkpoints::nested_vcs_roots_inside(&vcs_roots_locked, workspace_folder)
    };
    let mut indexing_blocklists: Vec<(PathBuf, Vec<String>, Vec<String>)> = Vec::new();
    for path in std::iter::once(workspace_folder.to_path_buf()).chain(nested_vcs_roots.clone()) {
        let path = normalize_shadow_path(&path);
        let settings = indexing_everywhere.indexing_for_path(&path);
        indexing_blocklists.push((
            path,
            settings.blocklist,
            settings.additional_indexing_dirs,
        ));
    }

    refact_git::checkpoints::open_shadow_repo_and_nested_repos(
        &cache_dir,
        workspace_folder,
        &nested_vcs_roots,
        &indexing_blocklists,
        allow_init_main_repo,
    )
}

async fn initialize_shadow_repo_for_root_if_needed(
    gcx: Arc<GlobalContext>,
    workspace_folder: &Path,
) -> Result<(), String> {
    let workspace_folder_str = workspace_folder.to_string_lossy().to_string();
    let abort_flag: Arc<AtomicBool> = gcx.git_operations_abort_flag.clone();
    let (repo, nested_repos, _) =
        open_shadow_repo_and_nested_repos(gcx.clone(), workspace_folder, true).await?;

    if repo_has_commits(&repo) {
        tracing::info!(
            "Shadow git repo for {} is already initialized.",
            workspace_folder_str
        );
        return Ok(());
    }

    let t0 = Instant::now();
    let (_, skipped) =
        refact_git::checkpoints::create_initial_shadow_commit(&repo, &nested_repos, &abort_flag)?;
    if skipped > 0 {
        tracing::warn!(
            "initial commit for {workspace_folder_str}: {skipped} large file(s) not snapshotted"
        );
    }
    tracing::info!(
        "Shadow git repo for {} initialized in {:.2}s.",
        workspace_folder_str,
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

pub async fn create_workspace_checkpoint_for_root(
    gcx: Arc<GlobalContext>,
    workspace_folder: &Path,
    prev_checkpoint: Option<&Checkpoint>,
    chat_id: &str,
) -> Result<(Checkpoint, Repository), String> {
    let t0 = Instant::now();

    let workspace_folder = resolve_checkpoint_workspace_folder(workspace_folder)?;
    if let Some(prev_checkpoint) = prev_checkpoint {
        if prev_checkpoint.workspace_folder != workspace_folder {
            return Err("Can not create checkpoint for different workspace folder".to_string());
        }
    }

    initialize_shadow_repo_for_root_if_needed(gcx.clone(), &workspace_folder).await?;

    let abort_flag: Arc<AtomicBool> = gcx.git_operations_abort_flag.clone();
    let (repo, nested_repos, _) =
        open_shadow_repo_and_nested_repos(gcx.clone(), &workspace_folder, false).await?;

    if !repo_has_commits(&repo) {
        return Err("No commits in shadow git repo.".to_string());
    }

    let checkpoint = {
        let branch = get_or_create_branch(&repo, &format!("refact-{chat_id}"))?;

        let (commit_oid, skipped) = refact_git::checkpoints::stage_and_commit_workdir_changes(
            &repo,
            &nested_repos,
            &branch,
            &format!("Auto commit for chat {chat_id}"),
            &abort_flag,
        )?;
        if skipped > 0 {
            tracing::warn!(
                "checkpoint for chat {chat_id}: {skipped} large file(s) not snapshotted"
            );
        }

        Checkpoint {
            workspace_folder,
            commit_hash: commit_oid.to_string(),
        }
    };

    tracing::info!("Checkpoint created in {:.2}s", t0.elapsed().as_secs_f64());

    {
        let mut ev = crate::buddy::actor::make_runtime_event(
            "checkpoint_saved",
            &format!("Checkpoint saved for chat {}", chat_id),
            "git",
            &format!("checkpoint_{}", chat_id),
            "completed",
            None,
        );
        ev.chat_id = Some(chat_id.to_string());
        crate::buddy::actor::buddy_enqueue_event(
            crate::app_state::app_state_from_gcx(gcx.clone()).await,
            ev,
        )
        .await;
    }

    Ok((checkpoint, repo))
}

pub async fn create_workspace_checkpoint(
    gcx: Arc<GlobalContext>,
    prev_checkpoint: Option<&Checkpoint>,
    chat_id: &str,
) -> Result<(Checkpoint, Repository), String> {
    let workspace_folder = get_active_workspace_folder(gcx.clone())
        .await
        .ok_or_else(|| "No active workspace folder".to_string())?;
    create_workspace_checkpoint_for_root(gcx, &workspace_folder, prev_checkpoint, chat_id).await
}

pub async fn preview_changes_for_workspace_checkpoint_for_root(
    gcx: Arc<GlobalContext>,
    workspace_folder: &Path,
    checkpoint_to_restore: &Checkpoint,
    chat_id: &str,
) -> Result<(Vec<refact_git::FileChange>, DateTime<Utc>, Checkpoint), String> {
    let (checkpoint_for_undo, repo) = create_workspace_checkpoint_for_root(
        gcx.clone(),
        workspace_folder,
        Some(checkpoint_to_restore),
        chat_id,
    )
    .await?;

    let commit_to_restore_oid =
        Oid::from_str(&checkpoint_to_restore.commit_hash).map_err_to_string()?;
    let reverted_to = match get_commit_datetime(&repo, &commit_to_restore_oid) {
        Ok(dt) => dt,
        Err(_) => return Err(
            "This checkpoint has expired (checkpoints older than 3 days are removed automatically)"
                .to_string(),
        ),
    };

    let mut files_changed = match get_diff_statuses_index_to_commit(
        &repo,
        &commit_to_restore_oid,
        true,
    ) {
        Ok(files_changed) => files_changed,
        Err(e) => {
            let recent_cutoff_timestamp = SystemTime::now()
                .checked_sub(RECENT_COMMITS_DURATION)
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            if reverted_to.timestamp() < recent_cutoff_timestamp as i64 {
                return Err("This checkpoint has expired (checkpoints older than 3 days are removed automatically)".to_string());
            } else {
                return Err(e);
            }
        }
    };

    for change in &mut files_changed {
        change.status = reverse_file_change_status(&change.status);
    }

    Ok((files_changed, reverted_to, checkpoint_for_undo))
}

pub async fn preview_changes_for_workspace_checkpoint(
    gcx: Arc<GlobalContext>,
    checkpoint_to_restore: &Checkpoint,
    chat_id: &str,
) -> Result<(Vec<refact_git::FileChange>, DateTime<Utc>, Checkpoint), String> {
    let workspace_folder = get_active_workspace_folder(gcx.clone())
        .await
        .ok_or_else(|| "No active workspace folder".to_string())?;
    preview_changes_for_workspace_checkpoint_for_root(
        gcx,
        &workspace_folder,
        checkpoint_to_restore,
        chat_id,
    )
    .await
}

pub async fn restore_workspace_checkpoint_for_root(
    gcx: Arc<GlobalContext>,
    workspace_folder: &Path,
    checkpoint_to_restore: &Checkpoint,
    chat_id: &str,
) -> Result<(), String> {
    let workspace_folder = resolve_checkpoint_workspace_folder(workspace_folder)?;
    if checkpoint_to_restore.workspace_folder != workspace_folder {
        return Err("Can not restore checkpoint for different workspace folder".to_string());
    }

    let (repo, nested_repos, _) =
        open_shadow_repo_and_nested_repos(gcx.clone(), &workspace_folder, false).await?;

    let commit_to_restore_oid =
        Oid::from_str(&checkpoint_to_restore.commit_hash).map_err_to_string()?;

    checkout_head_and_branch_to_commit(
        &repo,
        &format!("refact-{chat_id}"),
        &commit_to_restore_oid,
    )?;

    for nested_repo in &nested_repos {
        let nested_workdir = match nested_repo.workdir() {
            Some(wd) => wd.to_path_buf(),
            None => {
                tracing::error!("Failed to get workdir for nested repo");
                continue;
            }
        };

        let reset_index_result = nested_repo.index().and_then(|mut index| {
            index.add_all(
                ["*"],
                git2::IndexAddOption::DEFAULT,
                Some(&mut |path, _| {
                    let abs_path = nested_workdir.join(path);
                    if abs_path.is_dir() && abs_path.join(".git").exists() {
                        1
                    } else {
                        0
                    }
                }),
            )?;
            index.write()
        });
        if let Err(e) = reset_index_result {
            tracing::error!(
                "Failed to reset index for {}: {e}",
                nested_workdir.display()
            );
        }
    }

    Ok(())
}

pub async fn restore_workspace_checkpoint(
    gcx: Arc<GlobalContext>,
    checkpoint_to_restore: &Checkpoint,
    chat_id: &str,
) -> Result<(), String> {
    let workspace_folder = get_active_workspace_folder(gcx.clone())
        .await
        .ok_or_else(|| "No active workspace folder".to_string())?;
    restore_workspace_checkpoint_for_root(gcx, &workspace_folder, checkpoint_to_restore, chat_id)
        .await
}

pub async fn init_shadow_repos_if_needed(gcx: Arc<GlobalContext>) -> () {
    let init_shadow_repos_lock: Arc<AMutex<bool>> = gcx.init_shadow_repos_lock.clone();
    let _init_shadow_repos_lock = init_shadow_repos_lock.lock().await; // wait for previous init

    let workspace_folders = get_project_dirs(gcx.clone()).await;
    let abort_flag: Arc<AtomicBool> = gcx.git_operations_abort_flag.clone();

    for workspace_folder in workspace_folders {
        let workspace_folder_str = workspace_folder.to_string_lossy().to_string();

        let (repo, nested_repos) =
            match open_shadow_repo_and_nested_repos(gcx.clone(), &workspace_folder, true).await {
                Ok((repo, nested_repos, _)) => (repo, nested_repos),
                Err(e) => {
                    tracing::error!(
                        "Failed to open or init shadow repo for {workspace_folder_str}: {e}"
                    );
                    continue;
                }
            };

        if repo_has_commits(&repo) {
            tracing::info!(
                "Shadow git repo for {} is already initialized.",
                workspace_folder_str
            );
            continue;
        }

        let t0 = Instant::now();

        match refact_git::checkpoints::create_initial_shadow_commit(
            &repo,
            &nested_repos,
            &abort_flag,
        ) {
            Ok((_, skipped)) => {
                if skipped > 0 {
                    tracing::warn!("initial commit for {workspace_folder_str}: {skipped} large file(s) not snapshotted");
                }
                tracing::info!(
                    "Shadow git repo for {} initialized in {:.2}s.",
                    workspace_folder_str,
                    t0.elapsed().as_secs_f64()
                );
            }
            Err(e) => {
                tracing::error!("Initial commit for {workspace_folder_str} failed: {e}");
                continue;
            }
        }
    }
}

pub async fn enqueue_init_shadow_repos(gcx: Arc<GlobalContext>) {
    // NOTE: potentially we can run init multiple times
    let gcx_cloned = gcx.clone();
    tokio::spawn(async move {
        init_shadow_repos_if_needed(gcx_cloned).await;
    });
}

pub async fn abort_init_shadow_repos(gcx: Arc<GlobalContext>) {
    // NOTE: git2 operations are synchronous and can't be cancelled by tokio abort;
    // we set the abort flag and wait with a timeout to avoid hanging shutdown.
    let holder = {
        gcx.git_operations_abort_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        BackgroundTasksHolder::default()
    };
    let mut holder = holder;
    holder.abort().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileChangeStatus;
    use crate::worktrees::types::WorktreeMeta;
    use std::fs;
    use std::path::{Path, PathBuf};

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
