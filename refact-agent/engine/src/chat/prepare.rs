pub use refact_tools::chat_prepare::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::global_context::GlobalContext;
    use crate::llm::adapter::{AdapterSettings, LlmWireAdapter};
    use crate::llm::adapters::openai_chat::OpenAiChatAdapter;
    use crate::yaml_configs::customization_types::{ModeConfig, ProjectRegistry};
    use indexmap::IndexMap;
    use refact_chat_history::trajectory_ops::COMPRESSION_REPORT_KIND;
    use refact_tool_api::{ToolDesc, ToolSource, ToolSourceType};
    use serde_json::json;
    use std::collections::HashMap;
    use tokio::sync::Mutex as AMutex;
    use std::sync::Arc;
    use crate::call_validation::{
        ChatContent, ChatMessage, ChatMeta, ReasoningEffort, SamplingParameters,
    };
    use crate::caps::{resolve_chat_model, ChatModelRecord};
    use crate::llm::{LlmRequest, CanonicalToolChoice};
    use crate::llm::params::{CacheControl, ReasoningIntent};
    use crate::scratchpad_abstract::HasTokenizerAndEot;
    use crate::llm::WireFormat;
    use refact_chat_api::FrozenRequestPrefix;
    use refact_chat_api::diagnostics::filter_ui_only_messages;
    use refact_chat_history::trajectory_ops::COMPRESSION_REPORT_ROLE;
    use refact_chat_api::types::ThreadParams;

    #[test]
    fn images_replaced_for_text_only_model() {
        use crate::call_validation::MultimodalElement;
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Multimodal(vec![
                MultimodalElement {
                    m_type: "text".to_string(),
                    m_content: "look at this".to_string(),
                },
                MultimodalElement {
                    m_type: "image/png".to_string(),
                    m_content: "base64data".to_string(),
                },
            ]),
            ..Default::default()
        }];

        let text_only = ChatModelRecord {
            supports_multimodality: false,
            ..Default::default()
        };
        let out = replace_images_for_text_only_model(messages.clone(), &text_only);
        match &out[0].content {
            ChatContent::Multimodal(elements) => {
                assert_eq!(elements[0].m_content, "look at this");
                assert_eq!(elements[1].m_type, "text");
                assert_eq!(elements[1].m_content, TEXT_ONLY_IMAGE_PLACEHOLDER);
            }
            other => panic!("unexpected content: {:?}", other),
        }

        let vision = ChatModelRecord {
            supports_multimodality: true,
            ..Default::default()
        };
        let untouched = replace_images_for_text_only_model(messages, &vision);
        match &untouched[0].content {
            ChatContent::Multimodal(elements) => {
                assert_eq!(elements[1].m_type, "image/png");
            }
            other => panic!("unexpected content: {:?}", other),
        }
    }

    fn make_model_record_effort(effort_options: Option<Vec<&str>>) -> ChatModelRecord {
        ChatModelRecord {
            base: Default::default(),
            default_temperature: Some(0.7),
            reasoning_effort_options: effort_options
                .map(|opts| opts.into_iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }
    }

    fn make_model_record_thinking_budget() -> ChatModelRecord {
        ChatModelRecord {
            base: Default::default(),
            default_temperature: Some(0.7),
            supports_thinking_budget: true,
            ..Default::default()
        }
    }

    fn make_model_record_adaptive() -> ChatModelRecord {
        ChatModelRecord {
            base: Default::default(),
            default_temperature: Some(0.7),
            supports_adaptive_thinking_budget: true,
            reasoning_effort_options: Some(vec![
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "max".to_string(),
            ]),
            ..Default::default()
        }
    }

    fn make_model_record_no_reasoning() -> ChatModelRecord {
        ChatModelRecord {
            base: Default::default(),
            default_temperature: Some(0.7),
            ..Default::default()
        }
    }

    fn make_sampling_params() -> SamplingParameters {
        SamplingParameters {
            max_new_tokens: 4096,
            temperature: Some(1.0),
            reasoning_effort: None,
            thinking: None,
            enable_thinking: None,
            boost_reasoning: false,
            ..Default::default()
        }
    }

    fn handoff_tool_desc() -> ToolDesc {
        ToolDesc {
            name: "handoff_to_mode".to_string(),
            display_name: "Handoff To Mode".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description:
                "Create a new chat in another mode using the current conversation context."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target_mode": {
                        "type": "string",
                        "description": "Target mode ID to hand off to."
                    }
                },
                "required": ["target_mode"]
            }),
            output_schema: None,
            annotations: None,
        }
    }

    fn mode_config(id: &str, title: &str, description: &str) -> ModeConfig {
        ModeConfig {
            schema_version: 1,
            id: id.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            specific: false,
            prompt: String::new(),
            plan_template: String::new(),
            tools: Vec::new(),
            allow_integrations: false,
            allow_mcp: false,
            allow_subagents: false,
            model_defaults: Default::default(),
            tool_confirm: Default::default(),
            thread_defaults: Default::default(),
            ui: Default::default(),
            base: None,
            match_models: None,
            override_config: None,
        }
    }

    async fn gcx_with_modes(modes: Vec<ModeConfig>) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let workspace = crate::files_correction::canonicalize_normalized_path(
            std::env::temp_dir().join(format!("refact-frozen-prefix-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&workspace).unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.clone()];
        let registry = ProjectRegistry {
            modes: modes
                .into_iter()
                .map(|mode| (mode.id.clone(), mode))
                .collect::<HashMap<_, _>>(),
            ..Default::default()
        };
        gcx.project_registry_cache
            .write()
            .unwrap()
            .insert(workspace.clone(), registry);
        gcx
    }

    fn plan_message(mode: &str, version: u32, content: &str) -> ChatMessage {
        crate::chat::internal_roles::plan(mode, version, content, None)
    }

    fn user_message(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText(content.to_string()),
            ..Default::default()
        }
    }

    fn compression_report_message(content: &str) -> ChatMessage {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "compression_report".to_string(),
            json!({
                "kind": COMPRESSION_REPORT_KIND,
                "compression_kind": "llm_segment_summary",
                "source_message_count": 1,
                "summary_model": "test/frozen-model",
                "tokens_before": 100,
                "tokens_after": 20,
                "estimated_tokens_saved": 80,
                "reduction_percent": 80,
            }),
        );
        ChatMessage {
            role: COMPRESSION_REPORT_ROLE.to_string(),
            content: ChatContent::SimpleText(content.to_string()),
            summarization_tier: Some("tier1_llm".to_string()),
            summarized_token_estimate: Some(80),
            extra,
            ..Default::default()
        }
    }

    fn frozen_prefix(
        system_prompt: &str,
        tools_canonical: serde_json::Value,
    ) -> FrozenRequestPrefix {
        FrozenRequestPrefix {
            schema_version: 1,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            system_prompt: Some(system_prompt.to_string()),
            tools_canonical: Some(tools_canonical),
        }
    }

    fn caps_with_model(model_id: &str) -> crate::caps::CodeAssistantCaps {
        let mut caps = crate::caps::CodeAssistantCaps::default();
        caps.chat_models = IndexMap::new();
        caps.chat_models.insert(
            model_id.to_string(),
            Arc::new(ChatModelRecord {
                base: crate::caps::BaseModelRecord {
                    id: model_id.to_string(),
                    name: model_id.to_string(),
                    n_ctx: 8192,
                    tokenizer: "fake".to_string(),
                    ..Default::default()
                },
                supports_tools: true,
                supports_strict_tools: false,
                supports_temperature: true,
                ..Default::default()
            }),
        );
        caps.defaults.chat_default_model = model_id.to_string();
        caps
    }

    async fn gcx_with_model_and_modes(
        model_id: &str,
        modes: Vec<ModeConfig>,
    ) -> Arc<GlobalContext> {
        let gcx = gcx_with_modes(modes).await;
        {
            let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
            let mut state = app.model.caps.write().await;
            state.caps = Some(Arc::new(caps_with_model(model_id)));
            state.last_attempted_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
        }
        gcx
    }

    fn segment_summary_message(content: &str) -> ChatMessage {
        let mut message = ChatMessage::new("assistant".into(), content.into());
        message
            .extra
            .insert("compression".into(), json!({"kind": "llm_segment_summary"}));
        message
    }

    fn reconstructed_report(content: &str) -> ChatMessage {
        let mut message = ChatMessage::new("user".into(), content.into());
        message.message_id = uuid::Uuid::new_v4().to_string();
        refact_core::active_context::make_reconstruction_report(vec![message], Default::default())
            .unwrap()
    }

    async fn prepare_with_prefix(
        gcx: Arc<GlobalContext>,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDesc>,
        frozen_request_prefix: Option<FrozenRequestPrefix>,
    ) -> PreparedChat {
        try_prepare_with_prefix(gcx, messages, tools, frozen_request_prefix)
            .await
            .unwrap()
    }

    async fn try_prepare_with_prefix(
        gcx: Arc<GlobalContext>,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDesc>,
        frozen_request_prefix: Option<FrozenRequestPrefix>,
    ) -> Result<PreparedChat, String> {
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let ctx = app.chat_ctx();
        let model_id = "test/frozen-model";
        let ccx = crate::at_commands::at_commands::at_commands_new_from_app(
            app,
            8192,
            1,
            false,
            messages.clone(),
            "frozen-chat".to_string(),
            None,
            model_id.to_string(),
            None,
            None,
        )
        .await;
        let tokenizer = crate::tokens::cached_tokenizer(
            gcx.clone(),
            &crate::caps::BaseModelRecord {
                id: model_id.to_string(),
                name: model_id.to_string(),
                tokenizer: "fake".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let t = HasTokenizerAndEot::new(tokenizer);
        let thread = ThreadParams {
            model: model_id.to_string(),
            mode: "agent".to_string(),
            include_project_info: false,
            frozen_request_prefix: frozen_request_prefix.clone(),
            ..Default::default()
        };
        let meta = ChatMeta {
            chat_id: "frozen-chat".to_string(),
            chat_mode: "agent".to_string(),
            chat_remote: false,
            current_config_file: String::new(),
            context_tokens_cap: Some(8192),
            include_project_info: false,
            request_attempt_id: "attempt".to_string(),
            worktree: None,
        };
        let mut sampling = SamplingParameters {
            max_new_tokens: 1024,
            ..Default::default()
        };
        let options = ChatPrepareOptions {
            prepend_system_prompt: false,
            allow_at_commands: false,
            allow_tool_prerun: false,
            supports_tools: true,
            frozen_request_prefix,
            ..Default::default()
        };

        prepare_chat_passthrough(
            ctx,
            Arc::new(AMutex::new(ccx)),
            &t,
            messages,
            &thread,
            model_id,
            "agent",
            tools,
            &meta,
            &mut sampling,
            &options,
        )
        .await
    }

    #[tokio::test]
    async fn frozen_build_canonical_openai_tools_enriches_handoff_and_is_deterministic() {
        let gcx = gcx_with_modes(vec![
            mode_config("agent", "Agent", "Do agent work"),
            mode_config("explore", "Explore", "Look around"),
        ])
        .await;
        let tools = vec![handoff_tool_desc()];

        let ctx = crate::app_state::app_state_from_gcx(gcx).await.chat_ctx();
        let first = build_canonical_openai_tools(ctx.clone(), &tools, false, true).await;
        let second = build_canonical_openai_tools(ctx, &tools, false, true).await;

        assert_eq!(first.tools, second.tools);
        let description = first.tools[0]["function"]["description"].as_str().unwrap();
        assert!(description.contains("Available modes:"));
        assert!(description.contains("- agent: Do agent work"));
        assert!(description.contains("- explore: Look around"));
        assert_eq!(
            first.tools[0]["function"]["parameters"]["properties"]["target_mode"]["enum"],
            json!(["agent", "explore"])
        );
    }

    #[tokio::test]
    async fn frozen_prepare_uses_frozen_tools_and_system_verbatim() {
        let frozen_tools = json!([{
            "type": "function",
            "function": {
                "name": "frozen_tool",
                "description": "Frozen tool description",
                "parameters": {"type": "object"}
            }
        }]);
        let prefix = frozen_prefix("FROZEN SYSTEM", frozen_tools.clone());
        let messages = vec![
            ChatMessage::new("system".to_string(), "dynamic system".to_string()),
            ChatMessage::new("user".to_string(), "hello".to_string()),
        ];
        let gcx = gcx_with_model_and_modes(
            "test/frozen-model",
            vec![mode_config("agent", "Agent", "Do agent work")],
        )
        .await;

        let prepared =
            prepare_with_prefix(gcx, messages, vec![handoff_tool_desc()], Some(prefix)).await;

        assert_eq!(
            prepared.llm_request.tools,
            Some(frozen_tools.as_array().unwrap().clone())
        );
        assert_eq!(
            prepared.llm_request.messages[0].content.content_text_only(),
            "FROZEN SYSTEM"
        );
        assert_eq!(
            prepared.limited_messages[0].content.content_text_only(),
            "FROZEN SYSTEM"
        );
    }

    #[tokio::test]
    async fn frozen_prepare_skips_handoff_mode_enrichment() {
        let frozen_tools = json!([{
            "type": "function",
            "function": {
                "name": "handoff_to_mode",
                "description": "Frozen handoff only",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target_mode": {"type": "string", "description": "Frozen target"}
                    }
                }
            }
        }]);
        let prefix = frozen_prefix("Frozen system", frozen_tools.clone());
        let gcx = gcx_with_model_and_modes(
            "test/frozen-model",
            vec![
                mode_config("agent", "Agent", "Do agent work"),
                mode_config("mutated", "Mutated", "Must not enter frozen tools"),
            ],
        )
        .await;

        let prepared = prepare_with_prefix(
            gcx,
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
            vec![handoff_tool_desc()],
            Some(prefix),
        )
        .await;
        let tool = &prepared.llm_request.tools.as_ref().unwrap()[0];

        assert_eq!(tool, &frozen_tools[0]);
        assert!(!tool.to_string().contains("Available modes"));
        assert!(tool["function"]["parameters"]["properties"]["target_mode"]
            .get("enum")
            .is_none());
    }

    #[tokio::test]
    async fn frozen_first_turn_parity_with_stored_prefix() {
        let gcx = gcx_with_model_and_modes(
            "test/frozen-model",
            vec![mode_config("agent", "Agent", "Do agent work")],
        )
        .await;
        let messages = vec![
            ChatMessage::new("system".to_string(), "FIRST SYSTEM".to_string()),
            ChatMessage::new("user".to_string(), "hello".to_string()),
        ];
        let tools = vec![handoff_tool_desc()];

        let first = prepare_with_prefix(gcx.clone(), messages.clone(), tools.clone(), None).await;
        let stored = frozen_prefix(
            "FIRST SYSTEM",
            serde_json::Value::Array(first.llm_request.tools.clone().unwrap()),
        );
        let second = prepare_with_prefix(gcx, messages, tools, Some(stored)).await;

        assert_eq!(first.llm_request.tools, second.llm_request.tools);
        assert_eq!(
            first.llm_request.messages[0].content.content_text_only(),
            second.llm_request.messages[0].content.content_text_only()
        );
    }

    #[tokio::test]
    async fn prepare_filters_compression_reports_from_model_context() {
        let gcx = gcx_with_model_and_modes(
            "test/frozen-model",
            vec![mode_config("agent", "Agent", "Do agent work")],
        )
        .await;
        let messages = vec![
            user_message("archived before compression"),
            reconstructed_report("obsolete reconstructed payload"),
            compression_report_message("archived static diagnostic"),
            reconstructed_report("authoritative reconstructed payload"),
            compression_report_message("visible static diagnostic"),
            user_message("after compression"),
        ];

        let prepared = prepare_with_prefix(gcx, messages, Vec::new(), None).await;
        for projected in [&prepared.llm_request.messages, &prepared.limited_messages] {
            let roles: Vec<_> = projected
                .iter()
                .map(|message| message.role.as_str())
                .collect();
            let texts: Vec<_> = projected
                .iter()
                .map(|message| message.content.content_text_only())
                .collect();
            assert_eq!(roles, vec!["user", "user"]);
            assert_eq!(
                texts,
                vec!["authoritative reconstructed payload", "after compression"]
            );
        }
    }

    #[tokio::test]
    async fn prepare_excludes_archive_even_after_source_mutation() {
        let gcx = gcx_with_model_and_modes(
            "test/frozen-model",
            vec![mode_config("agent", "Agent", "Do agent work")],
        )
        .await;
        let mut messages = vec![
            user_message("archived before"),
            ChatMessage::new("assistant".into(), "huge old assistant output".into()),
            segment_summary_message("archived legacy summary"),
            reconstructed_report("compact reconstructed history"),
            user_message("after"),
        ];
        let before = prepare_with_prefix(gcx.clone(), messages.clone(), Vec::new(), None).await;
        // Archive edits cannot resurrect source text or trigger legacy migration.
        messages[1].content = ChatContent::SimpleText("mutated archive output ".repeat(50));
        messages[1].message_id = "mutated-source-id".into();
        messages[2].extra.insert(
            "summarized_source_message_ids".into(),
            json!(["mutated-source-id"]),
        );
        let after = prepare_with_prefix(gcx, messages, Vec::new(), None).await;
        for projected in [
            &before.llm_request.messages,
            &before.limited_messages,
            &after.llm_request.messages,
            &after.limited_messages,
        ] {
            let texts: Vec<_> = projected
                .iter()
                .map(|message| message.content.content_text_only())
                .collect();
            assert_eq!(texts, vec!["compact reconstructed history", "after"]);
        }
    }

    #[tokio::test]
    async fn prepare_refuses_legacy_summary_without_explicit_rebuild() {
        let gcx = gcx_with_model_and_modes(
            "test/frozen-model",
            vec![mode_config("agent", "Agent", "Do agent work")],
        )
        .await;
        for prefix in [
            user_message("original source"),
            reconstructed_report("older valid payload"),
        ] {
            let result = try_prepare_with_prefix(
                gcx.clone(),
                vec![
                    prefix,
                    segment_summary_message("legacy summary"),
                    user_message("after"),
                ],
                Vec::new(),
                None,
            )
            .await;
            assert_eq!(
                result.err().as_deref(),
                Some("Legacy compressed history needs an explicit rebuild")
            );
        }
    }

    fn default_settings() -> AdapterSettings {
        AdapterSettings {
            api_key: "test-key".to_string(),
            auth_token: String::new(),
            endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
            extra_headers: Default::default(),
            model_name: "gpt-4".to_string(),
            supports_tools: true,
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
    fn system_prompt_does_not_inline_plan() {
        let full_plan = format!("{}\nTAIL_MARKER_DO_NOT_TRUNCATE", "plan line\n".repeat(32));
        let messages = vec![
            ChatMessage::new("system".to_string(), "Base prompt".to_string()),
            ChatMessage::new("user".to_string(), "Go".to_string()),
            plan_message("agent", 7, &full_plan),
        ];
        let req = crate::llm::LlmRequest::new("gpt-4".to_string(), messages);

        let body = OpenAiChatAdapter
            .build_http(&refact_privacy::testing::cleared(req), &default_settings())
            .unwrap()
            .body;
        let wire_messages = body["messages"].as_array().unwrap();

        assert_eq!(
            wire_messages[0],
            json!({"role": "system", "content": "Base prompt"})
        );
        assert_eq!(wire_messages[1], json!({"role": "user", "content": "Go"}));
        assert_eq!(wire_messages[2]["role"], "user");
        assert!(wire_messages[2]["content"]
            .as_str()
            .unwrap()
            .contains("<plan mode=\"agent\" version=\"7\">"));
        assert!(wire_messages[2]["content"]
            .as_str()
            .unwrap()
            .contains(&full_plan));
    }

    #[test]
    fn adapter_emits_plans_once_in_chronological_positions() {
        let current_plan = "OPENAI_WIRE_CURRENT_PLAN_UNIQUE";
        let messages = vec![
            ChatMessage::new("system".to_string(), "Base prompt".to_string()),
            plan_message("agent", 1, "older plan details"),
            ChatMessage::new("user".to_string(), "Go".to_string()),
            plan_message("agent", 2, current_plan),
            ChatMessage::new("user".to_string(), "Continue".to_string()),
        ];
        let req = crate::llm::LlmRequest::new("gpt-4".to_string(), messages);

        let body = OpenAiChatAdapter
            .build_http(&refact_privacy::testing::cleared(req), &default_settings())
            .unwrap()
            .body;
        let wire_messages = body["messages"].as_array().unwrap();

        assert_eq!(
            wire_messages[0],
            json!({"role": "system", "content": "Base prompt"})
        );
        assert_eq!(wire_messages[1]["role"], "user");
        assert!(wire_messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("version=\"1\""));
        assert_eq!(wire_messages[2], json!({"role": "user", "content": "Go"}));
        assert_eq!(wire_messages[3]["role"], "user");
        assert!(wire_messages[3]["content"]
            .as_str()
            .unwrap()
            .contains("version=\"2\""));
        assert_eq!(
            wire_messages[4],
            json!({"role": "user", "content": "Continue"})
        );

        let serialized = body.to_string();
        assert_eq!(serialized.matches(current_plan).count(), 1);
        assert_eq!(serialized.matches("<plan mode=").count(), 2);
        assert!(!serialized.contains("<plan-history>"));
        assert!(!serialized.contains("\"role\":\"plan\""));
    }

    #[test]
    fn test_is_thinking_enabled_with_thinking_json() {
        let mut params = make_sampling_params();
        params.thinking = Some(serde_json::json!({"type": "enabled", "budget_tokens": 1024}));
        assert!(is_thinking_enabled(&params));
    }

    #[test]
    fn test_is_thinking_enabled_with_thinking_disabled() {
        let mut params = make_sampling_params();
        params.thinking = Some(serde_json::json!({"type": "disabled"}));
        assert!(!is_thinking_enabled(&params));
    }

    #[test]
    fn test_is_thinking_enabled_with_reasoning_effort() {
        let mut params = make_sampling_params();
        params.reasoning_effort = Some(ReasoningEffort::Medium);
        assert!(is_thinking_enabled(&params));
    }

    #[test]
    fn test_is_thinking_enabled_with_enable_thinking_true() {
        let mut params = make_sampling_params();
        params.enable_thinking = Some(true);
        assert!(is_thinking_enabled(&params));
    }

    #[test]
    fn test_is_thinking_enabled_with_enable_thinking_false() {
        let mut params = make_sampling_params();
        params.enable_thinking = Some(false);
        assert!(!is_thinking_enabled(&params));
    }

    #[test]
    fn test_is_thinking_enabled_all_none() {
        let params = make_sampling_params();
        assert!(!is_thinking_enabled(&params));
    }

    #[test]
    fn test_strip_thinking_blocks_when_no_reasoning_support() {
        let model = make_model_record_no_reasoning();
        let params = make_sampling_params();
        let msgs = vec![ChatMessage {
            thinking_blocks: Some(vec![serde_json::json!({"type": "thinking"})]),
            content: ChatContent::SimpleText("hello".into()),
            ..Default::default()
        }];
        let result = strip_thinking_blocks_if_disabled(msgs, &params, &model);
        assert!(result[0].thinking_blocks.is_none());
    }

    #[test]
    fn test_strip_thinking_blocks_when_thinking_disabled() {
        let model = make_model_record_thinking_budget();
        let params = make_sampling_params();
        let msgs = vec![ChatMessage {
            thinking_blocks: Some(vec![serde_json::json!({"type": "thinking"})]),
            content: ChatContent::SimpleText("hello".into()),
            ..Default::default()
        }];
        let result = strip_thinking_blocks_if_disabled(msgs, &params, &model);
        assert!(result[0].thinking_blocks.is_none());
    }

    #[test]
    fn test_strip_thinking_blocks_preserves_when_enabled() {
        let model = make_model_record_thinking_budget();
        let mut params = make_sampling_params();
        params.thinking = Some(serde_json::json!({"type": "enabled", "budget_tokens": 1024}));
        let msgs = vec![ChatMessage {
            thinking_blocks: Some(vec![serde_json::json!({"type": "thinking"})]),
            content: ChatContent::SimpleText("hello".into()),
            ..Default::default()
        }];
        let result = strip_thinking_blocks_if_disabled(msgs, &params, &model);
        assert!(result[0].thinking_blocks.is_some());
    }

    #[test]
    fn test_strip_thinking_blocks_preserves_other_fields() {
        let model = make_model_record_no_reasoning();
        let params = make_sampling_params();
        let msgs = vec![ChatMessage {
            role: "assistant".into(),
            content: ChatContent::SimpleText("hello".into()),
            reasoning_content: Some("reasoning".into()),
            thinking_blocks: Some(vec![serde_json::json!({"type": "thinking"})]),
            citations: vec![serde_json::json!({"url": "http://x"})],
            ..Default::default()
        }];
        let result = strip_thinking_blocks_if_disabled(msgs, &params, &model);
        assert_eq!(result[0].role, "assistant");
        assert_eq!(result[0].reasoning_content.as_deref(), Some("reasoning"));
        assert_eq!(result[0].citations.len(), 1);
        assert!(result[0].thinking_blocks.is_none());
    }

    #[test]
    fn test_strip_thinking_blocks_preserves_cloud_code_signatures() {
        let mut model = make_model_record_no_reasoning();
        model.base.wire_format = WireFormat::GoogleCloudCode;
        let params = make_sampling_params();
        let msgs = vec![ChatMessage {
            reasoning_content: Some("hidden reasoning".into()),
            thinking_blocks: Some(vec![
                serde_json::json!({
                    "type": "thinking",
                    "provider": "google_cloud_code",
                    "thinking": "signed reasoning",
                    "signature": "sig-google"
                }),
                serde_json::json!({
                    "type": "thinking",
                    "thinking": "unsigned reasoning"
                }),
            ]),
            content: ChatContent::SimpleText("answer".into()),
            ..Default::default()
        }];

        let result = strip_thinking_blocks_if_disabled(msgs, &params, &model);

        assert_eq!(
            result[0].reasoning_content.as_deref(),
            Some("hidden reasoning")
        );
        let blocks = result[0].thinking_blocks.as_ref().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["provider"], "google_cloud_code");
        assert_eq!(blocks[0]["signature"], "sig-google");
    }

    fn assistant_msg_with_reasoning() -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: ChatContent::SimpleText("visible answer".into()),
            reasoning_content: Some("model thought process".into()),
            thinking_blocks: Some(vec![json!({ "type": "thinking", "thinking": "x" })]),
            citations: vec![json!({ "url": "http://x" })],
            ..Default::default()
        }
    }

    #[test]
    fn strip_reasoning_from_prompt_strips_when_enabled() {
        let messages = vec![
            assistant_msg_with_reasoning(),
            ChatMessage::new("user".to_string(), "follow up".to_string()),
        ];
        let stripped = strip_reasoning_from_prompt_messages(messages, true);
        assert_eq!(stripped[0].reasoning_content, None);
        assert_eq!(stripped[0].thinking_blocks, None);
        // Non-reasoning fields survive the strip.
        assert_eq!(stripped[0].role, "assistant");
        assert_eq!(stripped[0].content.content_text_only(), "visible answer");
        assert_eq!(stripped[0].citations.len(), 1);
    }

    #[test]
    fn strip_reasoning_from_prompt_preserves_when_disabled() {
        let messages = vec![assistant_msg_with_reasoning()];
        let kept = strip_reasoning_from_prompt_messages(messages, false);
        assert_eq!(
            kept[0].reasoning_content.as_deref(),
            Some("model thought process")
        );
        assert_eq!(kept[0].thinking_blocks.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn strip_reasoning_from_prompt_does_not_mutate_input() {
        let original = assistant_msg_with_reasoning();
        let clone = original.clone();
        let _ = strip_reasoning_from_prompt_messages(vec![original.clone()], true);
        // The caller's copy is untouched: we cloned before consuming, so the
        // function only ever sees its own argument.
        assert_eq!(
            clone.reasoning_content.as_deref(),
            Some("model thought process")
        );
        assert_eq!(clone.thinking_blocks.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn strip_reasoning_from_prompt_noop_when_no_reasoning_present() {
        let messages = vec![ChatMessage::new("user".to_string(), "hello".to_string())];
        let stripped = strip_reasoning_from_prompt_messages(messages.clone(), true);
        assert_eq!(stripped.len(), 1);
        assert_eq!(stripped[0].content.content_text_only(), "hello");
        assert_eq!(stripped[0].reasoning_content, None);
        assert_eq!(stripped[0].thinking_blocks, None);
    }

    #[tokio::test]
    async fn prepare_passthrough_strips_reasoning_when_flag_enabled_and_preserves_when_disabled() {
        let model_id = "test/reasoning-model";
        let mut caps = crate::caps::CodeAssistantCaps::default();
        caps.chat_models = IndexMap::new();
        caps.chat_models.insert(
            model_id.to_string(),
            Arc::new(ChatModelRecord {
                base: crate::caps::BaseModelRecord {
                    id: model_id.to_string(),
                    name: model_id.to_string(),
                    n_ctx: 8192,
                    tokenizer: "fake".to_string(),
                    ..Default::default()
                },
                supports_tools: true,
                supports_thinking_budget: true,
                ..Default::default()
            }),
        );
        caps.defaults.chat_default_model = model_id.to_string();

        let gcx = gcx_with_modes(vec![mode_config("agent", "Agent", "Do agent work")]).await;
        {
            let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
            let mut state = app.model.caps.write().await;
            state.caps = Some(Arc::new(caps));
            state.last_attempted_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
        }

        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let tokenizer = crate::tokens::cached_tokenizer(
            gcx.clone(),
            &crate::caps::BaseModelRecord {
                id: model_id.to_string(),
                name: model_id.to_string(),
                tokenizer: "fake".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let t = HasTokenizerAndEot::new(tokenizer);

        let make_messages = || vec![
            ChatMessage::new("user".to_string(), "earlier question".to_string()),
            assistant_msg_with_reasoning(),
            ChatMessage::new("user".to_string(), "next question".to_string()),
        ];

        let meta = ChatMeta {
            chat_id: "test-strip-chat".to_string(),
            chat_mode: "agent".to_string(),
            chat_remote: false,
            current_config_file: String::new(),
            context_tokens_cap: Some(8192),
            include_project_info: false,
            request_attempt_id: "attempt".to_string(),
            worktree: None,
        };

        let options = ChatPrepareOptions {
            prepend_system_prompt: false,
            allow_at_commands: false,
            allow_tool_prerun: false,
            supports_tools: true,
            ..Default::default()
        };

        // 1. When strip_reasoning_from_prompt is default (effective = false)
        let thread_default = ThreadParams {
            model: model_id.to_string(),
            mode: "agent".to_string(),
            ..Default::default()
        };
        assert!(!thread_default.strip_reasoning_from_prompt_effective());

        let ccx = crate::at_commands::at_commands::at_commands_new_from_app(
            app.clone(),
            8192,
            1,
            false,
            make_messages(),
            "test-strip-chat".to_string(),
            None,
            model_id.to_string(),
            None,
            None,
        )
        .await;
        let mut sampling = SamplingParameters {
            max_new_tokens: 1024,
            thinking_budget: Some(2048),
            ..Default::default()
        };

        let prepared_default = prepare_chat_passthrough(
            app.chat_ctx(),
            Arc::new(AMutex::new(ccx)),
            &t,
            make_messages(),
            &thread_default,
            model_id,
            "agent",
            vec![],
            &meta,
            &mut sampling,
            &options,
        )
        .await
        .unwrap();

        assert_eq!(
            prepared_default.llm_request.messages[1]
                .reasoning_content
                .as_deref(),
            Some("model thought process")
        );
        assert!(prepared_default.llm_request.messages[1]
            .thinking_blocks
            .is_some());

        // 2. When strip_reasoning_from_prompt is explicitly true
        let thread_true = ThreadParams {
            model: model_id.to_string(),
            mode: "agent".to_string(),
            strip_reasoning_from_prompt: Some(true),
            ..Default::default()
        };
        assert!(thread_true.strip_reasoning_from_prompt_effective());

        let ccx_true = crate::at_commands::at_commands::at_commands_new_from_app(
            app.clone(),
            8192,
            1,
            false,
            make_messages(),
            "test-strip-chat".to_string(),
            None,
            model_id.to_string(),
            None,
            None,
        )
        .await;
        let mut sampling_true = SamplingParameters {
            max_new_tokens: 1024,
            thinking_budget: Some(2048),
            ..Default::default()
        };

        let prepared_true = prepare_chat_passthrough(
            app.chat_ctx(),
            Arc::new(AMutex::new(ccx_true)),
            &t,
            make_messages(),
            &thread_true,
            model_id,
            "agent",
            vec![],
            &meta,
            &mut sampling_true,
            &options,
        )
        .await
        .unwrap();

        assert_eq!(prepared_true.llm_request.messages[1].reasoning_content, None);
        assert_eq!(prepared_true.llm_request.messages[1].thinking_blocks, None);
    }

    #[tokio::test]
    async fn prepare_strip_reasoning_stateful_api_invalidates_response_id() {
        let model_id = "test/openai-responses-model";
        let mut caps = crate::caps::CodeAssistantCaps::default();
        caps.chat_models = IndexMap::new();
        caps.chat_models.insert(
            model_id.to_string(),
            Arc::new(ChatModelRecord {
                base: crate::caps::BaseModelRecord {
                    id: model_id.to_string(),
                    name: model_id.to_string(),
                    n_ctx: 8192,
                    tokenizer: "fake".to_string(),
                    wire_format: WireFormat::OpenaiResponses,
                    ..Default::default()
                },
                supports_tools: true,
                supports_thinking_budget: true,
                ..Default::default()
            }),
        );
        caps.defaults.chat_default_model = model_id.to_string();

        let gcx = gcx_with_modes(vec![mode_config("agent", "Agent", "Do agent work")]).await;
        {
            let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
            let mut state = app.model.caps.write().await;
            state.caps = Some(Arc::new(caps));
            state.last_attempted_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
        }

        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let tokenizer = crate::tokens::cached_tokenizer(
            gcx.clone(),
            &crate::caps::BaseModelRecord {
                id: model_id.to_string(),
                name: model_id.to_string(),
                tokenizer: "fake".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let t = HasTokenizerAndEot::new(tokenizer);

        let make_messages = || vec![
            ChatMessage::new("user".to_string(), "earlier question".to_string()),
            assistant_msg_with_reasoning(),
            ChatMessage::new("user".to_string(), "next question".to_string()),
        ];

        let meta = ChatMeta {
            chat_id: "test-strip-stateful-chat".to_string(),
            chat_mode: "agent".to_string(),
            chat_remote: false,
            current_config_file: String::new(),
            context_tokens_cap: Some(8192),
            include_project_info: false,
            request_attempt_id: "attempt".to_string(),
            worktree: None,
        };

        let options = ChatPrepareOptions {
            prepend_system_prompt: false,
            allow_at_commands: false,
            allow_tool_prerun: false,
            supports_tools: true,
            ..Default::default()
        };

        let thread = ThreadParams {
            model: model_id.to_string(),
            mode: "agent".to_string(),
            strip_reasoning_from_prompt: Some(true),
            previous_response_id: Some("resp_abc123".to_string()),
            ..Default::default()
        };

        let ccx = crate::at_commands::at_commands::at_commands_new_from_app(
            app.clone(),
            8192,
            1,
            false,
            make_messages(),
            "test-strip-stateful-chat".to_string(),
            None,
            model_id.to_string(),
            None,
            None,
        )
        .await;
        let mut sampling = SamplingParameters {
            max_new_tokens: 1024,
            thinking_budget: Some(2048),
            ..Default::default()
        };

        let prepared = prepare_chat_passthrough(
            app.chat_ctx(),
            Arc::new(AMutex::new(ccx)),
            &t,
            make_messages(),
            &thread,
            model_id,
            "agent",
            vec![],
            &meta,
            &mut sampling,
            &options,
        )
        .await
        .unwrap();

        assert_eq!(
            prepared.llm_request.previous_response_id, None,
            "previous_response_id must be cleared when strip_reasoning is active with stateful API"
        );
        assert_eq!(
            prepared.llm_request.messages[1].reasoning_content, None,
            "reasoning must be stripped from all messages on full resubmission"
        );
        assert_eq!(prepared.llm_request.messages.len(), 3);
    }

    #[test]
    fn test_adapt_sampling_effort_boost_reasoning() {
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        params.temperature = None;
        let model = make_model_record_effort(Some(vec!["low", "medium", "high"]));
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::Medium));
        assert_eq!(params.temperature, Some(0.7));
    }

    #[test]
    fn test_adapt_sampling_effort_preserves_user_temperature() {
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        params.temperature = Some(0.3);
        let model = make_model_record_effort(Some(vec!["low", "medium", "high"]));
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::Medium));
        assert_eq!(params.temperature, Some(0.3));
    }

    #[test]
    fn test_adapt_sampling_effort_takes_precedence() {
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        params.reasoning_effort = Some(ReasoningEffort::High);
        let model = make_model_record_effort(Some(vec!["low", "medium", "high"]));
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn test_adapt_sampling_thinking_budget_boost_reasoning() {
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        params.max_new_tokens = 4096;
        let model = make_model_record_thinking_budget();
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert!(params.thinking_budget.is_some());
        assert!(params.thinking_budget.unwrap() > 0);
        assert!(params.reasoning_effort.is_none());
        assert!(params.thinking.is_none());
        assert!(params.enable_thinking.is_none());
    }

    #[test]
    fn test_adapt_sampling_thinking_budget_explicit_preserved() {
        let mut params = make_sampling_params();
        params.thinking_budget = Some(5000);
        let model = make_model_record_thinking_budget();
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert_eq!(params.thinking_budget, Some(5000));
        assert!(params.reasoning_effort.is_none());
        assert!(params.thinking.is_none());
    }

    #[test]
    fn test_adapt_sampling_thinking_budget_no_boost_no_budget() {
        let mut params = make_sampling_params();
        let model = make_model_record_thinking_budget();
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert!(params.thinking_budget.is_none());
        assert!(params.reasoning_effort.is_none());
    }

    #[test]
    fn test_adapt_sampling_adaptive_boost_reasoning() {
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        let model = make_model_record_adaptive();
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::Medium));
        assert!(params.thinking.is_none());
        assert!(params.enable_thinking.is_none());
    }

    #[test]
    fn test_adapt_sampling_adaptive_preserves_reasoning_effort() {
        let mut params = make_sampling_params();
        params.reasoning_effort = Some(ReasoningEffort::High);
        let model = make_model_record_adaptive();
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn test_adapt_sampling_no_reasoning_clears_all() {
        let mut params = make_sampling_params();
        params.reasoning_effort = Some(ReasoningEffort::High);
        params.thinking = Some(serde_json::json!({"type": "enabled"}));
        params.enable_thinking = Some(true);
        let model = make_model_record_no_reasoning();
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert!(params.reasoning_effort.is_none());
        assert!(params.thinking.is_none());
        assert!(params.enable_thinking.is_none());
    }

    #[test]
    fn test_adapt_sampling_empty_effort_options_clear_reasoning() {
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        params.reasoning_effort = Some(ReasoningEffort::High);
        let model = make_model_record_effort(Some(vec![]));

        adapt_sampling_for_reasoning_models(&mut params, &model);

        assert!(params.reasoning_effort.is_none());
        assert_eq!(
            sampling_params_to_reasoning_intent(&params, &model),
            ReasoningIntent::Off
        );
    }

    #[test]
    fn test_adapt_sampling_stale_effort_cleared_without_boost() {
        let mut params = make_sampling_params();
        params.reasoning_effort = Some(ReasoningEffort::Medium);
        let model = make_model_record_effort(Some(vec!["high", "xhigh"]));

        adapt_sampling_for_reasoning_models(&mut params, &model);

        assert!(params.reasoning_effort.is_none());
    }

    #[test]
    fn test_adapt_sampling_stale_effort_uses_default_with_boost() {
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        params.reasoning_effort = Some(ReasoningEffort::Low);
        let model = make_model_record_effort(Some(vec!["high", "xhigh"]));

        adapt_sampling_for_reasoning_models(&mut params, &model);

        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::XHigh));
    }

    #[test]
    fn test_adapt_sampling_effort_default_to_last_option() {
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        let model = make_model_record_effort(Some(vec!["low", "high"]));
        adapt_sampling_for_reasoning_models(&mut params, &model);
        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn test_no_reasoning_intent_for_no_support() {
        let model = make_model_record_no_reasoning();
        let mut params = make_sampling_params();
        params.boost_reasoning = true;
        let intent = sampling_params_to_reasoning_intent(&params, &model);
        assert_eq!(intent, ReasoningIntent::Off);

        params.reasoning_effort = Some(ReasoningEffort::High);
        let intent = sampling_params_to_reasoning_intent(&params, &model);
        assert_eq!(intent, ReasoningIntent::Off);
    }

    #[test]
    fn test_ui_only_messages_are_filtered_before_prepare_steps() {
        let visible = ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("visible".to_string()),
            ..Default::default()
        };
        let hidden = crate::chat::diagnostics::make_ui_only_error_message(
            "context_length_exceeded: too large",
        );

        let filtered = filter_ui_only_messages(vec![hidden, visible.clone()]);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].role, "user");
        assert_eq!(filtered[0].content.content_text_only(), "visible");
    }

    #[test]
    fn test_chat_prepare_options_default() {
        let opts = ChatPrepareOptions::default();
        assert!(opts.prepend_system_prompt);
        assert!(opts.allow_at_commands);
        assert!(opts.allow_tool_prerun);
        assert!(opts.supports_tools);
    }
}
