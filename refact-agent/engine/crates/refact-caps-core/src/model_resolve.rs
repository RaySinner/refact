use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use refact_core::model_caps::{ModelCapabilities, ResolvedCaps, resolve_model_caps};

use crate::code_assistant_caps::CodeAssistantCaps;
use crate::model_records::{ChatModelRecord, strip_model_from_finetune};

pub fn resolve_model<'a, T>(
    models: &'a IndexMap<String, Arc<T>>,
    model_id: &str,
) -> Result<Arc<T>, String> {
    models
        .get(model_id)
        .or_else(|| models.get(&strip_model_from_finetune(model_id)))
        .cloned()
        .ok_or(format!(
            "Model '{}' not found. Server has the following models: {:?}",
            model_id,
            models.keys()
        ))
}

pub fn resolve_model_caps_for_provider_model_with_bases(
    model_caps: &HashMap<String, ModelCapabilities>,
    model_id: &str,
    base_provider_names: &[String],
) -> Option<ResolvedCaps> {
    if let Some(resolved) = resolve_model_caps(model_caps, model_id) {
        return Some(resolved);
    }

    let Some((provider_name, bare_model_id)) = model_id.split_once('/') else {
        return None;
    };

    let mut aliases = model_caps_provider_aliases(provider_name);
    for base_provider_name in base_provider_names {
        aliases.extend(model_caps_provider_aliases(base_provider_name));
    }
    aliases.sort();
    aliases.dedup();

    for provider_alias in aliases {
        let qualified = format!("{provider_alias}/{bare_model_id}");
        if let Some(resolved) = resolve_model_caps(model_caps, &qualified) {
            return Some(resolved);
        }
    }

    None
}

fn model_caps_provider_aliases(provider_name: &str) -> Vec<String> {
    let mut aliases = vec![provider_name.replace('_', "-")];
    for suffix in ["_responses", "-responses"] {
        if let Some(stripped) = provider_name.strip_suffix(suffix) {
            aliases.push(stripped.to_string());
            aliases.push(stripped.replace('_', "-"));
        }
    }
    if provider_name == "google_gemini" {
        aliases.push("google".to_string());
    }
    aliases.sort();
    aliases.dedup();
    aliases
}

pub fn apply_registry_caps_to_chat_model(record: &mut ChatModelRecord, caps: &ModelCapabilities) {
    if record.pricing.is_none() {
        record.pricing = caps.pricing.clone();
    }

    if !record.live_fields.is_empty() {
        let live = &record.live_fields;
        if live.n_ctx.is_none() && caps.n_ctx > 0 {
            record.base.n_ctx = caps.n_ctx;
        }
        if live.max_output_tokens.is_none() && caps.max_output_tokens > 0 {
            record.max_output_tokens = Some(caps.max_output_tokens);
        }
        if live.supports_tools.is_none() {
            record.supports_tools = caps.supports_tools;
        }
        if live.supports_parallel_tools.is_none() {
            record.supports_parallel_tools = caps.supports_parallel_tools;
        }
        if live.supports_strict_tools.is_none() {
            record.supports_strict_tools = caps.supports_strict_tools;
        }
        if live.supports_multimodality.is_none() {
            record.supports_multimodality = caps.supports_vision
                || caps.supports_video
                || caps.supports_audio
                || caps.supports_pdf;
        }
        if live.supports_clicks.is_none() {
            record.supports_clicks = caps.supports_clicks;
        }
        if live.reasoning_effort_options.is_none() {
            record.reasoning_effort_options = caps.reasoning_effort_options.clone();
        }
        if live.supports_thinking_budget.is_none() {
            record.supports_thinking_budget = caps.supports_thinking_budget;
        }
        if live.supports_adaptive_thinking_budget.is_none() {
            record.supports_adaptive_thinking_budget = caps.supports_adaptive_thinking_budget;
        }
        if live.max_thinking_tokens.is_none() {
            record.max_thinking_tokens = caps.max_thinking_tokens;
        }
        if live.tokenizer.is_none() && !caps.tokenizer.is_empty() {
            record.base.tokenizer = caps.tokenizer.clone();
        }
        if live.default_temperature.is_none() {
            record.default_temperature = caps.default_temperature;
        }
        if live.default_max_tokens.is_none() {
            record.default_max_tokens = caps.default_max_tokens;
        }
        if live.supports_temperature.is_none() {
            record.supports_temperature = caps.supports_temperature;
        }
        if live.supports_web_search.is_none() {
            record.base.supports_web_search = caps.supports_web_search;
        }
        if live.supports_max_completion_tokens.is_none() {
            record.base.supports_max_completion_tokens = caps.supports_max_completion_tokens;
        }
        record.supports_agent = record.supports_tools;
        return;
    }

    if record.base.user_configured {
        if caps.n_ctx > 0 {
            record.base.n_ctx = record.base.n_ctx.min(caps.n_ctx);
        }
        if caps.max_output_tokens > 0 {
            record.max_output_tokens = record
                .max_output_tokens
                .map(|v| v.min(caps.max_output_tokens))
                .or(Some(caps.max_output_tokens));
        }
        if record.base.tokenizer.is_empty() && !caps.tokenizer.is_empty() {
            record.base.tokenizer = caps.tokenizer.clone();
        }
        if record.default_temperature.is_none() {
            record.default_temperature = caps.default_temperature;
        }
        if record.default_max_tokens.is_none() {
            record.default_max_tokens = caps.default_max_tokens;
        }
        record.base.supports_max_completion_tokens = caps.supports_max_completion_tokens;
        return;
    }

    if caps.n_ctx > 0 {
        record.base.n_ctx = if record.base.n_ctx > 0 {
            record.base.n_ctx.min(caps.n_ctx)
        } else {
            caps.n_ctx
        };
    }
    record.base.supports_max_completion_tokens = caps.supports_max_completion_tokens;

    // For live provider-discovered models (ollama, vllm, lmstudio), the provider
    // already reported these booleans accurately. The registry should only add
    // capability knowledge the provider omitted, never remove what the provider reported.
    // For cloud/catalog models the registry is authoritative, and build_chat_model_record
    // already set these from registry caps before this point — so ||= is safe for both.
    record.supports_tools = record.supports_tools || caps.supports_tools;
    record.supports_parallel_tools = record.supports_parallel_tools || caps.supports_parallel_tools;
    record.supports_strict_tools = record.supports_strict_tools || caps.supports_strict_tools;
    record.supports_multimodality = record.supports_multimodality
        || caps.supports_vision
        || caps.supports_video
        || caps.supports_audio
        || caps.supports_pdf;
    record.supports_clicks = record.supports_clicks || caps.supports_clicks;
    record.default_temperature = caps.default_temperature;
    record.default_max_tokens = caps.default_max_tokens;
    if caps.max_output_tokens > 0 {
        record.max_output_tokens = record
            .max_output_tokens
            .map(|v| v.min(caps.max_output_tokens))
            .or(Some(caps.max_output_tokens));
    }

    if !caps.tokenizer.is_empty() {
        record.base.tokenizer = caps.tokenizer.clone();
    }

    record.reasoning_effort_options = caps.reasoning_effort_options.clone();
    record.supports_thinking_budget = caps.supports_thinking_budget;
    record.supports_adaptive_thinking_budget = caps.supports_adaptive_thinking_budget;
    record.supports_agent = record.supports_tools;
    record.supports_temperature = caps.supports_temperature;
    record.base.supports_web_search = caps.supports_web_search;
}

pub fn resolve_chat_model(
    caps: Arc<CodeAssistantCaps>,
    requested_model_id: &str,
) -> Result<Arc<ChatModelRecord>, String> {
    let model_id = if !requested_model_id.is_empty() {
        requested_model_id
    } else {
        &caps.defaults.chat_default_model
    };

    let base_record = resolve_model(&caps.chat_models, model_id)?;

    let base_provider_names = model_id
        .split_once('/')
        .and_then(|(provider_name, _)| caps.provider_base_names.get(provider_name))
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let resolved = resolve_model_caps_for_provider_model_with_bases(
        &caps.model_caps,
        model_id,
        base_provider_names,
    );

    match resolved {
        Some(resolved_caps) => {
            tracing::debug!(
                "Model '{}' resolved via {:?}, matched key: '{}'",
                model_id,
                resolved_caps.source,
                resolved_caps.matched_key
            );
            let mut effective = (*base_record).clone();
            apply_registry_caps_to_chat_model(&mut effective, &resolved_caps.caps);
            Ok(Arc::new(effective))
        }
        None => {
            // Model not in registry (e.g., custom model) - use base_record as-is
            // The base_record already has capabilities from build_chat_model_record
            tracing::debug!(
                "Model '{}' not in model_caps registry, using configured capabilities",
                model_id
            );
            Ok(base_record)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::llm_types::BaseModelRecord;

    fn create_test_caps() -> CodeAssistantCaps {
        let mut caps = CodeAssistantCaps::default();

        let test_model = ChatModelRecord {
            base: BaseModelRecord {
                id: "test-provider/test-model".to_string(),
                n_ctx: 8192,
                ..Default::default()
            },
            ..Default::default()
        };

        caps.chat_models
            .insert("test-provider/test-model".to_string(), Arc::new(test_model));

        caps.defaults.chat_default_model = "test-provider/test-model".to_string();

        caps
    }

    #[test]
    fn test_resolve_chat_model_with_explicit_model() {
        let caps = Arc::new(create_test_caps());
        let result = resolve_chat_model(caps, "test-provider/test-model");

        assert!(result.is_ok());
        let model = result.unwrap();
        assert_eq!(model.base.id, "test-provider/test-model");
    }

    #[test]
    fn test_resolve_chat_model_with_empty_string_uses_default() {
        let caps = Arc::new(create_test_caps());
        let result = resolve_chat_model(caps, "");

        assert!(result.is_ok());
        let model = result.unwrap();
        assert_eq!(model.base.id, "test-provider/test-model");
    }

    #[test]
    fn test_resolve_chat_model_with_nonexistent_model() {
        let caps = Arc::new(create_test_caps());
        let result = resolve_chat_model(caps, "nonexistent-provider/nonexistent-model");

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Model"));
    }

    #[test]
    fn test_resolve_model_generic() {
        let mut models = IndexMap::new();
        let test_model = ChatModelRecord {
            base: BaseModelRecord {
                id: "test/model".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        models.insert("test/model".to_string(), Arc::new(test_model));

        let result = resolve_model(&models, "test/model");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().base.id, "test/model");

        let result = resolve_model(&models, "nonexistent");
        assert!(result.is_err());
    }
}
