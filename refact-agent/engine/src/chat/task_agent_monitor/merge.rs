#[allow(unused_imports)]
use super::*;

#[allow(unused_imports)]
use super::*;

pub(super) fn expected_agent_branch_prefix(task_id: &str, card_id: &str) -> String {
    format!("refact/task/{}/card/{}/", task_id, card_id)
}

pub(super) fn validate_agent_branch_for_cleanup(
    task_id: &str,
    card_id: &str,
    branch: &str,
) -> Result<(), String> {
    let expected_prefix = expected_agent_branch_prefix(task_id, card_id);
    if branch.starts_with(&expected_prefix) {
        Ok(())
    } else {
        Err(format!(
            "Refusing to delete unsafe branch '{}'; expected prefix '{}'.",
            branch, expected_prefix
        ))
    }
}

pub(super) fn canonical_existing_path(path: &Path) -> Result<PathBuf, String> {
    dunce::canonicalize(path)
        .map_err(|e| format!("Failed to canonicalize '{}': {}", path.display(), e))
}

pub(super) fn validate_agent_worktree_path_for_cleanup(
    cache_dir: &Path,
    workspace_roots: &[PathBuf],
    worktree: &Path,
) -> Result<PathBuf, String> {
    let worktree = canonical_existing_path(worktree)?;
    let cache_dir = canonical_existing_path(cache_dir)?;
    let cache_root = refact_worktrees::service::worktrees_base_dir(&cache_dir)
        .unwrap_or_else(|_| cache_dir.join("worktrees"));
    let cache_root = if cache_root.exists() {
        canonical_existing_path(&cache_root)?
    } else {
        return Err(format!(
            "Refusing to delete worktree path '{}': worktree cache '{}' does not exist.",
            worktree.display(),
            cache_root.display()
        ));
    };

    if worktree == Path::new("/") || worktree == cache_dir || worktree == cache_root {
        return Err(format!(
            "Refusing to delete unsafe worktree path '{}'.",
            worktree.display()
        ));
    }

    for workspace_root in workspace_roots {
        if let Ok(workspace_root) = canonical_existing_path(workspace_root) {
            if worktree == workspace_root {
                return Err(format!(
                    "Refusing to delete workspace root '{}'.",
                    worktree.display()
                ));
            }
        }
    }

    if !worktree.starts_with(&cache_root) {
        return Err(format!(
            "Refusing to delete worktree path '{}' outside Refact worktree cache '{}'.",
            worktree.display(),
            cache_root.display()
        ));
    }

    Ok(worktree)
}

pub(super) fn fallback_remove_worktree_and_branch(
    cache_dir: PathBuf,
    workspace_root: PathBuf,
    workspace_roots: Vec<PathBuf>,
    task_id: String,
    card_id: String,
    agent_worktree: String,
    agent_branch: String,
    remove_worktree: bool,
    remove_branch: bool,
) -> (bool, bool) {
    let branch_is_safe = match validate_agent_branch_for_cleanup(&task_id, &card_id, &agent_branch)
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("Skipping fallback agent branch cleanup: {}", e);
            false
        }
    };

    let mut worktree_removed = false;
    if remove_worktree {
        match validate_agent_worktree_path_for_cleanup(
            &cache_dir,
            &workspace_roots,
            Path::new(&agent_worktree),
        ) {
            Ok(worktree) => {
                worktree_removed = !worktree.exists();
                if !worktree_removed {
                    let warnings = git::remove_worktree_path(&workspace_root, &worktree);
                    for warning in warnings {
                        tracing::warn!("Fallback agent worktree cleanup warning: {}", warning);
                    }
                    worktree_removed = !worktree.exists();
                }
            }
            Err(e) => tracing::warn!("Skipping fallback agent worktree cleanup: {}", e),
        }
    }

    let mut branch_deleted = false;
    if remove_branch && branch_is_safe {
        match git::delete_branch(&workspace_root, &agent_branch) {
            Ok(deleted) => branch_deleted = deleted,
            Err(e) => tracing::warn!("Fallback agent branch cleanup failed: {}", e),
        }
    }

    (worktree_removed, branch_deleted)
}

pub async fn remove_agent_worktree_and_branch(
    ctx: ChatCtx,
    task_id: &str,
    card_id: &str,
    agent_worktree: &str,
    agent_branch: &str,
    agent_worktree_name: Option<&str>,
) -> (bool, bool) {
    let cache_dir = ctx.paths.cache_dir.clone();
    let project_dirs = crate::files_correction::get_unscoped_project_dirs_for_ctx(&ctx).await;
    let Some(workspace_root) = project_dirs.first().cloned() else {
        tracing::warn!(
            "Cannot cleanup failed agent worktree for card {}: no workspace folder found",
            card_id
        );
        return (false, false);
    };

    let mut worktree_removed = !tokio::fs::try_exists(agent_worktree).await.unwrap_or(true);
    let mut branch_deleted = false;

    if let Some(worktree_id) = agent_worktree_name {
        if let Ok(service) =
            WorktreeService::new_async(cache_dir.clone(), workspace_root.clone()).await
        {
            match service.delete_worktree(worktree_id, true, true).await {
                Ok(deleted) => {
                    worktree_removed = deleted.deleted
                        && !tokio::fs::try_exists(agent_worktree).await.unwrap_or(true);
                    branch_deleted = deleted.branch_deleted;
                    for warning in deleted.warnings {
                        tracing::warn!(
                            "Registered agent worktree cleanup warning for '{}': {}",
                            worktree_id,
                            warning
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to delete registered worktree '{}': {}",
                        worktree_id,
                        e
                    );
                }
            }
        }
    }

    if worktree_removed && branch_deleted {
        return (true, true);
    }

    let remove_worktree = !worktree_removed;
    let remove_branch = !branch_deleted;
    let task_id = task_id.to_string();
    let card_id = card_id.to_string();
    let agent_worktree = agent_worktree.to_string();
    let agent_branch = agent_branch.to_string();
    tokio::task::spawn_blocking(move || {
        fallback_remove_worktree_and_branch(
            cache_dir,
            workspace_root,
            project_dirs,
            task_id,
            card_id,
            agent_worktree,
            agent_branch,
            remove_worktree,
            remove_branch,
        )
    })
    .await
    .map(|(fallback_worktree_removed, fallback_branch_deleted)| {
        (
            worktree_removed || fallback_worktree_removed,
            branch_deleted || fallback_branch_deleted,
        )
    })
    .unwrap_or_else(|e| {
        tracing::warn!("Fallback agent cleanup task failed: {}", e);
        (worktree_removed, branch_deleted)
    })
}

pub(crate) async fn capture_failed_agent_diff(
    _ctx: &ChatCtx,
    agent_worktree: &str,
    agent_branch: &str,
    _agent_worktree_name: Option<&str>,
) -> String {
    let worktree_path = Path::new(agent_worktree);
    let mut diff_report = String::new();

    if worktree_path.exists() {
        let uncommitted_diff = run_git_stdout(worktree_path, &["diff", "HEAD"]).unwrap_or_default();

        let committed_diff =
            run_git_stdout(worktree_path, &["log", "--patch", "--reverse", "HEAD@{upstream}..HEAD"])
                .unwrap_or_default();

        let committed_diff = if committed_diff.is_empty() {
            run_git_stdout(
                worktree_path,
                &["diff", &format!("{}~1..HEAD", agent_branch)],
            )
            .unwrap_or_default()
        } else {
            committed_diff
        };

        if !committed_diff.is_empty() || !uncommitted_diff.is_empty() {
            diff_report.push_str("\n\n## Changes made before failure\n\n");
            if !committed_diff.is_empty() {
                diff_report.push_str("### Committed changes\n```diff\n");
                let truncated: String = committed_diff.chars().take(2000).collect();
                diff_report.push_str(&truncated);
                if committed_diff.len() > 2000 {
                    diff_report.push_str("\n... (truncated)");
                }
                diff_report.push_str("\n```\n");
            }
            if !uncommitted_diff.is_empty() {
                diff_report.push_str("### Uncommitted changes\n```diff\n");
                let truncated: String = uncommitted_diff.chars().take(2000).collect();
                diff_report.push_str(&truncated);
                if uncommitted_diff.len() > 2000 {
                    diff_report.push_str("\n... (truncated)");
                }
                diff_report.push_str("\n```\n");
            }
        }
    }

    diff_report
}

pub async fn update_card_heartbeat(
    ctx: ChatCtx,
    task_id: &str,
    card_id: &str,
) -> Result<(), String> {
    let card_id_owned = card_id.to_string();
    let heartbeat = Utc::now().to_rfc3339();
    storage::update_board_atomic_for_ctx(&ctx, task_id, move |board| {
        let card = board
            .get_card_mut(&card_id_owned)
            .ok_or_else(|| format!("Card {} not found", card_id_owned))?;
        card.last_heartbeat_at = Some(heartbeat.clone());
        Ok(())
    })
    .await
    .map(|_| ())
}

pub(super) use crate::worktrees::git::run_git_stdout;
