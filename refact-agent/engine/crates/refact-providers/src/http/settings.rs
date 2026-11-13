use std::collections::HashMap;

use refact_core::custom_error::{ScratchError, StatusCode};

use super::common::{supports_structured_command_credentials, HttpProviderIdentity};
use crate::traits::{
    extra_headers_mapping_to_hash_map, parse_extra_headers_value, CustomModelConfig, ProviderTrait,
};

pub const DERIVED_SETTINGS_KEYS: &[&str] = &[
    "auth_status",
    "auth_source",
    "oauth_connected",
    "cli_refresh_managed",
    "api_key_ready",
    "claude_cli_path",
    "readonly",
];

pub fn strip_derived_fields(value: serde_yaml::Value) -> serde_yaml::Value {
    if let serde_yaml::Value::Mapping(mut map) = value {
        for key in DERIVED_SETTINGS_KEYS {
            map.remove(serde_yaml::Value::String(key.to_string()));
        }
        serde_yaml::Value::Mapping(map)
    } else {
        value
    }
}

pub fn merge_yaml_preserving_secrets_for_provider(
    provider_name: &str,
    existing: serde_yaml::Value,
    new: serde_yaml::Value,
) -> Result<serde_yaml::Value, String> {
    use serde_yaml::Value;

    match (existing, new) {
        (Value::Mapping(mut existing_map), Value::Mapping(new_map)) => {
            for (key, new_value) in new_map {
                let replace_extra_headers = supports_structured_command_credentials(provider_name)
                    && key.as_str() == Some("extra_headers");
                let replace_credential = supports_structured_command_credentials(provider_name)
                    && key.as_str() == Some("credential");
                let existing_value = existing_map.remove(&key);
                let merged_value = if replace_extra_headers {
                    merge_structured_extra_headers_replace(existing_value.as_ref(), &new_value)?
                } else if replace_credential {
                    strip_masked_secrets(new_value)
                } else if let Some(existing_value) = existing_value {
                    merge_yaml_preserving_secrets_for_provider(
                        provider_name,
                        existing_value,
                        new_value,
                    )?
                } else {
                    strip_masked_secrets(new_value)
                };
                existing_map.insert(key, merged_value);
            }
            Ok(Value::Mapping(existing_map))
        }
        (existing, Value::String(s)) if s == "***" => Ok(existing),
        (_, new) => Ok(strip_masked_secrets(new)),
    }
}

pub fn merge_structured_extra_headers_replace(
    existing: Option<&serde_yaml::Value>,
    incoming: &serde_yaml::Value,
) -> Result<serde_yaml::Value, String> {
    use serde_yaml::{Mapping, Value};

    let incoming_map = parse_extra_headers_value(incoming)?;
    let existing_headers = match existing {
        Some(value) => extra_headers_mapping_to_hash_map(None, &parse_extra_headers_value(value)?),
        None => HashMap::new(),
    };
    let merged_headers = extra_headers_mapping_to_hash_map(Some(&existing_headers), &incoming_map);
    let mut out = Mapping::new();

    for (key, value) in merged_headers {
        out.insert(Value::String(key), Value::String(value));
    }

    Ok(Value::Mapping(out))
}

pub fn strip_masked_secrets(value: serde_yaml::Value) -> serde_yaml::Value {
    use serde_yaml::Value;

    match value {
        Value::String(s) if s == "***" => Value::String(String::new()),
        Value::Mapping(map) => {
            let filtered: serde_yaml::Mapping = map
                .into_iter()
                .map(|(k, v)| (k, strip_masked_secrets(v)))
                .collect();
            Value::Mapping(filtered)
        }
        Value::Sequence(seq) => {
            Value::Sequence(seq.into_iter().map(strip_masked_secrets).collect())
        }
        other => other,
    }
}

pub fn identity_from_provider(provider: &dyn ProviderTrait) -> HttpProviderIdentity {
    HttpProviderIdentity {
        instance_id: provider.name().to_string(),
        base_provider: provider.base_provider_name().to_string(),
        display_name: provider.display_name().to_string(),
    }
}

pub fn ensure_yaml_identity_fields(
    yaml_map: &mut serde_yaml::Mapping,
    identity: &HttpProviderIdentity,
) {
    yaml_map.insert(
        yaml_key("base_provider"),
        serde_yaml::Value::String(identity.base_provider.clone()),
    );
    yaml_map.insert(
        yaml_key("display_name"),
        serde_yaml::Value::String(identity.display_name.clone()),
    );
}

#[derive(Clone, Copy, Default)]
pub struct ProviderModelConfigFields {
    pub enabled_models: bool,
    pub disabled_models: bool,
    pub custom_models: bool,
    pub selected_providers: bool,
}

impl ProviderModelConfigFields {
    pub fn from_settings(settings: &serde_yaml::Value) -> Self {
        let Some(map) = settings.as_mapping() else {
            return Self::default();
        };
        Self {
            enabled_models: yaml_map_contains_key(map, "enabled_models"),
            disabled_models: yaml_map_contains_key(map, "disabled_models"),
            custom_models: yaml_map_contains_key(map, "custom_models"),
            selected_providers: yaml_map_contains_key(map, "selected_providers"),
        }
    }
}

pub struct ProviderModelConfigSnapshot {
    pub enabled_models: Vec<String>,
    pub disabled_models: Vec<String>,
    pub custom_models: HashMap<String, CustomModelConfig>,
    pub selected_providers: HashMap<String, String>,
}

impl ProviderModelConfigSnapshot {
    pub fn from_provider(provider: &dyn ProviderTrait) -> Self {
        Self {
            enabled_models: provider.enabled_models().to_vec(),
            disabled_models: provider.disabled_models().to_vec(),
            custom_models: provider.custom_models().clone(),
            selected_providers: provider.selected_providers().clone(),
        }
    }
}

pub fn yaml_key(key: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(key.to_string())
}

pub fn yaml_map_contains_key(map: &serde_yaml::Mapping, key: &str) -> bool {
    map.contains_key(&yaml_key(key))
}

pub fn insert_yaml_value<T: serde::Serialize>(
    yaml_map: &mut serde_yaml::Mapping,
    key: &str,
    value: &T,
) -> Result<(), ScratchError> {
    yaml_map.insert(
        yaml_key(key),
        serde_yaml::to_value(value).map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to serialize {key}: {e}"),
            )
        })?,
    );
    Ok(())
}

pub fn apply_model_config_snapshot_for_omitted_fields(
    yaml: serde_yaml::Value,
    snapshot: &ProviderModelConfigSnapshot,
    present_fields: ProviderModelConfigFields,
) -> Result<serde_yaml::Value, ScratchError> {
    let mut yaml_map = yaml.as_mapping().cloned().ok_or_else(|| {
        ScratchError::new(
            StatusCode::CONFLICT,
            "Config file root is not a YAML mapping. Cannot safely reload.".to_string(),
        )
    })?;

    if !present_fields.enabled_models
        && (!snapshot.enabled_models.is_empty()
            || yaml_map_contains_key(&yaml_map, "enabled_models"))
    {
        insert_yaml_value(&mut yaml_map, "enabled_models", &snapshot.enabled_models)?;
    }
    if !present_fields.disabled_models {
        if snapshot.disabled_models.is_empty() {
            yaml_map.remove(yaml_key("disabled_models"));
        } else {
            insert_yaml_value(&mut yaml_map, "disabled_models", &snapshot.disabled_models)?;
        }
    }
    if !present_fields.custom_models
        && (!snapshot.custom_models.is_empty() || yaml_map_contains_key(&yaml_map, "custom_models"))
    {
        insert_yaml_value(&mut yaml_map, "custom_models", &snapshot.custom_models)?;
    }
    if !present_fields.selected_providers {
        if snapshot.selected_providers.is_empty() {
            yaml_map.remove(yaml_key("selected_providers"));
        } else {
            insert_yaml_value(
                &mut yaml_map,
                "selected_providers",
                &snapshot.selected_providers,
            )?;
        }
    }

    Ok(serde_yaml::Value::Mapping(yaml_map))
}
