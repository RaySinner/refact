use super::*;

pub use refact_providers::http::common::*;

pub(crate) fn json_response(
    status: StatusCode,
    body: &impl Serialize,
) -> Result<Response<Body>, ScratchError> {
    let json = serde_json::to_string(body).map_err(|e| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("JSON serialization failed: {}", e),
        )
    })?;
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(json))
        .map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Response build failed: {}", e),
            )
        })
}


pub(crate) async fn invalidate_caps(ctx: &ChatCtx) {
    let caps_state = ctx.model.caps.clone();
    let mut caps_state = caps_state.write().await;
    caps_state.caps = None;
    caps_state.last_attempted_ts = 0;
    ctx.tool_catalog_generations.advance_capabilities();
}


pub(crate) async fn resolve_provider_identity(
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


pub(crate) async fn resolve_provider_for_base(
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


pub(crate) async fn ensure_provider_for_model_management(
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
