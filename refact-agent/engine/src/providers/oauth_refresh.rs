pub use refact_runtime_api::providers_oauth_refresh::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claude_code_refresh_adopts_tokens_written_by_another_process() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("claude_code.yaml"),
            "oauth_tokens:\n  access_token: fresh-access\n  refresh_token: fresh-refresh\n  expires_at: 9223372036854775807\n",
        )
        .await
        .unwrap();
        let current = crate::providers::claude_code::ClaudeCodeProvider {
            oauth_tokens: crate::providers::claude_code_oauth::OAuthTokens {
                access_token: "stale-access".to_string(),
                refresh_token: "stale-refresh".to_string(),
                expires_at: 1,
            },
            ..Default::default()
        };
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(current));
        }
        let http_client = gcx.http_client.clone();

        let refreshed = super::force_refresh_claude_code_for_retry(
            &gcx,
            &http_client,
            "claude_code",
            "stale-access",
            None,
            Some(reqwest::StatusCode::UNAUTHORIZED),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(refreshed.oauth_tokens.access_token, "fresh-access");
        let registry = gcx.providers.read().await;
        let provider = registry.get("claude_code").unwrap();
        let provider = provider
            .as_any()
            .downcast_ref::<crate::providers::claude_code::ClaudeCodeProvider>()
            .unwrap();
        assert_eq!(provider.oauth_tokens.refresh_token, "fresh-refresh");
    }

    #[tokio::test]
    async fn claude_code_proactive_refresh_skips_already_renewed_expiry() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let current = crate::providers::claude_code::ClaudeCodeProvider {
            oauth_tokens: crate::providers::claude_code_oauth::OAuthTokens {
                access_token: "same-access".to_string(),
                refresh_token: "refresh".to_string(),
                expires_at: i64::MAX,
            },
            ..Default::default()
        };
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(current));
        }
        let http_client = gcx.http_client.clone();

        let refreshed = super::force_refresh_claude_code_for_retry(
            &gcx,
            &http_client,
            "claude_code",
            "same-access",
            Some(1),
            None,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(refreshed.oauth_tokens.access_token, "same-access");
        assert_eq!(refreshed.oauth_tokens.expires_at, i64::MAX);
    }

    #[tokio::test]
    async fn claude_code_refreshed_tokens_preserve_provider_identity() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("claude-code-work.yaml"),
            "base_provider: claude_code\ndisplay_name: Work Claude\nenabled_models:\n  - claude-sonnet-4-6\noauth_tokens:\n  access_token: old\n  refresh_token: old-refresh\n  expires_at: 1\n",
        )
        .await
        .unwrap();
        let expected_tokens = crate::providers::claude_code_oauth::OAuthTokens {
            access_token: "old".to_string(),
            refresh_token: "old-refresh".to_string(),
            expires_at: 1,
        };
        let provider = crate::providers::claude_code::ClaudeCodeProvider {
            enabled: true,
            enabled_models: vec!["claude-sonnet-4-6".to_string()],
            oauth_tokens: expected_tokens.clone(),
            ..Default::default()
        };
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(crate::providers::instance::ProviderInstance::new(
                "claude-code-work",
                "claude_code",
                "Work Claude",
                Box::new(provider),
            )));
        }
        let refreshed_tokens = crate::providers::claude_code_oauth::OAuthTokens {
            access_token: "new".to_string(),
            refresh_token: "new-refresh".to_string(),
            expires_at: i64::MAX,
        };

        let saved = super::save_claude_code_refreshed_tokens(
            &gcx,
            &config_dir,
            "claude-code-work",
            &expected_tokens,
            &refreshed_tokens,
        )
        .await
        .unwrap();

        assert!(saved);
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            &tokio::fs::read_to_string(providers_dir.join("claude-code-work.yaml"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(yaml["base_provider"].as_str(), Some("claude_code"));
        assert_eq!(yaml["display_name"].as_str(), Some("Work Claude"));
        assert_eq!(
            yaml["enabled_models"][0].as_str(),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(yaml["oauth_tokens"]["access_token"].as_str(), Some("new"));
    }

    #[tokio::test]
    async fn claude_code_stale_refresh_does_not_overwrite_newer_login() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("claude-code-work.yaml"),
            "base_provider: claude_code\ndisplay_name: Work Claude\noauth_tokens:\n  access_token: login-access\n  refresh_token: login-refresh\n  expires_at: 99\n",
        )
        .await
        .unwrap();
        let stale_tokens = crate::providers::claude_code_oauth::OAuthTokens {
            access_token: "old-access".to_string(),
            refresh_token: "old-refresh".to_string(),
            expires_at: 1,
        };
        let provider = crate::providers::claude_code::ClaudeCodeProvider {
            oauth_tokens: crate::providers::claude_code_oauth::OAuthTokens {
                access_token: "login-access".to_string(),
                refresh_token: "login-refresh".to_string(),
                expires_at: 99,
            },
            ..Default::default()
        };
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(crate::providers::instance::ProviderInstance::new(
                "claude-code-work",
                "claude_code",
                "Work Claude",
                Box::new(provider),
            )));
        }
        let stale_refresh = crate::providers::claude_code_oauth::OAuthTokens {
            access_token: "stale-access".to_string(),
            refresh_token: "stale-refresh".to_string(),
            expires_at: i64::MAX,
        };

        let saved = super::save_claude_code_refreshed_tokens(
            &gcx,
            &config_dir,
            "claude-code-work",
            &stale_tokens,
            &stale_refresh,
        )
        .await
        .unwrap();

        assert!(!saved);
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            &tokio::fs::read_to_string(providers_dir.join("claude-code-work.yaml"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            yaml["oauth_tokens"]["access_token"].as_str(),
            Some("login-access")
        );
        let registry = gcx.providers.read().await;
        let provider = registry.get("claude-code-work").unwrap();
        let provider = provider
            .as_any()
            .downcast_ref::<crate::providers::claude_code::ClaudeCodeProvider>()
            .unwrap();
        assert_eq!(provider.oauth_tokens.access_token, "login-access");
    }

    #[tokio::test]
    async fn refreshed_tokens_preserve_existing_provider_identity() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("codex-prod.yaml"),
            "base_provider: openai_codex\ndisplay_name: Renamed Codex\noauth_tokens:\n  access_token: old\n  refresh_token: old-refresh\n  expires_at: 1\n",
        )
        .await
        .unwrap();
        let mut provider = crate::providers::openai_codex::OpenAICodexProvider::default();
        provider.oauth_tokens.access_token = "old".to_string();
        provider.oauth_tokens.refresh_token = "old-refresh".to_string();
        provider.oauth_tokens.expires_at = 1;
        let expected_tokens = provider.oauth_tokens.clone();
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(crate::providers::instance::ProviderInstance::new(
                "codex-prod",
                "openai_codex",
                "Renamed Codex",
                Box::new(provider),
            )));
        }

        let saved = super::save_refreshed_tokens(
            &gcx,
            &config_dir,
            "codex-prod",
            &expected_tokens,
            "new",
            "new-refresh",
            i64::MAX,
        )
        .await
        .unwrap();
        assert!(saved);

        let yaml: serde_yaml::Value = serde_yaml::from_str(
            &tokio::fs::read_to_string(providers_dir.join("codex-prod.yaml"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(yaml["base_provider"].as_str(), Some("openai_codex"));
        assert_eq!(yaml["display_name"].as_str(), Some("Renamed Codex"));
        assert_eq!(yaml["oauth_tokens"]["access_token"].as_str(), Some("new"));
    }

    #[tokio::test]
    async fn stale_refreshed_tokens_do_not_overwrite_newer_login() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("codex-prod.yaml"),
            "base_provider: openai_codex\ndisplay_name: Codex\noauth_tokens:\n  access_token: login-access\n  refresh_token: login-refresh\n  expires_at: 99\n",
        )
        .await
        .unwrap();
        let stale_tokens = crate::providers::openai_codex_oauth::OAuthTokens {
            access_token: "old-access".to_string(),
            refresh_token: "old-refresh".to_string(),
            expires_at: 1,
            ..Default::default()
        };
        let mut provider = crate::providers::openai_codex::OpenAICodexProvider::default();
        provider.oauth_tokens = crate::providers::openai_codex_oauth::OAuthTokens {
            access_token: "login-access".to_string(),
            refresh_token: "login-refresh".to_string(),
            expires_at: 99,
            ..Default::default()
        };
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(crate::providers::instance::ProviderInstance::new(
                "codex-prod",
                "openai_codex",
                "Codex",
                Box::new(provider),
            )));
        }

        let saved = super::save_refreshed_tokens(
            &gcx,
            &config_dir,
            "codex-prod",
            &stale_tokens,
            "stale-access",
            "stale-refresh",
            i64::MAX,
        )
        .await
        .unwrap();

        assert!(!saved);
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            &tokio::fs::read_to_string(providers_dir.join("codex-prod.yaml"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            yaml["oauth_tokens"]["access_token"].as_str(),
            Some("login-access")
        );
    }

    #[tokio::test]
    async fn stale_refreshed_tokens_do_not_recreate_deleted_provider() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let stale_tokens = crate::providers::openai_codex_oauth::OAuthTokens {
            access_token: "old-access".to_string(),
            refresh_token: "old-refresh".to_string(),
            expires_at: 1,
            ..Default::default()
        };

        let saved = super::save_refreshed_tokens(
            &gcx,
            &config_dir,
            "codex-prod",
            &stale_tokens,
            "stale-access",
            "stale-refresh",
            i64::MAX,
        )
        .await
        .unwrap();

        assert!(!saved);
        assert!(!config_dir.join("providers.d/codex-prod.yaml").exists());
        assert!(!gcx.providers.read().await.has_instance("codex-prod"));
    }
}
