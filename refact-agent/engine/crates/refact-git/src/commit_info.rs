use std::path::{Path, PathBuf};

use url::Url;

use crate::operations::{get_diff_statuses, git_diff_head_to_workdir_as_string};
use crate::CommitInfo;

const MAX_DIFF_SIZE: usize = 4096;

pub fn commit_information_from_vcs_roots(vcs_roots: &[PathBuf]) -> Vec<CommitInfo> {
    let mut commits = Vec::new();

    for project_path in vcs_roots {
        let repository = match git2::Repository::open(project_path) {
            Ok(repo) => repo,
            Err(e) => {
                tracing::warn!("{}", e);
                continue;
            }
        };

        let (staged_changes, unstaged_changes) =
            match get_diff_statuses(git2::StatusShow::IndexAndWorkdir, &repository, true) {
                Ok((staged, unstaged)) if staged.is_empty() && unstaged.is_empty() => {
                    continue;
                }
                Ok(changes) => changes,
                Err(e) => {
                    tracing::warn!("{}", e);
                    continue;
                }
            };

        commits.push(CommitInfo {
            project_path: Url::from_file_path(project_path)
                .ok()
                .unwrap_or_else(|| Url::parse("file:///").unwrap()),
            commit_message: "".to_string(),
            staged_changes,
            unstaged_changes,
        });
    }

    commits
}

pub fn workdir_diff_for_project_root(project_path: &Path) -> Result<Option<String>, String> {
    let repository = git2::Repository::open(project_path).map_err(|e| e.to_string())?;
    match git_diff_head_to_workdir_as_string(&repository, MAX_DIFF_SIZE) {
        Ok(diff) if diff.is_empty() => Ok(None),
        Ok(diff) => Ok(Some(diff)),
        Err(e) => Err(e),
    }
}
