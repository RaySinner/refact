//! Bodies behind `FilesRuntime` (see `files_in_workspace::FilesRuntime`).
//!
//! These stay separate from `files_correction`'s trait-routed public names on
//! purpose: the routing fns call `gcx.files_runtime.<name>`, so wiring an
//! implementation to them would recurse (trait impl -> routing fn -> trait).
//! The engine's `EngineFilesRuntime` calls these bodies directly instead.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::info;

use refact_files::fuzzy_search::fuzzy_search;
use refact_files::path_utils::canonical_path;
use refact_files::path_utils::shortify_paths_from_indexed;
use refact_files::vcs_detect::detect_vcs_for_a_file_path;
use refact_worktrees::path_mappings::{
    dedupe_paths, normalize_path_for_unscoped_paths, normalize_path_for_unscoped_root_selection,
    project_dirs_for_unscoped_paths, registered_worktree_path_mappings,
};

use crate::files_correction::{
    check_if_its_inside_a_workspace_or_config_for_ctx, files_cache_rebuild_as_needed,
    get_project_dirs, get_raw_project_dirs, get_unscoped_project_dirs,
};
use crate::global_context::{GlobalContext, GlobalContextCtx};
use crate::{PathServices, WorkspaceServices};

pub async fn paths_from_anywhere_impl(global_context: Arc<GlobalContext>) -> Vec<PathBuf> {
    let (file_paths_from_memory, paths_from_workspace, paths_from_jsonl) = {
        let documents_state = &global_context.documents_state; // somehow keeps lock until out of scope
        let file_paths_from_memory = documents_state
            .memory_document_map
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let paths_from_workspace = documents_state.workspace_files.lock().unwrap().clone();
        let paths_from_jsonl = documents_state.jsonl_files.lock().unwrap().clone();
        (
            file_paths_from_memory,
            paths_from_workspace,
            paths_from_jsonl,
        )
    };

    let worktree_mappings = registered_worktree_path_mappings(global_context.cache_dir.as_path());
    let paths_from_anywhere = file_paths_from_memory.into_iter().chain(
        paths_from_workspace
            .into_iter()
            .chain(paths_from_jsonl.into_iter()),
    );

    dedupe_paths(
        paths_from_anywhere
            .filter_map(|path| normalize_path_for_unscoped_paths(&path, &worktree_mappings)),
    )
}

async fn complete_path_with_project_dir(
    gcx: Arc<GlobalContext>,
    correction_candidate: &String,
    is_dir: bool,
) -> Option<PathBuf> {
    fn path_exists(path: &PathBuf, is_dir: bool) -> bool {
        (is_dir && path.is_dir()) || (!is_dir && path.is_file())
    }
    let candidate_path = canonical_path(correction_candidate);
    let project_dirs = get_unscoped_project_dirs(gcx.clone()).await;
    for p in project_dirs {
        if path_exists(&candidate_path, is_dir) && candidate_path.starts_with(&p) {
            return Some(candidate_path);
        }

        // This might save a roundtrip:
        // .../project1/project1/1.cpp
        // model likes to output only one "project1" of the two needed
        if candidate_path.starts_with(&p) {
            let last_component = p
                .components()
                .last()
                .map(|x| x.as_os_str().to_string_lossy().to_string())
                .unwrap_or("".to_string());
            let last_component_duplicated = p.join(&last_component).join(
                &candidate_path
                    .strip_prefix(&p)
                    .unwrap_or(candidate_path.as_path()),
            );
            if path_exists(&last_component_duplicated, is_dir) {
                info!(
                    "autocorrected by duplicating the project last component: {} -> {}",
                    p.to_string_lossy().to_string(),
                    last_component_duplicated.to_string_lossy().to_string()
                );
                return Some(last_component_duplicated);
            }
        }
    }
    None
}

async fn _correct_to_nearest(
    gcx: Arc<GlobalContext>,
    correction_candidate: &String,
    is_dir: bool,
    fuzzy: bool,
    top_n: usize,
) -> Vec<String> {
    if let Some(fixed) =
        complete_path_with_project_dir(gcx.clone(), correction_candidate, is_dir).await
    {
        return vec![fixed.to_string_lossy().to_string()];
    }

    let cache_correction_arc = files_cache_rebuild_as_needed(gcx.clone()).await;
    // it's dangerous to use cache_correction_arc without a mutex, but should be fine as long as it's read-only
    // (another thread never writes to the map itself, it can only replace the arc with a different map)

    // NOTE: do we need top_n here?
    let correction_cache = if is_dir {
        &cache_correction_arc.directories
    } else {
        &cache_correction_arc.filenames
    };
    let matches = correction_cache.find_matches(&PathBuf::from(correction_candidate));
    if matches.is_empty() {
        info!(
            "not found {:?} in cache_correction, is_dir={}",
            correction_candidate, is_dir
        );
    } else {
        return matches
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<String>>();
    }

    if fuzzy {
        info!(
            "fuzzy search {:?} is_dir={}, cache_fuzzy_arc.len={}",
            correction_candidate,
            is_dir,
            correction_cache.len()
        );
        return fuzzy_search(
            correction_candidate,
            correction_cache.short_paths_iter(),
            top_n,
            &['/', '\\'],
        );
    }

    vec![]
}

pub async fn correct_to_nearest_filename_impl(
    gcx: Arc<GlobalContext>,
    correction_candidate: &String,
    fuzzy: bool,
    top_n: usize,
) -> Vec<String> {
    _correct_to_nearest(gcx, correction_candidate, false, fuzzy, top_n).await
}

pub async fn correct_to_nearest_dir_path_impl(
    gcx: Arc<GlobalContext>,
    correction_candidate: &String,
    fuzzy: bool,
    top_n: usize,
) -> Vec<String> {
    _correct_to_nearest(gcx, correction_candidate, true, fuzzy, top_n).await
}

pub async fn get_project_dirs_impl(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    project_dirs_for_unscoped_paths(
        gcx.cache_dir.as_path(),
        get_raw_project_dirs(gcx.clone()).await.as_slice(),
    )
}

pub async fn get_unscoped_project_dirs_impl(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    get_project_dirs(gcx).await
}

pub async fn get_active_project_path_impl(gcx: Arc<GlobalContext>) -> Option<PathBuf> {
    let cache_dir = gcx.cache_dir.clone();
    let active_file_path = gcx.documents_state.active_file_path.clone();
    let workspace_folders = get_unscoped_project_dirs(gcx).await;
    active_project_path(cache_dir.as_path(), &active_file_path, workspace_folders).await
}

pub async fn get_active_project_path_for_services_impl(
    paths: &PathServices,
    workspace: &WorkspaceServices,
) -> Option<PathBuf> {
    let workspace_folders = project_dirs_for_unscoped_paths(
        paths.cache_dir.as_path(),
        workspace
            .documents_state
            .workspace_folders
            .lock()
            .unwrap()
            .as_slice(),
    );
    active_project_path(
        paths.cache_dir.as_path(),
        &workspace.documents_state.active_file_path,
        workspace_folders,
    )
    .await
}

async fn active_project_path(
    cache_dir: &Path,
    active_file_path: &Arc<tokio::sync::Mutex<Option<PathBuf>>>,
    workspace_folders: Vec<PathBuf>,
) -> Option<PathBuf> {
    if workspace_folders.is_empty() {
        return None;
    }

    let worktree_mappings = registered_worktree_path_mappings(cache_dir);
    let active_file = active_file_path
        .lock()
        .await
        .clone()
        .and_then(|path| normalize_path_for_unscoped_root_selection(&path, &worktree_mappings));

    let active_file_path = if let Some(active_file) = active_file {
        active_file
    } else {
        return Some(workspace_folders[0].clone());
    };

    if !workspace_folders
        .iter()
        .any(|folder| active_file_path.starts_with(folder))
    {
        return Some(workspace_folders[0].clone());
    }

    if let Some((path, _)) = detect_vcs_for_a_file_path(&active_file_path).await {
        if workspace_folders
            .iter()
            .any(|folder| path.starts_with(folder))
        {
            return Some(path);
        }
    }

    // Without VCS, return one of workspace_folders that is a parent for active_file_path
    for f in workspace_folders {
        if active_file_path.starts_with(&f) {
            return Some(f);
        }
    }

    tracing::info!("no project is active");
    None
}

pub async fn shortify_paths_impl(gcx: Arc<GlobalContext>, paths: &Vec<String>) -> Vec<String> {
    let cache_correction_arc = files_cache_rebuild_as_needed(gcx.clone()).await;
    shortify_paths_from_indexed(&cache_correction_arc, paths)
}

pub async fn check_if_its_inside_a_workspace_or_config_impl(
    gcx: Arc<GlobalContext>,
    path: &Path,
) -> Result<(), String> {
    check_if_its_inside_a_workspace_or_config_for_ctx(&gcx.chat_ctx(gcx.clone()), path).await
}
