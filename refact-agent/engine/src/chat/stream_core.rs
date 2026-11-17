pub use refact_runtime_api::stream_core::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::{Duration, Instant};

#[cfg(test)]
use serde_json::json;

#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::call_validation::ChatUsage;
#[cfg(test)]
use crate::caps::BaseModelRecord;
#[cfg(test)]
use crate::chat::retry_policy::{classify_llm_error_for_retry, RetryDecision};
#[cfg(test)]
use crate::chat::types::DeltaOp;
#[cfg(test)]
use crate::chat::openai_codex_ws::OpenAICodexWebSocketSession;
#[cfg(test)]
use crate::llm::adapter::AdapterSettings;
#[cfg(test)]
use crate::llm::{get_adapter, LlmRequest, WireFormat};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_validation::ChatMessage;
    #[cfg(unix)]
    use refact_core::provider_types::{
        CredentialSpec, DEFAULT_CREDENTIAL_REFRESH_INTERVAL_MS, DEFAULT_CREDENTIAL_TIMEOUT_MS,
    };
    use refact_privacy::{Attribution, FileRecord, PolicyLoad, PrivacyPolicy, PrivacyRecord};
    use refact_privacy::{ShellBehavior, SubagentPolicy, Zone};

    fn privacy_policy(allowed_provider: &str) -> PrivacyPolicy {
        PrivacyPolicy {
            blocked: Vec::new(),
            zones: vec![
                Zone {
                    name: "secrets".to_string(),
                    patterns: vec![".env*".to_string()],
                    send_to: vec![allowed_provider.to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
                Zone {
                    name: "normal".to_string(),
                    patterns: vec!["*".to_string()],
                    send_to: vec!["*".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
            ],
            subagents: SubagentPolicy::default(),
            ..Default::default()
        }
    }

    fn request_with_secret() -> LlmRequest {
        let mut message = ChatMessage::new("user".to_string(), "guarded".to_string());
        message.extra.insert(
            "privacy".to_string(),
            serde_json::to_value(PrivacyRecord {
                files: vec![FileRecord {
                    path: ".env".to_string(),
                    zone: "secrets".to_string(),
                    attribution: Attribution::Declared,
                }],
            })
            .unwrap(),
        );
        LlmRequest::new("model".to_string(), vec![message])
    }

    fn model_record(provider: &str) -> BaseModelRecord {
        BaseModelRecord {
            id: format!("{provider}/model"),
            name: "model".to_string(),
            endpoint: "https://example.com/v1/chat/completions".to_string(),
            wire_format: WireFormat::OpenaiChatCompletions,
            ..Default::default()
        }
    }

    fn adapter_settings(model: &BaseModelRecord) -> AdapterSettings {
        AdapterSettings {
            api_key: String::new(),
            auth_token: String::new(),
            endpoint: model.endpoint.clone(),
            extra_headers: HashMap::new(),
            model_name: model.name.clone(),
            supports_tools: false,
            supports_reasoning: false,
            reasoning_type: None,
            supports_temperature: true,
            supports_max_completion_tokens: false,
            eof_is_done: false,
            supports_web_search: false,
            supports_cache_control: false,
        }
    }

    #[test]
    fn litellm_identity_supports_named_provider_instances() {
        let direct = model_record("litellm");
        let mut named = model_record("company-gateway");
        named.extra_headers.insert(
            crate::caps::caps::MODEL_BASE_PROVIDER_HEADER.to_string(),
            "litellm".to_string(),
        );

        assert!(is_litellm_model(&direct));
        assert!(is_litellm_model(&named));
        assert!(!is_litellm_model(&model_record("custom")));
    }

    async fn app_with_privacy_policy(policy: PrivacyPolicy) -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.privacy_policy_load.write().unwrap() = PolicyLoad {
            policy: Arc::new(policy),
            error: None,
            source_paths: Vec::new(),
        };
        crate::app_state::app_state_from_gcx(gcx).await
    }

    #[tokio::test]
    async fn privacy_gate_refuses_disallowed_provider_before_build_http() {
        let app = app_with_privacy_policy(privacy_policy("trusted")).await;
        let model = model_record("untrusted");

        let result = build_http_for_attempt(
            &app,
            &request_with_secret(),
            &model,
            get_adapter(model.wire_format),
            &adapter_settings(&model),
        );
        let error = match result {
            Ok(_) => panic!("untrusted provider should be refused"),
            Err(error) => error,
        };

        assert!(error.starts_with("Output withheld by user privacy policy"));
    }

    #[tokio::test]
    async fn privacy_gate_allows_provider_through_build_http() {
        let app = app_with_privacy_policy(privacy_policy("trusted")).await;
        let model = model_record("trusted");

        let parts = build_http_for_attempt(
            &app,
            &request_with_secret(),
            &model,
            get_adapter(model.wire_format),
            &adapter_settings(&model),
        )
        .unwrap();

        assert_eq!(parts.url, model.endpoint);
        assert_eq!(parts.body["model"], model.name);
    }

    #[tokio::test]
    async fn privacy_gate_rechecks_each_provider_attempt() {
        let app = app_with_privacy_policy(privacy_policy("fallback")).await;
        let request = request_with_secret();
        let denied = model_record("primary");
        let allowed = model_record("fallback");

        let first = build_http_for_attempt(
            &app,
            &request,
            &denied,
            get_adapter(denied.wire_format),
            &adapter_settings(&denied),
        );
        let second = build_http_for_attempt(
            &app,
            &request,
            &allowed,
            get_adapter(allowed.wire_format),
            &adapter_settings(&allowed),
        );

        let first_error = match first {
            Ok(_) => panic!("primary provider should be refused"),
            Err(error) => error,
        };
        assert!(first_error.starts_with("Output withheld by user privacy policy"));
        assert_eq!(second.unwrap().body["model"], allowed.name);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_credential_retries_once_after_unauthorized() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let provider_id = "custom_helper";
        crate::providers::credential::invalidate(provider_id);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (expected_token, status, body) in [
                ("1", "401 Unauthorized", ""),
                (
                    "2",
                    "200 OK",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                ),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let count = socket.read(&mut chunk).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
                assert!(request.contains(&format!("authorization: bearer {expected_token}")));
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("token-count");
        let helper = format!(
            "n=$(cat '{}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{}'; echo $n",
            count.display(),
            count.display()
        );
        let mut model = model_record(provider_id);
        model.endpoint = format!("http://{address}/v1/chat/completions");
        model.credential = Some(CredentialSpec::Command {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), helper],
            timeout_ms: DEFAULT_CREDENTIAL_TIMEOUT_MS,
            refresh_interval_ms: DEFAULT_CREDENTIAL_REFRESH_INTERVAL_MS,
            cwd: None,
            env_passthrough: Vec::new(),
        });
        let params = StreamRunParams {
            llm_request: LlmRequest::new(
                model.id.clone(),
                vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            ),
            model_rec: model,
            chat_id: None,
            allow_websocket: false,
            abort_flag: None,
            abort_notify: None,
            supports_tools: false,
            supports_reasoning: false,
            reasoning_type: None,
            supports_temperature: true,
        };
        let app = app_with_privacy_policy(privacy_policy(provider_id)).await;
        let mut collector = ReplayCollector::default();

        let result = run_llm_stream(app, params, &mut collector).await.unwrap();
        match result {
            LlmStreamOutcome::Choices(choices) => assert_eq!(choices[0].content, "ok"),
            LlmStreamOutcome::PausedForCacheGuard => panic!("unexpected cache guard pause"),
        }
        server.await.unwrap();
        crate::providers::credential::invalidate(provider_id);
    }

    #[test]
    fn websocket_endpoint_requires_codex_backend_and_internal_marker() {
        let mut model = BaseModelRecord {
            id: "openai_codex/gpt-5.6-codex".to_string(),
            endpoint: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            ..Default::default()
        };

        assert!(openai_codex_websocket_endpoint(&model).is_none());

        model.extra_headers.insert(
            crate::providers::openai_codex::CODEX_WEBSOCKET_ENDPOINT_HEADER.to_string(),
            "wss://chatgpt.com/backend-api/codex/responses".to_string(),
        );
        assert_eq!(
            openai_codex_websocket_endpoint(&model),
            Some("wss://chatgpt.com/backend-api/codex/responses")
        );

        model.endpoint = "https://api.openai.com/v1/responses".to_string();
        assert!(openai_codex_websocket_endpoint(&model).is_none());

        model.endpoint = "https://chatgpt.com/backend-api/codex/responses".to_string();
        model.id = "openai/gpt-5.6-codex".to_string();
        assert!(openai_codex_websocket_endpoint(&model).is_none());
    }

    #[test]
    fn websocket_stack_mb_parser_uses_default_and_clamps_bounds() {
        assert_eq!(
            stack_mb_to_bytes_clamped(None, DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES),
            DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES
        );
        assert_eq!(
            stack_mb_to_bytes_clamped(
                Some("nope"),
                DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES
            ),
            DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES
        );
        assert_eq!(
            stack_mb_to_bytes_clamped(Some("1"), DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES),
            MIN_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_MB * 1024 * 1024
        );
        assert_eq!(
            stack_mb_to_bytes_clamped(
                Some("99999"),
                DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES
            ),
            MAX_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_MB * 1024 * 1024
        );
        assert_eq!(
            stack_mb_to_bytes_clamped(
                Some("96"),
                DEFAULT_OPENAI_CODEX_WEBSOCKET_THREAD_STACK_BYTES
            ),
            96 * 1024 * 1024
        );
    }

    #[test]
    fn llm_http_header_retry_config_requires_opt_in() {
        let model = BaseModelRecord {
            id: "anthropic/claude-sonnet-4".to_string(),
            endpoint: "https://api.anthropic.com/v1/messages".to_string(),
            ..Default::default()
        };

        assert_eq!(llm_http_header_retry_config(&model), None);
    }

    #[test]
    fn claude_code_instance_id_accepts_base_and_named_instances() {
        let model = |provider_name: &str, base_provider: &str| {
            let mut model = BaseModelRecord {
                id: format!("{provider_name}/claude-sonnet-4"),
                wire_format: crate::llm::WireFormat::AnthropicMessages,
                ..Default::default()
            };
            model.extra_headers.insert(
                crate::caps::caps::MODEL_BASE_PROVIDER_HEADER.to_string(),
                base_provider.to_string(),
            );
            model
        };
        let base = BaseModelRecord {
            id: "claude_code/claude-sonnet-4".to_string(),
            wire_format: crate::llm::WireFormat::AnthropicMessages,
            ..Default::default()
        };
        let named = model("work", "claude_code");
        let hyphenated = model("claude-code-work", "claude_code");
        let plain_anthropic = BaseModelRecord {
            id: "anthropic/claude-sonnet-4".to_string(),
            wire_format: crate::llm::WireFormat::AnthropicMessages,
            ..Default::default()
        };

        assert_eq!(claude_code_instance_id(&base), Some("claude_code"));
        assert_eq!(claude_code_instance_id(&named), Some("work"));
        assert_eq!(
            claude_code_instance_id(&hyphenated),
            Some("claude-code-work")
        );
        assert_eq!(claude_code_instance_id(&plain_anthropic), None);
    }

    #[test]
    fn xai_oauth_instance_id_uses_model_prefix_for_configured_instance() {
        let singleton = BaseModelRecord {
            id: "xai_oauth/grok-4".to_string(),
            ..Default::default()
        };
        let mut named = BaseModelRecord {
            id: "grok-work/grok-4".to_string(),
            ..Default::default()
        };
        named.extra_headers.insert(
            crate::caps::caps::MODEL_BASE_PROVIDER_HEADER.to_string(),
            "xai_oauth".to_string(),
        );
        let plain_xai = BaseModelRecord {
            id: "xai/grok-4".to_string(),
            ..Default::default()
        };

        assert_eq!(xai_oauth_instance_id(&singleton), Some("xai_oauth"));
        assert_eq!(xai_oauth_instance_id(&named), Some("grok-work"));
        assert_eq!(xai_oauth_instance_id(&plain_xai), None);
    }

    #[test]
    fn xai_429_rate_limit_header_extraction_is_strictly_allowlisted() {
        use reqwest::header::{HeaderMap, HeaderValue};

        // The response body/status do not affect extraction, so this models the
        // headers captured before a rejected 429 response body is consumed.
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-requests", HeaderValue::from_static("60"));
        headers.insert(
            "x-ratelimit-remaining-requests",
            HeaderValue::from_static("42"),
        );
        headers.insert("x-ratelimit-reset-tokens", HeaderValue::from_static("1s"));
        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        headers.insert("set-cookie", HeaderValue::from_static("credential=secret"));
        headers.insert(
            "x-ratelimit-limit-tokens",
            HeaderValue::from_bytes(&[0xff]).unwrap(),
        );

        assert_eq!(
            xai_rate_limit_headers(&headers),
            HashMap::from([
                ("x-ratelimit-limit-requests".to_string(), "60".to_string()),
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "42".to_string(),
                ),
                ("x-ratelimit-reset-tokens".to_string(), "1s".to_string()),
            ])
        );
    }

    #[test]
    fn openai_codex_instance_id_accepts_singleton_and_arbitrary_alias() {
        let singleton = BaseModelRecord {
            id: "openai_codex/gpt-5.6-codex".to_string(),
            endpoint: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            ..Default::default()
        };
        let mut alias = BaseModelRecord {
            id: "codex-prod/gpt-5.6-codex".to_string(),
            endpoint: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            ..Default::default()
        };
        alias.extra_headers.insert(
            crate::caps::caps::MODEL_BASE_PROVIDER_HEADER.to_string(),
            "openai_codex".to_string(),
        );

        assert_eq!(openai_codex_instance_id(&singleton), Some("openai_codex"));
        assert_eq!(openai_codex_instance_id(&alias), Some("codex-prod"));
    }

    #[test]
    fn claude_code_request_expiry_uses_internal_model_header() {
        let mut model = BaseModelRecord::default();
        model.extra_headers.insert(
            crate::providers::claude_code::CLAUDE_CODE_OAUTH_EXPIRES_AT_HEADER.to_string(),
            "12345".to_string(),
        );
        assert_eq!(claude_code_request_expires_at(&model), Some(12345));
    }

    #[test]
    fn llm_http_header_retry_config_defaults_and_overrides() {
        let mut model = BaseModelRecord {
            id: "openai_codex/gpt-5.6-codex".to_string(),
            endpoint: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            ..Default::default()
        };

        model.extra_headers.insert(
            crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_ENABLED_HEADER.to_string(),
            "true".to_string(),
        );
        assert_eq!(
            llm_http_header_retry_config(&model),
            Some((
                Duration::from_secs(
                    crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_TIMEOUT_SECONDS_DEFAULT,
                ),
                crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_MAX_ATTEMPTS_DEFAULT,
            ))
        );

        model.extra_headers.insert(
            crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_TIMEOUT_SECONDS_HEADER
                .to_string(),
            "3".to_string(),
        );
        model.extra_headers.insert(
            crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_MAX_ATTEMPTS_HEADER.to_string(),
            "4".to_string(),
        );
        assert_eq!(
            llm_http_header_retry_config(&model),
            Some((Duration::from_secs(3), 4))
        );

        model.extra_headers.insert(
            crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_ENABLED_HEADER.to_string(),
            "false".to_string(),
        );
        assert_eq!(llm_http_header_retry_config(&model), None);
    }

    #[test]
    fn llm_http_header_retry_config_applies_to_generic_openai_provider() {
        let mut model = BaseModelRecord {
            id: "openai/gpt-5".to_string(),
            endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
            ..Default::default()
        };

        crate::providers::llm_http_retry::insert_llm_http_header_retry_config(
            &mut model.extra_headers,
            true,
            crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_TIMEOUT_SECONDS_DEFAULT,
            crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_MAX_ATTEMPTS_DEFAULT,
        );

        assert_eq!(
            llm_http_header_retry_config(&model),
            Some((
                Duration::from_secs(
                    crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_TIMEOUT_SECONDS_DEFAULT,
                ),
                crate::providers::llm_http_retry::LLM_HTTP_HEADER_RETRY_MAX_ATTEMPTS_DEFAULT,
            ))
        );
    }

    #[test]
    fn websocket_create_message_wraps_responses_body() {
        let body = json!({
            "model": "gpt-5.6-codex",
            "stream": true,
            "input": []
        });

        let message = build_openai_codex_websocket_message(&body);

        assert_eq!(message["type"], json!("response.create"));
        assert_eq!(message["model"], json!("gpt-5.6-codex"));
        assert_eq!(message["stream"], json!(true));
    }

    #[test]
    fn websocket_body_uses_previous_response_delta_when_history_matches() {
        let previous_user = json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "one"}]
        });
        let previous_assistant = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "two"}]
        });
        let next_user = json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "three"}]
        });
        let last_request_body = json!({
            "model": "gpt-5.6-codex",
            "stream": true,
            "input": [previous_user.clone()]
        });
        let current_body = json!({
            "model": "gpt-5.6-codex",
            "stream": true,
            "input": [previous_user, previous_assistant.clone(), next_user.clone()]
        });
        let mut session = OpenAICodexWebSocketSession::default();
        session.last_request_body = Some(last_request_body);
        session.last_response = Some(
            crate::chat::openai_codex_ws::OpenAICodexWebSocketLastResponse {
                response_id: "resp_1".to_string(),
                output_items: vec![previous_assistant],
            },
        );

        let message = prepare_openai_codex_websocket_body(&current_body, &session, false);

        assert_eq!(message["type"], json!("response.create"));
        assert_eq!(message["previous_response_id"], json!("resp_1"));
        assert_eq!(message["input"], json!([next_user]));
    }

    #[test]
    fn websocket_previous_response_missing_error_is_incremental_only() {
        assert!(is_openai_codex_previous_response_missing_error(
            "OpenAI Codex WebSocket error status=400 code=previous_response_not_found: Previous response with id 'resp_1' not found."
        ));
        assert!(!is_openai_codex_previous_response_missing_error(
            "OpenAI Codex WebSocket stream stalled"
        ));
    }

    #[test]
    fn llm_http_header_timeout_is_retryable() {
        let error = llm_http_response_header_timeout_message(Duration::from_secs(10));

        assert!(matches!(
            classify_llm_error_for_retry(&error),
            RetryDecision::Retry { .. }
        ));
    }

    #[test]
    fn cache_guard_is_not_committed_for_retryable_http_errors() {
        assert!(!should_commit_cache_guard_after_http_success(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"rate limit"}}"#,
        ));
        assert!(!should_commit_cache_guard_after_http_success(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":{"message":"overloaded"}}"#,
        ));
    }

    #[test]
    fn cache_guard_is_not_committed_for_context_limit_errors() {
        assert!(!should_commit_cache_guard_after_http_success(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"context length exceeded"}}"#,
        ));
        assert!(!should_commit_cache_guard_after_http_success(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"This model's maximum context length is 128000 tokens"}}"#,
        ));
    }

    #[test]
    fn cache_guard_is_committed_for_non_retryable_http_errors() {
        assert!(should_commit_cache_guard_after_http_success(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"invalid api key"}}"#,
        ));
    }

    #[test]
    fn stream_error_with_partial_output_retries_transient_error() {
        let abort = AtomicBool::new(false);
        let error = LlmStreamError {
            message: "LLM error (503 Service Unavailable): overloaded".to_string(),
            partial_output_emitted: true,
        };

        assert!(matches!(
            error.retry_decision(),
            RetryDecision::Retry { .. }
        ));
        assert!(error.should_retry(0, &abort));
    }

    #[test]
    fn stream_error_without_partial_output_retries_transient_error() {
        let abort = AtomicBool::new(false);
        let error = LlmStreamError {
            message: "LLM error (503 Service Unavailable): overloaded".to_string(),
            partial_output_emitted: false,
        };

        assert!(matches!(
            error.retry_decision(),
            RetryDecision::Retry { .. }
        ));
        assert!(error.should_retry(0, &abort));
    }

    #[test]
    fn user_cancelled_with_partial_output_does_not_retry() {
        let abort = AtomicBool::new(false);
        let error = LlmStreamError {
            message: ABORT_ERROR_MESSAGE.to_string(),
            partial_output_emitted: true,
        };

        assert!(error.retry_decision().is_user_cancelled());
        assert!(!error.should_retry(0, &abort));
    }

    #[test]
    fn mid_stream_disconnect_with_partial_output_retries() {
        let abort = AtomicBool::new(false);
        let error = LlmStreamError {
            message: "LLM stream ended unexpectedly without completion signal".to_string(),
            partial_output_emitted: true,
        };

        assert!(matches!(
            error.retry_decision(),
            RetryDecision::Retry { .. }
        ));
        assert!(error.should_retry(0, &abort));
    }

    #[tokio::test]
    async fn wait_for_abort_signal_returns_instantly_on_notify() {
        let abort_flag = Arc::new(AtomicBool::new(false));
        let abort_notify = Arc::new(tokio::sync::Notify::new());
        let start = std::time::Instant::now();
        let flag_for_setter = abort_flag.clone();
        let notify_for_setter = abort_notify.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            flag_for_setter.store(true, Ordering::SeqCst);
            notify_for_setter.notify_waiters();
        });
        wait_for_abort_signal(Some(abort_flag), Some(abort_notify)).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "abort_notify should propagate within 50ms, took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn wait_for_abort_signal_falls_back_to_polling_without_notify() {
        let abort_flag = Arc::new(AtomicBool::new(false));
        let flag_for_setter = abort_flag.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            flag_for_setter.store(true, Ordering::SeqCst);
        });
        let start = std::time::Instant::now();
        wait_for_abort_signal(Some(abort_flag), None).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "polling fallback should detect abort within ~50ms tick, took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn wait_for_abort_signal_returns_immediately_if_already_set() {
        let abort_flag = Arc::new(AtomicBool::new(true));
        let abort_notify = Arc::new(tokio::sync::Notify::new());
        let start = std::time::Instant::now();
        wait_for_abort_signal(Some(abort_flag), Some(abort_notify)).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(5),
            "already-set flag should return immediately, took {:?}",
            elapsed
        );
    }

    #[test]
    fn anthropic_eof_after_stop_reason_can_finish_stream() {
        let mut acc = ChoiceAccumulator::default();
        acc.finish_reason = Some("end_turn".to_string());

        assert!(should_finish_on_anthropic_stop_reason_eof(
            WireFormat::AnthropicMessages,
            &[acc],
        ));
    }

    #[test]
    fn anthropic_eof_without_stop_reason_stays_unexpected() {
        let acc = ChoiceAccumulator::default();

        assert!(!should_finish_on_anthropic_stop_reason_eof(
            WireFormat::AnthropicMessages,
            &[acc],
        ));
    }

    #[test]
    fn non_anthropic_eof_after_stop_reason_stays_unexpected() {
        let mut acc = ChoiceAccumulator::default();
        acc.finish_reason = Some("end_turn".to_string());

        assert!(!should_finish_on_anthropic_stop_reason_eof(
            WireFormat::OpenaiChatCompletions,
            &[acc],
        ));
    }

    #[test]
    fn function_calls_xml_is_not_emitted_as_content() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_without_function_calls(
            &mut acc,
            &mut ops,
            "Before <function_calls><invoke name=\"mcp_tool_search\"><parameter name=\"query\">x"
                .to_string(),
            None,
        );
        route_append_content_without_function_calls(
            &mut acc,
            &mut ops,
            "</parameter></invoke></function_calls> after".to_string(),
            None,
        );
        flush_pending_function_calls_parse(&mut acc, &mut ops);

        assert_eq!(acc.content, "Before  after");
        assert!(ops.iter().all(|op| match op {
            DeltaOp::AppendContent { text } =>
                !text.contains("<function_calls>")
                    && !text.contains("<invoke")
                    && !text.contains("<parameter"),
            _ => true,
        }));
    }

    #[test]
    fn partial_function_calls_open_tag_is_buffered() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_without_function_calls(
            &mut acc,
            &mut ops,
            "Before <function".to_string(),
            None,
        );
        assert_eq!(acc.content, "Before ");
        assert_eq!(acc.pending_function_calls_parse, "<function");

        route_append_content_without_function_calls(
            &mut acc,
            &mut ops,
            "_calls>hidden</function_calls> after".to_string(),
            None,
        );
        flush_pending_function_calls_parse(&mut acc, &mut ops);

        assert_eq!(acc.content, "Before  after");
    }

    #[test]
    fn split_with_partial_literal_suffix_handles_multibyte_boundaries() {
        let text = "aaaaaaaaaaaaa🙂<";

        let (emit, keep) = split_with_partial_literal_suffix(text, FUNCTION_CALLS_OPEN_TAG);

        assert_eq!(emit, "aaaaaaaaaaaaa🙂");
        assert_eq!(keep, "<");
    }

    #[test]
    fn function_calls_parser_handles_multibyte_before_partial_open_tag() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_without_function_calls(
            &mut acc,
            &mut ops,
            "aaaaaaaaaaaaa🙂<".to_string(),
            None,
        );

        assert_eq!(acc.content, "aaaaaaaaaaaaa🙂");
        assert_eq!(acc.pending_function_calls_parse, "<");
        assert!(matches!(
            ops.last(),
            Some(DeltaOp::AppendContent { text }) if text == "aaaaaaaaaaaaa🙂"
        ));
    }

    #[test]
    fn function_calls_parser_handles_multibyte_before_partial_close_tag() {
        let mut acc = ChoiceAccumulator {
            inside_function_calls_tag: true,
            pending_function_calls_parse: "aaaaaaaaaaaaa🙂</".to_string(),
            ..Default::default()
        };
        let mut ops = Vec::new();

        route_append_content_without_function_calls(&mut acc, &mut ops, "".to_string(), None);

        assert_eq!(acc.function_calls_xml_raw, "aaaaaaaaaaaaa🙂");
        assert_eq!(acc.pending_function_calls_parse, "</");
        assert!(ops.is_empty());
    }

    #[test]
    fn websocket_headers_override_beta_and_omit_http_only_metadata() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_static("Bearer tok"),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            "OpenAI-Beta",
            reqwest::header::HeaderValue::from_static("responses=experimental"),
        );
        headers.insert(
            crate::providers::openai_codex::CODEX_WEBSOCKET_ENDPOINT_HEADER,
            reqwest::header::HeaderValue::from_static(
                "wss://chatgpt.com/backend-api/codex/responses",
            ),
        );

        let entries = websocket_header_entries(&headers, Some("turn-state-1")).unwrap();
        let map: HashMap<_, _> = entries.into_iter().collect();

        assert_eq!(
            map.get("authorization").map(String::as_str),
            Some("Bearer tok")
        );
        assert_eq!(
            map.get("openai-beta").map(String::as_str),
            Some("responses_websockets=2026-02-06")
        );
        assert_eq!(
            map.get("x-codex-turn-state").map(String::as_str),
            Some("turn-state-1")
        );
        assert!(!map.contains_key("accept"));
        assert!(!map.contains_key("content-type"));
        assert!(!map.contains_key(crate::providers::openai_codex::CODEX_WEBSOCKET_ENDPOINT_HEADER));
    }

    #[test]
    fn websocket_parse_errors_are_fatal_before_replay() {
        let adapter = get_adapter(crate::llm::WireFormat::OpenaiResponses);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();

        let err = process_stream_event_data(
            adapter,
            "",
            "not json",
            &mut accumulators,
            &mut collector,
            true,
        )
        .unwrap_err();

        assert!(err.contains("Malformed stream chunk"));
        assert!(collector.events.is_empty());
    }

    #[test]
    fn ollama_ndjson_handles_split_and_multiple_lines() {
        let adapter = get_adapter(crate::llm::WireFormat::OllamaNative);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();
        let mut pending = Vec::new();

        let done = process_ndjson_bytes(
            adapter,
            "",
            &mut pending,
            br#"{"message":{"content":"Hel"#,
            &mut accumulators,
            &mut collector,
        )
        .unwrap();
        assert!(!done);
        assert!(collector.events.is_empty());

        let done = process_ndjson_bytes(
            adapter,
            "",
            &mut pending,
            br#"lo"}}
{"message":{"content":"!"}}
{"prompt_eval_count":7,"eval_count":3,"done":true}
"#,
            &mut accumulators,
            &mut collector,
        )
        .unwrap();

        assert!(done);
        assert_eq!(accumulators[0].content, "Hello!");
        let usage = accumulators[0].usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 7);
        assert_eq!(usage.completion_tokens, 3);
        assert!(pending.is_empty());
    }

    #[test]
    fn ollama_ndjson_malformed_line_is_fatal() {
        let adapter = get_adapter(crate::llm::WireFormat::OllamaNative);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();
        let mut pending = Vec::new();

        let err = process_ndjson_bytes(
            adapter,
            "",
            &mut pending,
            b"not-json\n",
            &mut accumulators,
            &mut collector,
        )
        .unwrap_err();

        assert!(err.contains("Malformed stream chunk"));
        assert!(collector.events.is_empty());
    }

    #[test]
    fn ollama_ndjson_oversized_complete_line_is_fatal() {
        let adapter = get_adapter(crate::llm::WireFormat::OllamaNative);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();
        let mut pending = Vec::new();
        let mut line = vec![b' '; MAX_NDJSON_LINE_BYTES + 1];
        line.push(b'\n');

        let err = process_ndjson_bytes(
            adapter,
            "",
            &mut pending,
            &line,
            &mut accumulators,
            &mut collector,
        )
        .unwrap_err();

        assert!(err.contains("exceeds"));
        assert!(collector.events.is_empty());
    }

    #[test]
    fn ollama_ndjson_oversized_pending_buffer_is_fatal() {
        let adapter = get_adapter(crate::llm::WireFormat::OllamaNative);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();
        let mut pending = Vec::new();
        let bytes = vec![b'a'; MAX_NDJSON_LINE_BYTES + 1];

        let err = process_ndjson_bytes(
            adapter,
            "",
            &mut pending,
            &bytes,
            &mut accumulators,
            &mut collector,
        )
        .unwrap_err();

        assert!(err.contains("exceeds"));
        assert!(collector.events.is_empty());
    }

    #[test]
    fn ollama_ndjson_processes_final_line_without_newline() {
        let adapter = get_adapter(crate::llm::WireFormat::OllamaNative);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();
        let mut pending = Vec::new();

        process_ndjson_bytes(
            adapter,
            "",
            &mut pending,
            br#"{"message":{"content":"done"}}"#,
            &mut accumulators,
            &mut collector,
        )
        .unwrap();
        assert_eq!(accumulators[0].content, "");

        let done = process_ndjson_eof(adapter, "", &mut pending, &mut accumulators, &mut collector)
            .unwrap();

        assert!(!done);
        assert_eq!(accumulators[0].content, "done");
        assert!(pending.is_empty());
    }

    #[test]
    fn test_merge_usage_cache_read_appears_later() {
        let prev = ChatUsage {
            prompt_tokens: 1500,
            completion_tokens: 100,
            total_tokens: 1600,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            metering_usd: None,
        };

        let incoming = ChatUsage {
            prompt_tokens: 500,
            completion_tokens: 200,
            total_tokens: 1700,
            cache_creation_tokens: None,
            cache_read_tokens: Some(1000),
            metering_usd: None,
        };

        let merged = merge_usage(Some(prev), incoming);

        assert_eq!(merged.prompt_tokens, 500);
        assert_eq!(merged.completion_tokens, 200);
        assert_eq!(merged.cache_read_tokens, Some(1000));
        assert_eq!(merged.total_tokens, 1700);
    }

    #[test]
    fn test_merge_usage_prompt_increases_normally() {
        let prev = ChatUsage {
            prompt_tokens: 500,
            completion_tokens: 100,
            total_tokens: 600,
            cache_creation_tokens: None,
            cache_read_tokens: Some(1000),
            metering_usd: None,
        };

        let incoming = ChatUsage {
            prompt_tokens: 600,
            completion_tokens: 150,
            total_tokens: 750,
            cache_creation_tokens: None,
            cache_read_tokens: Some(1000),
            metering_usd: None,
        };

        let merged = merge_usage(Some(prev), incoming);

        assert_eq!(merged.prompt_tokens, 600);
        assert_eq!(merged.completion_tokens, 150);
    }

    #[test]
    fn test_merge_usage_from_none() {
        let incoming = ChatUsage {
            prompt_tokens: 500,
            completion_tokens: 200,
            total_tokens: 700,
            cache_creation_tokens: Some(100),
            cache_read_tokens: Some(200),
            metering_usd: None,
        };

        let merged = merge_usage(None, incoming.clone());

        assert_eq!(merged.prompt_tokens, 500);
        assert_eq!(merged.completion_tokens, 200);
        assert_eq!(merged.cache_creation_tokens, Some(100));
        assert_eq!(merged.cache_read_tokens, Some(200));
    }

    #[test]
    fn test_merge_usage_metering_incoming_wins() {
        use crate::call_validation::MeteringUsd;

        let prev = ChatUsage {
            prompt_tokens: 500,
            completion_tokens: 200,
            total_tokens: 700,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            metering_usd: Some(MeteringUsd {
                prompt_usd: 0.001,
                generated_usd: 0.002,
                cache_read_usd: None,
                cache_creation_usd: None,
                total_usd: 0.003,
            }),
        };

        let incoming = ChatUsage {
            prompt_tokens: 500,
            completion_tokens: 300,
            total_tokens: 800,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            metering_usd: Some(MeteringUsd {
                prompt_usd: 0.002,
                generated_usd: 0.004,
                cache_read_usd: None,
                cache_creation_usd: None,
                total_usd: 0.006,
            }),
        };

        let merged = merge_usage(Some(prev), incoming);

        assert!(merged.metering_usd.is_some());
        assert_eq!(merged.metering_usd.unwrap().total_usd, 0.006);
    }

    /// Helper: simulate accumulator finalization (same logic as run_llm_stream).
    fn finalize_accumulator(acc: ChoiceAccumulator) -> ChoiceFinal {
        let keyed_reasoning = joined_keyed_reasoning(&acc);
        let reasoning = if !acc.reasoning_per_key.is_empty() {
            keyed_reasoning
        } else if !acc.finalized_reasoning_per_block.is_empty() {
            let mut blocks: Vec<_> = acc.finalized_reasoning_per_block.into_iter().collect();
            blocks.sort_by_key(|(block_index, _)| *block_index);
            blocks
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            acc.finalized_reasoning.unwrap_or(acc.reasoning)
        };
        let thinking_blocks = if !acc.thinking_blocks.is_empty() && !reasoning.is_empty() {
            acc.thinking_blocks
                .into_iter()
                .map(|mut block| {
                    if let Some(obj) = block.as_object_mut() {
                        let is_anthropic_thinking =
                            obj.get("type").and_then(|t| t.as_str()) == Some("thinking");
                        let thinking_is_empty = obj
                            .get("thinking")
                            .and_then(|v| v.as_str())
                            .map_or(true, |s| s.trim().is_empty());
                        if is_anthropic_thinking && thinking_is_empty {
                            let block_idx = obj.get("index").and_then(|v| v.as_u64());
                            let reasoning_text = block_idx
                                .and_then(|idx| acc.reasoning_per_block.get(&idx))
                                .unwrap_or(&reasoning);
                            if !reasoning_text.is_empty() {
                                obj.insert("thinking".to_string(), json!(reasoning_text.clone()));
                            }
                        }
                    }
                    block
                })
                .collect()
        } else if acc.thinking_blocks.is_empty() && !reasoning.is_empty() {
            vec![json!({
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": reasoning.clone()}]
            })]
        } else {
            acc.thinking_blocks
        };

        ChoiceFinal {
            content: acc.content,
            raw_content: String::new(),
            reasoning,
            thinking_blocks,
            tool_calls_raw: acc.tool_calls.finalize(),
            citations: acc.citations,
            server_content_blocks: acc.server_content_blocks,
            extra: acc.extra,
            finish_reason: acc.finish_reason,
            usage: acc.usage,
        }
    }

    #[test]
    fn test_litellm_empty_thinking_text_gets_reasoning_merged() {
        // LiteLLM sends signed thinking blocks with empty "thinking": ""
        // because reasoning was already streamed via reasoning_content.
        // The accumulator must merge the accumulated reasoning text in.
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "Let me think about this step by step...".to_string();
        acc.thinking_blocks = vec![json!({
            "type": "thinking",
            "thinking": "",
            "signature": "sig_abc123"
        })];

        let result = finalize_accumulator(acc);

        assert_eq!(result.thinking_blocks.len(), 1);
        assert_eq!(
            result.thinking_blocks[0]["thinking"].as_str().unwrap(),
            "Let me think about this step by step..."
        );
        assert_eq!(result.thinking_blocks[0]["signature"], "sig_abc123");
    }

    #[test]
    fn test_litellm_null_thinking_text_gets_reasoning_merged() {
        // Edge case: thinking field is present but null (not a string)
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "Reasoning text".to_string();
        acc.thinking_blocks = vec![json!({
            "type": "thinking",
            "thinking": null,
            "signature": "sig_xyz"
        })];

        let result = finalize_accumulator(acc);

        assert_eq!(result.thinking_blocks.len(), 1);
        assert_eq!(
            result.thinking_blocks[0]["thinking"].as_str().unwrap(),
            "Reasoning text"
        );
    }

    #[test]
    fn test_anthropic_signature_only_block_gets_reasoning() {
        // Native Anthropic adapter: signature_delta creates blocks with no "thinking" key.
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "Deep analysis here".to_string();
        acc.thinking_blocks = vec![json!({
            "index": 0,
            "type": "thinking",
            "signature": "sig_native"
        })];

        let result = finalize_accumulator(acc);

        assert_eq!(result.thinking_blocks.len(), 1);
        assert_eq!(
            result.thinking_blocks[0]["thinking"].as_str().unwrap(),
            "Deep analysis here"
        );
    }

    #[test]
    fn test_interleaved_thinking_per_block_reasoning() {
        // Anthropic interleaved thinking: multiple thinking blocks at different indices.
        // Each block must get only its own reasoning text, not the concatenation.
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "First thought...Second thought...".to_string();
        acc.reasoning_per_block
            .insert(0, "First thought...".to_string());
        acc.reasoning_per_block
            .insert(4, "Second thought...".to_string());
        acc.thinking_blocks = vec![
            json!({"index": 0, "type": "thinking", "signature": "sig1"}),
            json!({"index": 4, "type": "thinking", "signature": "sig2"}),
        ];

        let result = finalize_accumulator(acc);

        assert_eq!(result.thinking_blocks.len(), 2);
        assert_eq!(
            result.thinking_blocks[0]["thinking"].as_str().unwrap(),
            "First thought...",
            "Block 0 should get only its own reasoning text"
        );
        assert_eq!(
            result.thinking_blocks[1]["thinking"].as_str().unwrap(),
            "Second thought...",
            "Block 4 should get only its own reasoning text"
        );
    }

    #[test]
    fn test_completed_reasoning_summary_replaces_partial_heading() {
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "**Checking parser**".to_string();
        acc.reasoning_per_block
            .insert(0, "**Checking parser**".to_string());
        acc.finalized_reasoning_per_block.insert(
            0,
            "**Checking parser**\n\nThe completed reasoning body.".to_string(),
        );

        let result = finalize_accumulator(acc);

        assert_eq!(
            result.reasoning,
            "**Checking parser**\n\nThe completed reasoning body."
        );
        assert_eq!(
            result.thinking_blocks[0]["summary"][0]["text"],
            "**Checking parser**\n\nThe completed reasoning body."
        );
    }

    #[test]
    fn test_signature_delta_concatenation() {
        // Anthropic sends a single signature_delta per thinking block.
        // Some proxies may emit multiple updates; signature must be treated as
        // an opaque integrity token, so the latest update must replace the prior.
        let mut blocks = vec![json!({
            "index": 0,
            "type": "thinking",
        })];

        // First signature chunk
        merge_thinking_blocks(
            &mut blocks,
            vec![json!({
                "index": 0,
                "type": "thinking",
                "signature": "abc"
            })],
        );
        assert_eq!(blocks[0]["signature"].as_str().unwrap(), "abc");

        // Second signature chunk — should replace, not concatenate
        merge_thinking_blocks(
            &mut blocks,
            vec![json!({
                "index": 0,
                "type": "thinking",
                "signature": "def"
            })],
        );
        assert_eq!(
            blocks[0]["signature"].as_str().unwrap(),
            "def",
            "Signature updates must replace, not concatenate"
        );

        // Third chunk
        merge_thinking_blocks(
            &mut blocks,
            vec![json!({
                "index": 0,
                "type": "thinking",
                "signature": "ghi"
            })],
        );
        assert_eq!(
            blocks[0]["signature"].as_str().unwrap(),
            "ghi",
            "Latest signature update must win"
        );
    }

    #[test]
    fn test_thinking_block_with_existing_text_not_overwritten() {
        // If a thinking block already has non-empty thinking text (e.g., from LiteLLM
        // final chunk that included the text), it should NOT be overwritten.
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "Streamed reasoning".to_string();
        acc.thinking_blocks = vec![json!({
            "type": "thinking",
            "thinking": "Original block text",
            "signature": "sig_keep"
        })];

        let result = finalize_accumulator(acc);

        assert_eq!(result.thinking_blocks.len(), 1);
        assert_eq!(
            result.thinking_blocks[0]["thinking"].as_str().unwrap(),
            "Original block text",
            "Pre-existing thinking text should be preserved"
        );
    }

    #[test]
    fn test_redacted_thinking_blocks_unchanged() {
        // Redacted thinking blocks should pass through without modification.
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "Some reasoning".to_string();
        acc.thinking_blocks = vec![
            json!({"type": "thinking", "signature": "sig1"}),
            json!({"type": "redacted_thinking", "data": "encrypted_blob"}),
        ];

        let result = finalize_accumulator(acc);

        assert_eq!(result.thinking_blocks.len(), 2);
        // thinking block gets reasoning merged
        assert_eq!(
            result.thinking_blocks[0]["thinking"].as_str().unwrap(),
            "Some reasoning"
        );
        // redacted block untouched
        assert_eq!(result.thinking_blocks[1]["type"], "redacted_thinking");
        assert_eq!(result.thinking_blocks[1]["data"], "encrypted_blob");
        assert!(result.thinking_blocks[1].get("thinking").is_none());
    }

    #[test]
    fn test_synthetic_reasoning_block_when_no_thinking_blocks() {
        // When there are no thinking_blocks but reasoning exists,
        // a synthetic reasoning block should be created.
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "Some reasoning from OpenAI".to_string();

        let result = finalize_accumulator(acc);

        assert_eq!(result.thinking_blocks.len(), 1);
        assert_eq!(result.thinking_blocks[0]["type"], "reasoning");
    }

    #[test]
    fn test_whitespace_only_thinking_text_gets_replaced() {
        // Whitespace-only thinking text should be treated as empty.
        let mut acc = ChoiceAccumulator::default();
        acc.reasoning = "Real reasoning".to_string();
        acc.thinking_blocks = vec![json!({
            "type": "thinking",
            "thinking": "   \n\t  ",
            "signature": "sig_ws"
        })];

        let result = finalize_accumulator(acc);

        assert_eq!(
            result.thinking_blocks[0]["thinking"].as_str().unwrap(),
            "Real reasoning",
            "Whitespace-only thinking should be replaced with accumulated reasoning"
        );
    }

    #[test]
    fn test_cache_guard_sanitize_removes_fields() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "x", "cache_control": {"type": "ephemeral"}}]}
            ],
            "temperature": 0.2,
            "max_tokens": 1000,
            "reasoning_effort": "high"
        });

        let sanitized = crate::chat::cache_guard::sanitize_body_for_cache_guard(&body);
        assert!(sanitized.get("temperature").is_none());
        assert!(sanitized.get("max_tokens").is_none());
        assert_eq!(sanitized["reasoning_effort"], "high");
        assert_eq!(sanitized["messages"][0]["content"], serde_json::json!("x"));
    }

    #[test]
    fn test_cache_guard_append_only_prefix_logic() {
        let prev = serde_json::json!({
            "messages": [
                {"role": "user", "content": "a"},
                {"role": "assistant", "content": "b"}
            ],
            "meta": {"chat_id": "c1"}
        });
        let next_ok = serde_json::json!({
            "messages": [
                {"role": "user", "content": "a"},
                {"role": "assistant", "content": "b"},
                {"role": "user", "content": "c"}
            ],
            "meta": {"chat_id": "c1", "request_attempt_id": "r2"}
        });
        let next_bad = serde_json::json!({
            "messages": [
                {"role": "user", "content": "a"},
                {"role": "assistant", "content": "CHANGED"}
            ],
            "meta": {"chat_id": "c1"}
        });

        assert!(crate::chat::cache_guard::is_append_only_prefix(
            &prev, &next_ok
        ));
        assert!(!crate::chat::cache_guard::is_append_only_prefix(
            &prev, &next_bad
        ));
    }

    #[test]
    fn test_merge_thinking_blocks_dedupe_by_index() {
        let mut dst = vec![json!({"index": 0, "type": "thinking", "signature": "sig_v1"})];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({"index": 0, "type": "thinking", "signature": "sig_v2"})],
        );

        assert_eq!(dst.len(), 1, "Same (type, index) should dedupe");
        assert_eq!(
            dst[0]["signature"], "sig_v2",
            "Signature should be updated to latest"
        );
    }

    #[test]
    fn test_merge_litellm_thinking_block_enriches_final_fields() {
        let mut dst = vec![json!({
            "index": 0,
            "type": "thinking",
            "thinking": "",
            "signature": "sig_partial",
            "provider_metadata": {"opaque": "old"},
            "retained_extension": "keep"
        })];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({
                "index": 0,
                "type": "thinking",
                "thinking": "Final LiteLLM reasoning text",
                "signature": "sig_final",
                "provider_metadata": {"opaque": "final", "version": 2},
                "provider_extension": "opaque-value",
                "retained_extension": null
            })],
        );

        assert_eq!(dst.len(), 1, "Matched block must be updated in place");
        assert_eq!(dst[0]["thinking"], "Final LiteLLM reasoning text");
        assert_eq!(dst[0]["signature"], "sig_final");
        assert_eq!(
            dst[0]["provider_metadata"],
            json!({"opaque": "final", "version": 2})
        );
        assert_eq!(dst[0]["provider_extension"], "opaque-value");
        assert_eq!(dst[0]["retained_extension"], "keep");
    }

    #[test]
    fn test_merge_thinking_blocks_streaming_signature_does_not_concat() {
        // Even if the upstream sends multiple signature updates, we must NOT
        // concatenate them: signatures are integrity-checked by the provider.
        let mut dst = vec![json!({"index": 0, "type": "thinking", "signature": "sig_part1"})];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({"index": 0, "type": "thinking", "signature": "sig_part2"})],
        );

        assert_eq!(dst.len(), 1);
        assert_eq!(
            dst[0]["signature"], "sig_part2",
            "Signature must be replaced, not concatenated"
        );
    }

    #[test]
    fn test_merge_thinking_blocks_different_indices_kept() {
        let mut dst = Vec::new();

        merge_thinking_blocks(
            &mut dst,
            vec![
                json!({"index": 0, "type": "thinking", "signature": "sig1"}),
                json!({"index": 4, "type": "thinking", "signature": "sig2"}),
            ],
        );

        assert_eq!(
            dst.len(),
            2,
            "Different indices should produce separate blocks"
        );
    }

    #[test]
    fn test_merge_thinking_blocks_dedupe_by_signature_no_index() {
        // LiteLLM blocks often have no index — dedupe by (type, signature)
        let mut dst = vec![json!({"type": "thinking", "thinking": "text", "signature": "sig_abc"})];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({"type": "thinking", "thinking": "text", "signature": "sig_abc"})],
        );

        assert_eq!(
            dst.len(),
            1,
            "Same (type, signature) without index should dedupe"
        );
    }

    #[test]
    fn test_merge_thinking_blocks_different_types_same_index_not_deduped() {
        let mut dst = vec![json!({"index": 0, "type": "thinking", "signature": "sig1"})];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({"index": 0, "type": "redacted_thinking", "data": "encrypted"})],
        );

        assert_eq!(
            dst.len(),
            2,
            "Different types at same index should not dedupe"
        );
    }

    #[test]
    fn test_merge_thinking_blocks_signature_added_to_existing() {
        // First block has no signature, second adds it
        let mut dst = vec![json!({"index": 0, "type": "thinking"})];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({"index": 0, "type": "thinking", "signature": "sig_new"})],
        );

        assert_eq!(dst.len(), 1);
        assert_eq!(
            dst[0]["signature"], "sig_new",
            "Signature should be added to existing block"
        );
    }

    #[test]
    fn test_merge_thinking_blocks_dedupe_by_id() {
        let mut dst = vec![json!({"id": "block_1", "type": "thinking", "signature": "sig_old"})];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({"id": "block_1", "type": "thinking", "signature": "sig_new"})],
        );

        assert_eq!(dst.len(), 1, "Same id should dedupe");
        assert_eq!(dst[0]["signature"], "sig_new");
    }

    #[test]
    fn test_merge_thinking_blocks_same_id_enriches_sparse_reasoning() {
        // OpenAI Responses: an early sparse reasoning item (id only) must be enriched
        // by the later complete object, and remain exactly one block.
        let mut dst = vec![json!({"id": "rs_1", "type": "reasoning", "summary": []})];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({
                "id": "rs_1",
                "type": "reasoning",
                "encrypted_content": "gAAAA",
                "summary": [{"type": "summary_text", "text": "Complete summary"}]
            })],
        );

        assert_eq!(dst.len(), 1, "Same id must remain exactly one block");
        assert_eq!(dst[0]["encrypted_content"], "gAAAA");
        assert_eq!(
            dst[0]["summary"][0]["text"], "Complete summary",
            "Sparse block must be enriched by the later complete object"
        );
    }

    #[test]
    fn test_two_reasoning_items_same_summary_index_stay_distinct() {
        // Two distinct reasoning item keys both derived with summary_index=0.
        // Each must be present once, in first-seen order.
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();
        push_keyed_reasoning_delta(
            &mut acc,
            &mut ops,
            "First item summary".into(),
            "item:rs_a#s0".into(),
        );
        push_keyed_reasoning_delta(
            &mut acc,
            &mut ops,
            "Second item summary".into(),
            "item:rs_b#s0".into(),
        );

        let joined = joined_keyed_reasoning(&acc);
        assert_eq!(joined, "First item summary\n\nSecond item summary");
        assert_eq!(joined.matches("First item summary").count(), 1);
        assert_eq!(joined.matches("Second item summary").count(), 1);
    }

    #[test]
    fn test_keyed_reasoning_replay_replaces_not_duplicates() {
        // A sequential-cutoff replay finalizes the same key with the full text.
        // The visible joined text must be replaced, not duplicated.
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();
        push_keyed_reasoning_delta(
            &mut acc,
            &mut ops,
            "**Checking parser**".into(),
            "item:rs_a#s0".into(),
        );
        finalize_keyed_reasoning(
            &mut acc,
            &mut ops,
            "**Checking parser**\n\nThe completed reasoning body.".into(),
            "item:rs_a#s0".into(),
        );

        let joined = joined_keyed_reasoning(&acc);
        assert_eq!(
            joined,
            "**Checking parser**\n\nThe completed reasoning body."
        );
        assert_eq!(joined.matches("Checking parser").count(), 1);

        // A later replayed delta for the finalized key must not clobber it.
        push_keyed_reasoning_delta(
            &mut acc,
            &mut ops,
            "**Checking parser**".into(),
            "item:rs_a#s0".into(),
        );
        assert_eq!(
            joined_keyed_reasoning(&acc),
            "**Checking parser**\n\nThe completed reasoning body.",
            "Replayed delta must not clobber finalized keyed reasoning"
        );
    }

    #[test]
    fn test_keyed_reasoning_final_uses_summary_when_no_stream_events() {
        // Only a finalize (from the completed reasoning item's safe summary) arrived.
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();
        finalize_keyed_reasoning(
            &mut acc,
            &mut ops,
            "Final visible summary".into(),
            "item:rs_final#s0".into(),
        );

        let final_ = finalize_accumulator(acc);
        assert_eq!(final_.reasoning, "Final visible summary");
    }

    #[test]
    fn test_keyed_reasoning_duplicate_finalization_is_noop() {
        // Same authoritative content arriving twice (summary-text done + completed item)
        // must not emit a redundant SetReasoning.
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();
        finalize_keyed_reasoning(
            &mut acc,
            &mut ops,
            "Authoritative summary".into(),
            "item:rs_a#s0".into(),
        );
        let ops_after_first = ops.len();
        finalize_keyed_reasoning(
            &mut acc,
            &mut ops,
            "Authoritative summary".into(),
            "item:rs_a#s0".into(),
        );
        assert_eq!(
            ops.len(),
            ops_after_first,
            "Duplicate identical finalization must be a no-op"
        );
    }

    #[test]
    fn test_responses_reasoning_event_sequence_stays_unique_and_survives_completion() {
        let adapter = get_adapter(WireFormat::OpenaiResponses);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();
        let summaries = [
            ("rs_a", 0, "Analyzing search performance"),
            ("rs_b", 1, "Diagnosing redundant file reading"),
            ("rs_c", 2, "Analyzing search performance"),
            ("rs_d", 3, "Diagnosing redundant file reading"),
        ];

        for (item_id, output_index, text) in summaries {
            let delta = json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": item_id,
                "output_index": output_index,
                "summary_index": 0,
                "delta": text
            });
            let done = json!({
                "type": "response.reasoning_summary_text.done",
                "item_id": item_id,
                "output_index": output_index,
                "summary_index": 0,
                "text": text
            });

            assert!(!process_stream_event_data(
                adapter,
                "",
                &delta.to_string(),
                &mut accumulators,
                &mut collector,
                true,
            )
            .unwrap());
            assert!(!process_stream_event_data(
                adapter,
                "",
                &done.to_string(),
                &mut accumulators,
                &mut collector,
                true,
            )
            .unwrap());
            assert!(accumulators[0].reasoning.matches(text).count() <= 1);
        }

        assert_eq!(
            accumulators[0].reasoning,
            "Analyzing search performance\n\nDiagnosing redundant file reading"
        );

        let completed = json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "output": [
                    {"id": "rs_a", "type": "reasoning", "summary": [{"type": "summary_text", "text": "Analyzing search performance"}], "encrypted_content": "encrypted-a"},
                    {"id": "rs_b", "type": "reasoning", "summary": [{"type": "summary_text", "text": "Diagnosing redundant file reading"}], "encrypted_content": "encrypted-b"},
                    {"id": "rs_c", "type": "reasoning", "summary": [{"type": "summary_text", "text": "Analyzing search performance"}], "encrypted_content": "encrypted-c"},
                    {"id": "rs_d", "type": "reasoning", "summary": [{"type": "summary_text", "text": "Diagnosing redundant file reading"}], "encrypted_content": "encrypted-d"}
                ]
            }
        });

        assert!(process_stream_event_data(
            adapter,
            "",
            &completed.to_string(),
            &mut accumulators,
            &mut collector,
            true,
        )
        .unwrap());

        let results = finalize_accumulators(accumulators, &mut collector);
        assert_eq!(
            results[0].reasoning,
            "Analyzing search performance\n\nDiagnosing redundant file reading"
        );
        assert_eq!(results[0].thinking_blocks.len(), 4);
    }

    #[test]
    fn test_responses_completion_only_restores_safe_reasoning_summary() {
        let adapter = get_adapter(WireFormat::OpenaiResponses);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();
        let completed = json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "output": [{
                    "id": "rs_final",
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "Final visible summary"}],
                    "content": [{"type": "reasoning_text", "text": "raw private reasoning"}],
                    "encrypted_content": "encrypted-private-content"
                }]
            }
        });

        assert!(process_stream_event_data(
            adapter,
            "",
            &completed.to_string(),
            &mut accumulators,
            &mut collector,
            true,
        )
        .unwrap());

        let results = finalize_accumulators(accumulators, &mut collector);
        assert_eq!(results[0].reasoning, "Final visible summary");
        assert!(!results[0].reasoning.contains("raw private reasoning"));
        assert!(!results[0].reasoning.contains("encrypted-private-content"));
    }

    #[test]
    fn test_merge_thinking_blocks_no_key_never_dedupes() {
        // Blocks with no id, no index, no signature always append
        let mut dst = vec![json!({"type": "thinking", "thinking": "text1"})];

        merge_thinking_blocks(
            &mut dst,
            vec![json!({"type": "thinking", "thinking": "text2"})],
        );

        assert_eq!(
            dst.len(),
            2,
            "Blocks with no dedup key should always append"
        );
    }

    #[test]
    fn merge_extra_appends_google_cloud_code_parts_across_chunks() {
        let adapter = get_adapter(WireFormat::GoogleCloudCode);
        let mut accumulators = vec![ChoiceAccumulator::default()];
        let mut collector = ReplayCollector::default();
        let first = json!({
            "response": {"candidates": [{"content": {"parts": [{"text": "first"}]}}]}
        });
        let second = json!({
            "response": {"candidates": [{"content": {"parts": [{"text": "second"}]}}]}
        });

        assert!(!process_stream_event_data(
            adapter,
            "",
            &first.to_string(),
            &mut accumulators,
            &mut collector,
            true,
        )
        .unwrap());
        assert!(!process_stream_event_data(
            adapter,
            "",
            &second.to_string(),
            &mut accumulators,
            &mut collector,
            true,
        )
        .unwrap());

        assert_eq!(
            accumulators[0].extra["_google_cloud_code_parts"],
            json!([{"text": "first"}, {"text": "second"}])
        );
    }

    #[test]
    fn test_route_append_content_with_think_tags_single_chunk() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_with_think_tags(
            &mut acc,
            &mut ops,
            "before <think>secret</think> after".to_string(),
            None,
        );

        assert_eq!(acc.content, "before  after");
        assert_eq!(acc.reasoning, "secret");
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[0], DeltaOp::AppendContent { text } if text == "before "));
        assert!(matches!(&ops[1], DeltaOp::AppendReasoning { text } if text == "secret"));
        assert!(matches!(&ops[2], DeltaOp::AppendContent { text } if text == " after"));
    }

    #[test]
    fn test_route_append_content_with_think_tags_split_open_and_close() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_with_think_tags(&mut acc, &mut ops, "before <thi".to_string(), None);
        route_append_content_with_think_tags(&mut acc, &mut ops, "nk>secret</th".to_string(), None);
        route_append_content_with_think_tags(&mut acc, &mut ops, "ink> after".to_string(), None);

        assert_eq!(acc.content, "before  after");
        assert_eq!(acc.reasoning, "secret");
        assert!(!acc.inside_think_tag);
        assert!(acc.pending_think_parse.is_empty());
    }

    #[test]
    fn test_route_append_content_with_think_tags_case_insensitive() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_with_think_tags(
            &mut acc,
            &mut ops,
            "A<THINK>B</THINK>C".to_string(),
            None,
        );

        assert_eq!(acc.content, "AC");
        assert_eq!(acc.reasoning, "B");
    }

    #[test]
    fn test_flush_pending_think_parse_outside_think_keeps_text() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_with_think_tags(&mut acc, &mut ops, "hello <thi".to_string(), None);
        flush_pending_think_parse(&mut acc, &mut ops);

        assert_eq!(acc.content, "hello <thi");
        assert_eq!(acc.reasoning, "");
    }

    #[test]
    fn test_flush_pending_think_parse_inside_think_goes_to_reasoning() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        acc.inside_think_tag = true;
        acc.pending_think_parse = "secret tail".to_string();
        flush_pending_think_parse(&mut acc, &mut ops);

        assert_eq!(acc.content, "");
        assert_eq!(acc.reasoning, "secret tail");
    }

    #[test]
    fn test_route_append_content_with_think_tags_multiple_segments_single_chunk() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_with_think_tags(
            &mut acc,
            &mut ops,
            "a <think>x</think> b <think>y</think> c".to_string(),
            None,
        );

        assert_eq!(acc.content, "a  b  c");
        assert_eq!(acc.reasoning, "xy");
    }

    #[test]
    fn test_route_append_content_with_think_tags_close_without_open_is_content() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        route_append_content_with_think_tags(&mut acc, &mut ops, "a </think> b".to_string(), None);

        assert_eq!(acc.content, "a </think> b");
        assert_eq!(acc.reasoning, "");
    }

    #[test]
    fn test_handle_append_content_delta_indexed_keeps_tags_as_content() {
        let mut acc = ChoiceAccumulator::default();
        let mut ops = Vec::new();

        handle_append_content_delta(
            &mut acc,
            &mut ops,
            "before <think>secret</think> after".to_string(),
            Some(4),
        );

        assert_eq!(acc.content, "before <think>secret</think> after");
        assert_eq!(acc.reasoning, "");
        assert_eq!(
            acc.content_per_block.get(&4).map(|s| s.as_str()),
            Some("before <think>secret</think> after")
        );
    }
}
