use std::path::Path;
use std::sync::Arc;

use refact_caps_core::caps_state::CapsState;
use refact_providers::claude_code::ClaudeCodeProvider;
use refact_providers::claude_code_oauth::OAuthTokens;
use refact_providers::traits::ProviderTrait;
use refact_providers::{config_store, oauth_refresh as oauth_tokens};
use tokio::sync::RwLock as ARwLock;

use crate::tool_catalog::ToolCatalogGenerations;

pub const REFRESH_BEFORE_EXPIRY_MS: i64 = 5 * 60 * 1000;

pub fn needs_refresh(expires_at: i64) -> bool {
    if expires_at == 0 {
        return true;
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    now_ms >= expires_at - REFRESH_BEFORE_EXPIRY_MS
}

pub fn proactive_refresh_can_reuse_newer_expiry(
    rejected_expires_at: Option<i64>,
    rejected_status: Option<reqwest::StatusCode>,
    current_expires_at: i64,
) -> bool {
    rejected_status.is_none()
        && rejected_expires_at.is_some_and(|expires_at| current_expires_at > expires_at)
}

pub async fn invalidate_caps(
    caps_state: &Arc<ARwLock<CapsState>>,
    tool_catalog_generations: &ToolCatalogGenerations,
) {
    let mut caps = caps_state.write().await;
    caps.caps = None;
    caps.last_attempted_ts = 0;
    tool_catalog_generations.advance_capabilities();
}

pub async fn current_claude_code_provider(
    providers: &Arc<ARwLock<refact_providers::ProviderRegistry>>,
    provider_name: &str,
) -> Option<ClaudeCodeProvider> {
    providers
        .read()
        .await
        .get(provider_name)
        .and_then(|provider| provider.as_any().downcast_ref::<ClaudeCodeProvider>())
        .cloned()
}

pub async fn sync_claude_code_tokens_from_disk(
    providers: &Arc<ARwLock<refact_providers::ProviderRegistry>>,
    config_dir: &Path,
    provider_name: &str,
    provider: &mut ClaudeCodeProvider,
) -> Result<(), String> {
    let path = config_store::provider_config_path(config_dir, provider_name);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Failed to read Claude Code provider config: {error}"
            ))
        }
    };
    let yaml: serde_yaml::Value = serde_yaml::from_str(&content)
        .map_err(|error| format!("Claude Code provider config is invalid YAML: {error}"))?;
    let Some(tokens_value) = yaml.get("oauth_tokens").cloned() else {
        return Ok(());
    };
    let disk_tokens: OAuthTokens = serde_yaml::from_value(tokens_value)
        .map_err(|error| format!("Failed to parse Claude Code OAuth tokens: {error}"))?;
    if disk_tokens == provider.oauth_tokens {
        return Ok(());
    }

    provider.oauth_tokens = disk_tokens.clone();
    let mut registry = providers.write().await;
    if let Some(current) = registry
        .get_mut(provider_name)
        .and_then(|current| current.as_any_mut().downcast_mut::<ClaudeCodeProvider>())
    {
        current.apply_oauth_refresh_tokens(
            &disk_tokens.access_token,
            &disk_tokens.refresh_token,
            disk_tokens.expires_at,
        );
    }
    Ok(())
}

pub async fn save_claude_code_refreshed_tokens(
    providers: &Arc<ARwLock<refact_providers::ProviderRegistry>>,
    config_dir: &Path,
    provider_name: &str,
    expected_tokens: &OAuthTokens,
    refreshed_tokens: &OAuthTokens,
) -> Result<bool, String> {
    let registry_matches = {
        let registry = providers.read().await;
        registry
            .get(provider_name)
            .and_then(|provider| provider.as_any().downcast_ref::<ClaudeCodeProvider>())
            .is_some_and(|provider| &provider.oauth_tokens == expected_tokens)
    };
    if !registry_matches {
        return Ok(false);
    }

    let updated = config_store::update_provider_config_if(config_dir, provider_name, |existing| {
        let Some(value) = existing else {
            return Ok(None);
        };
        let mut yaml_map = value.as_mapping().cloned().ok_or_else(|| {
            "Config file root is not a YAML mapping. Cannot safely patch.".to_string()
        })?;
        let current_tokens: OAuthTokens = yaml_map
            .get(&serde_yaml::Value::String("oauth_tokens".to_string()))
            .cloned()
            .map(serde_yaml::from_value)
            .transpose()
            .map_err(|error| format!("Failed to parse existing OAuth tokens: {}", error))?
            .unwrap_or_default();
        if &current_tokens != expected_tokens {
            return Ok(None);
        }

        yaml_map.insert(
            serde_yaml::Value::String("oauth_tokens".to_string()),
            serde_yaml::to_value(refreshed_tokens)
                .map_err(|error| format!("Failed to serialize OAuth tokens: {}", error))?,
        );
        Ok(Some(serde_yaml::Value::Mapping(yaml_map)))
    })
    .await?;
    if updated.is_none() {
        return Ok(false);
    }

    let changed = {
        let mut registry = providers.write().await;
        registry
            .get_mut(provider_name)
            .and_then(|provider| {
                provider
                    .as_any_mut()
                    .downcast_mut::<ClaudeCodeProvider>()
            })
            .filter(|provider| &provider.oauth_tokens == expected_tokens)
            .map(|provider| {
                provider.apply_oauth_refresh_tokens(
                    &refreshed_tokens.access_token,
                    &refreshed_tokens.refresh_token,
                    refreshed_tokens.expires_at,
                );
            })
            .is_some()
    };

    Ok(changed)
}

pub async fn force_refresh_claude_code_for_retry(
    providers: &Arc<ARwLock<refact_providers::ProviderRegistry>>,
    http_client: &reqwest::Client,
    config_dir: &Path,
    caps_state: &Arc<ARwLock<CapsState>>,
    tool_catalog_generations: &ToolCatalogGenerations,
    provider_name: &str,
    rejected_access_token: &str,
    rejected_expires_at: Option<i64>,
    rejected_status: Option<reqwest::StatusCode>,
) -> Result<Option<ClaudeCodeProvider>, String> {
    let refresh_guard = ClaudeCodeProvider::lock_refresh_guard().await?;
    let oauth_file_guard =
        config_store::lock_provider_oauth_file(config_dir, provider_name).await?;
    let provider = {
        let registry = providers.read().await;
        registry
            .get(provider_name)
            .and_then(|provider| provider.as_any().downcast_ref::<ClaudeCodeProvider>())
            .cloned()
    };
    let Some(mut provider) = provider else {
        return Ok(None);
    };
    sync_claude_code_tokens_from_disk(providers, config_dir, provider_name, &mut provider).await?;

    if provider
        .access_token_changed_since_rejection(rejected_access_token)
        .is_some()
    {
        return Ok(Some(provider));
    }
    if proactive_refresh_can_reuse_newer_expiry(
        rejected_expires_at,
        rejected_status,
        provider.oauth_tokens.expires_at,
    ) {
        return Ok(Some(provider));
    }

    if let Some(status) = rejected_status {
        if !ClaudeCodeProvider::should_force_refresh_for_status(
            status,
            &provider.oauth_tokens.refresh_token,
            false,
        ) {
            return Ok(None);
        }
    } else {
        if !needs_refresh(provider.oauth_tokens.expires_at) {
            return Ok(Some(provider));
        }
        if provider.oauth_tokens.refresh_token.is_empty() {
            return Ok(None);
        }
    }

    let previous_tokens = provider.oauth_tokens.clone();
    let refreshed_tokens = match refact_providers::claude_code_oauth::refresh_access_token(
        http_client,
        &previous_tokens.refresh_token,
    )
    .await
    {
        Ok(tokens) => tokens,
        Err(error) if oauth_tokens::is_permanent_refresh_error(&error) => {
            let cleared = save_claude_code_refreshed_tokens(
                providers,
                config_dir,
                provider_name,
                &previous_tokens,
                &OAuthTokens::default(),
            )
            .await?;
            if !cleared {
                return Ok(current_claude_code_provider(providers, provider_name).await);
            }
            oauth_tokens::mark_invalid_refresh_token(provider_name, &previous_tokens.refresh_token);
            drop(oauth_file_guard);
            drop(refresh_guard);
            invalidate_caps(caps_state, tool_catalog_generations).await;
            return Err(format!(
                "Claude Code OAuth refresh token is invalid. Please log in again in Claude Code provider settings: {}",
                error
            ));
        }
        Err(error) => return Err(format!("Claude Code OAuth refresh failed: {}", error)),
    };

    if !save_claude_code_refreshed_tokens(
        providers,
        config_dir,
        provider_name,
        &previous_tokens,
        &refreshed_tokens,
    )
    .await?
    {
        return Ok(current_claude_code_provider(providers, provider_name).await);
    }

    provider.oauth_tokens = refreshed_tokens;
    drop(oauth_file_guard);
    drop(refresh_guard);
    invalidate_caps(caps_state, tool_catalog_generations).await;
    Ok(Some(provider))
}
