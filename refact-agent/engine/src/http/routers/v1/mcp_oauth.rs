pub use refact_http::routers::v1::mcp_oauth::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::NamedTempFile;
    use crate::integrations::mcp::mcp_auth::{
        MCPOAuthTokens, save_tokens_to_config, load_tokens_from_config, clear_tokens_from_config,
    };

    #[test]
    fn test_html_escape_script_tags() {
        let result = html_escape("<script>alert('xss')</script>");
        assert_eq!(
            result,
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
        );
        assert!(!result.contains('<'));
        assert!(!result.contains('>'));
        assert!(!result.contains('\''));
    }

    #[test]
    fn test_html_response_contains_csp_header() {
        let response = html_response("Title", "Heading", "#4ade80", "Message").unwrap();
        let csp = response.headers().get("Content-Security-Policy").unwrap();
        let csp_str = csp.to_str().unwrap();
        assert!(csp_str.contains("default-src 'none'"));
        assert!(csp_str.contains("style-src 'unsafe-inline'"));
    }

    #[test]
    fn test_config_path_traversal_rejected() {
        assert!(reject_path_traversal("../../etc/passwd").is_err());
        assert!(reject_path_traversal("/tmp/../etc/passwd").is_err());
        assert!(reject_path_traversal("foo/../bar").is_err());
        assert!(reject_path_traversal("/safe/path/config.yaml").is_ok());
        assert!(reject_path_traversal(
            "/home/user/.config/refact/integrations.d/mcp_http_myserver.yaml"
        )
        .is_ok());
    }

    #[tokio::test]
    async fn test_oauth_start_fails_gracefully_when_server_unreachable() {
        use crate::integrations::mcp::mcp_auth::MCPOAuthSessionManager;
        let result = MCPOAuthSessionManager::start_oauth_flow(
            "http://127.0.0.1:1",
            "/tmp/test_mcp_oauth.yaml",
            &[],
            "http://127.0.0.1:8001/v1/mcp/oauth/callback",
        )
        .await;
        assert!(
            result.is_err(),
            "start_oauth_flow should fail when server is unreachable"
        );
        let err = result.unwrap_err();
        assert!(!err.is_empty(), "error message should not be empty");
    }

    #[tokio::test]
    async fn test_exchange_code_rejects_unknown_session_id() {
        use crate::integrations::mcp::mcp_auth::MCPOAuthSessionManager;
        let result =
            MCPOAuthSessionManager::exchange_code("unknown-session-id-12345", "some_code").await;
        assert!(result.is_err(), "exchange with unknown session should fail");
        assert!(
            result.unwrap_err().contains("No pending OAuth session"),
            "should say session not found"
        );
    }

    #[tokio::test]
    async fn test_expired_sessions_are_rejected() {
        use crate::integrations::mcp::mcp_auth::MCPOAuthSessionManager;
        MCPOAuthSessionManager::cleanup_expired_sessions().await;
        let result = MCPOAuthSessionManager::exchange_code("nonexistent-session-xyz", "code").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No pending OAuth session"));
    }

    #[tokio::test]
    async fn test_logout_clears_tokens_from_config() {
        let mut tmp = NamedTempFile::new().unwrap();
        let existing = "url: https://example.com\nauth_type: oauth2_pkce\n";
        tmp.write_all(existing.as_bytes()).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tokens = MCPOAuthTokens {
            access_token: "tok".to_string(),
            refresh_token: "ref".to_string(),
            expires_at: 9999999999000,
            client_id: "cid".to_string(),
            client_secret: None,
            scopes: vec!["read".to_string()],
            bound_url: None,
        };
        save_tokens_to_config(&path, &tokens).await.unwrap();
        assert!(load_tokens_from_config(&path).await.is_some());

        clear_tokens_from_config(&path).await.unwrap();
        assert!(
            load_tokens_from_config(&path).await.is_none(),
            "tokens should be cleared"
        );
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            content.contains("url: https://example.com"),
            "other fields preserved"
        );
    }

    #[tokio::test]
    async fn test_status_returns_authenticated_when_valid_token() {
        let mut tmp = NamedTempFile::new().unwrap();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let future_expiry = now_ms + 3_600_000;
        let yaml = format!(
            "url: https://example.com\nauth_type: oauth2_pkce\noauth_tokens:\n  access_token: live_token\n  refresh_token: ref\n  expires_at: {}\n  client_id: cid\n  scopes:\n    - read\n",
            future_expiry
        );
        tmp.write_all(yaml.as_bytes()).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tokens = load_tokens_from_config(&path).await;
        assert!(tokens.is_some());
        let t = tokens.unwrap();
        assert_eq!(t.access_token, "live_token");
        let authenticated =
            !t.access_token.is_empty() && (t.expires_at == 0 || t.expires_at > now_ms);
        assert!(authenticated, "token should be valid");
    }

    #[tokio::test]
    async fn test_status_returns_not_authenticated_when_expired() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let past_expiry = now_ms - 1_000;
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = format!(
            "auth_type: oauth2_pkce\noauth_tokens:\n  access_token: expired_token\n  refresh_token: ref\n  expires_at: {}\n  client_id: cid\n  scopes: []\n",
            past_expiry
        );
        tmp.write_all(yaml.as_bytes()).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tokens = load_tokens_from_config(&path).await;
        assert!(tokens.is_some());
        let t = tokens.unwrap();
        let authenticated =
            !t.access_token.is_empty() && (t.expires_at == 0 || t.expires_at > now_ms);
        assert!(!authenticated, "expired token should not be authenticated");
    }
}
