use super::*;

pub(crate) fn html_response(
    title: &str,
    heading: &str,
    heading_color: &str,
    message: &str,
) -> Result<Response<Body>, ScratchError> {
    let html = format!(
        r#"<!DOCTYPE html>
<html><head><title>{title}</title></head>
<body style="font-family: system-ui; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #1a1a2e; color: #e0e0e0;">
<div style="text-align: center;">
<h1 style="color: {heading_color};">{heading}</h1>
<p>{message}</p>
</div>
</body></html>"#,
        title = html_escape(title),
        heading = html_escape(heading),
        heading_color = heading_color,
        message = html_escape(message),
    );

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html; charset=utf-8")
        .header(
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'",
        )
        .body(Body::from(html))
        .map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Response build failed: {}", e),
            )
        })
}


pub(crate) async fn handle_openai_codex_oauth_callback_impl(
    ctx: &ChatCtx,
    quota_cache: &Arc<AMutex<ProviderQuotaCache>>,
    query: OAuthCallbackParams,
    expected_provider_name: Option<String>,
) -> Result<Response<Body>, ScratchError> {
    if let Some(err) = &query.error {
        let desc = query
            .error_description
            .as_deref()
            .unwrap_or("Unknown error");
        tracing::warn!("OpenAI OAuth error: {} — {}", err, desc);
        return html_response(
            "Authentication Failed",
            "✗ Authentication Failed",
            "#ef4444",
            &format!("{}: {}", err, desc),
        );
    }

    let code = match &query.code {
        Some(c) if !c.is_empty() => c.clone(),
        _ => {
            return html_response(
                "Authentication Failed",
                "✗ Authentication Failed",
                "#ef4444",
                "No authorization code received. Please try again.",
            );
        }
    };

    let session_id = match &query.state {
        Some(s) if !s.is_empty() => s.clone(),
        _ => {
            return html_response(
                "Authentication Failed",
                "✗ Authentication Failed",
                "#ef4444",
                "Missing state parameter. Please start the OAuth flow again.",
            );
        }
    };

    let http_client = ctx.runtime.http_client.clone();
    let config_dir = ctx.paths.config_dir.clone();

    let (tokens, provider_instance_id) =
        match crate::providers::openai_codex_oauth::exchange_code_for_session(
            &http_client,
            &session_id,
            &code,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("OpenAI OAuth exchange failed: {}", e);
                return html_response(
                    "Authentication Failed",
                    "✗ Authentication Failed",
                    "#ef4444",
                    &format!("Token exchange failed: {}", e),
                );
            }
        };

    if let Some(expected_provider_name) = expected_provider_name {
        if provider_instance_id != expected_provider_name {
            return html_response(
                "Authentication Failed",
                "✗ Authentication Failed",
                "#ef4444",
                &format!(
                    "OAuth session belongs to provider '{}'. Please restart login.",
                    provider_instance_id
                ),
            );
        }
    }

    if let Err(e) = save_provider_oauth_tokens(
        ctx,
        quota_cache,
        &config_dir,
        &provider_instance_id,
        "openai_codex",
        &serde_yaml::to_value(&tokens).map_err(|e| {
            ScratchError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to serialize tokens: {}", e),
            )
        })?,
    )
    .await
    {
        tracing::warn!("Failed to save OAuth tokens: {:?}", e);
        return html_response(
            "Authentication Failed",
            "✗ Authentication Failed",
            "#ef4444",
            "Tokens received but failed to save. Please try again.",
        );
    }

    html_response(
        "Authentication Successful",
        "✓ Authentication Successful",
        "#4ade80",
        "You can close this window and return to the application.",
    )
}


pub async fn handle_v1_provider_oauth_callback(
    State(app): State<AppState>,
    Path(params): Path<ProviderPathParams>,
    Query(query): Query<OAuthCallbackParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    validate_instance_id_for_http(&params.name)?;
    let base_provider = oauth_base_provider_for_instance(&ctx, &params.name).await?;
    if base_provider != "openai_codex" {
        return Err(ScratchError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "OAuth callback not supported for provider '{}'",
                params.name
            ),
        ));
    }
    handle_openai_codex_oauth_callback_impl(&ctx, &quota_cache, query, Some(params.name)).await
}


pub async fn handle_openai_codex_auth_callback(
    State(app): State<AppState>,
    Query(query): Query<OAuthCallbackParams>,
) -> Result<Response<Body>, ScratchError> {
    let ctx = app.chat_ctx();
    let quota_cache = app.gcx.quota_cache.clone();
    handle_openai_codex_oauth_callback_impl(&ctx, &quota_cache, query, None).await
}
