use super::*;

pub(crate) async fn claude_code_usage_response(
    ctx: &ChatCtx,
    gcx: &Arc<GlobalContext>,
    provider_name: &str,
) -> Result<Response<Body>, ScratchError> {
    let result = fetch_claude_code_usage_with_refresh(ctx, gcx, provider_name).await;

    match result {
        Ok(usage) => json_response(StatusCode::OK, &json!({"data": usage})),
        Err(e) => json_response(StatusCode::OK, &json!({"error": e})),
    }
}


pub(crate) async fn fetch_claude_code_usage_with_refresh(
    ctx: &ChatCtx,
    gcx: &Arc<GlobalContext>,
    provider_name: &str,
) -> Result<crate::providers::claude_code::ClaudeCodeUsage, String> {
    use crate::providers::claude_code::ClaudeCodeUsageRequestError;

    let (provider, http_client) = resolve_provider_for_base(ctx, provider_name, "claude_code")
        .await
        .map_err(|error| error.message)?;
    let mut request_provider =
        downcast_provider::<ClaudeCodeProvider>(provider.as_ref(), "Claude Code")
            .map_err(|error| error.message)?
            .clone();

    let mut refresh_attempted = false;
    let access_token = match request_provider.resolve_auth() {
        Ok(access_token) => access_token,
        Err(_) if !request_provider.oauth_tokens.refresh_token.is_empty() => {
            refresh_attempted = true;
            let rejected_access_token = request_provider.oauth_tokens.access_token.clone();
            request_provider = crate::providers::oauth_refresh::force_refresh_claude_code_for_retry(
                gcx,
                &http_client,
                provider_name,
                &rejected_access_token,
                Some(request_provider.oauth_tokens.expires_at),
                None,
            )
            .await?
            .ok_or_else(|| {
                "Claude Code usage access token is expired and refresh returned no access token. Log in again in Claude Code provider settings."
                    .to_string()
            })?;
            request_provider.resolve_auth()?
        }
        Err(error) => return Err(error),
    };

    match request_provider
        .fetch_usage_once(&http_client, &access_token)
        .await
    {
        Ok(usage) => Ok(usage),
        Err(ClaudeCodeUsageRequestError::Status(status, _body))
            if ClaudeCodeProvider::should_force_refresh_for_status(
                status,
                &request_provider.oauth_tokens.refresh_token,
                refresh_attempted,
            ) =>
        {
            let retry_provider = crate::providers::oauth_refresh::force_refresh_claude_code_for_retry(
                gcx,
                &http_client,
                provider_name,
                &access_token,
                Some(request_provider.oauth_tokens.expires_at),
                Some(status),
            )
            .await?
            .ok_or_else(|| {
                "Claude Code usage API rejected the access token and refresh returned no access token. Log in again in Claude Code provider settings."
                    .to_string()
            })?;
            let retry_access_token = retry_provider.resolve_auth()?;
            retry_provider
                .fetch_usage_once(&http_client, &retry_access_token)
                .await
                .map_err(ClaudeCodeProvider::usage_request_error_to_string)
        }
        Err(error) => Err(ClaudeCodeProvider::usage_request_error_to_string(error)),
    }
}


pub(crate) async fn openai_codex_usage_response(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<Response<Body>, ScratchError> {
    let result = fetch_openai_codex_usage_with_refresh(ctx, provider_name).await;

    match result {
        Ok(usage) => json_response(StatusCode::OK, &json!({"data": usage})),
        Err(e) => json_response(StatusCode::OK, &json!({"error": e})),
    }
}


pub(crate) async fn opencode_usage_response(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<Response<Body>, ScratchError> {
    let (provider, http_client) =
        resolve_provider_for_base(ctx, provider_name, "opencode").await?;
    let opencode = downcast_provider::<OpenCodeProvider>(provider.as_ref(), "OpenCode")?;

    if opencode.usage_endpoint.trim().is_empty() {
        return json_response(StatusCode::OK, &json!({"data": null}));
    }

    match opencode.fetch_usage(&http_client).await {
        Ok(usage) => json_response(StatusCode::OK, &json!({"data": usage})),
        Err(e) => json_response(StatusCode::OK, &json!({"error": e})),
    }
}


pub(crate) async fn google_antigravity_usage_response(
    ctx: &ChatCtx,
    quota_cache: &Arc<AMutex<ProviderQuotaCache>>,
    provider_name: &str,
) -> Result<Response<Body>, ScratchError> {
    let result = fetch_google_antigravity_usage_with_refresh(ctx, quota_cache, provider_name).await;
    match result {
        Ok(usage) => json_response(StatusCode::OK, &json!({"data": usage})),
        Err(error) => json_response(StatusCode::OK, &json!({"error": error})),
    }
}


pub(crate) async fn fetch_google_antigravity_usage_with_refresh(
    ctx: &ChatCtx,
    quota_cache: &Arc<AMutex<ProviderQuotaCache>>,
    provider_name: &str,
) -> Result<crate::providers::google_antigravity::GoogleAntigravityQuotaSummary, String> {
    let (provider, http_client) =
        resolve_provider_for_base(ctx, provider_name, "google_antigravity")
            .await
            .map_err(|error| error.message)?;
    let request_provider =
        downcast_provider::<GoogleAntigravityProvider>(provider.as_ref(), "Google Antigravity")
            .map_err(|error| error.message)?
            .clone();
    match request_provider
        .fetch_quota_summary_once(&http_client, &request_provider.oauth_tokens.access_token)
        .await
    {
        Ok(usage) => Ok(usage),
        Err(GoogleAntigravityQuotaRequestError::Status(status, _))
            if matches!(
                status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            ) && request_provider.oauth_tokens.has_refresh_token() =>
        {
            let _guard = GoogleAntigravityProvider::lock_refresh_guard().await?;
            let (current_provider, _) =
                resolve_provider_for_base(ctx, provider_name, "google_antigravity")
                    .await
                    .map_err(|error| error.message)?;
            let current_provider = downcast_provider::<GoogleAntigravityProvider>(
                current_provider.as_ref(),
                "Google Antigravity",
            )
            .map_err(|error| error.message)?
            .clone();
            if current_provider.oauth_tokens.access_token
                != request_provider.oauth_tokens.access_token
            {
                return current_provider
                    .fetch_quota_summary(&http_client)
                    .await
                    .map_err(GoogleAntigravityProvider::quota_request_error_to_string);
            }
            if !current_provider.oauth_tokens.has_refresh_token() {
                return Err("Google Antigravity OAuth refresh token is missing".to_string());
            }
            let mut refreshed = crate::providers::google_antigravity_oauth::refresh_access_token(
                &http_client,
                &current_provider.oauth_tokens.refresh_token,
            )
            .await?;
            if refreshed.project_id.is_empty() {
                refreshed.project_id = current_provider.oauth_tokens.project_id.clone();
            }
            let config_dir = ctx.paths.config_dir.clone();
            let tokens_value = serde_yaml::to_value(&refreshed)
                .map_err(|error| format!("Failed to serialize refreshed OAuth tokens: {error}"))?;
            save_provider_oauth_tokens(
                ctx,
                quota_cache,
                &config_dir,
                provider_name,
                "google_antigravity",
                &tokens_value,
            )
            .await
            .map_err(|error| error.message)?;
            let mut retry_provider = current_provider;
            retry_provider.oauth_tokens = refreshed;
            retry_provider
                .fetch_quota_summary(&http_client)
                .await
                .map_err(GoogleAntigravityProvider::quota_request_error_to_string)
        }
        Err(error) => Err(GoogleAntigravityProvider::quota_request_error_to_string(
            error,
        )),
    }
}


pub(crate) async fn xai_oauth_usage_response(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<Response<Body>, ScratchError> {
    let (provider, _) = resolve_provider_for_base(ctx, provider_name, "xai_oauth").await?;
    let xai = downcast_provider::<XAIOAuthProvider>(provider.as_ref(), "xAI OAuth")?;
    match xai.fetch_usage().await {
        Ok(usage) => json_response(StatusCode::OK, &json!({"data": usage})),
        Err(error) => json_response(StatusCode::OK, &json!({"error": error})),
    }
}


pub async fn handle_v1_provider_quota(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    Query(query): Query<ProviderQuotaQuery>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    validate_instance_id_for_http(&params.name)?;
    let identity = resolve_provider_identity(&ctx, &params.name).await?;
    let snapshot = provider_quota_cached(
        &ctx,
        &app.gcx,
        &quota_cache,
        &identity.instance_id,
        &identity.base_provider,
        query.refresh,
    )
    .await;
    json_response(StatusCode::OK, &ProviderQuotaResponse { quota: snapshot })
}


pub async fn handle_v1_provider_quotas(
    State(app): State<AppState>,
    Query(query): Query<ProviderQuotaQuery>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    let providers = {
        let registry = ctx.model.providers.read().await;
        let mut providers = registry
            .iter()
            .filter(|(_, provider)| !provider.is_hidden_from_list())
            .map(|(name, provider)| (name.to_string(), provider.base_provider_name().to_string()))
            .collect::<Vec<_>>();
        providers.sort_by(|a, b| a.0.cmp(&b.0));
        providers
    };
    let mut snapshots = vec![None; providers.len()];
    let mut tasks = tokio::task::JoinSet::new();
    let mut next = 0;
    while next < providers.len() || !tasks.is_empty() {
        while next < providers.len() && tasks.len() < 8 {
            let (name, base_provider) = providers[next].clone();
            let ctx = ctx.clone();
            let gcx = app.gcx.clone();
            let quota_cache = quota_cache.clone();
            let refresh = query.refresh;
            let index = next;
            tasks.spawn(async move {
                (
                    index,
                    provider_quota_cached(&ctx, &gcx, &quota_cache, &name, &base_provider, refresh)
                        .await,
                )
            });
            next += 1;
        }
        if let Some(result) = tasks.join_next().await {
            let (index, snapshot) = result.map_err(|error| {
                ScratchError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Quota fetch task failed: {error}"),
                )
            })?;
            snapshots[index] = Some(snapshot);
        }
    }
    let snapshots = snapshots.into_iter().flatten().collect();
    json_response(
        StatusCode::OK,
        &ProviderQuotasResponse {
            quotas: snapshots,
            truncated: false,
        },
    )
}


pub(crate) async fn openai_codex_redeem_response(
    ctx: &ChatCtx,
    provider_name: &str,
    redeem_request_id: &str,
) -> Result<Response<Body>, ScratchError> {
    let result =
        redeem_openai_codex_reset_credit_with_refresh(ctx, provider_name, redeem_request_id).await;

    match result {
        Ok(redeem) => json_response(StatusCode::OK, &json!({"data": redeem})),
        Err(e) => json_response(StatusCode::OK, &json!({"error": e})),
    }
}


pub async fn handle_v1_provider_usage(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    validate_instance_id_for_http(&params.name)?;
    let identity = resolve_provider_identity(&ctx, &params.name).await?;
    if !usage_base_provider_supported(&identity.base_provider) {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Provider '{}' does not support usage", params.name),
        ));
    }
    match identity.base_provider.as_str() {
        "claude_code" => claude_code_usage_response(&ctx, &app.gcx, &params.name).await,
        "openai_codex" => openai_codex_usage_response(&ctx, &params.name).await,
        "opencode" => opencode_usage_response(&ctx, &params.name).await,
        "google_antigravity" => {
            google_antigravity_usage_response(&ctx, &quota_cache, &params.name).await
        }
        "xai_oauth" => xai_oauth_usage_response(&ctx, &params.name).await,
        _ => Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("Provider '{}' does not support usage", params.name),
        )),
    }
}


#[derive(serde::Deserialize, Default)]
pub(crate) struct ResetRedeemRequest {
    #[serde(default)]
    pub(crate) redeem_request_id: Option<String>,
}


pub async fn handle_v1_provider_usage_redeem(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    validate_instance_id_for_http(&params.name)?;
    let request: ResetRedeemRequest = if body_bytes.is_empty() {
        ResetRedeemRequest::default()
    } else {
        serde_json::from_slice(&body_bytes).map_err(|e| {
            ScratchError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Invalid JSON: {}", e),
            )
        })?
    };
    let identity = resolve_provider_identity(&ctx, &params.name).await?;
    if identity.base_provider != "openai_codex" {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "Provider '{}' does not support rate-limit reset redemption",
                params.name
            ),
        ));
    }
    let redeem_request_id = request
        .redeem_request_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    openai_codex_redeem_response(&ctx, &params.name, &redeem_request_id).await
}


pub(crate) async fn current_openai_codex_provider(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<(OpenAICodexProvider, reqwest::Client, std::path::PathBuf), String> {
    let (provider, http_client, config_dir) = {
        let registry = ctx.model.providers.read().await;
        (
            registry
                .get(provider_name)
                .map(|provider| provider.clone_box()),
            ctx.runtime.http_client.clone(),
            ctx.paths.config_dir.clone(),
        )
    };
    let provider = if let Some(provider) = provider {
        provider
    } else if provider_file_exists(&config_dir, provider_name) {
        let settings = read_existing_provider_settings(&config_dir, provider_name)
            .await
            .map_err(|e| e.message)?
            .ok_or_else(|| format!("OpenAI Codex provider '{}' is not available", provider_name))?;
        provider_from_yaml(provider_name, settings).map_err(|e| e.message)?
    } else if provider_name == "openai_codex" {
        create_provider("openai_codex")
            .ok_or_else(|| "OpenAI Codex provider is not available".to_string())?
    } else {
        return Err(format!(
            "OpenAI Codex provider '{}' is not available",
            provider_name
        ));
    };
    if provider.base_provider_name() != "openai_codex" {
        return Err(format!(
            "Provider '{}' is not an OpenAI Codex instance",
            provider_name
        ));
    }
    let Some(codex) = provider.as_any().downcast_ref::<OpenAICodexProvider>() else {
        return Err("Failed to resolve OpenAI Codex provider type".to_string());
    };
    Ok((codex.clone(), http_client, config_dir))
}


pub(crate) async fn force_refresh_openai_codex_usage_for_retry(
    ctx: &ChatCtx,
    http_client: &reqwest::Client,
    provider_name: &str,
    rejected_access_token: &str,
    rejected_status: Option<reqwest::StatusCode>,
) -> Result<Option<OpenAICodexProvider>, String> {
    let _guard = OpenAICodexProvider::lock_refresh_guard().await?;
    let (mut provider, _, config_dir) = current_openai_codex_provider(ctx, provider_name).await?;

    if provider
        .access_token_changed_since_rejection(rejected_access_token)
        .is_some()
    {
        return Ok(Some(provider));
    }

    if let Some(status) = rejected_status {
        if !OpenAICodexProvider::should_force_refresh_for_status(
            status,
            &provider.oauth_tokens.refresh_token,
            false,
        ) {
            return Ok(None);
        }
    } else if provider.oauth_tokens.refresh_token.is_empty() {
        return Ok(None);
    }

    let previous_tokens = provider.oauth_tokens.clone();
    let previous_session_id = provider.session_id.clone();
    let refresh_result = provider
        .force_refresh_after_auth_rejection(http_client, &config_dir, provider_name)
        .await;

    if !provider.auth_state_matches(&previous_tokens, &previous_session_id) {
        if sync_openai_codex_auth_state(
            ctx,
            provider_name,
            &provider,
            &previous_tokens,
            &previous_session_id,
        )
        .await
        .map_err(|e| e.message)?
        {
            invalidate_caps(ctx).await;
        }
    }

    refresh_result.map(|access_token| access_token.map(|_| provider))
}


pub(crate) async fn fetch_openai_codex_usage_with_refresh(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<crate::providers::openai_codex::OpenAICodexUsage, String> {
    let (mut request_provider, http_client, _) =
        current_openai_codex_provider(ctx, provider_name).await?;
    if !request_provider.oauth_tokens.has_refresh_token() {
        return request_provider.fetch_usage(&http_client).await;
    }

    let mut refresh_attempted = false;
    let context = match request_provider.resolve_wham_context() {
        Ok(context) => context,
        Err(_) if request_provider.oauth_tokens.has_refresh_token() => {
            refresh_attempted = true;
            let rejected_access_token = request_provider.oauth_tokens.access_token.clone();
            request_provider = force_refresh_openai_codex_usage_for_retry(
                ctx,
                &http_client,
                provider_name,
                &rejected_access_token,
                None,
            )
            .await?
            .ok_or_else(|| {
                "OpenAI Codex usage access token is expired and refresh returned no access token. Log in again in OpenAI Codex provider settings."
                    .to_string()
            })?;
            request_provider.resolve_wham_context()?
        }
        Err(error) => return Err(error),
    };

    match request_provider
        .fetch_usage_once(
            &http_client,
            &context.access_token,
            &context.chatgpt_account_id,
        )
        .await
    {
        Ok(usage) => Ok(usage),
        Err(UsageRequestError::Status(status, _body))
            if OpenAICodexProvider::should_force_refresh_for_status(
                status,
                &request_provider.oauth_tokens.refresh_token,
                refresh_attempted,
            ) =>
        {
            let Some(retry_provider) = force_refresh_openai_codex_usage_for_retry(
                ctx,
                &http_client,
                provider_name,
                &context.access_token,
                Some(status),
            )
            .await?
            else {
                return Err(
                    "OpenAI Codex usage API rejected the access token and refresh returned no access token. Log in again in OpenAI Codex provider settings."
                        .to_string(),
                );
            };
            let retry_context = retry_provider.resolve_wham_context()?;
            retry_provider
                .fetch_usage_once(
                    &http_client,
                    &retry_context.access_token,
                    &retry_context.chatgpt_account_id,
                )
                .await
                .map_err(|error| {
                    OpenAICodexProvider::usage_request_error_to_string(error, retry_context.source)
                })
        }
        Err(error) => Err(OpenAICodexProvider::usage_request_error_to_string(
            error,
            context.source,
        )),
    }
}


pub(crate) async fn redeem_openai_codex_reset_credit_with_refresh(
    ctx: &ChatCtx,
    provider_name: &str,
    redeem_request_id: &str,
) -> Result<crate::providers::openai_codex::OpenAICodexResetRedeem, String> {
    let (mut request_provider, http_client, _) =
        current_openai_codex_provider(ctx, provider_name).await?;
    if !request_provider.oauth_tokens.has_refresh_token() {
        return request_provider
            .redeem_reset_credit(&http_client, redeem_request_id)
            .await;
    }

    let mut refresh_attempted = false;
    let context = match request_provider.resolve_wham_context() {
        Ok(context) => context,
        Err(_) if request_provider.oauth_tokens.has_refresh_token() => {
            refresh_attempted = true;
            let rejected_access_token = request_provider.oauth_tokens.access_token.clone();
            request_provider = force_refresh_openai_codex_usage_for_retry(
                ctx,
                &http_client,
                provider_name,
                &rejected_access_token,
                None,
            )
            .await?
            .ok_or_else(|| {
                "OpenAI Codex reset access token is expired and refresh returned no access token. Log in again in OpenAI Codex provider settings."
                    .to_string()
            })?;
            request_provider.resolve_wham_context()?
        }
        Err(error) => return Err(error),
    };

    match request_provider
        .redeem_reset_credit_once(
            &http_client,
            &context.access_token,
            &context.chatgpt_account_id,
            redeem_request_id,
        )
        .await
    {
        Ok(redeem) => Ok(redeem),
        Err(UsageRequestError::Status(status, _body))
            if OpenAICodexProvider::should_force_refresh_for_status(
                status,
                &request_provider.oauth_tokens.refresh_token,
                refresh_attempted,
            ) =>
        {
            let Some(retry_provider) = force_refresh_openai_codex_usage_for_retry(
                ctx,
                &http_client,
                provider_name,
                &context.access_token,
                Some(status),
            )
            .await?
            else {
                return Err(
                    "OpenAI Codex reset API rejected the access token and refresh returned no access token. Log in again in OpenAI Codex provider settings."
                        .to_string(),
                );
            };
            let retry_context = retry_provider.resolve_wham_context()?;
            retry_provider
                .redeem_reset_credit_once(
                    &http_client,
                    &retry_context.access_token,
                    &retry_context.chatgpt_account_id,
                    redeem_request_id,
                )
                .await
                .map_err(|error| {
                    OpenAICodexProvider::usage_request_error_to_string(error, retry_context.source)
                })
        }
        Err(error) => Err(OpenAICodexProvider::usage_request_error_to_string(
            error,
            context.source,
        )),
    }
}


pub(crate) async fn sync_openai_codex_auth_state(
    ctx: &ChatCtx,
    provider_name: &str,
    source: &OpenAICodexProvider,
    previous_tokens: &crate::providers::openai_codex_oauth::OAuthTokens,
    previous_session_id: &str,
) -> Result<bool, ScratchError> {
    if source.auth_state_matches(previous_tokens, previous_session_id) {
        return Ok(false);
    }

    let mut registry = ctx.model.providers.write().await;
    let provider = registry.get_mut(provider_name).ok_or_else(|| {
        ScratchError::new(
            StatusCode::NOT_FOUND,
            format!("OpenAI Codex provider '{}' is not available", provider_name),
        )
    })?;
    let Some(current) = provider.as_any_mut().downcast_mut::<OpenAICodexProvider>() else {
        return Err(ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to resolve OpenAI Codex provider type".to_string(),
        ));
    };
    Ok(current.update_auth_state_from_if_current(source, previous_tokens, previous_session_id))
}


pub(crate) fn ensure_openai_codex_session_id(yaml_map: &mut serde_yaml::Mapping) -> String {
    let key = serde_yaml::Value::String("session_id".to_string());
    if let Some(session_id) = yaml_map
        .get(&key)
        .and_then(|value| value.as_str())
        .filter(|session_id| !session_id.is_empty())
    {
        return session_id.to_string();
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    yaml_map.insert(key, serde_yaml::Value::String(session_id.clone()));
    session_id
}


pub(crate) async fn save_provider_oauth_tokens(
    ctx: &ChatCtx,
    quota_cache: &Arc<AMutex<ProviderQuotaCache>>,
    config_dir: &std::path::Path,
    provider_name: &str,
    base_provider: &str,
    tokens_value: &serde_yaml::Value,
) -> Result<(), ScratchError> {
    validate_instance_id_for_http(provider_name)?;
    if !oauth_supported_for_base(base_provider) {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("OAuth not supported for provider '{}'", provider_name),
        ));
    }
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
            config_store::lock_provider_oauth_file(config_dir, provider_name)
                .await
                .map_err(|error| ScratchError::new(StatusCode::CONFLICT, error))?,
        )
    } else {
        None
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

            ensure_oauth_identity_for_instance_sync(
                config_dir,
                provider_name,
                base_provider,
                &mut yaml_map,
            )?;

            yaml_map.insert(
                serde_yaml::Value::String("oauth_tokens".to_string()),
                tokens_value.clone(),
            );

            if base_provider == "openai_codex" {
                // Subscription-only: never persist OPENAI_API_KEY at the provider level.
                yaml_map.remove(serde_yaml::Value::String("OPENAI_API_KEY".to_string()));
                ensure_openai_codex_session_id(&mut yaml_map);
            }

            Ok(serde_yaml::Value::Mapping(yaml_map))
        },
    )
    .await?;

    reload_provider_from_disk(ctx, provider_name, config_dir).await?;

    let refresh_token =
        match base_provider {
            "claude_code" => serde_yaml::from_value::<
                crate::providers::claude_code_oauth::OAuthTokens,
            >(tokens_value.clone())
            .ok()
            .map(|tokens| tokens.refresh_token),
            "openai_codex" => serde_yaml::from_value::<
                crate::providers::openai_codex_oauth::OAuthTokens,
            >(tokens_value.clone())
            .ok()
            .map(|tokens| tokens.refresh_token),
            "xai_oauth" => serde_yaml::from_value::<crate::providers::xai_oauth_flow::OAuthTokens>(
                tokens_value.clone(),
            )
            .ok()
            .map(|tokens| tokens.refresh_token),
            "google_antigravity" => serde_yaml::from_value::<
                crate::providers::google_antigravity_oauth::OAuthTokens,
            >(tokens_value.clone())
            .ok()
            .map(|tokens| tokens.refresh_token),
            _ => None,
        };
    if let Some(refresh_token) = refresh_token {
        crate::providers::oauth_refresh::clear_invalid_refresh_token(provider_name, &refresh_token);
    }

    invalidate_provider_quota(quota_cache, provider_name).await;
    invalidate_caps(ctx).await;
    Ok(())
}
