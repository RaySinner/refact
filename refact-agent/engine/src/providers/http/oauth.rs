use super::*;

pub(crate) fn ensure_oauth_identity_for_instance_sync(
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


#[derive(Deserialize, Default)]
pub(crate) struct GitHubCopilotOAuthStartRequest {
    #[serde(default, alias = "enterpriseUrl")]
    pub(crate) enterprise_url: Option<String>,
    #[serde(default, alias = "deploymentType")]
    pub(crate) deployment_type: Option<String>,
}


pub(crate) async fn oauth_base_provider_for_instance(
    ctx: &ChatCtx,
    provider_name: &str,
) -> Result<String, ScratchError> {
    let identity = resolve_provider_identity(ctx, provider_name).await?;
    if !oauth_supported_for_base(&identity.base_provider) {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("OAuth not supported for provider '{}'", provider_name),
        ));
    }
    Ok(identity.base_provider)
}


pub async fn handle_v1_provider_oauth_start(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    validate_instance_id_for_http(&params.name)?;
    let base_provider = oauth_base_provider_for_instance(&ctx, &params.name).await?;
    match base_provider.as_str() {
        "claude_code" => {
            let mode = crate::providers::claude_code_oauth::OAuthMode::Max;
            let (callback_listener, callback_port) =
                crate::providers::claude_code_oauth::bind_callback_listener()
                    .await
                    .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            let (session_id, authorize_url) =
                crate::providers::claude_code_oauth::start_oauth_session(
                    mode,
                    callback_port,
                    params.name.clone(),
                )
                .await;

            let http_client = ctx.runtime.http_client.clone();
            let ctx_for_save = ctx.clone();
            let quota_cache_for_save = quota_cache.clone();
            let listener_handle = crate::providers::claude_code_oauth::start_callback_listener(
                callback_listener,
                http_client,
                move |tokens, provider_instance_id| {
                    let ctx_for_save = ctx_for_save.clone();
                    let quota_cache_for_save = quota_cache_for_save.clone();
                    async move {
                        let config_dir = ctx_for_save.paths.config_dir.clone();
                        let tokens_value = serde_yaml::to_value(&tokens)
                            .map_err(|e| format!("Failed to serialize tokens: {}", e))?;
                        save_provider_oauth_tokens(
                            &ctx_for_save,
                            &quota_cache_for_save,
                            &config_dir,
                            &provider_instance_id,
                            "claude_code",
                            &tokens_value,
                        )
                        .await
                        .map_err(|e| format!("{:?}", e))?;
                        tracing::info!(
                            "Claude Code: OAuth tokens saved successfully from callback listener"
                        );
                        Ok(())
                    }
                },
            )
            .await
            .map_err(|e| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            tokio::spawn(async move {
                let _ = listener_handle.await;
            });

            json_response(
                StatusCode::OK,
                &json!({
                    "session_id": session_id,
                    "authorize_url": authorize_url,
                    "mode": "callback",
                }),
            )
        }
        "openai_codex" => {
            let (session_id, authorize_url, listener) =
                crate::providers::openai_codex_oauth::start_oauth_session(params.name.clone())
                    .await
                    .map_err(|e| ScratchError::new(StatusCode::SERVICE_UNAVAILABLE, e))?;

            let listener_handle = crate::providers::openai_codex_oauth::start_callback_listener(
                listener,
                ctx.runtime.http_client.clone(),
            );
            let ctx_clone = ctx.clone();
            let quota_cache_clone = quota_cache.clone();
            tokio::spawn(async move {
                if let Some((tokens, provider_instance_id)) = listener_handle.await.ok().flatten() {
                    let config_dir = ctx_clone.paths.config_dir.clone();
                    if let Ok(tokens_value) = serde_yaml::to_value(&tokens) {
                        if let Err(e) = save_provider_oauth_tokens(
                            &ctx_clone,
                            &quota_cache_clone,
                            &config_dir,
                            &provider_instance_id,
                            "openai_codex",
                            &tokens_value,
                        )
                        .await
                        {
                            tracing::warn!("OpenAI Codex: failed to save OAuth tokens from callback listener: {:?}", e);
                        } else {
                            tracing::info!("OpenAI Codex: OAuth tokens saved successfully from callback listener");
                        }
                    }
                }
            });

            json_response(
                StatusCode::OK,
                &json!({
                    "session_id": session_id,
                    "authorize_url": authorize_url,
                }),
            )
        }
        "xai_oauth" => {
            let (session_id, authorize_url, listener) =
                crate::providers::xai_oauth_flow::start_oauth_session(params.name.clone())
                    .await
                    .map_err(|e| ScratchError::new(StatusCode::SERVICE_UNAVAILABLE, e))?;

            let listener_handle = crate::providers::xai_oauth_flow::start_callback_listener(
                listener,
                ctx.runtime.http_client.clone(),
            );
            let ctx_clone = ctx.clone();
            let quota_cache_clone = quota_cache.clone();
            tokio::spawn(async move {
                if let Some((tokens, provider_instance_id)) = listener_handle.await.ok().flatten() {
                    let config_dir = ctx_clone.paths.config_dir.clone();
                    if let Ok(tokens_value) = serde_yaml::to_value(&tokens) {
                        if let Err(e) = save_provider_oauth_tokens(
                            &ctx_clone,
                            &quota_cache_clone,
                            &config_dir,
                            &provider_instance_id,
                            "xai_oauth",
                            &tokens_value,
                        )
                        .await
                        {
                            tracing::warn!(
                                "xAI Grok: failed to save OAuth tokens from callback listener: {:?}",
                                e
                            );
                        } else {
                            tracing::info!(
                                "xAI Grok: OAuth tokens saved successfully from callback listener"
                            );
                        }
                    }
                }
            });

            json_response(
                StatusCode::OK,
                &json!({
                    "session_id": session_id,
                    "authorize_url": authorize_url,
                }),
            )
        }
        "google_antigravity" => {
            let (session_id, authorize_url, listener) =
                crate::providers::google_antigravity_oauth::start_oauth_session(
                    params.name.clone(),
                )
                .await
                .map_err(|e| ScratchError::new(StatusCode::SERVICE_UNAVAILABLE, e))?;

            let listener_handle =
                crate::providers::google_antigravity_oauth::start_callback_listener(
                    listener,
                    ctx.runtime.http_client.clone(),
                );
            let ctx_clone = ctx.clone();
            let quota_cache_clone = quota_cache.clone();
            tokio::spawn(async move {
                if let Some((tokens, provider_instance_id)) = listener_handle.await.ok().flatten() {
                    let config_dir = ctx_clone.paths.config_dir.clone();
                    if let Ok(tokens_value) = serde_yaml::to_value(&tokens) {
                        if let Err(e) = save_provider_oauth_tokens(
                            &ctx_clone,
                            &quota_cache_clone,
                            &config_dir,
                            &provider_instance_id,
                            "google_antigravity",
                            &tokens_value,
                        )
                        .await
                        {
                            tracing::warn!(
                                "Google Antigravity: failed to save OAuth tokens from callback listener: {:?}",
                                e
                            );
                        } else {
                            tracing::info!(
                                "Google Antigravity: OAuth tokens saved successfully from callback listener"
                            );
                        }
                    }
                }
            });

            json_response(
                StatusCode::OK,
                &json!({
                    "session_id": session_id,
                    "authorize_url": authorize_url,
                }),
            )
        }
        "github_copilot" => {
            let request = if body_bytes.is_empty() {
                GitHubCopilotOAuthStartRequest::default()
            } else {
                serde_json::from_slice::<GitHubCopilotOAuthStartRequest>(&body_bytes).map_err(
                    |e| {
                        ScratchError::new(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            format!("Invalid JSON: {e}"),
                        )
                    },
                )?
            };
            if request
                .deployment_type
                .as_deref()
                .is_some_and(|value| value == "enterprise")
                && request
                    .enterprise_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
            {
                return Err(ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    "GitHub Enterprise login requires enterprise_url".to_string(),
                ));
            }
            let http_client = ctx.runtime.http_client.clone();
            let start = crate::providers::github_copilot_oauth::start_oauth_session(
                &http_client,
                request.enterprise_url.as_deref(),
            )
            .await
            .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
            json_response(StatusCode::OK, &start)
        }
        _ => Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!("OAuth not supported for provider '{}'", params.name),
        )),
    }
}


#[derive(Deserialize)]
pub struct OAuthExchangeRequest {
    pub session_id: String,
    #[serde(default)]
    pub code: String,
}


pub async fn handle_v1_provider_oauth_exchange(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    let request: OAuthExchangeRequest = serde_json::from_slice(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid JSON: {}", e),
        )
    })?;

    validate_instance_id_for_http(&params.name)?;
    let base_provider = oauth_base_provider_for_instance(&ctx, &params.name).await?;
    let http_client = ctx.runtime.http_client.clone();
    let config_dir = ctx.paths.config_dir.clone();

    match base_provider.as_str() {
        "claude_code" => {
            let (tokens, session_provider_name) =
                crate::providers::claude_code_oauth::exchange_code(
                    &http_client,
                    &request.session_id,
                    &request.code,
                    &params.name,
                )
                .await
                .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
            if session_provider_name != params.name {
                return Err(ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "OAuth session belongs to provider '{}'",
                        session_provider_name
                    ),
                ));
            }

            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &session_provider_name,
                "claude_code",
                &serde_yaml::to_value(&tokens).map_err(|e| {
                    ScratchError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to serialize tokens: {}", e),
                    )
                })?,
            )
            .await?;
        }
        "openai_codex" => {
            let (tokens, session_provider_name) =
                crate::providers::openai_codex_oauth::exchange_code_for_session(
                    &http_client,
                    &request.session_id,
                    &request.code,
                )
                .await
                .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
            if session_provider_name != params.name {
                return Err(ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "OAuth session belongs to provider '{}'",
                        session_provider_name
                    ),
                ));
            }

            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &session_provider_name,
                "openai_codex",
                &serde_yaml::to_value(&tokens).map_err(|e| {
                    ScratchError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to serialize tokens: {}", e),
                    )
                })?,
            )
            .await?;
        }
        "xai_oauth" => {
            let (tokens, session_provider_name) =
                crate::providers::xai_oauth_flow::exchange_code_for_session(
                    &http_client,
                    &request.session_id,
                    &request.code,
                )
                .await
                .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
            if session_provider_name != params.name {
                return Err(ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "OAuth session belongs to provider '{}'",
                        session_provider_name
                    ),
                ));
            }

            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &session_provider_name,
                "xai_oauth",
                &serde_yaml::to_value(&tokens).map_err(|e| {
                    ScratchError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to serialize tokens: {}", e),
                    )
                })?,
            )
            .await?;
        }
        "google_antigravity" => {
            let (tokens, session_provider_name) =
                crate::providers::google_antigravity_oauth::exchange_code_for_session(
                    &http_client,
                    &request.session_id,
                    &request.code,
                )
                .await
                .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
            if session_provider_name != params.name {
                return Err(ScratchError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "OAuth session belongs to provider '{}'",
                        session_provider_name
                    ),
                ));
            }

            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &session_provider_name,
                "google_antigravity",
                &serde_yaml::to_value(&tokens).map_err(|e| {
                    ScratchError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to serialize tokens: {}", e),
                    )
                })?,
            )
            .await?;
        }
        "github_copilot" => {
            let outcome = crate::providers::github_copilot_oauth::poll_oauth_session(
                &http_client,
                &request.session_id,
            )
            .await
            .map_err(|e| ScratchError::new(StatusCode::BAD_REQUEST, e))?;
            match outcome {
                crate::providers::github_copilot_oauth::DevicePollOutcome::Success(tokens) => {
                    save_provider_oauth_tokens(
                        &ctx,
                        &quota_cache,
                        &config_dir,
                        &params.name,
                        "github_copilot",
                        &serde_yaml::to_value(&tokens).map_err(|e| {
                            ScratchError::new(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Failed to serialize tokens: {}", e),
                            )
                        })?,
                    )
                    .await?;
                }
                crate::providers::github_copilot_oauth::DevicePollOutcome::AuthorizationPending {
                    poll_interval,
                } => {
                    return json_response(
                        StatusCode::OK,
                        &json!({
                            "success": false,
                            "status": "authorization_pending",
                            "poll_interval": poll_interval,
                            "auth_status": "Waiting for GitHub device authorization",
                        }),
                    );
                }
                crate::providers::github_copilot_oauth::DevicePollOutcome::SlowDown {
                    poll_interval,
                } => {
                    return json_response(
                        StatusCode::OK,
                        &json!({
                            "success": false,
                            "status": "slow_down",
                            "poll_interval": poll_interval,
                            "auth_status": "GitHub requested a slower polling interval",
                        }),
                    );
                }
                crate::providers::github_copilot_oauth::DevicePollOutcome::ExpiredToken { message }
                | crate::providers::github_copilot_oauth::DevicePollOutcome::AccessDenied {
                    message,
                }
                | crate::providers::github_copilot_oauth::DevicePollOutcome::Error { message } => {
                    return Err(ScratchError::new(StatusCode::BAD_REQUEST, message));
                }
            }
        }
        _ => {
            return Err(ScratchError::new(
                StatusCode::BAD_REQUEST,
                format!("OAuth not supported for provider '{}'", params.name),
            ));
        }
    }

    json_response(
        StatusCode::OK,
        &json!({
            "success": true,
            "auth_status": "OK (OAuth login)",
        }),
    )
}


pub async fn handle_v1_provider_oauth_logout(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    validate_instance_id_for_http(&params.name)?;
    let base_provider = oauth_base_provider_for_instance(&ctx, &params.name).await?;
    let config_dir = ctx.paths.config_dir.clone();

    match base_provider.as_str() {
        "claude_code" => {
            let empty =
                serde_yaml::to_value(&crate::providers::claude_code_oauth::OAuthTokens::default())
                    .map_err(|e| {
                        ScratchError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to serialize: {}", e),
                        )
                    })?;
            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &params.name,
                "claude_code",
                &empty,
            )
            .await?;
        }
        "openai_codex" => {
            let empty =
                serde_yaml::to_value(&crate::providers::openai_codex_oauth::OAuthTokens::default())
                    .map_err(|e| {
                        ScratchError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to serialize: {}", e),
                        )
                    })?;
            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &params.name,
                "openai_codex",
                &empty,
            )
            .await?;
        }
        "xai_oauth" => {
            let empty =
                serde_yaml::to_value(&crate::providers::xai_oauth_flow::OAuthTokens::default())
                    .map_err(|e| {
                        ScratchError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to serialize: {}", e),
                        )
                    })?;
            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &params.name,
                "xai_oauth",
                &empty,
            )
            .await?;
        }
        "google_antigravity" => {
            let empty = serde_yaml::to_value(
                &crate::providers::google_antigravity_oauth::OAuthTokens::default(),
            )
            .map_err(|e| {
                ScratchError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to serialize: {}", e),
                )
            })?;
            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &params.name,
                "google_antigravity",
                &empty,
            )
            .await?;
        }
        "github_copilot" => {
            let empty = serde_yaml::to_value(
                &crate::providers::github_copilot_oauth::OAuthTokens::default(),
            )
            .map_err(|e| {
                ScratchError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to serialize: {}", e),
                )
            })?;
            save_provider_oauth_tokens(
                &ctx,
                &quota_cache,
                &config_dir,
                &params.name,
                "github_copilot",
                &empty,
            )
            .await?;
        }
        _ => {
            return Err(ScratchError::new(
                StatusCode::BAD_REQUEST,
                format!("OAuth not supported for provider '{}'", params.name),
            ));
        }
    }

    json_response(
        StatusCode::OK,
        &json!({
            "success": true,
            "auth_status": "No credentials found",
        }),
    )
}


#[derive(Deserialize)]
pub struct OAuthCallbackParams {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}
