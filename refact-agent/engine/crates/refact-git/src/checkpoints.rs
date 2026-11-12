use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use git2::{Branch, Oid, Repository};

use refact_core::chunk_utils::official_text_hashing_function;
use refact_core::custom_error::MapErrToString;

use crate::operations::{commit, get_diff_statuses, open_or_init_repo, stage_changes};
use crate::{from_unix_glob_pattern_to_gitignore, FileChange, FileChangeStatus};

pub fn open_shadow_repo_for_path(
    git_dir_path: &Path,
    workdir: &Path,
    allow_init: bool,
    blocklist: &[String],
    additional_indexing_dirs: &[String],
) -> Result<Repository, String> {
    let repo = if allow_init {
        open_or_init_repo(git_dir_path).map_err_to_string()
    } else {
        Repository::open(git_dir_path).map_err_to_string()
    }?;
    let filetime_now = filetime::FileTime::now();
    filetime::set_file_times(git_dir_path, filetime_now, filetime_now).map_err_to_string()?;
    repo.set_workdir(workdir, false).map_err_to_string()?;
    for git_rule in [".git", ".git/**"] {
        if let Err(e) = repo.add_ignore_rule(git_rule) {
            tracing::warn!(
                "Failed to add ignore rule for {}: {}",
                workdir.to_string_lossy(),
                e
            );
        }
    }
    for blocklisted_rule in blocklist {
        if let Err(e) = repo.add_ignore_rule(&from_unix_glob_pattern_to_gitignore(blocklisted_rule)) {
            tracing::warn!(
                "Failed to add ignore rule for {}: {}",
                workdir.to_string_lossy(),
                e
            );
        }
    }
    for additional_indexing_rule in additional_indexing_dirs {
        if let Err(e) = repo.add_ignore_rule(&format!(
            "!{}",
            from_unix_glob_pattern_to_gitignore(additional_indexing_rule)
        )) {
            tracing::warn!(
                "Failed to add ignore rule for {}: {}",
                workdir.to_string_lossy(),
                e
            );
        }
    }
    Ok(repo)
}

pub fn nested_vcs_roots_inside(
    vcs_roots: &[PathBuf],
    workspace_folder: &Path,
) -> Vec<PathBuf> {
    let workspace_key = normalize_shadow_path(workspace_folder);
    vcs_roots
        .iter()
        .map(|vcs| normalize_shadow_path(vcs))
        .filter(|vcs| vcs.starts_with(&workspace_key) && *vcs != workspace_key)
        .collect()
}

pub fn open_shadow_repo_and_nested_repos(
    cache_dir: &Path,
    workspace_folder: &Path,
    nested_vcs_roots: &[PathBuf],
    indexing_blocklists: &[(PathBuf, Vec<String>, Vec<String>)],
    allow_init_main_repo: bool,
) -> Result<(Repository, Vec<Repository>, String), String> {
    let open_repos = |paths: &[PathBuf], allow_init: bool, nested: bool| {
        let mut result = Vec::new();
        for path in paths {
            let path = normalize_shadow_path(path);
            let (blocklist, additional_indexing_dirs) = indexing_blocklists
                .iter()
                .find(|(candidate, _, _)| candidate == &path)
                .map(|(_, blocklist, additional)| (blocklist.clone(), additional.clone()))
                .unwrap_or_default();
            let path_hash = shadow_repo_hash(&path);
            let git_dir_path = if nested {
                cache_dir.join("shadow_git").join("nested").join(&path_hash)
            } else {
                cache_dir.join("shadow_git").join(&path_hash)
            };
            result.push(open_shadow_repo_for_path(
                &git_dir_path,
                &path,
                allow_init,
                &blocklist,
                &additional_indexing_dirs,
            )?);
        }
        Ok::<Vec<Repository>, String>(result)
    };

    let workspace_folder_hash = workspace_folder_hash(workspace_folder);
    let repo = open_repos(&[workspace_folder.to_path_buf()], allow_init_main_repo, false)?
        .into_iter()
        .next()
        .unwrap();
    let nested_repos = open_repos(nested_vcs_roots, true, true)?;

    Ok((repo, nested_repos, workspace_folder_hash))
}

pub fn get_file_changes_from_nested_repos<'a>(
    parent_repo: &'a Repository,
    nested_repos: &'a [Repository],
    include_abs_paths: bool,
) -> Result<(Vec<(&'a Repository, Vec<FileChange>)>, Vec<FileChange>), String> {
    let repo_workdir = parent_repo
        .workdir()
        .ok_or("Failed to get workdir.".to_string())?;
    let mut file_changes_per_repo = Vec::new();
    let mut file_changes_flattened = Vec::new();

    for nested_repo in nested_repos {
        let (_, nested_repo_changes) =
            get_diff_statuses(git2::StatusShow::Workdir, nested_repo, include_abs_paths)?;
        let nested_repo_workdir = nested_repo
            .workdir()
            .ok_or("Failed to get nested repo workdir".to_string())?;
        let nested_repo_rel_path = nested_repo_workdir
            .strip_prefix(repo_workdir)
            .map_err_to_string()?;

        for change in &nested_repo_changes {
            file_changes_flattened.push(FileChange {
                relative_path: nested_repo_rel_path.join(&change.relative_path),
                absolute_path: change.absolute_path.clone(),
                status: change.status.clone(),
            });
        }
        file_changes_per_repo.push((nested_repo, nested_repo_changes));
    }

    Ok((file_changes_per_repo, file_changes_flattened))
}

pub fn resolve_checkpoint_workspace_folder(workspace_folder: &Path) -> Result<PathBuf, String> {
    let resolved = std::fs::canonicalize(workspace_folder).map_err(|e| {
        format!(
            "Checkpoint workspace root '{}' does not exist or cannot be resolved: {}",
            workspace_folder.display(),
            e
        )
    })?;
    if !resolved.is_dir() {
        return Err(format!(
            "Checkpoint workspace root '{}' is not a directory",
            resolved.display()
        ));
    }
    Ok(dunce::simplified(&resolved).to_path_buf())
}

pub fn workspace_folder_hash(workspace_folder: &Path) -> String {
    let normalized = normalize_shadow_path(workspace_folder);
    official_text_hashing_function(&normalized.to_string_lossy().to_string())
}

pub fn shadow_repo_hash(workspace_folder: &Path) -> String {
    let hash_path = std::fs::canonicalize(workspace_folder)
        .map(|path| dunce::simplified(&path).to_path_buf())
        .unwrap_or_else(|_| normalize_shadow_path(workspace_folder));
    official_text_hashing_function(&hash_path.to_string_lossy().to_string())
}

pub fn normalize_shadow_path(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(canonical) => dunce::simplified(&canonical).to_path_buf(),
        Err(_) => dunce::simplified(path).to_path_buf(),
    }
}

pub fn repo_has_commits(repo: &Repository) -> bool {
    repo.head()
        .map(|head| head.target().is_some())
        .unwrap_or(false)
}

pub fn create_initial_shadow_commit(
    repo: &Repository,
    nested_repos: &[Repository],
    abort_flag: &Arc<AtomicBool>,
) -> Result<(Oid, usize), String> {
    let (_, mut file_changes) = get_diff_statuses(git2::StatusShow::Workdir, repo, false)?;
    let (nested_file_changes, all_nested_changes) =
        get_file_changes_from_nested_repos(repo, nested_repos, false)?;
    file_changes.extend(all_nested_changes);

    let mut skipped = stage_changes(repo, &file_changes, abort_flag)?;

    let mut index = repo.index().map_err_to_string()?;
    let tree_id = index.write_tree().map_err_to_string()?;
    let tree = repo.find_tree(tree_id).map_err_to_string()?;
    let signature = git2::Signature::now("Refact Agent", "agent@refact.ai").map_err_to_string()?;
    let commit = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Initial commit",
            &tree,
            &[],
        )
        .map_err_to_string()?;

    for (nested_repo, changes) in nested_file_changes {
        skipped += stage_changes(nested_repo, &changes, abort_flag)?;
    }

    Ok((commit, skipped))
}

pub fn stage_and_commit_workdir_changes(
    repo: &Repository,
    nested_repos: &[Repository],
    branch: &Branch<'_>,
    commit_message: &str,
    abort_flag: &Arc<AtomicBool>,
) -> Result<(Oid, usize), String> {
    let (_, mut file_changes) = get_diff_statuses(git2::StatusShow::Workdir, repo, false)?;
    let (nested_file_changes, flattened_nested_file_changes) =
        get_file_changes_from_nested_repos(repo, nested_repos, false)?;
    file_changes.extend(flattened_nested_file_changes);

    let mut skipped = stage_changes(repo, &file_changes, abort_flag)?;
    let commit_oid = commit(
        repo,
        branch,
        commit_message,
        "Refact Agent",
        "agent@refact.ai",
    )?;

    for (nested_repo, changes) in nested_file_changes {
        skipped += stage_changes(nested_repo, &changes, abort_flag)?;
    }

    Ok((commit_oid, skipped))
}

pub fn reverse_file_change_status(status: &FileChangeStatus) -> FileChangeStatus {
    match status {
        FileChangeStatus::ADDED => FileChangeStatus::DELETED,
        FileChangeStatus::DELETED => FileChangeStatus::ADDED,
        FileChangeStatus::MODIFIED => FileChangeStatus::MODIFIED,
    }
}
