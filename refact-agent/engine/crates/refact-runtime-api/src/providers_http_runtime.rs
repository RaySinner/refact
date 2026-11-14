use refact_core::custom_error::{ScratchError, StatusCode};
use refact_providers::config_store;
use refact_providers::create_provider;
use refact_providers::http::common::{
    identity_for_model_management, provider_config_store_error, provider_file_exists,
    provider_file_path, provider_from_yaml, read_existing_provider_settings,
    resolve_config_identity, validate_instance_id_for_http, HttpProviderIdentity,
};
use refact_providers::http::settings::{
    ensure_yaml_identity_fields, identity_from_provider, insert_yaml_value, yaml_key,
    ProviderModelConfigSnapshot,
};
use refact_providers::instance::ProviderInstance;
use refact_providers::traits::ProviderTrait;

use crate::chat_ctx::ChatCtx;

pub async fn invalidate_caps(ctx: &ChatCtx) {
    let caps_state = ctx.model.caps.clone();
    let mut caps_state = caps_state.write().await;
    caps_state.caps = None;
    caps_state.last_attempted_ts = 0;
    ctx.tool_catalog_generations.advance_capabilities();
}

pub async fn resolve_provider_identity(
    ctx: &ChatCtx,
    instance_id: &str,
) -> Result<HttpProviderIdentity, ScratchError> {
    validate_instance_id_for_http(instance_id)?;
    let (config_dir, registry_identity) = {
        let registry = ctx.model.providers.read().await;
        let registry_identity = registry
            .get(instance_id)
            .map(|provider| HttpProviderIdentity {
                instance_id: provider.name().to_string(),
                base_provider: provider.base_provider_name().to_string(),
                display_name: provider.display_name().to_string(),
            });
        (ctx.paths.config_dir.clone(), registry_identity)
    };
    if let Some(identity) = registry_identity {
        return Ok(identity);
    }
    resolve_config_identity(&config_dir, instance_id).await
}

pub async fn resolve_provider_for_base(
    ctx: &ChatCtx,
    provider_name: &str,
    expected_base: &str,
) -> Result<(Box<dyn ProviderTrait>, reqwest::Client), ScratchError> {
    let identity = resolve_provider_identity(ctx, provider_name).await?;
    if identity.base_provider != expected_base {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "Provider '{}' uses base_provider '{}' and does not support this route",
                provider_name, identity.base_provider
            ),
        ));
    }
    let (provider, http_client) = {
        let registry = ctx.model.providers.read().await;
        (
            registry
                .get(provider_name)
                .map(|provider| provider.clone_box()),
            ctx.runtime.http_client.clone(),
        )
    };
    let provider = if let Some(provider) = provider {
        provider
    } else {
        let config_dir = ctx.paths.config_dir.clone();
        if provider_file_exists(&config_dir, provider_name) {
            let settings = read_existing_provider_settings(&config_dir, provider_name)
                .await?
                .ok_or_else(|| {
                    ScratchError::new(
                        StatusCode::NOT_FOUND,
                        format!("Provider '{}' not found", provider_name),
                    )
                })?;
            provider_from_yaml(provider_name, settings)?
        } else {
            create_provider(expected_base).ok_or_else(|| {
                ScratchError::new(
                    StatusCode::NOT_FOUND,
                    format!("Provider '{}' not found", provider_name),
                )
            })?
        }
    };
    Ok((provider, http_client))
}

pub async fn ensure_provider_for_model_management(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<(), ScratchError> {
    let (config_dir, already_registered) = {
        let registry = ctx.model.providers.read().await;
        (
            ctx.paths.config_dir.clone(),
            registry.get(provider_name).is_some(),
        )
    };
    if already_registered {
        return Ok(());
    }
    if provider_file_exists(&config_dir, provider_name) {
        reload_provider_from_disk(ctx, provider_name, &config_dir).await
    } else {
        let identity = identity_for_model_management(&config_dir, provider_name).await?;
        let provider = create_provider(&identity.base_provider).ok_or_else(|| {
            ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found or not configured", provider_name),
            )
        })?;
        let provider: Box<dyn ProviderTrait> = if identity.instance_id == identity.base_provider {
            provider
        } else {
            Box::new(ProviderInstance::new(
                identity.instance_id,
                identity.base_provider,
                identity.display_name,
                provider,
            ))
        };
        let mut registry = ctx.model.providers.write().await;
        registry.add(provider);
        Ok(())
    }
}

pub async fn patch_provider_model_config(
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

pub async fn reload_provider_from_disk(
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
