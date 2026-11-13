use std::time::Duration;

use refact_core::custom_error::{ScratchError, StatusCode};

use crate::config_store;
use crate::create_provider;
use crate::identity::{provider_identity_from_yaml, validate_provider_instance_id};
use crate::instance::ProviderInstance;
use crate::traits::ProviderTrait;

pub const PROVIDER_AVAILABLE_MODELS_TIMEOUT: Duration = Duration::from_secs(20);

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[derive(Clone)]
pub struct HttpProviderIdentity {
    pub instance_id: String,
    pub base_provider: String,
    pub display_name: String,
}

pub fn validate_instance_id_for_http(instance_id: &str) -> Result<(), ScratchError> {
    validate_provider_instance_id(instance_id)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))
}

pub fn yaml_string_field(
    value: &serde_yaml::Value,
    key: &str,
    allow_empty: bool,
) -> Result<Option<String>, ScratchError> {
    let Some(field) = value.get(key) else {
        return Ok(None);
    };
    let Some(text) = field.as_str() else {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Provider field '{key}' must be a string"),
        ));
    };
    let text = text.trim();
    if text.is_empty() {
        if allow_empty {
            return Ok(None);
        }
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Provider field '{key}' cannot be empty"),
        ));
    }
    Ok(Some(text.to_string()))
}

pub fn ensure_settings_mapping(
    settings: serde_yaml::Value,
) -> Result<serde_yaml::Mapping, ScratchError> {
    match settings {
        serde_yaml::Value::Mapping(map) => Ok(map),
        _ => Err(ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Provider settings must be a YAML mapping".to_string(),
        )),
    }
}

pub fn provider_display_name(base_provider: &str) -> Result<String, ScratchError> {
    create_provider(base_provider)
        .map(|provider| provider.display_name().to_string())
        .ok_or_else(|| {
            ScratchError::new(
                StatusCode::BAD_REQUEST,
                format!("Unknown base provider '{base_provider}'"),
            )
        })
}

pub fn oauth_supported_for_base(base_provider: &str) -> bool {
    matches!(
        base_provider,
        "claude_code" | "openai_codex" | "github_copilot" | "xai_oauth" | "google_antigravity"
    )
}

pub fn openrouter_base_provider_supported(base_provider: &str) -> bool {
    base_provider == "openrouter"
}

pub fn health_base_provider_supported(base_provider: &str) -> bool {
    matches!(base_provider, "openrouter" | "google_gemini")
}

pub fn usage_base_provider_supported(base_provider: &str) -> bool {
    matches!(
        base_provider,
        "claude_code" | "openai_codex" | "opencode" | "google_antigravity" | "xai_oauth"
    )
}

pub fn supports_structured_command_credentials(base_provider: &str) -> bool {
    matches!(base_provider, "custom" | "litellm")
}

pub fn provider_identity_from_existing_config(
    instance_id: &str,
    settings: &serde_yaml::Value,
) -> Result<HttpProviderIdentity, ScratchError> {
    let identity = provider_identity_from_yaml(instance_id, settings)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
    let display_name = identity
        .display_name
        .unwrap_or(provider_display_name(&identity.base_provider)?);
    Ok(HttpProviderIdentity {
        instance_id: identity.instance_id,
        base_provider: identity.base_provider,
        display_name,
    })
}

pub async fn resolve_config_identity(
    config_dir: &std::path::Path,
    instance_id: &str,
) -> Result<HttpProviderIdentity, ScratchError> {
    validate_instance_id_for_http(instance_id)?;
    if let Some(existing_settings) =
        read_existing_provider_settings(config_dir, instance_id).await?
    {
        return provider_identity_from_existing_config(instance_id, &existing_settings);
    }
    if create_provider(instance_id).is_none() {
        return Err(ScratchError::new(
            StatusCode::NOT_FOUND,
            format!("Provider '{}' not found or not configured", instance_id),
        ));
    }
    Ok(HttpProviderIdentity {
        instance_id: instance_id.to_string(),
        base_provider: instance_id.to_string(),
        display_name: provider_display_name(instance_id)?,
    })
}

pub fn downcast_provider<'a, T: 'static>(
    provider: &'a dyn ProviderTrait,
    type_name: &str,
) -> Result<&'a T, ScratchError> {
    provider.as_any().downcast_ref::<T>().ok_or_else(|| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to resolve {type_name} provider type"),
        )
    })
}

pub async fn identity_for_model_management(
    config_dir: &std::path::Path,
    provider_name: &str,
) -> Result<HttpProviderIdentity, ScratchError> {
    validate_instance_id_for_http(provider_name)?;
    if let Some(existing_settings) =
        read_existing_provider_settings(config_dir, provider_name).await?
    {
        return provider_identity_from_existing_config(provider_name, &existing_settings);
    }
    if create_provider(provider_name).is_none() {
        return Err(ScratchError::new(
            StatusCode::NOT_FOUND,
            format!("Provider '{}' not found or not configured", provider_name),
        ));
    }
    Ok(HttpProviderIdentity {
        instance_id: provider_name.to_string(),
        base_provider: provider_name.to_string(),
        display_name: provider_display_name(provider_name)?,
    })
}

pub fn settings_with_identity(provider: &dyn ProviderTrait) -> serde_json::Value {
    let mut settings = provider.provider_settings_as_json();
    if let serde_json::Value::Object(map) = &mut settings {
        map.insert(
            "base_provider".to_string(),
            serde_json::Value::String(provider.base_provider_name().to_string()),
        );
        map.insert(
            "display_name".to_string(),
            serde_json::Value::String(provider.display_name().to_string()),
        );
    }
    settings
}

pub fn provider_list_item(provider: &dyn ProviderTrait) -> ProviderListItem {
    let (enabled, readonly) = match provider.build_runtime() {
        Ok(runtime) => (runtime.enabled, provider.is_readonly()),
        Err(_) => (false, provider.is_readonly()),
    };
    let has_creds = provider.has_credentials();
    let model_count = provider.selected_model_count();
    let status = if has_creds && model_count > 0 && enabled {
        "active"
    } else if has_creds {
        "configured"
    } else {
        "not_configured"
    };
    ProviderListItem {
        name: provider.name().to_string(),
        base_provider: provider.base_provider_name().to_string(),
        display_name: provider.display_name().to_string(),
        enabled,
        readonly,
        has_credentials: has_creds,
        status,
        model_count,
    }
}

pub fn provider_file_path(config_dir: &std::path::Path, instance_id: &str) -> std::path::PathBuf {
    config_store::provider_config_path(config_dir, instance_id)
}

pub fn provider_file_exists(config_dir: &std::path::Path, instance_id: &str) -> bool {
    provider_file_path(config_dir, instance_id).exists()
}

pub async fn read_existing_provider_settings(
    config_dir: &std::path::Path,
    instance_id: &str,
) -> Result<Option<serde_yaml::Value>, ScratchError> {
    let config_path = provider_file_path(config_dir, instance_id);
    if !provider_file_exists(config_dir, instance_id) {
        return Ok(None);
    }
    let content = tokio::fs::read_to_string(&config_path).await.map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read config: {e}"),
        )
    })?;
    let value = serde_yaml::from_str(&content).map_err(|e| {
        ScratchError::new(
            StatusCode::CONFLICT,
            format!("Existing config is invalid YAML: {e}. Fix manually or delete the file."),
        )
    })?;
    Ok(Some(value))
}

pub fn provider_config_store_error(error: String) -> ScratchError {
    let status = if error.contains("invalid YAML") || error.contains("root is not a YAML mapping") {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    ScratchError::new(status, error)
}

pub fn resolve_provider_update_identity_from_existing(
    instance_id: &str,
    registry_identity: Option<HttpProviderIdentity>,
    settings: &serde_yaml::Value,
    existing_settings: Option<&serde_yaml::Value>,
) -> Result<HttpProviderIdentity, ScratchError> {
    validate_instance_id_for_http(instance_id)?;
    let request_base = yaml_string_field(settings, "base_provider", false)?;
    let request_display = yaml_string_field(settings, "display_name", true)?;
    let disk_identity = existing_settings
        .as_ref()
        .and_then(|value| provider_identity_from_yaml(instance_id, value).ok());
    let disk_display = existing_settings.as_ref().and_then(|value| {
        yaml_string_field(value, "display_name", true)
            .ok()
            .flatten()
    });

    let base_provider = if let Some(existing) = registry_identity.as_ref() {
        if let Some(request_base) = request_base.as_ref() {
            if request_base != &existing.base_provider {
                return Err(ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "Provider instance '{}' already uses base_provider '{}'",
                        instance_id, existing.base_provider
                    ),
                ));
            }
        }
        existing.base_provider.clone()
    } else if let Some(identity) = disk_identity.as_ref() {
        if let Some(request_base) = request_base.as_ref() {
            if request_base != &identity.base_provider {
                return Err(ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "Provider instance '{}' already uses base_provider '{}'",
                        instance_id, identity.base_provider
                    ),
                ));
            }
        }
        identity.base_provider.clone()
    } else if let Some(request_base) = request_base {
        request_base
    } else if create_provider(instance_id).is_some() {
        instance_id.to_string()
    } else {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "Provider instance '{}' must include base_provider",
                instance_id
            ),
        ));
    };

    if create_provider(&base_provider).is_none() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Unknown base provider '{base_provider}'"),
        ));
    }

    let display_name = request_display
        .or_else(|| registry_identity.map(|identity| identity.display_name))
        .or(disk_display)
        .or_else(|| disk_identity.and_then(|identity| identity.display_name))
        .unwrap_or(provider_display_name(&base_provider)?);

    Ok(HttpProviderIdentity {
        instance_id: instance_id.to_string(),
        base_provider,
        display_name,
    })
}

pub fn settings_with_forced_identity(
    settings: serde_yaml::Value,
    identity: &HttpProviderIdentity,
) -> Result<serde_yaml::Value, ScratchError> {
    let mut map = ensure_settings_mapping(settings)?;
    map.insert(
        serde_yaml::Value::String("base_provider".to_string()),
        serde_yaml::Value::String(identity.base_provider.clone()),
    );
    map.insert(
        serde_yaml::Value::String("display_name".to_string()),
        serde_yaml::Value::String(identity.display_name.clone()),
    );
    Ok(serde_yaml::Value::Mapping(map))
}

pub fn provider_from_yaml(
    instance_id: &str,
    yaml: serde_yaml::Value,
) -> Result<Box<dyn ProviderTrait>, ScratchError> {
    let identity = provider_identity_from_yaml(instance_id, &yaml)
        .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
    let mut provider = create_provider(&identity.base_provider).ok_or_else(|| {
        ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Unknown base provider '{}'", identity.base_provider),
        )
    })?;
    provider.provider_settings_apply(yaml).map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to apply settings: {e}"),
        )
    })?;
    if identity.wrap_instance {
        provider = match identity.display_name {
            Some(display_name) => Box::new(ProviderInstance::new(
                identity.instance_id,
                identity.base_provider,
                display_name,
                provider,
            )),
            None => Box::new(ProviderInstance::from_inner(identity.instance_id, provider)),
        };
    }
    Ok(provider)
}

#[derive(serde::Serialize)]
pub struct ProviderListItem {
    pub name: String,
    pub base_provider: String,
    pub display_name: String,
    pub enabled: bool,
    pub readonly: bool,
    pub has_credentials: bool,
    pub status: &'static str,
    pub model_count: usize,
}

#[derive(serde::Serialize)]
pub struct ProviderListResponse {
    pub providers: Vec<ProviderListItem>,
}
