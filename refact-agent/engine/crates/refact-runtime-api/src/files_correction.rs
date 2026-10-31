use std::path::{Path, PathBuf};

use refact_files::path_utils::canonicalize_normalized_path;
use refact_worktrees::path_mappings::project_dirs_for_unscoped_paths;

use crate::ChatCtx;

pub async fn get_unscoped_project_dirs_for_ctx(ctx: &ChatCtx) -> Vec<PathBuf> {
    let workspace_folders = ctx.workspace.workspace_folders.lock().unwrap().clone();
    project_dirs_for_unscoped_paths(ctx.paths.cache_dir.as_path(), workspace_folders.as_slice())
}

pub async fn check_if_its_inside_a_workspace_or_config_for_ctx(
    ctx: &ChatCtx,
    path: &Path,
) -> Result<(), String> {
    path_inside_workspace_or_config(
        get_unscoped_project_dirs_for_ctx(ctx).await,
        ctx.paths.config_dir.as_path(),
        path,
    )
}

fn path_inside_workspace_or_config(
    workspace_folders: Vec<PathBuf>,
    config_dir: &Path,
    path: &Path,
) -> Result<(), String> {
    let path = canonicalize_normalized_path(path.to_path_buf());
    let workspace_folders_normalized = workspace_folders
        .iter()
        .map(|path| canonicalize_normalized_path(path.clone()))
        .collect::<Vec<_>>();
    let config_dir = canonicalize_normalized_path(config_dir.to_path_buf());

    if workspace_folders_normalized
        .iter()
        .any(|dir| path.starts_with(dir))
        || path.starts_with(&config_dir)
    {
        Ok(())
    } else {
        Err(format!(
            "Path '{path:?}' is outside of project directories:\n{workspace_folders_normalized:?}"
        ))
    }
}
