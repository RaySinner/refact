use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, SystemTime};

use refact_core::privacy_types::PrivacySettings;
use refact_files::path_utils::canonical_path;
use refact_privacy::legacy::legacy_settings;
use refact_privacy::PolicyLoad;
use tracing::error;

use crate::global_context::GlobalContext;
use crate::ToolCatalogGenerations;

const PRIVACY_TOO_OLD: Duration = Duration::from_secs(3);

pub fn project_privacy_paths(project_dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    project_dirs
        .into_iter()
        .map(|project_dir| project_dir.join(".refact").join("privacy.yaml"))
        .collect()
}

pub async fn load_privacy_if_needed(gcx: Arc<GlobalContext>) -> Arc<PrivacySettings> {
    let workspace_folders = gcx
        .documents_state
        .workspace_folders
        .lock()
        .unwrap()
        .clone();
    let project_dirs = refact_worktrees::path_mappings::project_dirs_for_unscoped_paths(
        gcx.cache_dir.as_path(),
        workspace_folders.as_slice(),
    );
    load_privacy_if_needed_for_policy(
        &gcx.privacy_policy_load,
        &gcx.privacy_settings,
        gcx.config_dir.as_path(),
        gcx.cmdline.privacy_yaml.as_str(),
        project_dirs,
        &gcx.tool_catalog_generations,
    )
    .await
}

pub async fn load_privacy_if_needed_for_policy(
    privacy_policy_load: &Arc<StdRwLock<PolicyLoad>>,
    privacy_settings: &Arc<StdRwLock<Arc<PrivacySettings>>>,
    config_dir: &Path,
    privacy_yaml: &str,
    project_dirs: Vec<PathBuf>,
    tool_catalog_generations: &ToolCatalogGenerations,
) -> Arc<PrivacySettings> {
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (previous_settings, previous_load) = {
        let previous_settings = privacy_settings.read().unwrap().clone();
        let should_reload = previous_settings.loaded_ts + PRIVACY_TOO_OLD.as_secs() <= current_time;
        if !should_reload {
            return previous_settings;
        }
        (
            previous_settings,
            privacy_policy_load.read().unwrap().clone(),
        )
    };

    let global_path = if privacy_yaml.is_empty() {
        config_dir.join("privacy.yaml")
    } else {
        canonical_path(privacy_yaml.to_string())
    };
    let project_paths = project_privacy_paths(project_dirs);
    let loaded =
        refact_privacy::load_policy(&global_path, &project_paths, Some(&previous_load)).await;

    if let Some(error_message) = &loaded.error {
        error!("{error_message}");
        *privacy_policy_load.write().unwrap() = loaded;
        return previous_settings;
    }

    let policy_changed = previous_load.policy != loaded.policy;
    let new_privacy_settings = Arc::new(legacy_settings(&loaded.policy, current_time));
    *privacy_policy_load.write().unwrap() = loaded;
    *privacy_settings.write().unwrap() = new_privacy_settings.clone();
    if policy_changed {
        tool_catalog_generations.advance_privacy();
    }
    new_privacy_settings
}
