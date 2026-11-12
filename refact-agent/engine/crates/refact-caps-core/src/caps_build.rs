use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use tracing::{info, warn};

use refact_core::llm_types::{BaseModelRecord, EmbeddingModelRecord, WireFormat};
use refact_core::model_caps::{
    ModelCapabilities, normalize_tokenizer_or_default, predefined_cloud_tokenizer_for_model,
    resolve_model_caps,
};
use refact_core::provider_types::{
    AvailableModel, CredentialSpec, ImageTokenMode, ModelTypeDefaults, is_legacy_refact_model,
};

use crate::code_assistant_caps::CodeAssistantCaps;
use crate::model_records::{ChatModelRecord, CompletionModelRecord};
use crate::model_resolve::{
    apply_registry_caps_to_chat_model, resolve_model,
    resolve_model_caps_for_provider_model_with_bases,
};

pub const MODEL_BASE_PROVIDER_HEADER: &str = "x-refact-internal-base-provider";

fn resolve_image_token_mode(configured: ImageTokenMode, wire_format: WireFormat) -> ImageTokenMode {
    if configured != ImageTokenMode::Provider {
        return configured;
    }
    match wire_format {
        WireFormat::AnthropicMessages => ImageTokenMode::Detail,
        WireFormat::OpenaiChatCompletions | WireFormat::OpenaiResponses => ImageTokenMode::Tile,
        WireFormat::OllamaNative => ImageTokenMode::Provider,
        WireFormat::GoogleCloudCode => ImageTokenMode::Provider,
    }
}

fn resolve_model_caps_for_provider_model(
    model_caps: &HashMap<String, ModelCapabilities>,
    model_id: &str,
) -> Option<refact_core::model_caps::ResolvedCaps> {
    resolve_model_caps_for_provider_model_with_bases(model_caps, model_id, &[])
}

/// Build ChatModelRecord from an AvailableModel and provider runtime info
pub fn build_chat_model_record(
    provider_name: &str,
    base_provider_names: &[String],
    model: &AvailableModel,
    model_caps: &HashMap<String, ModelCapabilities>,
    runtime_wire_format: WireFormat,
    runtime_endpoint: &str,
    runtime_api_key: &str,
    runtime_auth_token: &str,
    runtime_tokenizer_api_key: &str,
    runtime_extra_headers: &HashMap<String, String>,
    runtime_credential: Option<&CredentialSpec>,
    runtime_supports_cache_control: bool,
) -> ChatModelRecord {
    let prefix = format!("{}/", provider_name);
    let model_id = if model.id.starts_with(&prefix) {
        model.id.clone()
    } else {
        format!("{}/{}", provider_name, model.id)
    };

    let resolved_caps = if provider_name == "vllm" {
        model
            .base_model
            .as_deref()
            .map(str::trim)
            .filter(|base_model| !base_model.is_empty())
            .and_then(|base_model| resolve_model_caps(model_caps, base_model))
    } else {
        None
    }
    .or_else(|| {
        resolve_model_caps_for_provider_model_with_bases(model_caps, &model_id, base_provider_names)
    })
    .or_else(|| {
        if model_id.starts_with("openrouter/") {
            None
        } else {
            resolve_model_caps(model_caps, &model.id)
        }
    });

    let (
        n_ctx,
        supports_tools,
        supports_multimodality,
        reasoning_effort_options,
        supports_thinking_budget,
        supports_adaptive_thinking_budget,
        tokenizer,
        supports_clicks,
        max_output_tokens,
        supports_parallel_tools,
        supports_strict_tools,
    ) = if !model.live_fields.is_empty() {
        (
            model.n_ctx,
            model.supports_tools,
            model.supports_multimodality,
            model.reasoning_effort_options.clone(),
            model.supports_thinking_budget,
            model.supports_adaptive_thinking_budget,
            model.tokenizer.clone().unwrap_or_default(),
            model.supports_clicks,
            model.max_output_tokens,
            model.supports_parallel_tools,
            model.supports_strict_tools,
        )
    } else if let Some(ref resolved) = resolved_caps {
        let caps = &resolved.caps;
        if model.is_custom {
            let clamped_n_ctx = if caps.n_ctx > 0 {
                model.n_ctx.min(caps.n_ctx)
            } else {
                model.n_ctx
            };
            let clamped_max_output = model.max_output_tokens.map(|v| {
                if caps.max_output_tokens > 0 {
                    v.min(caps.max_output_tokens)
                } else {
                    v
                }
            });
            let tok = model
                .tokenizer
                .clone()
                .unwrap_or_else(|| caps.tokenizer.clone());
            (
                clamped_n_ctx,
                model.supports_tools,
                model.supports_multimodality,
                model.reasoning_effort_options.clone(),
                model.supports_thinking_budget,
                model.supports_adaptive_thinking_budget,
                tok,
                caps.supports_clicks,
                clamped_max_output,
                model.supports_parallel_tools,
                model.supports_strict_tools,
            )
        } else {
            let effective_n_ctx = if model.n_ctx > 0 && caps.n_ctx > 0 {
                model.n_ctx.min(caps.n_ctx)
            } else if caps.n_ctx > 0 {
                caps.n_ctx
            } else {
                model.n_ctx
            };
            let effective_max_output = if caps.max_output_tokens > 0 {
                model
                    .max_output_tokens
                    .map(|v| v.min(caps.max_output_tokens))
                    .or(Some(caps.max_output_tokens))
            } else {
                model.max_output_tokens
            };
            (
                effective_n_ctx,
                caps.supports_tools,
                caps.supports_vision
                    || caps.supports_video
                    || caps.supports_audio
                    || caps.supports_pdf,
                caps.reasoning_effort_options.clone(),
                caps.supports_thinking_budget,
                caps.supports_adaptive_thinking_budget,
                model
                    .tokenizer
                    .clone()
                    .unwrap_or_else(|| caps.tokenizer.clone()),
                caps.supports_clicks,
                effective_max_output,
                caps.supports_parallel_tools,
                caps.supports_strict_tools,
            )
        }
    } else {
        // No registry entry for this model: trust whatever the provider reported.
        // supports_clicks defaults to false because click support is a UI-level
        // capability that no local provider currently reports.
        (
            model.n_ctx,
            model.supports_tools,
            model.supports_multimodality,
            model.reasoning_effort_options.clone(),
            model.supports_thinking_budget,
            model.supports_adaptive_thinking_budget,
            model.tokenizer.clone().unwrap_or_default(),
            false,
            model.max_output_tokens,
            model.supports_parallel_tools,
            model.supports_strict_tools,
        )
    };

    let supports_agent = supports_tools;
    let supports_cache_control = if model.live_fields.is_empty() {
        model.supports_cache_control
            || resolved_caps
                .as_ref()
                .is_some_and(|resolved| resolved.caps.supports_cache_control)
    } else {
        model.supports_cache_control
    };
    let effective_wire_format = model.wire_format_override.unwrap_or(runtime_wire_format);
    let effective_endpoint = model
        .endpoint_override
        .as_deref()
        .unwrap_or(runtime_endpoint);
    let endpoint = effective_endpoint.replace("$MODEL", &model.id);

    let endpoint_style = match effective_wire_format {
        WireFormat::AnthropicMessages => "anthropic",
        _ => "openai",
    }
    .to_string();
    let tokenizer = if tokenizer.trim().is_empty() {
        base_provider_names
            .iter()
            .find_map(|base_provider| {
                refact_core::model_caps::predefined_cloud_tokenizer_for_model(
                    base_provider,
                    &model_id,
                )
            })
            .or_else(|| {
                refact_core::model_caps::predefined_cloud_tokenizer_for_model(
                    provider_name,
                    &model_id,
                )
            })
            .unwrap_or("fake")
            .to_string()
    } else {
        refact_core::model_caps::normalize_tokenizer_or_default(
            provider_name,
            &model_id,
            &tokenizer,
        )
    };

    let mut extra_headers = runtime_extra_headers.clone();
    if let Some(base_provider) = base_provider_names.first() {
        extra_headers.insert(
            MODEL_BASE_PROVIDER_HEADER.to_string(),
            base_provider.clone(),
        );
    }

    let image_token_mode = resolve_image_token_mode(model.image_token_mode, effective_wire_format);

    ChatModelRecord {
        base: BaseModelRecord {
            n_ctx,
            name: model.id.clone(),
            id: model_id,
            endpoint,
            endpoint_style,
            completion_endpoint_style: String::new(),
            embedding_endpoint_style: String::new(),
            wire_format: effective_wire_format,
            api_key: runtime_api_key.to_string(),
            credential: runtime_credential.cloned(),
            auth_token: runtime_auth_token.to_string(),
            tokenizer_api_key: runtime_tokenizer_api_key.to_string(),
            extra_headers,
            similar_models: Vec::new(),
            tokenizer,
            enabled: model.enabled,
            experimental: false,
            supports_max_completion_tokens: if model.live_fields.is_empty() {
                resolved_caps
                    .as_ref()
                    .map(|r| r.caps.supports_max_completion_tokens)
                    .unwrap_or(false)
            } else {
                model.supports_max_completion_tokens
            },
            eof_is_done: false,
            supports_web_search: if model.live_fields.is_empty() {
                resolved_caps
                    .as_ref()
                    .map(|r| r.caps.supports_web_search)
                    .unwrap_or(false)
            } else {
                model.supports_web_search
            },
            supports_cache_control: runtime_supports_cache_control && supports_cache_control,
            image_max_side_px: model.image_max_side_px,
            image_preferred_side_px: model.image_preferred_side_px,
            image_token_mode,
            removable: model.is_custom,
            user_configured: model.is_custom,
        },
        scratchpad: String::new(),
        scratchpad_patch: serde_json::Value::Null,
        supports_tools,
        supports_multimodality,
        supports_clicks,
        supports_agent,
        reasoning_effort_options,
        supports_thinking_budget,
        supports_adaptive_thinking_budget,
        max_thinking_tokens: if model.live_fields.is_empty() {
            resolved_caps
                .as_ref()
                .and_then(|r| r.caps.max_thinking_tokens)
        } else {
            model.max_thinking_tokens
        },
        default_temperature: if model.live_fields.is_empty() {
            resolved_caps
                .as_ref()
                .and_then(|r| r.caps.default_temperature)
        } else {
            model.default_temperature
        },
        default_frequency_penalty: None,
        default_max_tokens: if model.live_fields.is_empty() {
            resolved_caps
                .as_ref()
                .and_then(|r| r.caps.default_max_tokens)
        } else {
            model.default_max_tokens
        },
        max_output_tokens,
        supports_parallel_tools,
        supports_strict_tools: if model.live_fields.is_empty() {
            resolved_caps
                .as_ref()
                .map(|r| {
                    if model.is_custom {
                        supports_strict_tools
                    } else {
                        r.caps.supports_strict_tools
                    }
                })
                .unwrap_or(supports_strict_tools)
        } else {
            model.supports_strict_tools
        },
        supports_temperature: if model.live_fields.is_empty() {
            resolved_caps
                .as_ref()
                .map(|r| r.caps.supports_temperature)
                .unwrap_or(true)
        } else {
            model.supports_temperature
        },
        pricing: model.pricing.clone(),
        available_providers: model.available_providers.clone(),
        selected_provider: model.selected_provider.clone(),
        live_fields: model.live_fields.clone(),
    }
}

fn resolve_user_default_chat_model(
    model: &str,
    chat_models: &IndexMap<String, Arc<ChatModelRecord>>,
) -> Option<String> {
    if model.is_empty() {
        return None;
    }
    if chat_models.contains_key(model) {
        return Some(model.to_string());
    }
    if !model.contains('/') {
        for key in chat_models.keys() {
            if let Some(name) = key.split('/').last() {
                if name == model {
                    return Some(key.clone());
                }
            }
        }
    }
    None
}

pub fn apply_user_default_chat_model(
    target: &mut String,
    defaults: &ModelTypeDefaults,
    label: &str,
    chat_models: &IndexMap<String, Arc<ChatModelRecord>>,
) {
    let Some(model) = defaults.model.as_deref() else {
        return;
    };

    let model = model.trim();
    if model.is_empty() {
        target.clear();
        return;
    }

    if is_legacy_refact_model(model) {
        warn!(
            "Legacy Refact Cloud {} default '{}' was reset to none",
            label, model
        );
        target.clear();
        return;
    }

    match resolve_user_default_chat_model(model, chat_models) {
        Some(resolved) => *target = resolved,
        None => {
            warn!(
                "User default {} model '{}' not found in available models; keeping configured value for setup diagnostics",
                label, model
            );
            *target = model.to_string();
        }
    }
}

fn resolve_user_default_completion_model(
    model: &str,
    completion_models: &IndexMap<String, Arc<CompletionModelRecord>>,
) -> Option<String> {
    if model.is_empty() {
        return None;
    }
    if completion_models.contains_key(model) {
        return Some(model.to_string());
    }
    if !model.contains('/') {
        for key in completion_models.keys() {
            if let Some(name) = key.split('/').last() {
                if name == model {
                    return Some(key.clone());
                }
            }
        }
    }
    None
}

pub fn apply_user_default_completion_model(
    target: &mut String,
    model: Option<&str>,
    completion_models: &IndexMap<String, Arc<CompletionModelRecord>>,
) {
    let Some(model) = model else {
        return;
    };

    let model = model.trim();
    if model.is_empty() {
        target.clear();
        return;
    }

    if is_legacy_refact_model(model) {
        warn!(
            "Legacy Refact Cloud completion default '{}' was reset to none",
            model
        );
        target.clear();
        return;
    }

    match resolve_user_default_completion_model(model, completion_models) {
        Some(resolved) => *target = resolved,
        None => {
            warn!(
                "User default completion model '{}' not found in available models; keeping configured value for setup diagnostics",
                model
            );
            *target = model.to_string();
        }
    }
}

fn resolve_user_default_embedding_model(
    model: &str,
    embedding_models: &[EmbeddingModelRecord],
) -> Option<EmbeddingModelRecord> {
    if model.is_empty() {
        return None;
    }

    let mut matches: Vec<&EmbeddingModelRecord> = embedding_models
        .iter()
        .filter(|record| record.base.id == model || record.base.name == model)
        .collect();
    matches.sort_by(|a, b| a.base.id.cmp(&b.base.id));
    matches.first().map(|record| (*record).clone())
}

pub fn apply_user_default_embedding_model(
    target: &mut EmbeddingModelRecord,
    model: Option<&str>,
    embedding_models: &[EmbeddingModelRecord],
) {
    let Some(model) = model else {
        return;
    };

    let model = model.trim();
    if model.is_empty() {
        *target = EmbeddingModelRecord::default();
        return;
    }

    if is_legacy_refact_model(model) {
        warn!(
            "Legacy Refact Cloud embedding default '{}' was reset to none",
            model
        );
        *target = EmbeddingModelRecord::default();
        return;
    }

    match resolve_user_default_embedding_model(model, embedding_models) {
        Some(resolved) => *target = resolved,
        None => warn!(
            "User default embedding model '{}' not found in available models; keeping configured embedding model for setup diagnostics",
            model
        ),
    }
}

fn clear_legacy_refact_chat_defaults(caps: &mut CodeAssistantCaps) {
    let defaults = &mut caps.defaults;
    clear_legacy_refact_chat_default("chat", &mut defaults.chat_default_model);
    clear_legacy_refact_chat_default("chat 2", &mut defaults.chat_model_2);
    clear_legacy_refact_chat_default("task planner agent", &mut defaults.task_planner_agent_model);
    clear_legacy_refact_chat_default("light", &mut defaults.chat_light_model);
    clear_legacy_refact_chat_default("thinking", &mut defaults.chat_thinking_model);
    clear_legacy_refact_chat_default("buddy", &mut defaults.chat_buddy_model);
}

fn clear_legacy_refact_chat_default(label: &str, value: &mut String) {
    if is_legacy_refact_model(value) {
        warn!(
            "Legacy Refact Cloud {} default '{}' was reset to none",
            label, value
        );
        value.clear();
    }
}

pub fn remove_legacy_refact_models_from_caps(caps: &mut CodeAssistantCaps) {
    caps.chat_models.retain(|model_id, _| {
        let keep = !is_legacy_refact_model(model_id);
        if !keep {
            warn!(
                "Legacy Refact Cloud chat model '{}' was removed from caps",
                model_id
            );
        }
        keep
    });

    caps.completion_models.retain(|model_id, _| {
        let keep = !is_legacy_refact_model(model_id);
        if !keep {
            warn!(
                "Legacy Refact Cloud completion model '{}' was removed from caps",
                model_id
            );
        }
        keep
    });

    if is_legacy_refact_model(&caps.embedding_model.base.id)
        || is_legacy_refact_model(&caps.embedding_model.base.name)
    {
        warn!(
            "Legacy Refact Cloud embedding model '{}' was reset to none",
            caps.embedding_model.base.id
        );
        caps.embedding_model = EmbeddingModelRecord::default();
    }

    clear_legacy_refact_chat_defaults(caps);

    if is_legacy_refact_model(&caps.defaults.completion_default_model)
        || (!caps.defaults.completion_default_model.is_empty()
            && !caps
                .completion_models
                .contains_key(&caps.defaults.completion_default_model))
    {
        warn!(
            "Completion default model '{}' was reset to none because it is no longer available",
            caps.defaults.completion_default_model
        );
        caps.defaults.completion_default_model.clear();
    }

    if caps.defaults.completion_default_model.is_empty() && !caps.completion_models.is_empty() {
        let mut candidates: Vec<&String> = caps.completion_models.keys().collect();
        candidates.sort();
        if let Some(first_model_id) = candidates.first() {
            info!(
                "Auto-selecting default completion model after legacy cleanup: {}",
                first_model_id
            );
            caps.defaults.completion_default_model = (*first_model_id).clone();
        }
    }
}

pub fn validate_default_models(caps: &CodeAssistantCaps) -> Result<(), String> {
    validate_default_chat_model(caps, "chat", &caps.defaults.chat_default_model);
    validate_default_chat_model(caps, "chat 2", &caps.defaults.chat_model_2);
    validate_default_chat_model(
        caps,
        "task planner agent",
        &caps.defaults.task_planner_agent_model,
    );
    validate_default_chat_model(caps, "thinking", &caps.defaults.chat_thinking_model);
    validate_default_chat_model(caps, "buddy", &caps.defaults.chat_buddy_model);
    validate_default_chat_model(caps, "light", &caps.defaults.chat_light_model);
    validate_default_completion_model(caps, &caps.defaults.completion_default_model);
    validate_default_embedding_model(caps);
    Ok(())
}

fn validate_default_chat_model(caps: &CodeAssistantCaps, label: &str, model: &str) {
    if model.is_empty() || caps.chat_models.contains_key(model) {
        return;
    }
    if resolve_model_caps_for_provider_model(&caps.model_caps, model).is_none() {
        warn!(
            "Default {} model '{}' is not in chat_models and not found in model capabilities registry",
            label, model
        );
    }
}

fn validate_default_completion_model(caps: &CodeAssistantCaps, model: &str) {
    if model.is_empty() || caps.completion_models.contains_key(model) {
        return;
    }
    warn!(
        "Default completion model '{}' is not in completion_models",
        model
    );
}

fn validate_default_embedding_model(caps: &CodeAssistantCaps) {
    let model = caps.embedding_model.base.id.as_str();
    if model.is_empty() {
        return;
    }
    if is_legacy_refact_model(model) || is_legacy_refact_model(&caps.embedding_model.base.name) {
        warn!(
            "Default embedding model '{}' is legacy or unavailable",
            model
        );
    }
}

pub fn apply_model_caps_to_all_chat_models(caps: &mut CodeAssistantCaps) {
    let model_ids: Vec<String> = caps.chat_models.keys().cloned().collect();
    for model_id in model_ids {
        let base_provider_names = model_id
            .split_once('/')
            .and_then(|(provider_name, _)| caps.provider_base_names.get(provider_name))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if let Some(resolved) = resolve_model_caps_for_provider_model_with_bases(
            &caps.model_caps,
            &model_id,
            base_provider_names,
        ) {
            if let Some(record) = caps.chat_models.get(&model_id) {
                let mut updated = (**record).clone();
                apply_registry_caps_to_chat_model(&mut updated, &resolved.caps);
                caps.chat_models.insert(model_id, Arc::new(updated));
            }
        }
    }
}

pub fn resolve_completion_model<'a>(
    caps: Arc<CodeAssistantCaps>,
    requested_model_id: &str,
) -> Result<Arc<CompletionModelRecord>, String> {
    let model_id = if !requested_model_id.is_empty() {
        requested_model_id
    } else {
        &caps.defaults.completion_default_model
    };

    resolve_model(&caps.completion_models, model_id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use refact_core::model_caps::ModelCapabilities;
    use refact_core::provider_types::{
        CustomModelConfig, LiveModelFields, ModelPricing, available_model_from_catalog_and_live,
        merge_custom_models,
    };

    use super::*;

    #[test]
    fn registry_post_pass_preserves_explicit_live_values_and_fills_absent_fields() {
        let catalog = ModelCapabilities {
            n_ctx: 128_000,
            max_output_tokens: 16_384,
            pricing: Some(ModelPricing {
                prompt: 2.0,
                generated: 8.0,
                cache_read: Some(1.0),
                cache_creation: Some(3.0),
                context_over_200k: None,
            }),
            supports_tools: true,
            supports_parallel_tools: true,
            supports_strict_tools: true,
            supports_clicks: true,
            supports_temperature: true,
            reasoning_effort_options: Some(vec!["high".to_string()]),
            tokenizer: "catalog-tokenizer".to_string(),
            ..Default::default()
        };
        let live = LiveModelFields {
            supports_tools: Some(false),
            supports_temperature: Some(false),
            reasoning_effort_options: Some(Vec::new()),
            tokenizer: Some(String::new()),
            ..Default::default()
        };
        let model =
            available_model_from_catalog_and_live("model", Some(&catalog), &live, true, 4096);
        let mut record = ChatModelRecord {
            base: BaseModelRecord {
                n_ctx: model.n_ctx,
                tokenizer: model.tokenizer.clone().unwrap_or_default(),
                ..Default::default()
            },
            supports_tools: model.supports_tools,
            supports_parallel_tools: model.supports_parallel_tools,
            supports_strict_tools: model.supports_strict_tools,
            supports_clicks: model.supports_clicks,
            reasoning_effort_options: model.reasoning_effort_options.clone(),
            max_output_tokens: model.max_output_tokens,
            supports_temperature: model.supports_temperature,
            live_fields: model.live_fields.clone(),
            ..Default::default()
        };

        apply_registry_caps_to_chat_model(&mut record, &catalog);

        assert!(!record.supports_tools);
        assert!(!record.supports_temperature);
        assert_eq!(record.reasoning_effort_options, Some(Vec::new()));
        assert_eq!(record.base.tokenizer, "");
        assert!(record.supports_parallel_tools);
        assert!(record.supports_strict_tools);
        assert!(record.supports_clicks);
        assert_eq!(record.base.n_ctx, 128_000);
        assert_eq!(record.max_output_tokens, Some(16_384));
        let pricing = record.pricing.as_ref().unwrap();
        assert_eq!(pricing.prompt, 2.0);
        assert_eq!(pricing.generated, 8.0);
        assert_eq!(pricing.cache_read, Some(1.0));
        assert_eq!(pricing.cache_creation, Some(3.0));
    }

    #[test]
    fn caps_payload_serializes_typed_model_pricing_and_omits_unknown_pricing() {
        let priced_caps = ModelCapabilities {
            n_ctx: 128_000,
            pricing: Some(ModelPricing {
                prompt: 2.0,
                generated: 8.0,
                cache_read: Some(1.0),
                cache_creation: Some(3.0),
                context_over_200k: None,
            }),
            ..Default::default()
        };
        let priced_model = AvailableModel::from_caps("priced", &priced_caps, true, None);
        let unpriced_model = AvailableModel::from_caps(
            "unpriced",
            &ModelCapabilities {
                n_ctx: 128_000,
                ..Default::default()
            },
            true,
            None,
        );
        let mut caps = CodeAssistantCaps::default();
        for model in [&priced_model, &unpriced_model] {
            let record = build_chat_model_record(
                "provider",
                &[],
                model,
                &HashMap::new(),
                WireFormat::OpenaiChatCompletions,
                "https://example.com/v1/chat/completions",
                "",
                "",
                "",
                &HashMap::new(),
                None,
                true,
            );
            caps.chat_models
                .insert(record.base.id.clone(), Arc::new(record));
        }

        let payload = serde_json::to_value(caps).unwrap();

        assert_eq!(
            payload["chat_models"]["provider/priced"]["pricing"]["prompt"],
            serde_json::json!(2.0)
        );
        assert_eq!(
            payload["chat_models"]["provider/priced"]["pricing"]["generated"],
            serde_json::json!(8.0)
        );
        assert_eq!(
            payload["chat_models"]["provider/priced"]["pricing"]["cache_read"],
            serde_json::json!(1.0)
        );
        assert_eq!(
            payload["chat_models"]["provider/priced"]["pricing"]["cache_creation"],
            serde_json::json!(3.0)
        );
        assert!(payload["chat_models"]["provider/unpriced"]
            .get("pricing")
            .is_none());
        assert!(payload["metadata"].get("pricing").is_none());
    }

    #[test]
    fn registry_post_pass_preserves_custom_overrides_after_catalog_and_live_merge() {
        let catalog = ModelCapabilities {
            n_ctx: 128_000,
            max_output_tokens: 16_384,
            pricing: Some(ModelPricing {
                prompt: 2.0,
                generated: 8.0,
                cache_read: Some(1.0),
                cache_creation: Some(3.0),
                context_over_200k: None,
            }),
            supports_tools: true,
            supports_parallel_tools: true,
            supports_strict_tools: true,
            supports_vision: true,
            supports_thinking_budget: true,
            supports_adaptive_thinking_budget: true,
            supports_cache_control: true,
            reasoning_effort_options: Some(vec!["high".to_string()]),
            tokenizer: "catalog-tokenizer".to_string(),
            ..Default::default()
        };
        let live = LiveModelFields {
            n_ctx: Some(64_000),
            max_output_tokens: Some(8_192),
            supports_tools: Some(true),
            ..Default::default()
        };
        let mut models = vec![available_model_from_catalog_and_live(
            "model",
            Some(&catalog),
            &live,
            true,
            4096,
        )];
        let custom = CustomModelConfig {
            n_ctx: Some(32_000),
            max_output_tokens: Some(2_048),
            supports_tools: Some(false),
            supports_parallel_tools: Some(false),
            supports_strict_tools: Some(false),
            supports_multimodality: Some(false),
            reasoning_effort_options: Some(Vec::new()),
            supports_thinking_budget: Some(false),
            supports_adaptive_thinking_budget: Some(false),
            supports_cache_control: Some(false),
            tokenizer: Some(String::new()),
            ..Default::default()
        };
        merge_custom_models(
            &mut models,
            &HashMap::from([("model".to_string(), custom)]),
            &HashSet::from(["model"]),
        );
        let model_caps = HashMap::from([("provider/model".to_string(), catalog.clone())]);
        let mut record = build_chat_model_record(
            "provider",
            &[],
            &models[0],
            &model_caps,
            WireFormat::OpenaiChatCompletions,
            "https://example.com/v1/chat/completions",
            "",
            "",
            "",
            &HashMap::new(),
            None,
            true,
        );

        apply_registry_caps_to_chat_model(&mut record, &catalog);

        assert_eq!(record.base.n_ctx, 32_000);
        assert_eq!(record.max_output_tokens, Some(2_048));
        assert!(!record.supports_tools);
        assert!(!record.supports_parallel_tools);
        assert!(!record.supports_strict_tools);
        assert!(!record.supports_multimodality);
        assert_eq!(record.reasoning_effort_options, Some(Vec::new()));
        assert!(!record.supports_thinking_budget);
        assert!(!record.supports_adaptive_thinking_budget);
        assert!(!record.base.supports_cache_control);
        assert_eq!(record.base.tokenizer, "fake");
        let pricing = record.pricing.as_ref().unwrap();
        assert_eq!(pricing.prompt, 2.0);
        assert_eq!(pricing.generated, 8.0);
        assert_eq!(pricing.cache_read, Some(1.0));
        assert_eq!(pricing.cache_creation, Some(3.0));
    }

    #[test]
    fn image_token_mode_uses_effective_provider_wire_format() {
        assert_eq!(
            resolve_image_token_mode(ImageTokenMode::Provider, WireFormat::AnthropicMessages),
            ImageTokenMode::Detail
        );
        assert_eq!(
            resolve_image_token_mode(ImageTokenMode::Provider, WireFormat::OpenaiChatCompletions,),
            ImageTokenMode::Tile
        );
        assert_eq!(
            resolve_image_token_mode(ImageTokenMode::Provider, WireFormat::OpenaiResponses),
            ImageTokenMode::Tile
        );
        assert_eq!(
            resolve_image_token_mode(ImageTokenMode::Provider, WireFormat::OllamaNative),
            ImageTokenMode::Provider
        );
        assert_eq!(
            resolve_image_token_mode(ImageTokenMode::Detail, WireFormat::OpenaiResponses),
            ImageTokenMode::Detail
        );
    }

    #[test]
    fn test_sorted_model_selection_is_deterministic() {
        let mut caps = CodeAssistantCaps::default();

        let model_z = ChatModelRecord {
            base: BaseModelRecord {
                id: "provider/zzz-model".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let model_a = ChatModelRecord {
            base: BaseModelRecord {
                id: "provider/aaa-model".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        caps.chat_models
            .insert("provider/zzz-model".to_string(), Arc::new(model_z));
        caps.chat_models
            .insert("provider/aaa-model".to_string(), Arc::new(model_a));

        let mut sorted_model_ids: Vec<&String> = caps.chat_models.keys().collect();
        sorted_model_ids.sort();

        assert_eq!(sorted_model_ids[0], "provider/aaa-model");
        assert_eq!(sorted_model_ids[1], "provider/zzz-model");
    }

    #[test]
    fn user_completion_default_resolves_provider_qualified_and_bare_models() {
        let mut completion_models = IndexMap::new();
        completion_models.insert(
            "openai/starcoder".to_string(),
            Arc::new(CompletionModelRecord {
                base: BaseModelRecord {
                    id: "openai/starcoder".to_string(),
                    name: "starcoder".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        );

        let mut target = String::new();
        apply_user_default_completion_model(&mut target, Some("starcoder"), &completion_models);
        assert_eq!(target, "openai/starcoder");

        apply_user_default_completion_model(
            &mut target,
            Some("openai/starcoder"),
            &completion_models,
        );
        assert_eq!(target, "openai/starcoder");
    }

    #[test]
    fn user_embedding_default_selects_matching_embedding_record() {
        let embedding_models = vec![
            EmbeddingModelRecord {
                base: BaseModelRecord {
                    id: "openai/text-embedding-3-small".to_string(),
                    name: "text-embedding-3-small".to_string(),
                    ..Default::default()
                },
                embedding_size: 1536,
                ..Default::default()
            },
            EmbeddingModelRecord {
                base: BaseModelRecord {
                    id: "openai_2/text-embedding-3-small".to_string(),
                    name: "text-embedding-3-small".to_string(),
                    ..Default::default()
                },
                embedding_size: 3072,
                ..Default::default()
            },
        ];
        let mut target = EmbeddingModelRecord::default();

        apply_user_default_embedding_model(
            &mut target,
            Some("openai_2/text-embedding-3-small"),
            &embedding_models,
        );
        assert_eq!(target.base.id, "openai_2/text-embedding-3-small");
        assert_eq!(target.embedding_size, 3072);
    }

    #[test]
    fn test_instance_prefixed_model_cap_resolution_uses_base_provider() {
        let mut model_caps = HashMap::new();
        model_caps.insert(
            "openai/gpt-4.1".to_string(),
            ModelCapabilities {
                n_ctx: 128_000,
                supports_tools: true,
                supports_strict_tools: true,
                tokenizer: "openai-tokenizer".to_string(),
                ..Default::default()
            },
        );

        let model = AvailableModel {
            id: "gpt-4.1".to_string(),
            display_name: None,
            n_ctx: 0,
            supports_tools: false,
            supports_parallel_tools: false,
            supports_strict_tools: false,
            supports_multimodality: false,
            supports_video: false,
            supports_audio: false,
            supports_pdf: false,
            supports_clicks: false,
            image_max_side_px: None,
            image_preferred_side_px: None,
            image_token_mode: ImageTokenMode::Provider,
            reasoning_effort_options: None,
            supports_thinking_budget: false,
            supports_adaptive_thinking_budget: false,
            supports_cache_control: true,
            tokenizer: None,
            enabled: true,
            is_custom: false,
            pricing: None,
            available_providers: Vec::new(),
            selected_provider: None,
            max_output_tokens: None,
            provider_variants: Vec::new(),
            wire_format_override: None,
            endpoint_override: None,
            base_model: None,
            max_thinking_tokens: None,
            min_thinking_budget: None,
            supports_temperature: true,
            default_temperature: None,
            default_max_tokens: None,
            supports_web_search: false,
            supports_max_completion_tokens: false,
            supported_parameters: None,
            upstream_provider: None,
            api_mode: None,
            live_fields: Default::default(),
        };

        let record = build_chat_model_record(
            "openai_2",
            &["openai".to_string()],
            &model,
            &model_caps,
            WireFormat::OpenaiChatCompletions,
            "https://api.openai.com/v1/chat/completions",
            "sk-test",
            "",
            "",
            &HashMap::new(),
            None,
            true,
        );

        assert_eq!(record.base.id, "openai_2/gpt-4.1");
        assert_eq!(record.base.n_ctx, 128_000);
        assert_eq!(
            record
                .base
                .extra_headers
                .get(MODEL_BASE_PROVIDER_HEADER)
                .map(String::as_str),
            Some("openai")
        );
        assert_eq!(record.base.tokenizer, "openai-tokenizer");
        assert!(record.supports_tools);
        assert!(record.supports_strict_tools);
    }

    #[test]
    fn test_build_chat_model_record_migrates_missing_tokenizers_to_defaults() {
        let model = AvailableModel {
            id: "gpt-4.1".to_string(),
            display_name: None,
            n_ctx: 128_000,
            supports_tools: false,
            supports_parallel_tools: false,
            supports_strict_tools: false,
            supports_multimodality: false,
            supports_video: false,
            supports_audio: false,
            supports_pdf: false,
            supports_clicks: false,
            image_max_side_px: None,
            image_preferred_side_px: None,
            image_token_mode: ImageTokenMode::Provider,
            reasoning_effort_options: None,
            supports_thinking_budget: false,
            supports_adaptive_thinking_budget: false,
            supports_cache_control: false,
            tokenizer: None,
            enabled: true,
            is_custom: false,
            pricing: None,
            available_providers: Vec::new(),
            selected_provider: None,
            max_output_tokens: None,
            provider_variants: Vec::new(),
            wire_format_override: None,
            endpoint_override: None,
            base_model: None,
            max_thinking_tokens: None,
            min_thinking_budget: None,
            supports_temperature: true,
            default_temperature: None,
            default_max_tokens: None,
            supports_web_search: false,
            supports_max_completion_tokens: false,
            supported_parameters: None,
            upstream_provider: None,
            api_mode: None,
            live_fields: Default::default(),
        };

        let openai_record = build_chat_model_record(
            "openai",
            &[],
            &model,
            &HashMap::new(),
            WireFormat::OpenaiChatCompletions,
            "https://api.openai.com/v1/chat/completions",
            "sk-test",
            "",
            "",
            &HashMap::new(),
            None,
            true,
        );
        assert_eq!(openai_record.base.tokenizer, "fake");

        let mut instance_model = model.clone();
        instance_model.id = "custom-named-model".to_string();
        let instance_record = build_chat_model_record(
            "openai_2",
            &["openai".to_string()],
            &instance_model,
            &HashMap::new(),
            WireFormat::OpenaiChatCompletions,
            "https://api.openai.com/v1/chat/completions",
            "sk-test",
            "",
            "",
            &HashMap::new(),
            None,
            true,
        );
        assert_eq!(instance_record.base.tokenizer, "fake");

        let mut explicit_model = model.clone();
        explicit_model.tokenizer = Some("hf://custom/tokenizer".to_string());
        let mut model_caps = HashMap::new();
        model_caps.insert(
            "openai/gpt-4.1".to_string(),
            ModelCapabilities {
                tokenizer: "fake".to_string(),
                ..Default::default()
            },
        );
        let explicit_record = build_chat_model_record(
            "openai",
            &[],
            &explicit_model,
            &model_caps,
            WireFormat::OpenaiChatCompletions,
            "https://api.openai.com/v1/chat/completions",
            "sk-test",
            "",
            "",
            &HashMap::new(),
            None,
            true,
        );
        assert_eq!(explicit_record.base.tokenizer, "hf://custom/tokenizer");

        let mut unknown_model = model;
        unknown_model.id = "unknown-model".to_string();
        let unknown_record = build_chat_model_record(
            "custom_provider",
            &[],
            &unknown_model,
            &HashMap::new(),
            WireFormat::OpenaiChatCompletions,
            "http://localhost/v1/chat/completions",
            "",
            "",
            "",
            &HashMap::new(),
            None,
            true,
        );
        assert_eq!(unknown_record.base.tokenizer, "fake");
    }

    #[test]
    fn test_build_chat_model_record_uses_registry_cache_control_with_runtime_gate() {
        let mut model_caps = HashMap::new();
        model_caps.insert(
            "anthropic/claude-sonnet-4".to_string(),
            ModelCapabilities {
                n_ctx: 200_000,
                supports_tools: true,
                supports_cache_control: true,
                tokenizer: "fake".to_string(),
                ..Default::default()
            },
        );

        let model = AvailableModel {
            id: "claude-sonnet-4".to_string(),
            display_name: None,
            n_ctx: 0,
            supports_tools: false,
            supports_parallel_tools: false,
            supports_strict_tools: false,
            supports_multimodality: false,
            supports_video: false,
            supports_audio: false,
            supports_pdf: false,
            supports_clicks: false,
            image_max_side_px: None,
            image_preferred_side_px: None,
            image_token_mode: ImageTokenMode::Provider,
            reasoning_effort_options: None,
            supports_thinking_budget: false,
            supports_adaptive_thinking_budget: false,
            supports_cache_control: false,
            tokenizer: None,
            enabled: true,
            is_custom: false,
            pricing: None,
            available_providers: Vec::new(),
            selected_provider: None,
            max_output_tokens: None,
            provider_variants: Vec::new(),
            wire_format_override: None,
            endpoint_override: None,
            base_model: None,
            max_thinking_tokens: None,
            min_thinking_budget: None,
            supports_temperature: true,
            default_temperature: None,
            default_max_tokens: None,
            supports_web_search: false,
            supports_max_completion_tokens: false,
            supported_parameters: None,
            upstream_provider: None,
            api_mode: None,
            live_fields: Default::default(),
        };

        let record = build_chat_model_record(
            "anthropic",
            &["anthropic".to_string()],
            &model,
            &model_caps,
            WireFormat::AnthropicMessages,
            "https://api.anthropic.com/v1/messages",
            "sk-ant",
            "",
            "",
            &HashMap::new(),
            None,
            true,
        );

        assert!(record.base.supports_cache_control);

        let unsupported_runtime_record = build_chat_model_record(
            "anthropic_proxy",
            &["anthropic".to_string()],
            &model,
            &model_caps,
            WireFormat::AnthropicMessages,
            "https://proxy.example/v1/messages",
            "sk-ant",
            "",
            "",
            &HashMap::new(),
            None,
            false,
        );

        assert!(!unsupported_runtime_record.base.supports_cache_control);
    }

    #[test]
    fn test_vllm_caps_resolution_prefers_base_model_root() {
        let mut model_caps = HashMap::new();
        model_caps.insert(
            "served-alias".to_string(),
            ModelCapabilities {
                n_ctx: 1024,
                supports_tools: false,
                tokenizer: "alias-tokenizer".to_string(),
                ..Default::default()
            },
        );
        model_caps.insert(
            "Qwen/Qwen3.6-27B-FP8".to_string(),
            ModelCapabilities {
                n_ctx: 200_000,
                supports_tools: true,
                tokenizer: "root-tokenizer".to_string(),
                ..Default::default()
            },
        );

        let model = AvailableModel {
            id: "served-alias".to_string(),
            display_name: Some("Served Alias".to_string()),
            n_ctx: 131_072,
            supports_tools: false,
            supports_parallel_tools: false,
            supports_strict_tools: false,
            supports_multimodality: false,
            supports_video: false,
            supports_audio: false,
            supports_pdf: false,
            supports_clicks: false,
            image_max_side_px: None,
            image_preferred_side_px: None,
            image_token_mode: ImageTokenMode::Provider,
            reasoning_effort_options: None,
            supports_thinking_budget: false,
            supports_adaptive_thinking_budget: false,
            supports_cache_control: true,
            tokenizer: None,
            enabled: true,
            is_custom: false,
            pricing: None,
            available_providers: Vec::new(),
            selected_provider: None,
            max_output_tokens: None,
            provider_variants: Vec::new(),
            wire_format_override: None,
            endpoint_override: None,
            base_model: Some("Qwen/Qwen3.6-27B-FP8".to_string()),
            max_thinking_tokens: None,
            min_thinking_budget: None,
            supports_temperature: true,
            default_temperature: None,
            default_max_tokens: None,
            supports_web_search: false,
            supports_max_completion_tokens: false,
            supported_parameters: None,
            upstream_provider: None,
            api_mode: None,
            live_fields: Default::default(),
        };

        let record = build_chat_model_record(
            "vllm",
            &[],
            &model,
            &model_caps,
            WireFormat::OpenaiChatCompletions,
            "http://localhost:8000/v1/chat/completions",
            "",
            "",
            "",
            &HashMap::new(),
            None,
            false,
        );

        assert_eq!(record.base.id, "vllm/served-alias");
        assert_eq!(record.base.name, "served-alias");
        assert_eq!(record.base.tokenizer, "root-tokenizer");
        assert_eq!(record.base.n_ctx, 131_072);
        assert!(record.supports_tools);
    }

    #[test]
    fn test_build_chat_model_record_uses_models_dev_available_model_runtime_overrides() {
        let mut model = AvailableModel::from_caps(
            "qwen-override",
            &ModelCapabilities {
                n_ctx: 128_000,
                supports_tools: true,
                tokenizer: "fake".to_string(),
                ..Default::default()
            },
            true,
            None,
        );
        model.wire_format_override = Some(WireFormat::OpenaiResponses);
        model.endpoint_override =
            Some("https://dashscope.aliyuncs.com/model-specific/v1/responses".to_string());

        let record = build_chat_model_record(
            "qwen",
            &[],
            &model,
            &HashMap::new(),
            WireFormat::OpenaiChatCompletions,
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
            "test-key",
            "",
            "",
            &HashMap::new(),
            None,
            true,
        );

        assert_eq!(record.base.id, "qwen/qwen-override");
        assert_eq!(record.base.wire_format, WireFormat::OpenaiResponses);
        assert_eq!(
            record.base.endpoint,
            "https://dashscope.aliyuncs.com/model-specific/v1/responses"
        );
        assert_eq!(record.base.api_key, "test-key");
    }

    #[test]
    fn custom_chat_record_carries_command_credential_without_executing_it() {
        let model = AvailableModel::from_caps(
            "custom-model",
            &ModelCapabilities {
                n_ctx: 8_192,
                tokenizer: "fake".to_string(),
                ..Default::default()
            },
            true,
            None,
        );
        let credential: CredentialSpec =
            serde_yaml::from_str("type: command\ncommand: helper-that-must-not-run-during-caps\n")
                .unwrap();

        let record = build_chat_model_record(
            "custom_2",
            &["custom".to_string()],
            &model,
            &HashMap::new(),
            WireFormat::OpenaiChatCompletions,
            "https://example.com/v1/chat/completions",
            "",
            "",
            "",
            &HashMap::new(),
            Some(&credential),
            true,
        );

        assert_eq!(record.base.credential, Some(credential));
        assert!(serde_json::to_value(&record.base)
            .unwrap()
            .get("credential")
            .is_none());
    }
}
