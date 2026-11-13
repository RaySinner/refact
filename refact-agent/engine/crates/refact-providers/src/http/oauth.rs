use refact_core::custom_error::{ScratchError, StatusCode};

use super::common::{provider_display_name, provider_file_exists, HttpProviderIdentity};
use super::settings::ensure_yaml_identity_fields;
use crate::identity::provider_identity_from_yaml;

pub fn ensure_oauth_identity_for_instance_sync(
    config_dir: &std::path::Path,
    provider_name: &str,
    base_provider: &str,
    yaml_map: &mut serde_yaml::Mapping,
) -> Result<HttpProviderIdentity, ScratchError> {
    let identity = match provider_identity_from_yaml(
        provider_name,
        &serde_yaml::Value::Mapping(yaml_map.clone()),
    ) {
        Ok(identity) => HttpProviderIdentity {
            display_name: identity
                .display_name
                .clone()
                .unwrap_or(provider_display_name(&identity.base_provider)?),
            instance_id: identity.instance_id,
            base_provider: identity.base_provider,
        },
        Err(_) if !provider_file_exists(config_dir, provider_name) => HttpProviderIdentity {
            instance_id: provider_name.to_string(),
            base_provider: base_provider.to_string(),
            display_name: provider_display_name(base_provider)?,
        },
        Err(e) => return Err(ScratchError::new(StatusCode::BAD_REQUEST, e)),
    };
    if identity.base_provider != base_provider {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "OAuth not supported for provider '{}' with base_provider '{}'",
                provider_name, identity.base_provider
            ),
        ));
    }
    ensure_yaml_identity_fields(yaml_map, &identity);
    Ok(identity)
}
