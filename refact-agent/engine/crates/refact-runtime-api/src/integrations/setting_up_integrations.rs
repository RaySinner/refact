use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::fs as async_fs;

use refact_core::custom_error::YamlError;

use crate::global_context::GlobalContext;

pub use crate::integrations_runtime::{
    get_config_dirs, join_config_path, split_path_into_project_and_integration, IntegrationRecord,
};

#[derive(Serialize, Default)]
pub struct IntegrationResult {
    pub integrations: Vec<IntegrationRecord>,
    pub error_log: Vec<YamlError>,
}

pub async fn get_vars_for_replacements(
    gcx: Arc<GlobalContext>,
    error_log: &mut Vec<YamlError>,
) -> HashMap<String, String> {
    crate::integration_variables::get_vars_for_replacements(gcx, error_log).await
}

pub async fn integrations_all(
    gcx: Arc<GlobalContext>,
    include_non_existent_records: bool,
) -> IntegrationResult {
    let provider = gcx.context_factory.get().integration_provider();
    let (config_dirs, global_config_dir) = get_config_dirs(gcx.clone(), &None).await;
    let (allow_experimental, integrations_yaml_path) = {
        (
            gcx.cmdline.experimental,
            gcx.cmdline.integrations_yaml.clone(),
        )
    };
    let names = provider.integration_names(allow_experimental);
    let lst: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut error_log: Vec<YamlError> = Vec::new();
    let vars_for_replacements = get_vars_for_replacements(gcx.clone(), &mut error_log).await;
    let integrations = crate::integrations_runtime::read_integrations_d(
        &config_dirs,
        &global_config_dir,
        &integrations_yaml_path,
        &vars_for_replacements,
        &lst,
        &mut error_log,
        &["**/*".to_string()],
        include_non_existent_records,
        provider.as_ref(),
    );
    IntegrationResult {
        integrations,
        error_log,
    }
}

#[derive(Serialize, Default)]
pub struct IntegrationGetResult {
    pub project_path: String,
    pub integr_name: String,
    pub integr_config_path: String,
    pub integr_config_exists: bool,
    pub integr_schema: serde_json::Value,
    pub integr_values: serde_json::Value,
    pub error_log: Vec<YamlError>,
}

pub async fn integration_config_get(
    gcx: Arc<GlobalContext>,
    integr_config_path: String,
) -> Result<IntegrationGetResult, String> {
    let sanitized_path = refact_files::path_utils::canonical_path(&integr_config_path);
    let exists = sanitized_path.exists();

    let (integr_name, project_path) = split_path_into_project_and_integration(&sanitized_path)?;
    let better_integr_config_path = sanitized_path.to_string_lossy().to_string();
    let mut result = IntegrationGetResult {
        project_path: project_path.clone(),
        integr_name: integr_name.clone(),
        integr_config_path: better_integr_config_path.clone(),
        integr_config_exists: exists,
        integr_schema: serde_json::Value::Null,
        integr_values: serde_json::Value::Null,
        error_log: Vec::new(),
    };

    let mut integration_box = create_integration(&gcx, integr_name.as_str())?;
    result.integr_schema = {
        let y: serde_yaml::Value = serde_yaml::from_str(integration_box.integr_schema()).unwrap();
        let j = serde_json::to_value(y).unwrap();
        j
    };

    if exists {
        match fs::read_to_string(&sanitized_path) {
            Ok(content) => {
                match serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    Ok(y) => {
                        let j = serde_json::to_value(y).unwrap();
                        match integration_box
                            .integr_settings_apply(
                                gcx.clone(),
                                better_integr_config_path.clone(),
                                &j,
                            )
                            .await
                        {
                            Ok(_) => {}
                            Err(err) => {
                                result.error_log.push(YamlError {
                                    path: better_integr_config_path.clone(),
                                    error_line: err.line(),
                                    error_msg: err.to_string(),
                                });
                                tracing::warn!("cannot deserialize fields in {better_integr_config_path}: {err}");
                            }
                        }
                        let common_settings = integration_box.integr_common();
                        result.integr_values = integration_box.integr_settings_as_json();
                        if let Some(values) = result.integr_values.as_object_mut() {
                            values.remove("oauth_tokens");
                        }
                        result.integr_values["available"]["on_your_laptop"] =
                            common_settings.available.on_your_laptop.into();
                        result.integr_values["available"]["when_isolated"] =
                            common_settings.available.when_isolated.into();
                        result.integr_values["confirmation"]["ask_user"] =
                            common_settings.confirmation.ask_user.into();
                        result.integr_values["confirmation"]["deny"] =
                            common_settings.confirmation.deny.into();
                    }
                    Err(err) => {
                        result.error_log.push(YamlError {
                            path: better_integr_config_path.clone(),
                            error_line: err.location().map(|loc| loc.line()).unwrap_or(0),
                            error_msg: err.to_string(),
                        });
                        tracing::warn!("cannot parse {better_integr_config_path}: {err}");
                        return Ok(result);
                    }
                };
            }
            Err(e) => {
                return Err(format!(
                    "failed to read configuration file: {}",
                    e.to_string()
                ));
            }
        };
    }
    Ok(result)
}

fn create_integration(
    gcx: &Arc<GlobalContext>,
    name: &str,
) -> Result<Box<dyn crate::integr_abstract::IntegrationTrait + Send + Sync>, String> {
    gcx.context_factory.get().integration_provider().create_integration(name)
}

const OAUTH_TOKENS_FIELD: &str = "oauth_tokens";
const TRANSPORT_SELECTOR_FIELDS: &[&str] = &["url", "command"];

pub fn preserve_unknown_fields(
    config_path: &std::path::Path,
    existing_text: &str,
    new_value: &mut serde_yaml::Value,
) {
    let existing_value: serde_yaml::Value = match serde_yaml::from_str(existing_text) {
        Ok(value) => value,
        Err(_) => return,
    };
    let existing_map = match existing_value.as_mapping() {
        Some(map) => map.clone(),
        None => return,
    };
    let new_map = match new_value.as_mapping_mut() {
        Some(map) => map,
        None => return,
    };
    let remote_transport = new_map
        .get(&serde_yaml::Value::String("url".to_string()))
        .and_then(|value| value.as_str())
        .is_some_and(|url| !url.trim().is_empty());

    if !remote_transport {
        let key = serde_yaml::Value::String(OAUTH_TOKENS_FIELD.to_string());
        if new_map.remove(&key).is_some() {
            tracing::info!(
                "dropping stale {} from {}: config no longer uses a remote transport",
                OAUTH_TOKENS_FIELD,
                config_path.display()
            );
        }
    }

    for (key, value) in existing_map {
        if new_map.contains_key(&key) {
            continue;
        }
        let key_name = key.as_str().unwrap_or_default();
        if key_name == OAUTH_TOKENS_FIELD && !remote_transport {
            continue;
        }
        if TRANSPORT_SELECTOR_FIELDS.contains(&key_name) {
            continue;
        }
        new_map.insert(key, value);
    }
}

pub fn leading_marketplace_comments(existing_text: &str) -> String {
    let mut preserved = String::new();
    for line in existing_text.lines() {
        if line.starts_with("# mcp_marketplace_") {
            preserved.push_str(line);
            preserved.push('\n');
        } else if !line.trim_start().starts_with('#') && !line.trim().is_empty() {
            break;
        }
    }
    preserved
}

pub async fn write_config_atomically(
    config_path: &std::path::Path,
    contents: &str,
) -> Result<(), String> {
    let tmp_path = config_path.with_extension("yaml.tmp");
    async_fs::write(&tmp_path, contents.as_bytes())
        .await
        .map_err(|e| format!("Failed to write {}: {}", tmp_path.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = match async_fs::metadata(config_path).await {
            Ok(metadata) => metadata.permissions().mode() & 0o777,
            Err(_) => 0o600,
        };
        if let Err(e) =
            async_fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(mode)).await
        {
            let _ = async_fs::remove_file(&tmp_path).await;
            return Err(format!(
                "Failed to preserve permissions on {}: {}",
                config_path.display(),
                e
            ));
        }
    }
    async_fs::rename(&tmp_path, config_path)
        .await
        .map_err(|e| format!("Failed to write {}: {}", config_path.display(), e))
}

pub async fn integration_config_save(
    gcx: Arc<GlobalContext>,
    integr_config_path: &String,
    integr_values: &serde_json::Value,
) -> Result<(), String> {
    let config_path = refact_files::path_utils::canonical_path(integr_config_path);
    let (integr_name, _project_path) =
        split_path_into_project_and_integration(&config_path)
            .map_err(|e| format!("Failed to split path: {}", e))?;
    let mut integration_box = create_integration(&gcx, integr_name.as_str())
        .map_err(|e| format!("Failed to load integrations: {}", e))?;

    integration_box
        .integr_settings_apply(gcx.clone(), integr_config_path.clone(), integr_values)
        .await
        .map_err(|e| {
            format!(
                "validation error at {}:{}: {}",
                integr_config_path,
                e.line(),
                e
            )
        })?;

    let mut sanitized_json: serde_json::Value = integration_box.integr_settings_as_json();
    let common_settings = integration_box.integr_common();
    if let (Value::Object(sanitized_json_m), Value::Object(common_settings_m)) =
        (&mut sanitized_json, json!(common_settings))
    {
        sanitized_json_m.extend(common_settings_m);
    }

    tracing::info!("writing to {}", config_path.display());
    let existing_text = async_fs::read_to_string(&config_path)
        .await
        .unwrap_or_default();
    let mut sanitized_yaml = serde_yaml::to_value(sanitized_json).unwrap();
    preserve_unknown_fields(&config_path, &existing_text, &mut sanitized_yaml);

    let config_dir = config_path
        .parent()
        .ok_or_else(|| "Failed to get parent directory".to_string())?;
    async_fs::create_dir_all(config_dir)
        .await
        .map_err(|e| format!("Failed to create {}: {}", config_dir.display(), e))?;

    let sanitized_yaml_string = format!(
        "{}{}",
        leading_marketplace_comments(&existing_text),
        serde_yaml::to_string(&sanitized_yaml).unwrap()
    );
    write_config_atomically(&config_path, &sanitized_yaml_string).await?;
    gcx.tool_catalog_generations.advance_integrations();

    // If it is an mcp integration, ensure we restart or reconnect to the server
    if config_path
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.starts_with("mcp_"))
    {
        gcx.tool_catalog_generations.advance_mcp();
        let _ = crate::integrations_runtime::load_integrations(
            gcx.clone(),
            &["**/mcp_*".to_string()],
        )
        .await;
    }

    Ok(())
}
