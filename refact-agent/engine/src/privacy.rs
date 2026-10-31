use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde_yaml::{Mapping, Value};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::files_correction::{canonical_path, get_project_dirs};
use crate::global_context::GlobalContext;

pub mod destinations;
pub mod heuristic;
pub mod records;

pub use refact_core::privacy_types::{FilePrivacyLevel, FilePrivacySettings, PrivacySettings};
pub use refact_privacy::runtime_state::{
    observation_runtime_state, record_observation_status, PrivacyMatchCounts,
    PrivacyObservationRuntimeState,
};
pub use refact_privacy::{check_file_privacy, PolicyLoad, PrivacyPolicy};
pub(crate) use refact_privacy::check_file_privacy_for_read_preparation;
use refact_privacy::legacy::legacy_settings;
use refact_runtime_api::privacy::project_privacy_paths;
pub use refact_runtime_api::privacy::{
    load_privacy_if_needed, load_privacy_if_needed_for_policy,
};

pub async fn warn_observation_degraded_once(gcx: Arc<GlobalContext>, reason: &str) {
    crate::buddy::observers::privacy_degraded::warn_once(gcx, reason).await;
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
