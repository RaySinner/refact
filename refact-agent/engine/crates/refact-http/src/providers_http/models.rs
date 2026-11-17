use super::*;

pub use refact_providers::http::models::{
    AddCustomModelRequest, AvailableModelsResponse, GoogleGeminiHealthResponse,
    ModelProviderRequest, ModelToggleRequest, ModelsQueryParams, ModelsResponse,
    OpenRouterHealthResponse, RemoveCustomModelRequest, SimplifiedModel,
};


pub async fn handle_v1_models(
    State(app): State<AppState>,
    Query(params): Query<ModelsQueryParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.provider_name)?;
    let registry = ctx.model.providers.read().await;

    let provider: Box<dyn refact_providers::traits::ProviderTrait> =
        if let Some(p) = registry.get(&params.provider_name) {
            p.clone_box()
        } else if let Some(p) = create_provider(&params.provider_name) {
            p
        } else {
            return Err(ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found", params.provider_name),
            ));
        };

    let runtime = provider
        .build_runtime()
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let response = ModelsResponse {
        chat_models: runtime
            .chat_models
            .iter()
            .map(SimplifiedModel::from)
            .collect(),
        completion_models: runtime
            .completion_models
            .iter()
            .map(SimplifiedModel::from)
            .collect(),
        embedding_model: runtime.embedding_model.as_ref().map(SimplifiedModel::from),
    };

    json_response(StatusCode::OK, &response)
}

// ============================================================================
// Available Models Management Endpoints
// ============================================================================


/// GET /v1/providers/{name}/available-models
/// Fetches all available models for a provider from model_caps or API
pub async fn handle_v1_provider_available_models(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let (provider, http_client) = {
        let registry = ctx.model.providers.read().await;
        let http_client = ctx.runtime.http_client.clone();

        let provider: Box<dyn refact_providers::traits::ProviderTrait> =
            if let Some(p) = registry.get(&params.name) {
                p.clone_box()
            } else if let Some(p) = create_provider(&params.name) {
                p
            } else {
                return Err(ScratchError::new(
                    StatusCode::NOT_FOUND,
                    format!("Provider '{}' not found", params.name),
                ));
            };

        (provider, http_client)
    };

    let source = provider.model_source();
    let (model_caps, caps_error) = match get_model_caps(app.gcx.clone(), false).await {
        Ok(caps) => (caps, None),
        Err(e) => {
            tracing::warn!(
                "Failed to fetch model_caps for provider '{}': {}",
                params.name,
                e
            );
            (
                HashMap::new(),
                Some(format!(
                    "Failed to fetch model capabilities: {}. Model limits may be inaccurate.",
                    e
                )),
            )
        }
    };
    let models = match tokio::time::timeout(
        PROVIDER_AVAILABLE_MODELS_TIMEOUT,
        provider.fetch_available_models(&http_client, &model_caps),
    )
    .await
    {
        Ok(models) => models,
        Err(_) => {
            tracing::warn!(
                "Timed out fetching available models for provider '{}'; using model_caps fallback",
                params.name
            );
            provider.get_available_models_from_caps(&model_caps)
        }
    };
    let error = caps_error;

    let source_str = match source {
        ModelSource::ModelCaps => "model_caps",
        ModelSource::Api => "api",
        ModelSource::Local => "local",
        ModelSource::Manual => "manual",
    };

    let response = AvailableModelsResponse {
        models,
        source: source_str.to_string(),
        error,
    };

    json_response(StatusCode::OK, &response)
}


/// POST /v1/providers/{name}/models/toggle
/// Enable or disable a model for a provider
/// Body: { "model_id": "claude-3-5-sonnet", "enabled": true }
pub async fn handle_v1_provider_model_toggle(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let request: ModelToggleRequest = serde_json::from_slice(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid JSON: {}", e),
        )
    })?;

    // Validate model_id
    if request.model_id.is_empty() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Model ID cannot be empty".to_string(),
        ));
    }
    if request.model_id.len() > 256 {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Model ID too long (max 256 characters)".to_string(),
        ));
    }

    update_model_enabled_state(&ctx, &params.name, &request.model_id, request.enabled).await
}


/// POST /v1/providers/{name}/models/provider
/// Set preferred upstream provider for a model (OpenRouter)
/// Body: { "model_id": "openai/gpt-4.1", "selected_provider": "openai" }
pub async fn handle_v1_provider_model_provider_update(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let request: ModelProviderRequest = serde_json::from_slice(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid JSON: {}", e),
        )
    })?;

    if request.model_id.is_empty() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Model ID cannot be empty".to_string(),
        ));
    }

    if let Some(ref provider) = request.selected_provider {
        if provider.len() > 128 {
            return Err(ScratchError::new(
                StatusCode::BAD_REQUEST,
                "Selected provider is too long (max 128 characters)".to_string(),
            ));
        }
    }

    update_model_selected_provider_state(
        &ctx,
        &params.name,
        &request.model_id,
        request.selected_provider,
    )
    .await
}


/// GET /v1/providers/:name/models/:model_id/endpoints
pub async fn handle_v1_openrouter_model_endpoints(
    State(app): State<AppState>,
    Path(params): Path<ProviderModelPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let (provider, http_client) =
        resolve_provider_for_base(&ctx, &params.name, "openrouter").await?;
    let openrouter = downcast_provider::<OpenRouterProvider>(provider.as_ref(), "OpenRouter")?;

    let (provider_variants, available_providers) = openrouter
        .fetch_model_endpoints(&http_client, &params.model_id)
        .await
        .map_err(|e| ScratchError::new(StatusCode::BAD_GATEWAY, e))?;

    json_response(
        StatusCode::OK,
        &OpenRouterModelEndpointsResponse {
            provider_variants,
            available_providers,
        },
    )
}


pub async fn openrouter_account_info_response(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<Response<Body>, ScratchError> {
    let (provider, http_client) =
        resolve_provider_for_base(ctx, provider_name, "openrouter").await?;
    let openrouter = downcast_provider::<OpenRouterProvider>(provider.as_ref(), "OpenRouter")?;

    let account_info = openrouter
        .fetch_account_info(&http_client)
        .await
        .map_err(|e| ScratchError::new(StatusCode::BAD_GATEWAY, e))?;

    json_response(StatusCode::OK, &json!({"data": account_info}))
}


/// GET /v1/openrouter/account-info
pub async fn handle_v1_openrouter_account_info(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    openrouter_account_info_response(&ctx, "openrouter").await
}


pub async fn handle_v1_provider_account_info(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let identity = resolve_provider_identity(&ctx, &params.name).await?;
    if !openrouter_base_provider_supported(&identity.base_provider) {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Provider '{}' does not support account-info", params.name),
        ));
    }
    openrouter_account_info_response(&ctx, &params.name).await
}


pub async fn openrouter_health_response(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<Response<Body>, ScratchError> {
    let (provider, http_client) =
        resolve_provider_for_base(ctx, provider_name, "openrouter").await?;
    let openrouter = downcast_provider::<OpenRouterProvider>(provider.as_ref(), "OpenRouter")?;

    match openrouter.check_api_key_health(&http_client).await {
        Ok(info) => json_response(
            StatusCode::OK,
            &OpenRouterHealthResponse {
                ok: true,
                message: None,
                data: Some(info),
            },
        ),
        Err(e) => json_response(
            StatusCode::OK,
            &OpenRouterHealthResponse {
                ok: false,
                message: Some(e),
                data: None,
            },
        ),
    }
}


pub async fn google_gemini_health_response(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<Response<Body>, ScratchError> {
    let (provider, http_client) =
        resolve_provider_for_base(ctx, provider_name, "google_gemini").await?;
    let gemini = downcast_provider::<GoogleGeminiProvider>(provider.as_ref(), "Google Gemini")?;

    match gemini.check_api_key_health(&http_client).await {
        Ok(info) => json_response(
            StatusCode::OK,
            &GoogleGeminiHealthResponse {
                ok: true,
                message: None,
                data: Some(info),
            },
        ),
        Err(e) => json_response(
            StatusCode::OK,
            &GoogleGeminiHealthResponse {
                ok: false,
                message: Some(e),
                data: None,
            },
        ),
    }
}


/// GET /v1/openrouter/health
pub async fn handle_v1_openrouter_health(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    openrouter_health_response(&ctx, "openrouter").await
}


/// GET /v1/google-gemini/health
pub async fn handle_v1_google_gemini_health(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    google_gemini_health_response(&ctx, "google_gemini").await
}


pub async fn handle_v1_provider_health(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let identity = resolve_provider_identity(&ctx, &params.name).await?;
    if !health_base_provider_supported(&identity.base_provider) {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Provider '{}' does not support health", params.name),
        ));
    }
    match identity.base_provider.as_str() {
        "openrouter" => openrouter_health_response(&ctx, &params.name).await,
        "google_gemini" => google_gemini_health_response(&ctx, &params.name).await,
        _ => Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Provider '{}' does not support health", params.name),
        )),
    }
}


pub async fn update_model_enabled_state(
    ctx: &ChatCtx,
    provider_name: &str,
    model_id: &str,
    enabled: bool,
) -> Result<Response<Body>, ScratchError> {
    ensure_provider_for_model_management(ctx, provider_name).await?;
    // Capture previous state for rollback
    let (config_dir, previous_enabled_models, previous_disabled_models) = {
        let mut registry = ctx.model.providers.write().await;

        let provider = registry.get_mut(provider_name).ok_or_else(|| {
            ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found or not configured", provider_name),
            )
        })?;

        if provider.is_readonly() {
            return Err(ScratchError::new(
                StatusCode::FORBIDDEN,
                format!("Provider '{}' is readonly", provider_name),
            ));
        }

        // Capture previous state for rollback
        let previous_enabled = provider.enabled_models().to_vec();
        let previous_disabled = provider.disabled_models().to_vec();

        provider.set_model_enabled(model_id, enabled);
        (ctx.paths.config_dir.clone(), previous_enabled, previous_disabled)
    };

    // Try to save updated config
    if let Err(e) = patch_provider_model_config(ctx, &config_dir, provider_name).await {
        // Rollback in-memory state on persistence failure
        let mut registry = ctx.model.providers.write().await;
        if let Some(provider) = registry.get_mut(provider_name) {
            for model in provider.enabled_models().to_vec() {
                provider.set_model_enabled(&model, false);
            }
            for model in provider.disabled_models().to_vec() {
                provider.set_model_enabled(&model, true);
            }
            for model in &previous_enabled_models {
                provider.set_model_enabled(model, true);
            }
            for model in &previous_disabled_models {
                provider.set_model_enabled(model, false);
            }
        }
        return Err(e);
    }

    reload_provider_from_disk(ctx, provider_name, &config_dir).await?;

    invalidate_caps(ctx).await;

    json_response(
        StatusCode::OK,
        &json!({"success": true, "model_id": model_id, "enabled": enabled}),
    )
}


pub async fn update_model_selected_provider_state(
    ctx: &ChatCtx,
    provider_name: &str,
    model_id: &str,
    selected_provider: Option<String>,
) -> Result<Response<Body>, ScratchError> {
    ensure_provider_for_model_management(ctx, provider_name).await?;
    let (config_dir, previous_selected_provider) = {
        let mut registry = ctx.model.providers.write().await;

        let provider = registry.get_mut(provider_name).ok_or_else(|| {
            ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found or not configured", provider_name),
            )
        })?;

        if provider.is_readonly() {
            return Err(ScratchError::new(
                StatusCode::FORBIDDEN,
                format!("Provider '{}' is readonly", provider_name),
            ));
        }

        let prev = provider.selected_providers().get(model_id).cloned();
        provider.set_selected_provider(model_id, selected_provider.clone());
        (ctx.paths.config_dir.clone(), prev)
    };

    if let Err(e) = patch_provider_model_config(ctx, &config_dir, provider_name).await {
        let mut registry = ctx.model.providers.write().await;
        if let Some(provider) = registry.get_mut(provider_name) {
            provider.set_selected_provider(model_id, previous_selected_provider);
        }
        return Err(e);
    }

    reload_provider_from_disk(ctx, provider_name, &config_dir).await?;

    invalidate_caps(ctx).await;

    json_response(
        StatusCode::OK,
        &json!({
            "success": true,
            "model_id": model_id,
            "selected_provider": selected_provider
        }),
    )
}


/// POST /v1/providers/{name}/custom-models
/// Add a custom model to a provider
pub async fn handle_v1_provider_add_custom_model(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let request: AddCustomModelRequest = serde_json::from_slice(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid JSON: {}", e),
        )
    })?;

    // Validate model_id
    if request.id.is_empty() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Model ID cannot be empty".to_string(),
        ));
    }

    ensure_provider_for_model_management(&ctx, &params.name).await?;
    let (config_dir, previous_config) = {
        let mut registry = ctx.model.providers.write().await;

        let provider = registry.get_mut(&params.name).ok_or_else(|| {
            ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found or not configured", params.name),
            )
        })?;

        if provider.is_readonly() {
            return Err(ScratchError::new(
                StatusCode::FORBIDDEN,
                format!("Provider '{}' is readonly", params.name),
            ));
        }

        let previous_config = provider.custom_models().get(&request.id).cloned();
        provider.add_custom_model(request.id.clone(), request.config.clone());
        (ctx.paths.config_dir.clone(), previous_config)
    };

    if let Err(e) = patch_provider_model_config(&ctx, &config_dir, &params.name).await {
        let mut registry = ctx.model.providers.write().await;
        if let Some(provider) = registry.get_mut(&params.name) {
            if let Some(previous) = previous_config {
                provider.add_custom_model(request.id.clone(), previous);
            } else {
                provider.remove_custom_model(&request.id);
            }
        }
        return Err(e);
    }

    reload_provider_from_disk(&ctx, &params.name, &config_dir).await?;

    invalidate_caps(&ctx).await;

    json_response(
        StatusCode::OK,
        &json!({"success": true, "model_id": request.id}),
    )
}


/// POST /v1/providers/{name}/custom-models/remove
/// Remove a custom model from a provider (preferred over DELETE with body)
/// Body: { "model_id": "my-custom-model" }
pub async fn handle_v1_provider_remove_custom_model_post(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    handle_v1_provider_remove_custom_model_impl(&ctx, &params.name, body_bytes).await
}


/// DELETE /v1/providers/{name}/custom-models
/// Remove a custom model from a provider (kept for backward compatibility)
/// Note: Some proxies may strip DELETE request bodies. Prefer POST /custom-models/remove.
/// Body: { "model_id": "my-custom-model" }
pub async fn handle_v1_provider_remove_custom_model(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    handle_v1_provider_remove_custom_model_impl(&ctx, &params.name, body_bytes).await
}


pub async fn handle_v1_provider_remove_custom_model_impl(
    ctx: &ChatCtx,
    provider_name: &str,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    validate_instance_id_for_http(provider_name)?;
    let request: RemoveCustomModelRequest = serde_json::from_slice(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid JSON: {}", e),
        )
    })?;

    if request.model_id.is_empty() {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            "Model ID cannot be empty".to_string(),
        ));
    }

    let (config_dir, previous_custom_models, previous_enabled_models, previous_disabled_models) = {
        let mut registry = ctx.model.providers.write().await;

        let provider = registry.get_mut(provider_name).ok_or_else(|| {
            ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found or not configured", provider_name),
            )
        })?;

        if provider.is_readonly() {
            return Err(ScratchError::new(
                StatusCode::FORBIDDEN,
                format!("Provider '{}' is readonly", provider_name),
            ));
        }

        let previous = provider.custom_models().clone();
        let previous_enabled = provider.enabled_models().to_vec();
        let previous_disabled = provider.disabled_models().to_vec();

        if !provider.remove_custom_model(&request.model_id) {
            return Err(ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Custom model '{}' not found", request.model_id),
            ));
        }
        provider.set_model_enabled(&request.model_id, false);

        (
            ctx.paths.config_dir.clone(),
            previous,
            previous_enabled,
            previous_disabled,
        )
    };

    // Try to save updated config, rollback on failure
    if let Err(e) = patch_provider_model_config(ctx, &config_dir, provider_name).await {
        // Rollback in-memory state
        let mut registry = ctx.model.providers.write().await;
        if let Some(provider) = registry.get_mut(provider_name) {
            if let Some(config) = previous_custom_models.get(&request.model_id) {
                provider.add_custom_model(request.model_id.clone(), config.clone());
            }
            for model in provider.enabled_models().to_vec() {
                provider.set_model_enabled(&model, false);
            }
            for model in provider.disabled_models().to_vec() {
                provider.set_model_enabled(&model, true);
            }
            for model in &previous_enabled_models {
                provider.set_model_enabled(model, true);
            }
            for model in &previous_disabled_models {
                provider.set_model_enabled(model, false);
            }
        }
        return Err(e);
    }

    reload_provider_from_disk(ctx, provider_name, &config_dir).await?;

    invalidate_caps(ctx).await;

    json_response(
        StatusCode::OK,
        &json!({"success": true, "model_id": request.model_id}),
    )
}
