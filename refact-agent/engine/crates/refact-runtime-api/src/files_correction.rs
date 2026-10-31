use std::path::{Path, PathBuf};
use std::sync::Arc;

use refact_codegraph::QueuedPath;
use crate::global_context::GlobalContext;
use refact_worktrees::path_mappings::{map_registered_worktree_path, RegisteredWorktreePathMapping};

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

pub fn resolve_codegraph_queue_path(
    path: &Path,
    mappings: &[RegisteredWorktreePathMapping],
) -> QueuedPath {
    let read_path = canonicalize_normalized_path(path.to_path_buf());
    let store_path =
        map_registered_worktree_path(&read_path, mappings).unwrap_or_else(|| read_path.clone());
    QueuedPath::new(
        store_path.to_string_lossy().to_string(),
        read_path.to_string_lossy().to_string(),
    )
}

pub async fn paths_from_anywhere(global_context: Arc<GlobalContext>) -> Vec<PathBuf> {
    global_context
        .files_runtime
        .clone()
        .paths_from_anywhere(global_context)
        .await
}

pub async fn correct_to_nearest_filename(
    gcx: Arc<GlobalContext>,
    correction_candidate: &String,
    fuzzy: bool,
    top_n: usize,
) -> Vec<String> {
    gcx.files_runtime
        .clone()
        .correct_to_nearest_filename(gcx, correction_candidate, fuzzy, top_n)
        .await
}

pub async fn correct_to_nearest_dir_path(
    gcx: Arc<GlobalContext>,
    correction_candidate: &String,
    fuzzy: bool,
    top_n: usize,
) -> Vec<String> {
    gcx.files_runtime
        .clone()
        .correct_to_nearest_dir_path(gcx, correction_candidate, fuzzy, top_n)
        .await
}

pub async fn get_project_dirs(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    gcx.files_runtime.clone().get_project_dirs(gcx).await
}

pub async fn get_unscoped_project_dirs(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    gcx.files_runtime
        .clone()
        .get_unscoped_project_dirs(gcx)
        .await
}

pub async fn get_active_project_path(gcx: Arc<GlobalContext>) -> Option<PathBuf> {
    gcx.files_runtime.clone().get_active_project_path(gcx).await
}

pub async fn get_active_project_path_for_services(
    paths: &crate::PathServices,
    workspace: &crate::WorkspaceServices,
) -> Option<PathBuf> {
    workspace
        .files_runtime
        .get_active_project_path_for_services(paths, workspace)
        .await
}

pub async fn shortify_paths(gcx: Arc<GlobalContext>, paths: &Vec<String>) -> Vec<String> {
    gcx.files_runtime.clone().shortify_paths(gcx, paths).await
}

pub async fn check_if_its_inside_a_workspace_or_config(
    gcx: Arc<GlobalContext>,
    path: &Path,
) -> Result<(), String> {
    gcx.files_runtime
        .clone()
        .check_if_its_inside_a_workspace_or_config(gcx, path)
        .await
}
