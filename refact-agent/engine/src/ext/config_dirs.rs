use crate::app_state::AppState;
use crate::files_correction::get_project_dirs;

pub use refact_ext::config_dirs::{
    collect_md_files_recursive, get_ext_dirs as get_ext_dirs_for_dirs, is_claude_dir,
    source_for_dir, CommandSource, ExtDirs,
};

pub async fn get_ext_dirs(app: AppState) -> ExtDirs {
    let config_dir = app.paths.config_dir.clone();
    let workspace_dirs = get_project_dirs(app.gcx.clone()).await;
    get_ext_dirs_for_dirs(&config_dir, &workspace_dirs).await
}
