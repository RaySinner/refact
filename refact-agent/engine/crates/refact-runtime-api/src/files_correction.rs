use std::path::{Path, PathBuf};
use std::sync::Arc;

use refact_codegraph::QueuedPath;
use crate::global_context::GlobalContext;
use refact_worktrees::path_mappings::{
    map_registered_worktree_path, normalize_path_for_unscoped_root_selection,
    registered_worktree_path_mappings, RegisteredWorktreePathMapping,
};

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

pub async fn get_raw_project_dirs(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    let workspace_folders = gcx.documents_state.workspace_folders.clone();
    let workspace_folders_locked = workspace_folders.lock().unwrap();
    workspace_folders_locked.iter().cloned().collect::<Vec<_>>()
}

pub async fn files_cache_rebuild_as_needed(
    gcx: Arc<GlobalContext>,
) -> Arc<refact_files::correction_cache::CacheCorrection> {
    let cache_dirty_arc = gcx.documents_state.cache_dirty.clone();
    let mut cache_correction_arc = gcx
        .documents_state
        .cache_correction
        .lock()
        .unwrap()
        .clone();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let mut cache_dirty_ref = cache_dirty_arc.lock().await;
    if *cache_dirty_ref > 0.0 && now > *cache_dirty_ref {
        tracing::info!("rebuilding files cache...");
        let start_time = std::time::Instant::now();
        let paths = paths_from_anywhere(gcx.clone()).await;
        let workspace_folders = get_unscoped_project_dirs(gcx.clone()).await;
        let cache_correction =
            refact_files::correction_cache::CacheCorrection::build(&paths, &workspace_folders);

        tracing::info!(
            "rebuild completed in {:.3}s, over {}",
            start_time.elapsed().as_secs_f64(),
            paths.len()
        );
        cache_correction_arc = Arc::new(cache_correction);
        {
            *gcx.documents_state.cache_correction.lock().unwrap() = cache_correction_arc.clone();
        }
        *cache_dirty_ref = 0.0;
    }

    cache_correction_arc
}

pub async fn get_active_workspace_folder(gcx: Arc<GlobalContext>) -> Option<PathBuf> {
    let workspace_folders = get_unscoped_project_dirs(gcx.clone()).await;

    let worktree_mappings = registered_worktree_path_mappings(gcx.cache_dir.as_path());
    let active_file = gcx
        .documents_state
        .active_file_path
        .lock()
        .await
        .clone()
        .and_then(|path| normalize_path_for_unscoped_root_selection(&path, &worktree_mappings));
    if let Some(active_file) = active_file {
        for f in &workspace_folders {
            if active_file.starts_with(f) {
                tracing::info!("found that {:?} is the workspace folder", f);
                return Some(f.clone());
            }
        }
    }

    if let Some(first_workspace_folder) = workspace_folders.first() {
        tracing::info!(
            "found that {:?} is the workspace folder",
            first_workspace_folder
        );
        Some(first_workspace_folder.clone())
    } else {
        None
    }
}

pub async fn check_if_its_inside_a_workspace_worktree_or_config(
    gcx: Arc<GlobalContext>,
    path: &Path,
) -> Result<(), String> {
    let outside_error = match check_if_its_inside_a_workspace_or_config(gcx.clone(), path).await {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let path = canonicalize_normalized_path(path.to_path_buf());
    let worktrees_base = refact_worktrees::service::worktrees_base_dir(&gcx.cache_dir)
        .unwrap_or_else(|_| gcx.cache_dir.join("worktrees"));
    let worktrees_root = canonicalize_normalized_path(worktrees_base);
    if refact_worktrees::path_mappings::registered_worktree_roots(gcx.cache_dir.as_path())
        .into_iter()
        .map(canonicalize_normalized_path)
        .any(|root| root.starts_with(&worktrees_root) && path.starts_with(&root))
    {
        return Ok(());
    }
    Err(outside_error)
}
