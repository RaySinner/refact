use super::*;

    #[test]
    fn quota_envelopes_and_items_serialize_for_gui_contract() {
        let snapshot = quota_snapshot_from_details(
            "codex-work",
            "openai_codex",
            ProviderQuotaSource::OpenaiCodex,
            json!({
                "rate_limit": {"primary_window": {"limit": 100.0}},
                "credits": {"balance": 12.5, "opaque": {"ignored": true}}
            }),
        );
        let single = serde_json::to_value(ProviderQuotaResponse {
            quota: snapshot.clone(),
        })
        .unwrap();
        let multiple = serde_json::to_value(ProviderQuotasResponse {
            quotas: vec![snapshot],
            truncated: false,
        })
        .unwrap();

        assert_eq!(single.as_object().unwrap().len(), 1);
        assert!(single.get("quota").is_some());
        assert_eq!(multiple.as_object().unwrap().len(), 2);
        assert!(multiple.get("quotas").is_some());
        assert_eq!(multiple["truncated"], false);
        let quota = &single["quota"];
        for field in [
            "provider_name",
            "base_provider",
            "source",
            "available",
            "fetched_at",
            "stale",
            "windows",
            "facts",
        ] {
            assert!(
                quota.get(field).is_some(),
                "missing required quota field {field}"
            );
        }
        let window = &quota["windows"][0];
        assert!(window.get("id").is_some());
        assert!(window.get("label").is_some());
        assert!(window.get("used_percent").is_some());
        let fact = &quota["facts"][0];
        assert!(fact.get("id").is_some());
        assert!(fact.get("label").is_some());
        assert!(fact["value"].is_number());
        assert!(quota["facts"].as_array().unwrap().iter().all(|fact| {
            let value = &fact["value"];
            value.is_null() || value.is_boolean() || value.is_number() || value.is_string()
        }));
        assert!(quota_fact("opaque", Some(&json!({"ignored": true}))).is_none());
        assert!(quota.get("details").is_none());
        assert!(quota.get("account_id").is_none());
    }

    #[test]
    fn stale_fallback_preserves_prior_timestamp_and_skips_unavailable() {
        let mut prior = ProviderQuotaSnapshot::unavailable("codex-work", "openai_codex");
        prior.available = true;
        prior.source = ProviderQuotaSource::OpenaiCodex;
        prior.fetched_at = "old".to_string();
        let fetched = ProviderQuotaSnapshot::error(
            "codex-work",
            "openai_codex",
            ProviderQuotaSource::OpenaiCodex,
            "upstream failed",
        );
        let fallback = stale_quota_fallback(&fetched, Some(prior)).unwrap();
        assert!(fallback.stale);
        assert_eq!(fallback.fetched_at, "old");
        assert_eq!(fallback.error.as_deref(), Some("upstream failed"));

        let unavailable = ProviderQuotaSnapshot::unavailable("other", "other");
        assert!(stale_quota_fallback(&unavailable, Some(fallback)).is_none());
    }

    async fn response_json(response: Response<Body>) -> serde_json::Value {
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn provider_config_json(
        config_dir: &std::path::Path,
        provider_name: &str,
    ) -> serde_json::Value {
        let content = tokio::fs::read_to_string(provider_file_path(&config_dir, provider_name))
            .await
            .unwrap();
        serde_json::to_value(serde_yaml::from_str::<serde_yaml::Value>(&content).unwrap()).unwrap()
    }

    fn merged_settings_json(
        provider_name: &str,
        existing: &str,
        incoming: &str,
    ) -> serde_json::Value {
        let existing = serde_yaml::from_str(existing).unwrap();
        let incoming = serde_yaml::from_str(incoming).unwrap();
        serde_json::to_value(
            merge_yaml_preserving_secrets_for_provider(provider_name, existing, incoming).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn usage_dispatch_supports_subscription_oauth_providers() {
        assert!(usage_base_provider_supported("google_antigravity"));
        assert!(usage_base_provider_supported("xai_oauth"));
        assert!(!usage_base_provider_supported("openai"));
    }

    #[test]
    fn custom_provider_merge_replaces_extra_headers_map() {
        let merged = merged_settings_json(
            "custom",
            r#"
api_key: sk-old
extra_headers:
  X-Keep: keep-secret
  X-Replace: old-value
  X-Remove-Null: old-null
  X-Remove-Number: old-number
  X-Absent: old-absent
"#,
            r#"
api_key: "***"
extra_headers:
  X-Keep: "***"
  X-Replace: new-value
  X-Remove-Null:
  X-Remove-Number: 7
"#,
        );

        assert_eq!(merged["api_key"], "sk-old");
        assert_eq!(merged["extra_headers"]["X-Keep"], "keep-secret");
        assert_eq!(merged["extra_headers"]["X-Replace"], "new-value");
        assert!(merged["extra_headers"].get("X-Remove-Null").is_none());
        assert!(merged["extra_headers"].get("X-Remove-Number").is_none());
        assert!(merged["extra_headers"].get("X-Absent").is_none());
    }

    #[test]
    fn custom_provider_merge_empty_extra_headers_clears_all() {
        let merged = merged_settings_json(
            "custom",
            r#"
extra_headers:
  X-Secret: old-secret
"#,
            r#"
extra_headers: {}
"#,
        );

        assert!(merged["extra_headers"].as_object().unwrap().is_empty());
    }

    #[test]
    fn custom_provider_merge_null_extra_headers_clears_all() {
        let merged = merged_settings_json(
            "custom",
            r#"
extra_headers:
  X-Secret: old-secret
"#,
            r#"
extra_headers:
"#,
        );

        assert!(merged["extra_headers"].as_object().unwrap().is_empty());
    }

    #[test]
    fn custom_provider_merge_yaml_string_extra_headers() {
        let merged = merged_settings_json(
            "custom",
            r#"
extra_headers:
  X-Keep: keep-secret
  X-Absent: old-absent
"#,
            r#"
extra_headers: |
  X-Keep: "***"
  X-New: new-value
  X-Remove-Number: 7
"#,
        );

        assert_eq!(merged["extra_headers"]["X-Keep"], "keep-secret");
        assert_eq!(merged["extra_headers"]["X-New"], "new-value");
        assert!(merged["extra_headers"].get("X-Remove-Number").is_none());
        assert!(merged["extra_headers"].get("X-Absent").is_none());
    }

    #[test]
    fn custom_provider_merge_json_string_extra_headers() {
        let merged = merged_settings_json(
            "custom",
            r#"
extra_headers:
  X-Keep: keep-secret
  X-Absent: old-absent
"#,
            r#"
extra_headers: '{"X-Keep":"***","X-Json":"json-value","X-Remove":7}'
"#,
        );

        assert_eq!(merged["extra_headers"]["X-Keep"], "keep-secret");
        assert_eq!(merged["extra_headers"]["X-Json"], "json-value");
        assert!(merged["extra_headers"].get("X-Remove").is_none());
        assert!(merged["extra_headers"].get("X-Absent").is_none());
    }

    #[test]
    fn custom_provider_merge_invalid_string_extra_headers_errors() {
        let existing = serde_yaml::from_str(
            r#"
extra_headers:
  X-Secret: old-secret
"#,
        )
        .unwrap();
        let incoming = serde_yaml::from_str("extra_headers: '['").unwrap();
        let err =
            merge_yaml_preserving_secrets_for_provider("custom", existing, incoming).unwrap_err();

        assert!(err.contains("extra_headers"));
    }

    #[test]
    fn custom_provider_merge_absent_extra_headers_preserves_existing() {
        let merged = merged_settings_json(
            "custom",
            r#"
api_key: sk-old
extra_headers:
  X-Secret: old-secret
"#,
            r#"
api_key: "***"
"#,
        );

        assert_eq!(merged["api_key"], "sk-old");
        assert_eq!(merged["extra_headers"]["X-Secret"], "old-secret");
    }

    #[test]
    fn custom_provider_merge_replaces_credential_object() {
        let merged = merged_settings_json(
            "custom",
            r#"
credential:
  type: command
  command: old-helper
  args: [token]
  cwd: /old/path
  env_passthrough: [PROFILE]
"#,
            r#"
credential:
  type: command
  command: new-helper
  args: [token]
"#,
        );

        assert_eq!(merged["credential"]["command"], "new-helper");
        assert!(merged["credential"].get("cwd").is_none());
        assert!(merged["credential"].get("env_passthrough").is_none());
    }

    #[test]
    fn litellm_provider_merge_preserves_omitted_secrets() {
        let merged = merged_settings_json(
            "litellm",
            r#"
api_key: sk-old
extra_headers:
  Authorization: Bearer old-secret
enabled: true
"#,
            r#"
enabled: false
"#,
        );

        assert_eq!(merged["api_key"], "sk-old");
        assert_eq!(
            merged["extra_headers"]["Authorization"],
            "Bearer old-secret"
        );
        assert_eq!(merged["enabled"], false);
    }

    #[test]
    fn litellm_provider_merge_preserves_omitted_command_credential() {
        let merged = merged_settings_json(
            "litellm",
            r#"
credential:
  type: command
  command: old-helper
  args: [token]
enabled: true
"#,
            r#"
enabled: false
"#,
        );

        assert_eq!(merged["credential"]["command"], "old-helper");
        assert_eq!(merged["credential"]["args"], json!(["token"]));
        assert_eq!(merged["enabled"], false);
    }

    #[test]
    fn litellm_provider_merge_replaces_credential_object() {
        let merged = merged_settings_json(
            "litellm",
            r#"
credential:
  type: command
  command: old-helper
  args: [token]
  cwd: /old/path
  env_passthrough: [PROFILE]
"#,
            r#"
credential:
  type: command
  command: new-helper
  args: [token]
"#,
        );

        assert_eq!(merged["credential"]["command"], "new-helper");
        assert!(merged["credential"].get("cwd").is_none());
        assert!(merged["credential"].get("env_passthrough").is_none());
    }

    #[test]
    fn litellm_provider_merge_replaces_extra_headers_with_mask_preservation() {
        let merged = merged_settings_json(
            "litellm",
            r#"
extra_headers:
  X-Keep: keep-secret
  X-Replace: old-value
  X-Remove: remove-me
"#,
            r#"
extra_headers:
  X-Keep: "***"
  X-Replace: new-value
"#,
        );

        assert_eq!(merged["extra_headers"]["X-Keep"], "keep-secret");
        assert_eq!(merged["extra_headers"]["X-Replace"], "new-value");
        assert!(merged["extra_headers"].get("X-Remove").is_none());
    }

    #[test]
    fn unrelated_provider_retains_recursive_merge_behavior() {
        let merged = merged_settings_json(
            "openai_codex",
            r#"
oauth_tokens:
  access_token: old-access
  refresh_token: old-refresh
"#,
            r#"
oauth_tokens:
  access_token: new-access
"#,
        );

        assert_eq!(merged["oauth_tokens"]["access_token"], "new-access");
        assert_eq!(merged["oauth_tokens"]["refresh_token"], "old-refresh");
    }

    #[tokio::test]
    async fn provider_update_openai_alias_writes_instance_file_with_identity() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let response = handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
            hyper::body::Bytes::from(
                serde_json::to_vec(&json!({
                    "base_provider": "openai",
                    "display_name": "Work OpenAI",
                    "api_key": "sk-two",
                    "enabled": true,
                    "enabled_models": ["gpt-4.1"]
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert!(provider_file_exists(&config_dir, "openai_2"));
        assert!(!provider_file_exists(&config_dir, "openai"));
        let saved = provider_config_json(&config_dir, "openai_2").await;
        assert_eq!(saved["base_provider"], "openai");
        assert_eq!(saved["display_name"], "Work OpenAI");
        assert_eq!(saved["api_key"], "sk-two");

        let (name, base, display, enabled_models) = {
            let registry = gcx.providers.read().await;
            let provider = registry.get("openai_2").unwrap();
            (
                provider.name().to_string(),
                provider.base_provider_name().to_string(),
                provider.display_name().to_string(),
                provider.enabled_models().to_vec(),
            )
        };
        assert_eq!(name, "openai_2");
        assert_eq!(base, "openai");
        assert_eq!(display, "Work OpenAI");
        assert_eq!(enabled_models, vec!["gpt-4.1".to_string()]);
    }

    #[tokio::test]
    async fn provider_get_openai_alias_returns_identity_fields() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
            hyper::body::Bytes::from(
                serde_json::to_vec(&json!({
                    "base_provider": "openai",
                    "display_name": "Work OpenAI",
                    "api_key": "sk-two"
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap();

        let response = handle_v1_provider_get(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;

        assert_eq!(body["name"], "openai_2");
        assert_eq!(body["base_provider"], "openai");
        assert_eq!(body["display_name"], "Work OpenAI");
        assert_eq!(body["settings"]["base_provider"], "openai");
        assert_eq!(body["settings"]["display_name"], "Work OpenAI");
    }

    #[tokio::test]
    async fn provider_get_adopts_claude_tokens_refreshed_by_another_worker() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("claude_code.yaml"),
            "oauth_tokens:\n  access_token: fresh-access\n  refresh_token: fresh-refresh\n  expires_at: 9223372036854775807\nenabled_models:\n  - claude-sonnet-4-6\n",
        )
        .await
        .unwrap();
        let provider = ClaudeCodeProvider {
            enabled: true,
            enabled_models: vec!["claude-sonnet-4-6".to_string()],
            oauth_tokens: crate::providers::claude_code_oauth::OAuthTokens {
                access_token: "stale-access".to_string(),
                refresh_token: "stale-refresh".to_string(),
                expires_at: 1,
            },
            ..Default::default()
        };
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(provider));
        }

        let response = handle_v1_provider_get(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "claude_code".to_string(),
            }),
        )
        .await
        .unwrap();
        let body = response_json(response).await;

        assert_eq!(body["settings"]["oauth_connected"], true);
        assert_eq!(body["settings"]["auth_status"], "OK (OAuth login)");
        let registry = gcx.providers.read().await;
        let provider = registry.get("claude_code").unwrap();
        let provider = provider
            .as_any()
            .downcast_ref::<ClaudeCodeProvider>()
            .unwrap();
        assert_eq!(provider.oauth_tokens.access_token, "fresh-access");
    }

    #[tokio::test]
    async fn provider_model_toggle_updates_only_alias_file_and_preserves_identity() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("openai.yaml"),
            "base_provider: openai\ndisplay_name: Main OpenAI\napi_key: sk-main\nenabled_models:\n  - gpt-4.1\n",
        )
        .await
        .unwrap();
        reload_provider_from_disk(&gcx.chat_ctx(gcx.clone()), "openai", &config_dir)
            .await
            .unwrap();
        handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
            hyper::body::Bytes::from(
                serde_json::to_vec(&json!({
                    "base_provider": "openai",
                    "display_name": "Work OpenAI",
                    "api_key": "sk-two",
                    "enabled_models": ["gpt-4.1-mini"]
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap();

        let response = handle_v1_provider_model_toggle(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
            hyper::body::Bytes::from(r#"{"model_id":"gpt-4.1","enabled":true}"#),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let main = provider_config_json(&config_dir, "openai").await;
        let alias = provider_config_json(&config_dir, "openai_2").await;
        assert_eq!(main["enabled_models"], json!(["gpt-4.1"]));
        assert_eq!(alias["base_provider"], "openai");
        assert_eq!(alias["display_name"], "Work OpenAI");
        assert_eq!(alias["enabled_models"], json!(["gpt-4.1-mini", "gpt-4.1"]));
    }

    #[tokio::test]
    async fn custom_alias_update_preserves_masked_api_key_and_replaces_extra_headers() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("custom_2.yaml"),
            r#"
base_provider: custom
display_name: Work Custom
api_key: sk-old
chat_endpoint: https://example.com/v1/chat/completions
enabled: true
enabled_models:
  - custom-model
custom_models:
  custom-model:
    n_ctx: 4096
extra_headers:
  X-Keep: keep-secret
  X-Replace: old-value
  X-Remove: remove-me
"#,
        )
        .await
        .unwrap();

        let response = handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "custom_2".to_string(),
            }),
            hyper::body::Bytes::from(
                serde_json::to_vec(&json!({
                    "base_provider": "custom",
                    "display_name": "Work Custom",
                    "api_key": "***",
                    "extra_headers": {
                        "X-Keep": "***",
                        "X-Replace": "new-value"
                    }
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let saved = provider_config_json(&config_dir, "custom_2").await;
        assert_eq!(saved["base_provider"], "custom");
        assert_eq!(saved["display_name"], "Work Custom");
        assert_eq!(saved["api_key"], "sk-old");
        assert_eq!(saved["extra_headers"]["X-Keep"], "keep-secret");
        assert_eq!(saved["extra_headers"]["X-Replace"], "new-value");
        assert!(saved["extra_headers"].get("X-Remove").is_none());

        let runtime = {
            let registry = gcx.providers.read().await;
            registry.get("custom_2").unwrap().build_runtime().unwrap()
        };
        assert_eq!(runtime.name, "custom_2");
        assert_eq!(runtime.display_name, "Work Custom");
        assert_eq!(runtime.api_key, "sk-old");
        assert_eq!(
            runtime.extra_headers.get("X-Keep").map(String::as_str),
            Some("keep-secret")
        );
        assert_eq!(
            runtime.extra_headers.get("X-Replace").map(String::as_str),
            Some("new-value")
        );
        assert!(runtime.extra_headers.get("X-Remove").is_none());
    }

    #[tokio::test]
    async fn litellm_update_preserves_masked_api_key_and_replaces_extra_headers() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("litellm.yaml"),
            r#"
base_provider: litellm
display_name: LiteLLM
endpoint: https://gateway.example/v1
api_key: sk-old
enabled: false
extra_headers:
  X-Keep: keep-secret
  X-Replace: old-value
  X-Remove: remove-me
"#,
        )
        .await
        .unwrap();

        let response = handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "litellm".to_string(),
            }),
            hyper::body::Bytes::from(
                serde_json::to_vec(&json!({
                    "api_key": "***",
                    "extra_headers": {
                        "X-Keep": "***",
                        "X-Replace": "new-value"
                    }
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let saved = provider_config_json(&config_dir, "litellm").await;
        assert_eq!(saved["api_key"], "sk-old");
        assert_eq!(saved["extra_headers"]["X-Keep"], "keep-secret");
        assert_eq!(saved["extra_headers"]["X-Replace"], "new-value");
        assert!(saved["extra_headers"].get("X-Remove").is_none());

        let runtime = {
            let registry = gcx.providers.read().await;
            registry.get("litellm").unwrap().build_runtime().unwrap()
        };
        assert_eq!(runtime.api_key, "sk-old");
        assert_eq!(
            runtime.extra_headers.get("X-Keep").map(String::as_str),
            Some("keep-secret")
        );
        assert_eq!(
            runtime.extra_headers.get("X-Replace").map(String::as_str),
            Some("new-value")
        );
        assert!(runtime.extra_headers.get("X-Remove").is_none());
    }

    #[tokio::test]
    async fn provider_delete_instance_removes_only_that_instance() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("openai.yaml"),
            "base_provider: openai\ndisplay_name: Main OpenAI\napi_key: sk-main\n",
        )
        .await
        .unwrap();
        reload_provider_from_disk(&gcx.chat_ctx(gcx.clone()), "openai", &config_dir)
            .await
            .unwrap();
        handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
            hyper::body::Bytes::from(
                serde_json::to_vec(&json!({
                    "base_provider": "openai",
                    "display_name": "Work OpenAI",
                    "api_key": "sk-two"
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap();

        let response = handle_v1_provider_delete(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert!(provider_file_exists(&config_dir, "openai"));
        assert!(!provider_file_exists(&config_dir, "openai_2"));
        let main = provider_config_json(&config_dir, "openai").await;
        assert_eq!(main["api_key"], "sk-main");
        let (has_main, has_alias) = {
            let registry = gcx.providers.read().await;
            (
                registry.get("openai").is_some(),
                registry.get("openai_2").is_some(),
            )
        };
        assert!(has_main);
        assert!(!has_alias);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn claude_code_delete_waits_for_refresh_guard() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("claude_code_2.yaml"),
            "base_provider: claude_code\ndisplay_name: Work Claude\noauth_tokens:\n  access_token: access\n  refresh_token: refresh\n  expires_at: 9223372036854775807\n",
        )
        .await
        .unwrap();
        reload_provider_from_disk(&gcx.chat_ctx(gcx.clone()), "claude_code_2", &config_dir)
            .await
            .unwrap();
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let guard = ClaudeCodeProvider::lock_refresh_guard().await.unwrap();

        let delete = tokio::spawn(handle_v1_provider_delete(
            axum::extract::State(app),
            Path(ProviderPathParams {
                name: "claude_code_2".to_string(),
            }),
        ));
        tokio::task::yield_now().await;
        assert!(!delete.is_finished());
        drop(guard);

        let response = tokio::time::timeout(std::time::Duration::from_secs(2), delete)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!provider_file_exists(&config_dir, "claude_code_2"));
        assert!(gcx.providers.read().await.get("claude_code_2").is_none());
    }

    #[tokio::test]
    async fn custom_provider_update_removes_deleted_extra_headers_on_disk_and_runtime() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("custom.yaml"),
            r#"
api_key: sk-old
chat_endpoint: https://example.com/v1/chat/completions
enabled: true
enabled_models:
  - custom-model
custom_models:
  custom-model:
    n_ctx: 4096
extra_headers:
  X-Keep: keep-secret
  X-Replace: old-value
  X-Remove: remove-me
"#,
        )
        .await
        .unwrap();

        let body = serde_json::to_vec(&json!({
            "api_key": "***",
            "extra_headers": {
                "X-Keep": "***",
                "X-Replace": "new-value"
            }
        }))
        .unwrap();
        let response = handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "custom".to_string(),
            }),
            hyper::body::Bytes::from(body),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let content = tokio::fs::read_to_string(providers_dir.join("custom.yaml"))
            .await
            .unwrap();
        let saved: serde_json::Value =
            serde_json::to_value(serde_yaml::from_str::<serde_yaml::Value>(&content).unwrap())
                .unwrap();
        assert_eq!(saved["api_key"], "sk-old");
        assert_eq!(saved["extra_headers"]["X-Keep"], "keep-secret");
        assert_eq!(saved["extra_headers"]["X-Replace"], "new-value");
        assert!(saved["extra_headers"].get("X-Remove").is_none());

        let runtime = {
            let registry = gcx.providers.read().await;
            registry.get("custom").unwrap().build_runtime().unwrap()
        };
        assert_eq!(
            runtime.extra_headers.get("X-Keep").map(String::as_str),
            Some("keep-secret")
        );
        assert_eq!(
            runtime.extra_headers.get("X-Replace").map(String::as_str),
            Some("new-value")
        );
        assert!(runtime.extra_headers.get("X-Remove").is_none());
    }

    #[tokio::test]
    async fn custom_provider_update_invalid_extra_headers_string_returns_422_and_preserves_file() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        let config_path = providers_dir.join("custom.yaml");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            &config_path,
            r#"
api_key: sk-old
chat_endpoint: https://example.com/v1/chat/completions
enabled: true
enabled_models:
  - custom-model
extra_headers:
  X-Secret: keep-secret
"#,
        )
        .await
        .unwrap();

        let body = serde_json::to_vec(&json!({
            "extra_headers": "["
        }))
        .unwrap();
        let err = handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "custom".to_string(),
            }),
            hyper::body::Bytes::from(body),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status_code, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(err.message.contains("extra_headers"));

        let content = tokio::fs::read_to_string(config_path).await.unwrap();
        let saved: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(
            saved
                .get("extra_headers")
                .and_then(|headers| headers.get("X-Secret"))
                .and_then(|value| value.as_str()),
            Some("keep-secret")
        );
    }

    #[tokio::test]
    async fn openai_codex_oauth_callback_html_escapes_interpolated_values() {
        let response = html_response(
            "<script>title</script>",
            "Heading & \"quoted\"",
            "#ef4444",
            "<script>alert('xss')</script> & \"quote\"",
        )
        .unwrap();
        assert_eq!(
            response
                .headers()
                .get("Content-Type")
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();

        assert!(html.contains("&lt;script&gt;title&lt;/script&gt;"));
        assert!(html.contains("Heading &amp; &quot;quoted&quot;"));
        assert!(html.contains("&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"));
        assert!(html.contains("&amp; &quot;quote&quot;"));
        assert!(!html.contains("<script>alert"));
    }

    #[test]
    fn openai_codex_session_id_is_created_and_preserved() {
        let mut yaml_map = serde_yaml::Mapping::new();
        let created = ensure_openai_codex_session_id(&mut yaml_map);
        assert!(!created.is_empty());
        assert_eq!(
            yaml_map
                .get(&serde_yaml::Value::String("session_id".to_string()))
                .and_then(|value| value.as_str()),
            Some(created.as_str())
        );

        yaml_map.insert(
            serde_yaml::Value::String("session_id".to_string()),
            serde_yaml::Value::String("existing-session".to_string()),
        );
        let preserved = ensure_openai_codex_session_id(&mut yaml_map);
        assert_eq!(preserved, "existing-session");
    }

    #[tokio::test]
    async fn openai_codex_oauth_logout_removes_top_level_api_key() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        // Pre-existing YAML may have stale top-level OPENAI_API_KEY from older versions.
        tokio::fs::write(
            providers_dir.join("openai_codex.yaml"),
            "OPENAI_API_KEY: sk-stale\noauth_tokens:\n  access_token: old\n",
        )
        .await
        .unwrap();
        let empty =
            serde_yaml::to_value(&crate::providers::openai_codex_oauth::OAuthTokens::default())
                .unwrap();

        save_provider_oauth_tokens(&gcx.chat_ctx(gcx.clone()), &gcx.quota_cache, &config_dir, "openai_codex", "openai_codex", &empty)
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(providers_dir.join("openai_codex.yaml"))
            .await
            .unwrap();
        let yaml: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        // Subscription-only: top-level OPENAI_API_KEY must never persist.
        assert!(yaml.get("OPENAI_API_KEY").is_none());
        // Saved oauth_tokens must not carry an openai_api_key field anymore.
        assert!(yaml
            .get("oauth_tokens")
            .and_then(|tokens| tokens.get("openai_api_key"))
            .is_none());
    }
    #[tokio::test]
    async fn github_copilot_oauth_save_writes_redactable_yaml() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let tokens = crate::providers::github_copilot_oauth::OAuthTokens {
            access_token: "gho-secret".to_string(),
            expires_at: 0,
            enterprise_url: Some("company.ghe.com".to_string()),
            api_base: Some("https://copilot-api.company.ghe.com".to_string()),
        };
        let tokens_value = serde_yaml::to_value(&tokens).unwrap();

        save_provider_oauth_tokens(
            &gcx.chat_ctx(gcx.clone()),
            &gcx.quota_cache,
            &config_dir,
            "github_copilot",
            "github_copilot",
            &tokens_value,
        )
        .await
        .unwrap();

        let content = tokio::fs::read_to_string(provider_file_path(&config_dir, "github_copilot"))
            .await
            .unwrap();
        let yaml: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(
            yaml.get("oauth_tokens")
                .and_then(|tokens| tokens.get("access_token"))
                .and_then(|value| value.as_str()),
            Some("gho-secret")
        );
        assert_eq!(
            yaml.get("oauth_tokens")
                .and_then(|tokens| tokens.get("api_base"))
                .and_then(|value| value.as_str()),
            Some("https://copilot-api.company.ghe.com")
        );

        let stored = {
            let registry = gcx.providers.read().await;
            registry
                .get("github_copilot")
                .unwrap()
                .provider_settings_as_json()
        };
        assert_eq!(stored["oauth_tokens"]["access_token"], "***");
        assert!(!stored.to_string().contains("gho-secret"));
    }

    #[tokio::test]
    async fn github_copilot_oauth_logout_clears_token_fields() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("github_copilot.yaml"),
            "oauth_tokens:\n  access_token: gho-secret\n  expires_at: 0\n  api_base: https://api.githubcopilot.com\n",
        )
        .await
        .unwrap();
        let empty =
            serde_yaml::to_value(&crate::providers::github_copilot_oauth::OAuthTokens::default())
                .unwrap();

        save_provider_oauth_tokens(
            &gcx.chat_ctx(gcx.clone()),
            &gcx.quota_cache,
            &config_dir,
            "github_copilot",
            "github_copilot",
            &empty,
        )
        .await
        .unwrap();

        let content = tokio::fs::read_to_string(providers_dir.join("github_copilot.yaml"))
            .await
            .unwrap();
        let yaml: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        assert_eq!(
            yaml.get("oauth_tokens")
                .and_then(|tokens| tokens.get("access_token"))
                .and_then(|value| value.as_str()),
            Some("")
        );
        assert_eq!(
            yaml.get("oauth_tokens")
                .and_then(|tokens| tokens.get("expires_at"))
                .and_then(|value| value.as_i64()),
            Some(0)
        );
    }

    #[tokio::test]
    async fn save_oauth_tokens_writes_alias_config_and_preserves_identity() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("openai_codex_2.yaml"),
            "base_provider: openai_codex\ndisplay_name: Work Codex\noauth_tokens:\n  access_token: old\n",
        )
        .await
        .unwrap();
        let tokens = crate::providers::openai_codex_oauth::OAuthTokens {
            access_token: "alias-access".to_string(),
            refresh_token: "alias-refresh".to_string(),
            expires_at: i64::MAX,
            ..Default::default()
        };
        crate::providers::oauth_refresh::mark_invalid_refresh_token(
            "openai_codex_2",
            "alias-refresh",
        );
        assert!(crate::providers::oauth_refresh::is_invalid_refresh_token(
            "openai_codex_2",
            "alias-refresh"
        ));

        save_provider_oauth_tokens(
            &gcx.chat_ctx(gcx.clone()),
            &gcx.quota_cache,
            &config_dir,
            "openai_codex_2",
            "openai_codex",
            &serde_yaml::to_value(&tokens).unwrap(),
        )
        .await
        .unwrap();
        assert!(!crate::providers::oauth_refresh::is_invalid_refresh_token(
            "openai_codex_2",
            "alias-refresh"
        ));

        assert!(provider_file_exists(&config_dir, "openai_codex_2"));
        assert!(!provider_file_exists(&config_dir, "openai_codex"));
        let saved = provider_config_json(&config_dir, "openai_codex_2").await;
        assert_eq!(saved["base_provider"], "openai_codex");
        assert_eq!(saved["display_name"], "Work Codex");
        // Subscription-only: top-level OPENAI_API_KEY must never persist.
        assert!(saved.get("OPENAI_API_KEY").is_none());
        assert_eq!(saved["oauth_tokens"]["access_token"], "alias-access");
        let identity = {
            let registry = gcx.providers.read().await;
            let provider = registry.get("openai_codex_2").unwrap();
            (
                provider.name().to_string(),
                provider.base_provider_name().to_string(),
                provider.display_name().to_string(),
            )
        };
        assert_eq!(
            identity,
            (
                "openai_codex_2".to_string(),
                "openai_codex".to_string(),
                "Work Codex".to_string()
            )
        );
    }

    #[tokio::test]
    async fn claude_code_oauth_save_clears_invalid_refresh_suppression() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("claude_code_2.yaml"),
            "base_provider: claude_code\ndisplay_name: Work Claude\noauth_tokens:\n  access_token: old\n",
        )
        .await
        .unwrap();
        let tokens = crate::providers::claude_code_oauth::OAuthTokens {
            access_token: "alias-access".to_string(),
            refresh_token: "alias-refresh".to_string(),
            expires_at: i64::MAX,
        };
        crate::providers::oauth_refresh::mark_invalid_refresh_token(
            "claude_code_2",
            "alias-refresh",
        );
        assert!(crate::providers::oauth_refresh::is_invalid_refresh_token(
            "claude_code_2",
            "alias-refresh"
        ));

        save_provider_oauth_tokens(
            &gcx.chat_ctx(gcx.clone()),
            &gcx.quota_cache,
            &config_dir,
            "claude_code_2",
            "claude_code",
            &serde_yaml::to_value(&tokens).unwrap(),
        )
        .await
        .unwrap();

        assert!(!crate::providers::oauth_refresh::is_invalid_refresh_token(
            "claude_code_2",
            "alias-refresh"
        ));
        let saved = provider_config_json(&config_dir, "claude_code_2").await;
        assert_eq!(saved["base_provider"], "claude_code");
        assert_eq!(saved["display_name"], "Work Claude");
        assert_eq!(saved["oauth_tokens"]["access_token"], "alias-access");
    }

    #[tokio::test]
    async fn concurrent_oauth_save_and_model_toggle_preserve_auth_and_models() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("openai_codex_2.yaml"),
            "base_provider: openai_codex\ndisplay_name: Work Codex\noauth_tokens:\n  access_token: old\n",
        )
        .await
        .unwrap();
        reload_provider_from_disk(&gcx.chat_ctx(gcx.clone()), "openai_codex_2", &config_dir)
            .await
            .unwrap();
        let tokens = crate::providers::openai_codex_oauth::OAuthTokens {
            access_token: "alias-access".to_string(),
            refresh_token: "alias-refresh".to_string(),
            expires_at: i64::MAX,
            ..Default::default()
        };
        let tokens_value = serde_yaml::to_value(&tokens).unwrap();
        let ctx = gcx.chat_ctx(gcx.clone());

        let save = save_provider_oauth_tokens(
            &ctx,
            &gcx.quota_cache,
            &config_dir,
            "openai_codex_2",
            "openai_codex",
            &tokens_value,
        );
        let toggle = update_model_enabled_state(
            &ctx,
            "openai_codex_2",
            "gpt-5.6-codex",
            true,
        );
        let (save_result, toggle_result) = tokio::join!(save, toggle);
        save_result.unwrap();
        toggle_result.unwrap();

        let saved = provider_config_json(&config_dir, "openai_codex_2").await;
        assert_eq!(saved["oauth_tokens"]["access_token"], "alias-access");
        assert_eq!(saved["oauth_tokens"]["refresh_token"], "alias-refresh");
        // Subscription-only: top-level OPENAI_API_KEY must never persist.
        assert!(saved.get("OPENAI_API_KEY").is_none());
        assert!(saved["enabled_models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model.as_str() == Some("gpt-5.6-codex")));
    }

    #[tokio::test]
    async fn concurrent_token_save_and_custom_model_update_preserve_auth_and_models() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "custom_2".to_string(),
            }),
            hyper::body::Bytes::from(
                serde_json::to_vec(&json!({
                    "base_provider": "custom",
                    "display_name": "Work Custom",
                    "api_key": "sk-old",
                    "chat_endpoint": "https://example.com/v1/chat/completions",
                    "enabled": true,
                    "enabled_models": []
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        {
            let mut registry = gcx.providers.write().await;
            registry.get_mut("custom_2").unwrap().add_custom_model(
                "my-custom".to_string(),
                CustomModelConfig {
                    n_ctx: Some(4096),
                    ..Default::default()
                },
            );
        }

        let update_auth = handle_v1_provider_update(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "custom_2".to_string(),
            }),
            hyper::body::Bytes::from(
                serde_json::to_vec(&json!({
                    "base_provider": "custom",
                    "display_name": "Work Custom",
                    "api_key": "sk-new"
                }))
                .unwrap(),
            ),
        );
        let ctx = gcx.chat_ctx(gcx.clone());
        let patch_model = patch_provider_model_config(&ctx, &config_dir, "custom_2");
        let (auth_result, model_result) = tokio::join!(update_auth, patch_model);
        auth_result.unwrap();
        model_result.unwrap();

        let saved = provider_config_json(&config_dir, "custom_2").await;
        assert_eq!(saved["api_key"], "sk-new");
        assert_eq!(saved["custom_models"]["my-custom"]["n_ctx"], 4096);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn provider_secret_writes_are_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let tokens = crate::providers::openai_codex_oauth::OAuthTokens {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: i64::MAX,
            ..Default::default()
        };

        save_provider_oauth_tokens(
            &gcx.chat_ctx(gcx.clone()),
            &gcx.quota_cache,
            &config_dir,
            "openai_codex",
            "openai_codex",
            &serde_yaml::to_value(&tokens).unwrap(),
        )
        .await
        .unwrap();

        let metadata = std::fs::metadata(provider_file_path(&config_dir, "openai_codex")).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn github_copilot_oauth_alias_redacts_settings() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let tokens = crate::providers::github_copilot_oauth::OAuthTokens {
            access_token: "gho-alias-secret".to_string(),
            expires_at: i64::MAX,
            ..Default::default()
        };

        save_provider_oauth_tokens(
            &gcx.chat_ctx(gcx.clone()),
            &gcx.quota_cache,
            &config_dir,
            "github_copilot_2",
            "github_copilot",
            &serde_yaml::to_value(&tokens).unwrap(),
        )
        .await
        .unwrap();

        let saved = provider_config_json(&config_dir, "github_copilot_2").await;
        assert_eq!(saved["base_provider"], "github_copilot");
        assert_eq!(saved["oauth_tokens"]["access_token"], "gho-alias-secret");
        let settings = {
            let registry = gcx.providers.read().await;
            registry
                .get("github_copilot_2")
                .unwrap()
                .provider_settings_as_json()
        };
        assert_eq!(settings["base_provider"], "github_copilot");
        assert_eq!(settings["oauth_tokens"]["access_token"], "***");
        assert!(!settings.to_string().contains("gho-alias-secret"));
    }

    #[tokio::test]
    async fn instance_aware_route_resolution_rejects_unsupported_base() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("openai_2.yaml"),
            "base_provider: openai\ndisplay_name: Work OpenAI\napi_key: sk-test\n",
        )
        .await
        .unwrap();

        let err = handle_v1_provider_health(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);

        let err = handle_v1_provider_usage(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderPathParams {
                name: "openai_2".to_string(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn openrouter_model_endpoints_accepts_provider_instances() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        let providers_dir = config_dir.join("providers.d");
        tokio::fs::create_dir_all(&providers_dir).await.unwrap();
        tokio::fs::write(
            providers_dir.join("openrouter_2.yaml"),
            "base_provider: openrouter\ndisplay_name: Work OpenRouter\n",
        )
        .await
        .unwrap();

        let err = handle_v1_openrouter_model_endpoints(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Path(ProviderModelPathParams {
                name: "openrouter_2".to_string(),
                model_id: "openai/gpt-4.1".to_string(),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status_code, StatusCode::BAD_GATEWAY);
        assert_eq!(err.message, "OpenRouter API key is not configured");
    }

    async fn assert_remove_custom_model_clears_enabled_entry(
        provider_name: &'static str,
        provider: Box<dyn crate::providers::traits::ProviderTrait>,
    ) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        {
            let mut registry = gcx.providers.write().await;
            registry.add(provider);
        }

        handle_v1_provider_remove_custom_model_impl(
            &gcx.chat_ctx(gcx.clone()),
            provider_name,
            hyper::body::Bytes::from(r#"{"model_id":"stale-custom"}"#),
        )
        .await
        .unwrap();

        let (enabled_models, has_custom_model) = {
            let registry = gcx.providers.read().await;
            let provider = registry.get(provider_name).unwrap();
            (
                provider.enabled_models().to_vec(),
                provider.custom_models().contains_key("stale-custom"),
            )
        };
        assert!(!enabled_models.iter().any(|model| model == "stale-custom"));
        assert!(!has_custom_model);

        let content = tokio::fs::read_to_string(provider_file_path(&config_dir, provider_name))
            .await
            .unwrap();
        let yaml: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
        assert!(yaml
            .get("enabled_models")
            .and_then(|value| value.as_sequence())
            .map(|models| models.is_empty())
            .unwrap_or(false));
        assert!(yaml
            .get("custom_models")
            .and_then(|value| value.as_mapping())
            .map(|models| models.is_empty())
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn custom_provider_remove_custom_model_clears_stale_enabled_entry() {
        let provider = crate::providers::custom::CustomProvider {
            chat_endpoint: "https://example.com/v1/chat/completions".to_string(),
            enabled: true,
            enabled_models: vec!["stale-custom".to_string()],
            custom_models: HashMap::from([(
                "stale-custom".to_string(),
                CustomModelConfig::default(),
            )]),
            ..Default::default()
        };

        assert_remove_custom_model_clears_enabled_entry("custom", Box::new(provider)).await;
    }

    #[tokio::test]
    async fn doubao_remove_custom_model_clears_stale_enabled_entry() {
        let provider = crate::providers::doubao::DoubaoProvider {
            api_key: "sk-test".to_string(),
            enabled: true,
            enabled_models: vec!["stale-custom".to_string()],
            custom_models: HashMap::from([(
                "stale-custom".to_string(),
                CustomModelConfig::default(),
            )]),
            ..Default::default()
        };

        assert_remove_custom_model_clears_enabled_entry("doubao", Box::new(provider)).await;
    }

    #[tokio::test]
    async fn openai_codex_usage_no_token_change_does_not_replace_registry_provider() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut current = OpenAICodexProvider::default();
        current.enabled_models = vec!["keep-enabled".to_string()];
        current.custom_models.insert(
            "keep-custom".to_string(),
            CustomModelConfig {
                n_ctx: Some(4096),
                ..Default::default()
            },
        );
        current.oauth_tokens.access_token = "same-access".to_string();
        current.session_id = "same-session".to_string();
        let previous_tokens = current.oauth_tokens.clone();
        let previous_session_id = current.session_id.clone();
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(current.clone()));
        }
        let mut source = OpenAICodexProvider::default();
        source.enabled_models = vec!["clobber-enabled".to_string()];
        source.oauth_tokens = previous_tokens.clone();
        source.session_id = previous_session_id.clone();

        let changed = sync_openai_codex_auth_state(
            &gcx.chat_ctx(gcx.clone()),
            "openai_codex",
            &source,
            &previous_tokens,
            &previous_session_id,
        )
        .await
        .unwrap();

        let stored = {
            let registry = gcx.providers.read().await;
            registry
                .get("openai_codex")
                .unwrap()
                .as_any()
                .downcast_ref::<OpenAICodexProvider>()
                .unwrap()
                .clone()
        };
        assert!(!changed);
        assert_eq!(stored.enabled_models, vec!["keep-enabled".to_string()]);
        assert!(stored.custom_models.contains_key("keep-custom"));
    }

    #[tokio::test]
    async fn openai_codex_stale_auth_sync_does_not_overwrite_newer_registry_token() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut current = OpenAICodexProvider::default();
        current.oauth_tokens.access_token = "newer-access".to_string();
        current.oauth_tokens.refresh_token = "newer-refresh".to_string();
        current.session_id = "newer-session".to_string();
        let previous_tokens = crate::providers::openai_codex_oauth::OAuthTokens {
            access_token: "old-access".to_string(),
            refresh_token: "old-refresh".to_string(),
            ..Default::default()
        };
        let previous_session_id = "old-session".to_string();
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(current));
        }
        let mut source = OpenAICodexProvider::default();
        source.oauth_tokens.access_token = "stale-refresh-access".to_string();
        source.oauth_tokens.refresh_token = "stale-refresh".to_string();
        source.session_id = "stale-refresh-session".to_string();

        let changed = sync_openai_codex_auth_state(
            &gcx.chat_ctx(gcx.clone()),
            "openai_codex",
            &source,
            &previous_tokens,
            &previous_session_id,
        )
        .await
        .unwrap();

        let stored = {
            let registry = gcx.providers.read().await;
            registry
                .get("openai_codex")
                .unwrap()
                .as_any()
                .downcast_ref::<OpenAICodexProvider>()
                .unwrap()
                .clone()
        };
        assert!(!changed);
        assert_eq!(stored.oauth_tokens.access_token, "newer-access");
        assert_eq!(stored.oauth_tokens.refresh_token, "newer-refresh");
        assert_eq!(stored.session_id, "newer-session");
    }

    #[tokio::test]
    async fn openai_codex_usage_refresh_rereads_registry_and_skips_stale_token() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut current = OpenAICodexProvider::default();
        current.oauth_tokens.access_token = "fresh-access".to_string();
        current.oauth_tokens.refresh_token = "refresh".to_string();
        current.oauth_tokens.expires_at = i64::MAX;
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(current));
        }
        let http_client = gcx.http_client.clone();

        let refreshed = force_refresh_openai_codex_usage_for_retry(
            &gcx.chat_ctx(gcx.clone()),
            &http_client,
            "openai_codex",
            "stale-access",
            Some(reqwest::StatusCode::UNAUTHORIZED),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(refreshed.oauth_tokens.access_token, "fresh-access");
    }

    #[tokio::test]
    async fn openai_codex_token_refresh_preserves_concurrent_model_settings() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut current = OpenAICodexProvider::default();
        current.enabled_models = vec!["keep-enabled".to_string()];
        current.custom_models.insert(
            "keep-custom".to_string(),
            CustomModelConfig {
                n_ctx: Some(4096),
                ..Default::default()
            },
        );
        current.oauth_tokens.access_token = "old-access".to_string();
        current.session_id = "old-session".to_string();
        let previous_tokens = current.oauth_tokens.clone();
        let previous_session_id = current.session_id.clone();
        {
            let mut registry = gcx.providers.write().await;
            registry.add(Box::new(current));
        }
        let mut source = OpenAICodexProvider::default();
        source.enabled_models = vec!["clobber-enabled".to_string()];
        source.oauth_tokens.access_token = "new-access".to_string();
        source.oauth_tokens.refresh_token = "new-refresh".to_string();
        source.oauth_tokens.expires_at = 42;
        source.session_id = "new-session".to_string();

        let changed = sync_openai_codex_auth_state(
            &gcx.chat_ctx(gcx.clone()),
            "openai_codex",
            &source,
            &previous_tokens,
            &previous_session_id,
        )
        .await
        .unwrap();

        let stored = {
            let registry = gcx.providers.read().await;
            registry
                .get("openai_codex")
                .unwrap()
                .as_any()
                .downcast_ref::<OpenAICodexProvider>()
                .unwrap()
                .clone()
        };
        assert!(changed);
        assert_eq!(stored.oauth_tokens.access_token, "new-access");
        assert_eq!(stored.oauth_tokens.refresh_token, "new-refresh");
        assert_eq!(stored.session_id, "new-session");
        assert_eq!(stored.enabled_models, vec!["keep-enabled".to_string()]);
        assert!(stored.custom_models.contains_key("keep-custom"));
    }
