pub use refact_runtime_api::cloud_token_count::*;

#[cfg(test)]
use refact_core::llm_types::BaseModelRecord;
#[cfg(test)]
use refact_core::chat_types::ChatUsage;
#[cfg(test)]
use refact_core::model_caps::ANTHROPIC_CLOUD_TOKENIZER;
#[cfg(test)]
use refact_llm::adapter::LlmWireAdapter;
#[cfg(test)]
use refact_llm::adapter::{AdapterSettings, WireFormat};
#[cfg(test)]
use refact_llm::canonical::LlmRequest;
#[cfg(test)]
use refact_llm::ReasoningIntent;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_validation::ChatMessage;
    use refact_privacy::PrivacyPolicy;
    use serde_json::json;

    fn req() -> LlmRequest {
        LlmRequest::new(
            "openai/gpt-4.1".to_string(),
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        )
    }

    fn base_model(tokenizer: &str, wire_format: WireFormat, endpoint: &str) -> BaseModelRecord {
        BaseModelRecord {
            id: "openai/gpt-4.1".to_string(),
            name: "gpt-4.1".to_string(),
            tokenizer: tokenizer.to_string(),
            wire_format,
            endpoint: endpoint.to_string(),
            api_key: "sk-test".to_string(),
            supports_cache_control: true,
            ..Default::default()
        }
    }

    fn test_settings(model: &BaseModelRecord) -> AdapterSettings {
        AdapterSettings {
            api_key: model.api_key.clone(),
            auth_token: model.auth_token.clone(),
            endpoint: model.endpoint.clone(),
            extra_headers: model.extra_headers.clone(),
            model_name: model.name.clone(),
            supports_tools: true,
            supports_reasoning: true,
            reasoning_type: None,
            supports_temperature: true,
            supports_max_completion_tokens: model.supports_max_completion_tokens,
            eof_is_done: model.eof_is_done,
            supports_web_search: model.supports_web_search,
            supports_cache_control: model.supports_cache_control,
        }
    }

    fn cleared(
        request: LlmRequest,
        model: &BaseModelRecord,
    ) -> refact_privacy::Cleared<LlmRequest> {
        let policy = PrivacyPolicy {
            blocked: Vec::new(),
            zones: vec![refact_privacy::Zone {
                name: "normal".to_string(),
                patterns: vec!["*".to_string()],
                send_to: vec!["*".to_string()],
                on_shell_read: refact_privacy::ShellBehavior::Withhold,
            }],
            subagents: refact_privacy::SubagentPolicy::default(),
            ..Default::default()
        };
        let compiled = policy.compile().unwrap();
        refact_privacy::clear(
            request,
            &refact_privacy::Destination {
                id: refact_privacy::DestinationId("openai".to_string()),
                kind: refact_privacy::DestinationKind::Provider,
                display_name: model.id.clone(),
            },
            &compiled,
        )
        .unwrap()
    }

    #[test]
    fn anthropic_count_parts_use_messages_count_tokens_endpoint() {
        let model = base_model(
            ANTHROPIC_CLOUD_TOKENIZER,
            WireFormat::AnthropicMessages,
            "https://api.anthropic.com/v1/messages",
        );
        let settings = test_settings(&model);
        let cleared = cleared(req(), &model);
        let mut parts = crate::llm::adapters::anthropic::AnthropicAdapter
            .build_http(&cleared, &settings)
            .unwrap();
        parts.url = anthropic_count_url(&settings.endpoint).unwrap();
        strip_anthropic_count_unsupported_fields(&mut parts.body);

        assert_eq!(
            parts.url,
            "https://api.anthropic.com/v1/messages/count_tokens"
        );
        assert_eq!(parts.body["model"], json!("gpt-4.1"));
        assert!(parts.body.get("messages").is_some());
        assert!(parts.body.get("max_tokens").is_none());
        assert!(parts.body.get("temperature").is_none());
    }

    #[test]
    fn anthropic_count_accepts_already_count_endpoint() {
        let model = base_model(
            ANTHROPIC_CLOUD_TOKENIZER,
            WireFormat::AnthropicMessages,
            "https://api.anthropic.com/v1/messages/count_tokens",
        );

        assert_eq!(
            anthropic_count_url(&model.endpoint).unwrap(),
            "https://api.anthropic.com/v1/messages/count_tokens"
        );
    }

    #[test]
    fn anthropic_count_reserve_uses_adapter_adjusted_max_tokens() {
        let model = base_model(
            ANTHROPIC_CLOUD_TOKENIZER,
            WireFormat::AnthropicMessages,
            "https://api.anthropic.com/v1/messages",
        );
        let mut request = req();
        request.reasoning = ReasoningIntent::BudgetTokens(2048);
        request.params.max_tokens = 1024;
        let settings = test_settings(&model);
        let cleared = cleared(request, &model);

        let parts = crate::llm::adapters::anthropic::AnthropicAdapter
            .build_http(&cleared, &settings)
            .unwrap();
        let output_token_reserve = body_usize(&parts.body, "max_tokens").unwrap();

        assert_eq!(output_token_reserve, 3072);
    }

    #[test]
    fn cloud_context_limit_detects_exact_count_overflow() {
        let model = BaseModelRecord {
            id: "openai/gpt-4.1".to_string(),
            n_ctx: 100,
            ..Default::default()
        };
        let usage = ChatUsage {
            prompt_tokens: 101,
            total_tokens: 101,
            ..Default::default()
        };

        assert!(cloud_input_exceeds_context(&usage, model.n_ctx, 0));
        assert!(cloud_context_limit_message(&usage, &model, model.n_ctx, 0).contains("exceeding"));
    }

    #[test]
    fn cloud_context_limit_reserves_requested_output_tokens() {
        let usage = ChatUsage {
            prompt_tokens: 90,
            total_tokens: 90,
            ..Default::default()
        };

        assert!(!cloud_input_exceeds_context(&usage, 100, 10));
        assert!(cloud_input_exceeds_context(&usage, 100, 11));
    }

    #[tokio::test]
    async fn spawn_input_token_count_task_returns_none_for_unsupported_tokenizer() {
        let ctx = crate::global_context::tests::test_chat_ctx().await;
        let model = base_model(
            "tiktoken",
            WireFormat::OpenaiChatCompletions,
            "https://api.openai.com/v1/chat/completions",
        );
        let task = spawn_input_token_count_task(ctx, reqwest::Client::new(), &req(), &model, None);
        assert!(task.is_none());
    }
}
