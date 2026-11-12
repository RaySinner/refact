use super::*;

pub async fn handle_v1_providers_list(
    State(app): State<AppState>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let registry = ctx.model.providers.read().await;

    let mut providers = Vec::new();
    let mut seen = HashSet::new();
    for (_, provider) in registry.iter() {
        seen.insert(provider.name().to_string());
        providers.push(provider_list_item(provider));
    }
    for name in PROVIDER_NAMES {
        if seen.contains(*name) {
            continue;
        }
        if let Some(default_provider) = create_provider(name) {
            if default_provider.is_hidden_from_list() {
                continue;
            }
            seen.insert((*name).to_string());
            providers.push(provider_list_item(default_provider.as_ref()));
        }
    }
    providers.sort_by(|a, b| a.name.cmp(&b.name));

    let response = ProviderListResponse { providers };
    json_response(StatusCode::OK, &response)
}


#[derive(Deserialize)]
pub struct ProviderPathParams {
    pub(crate) name: String,
}


#[derive(Deserialize, Default)]
pub struct ProviderQuotaQuery {
    #[serde(default)]
    pub(crate) refresh: bool,
}


#[derive(Serialize)]
pub(crate) struct ProviderQuotaResponse {
    pub(crate) quota: ProviderQuotaSnapshot,
}


#[derive(Serialize)]
pub(crate) struct ProviderQuotasResponse {
    pub(crate) quotas: Vec<ProviderQuotaSnapshot>,
    pub(crate) truncated: bool,
}


#[derive(Deserialize)]
pub struct ProviderModelPathParams {
    pub(crate) name: String,
    pub(crate) model_id: String,
}


#[derive(Serialize)]
pub struct OpenRouterModelEndpointsResponse {
    pub provider_variants: Vec<crate::providers::traits::ProviderVariant>,
    pub available_providers: Vec<String>,
}


#[derive(Serialize)]
pub(crate) struct ProviderDetailResponse {
    pub(crate) name: String,
    pub(crate) base_provider: String,
    pub(crate) display_name: String,
    pub(crate) enabled: bool,
    pub(crate) readonly: bool,
    pub(crate) has_credentials: bool,
    pub(crate) selected_models_count: usize,
    pub(crate) status: &'static str,
    pub(crate) settings: serde_json::Value,
    pub(crate) runtime: Option<ProviderRuntime>,
}


pub async fn handle_v1_provider_get(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let is_claude_code = {
        let registry = ctx.model.providers.read().await;
        registry
            .get(&params.name)
            .is_some_and(|provider| provider.base_provider_name() == "claude_code")
    };
    if is_claude_code {
        if let Err(error) = crate::providers::oauth_refresh::refresh_claude_code_instance_if_needed(
            &app.gcx,
            &params.name,
        )
        .await
        {
            tracing::warn!("Claude Code provider refresh before settings load failed: {error}");
        }
    }
    let (registry_provider, config_dir) = {
        let registry = ctx.model.providers.read().await;
        (
            registry
                .get(&params.name)
                .map(|provider| provider.clone_box()),
            ctx.paths.config_dir.clone(),
        )
    };
    let provider: Box<dyn ProviderTrait> = if let Some(provider) = registry_provider {
        provider
    } else if let Some(settings) =
        read_existing_provider_settings(&config_dir, &params.name).await?
    {
        provider_from_yaml(&params.name, settings)?
    } else if let Some(provider) = create_provider(&params.name) {
        provider
    } else {
        return Err(ScratchError::new(
            StatusCode::NOT_FOUND,
            format!("Provider '{}' not found", params.name),
        ));
    };

    let runtime = provider.build_runtime().ok();
    let has_creds = provider.has_credentials();
    let selected_count = provider.selected_model_count();
    let enabled = runtime.as_ref().map(|r| r.enabled).unwrap_or(false);
    let status = if has_creds && selected_count > 0 && enabled {
        "active"
    } else if has_creds {
        "configured"
    } else {
        "not_configured"
    };
    let response = ProviderDetailResponse {
        name: provider.name().to_string(),
        base_provider: provider.base_provider_name().to_string(),
        display_name: provider.display_name().to_string(),
        enabled,
        readonly: provider.is_readonly(),
        has_credentials: has_creds,
        selected_models_count: selected_count,
        status,
        settings: settings_with_identity(provider.as_ref()),
        runtime: runtime.map(|r| r.redacted()),
    };

    json_response(StatusCode::OK, &response)
}


#[derive(Serialize)]
pub(crate) struct ProviderSchemaResponse {
    pub(crate) name: String,
    pub(crate) schema: String,
}


pub async fn handle_v1_provider_schema(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let schema = {
        let registry = ctx.model.providers.read().await;

        if let Some(provider) = registry.get(&params.name) {
            provider.provider_schema().to_string()
        } else if let Some(default_provider) = create_provider(&params.name) {
            default_provider.provider_schema().to_string()
        } else {
            return Err(ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found", params.name),
            ));
        }
    };

    let response = ProviderSchemaResponse {
        name: params.name,
        schema,
    };

    json_response(StatusCode::OK, &response)
}


pub async fn handle_v1_provider_update(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    validate_instance_id_for_http(&params.name)?;
    let settings: serde_yaml::Value =
        if let Ok(json_val) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
            serde_yaml::to_value(json_val).map_err(|e| {
                ScratchError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("Failed to convert JSON to YAML: {}", e),
                )
            })?
        } else {
            serde_yaml::from_slice(&body_bytes).map_err(|e| {
                ScratchError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("Invalid JSON/YAML: {}", e),
                )
            })?
        };

    let (config_dir, registry_identity, registry_model_config) = {
        let registry = ctx.model.providers.read().await;
        let registry_provider = registry.get(&params.name);
        let registry_identity = if let Some(provider) = registry_provider {
            if provider.is_readonly() {
                return Err(ScratchError::new(
                    StatusCode::FORBIDDEN,
                    format!("Provider '{}' is readonly", params.name),
                ));
            }
            Some(HttpProviderIdentity {
                instance_id: provider.name().to_string(),
                base_provider: provider.base_provider_name().to_string(),
                display_name: provider.display_name().to_string(),
            })
        } else {
            None
        };
        let registry_model_config =
            registry_provider.map(ProviderModelConfigSnapshot::from_provider);
        (
            ctx.paths.config_dir.clone(),
            registry_identity,
            registry_model_config,
        )
    };

    let settings = strip_derived_fields(settings);
    let model_config_fields = ProviderModelConfigFields::from_settings(&settings);
    let identity_cell = std::sync::Arc::new(std::sync::Mutex::new(None));
    let identity_out = identity_cell.clone();
    let config_dir_for_update = config_dir.clone();
    let params_name = params.name.clone();
    config_store::update_provider_config_with(
        &config_dir_for_update,
        &params.name,
        provider_config_store_error,
        move |existing_settings| {
            let identity = resolve_provider_update_identity_from_existing(
                &params_name,
                registry_identity,
                &settings,
                existing_settings.as_ref(),
            )?;
            let settings = settings_with_forced_identity(settings, &identity)?;
            let had_existing = existing_settings.is_some();
            let existing = existing_settings
                .unwrap_or_else(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
            let merged_settings = if had_existing
                || supports_structured_command_credentials(&identity.base_provider)
            {
                merge_yaml_preserving_secrets_for_provider(
                    &identity.base_provider,
                    existing,
                    settings,
                )
                .map_err(|e| ScratchError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?
            } else {
                strip_masked_secrets(settings)
            };
            let mut merged_settings = settings_with_forced_identity(merged_settings, &identity)?;
            if let Some(snapshot) = registry_model_config.as_ref() {
                merged_settings = apply_model_config_snapshot_for_omitted_fields(
                    merged_settings,
                    snapshot,
                    model_config_fields,
                )?;
            }
            provider_from_yaml(&identity.instance_id, merged_settings.clone()).map_err(
                |error| {
                    ScratchError::new(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!("Invalid provider settings: {}", error.message),
                    )
                },
            )?;
            *identity_cell.lock().expect("identity lock poisoned") = Some(identity);
            Ok(merged_settings)
        },
    )
    .await?;
    let identity = identity_out
        .lock()
        .expect("identity lock poisoned")
        .clone()
        .ok_or_else(|| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Provider identity was not resolved".to_string(),
            )
        })?;

    reload_provider_from_disk(&ctx, &identity.instance_id, &config_dir).await?;
    refact_providers::credential::invalidate(&identity.instance_id);
    invalidate_provider_quota(&quota_cache, &identity.instance_id).await;

    invalidate_caps(&ctx).await;

    json_response(StatusCode::OK, &json!({"success": true}))
}


pub async fn handle_v1_provider_delete(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    validate_instance_id_for_http(&params.name)?;

    let registered_base_provider = {
        let registry = ctx.model.providers.read().await;
        if let Some(provider) = registry.get(&params.name) {
            if provider.is_readonly() {
                return Err(ScratchError::new(
                    StatusCode::FORBIDDEN,
                    format!("Provider '{}' is readonly", params.name),
                ));
            }
            Some(provider.base_provider_name().to_string())
        } else {
            None
        }
    };
    let config_dir = ctx.paths.config_dir.clone();
    let base_provider = if let Some(base_provider) = registered_base_provider {
        base_provider
    } else if create_provider(&params.name).is_some() {
        params.name.clone()
    } else {
        if !provider_file_exists(&config_dir, &params.name) {
            return Err(ScratchError::new(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found", params.name),
            ));
        }
        resolve_config_identity(&config_dir, &params.name)
            .await?
            .base_provider
    };

    let _claude_code_refresh_guard = if base_provider == "claude_code" {
        Some(
            ClaudeCodeProvider::lock_refresh_guard()
                .await
                .map_err(|e| ScratchError::new(StatusCode::CONFLICT, e))?,
        )
    } else {
        None
    };
    let _openai_codex_refresh_guard = if base_provider == "openai_codex" {
        Some(
            OpenAICodexProvider::lock_refresh_guard()
                .await
                .map_err(|e| ScratchError::new(StatusCode::CONFLICT, e))?,
        )
    } else {
        None
    };
    let _claude_code_oauth_file_guard = if base_provider == "claude_code" {
        Some(
            config_store::lock_provider_oauth_file(&config_dir, &params.name)
                .await
                .map_err(|error| ScratchError::new(StatusCode::CONFLICT, error))?,
        )
    } else {
        None
    };

    delete_provider_config(&config_dir, &params.name)
        .await
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    {
        let mut registry = ctx.model.providers.write().await;
        registry.remove(&params.name);
    }
    refact_providers::credential::invalidate(&params.name);
    invalidate_provider_quota(&quota_cache, &params.name).await;

    invalidate_caps(&ctx).await;

    json_response(StatusCode::OK, &json!({"success": true}))
}


#[derive(Serialize)]
pub(crate) struct ProviderModelsResponse {
    pub(crate) models: Vec<crate::providers::traits::ProviderModel>,
}


pub async fn handle_v1_provider_models(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let registry = ctx.model.providers.read().await;

    let provider: Box<dyn crate::providers::traits::ProviderTrait> =
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

    let runtime = provider
        .build_runtime()
        .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut models = runtime.chat_models;
    models.extend(runtime.completion_models);
    if let Some(emb) = runtime.embedding_model {
        models.push(emb);
    }

    let response = ProviderModelsResponse { models };

    json_response(StatusCode::OK, &response)
}
