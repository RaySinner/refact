use std::path::PathBuf;
use std::sync::Arc;

use refact_files::path_utils::canonicalize_normalized_path;

use crate::global_context::GlobalContext;

pub async fn get_config_dirs(
    gcx: Arc<GlobalContext>,
    current_project_path: &Option<PathBuf>,
) -> (Vec<PathBuf>, PathBuf) {
    let (global_config_dir, workspace_folders_arc, workspace_vcs_roots_arc, _integrations_yaml) = {
        (
            gcx.config_dir.clone(),
            gcx.documents_state.workspace_folders.clone(),
            gcx.documents_state.workspace_vcs_roots.clone(),
            gcx.cmdline.integrations_yaml.clone(),
        )
    };

    let raw_workspace_folders = workspace_folders_arc.lock().unwrap().clone();
    let mut workspace_folders = raw_workspace_folders
        .into_iter()
        .map(canonicalize_normalized_path)
        .collect::<Vec<_>>();
    if let Some(current_project_path) = current_project_path {
        workspace_folders = workspace_folders
            .into_iter()
            .filter(|folder| current_project_path.starts_with(&folder))
            .collect::<Vec<_>>();
    }
    let raw_workspace_vcs_roots = workspace_vcs_roots_arc.lock().unwrap().clone();
    let workspace_vcs_roots = raw_workspace_vcs_roots
        .into_iter()
        .map(canonicalize_normalized_path)
        .collect::<Vec<_>>();

    let mut config_dirs = Vec::new();

    for folder in workspace_folders {
        let vcs_roots: Vec<PathBuf> = workspace_vcs_roots
            .iter()
            .filter(|root| root.starts_with(&folder))
            .cloned()
            .collect();

        if !vcs_roots.is_empty() {
            // it has any workspace_vcs_roots => take them as projects
            for root in vcs_roots {
                config_dirs.push(root.join(".refact"));
            }
        } else {
            // it doesn't => use workspace_folder itself
            config_dirs.push(folder.join(".refact"));
        }
    }

    config_dirs.sort();
    (config_dirs, global_config_dir)
}
