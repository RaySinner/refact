use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, SystemTime};

use refact_core::privacy_types::PrivacySettings;
use refact_files::path_utils::canonical_path;
use refact_privacy::legacy::legacy_settings;
use refact_privacy::PolicyLoad;
use refact_privacy::PrivacyPolicy;
use serde_yaml::{Mapping, Value};
use tokio::io::AsyncWriteExt;
use tracing::error;
use uuid::Uuid;

use crate::files_correction::get_project_dirs;
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

fn global_privacy_path(config_dir: &Path, privacy_yaml: &str) -> PathBuf {
    if privacy_yaml.is_empty() {
        config_dir.join("privacy.yaml")
    } else {
        canonical_path(privacy_yaml.to_string())
    }
}

pub async fn global_only_policy(gcx: Arc<GlobalContext>) -> Option<refact_privacy::PrivacyPolicy> {
    let path = global_privacy_path(gcx.config_dir.as_path(), gcx.cmdline.privacy_yaml.as_str());
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    refact_privacy::parse_policy_yaml(&content).ok()
}

pub async fn save_privacy_policy(
    gcx: Arc<GlobalContext>,
    policy: PrivacyPolicy,
) -> Result<(), String> {
    policy
        .compile()
        .map_err(|error| format!("invalid privacy policy: {error}"))?;
    let path = global_privacy_path(gcx.config_dir.as_path(), gcx.cmdline.privacy_yaml.as_str());
    let mut document = match tokio::fs::read_to_string(&path).await {
        Ok(content) => match serde_yaml::from_str::<Value>(&content) {
            Ok(Value::Mapping(document)) => document,
            Ok(_) | Err(_) => {
                backup_corrupt_privacy_file(&path, &content).await?;
                Mapping::new()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Mapping::new(),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    document.insert(
        Value::String("privacy_rules".to_string()),
        serde_yaml::to_value(&policy)
            .map_err(|error| format!("failed to serialize privacy policy: {error}"))?,
    );
    let content = serde_yaml::to_string(&Value::Mapping(document))
        .map_err(|error| format!("failed to serialize {}: {error}", path.display()))?;
    write_privacy_file(&path, &content).await?;

    let previous_load = gcx.privacy_policy_load.read().unwrap().clone();
    let project_paths = project_privacy_paths(get_project_dirs(gcx.clone()).await);
    let loaded = refact_privacy::load_policy(&path, &project_paths, Some(&previous_load)).await;
    if loaded.error.is_some() {
        *gcx.privacy_policy_load.write().unwrap() = loaded;
        return Ok(());
    }
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))?
        .as_secs();
    let settings = Arc::new(legacy_settings(&loaded.policy, current_time));
    *gcx.privacy_policy_load.write().unwrap() = loaded;
    *gcx.privacy_settings.write().unwrap() = settings;
    gcx.tool_catalog_generations.advance_privacy();
    Ok(())
}

async fn backup_corrupt_privacy_file(path: &Path, content: &str) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("privacy.yaml");
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))?
        .as_millis();
    let backup = parent.join(format!(
        "{file_name}.corrupt-{timestamp}-{}",
        Uuid::new_v4()
    ));
    write_privacy_file(&backup, content).await?;
    Ok(backup)
}

async fn write_privacy_file(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("privacy.yaml");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|error| format!("failed to create {}: {error}", temporary.display()))?;
        file.write_all(content.as_bytes())
            .await
            .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
        file.sync_all()
            .await
            .map_err(|error| format!("failed to sync {}: {error}", temporary.display()))?;
        drop(file);
        #[cfg(target_os = "windows")]
        if path.exists() {
            tokio::fs::remove_file(path)
                .await
                .map_err(|error| format!("failed to replace {}: {error}", path.display()))?;
        }
        tokio::fs::rename(&temporary, path)
            .await
            .map_err(|error| format!("failed to replace {}: {error}", path.display()))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}
