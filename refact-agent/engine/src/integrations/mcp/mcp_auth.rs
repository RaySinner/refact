pub use refact_integrations::mcp::mcp_auth::*;

use std::sync::Arc;

use tokio::sync::Mutex as AMutex;
use tracing::warn;

use crate::integrations::sessions::IntegrationSession;


pub async fn mcp_oauth_refresh_task(
    session_arc: Arc<AMutex<Box<dyn IntegrationSession>>>,
    config_path: String,
) {
    use super::session_mcp::{SessionMCP, MCPAuthStatus};

    let mut retry_delay_secs = REFRESH_RETRY_BASE_SECS;
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;

        let auth_manager_arc = {
            let mut session_locked = session_arc.lock().await;
            let mcp_session = match session_locked.as_any_mut().downcast_mut::<SessionMCP>() {
                Some(s) => s,
                None => return,
            };
            mcp_session.auth_manager.clone()
        };

        let auth_manager_arc = match auth_manager_arc {
            Some(am) => am,
            // auth_manager not yet set (session still starting up); wait for next cycle
            None => continue,
        };

        let tokens = match load_tokens_from_config(&config_path).await {
            Some(t) if !t.access_token.is_empty() => t,
            _ => {
                retry_delay_secs = REFRESH_RETRY_BASE_SECS;
                warn!("OAuth refresh task: no tokens in config {}", config_path);
                let mut session_locked = session_arc.lock().await;
                if let Some(mcp_session) = session_locked.as_any_mut().downcast_mut::<SessionMCP>()
                {
                    mcp_session.auth_status = MCPAuthStatus::NeedsLogin;
                }
                // No tokens on disk — user must re-authenticate; keep looping in case
                // tokens appear later (e.g., user completes OAuth flow in another tab).
                continue;
            }
        };

        if !needs_refresh(&tokens) {
            continue;
        }

        if tokens.refresh_token.is_empty() {
            warn!(
                "OAuth refresh task: no refresh token for {}; full re-authentication is required",
                config_path
            );
            let mut session_locked = session_arc.lock().await;
            if let Some(mcp_session) = session_locked.as_any_mut().downcast_mut::<SessionMCP>() {
                mcp_session.auth_status = MCPAuthStatus::NeedsReauth;
            }
            return;
        }

        {
            let mut session_locked = session_arc.lock().await;
            if let Some(mcp_session) = session_locked.as_any_mut().downcast_mut::<SessionMCP>() {
                mcp_session.auth_status = MCPAuthStatus::Refreshing;
            }
        }

        let refresh_result = {
            let am = auth_manager_arc.lock().await;
            am.refresh_token().await
        };

        match refresh_result {
            Ok(token_response) => {
                retry_delay_secs = REFRESH_RETRY_BASE_SECS;
                let new_tokens = tokens_from_response(
                    tokens.client_id.clone(),
                    &tokens.refresh_token,
                    &token_response,
                    &tokens.scopes,
                    tokens.bound_url.clone(),
                );
                if let Err(e) = save_tokens_to_config(&config_path, &new_tokens).await {
                    warn!(
                        "OAuth refresh task: failed to persist tokens for {}: {}",
                        config_path, e
                    );
                }
                let mut session_locked = session_arc.lock().await;
                if let Some(mcp_session) = session_locked.as_any_mut().downcast_mut::<SessionMCP>()
                {
                    mcp_session.auth_status = MCPAuthStatus::Authenticated;
                }
            }
            Err(e) => {
                let permanent = is_permanent_oauth_error(&e.to_string());
                if permanent {
                    warn!(
                        "MCP OAuth refresh failed permanently for {}; re-authentication is required: {}",
                        config_path, e
                    );
                } else {
                    warn!("MCP OAuth refresh failed for {}: {}", config_path, e);
                }
                let next_status = if permanent {
                    MCPAuthStatus::NeedsReauth
                } else if !is_token_expired(&tokens) {
                    MCPAuthStatus::Authenticated
                } else {
                    MCPAuthStatus::Error(e.to_string())
                };
                {
                    let mut session_locked = session_arc.lock().await;
                    if let Some(mcp_session) =
                        session_locked.as_any_mut().downcast_mut::<SessionMCP>()
                    {
                        mcp_session.auth_status = next_status;
                    }
                }
                if permanent {
                    return;
                }
                retry_delay_secs = (retry_delay_secs * 2).min(REFRESH_RETRY_MAX_SECS);
            }
        }
    }
}
