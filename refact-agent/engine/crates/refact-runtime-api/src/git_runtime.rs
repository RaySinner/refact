use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use git2::{Oid, Repository};
use tokio::sync::Mutex as AMutex;
use tokio::time::Instant;

use refact_core::custom_error::MapErrToString;

use crate::files_blocklist::reload_indexing_everywhere_if_needed;
use crate::files_correction::{get_active_workspace_folder, get_project_dirs};
use crate::git_cleanup::RECENT_COMMITS_DURATION;
use crate::global_context::GlobalContext;
use crate::global_context::GlobalContextCtx;
use refact_core::background_tasks::BackgroundTasksHolder;
use refact_git::operations::{
    checkout_head_and_branch_to_commit, get_commit_datetime, get_diff_statuses_index_to_commit,
    get_or_create_branch,
};

pub use refact_core::chat_types::Checkpoint;
pub use refact_git::checkpoints::{
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
        let mut ev = refact_buddy_core::actor::make_runtime_event(
            "checkpoint_saved",
            &format!("Checkpoint saved for chat {}", chat_id),
            "git",
            &format!("checkpoint_{}", chat_id),
            "completed",
            None,
        );
        ev.chat_id = Some(chat_id.to_string());
        crate::buddy_runtime::buddy_enqueue_event(
            crate::global_context::app_state_from_gcx(gcx.clone()).await,
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

pub async fn get_commit_information_from_current_changes(
    gcx: Arc<GlobalContext>,
) -> Vec<refact_git::CommitInfo> {
    let workspace_vcs_roots_arc = gcx.documents_state.workspace_vcs_roots.clone();
    let workspace_vcs_roots = workspace_vcs_roots_arc.lock().unwrap().clone();

    tracing::info!(
        "get_commit_information_from_current_changes() vcs_roots={:?}",
        workspace_vcs_roots
    );
    refact_git::commit_info::commit_information_from_vcs_roots(&workspace_vcs_roots)
}

pub async fn generate_commit_messages(
    gcx: Arc<GlobalContext>,
    commits: Vec<refact_git::CommitInfo>,
) -> Vec<refact_git::CommitInfo> {
    let mut commits_with_messages = Vec::new();
    for mut commit in commits {
        let project_path = commit.project_path.to_file_path().ok().unwrap_or_default();

        let diff = match refact_git::commit_info::workdir_diff_for_project_root(&project_path) {
            Ok(Some(diff)) => diff,
            Ok(None) => continue,
            Err(e) => {
                tracing::error!("{}", e);
                continue;
            }
        };

        commit.commit_message = match crate::commit_message_runtime::generate_commit_message_by_diff(
            gcx.chat_ctx(gcx.clone()),
            &diff,
            &None,
            None,
        )
        .await
        {
            Ok(msg) => msg,
            Err(e) => {
                tracing::error!("{}", e);
                continue;
            }
        };

        commits_with_messages.push(commit);
    }

    commits_with_messages
}
