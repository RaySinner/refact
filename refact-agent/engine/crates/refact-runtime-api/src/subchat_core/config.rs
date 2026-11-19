use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, Mutex as AMutex};
use tracing::warn;

use refact_chat_api::{
    ChatMessage, ExplicitSubchatSpec, SubchatConfig, SubchatResult, TaskMeta, ToolsPolicy,
    TraceParent, WrapUpConfig,
};
use refact_core::worktree_meta::WorktreeMeta;

use crate::at_commands_ctx::MAX_SUBCHAT_DEPTH;
use crate::caps_load_runtime::resolve_chat_model;
use crate::chat_ctx::ChatCtx;
use crate::global_context::{GlobalContextCtx, SharedGlobalContext};
use crate::subchat_core::session::resolve_subchat_worktree;
use crate::subchat_runtime::{
    llm_endpoint_unusable_reason, normalize_subchat_params_for_model,
    resolve_subchat_model_for_tool, resolve_subchat_params,
};

pub async fn resolve_subchat_config(
    gcx: SharedGlobalContext,
    tool_name: &str,
    stateful: bool,
    chat_id: Option<String>,
    title: Option<String>,
    parent_id: Option<String>,
    link_type: Option<String>,
    root_chat_id: Option<String>,
    tools: Option<Vec<String>>,
    max_steps: usize,
    prepend_system_prompt: bool,
    wrap_up: Option<WrapUpConfig>,
    mode: String,
) -> Result<SubchatConfig, String> {
    resolve_subchat_config_with_parent(
        gcx,
        tool_name,
        stateful,
        chat_id,
        title,
        parent_id,
        link_type,
        root_chat_id,
        tools,
        max_steps,
        prepend_system_prompt,
        wrap_up,
        mode,
        None,
        None,
        None,
        None,
        None,
        0,
    )
    .await
}

pub async fn resolve_subchat_config_with_parent(
    gcx: SharedGlobalContext,
    tool_name: &str,
    stateful: bool,
    chat_id: Option<String>,
    title: Option<String>,
    parent_id: Option<String>,
    link_type: Option<String>,
    root_chat_id: Option<String>,
    tools: Option<Vec<String>>,
    max_steps: usize,
    prepend_system_prompt: bool,
    wrap_up: Option<WrapUpConfig>,
    mode: String,
    task_meta: Option<TaskMeta>,
    worktree: Option<WorktreeMeta>,
    parent_tool_call_id: Option<String>,
    parent_subchat_tx: Option<Arc<AMutex<mpsc::UnboundedSender<Value>>>>,
    abort_flag: Option<Arc<AtomicBool>>,
    subchat_depth: usize,
) -> Result<SubchatConfig, String> {
    if max_steps == 0 {
        return Err("max_steps must be > 0".to_string());
    }
    if subchat_depth >= MAX_SUBCHAT_DEPTH {
        return Err(format!(
            "subchat depth limit ({}) exceeded",
            MAX_SUBCHAT_DEPTH
        ));
    }

    let ctx = gcx.chat_ctx(gcx.clone());
    let params = resolve_subchat_params(&ctx, tool_name).await?;
    let model = resolve_subchat_model_for_tool(&ctx, tool_name, &params).await?;
    let cache_control = params.subchat_cache_control;
    let (autonomous_no_confirm, auto_approve_editing_tools, auto_approve_dangerous_commands) =
        resolve_subagent_confirmation_defaults(&ctx, tool_name).await;

    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| format!("failed to load caps: {:?}", e))?;

    let model_rec = resolve_chat_model(caps, &model)?;
    if params.subchat_n_ctx > model_rec.base.n_ctx && model_rec.base.n_ctx > 0 {
        return Err(format!(
            "subchat_n_ctx ({}) exceeds model '{}' n_ctx ({})",
            params.subchat_n_ctx, model, model_rec.base.n_ctx
        ));
    }

    let worktree = resolve_subchat_worktree(&ctx, parent_id.as_deref(), worktree).await;

    if let Some(requested_tools) = tools.as_ref().filter(|list| !list.is_empty()) {
        let known_names: std::collections::HashSet<String> =
            ctx.tool_registry.get_available_tool_names().await;
        let unknown: Vec<&String> = requested_tools
            .iter()
            .filter(|name| !known_names.contains(*name))
            .collect();
        if !unknown.is_empty() {
            warn!(
                "subchat '{}' requested tools not present in the registry (check config tool names): {:?}",
                tool_name, unknown,
            );
        }
    }

    let trace_parent = TraceParent::from_parts(parent_id.as_deref(), root_chat_id.as_deref());

    Ok(SubchatConfig {
        tool_name: tool_name.to_string(),
        stateful,
        autonomous_no_confirm,
        auto_approve_editing_tools,
        auto_approve_dangerous_commands,
        chat_id,
        title,
        parent_id,
        link_type,
        root_chat_id,
        tools: ToolsPolicy::from_option(tools),
        max_steps,
        prepend_system_prompt,
        wrap_up,
        task_meta,
        worktree,
        model,
        mode,
        n_ctx: params.subchat_n_ctx,
        max_new_tokens: params.subchat_max_new_tokens,
        temperature: params.subchat_temperature,
        reasoning_effort: params.subchat_reasoning_effort,
        cache_control,
        parent_tool_call_id,
        parent_subchat_tx,
        abort_flag,
        soft_abort: false,
        activity_stamp: None,
        background_agent_id: None,
        subchat_depth,
        final_step_force_answer: false,
        buddy_meta: None,
        step_progress: None,
        trace_parent,
    })
}

pub async fn resolve_subchat_config_with_explicit_params(
    gcx: SharedGlobalContext,
    attribution_id: &str,
    spec: &ExplicitSubchatSpec,
    stateful: bool,
    chat_id: Option<String>,
    title: Option<String>,
    parent_id: Option<String>,
    link_type: Option<String>,
    root_chat_id: Option<String>,
    tools: Option<Vec<String>>,
    max_steps: usize,
    prepend_system_prompt: bool,
    mode: String,
    task_meta: Option<TaskMeta>,
    worktree: Option<WorktreeMeta>,
    parent_tool_call_id: Option<String>,
    parent_subchat_tx: Option<Arc<AMutex<mpsc::UnboundedSender<Value>>>>,
    abort_flag: Option<Arc<AtomicBool>>,
    subchat_depth: usize,
) -> Result<SubchatConfig, String> {
    if max_steps == 0 {
        return Err("max_steps must be > 0".to_string());
    }
    if subchat_depth >= MAX_SUBCHAT_DEPTH {
        return Err(format!(
            "subchat depth limit ({}) exceeded",
            MAX_SUBCHAT_DEPTH
        ));
    }
    if spec.model.trim().is_empty() {
        return Err(format!(
            "explicit subchat '{}' requires a resolved model id",
            attribution_id
        ));
    }
    if spec.params.subchat_n_ctx == 0 {
        return Err(format!(
            "subchat_n_ctx must be > 0 for '{}'",
            attribution_id
        ));
    }
    if spec.params.subchat_max_new_tokens == 0 {
        return Err(format!(
            "subchat_max_new_tokens must be > 0 for '{}'",
            attribution_id
        ));
    }

    let ctx = gcx.chat_ctx(gcx.clone());
    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| format!("failed to load caps: {:?}", e))?;
    let model_rec = resolve_chat_model(caps, &spec.model)?;
    if let Some(reason) = llm_endpoint_unusable_reason(&model_rec.base.endpoint) {
        return Err(format!(
            "model '{}' for '{}' is misconfigured: {}",
            spec.model, attribution_id, reason
        ));
    }
    let mut params = spec.params.clone();
    normalize_subchat_params_for_model(attribution_id, &mut params, &model_rec);

    let worktree = resolve_subchat_worktree(&ctx, parent_id.as_deref(), worktree).await;

    if let Some(requested_tools) = tools.as_ref().filter(|list| !list.is_empty()) {
        let known_names: std::collections::HashSet<String> =
            ctx.tool_registry.get_available_tool_names().await;
        let unknown: Vec<&String> = requested_tools
            .iter()
            .filter(|name| !known_names.contains(*name))
            .collect();
        if !unknown.is_empty() {
            warn!(
                "subchat '{}' requested tools not present in the registry (check config tool names): {:?}",
                attribution_id, unknown,
            );
        }
    }

    let trace_parent = TraceParent::from_parts(parent_id.as_deref(), root_chat_id.as_deref());

    Ok(SubchatConfig {
        tool_name: attribution_id.to_string(),
        stateful,
        autonomous_no_confirm: spec.autonomous_no_confirm,
        auto_approve_editing_tools: false,
        auto_approve_dangerous_commands: false,
        chat_id,
        title,
        parent_id,
        link_type,
        root_chat_id,
        tools: ToolsPolicy::from_option(tools),
        max_steps,
        prepend_system_prompt,
        wrap_up: None,
        task_meta,
        worktree,
        model: model_rec.base.id.clone(),
        mode,
        n_ctx: params.subchat_n_ctx,
        max_new_tokens: params.subchat_max_new_tokens,
        temperature: params.subchat_temperature,
        reasoning_effort: params.subchat_reasoning_effort,
        cache_control: params.subchat_cache_control,
        parent_tool_call_id,
        parent_subchat_tx,
        abort_flag,
        soft_abort: false,
        activity_stamp: None,
        background_agent_id: None,
        subchat_depth,
        final_step_force_answer: false,
        buddy_meta: None,
        step_progress: None,
        trace_parent,
    })
}

pub async fn resolve_subagent_confirmation_defaults(
    ctx: &ChatCtx,
    tool_name: &str,
) -> (bool, bool, bool) {
    ctx.mode_config
        .get_subagent_config(tool_name, None)
        .await
        .map(|config| {
            (
                config.subchat.autonomous_no_confirm.unwrap_or(false),
                config.subchat.auto_approve_editing_tools.unwrap_or(false),
                config
                    .subchat
                    .auto_approve_dangerous_commands
                    .unwrap_or(false),
            )
        })
        .unwrap_or_default()
}

pub async fn run_subchat_once_with_explicit_params(
    gcx: SharedGlobalContext,
    attribution_id: &str,
    spec: &ExplicitSubchatSpec,
    messages: Vec<ChatMessage>,
    parent_tool_call_id: String,
    parent_subchat_tx: Arc<AMutex<mpsc::UnboundedSender<Value>>>,
    parent_abort_flag: Arc<AtomicBool>,
    parent_depth: usize,
    parent_task_meta: Option<TaskMeta>,
    parent_worktree: Option<WorktreeMeta>,
    trace_parent: TraceParent,
) -> Result<SubchatResult, String> {
    let mut config = resolve_subchat_config_with_explicit_params(
        gcx.clone(),
        attribution_id,
        spec,
        false,
        None,
        None,
        None,
        None,
        None,
        Some(vec![]),
        1,
        false,
        "agent".to_string(),
        parent_task_meta,
        parent_worktree,
        Some(parent_tool_call_id),
        Some(parent_subchat_tx),
        Some(parent_abort_flag),
        parent_depth + 1,
    )
    .await?;
    config.trace_parent = trace_parent;

    let ctx = gcx.chat_ctx(gcx.clone());
    ctx.subchat.run_subchat(messages, config).await
}
