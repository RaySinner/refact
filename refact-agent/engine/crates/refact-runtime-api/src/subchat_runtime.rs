use tracing::info;

use refact_caps_core::model_records::ChatModelRecord;
use refact_caps_core::model_resolve::{resolve_chat_model, resolve_model};
use refact_chat_api::SubchatParameters;
use refact_core::chat_types::{CacheControl, ChatModelType, ReasoningEffort};

use crate::chat_ctx::ChatCtx;

pub fn scale_subchat_budget(value: usize, new_n_ctx: usize, old_n_ctx: usize) -> usize {
    if value == 0 || old_n_ctx == 0 || new_n_ctx >= old_n_ctx {
        return value;
    }

    (((value as u128) * (new_n_ctx as u128)) / (old_n_ctx as u128)) as usize
}

pub fn parse_subchat_cache_control(
    tool_name: &str,
    value: Option<&str>,
) -> Result<CacheControl, String> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(CacheControl::Ephemeral),
        Some(value) if value.eq_ignore_ascii_case("off") => Ok(CacheControl::Off),
        Some(value) if value.eq_ignore_ascii_case("ephemeral") => Ok(CacheControl::Ephemeral),
        Some(value) => Err(format!(
            "invalid cache_control '{}' for '{}', expected: off, ephemeral",
            value, tool_name
        )),
    }
}

pub fn normalize_subchat_params_for_model(
    tool_name: &str,
    params: &mut SubchatParameters,
    model_rec: &ChatModelRecord,
) {
    let requested_n_ctx = params.subchat_n_ctx;
    let requested_max_new_tokens = params.subchat_max_new_tokens;
    let requested_tokens_for_rag = params.subchat_tokens_for_rag;

    if model_rec.base.n_ctx > 0 && params.subchat_n_ctx > model_rec.base.n_ctx {
        params.subchat_n_ctx = model_rec.base.n_ctx;

        if requested_tokens_for_rag > 0 {
            params.subchat_max_new_tokens = scale_subchat_budget(
                requested_max_new_tokens,
                params.subchat_n_ctx,
                requested_n_ctx,
            )
            .max(1);
            params.subchat_tokens_for_rag = scale_subchat_budget(
                requested_tokens_for_rag,
                params.subchat_n_ctx,
                requested_n_ctx,
            );
        }

        info!(
            "normalized subchat '{}' budget for model '{}' from n_ctx={} to n_ctx={}, max_new_tokens={}, tokens_for_rag={}",
            tool_name,
            model_rec.base.id,
            requested_n_ctx,
            params.subchat_n_ctx,
            params.subchat_max_new_tokens,
            params.subchat_tokens_for_rag,
        );
    }

    if let Some(max_output_tokens) = model_rec.max_output_tokens.filter(|v| *v > 0) {
        if params.subchat_max_new_tokens > max_output_tokens {
            params.subchat_max_new_tokens = max_output_tokens;
        }
    }

    if params.subchat_n_ctx > 1 {
        params.subchat_max_new_tokens = params.subchat_max_new_tokens.min(params.subchat_n_ctx - 1);
    }

    let available_for_rag = params
        .subchat_n_ctx
        .saturating_sub(params.subchat_max_new_tokens)
        .saturating_sub(1);
    if params.subchat_tokens_for_rag > available_for_rag {
        params.subchat_tokens_for_rag = available_for_rag;
    }
}

pub async fn resolve_subchat_params(
    ctx: &ChatCtx,
    tool_name: &str,
) -> Result<SubchatParameters, String> {
    let subagent_config = ctx
        .mode_config
        .get_subagent_config(tool_name, None)
        .await
        .ok_or_else(|| {
            format!(
                "subchat params for '{}' not found in subagents registry",
                tool_name
            )
        })?;

    let subchat = &subagent_config.subchat;

    let model_type = match subchat.model_type.as_deref() {
        Some(mt) if mt.eq_ignore_ascii_case("light") => ChatModelType::Light,
        Some(mt) if mt.eq_ignore_ascii_case("thinking") => ChatModelType::Thinking,
        Some(mt) if mt.eq_ignore_ascii_case("default") => ChatModelType::Default,
        Some(mt) if mt.eq_ignore_ascii_case("buddy") => ChatModelType::Buddy,
        Some(mt) if mt.eq_ignore_ascii_case("model_2") => ChatModelType::Model2,
        Some(mt) if mt.eq_ignore_ascii_case("task_planner") => ChatModelType::TaskPlanner,
        Some(mt) => {
            return Err(format!(
                "invalid model_type '{}' for '{}', expected: light, default, thinking, buddy, model_2, task_planner",
                mt, tool_name
            ))
        }
        None => ChatModelType::Default,
    };

    let reasoning_effort = match subchat.reasoning_effort.as_deref() {
        Some(re) => match ReasoningEffort::from_str_opt(re) {
            Some(effort) => Some(effort),
            None => return Err(format!(
                "invalid reasoning_effort '{}' for '{}', expected: none, minimal, low, medium, high, xhigh, max",
                re, tool_name
            )),
        },
        None => None,
    };
    let cache_control = parse_subchat_cache_control(tool_name, subchat.cache_control.as_deref())?;

    let mut params = SubchatParameters {
        subchat_model_type: model_type,
        subchat_model: subchat.model.clone().unwrap_or_default(),
        subchat_n_ctx: subchat.n_ctx.unwrap_or(0),
        subchat_max_new_tokens: subchat.max_new_tokens.unwrap_or(0),
        subchat_temperature: subchat.temperature,
        subchat_tokens_for_rag: subchat.tokens_for_rag.unwrap_or(0),
        subchat_reasoning_effort: reasoning_effort,
        subchat_cache_control: cache_control,
    };

    if params.subchat_n_ctx == 0 {
        return Err(format!(
            "subchat_n_ctx must be > 0 for tool '{}'",
            tool_name
        ));
    }
    if params.subchat_max_new_tokens == 0 {
        return Err(format!(
            "subchat_max_new_tokens must be > 0 for tool '{}'",
            tool_name
        ));
    }

    let model = resolve_subchat_model_for_tool(ctx, tool_name, &params).await?;
    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| format!("failed to load caps: {:?}", e))?;
    let model_rec = resolve_chat_model(caps, &model)?;
    normalize_subchat_params_for_model(tool_name, &mut params, &model_rec);

    Ok(params)
}

pub async fn resolve_subchat_model(
    ctx: &ChatCtx,
    params: &SubchatParameters,
) -> Result<String, String> {
    resolve_subchat_model_inner(ctx, params, None).await
}

pub async fn resolve_subchat_model_for_tool(
    ctx: &ChatCtx,
    tool_name: &str,
    params: &SubchatParameters,
) -> Result<String, String> {
    resolve_subchat_model_inner(ctx, params, Some(tool_name)).await
}

pub async fn resolve_subchat_model_inner(
    ctx: &ChatCtx,
    params: &SubchatParameters,
    tool_name: Option<&str>,
) -> Result<String, String> {
    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| format!("failed to load caps: {:?}", e))?;

    if !params.subchat_model.is_empty() {
        let model_rec = resolve_chat_model(caps, &params.subchat_model).map_err(|e| {
            subchat_explicit_model_error(tool_name, &params.subchat_model, "is not available", &e)
        })?;
        if let Some(reason) = llm_endpoint_unusable_reason(&model_rec.base.endpoint) {
            return Err(subchat_explicit_model_error(
                tool_name,
                &params.subchat_model,
                "is misconfigured",
                &reason,
            ));
        }
        return Ok(model_rec.base.id.clone());
    }

    let model_id = match params.subchat_model_type {
        ChatModelType::Light => &caps.defaults.chat_light_model,
        ChatModelType::Default => &caps.defaults.chat_default_model,
        ChatModelType::Thinking => &caps.defaults.chat_thinking_model,
        ChatModelType::Buddy => &caps.defaults.chat_buddy_model,
        ChatModelType::Model2 => &caps.defaults.chat_model_2,
        ChatModelType::TaskPlanner => &caps.defaults.task_planner_agent_model,
    };
    let model_label = subchat_model_type_label(params.subchat_model_type);

    if model_id.trim().is_empty() {
        return Err(format!(
            "{} is not set up. Go to Default model settings and configure {}.",
            subchat_model_requirement_label(tool_name, params.subchat_model_type),
            model_label
        ));
    }

    let model_rec = resolve_model(&caps.chat_models, model_id).map_err(|e| {
        format!(
            "{} '{}' is not available: {}. Go to Default model settings and configure {}.",
            subchat_model_requirement_label(tool_name, params.subchat_model_type),
            model_id,
            e,
            model_label
        )
    })?;

    if let Some(reason) = llm_endpoint_unusable_reason(&model_rec.base.endpoint) {
        return Err(format!(
            "{} '{}' is misconfigured: {}. Go to Default model settings and configure {}.",
            subchat_model_requirement_label(tool_name, params.subchat_model_type),
            model_id,
            reason,
            model_label
        ));
    }

    Ok(model_rec.base.id.clone())
}

pub fn subchat_explicit_model_error(
    tool_name: Option<&str>,
    model: &str,
    state: &str,
    reason: &str,
) -> String {
    match tool_name {
        Some(tool_name) => format!(
            "Subagent '{}' is pinned to model '{}', but it {}: {}. Go to Default model settings and configure this model or update the subagent config.",
            tool_name, model, state, reason
        ),
        None => format!(
            "Subchat model '{}' {}: {}. Go to Default model settings and configure this model or update the subagent config.",
            model, state, reason
        ),
    }
}

pub fn subchat_model_requirement_label(tool_name: Option<&str>, model_type: ChatModelType) -> String {
    let model_label = subchat_model_type_label(model_type);
    match tool_name {
        Some(tool_name) => format!(
            "{} required by subagent '{}' (model_type: {})",
            model_label,
            tool_name,
            subchat_model_type_config_value(model_type)
        ),
        None => model_label.to_string(),
    }
}

pub fn subchat_model_type_config_value(model_type: ChatModelType) -> &'static str {
    match model_type {
        ChatModelType::Light => "light",
        ChatModelType::Default => "default",
        ChatModelType::Thinking => "thinking",
        ChatModelType::Buddy => "buddy",
        ChatModelType::Model2 => "model_2",
        ChatModelType::TaskPlanner => "task_planner",
    }
}

pub fn subchat_model_type_label(model_type: ChatModelType) -> &'static str {
    match model_type {
        ChatModelType::Light => "Light model",
        ChatModelType::Default => "Default model",
        ChatModelType::Thinking => "Thinking model",
        ChatModelType::Buddy => "Buddy model",
        ChatModelType::Model2 => "Chat Model 2",
        ChatModelType::TaskPlanner => "Task Planner model",
    }
}

pub fn llm_endpoint_unusable_reason(endpoint: &str) -> Option<String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Some("an empty LLM endpoint URL".to_string());
    }
    match url::Url::parse(endpoint) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => None,
        Ok(url) => Some(format!(
            "an unsupported LLM endpoint URL scheme '{}': {}",
            url.scheme(),
            endpoint
        )),
        Err(e) => Some(format!("an invalid LLM endpoint URL '{}': {}", endpoint, e)),
    }
}
