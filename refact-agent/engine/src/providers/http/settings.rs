use super::*;

pub use refact_providers::http::settings::*;

#[allow(dead_code)]
pub(crate) async fn merge_provider_settings_preserving_secrets(
    config_dir: &std::path::Path,
    instance_id: &str,
    base_provider: &str,
    new_settings: serde_yaml::Value,
) -> Result<serde_yaml::Value, ScratchError> {
    let config_path = provider_file_path(config_dir, instance_id);

    if !provider_file_exists(config_dir, instance_id) {
        if supports_structured_command_credentials(base_provider) {
            return merge_yaml_preserving_secrets_for_provider(
                base_provider,
                serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                new_settings,
            )
            .map_err(|e| ScratchError::new(StatusCode::UNPROCESSABLE_ENTITY, e));
        }
        return Ok(strip_masked_secrets(new_settings));
    }

    let content = tokio::fs::read_to_string(&config_path).await.map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read config: {}", e),
        )
    })?;

    let existing: serde_yaml::Value = serde_yaml::from_str(&content).map_err(|e| {
        ScratchError::new(
            StatusCode::CONFLICT,
            format!(
                "Existing config is invalid YAML: {}. Fix manually or delete the file.",
                e
            ),
        )
    })?;

    merge_yaml_preserving_secrets_for_provider(base_provider, existing, new_settings)
        .map_err(|e| ScratchError::new(StatusCode::UNPROCESSABLE_ENTITY, e))
}


pub(crate) async fn patch_provider_model_config(
    ctx: &ChatCtx,
    config_dir: &std::path::Path,
    provider_name: &str,
) -> Result<(), ScratchError> {
    validate_instance_id_for_http(provider_name)?;
    let (identity, model_config) = {
        let registry = ctx.model.providers.read().await;
        let provider = registry.get(provider_name).ok_or_else(|| {
            ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found", provider_name),
            )
        })?;
        (
            identity_from_provider(provider),
            ProviderModelConfigSnapshot::from_provider(provider),
        )
    };

    config_store::update_provider_config_with(
        config_dir,
        provider_name,
        provider_config_store_error,
        |existing| {
            let mut yaml_map = match existing {
                Some(value) => value.as_mapping().cloned().ok_or_else(|| {
                    ScratchError::new(
                        StatusCode::CONFLICT,
                        "Config file root is not a YAML mapping. Cannot safely patch.".to_string(),
                    )
                })?,
                None => serde_yaml::Mapping::new(),
            };

            ensure_yaml_identity_fields(&mut yaml_map, &identity);

            insert_yaml_value(
                &mut yaml_map,
                "enabled_models",
                &model_config.enabled_models,
            )?;
            if !model_config.enabled_models.is_empty() {
                yaml_map.insert(yaml_key("enabled"), serde_yaml::Value::Bool(true));
            }
            if !model_config.disabled_models.is_empty() {
                insert_yaml_value(
                    &mut yaml_map,
                    "disabled_models",
                    &model_config.disabled_models,
                )?;
            } else {
                yaml_map.remove(yaml_key("disabled_models"));
            }
            insert_yaml_value(&mut yaml_map, "custom_models", &model_config.custom_models)?;
            if model_config.selected_providers.is_empty() {
                yaml_map.remove(yaml_key("selected_providers"));
            } else {
                insert_yaml_value(
                    &mut yaml_map,
                    "selected_providers",
                    &model_config.selected_providers,
                )?;
            }

            Ok(serde_yaml::Value::Mapping(yaml_map))
        },
    )
    .await?;

    Ok(())
}


pub(crate) async fn reload_provider_from_disk(
    ctx: &ChatCtx,
    provider_name: &str,
    config_dir: &std::path::Path,
) -> Result<(), ScratchError> {
    validate_instance_id_for_http(provider_name)?;
    if !provider_file_exists(config_dir, provider_name) {
        return Ok(());
    }

    let provider_path = provider_file_path(config_dir, provider_name);
    let content = tokio::fs::read_to_string(provider_path)
        .await
        .map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to reload config: {}", e),
            )
        })?;

    let yaml: serde_yaml::Value = serde_yaml::from_str(&content).map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Invalid YAML after save: {}", e),
        )
    })?;

    let mut registry = ctx.model.providers.write().await;

    let provider = provider_from_yaml(provider_name, yaml)?;
    registry.add(provider);

    Ok(())
}
