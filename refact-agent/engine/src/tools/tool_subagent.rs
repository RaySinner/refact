use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use refact_core::chat_types::PushMode;
use serde_json::{Map, Value, json};
use tokio::sync::{oneshot, Mutex as AMutex};

use refact_agents_core::spawn::{NotifyParent, SpawnGoal, SpawnRequest, SpawnWorktreeMode};
use refact_runtime_api::spawn::spawn_background_agent;
use refact_runtime_api::GlobalContextCtx;
use refact_agents_core::types::{BackgroundAgent, BgAgentKind};
use refact_runtime_api::{AtCommandsContext, MAX_SUBCHAT_DEPTH};
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::types::{GoalBudget, GoalCriterion};
use refact_postprocessing::pp_command_output::OutputFilter;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use crate::tools::tool_background_agents::format_agent_result;
use crate::tools::tools_list::get_available_tools;
use refact_yaml_configs::customization_registry::resolve_subagent_for_model;

pub const SUBAGENT_FORCE_TOOLS: &[&str] = &[
    "tasks_set",
    "progress_report",
    "agents_overview",
    "agent_message",
    "validate_goal",
];

const MODEL_TYPES: &[&str] = &[
    "default",
    "light",
    "thinking",
    "buddy",
    "model_2",
    "task_planner",
];

const SUBAGENT_ABORT_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub struct ToolSubagent {
    pub config_path: String,
}

#[derive(Clone)]
struct SubagentArgs {
    task: String,
    expected_result: String,
    target_files: Vec<String>,
    tools: Option<Vec<String>>,
    model_type: Option<String>,
    model_name: Option<String>,
    goal: Option<SpawnGoal>,
    plan: Option<String>,
    worktree_mode: SpawnWorktreeMode,
    completion_push: PushMode,
    wait: bool,
}

#[async_trait]
impl Tool for ToolSubagent {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "subagent".to_string(),
            display_name: "Subagent".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Spawn a background-only, stateful child trajectory for research or implementation. The child inherits all parent-chat tools by default, can select a configured model or named model, and may receive a goal with a step budget, plan, target files, and an isolated worktree that auto-merges by default. By default a trajectory link is returned immediately and completion is auto-pushed. Set wait=true to return only when this child reaches a terminal state; parallel blocking calls still run concurrently. Use `agents_overview` and `agent_message` to coordinate with peers.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "Focused instructions for the background subagent."},
                    "expected_result": {"type": "string", "description": "Concrete outcome the subagent should deliver."},
                    "target_files": {"type": "array", "items": {"type": "string"}, "description": "Optional expected edit targets. When set, the child may edit only these files."},
                    "tools": {"type": "string", "description": "Optional comma-separated tool names. Omit to inherit all parent-chat tools."},
                    "model_type": {"type": "string", "enum": MODEL_TYPES, "description": "Configured model slot. Ignored when model_name is supplied."},
                    "model_name": {"type": "string", "description": "Concrete chat model id. Takes precedence over model_type."},
                    "goal": {"description": "Optional string or object {content, criteria?, budget?}; use goal.budget.max_turns for a step limit."},
                    "plan": {"type": "string", "description": "Optional installed plan; it is ground truth for the child."},
                    "worktree": {"type": "string", "enum": ["inherit", "isolated"], "default": "inherit", "description": "Use the parent worktree or create an isolated worktree."},
                    "push": PushMode::schema(),
                    "auto_merge": {"type": "boolean", "default": true, "description": "Only for worktree=isolated. Squash-merge completion automatically."},
                    "wait": {"type": "boolean", "default": false, "description": "Wait for this child to reach a terminal state and return its final result. Unrelated messages do not end the wait; user abort does not cancel the child."}
                },
                "required": ["task", "expected_result"]
            }),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let args = parse_subagent_args(args)?;
        let (
            gcx,
            app,
            parent_chat_id,
            parent_root_chat_id,
            parent_subchat_tx,
            subchat_depth,
            parent_task_meta,
            parent_worktree,
            current_model,
        ) = {
            let ccx_lock = ccx.lock().await;
            (
                ccx_lock.app.gcx.clone(),
                ccx_lock.app.clone(),
                ccx_lock.chat_id.clone(),
                ccx_lock.root_chat_id.clone(),
                ccx_lock.subchat_tx.clone(),
                ccx_lock.subchat_depth,
                ccx_lock.task_meta.clone(),
                ccx_lock.execution_scope_worktree(),
                ccx_lock.current_model.clone(),
            )
        };

        if subchat_depth >= MAX_SUBCHAT_DEPTH.saturating_sub(1) {
            return Err(format!(
                "subchat depth limit ({MAX_SUBCHAT_DEPTH}) exceeded"
            ));
        }

        let config = match app.tool.mode_config.get_project_registry().await {
            Some(registry) => resolve_subagent_for_model(&registry, "subagent", None),
            None => None,
        }
        .ok_or_else(|| "subagent config 'subagent' not found".to_string())?;
        let max_steps = config.subchat.max_steps.unwrap_or(60).max(1);
        let available_tools = available_tools(gcx.clone()).await;
        let spawn_tools = match args.tools.as_ref() {
            Some(requested) => Some(normalize_explicit_tools(requested, &available_tools)?),
            None => inherit_parent_tools(&app, &parent_chat_id, &current_model).await,
        };
        let (model, model_type) = resolve_model(
            &app.tool.caps,
            args.model_name.as_deref(),
            args.model_type.as_deref(),
            &current_model,
            config.subchat.model.as_deref(),
            config.subchat.model_type.as_deref(),
        )
        .await?;
        let peer_snapshot = peer_snapshot(&app, &parent_root_chat_id, &args.target_files).await;
        let prompt = build_subagent_prompt(
            &args.task,
            &args.expected_result,
            &args.target_files,
            &peer_snapshot,
            args.goal.is_some(),
        );
        let req = SpawnRequest {
            kind: BgAgentKind::Subagent,
            parent_chat_id: parent_chat_id.clone(),
            parent_root_chat_id: Some(parent_root_chat_id),
            parent_tool_call_id: Some(tool_call_id.clone()),
            config_name: "subagent".to_string(),
            title: short_title("Subagent", &args.task),
            prompt,
            tools: spawn_tools,
            target_files: args.target_files.clone(),
            max_steps,
            model: model.clone(),
            model_type: model_type.clone(),
            goal: args.goal,
            plan: args.plan,
            worktree_mode: args.worktree_mode,
            parent_subchat_tx: Some(parent_subchat_tx),
            parent_worktree,
            parent_task_meta,
            subchat_depth,
            notify_parent: NotifyParent::Auto,
            completion_push: args.completion_push,
        };
        let handle = spawn_background_agent(app.clone(), req).await?;
        if args.wait {
            let interrupt = refact_runtime_api::WaitInterrupt::from_context(&ccx).await;
            let record = wait_for_subagent_terminal(
                &handle.agent_id,
                handle.completion_rx,
                interrupt,
            )
            .await?;
            let content = format_agent_result(&record, false).await?;
            return Ok((
                false,
                vec![tool_message(
                    content,
                    tool_call_id,
                    Map::from_iter([
                        ("background_agent_id".to_string(), json!(record.agent_id)),
                        ("background_agent_kind".to_string(), json!("subagent")),
                        (
                            "background_agent_status".to_string(),
                            json!(record.status.as_str()),
                        ),
                        ("child_chat_id".to_string(), json!(handle.child_chat_id)),
                    ]),
                )],
            ));
        }
        Ok((
            false,
            vec![build_background_start_tool_result(
                &handle,
                &args.task,
                &parent_chat_id,
                &model,
                model_type.as_deref(),
                &peer_snapshot,
                tool_call_id,
            )],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

fn parse_subagent_args(args: &HashMap<String, Value>) -> Result<SubagentArgs, String> {
    let task = parse_required_string(args, "task")?;
    let expected_result = parse_required_string(args, "expected_result")?;
    let target_files = parse_target_files(args)?;
    let tools = parse_optional_csv(args, "tools")?;
    let model_type = parse_optional_string(args, "model_type")?;
    if let Some(model_type) = &model_type {
        if !MODEL_TYPES.contains(&model_type.as_str()) {
            return Err(format!(
                "argument `model_type` must be one of: {}",
                MODEL_TYPES.join(", ")
            ));
        }
    }
    let model_name = parse_optional_string(args, "model_name")?;
    let goal = parse_goal(args)?;
    let plan = parse_optional_string(args, "plan")?;
    let worktree =
        parse_optional_string(args, "worktree")?.unwrap_or_else(|| "inherit".to_string());
    let auto_merge_present =
        args.contains_key("auto_merge") && !args.get("auto_merge").is_some_and(Value::is_null);
    let worktree_mode = match worktree.as_str() {
        "inherit" => {
            if auto_merge_present {
                return Err(
                    "argument `auto_merge` is only valid when `worktree` is `isolated`".to_string(),
                );
            }
            SpawnWorktreeMode::Inherit
        }
        "isolated" => SpawnWorktreeMode::Isolated {
            auto_merge: parse_optional_bool(args, "auto_merge", true)?,
        },
        _ => return Err("argument `worktree` must be `inherit` or `isolated`".to_string()),
    };
    Ok(SubagentArgs {
        task,
        expected_result,
        target_files,
        tools,
        model_type,
        model_name,
        goal,
        plan,
        worktree_mode,
        completion_push: PushMode::from_args(args)?,
        wait: parse_wait(args)?,
    })
}

fn parse_wait(args: &HashMap<String, Value>) -> Result<bool, String> {
    match args.get("wait") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(wait)) => Ok(*wait),
        Some(value) => Err(format!("argument `wait` must be true or false: {value:?}")),
    }
}

async fn wait_for_subagent_abort(interrupt: &refact_runtime_api::WaitInterrupt) {
    loop {
        if interrupt.abort_flag.load(Ordering::SeqCst) {
            return;
        }
        let notified = async {
            if let Some(notify) = &interrupt.notify {
                notify.notified().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(notified);
        if interrupt.abort_flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::select! {
            _ = &mut notified => {},
            _ = tokio::time::sleep(SUBAGENT_ABORT_POLL_INTERVAL) => {},
        }
    }
}

async fn wait_for_subagent_terminal(
    agent_id: &str,
    completion_rx: oneshot::Receiver<BackgroundAgent>,
    interrupt: refact_runtime_api::WaitInterrupt,
) -> Result<BackgroundAgent, String> {
    let record = tokio::select! {
        biased;
        result = completion_rx => {
            result.map_err(|_| "subagent task ended without a result".to_string())?
        }
        _ = wait_for_subagent_abort(&interrupt) => {
            return Err(format!(
                "Subagent wait aborted by user; child agent '{agent_id}' was not cancelled."
            ));
        }
    };
    if record.status.is_terminal() {
        Ok(record)
    } else {
        Err(format!(
            "subagent task returned non-terminal status for '{agent_id}'"
        ))
    }
}

fn parse_required_string(args: &HashMap<String, Value>, name: &str) -> Result<String, String> {
    match args.get(name) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(Value::String(_)) | None => Err(format!("Missing argument `{name}`")),
        Some(value) => Err(format!(
            "argument `{name}` must be a non-empty string: {value:?}"
        )),
    }
}

fn parse_optional_string(
    args: &HashMap<String, Value>,
    name: &str,
) -> Result<Option<String>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.trim().to_string())),
        Some(value) => Err(format!("argument `{name}` must be a string: {value:?}")),
    }
}

fn parse_optional_csv(
    args: &HashMap<String, Value>,
    name: &str,
) -> Result<Option<Vec<String>>, String> {
    let Some(value) = parse_optional_string(args, name)? else {
        return Ok(None);
    };
    let tools = value
        .split(',')
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    Ok((!tools.is_empty()).then_some(tools))
}

fn parse_target_files(args: &HashMap<String, Value>) -> Result<Vec<String>, String> {
    match args.get("target_files") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::String(path) if !path.trim().is_empty() => Ok(path.trim().to_string()),
                Value::String(_) => {
                    Err("argument `target_files` contains an empty string".to_string())
                }
                other => Err(format!(
                    "argument `target_files` contains a non-string value: {other:?}"
                )),
            })
            .collect(),
        Some(value) => Err(format!(
            "argument `target_files` must be an array of strings: {value:?}"
        )),
    }
}

fn parse_optional_bool(
    args: &HashMap<String, Value>,
    name: &str,
    default: bool,
) -> Result<bool, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => refact_tool_api::coerce_bool(value)
            .ok_or_else(|| format!("argument `{name}` must be true or false: {value:?}")),
    }
}

fn parse_goal(args: &HashMap<String, Value>) -> Result<Option<SpawnGoal>, String> {
    let Some(value) = args.get("goal") else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(content) if !content.trim().is_empty() => Ok(Some(SpawnGoal {
            content: content.trim().to_string(),
            criteria: Vec::new(),
            budget: None,
        })),
        Value::String(_) => Err("argument `goal` must not be empty".to_string()),
        Value::Object(object) => {
            let content = object
                .get("content")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|content| !content.is_empty())
                .ok_or_else(|| "argument `goal.content` must be a non-empty string".to_string())?
                .to_string();
            let criteria = match object.get("criteria") {
                None | Some(Value::Null) => Vec::new(),
                Some(value) => serde_json::from_value::<Vec<GoalCriterion>>(value.clone())
                    .map_err(|error| format!("argument `goal.criteria` is malformed: {error}"))?,
            };
            let budget = match object.get("budget") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    serde_json::from_value::<GoalBudget>(value.clone())
                        .map_err(|error| format!("argument `goal.budget` is malformed: {error}"))?,
                ),
            };
            Ok(Some(SpawnGoal {
                content,
                criteria,
                budget,
            }))
        }
        other => Err(format!(
            "argument `goal` must be a string or object: {other:?}"
        )),
    }
}

async fn available_tools(gcx: Arc<crate::global_context::GlobalContext>) -> HashSet<String> {
    let mut names = get_available_tools(gcx)
        .await
        .into_iter()
        .map(|tool| tool.tool_description().name)
        .collect::<HashSet<_>>();
    names.extend(SUBAGENT_FORCE_TOOLS.iter().map(|tool| (*tool).to_string()));
    names
}

async fn inherit_parent_tools(
    app: &crate::app_state::AppState,
    parent_chat_id: &str,
    model: &str,
) -> Option<Vec<String>> {
    let session = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(parent_chat_id)?.clone()
    };
    let parent_mode = session.lock().await.thread.mode.clone();
    let mut tools = app
        .tool
        .tool_registry
        .get_tools_for_mode(&parent_mode, Some(model))
        .await
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    for tool in SUBAGENT_FORCE_TOOLS {
        if !tools.iter().any(|candidate| candidate == tool) {
            tools.push((*tool).to_string());
        }
    }
    Some(tools)
}

fn canonical_tool_name(tool: &str) -> String {
    let normalized = tool.trim().to_ascii_lowercase();
    let normalized =
        if normalized.starts_with(crate::llm::adapters::claude_code_compat::MCP_TOOL_PREFIX) {
            crate::llm::adapters::claude_code_compat::cc_resolve_tool_name(&normalized)
        } else {
            normalized
        };
    if normalized == "grep" {
        return "search_pattern".to_string();
    }
    crate::llm::adapters::claude_code_compat::CC_TOOL_RENAMES
        .iter()
        .find_map(|(original, renamed)| (*renamed == normalized).then(|| (*original).to_string()))
        .unwrap_or(normalized)
}

fn normalize_explicit_tools(
    requested: &[String],
    available: &HashSet<String>,
) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for tool in requested {
        let canonical = canonical_tool_name(tool);
        if !available.contains(&canonical) {
            let mut names = available.iter().cloned().collect::<Vec<_>>();
            names.sort();
            return Err(format!(
                "Unknown tool `{}`. Available tools: {}",
                tool.trim(),
                names.join(", ")
            ));
        }
        if seen.insert(canonical.clone()) {
            result.push(canonical);
        }
    }
    for tool in SUBAGENT_FORCE_TOOLS {
        if seen.insert((*tool).to_string()) {
            result.push((*tool).to_string());
        }
    }
    Ok(result)
}

async fn resolve_model(
    caps: &Arc<dyn refact_runtime_api::CapsProvider>,
    model_name: Option<&str>,
    model_type: Option<&str>,
    parent_model: &str,
    config_model_name: Option<&str>,
    config_model_type: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let caps = caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|error| format!("failed to load caps: {error:?}"))?;

    let effective_model_name = model_name.or(config_model_name);
    let effective_model_type = model_type.or(config_model_type);

    let model_id = if let Some(model_name) = effective_model_name {
        model_name.to_string()
    } else if let Some(model_type) = effective_model_type {
        let slot = match model_type {
            "default" => &caps.defaults.chat_default_model,
            "light" => &caps.defaults.chat_light_model,
            "thinking" => &caps.defaults.chat_thinking_model,
            "buddy" => &caps.defaults.chat_buddy_model,
            "model_2" => &caps.defaults.chat_model_2,
            "task_planner" => &caps.defaults.task_planner_agent_model,
            _ => {
                return Err(format!(
                    "invalid model_type `{model_type}`. Expected: default, light, thinking, buddy, model_2, task_planner"
                ))
            }
        };
        if slot.trim().is_empty() {
            return Err(format!(
                "model_type `{model_type}` is not configured. Configured model slots: {}",
                configured_model_slots(&caps)
            ));
        }
        slot.clone()
    } else {
        parent_model.to_string()
    };
    let selected_type = effective_model_name
        .is_none()
        .then(|| effective_model_type.map(str::to_string))
        .flatten();
    let model = refact_caps_core::model_resolve::resolve_chat_model(caps, &model_id)
        .map_err(|error| format!("model `{model_id}` is not available: {error}"))?;
    Ok((model.base.id.clone(), selected_type))
}

fn configured_model_slots(caps: &crate::caps::CodeAssistantCaps) -> String {
    [
        ("default", &caps.defaults.chat_default_model),
        ("light", &caps.defaults.chat_light_model),
        ("thinking", &caps.defaults.chat_thinking_model),
        ("buddy", &caps.defaults.chat_buddy_model),
        ("model_2", &caps.defaults.chat_model_2),
        ("task_planner", &caps.defaults.task_planner_agent_model),
    ]
    .into_iter()
    .filter(|(_, model)| !model.trim().is_empty())
    .map(|(name, model)| format!("{name}={model}"))
    .collect::<Vec<_>>()
    .join(", ")
}

async fn peer_snapshot(
    app: &crate::app_state::AppState,
    root_chat_id: &str,
    target_files: &[String],
) -> String {
    let requested = target_files
        .iter()
        .map(|path| refact_agents_core::registry::normalize_path_for_overlap(path))
        .collect::<HashSet<_>>();
    let peers = app
        .agents
        .list_all()
        .await
        .into_iter()
        .filter(|record| {
            !record.status.is_terminal()
                && (record.parent_root_chat_id.as_deref() == Some(root_chat_id)
                    || record.parent_chat_id == root_chat_id)
        })
        .collect::<Vec<_>>();
    if peers.is_empty() {
        return "(no active peers)".to_string();
    }
    let mut lines = Vec::new();
    for peer in peers {
        lines.push(format!(
            "- {} — status: {}; target_files: {}; current_tool: {}",
            peer.title,
            peer.status.as_str(),
            files_label(&peer.target_files),
            peer.current_tool.unwrap_or_else(|| "-".to_string()),
        ));
        let overlaps = peer
            .target_files
            .iter()
            .filter(|path| {
                requested.contains(&refact_agents_core::registry::normalize_path_for_overlap(path))
            })
            .collect::<Vec<_>>();
        if !overlaps.is_empty() {
            lines.push(format!(
                "  ⚠ Overlap warning: {} targets {}",
                peer.title,
                overlaps
                    .into_iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    lines.join("\n")
}

fn build_subagent_prompt(
    task: &str,
    expected_result: &str,
    target_files: &[String],
    peers: &str,
    goal_installed: bool,
) -> String {
    let mut prompt = format!("# Your Task\n{task}\n\n# Expected Result\n{expected_result}\n\n");
    if !target_files.is_empty() {
        prompt.push_str("# Target Files\nONLY edit files in this list:\n");
        for path in target_files {
            prompt.push_str(&format!("- {path}\n"));
        }
        prompt.push('\n');
    }
    prompt.push_str(&format!("# Active peers\n{peers}\n\n"));
    prompt.push_str("# Constraints\n- Work independently in this background trajectory.\n- You MAY run tests, compilation, lint, or other verification when your tools allow it.\n- Publish progress with `tasks_set` and `progress_report`.\n- Sibling digest notices arrive in your context automatically; coordinate with `agents_overview` and `agent_message`.\n");
    if goal_installed {
        prompt.push_str("- An installed goal is ground truth. You MUST call `validate_goal` before finishing and report any unmet criteria.\n");
    }
    prompt.push_str("- End with: `Status: DONE | DONE_WITH_CONCERNS | NEEDS_CONTEXT | BLOCKED`, then Findings, Changes, Evidence, Concerns, and Next action.\n");
    prompt
}

fn build_background_start_tool_result(
    handle: &refact_agents_core::spawn::SpawnHandle,
    task: &str,
    parent_chat_id: &str,
    model: &str,
    model_type: Option<&str>,
    peers: &str,
    tool_call_id: &str,
) -> ContextEnum {
    let mut lines = vec![
        format!(
            "✓ Started background subagent: {}",
            truncate_chars(task, 60)
        ),
        format!("- agent_id: {}", handle.agent_id),
        "- status: running".to_string(),
        format!("- child_chat_id: {}", handle.child_chat_id),
        format!("- model: {model}"),
    ];
    if let Some(model_type) = model_type {
        lines.push(format!("- model_type: {model_type}"));
    }
    if let Some(branch) = &handle.worktree_branch {
        lines.push(format!("- worktree_branch: {branch}"));
        lines.push(format!(
            "- auto_merge: {}",
            handle.auto_merge.unwrap_or(false)
        ));
    }
    lines.extend([
        String::new(),
        format!(
            "Open the child trajectory: [view](refact://chat/{})",
            handle.child_chat_id
        ),
        String::new(),
        "The completion will be pushed back into this chat automatically.".to_string(),
    ]);
    if peers.contains("⚠ Overlap warning") {
        lines.extend([
            String::new(),
            "Peer overlap warnings:".to_string(),
            peers.to_string(),
        ]);
    }
    tool_message(
        lines.join("\n"),
        tool_call_id,
        Map::from_iter([
            ("background_agent_id".to_string(), json!(handle.agent_id)),
            ("background_agent_kind".to_string(), json!("subagent")),
            ("background_agent_status".to_string(), json!("running")),
            ("child_chat_id".to_string(), json!(handle.child_chat_id)),
            ("model".to_string(), json!(model)),
            ("model_type".to_string(), json!(model_type)),
            ("worktree_branch".to_string(), json!(handle.worktree_branch)),
            ("auto_merge".to_string(), json!(handle.auto_merge)),
            (
                "background_agent_parent_chat_id".to_string(),
                json!(parent_chat_id),
            ),
        ]),
    )
}

fn tool_message(content: String, tool_call_id: &str, extra: Map<String, Value>) -> ContextEnum {
    ContextEnum::ChatMessage(ChatMessage {
        role: "tool".to_string(),
        content: ChatContent::SimpleText(content),
        tool_call_id: tool_call_id.to_string(),
        preserve: Some(true),
        extra,
        output_filter: Some(OutputFilter::no_limits()),
        ..Default::default()
    })
}

fn files_label(files: &[String]) -> String {
    if files.is_empty() {
        "-".to_string()
    } else {
        files.join(", ")
    }
}

fn short_title(prefix: &str, task: &str) -> String {
    let truncated = truncate_chars(task, 50);
    if task.chars().count() > 50 {
        format!("{prefix}: {truncated}…")
    } else {
        format!("{prefix}: {truncated}")
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.trim().chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use crate::app_state::AppState;
    use crate::caps::{BaseModelRecord, ChatModelRecord, CodeAssistantCaps};
    use refact_agents_core::types::{AgentCompletion, BgAgentStatus, CreateAgentRequest};
    use refact_chat_api::internal_roles::{event, EventSubkind};
    use refact_chat_api::{ChatSession, PendingDelivery, SubchatResult, ToolsPolicy};
    use serial_test::serial;
    use tokio::sync::{watch, Barrier};

    fn args() -> HashMap<String, Value> {
        HashMap::from([
            ("task".to_string(), json!("Implement frog support")),
            ("expected_result".to_string(), json!("Frogs are supported")),
        ])
    }

    fn waiting_args(task: &str) -> HashMap<String, Value> {
        let mut args = args();
        args.insert("task".to_string(), json!(task));
        args.insert("wait".to_string(), json!(true));
        args
    }

    fn subchat_result(mut messages: Vec<ChatMessage>, summary: &str) -> SubchatResult {
        messages.push(ChatMessage::new(
            "assistant".to_string(),
            summary.to_string(),
        ));
        SubchatResult {
            messages,
            metering: Map::new(),
            chat_id: Some("ignored".to_string()),
            aborted: false,
        }
    }

    async fn install_test_caps(gcx: Arc<crate::global_context::GlobalContext>) {
        let mut caps = CodeAssistantCaps::default();
        for slot in [
            "default",
            "light",
            "thinking",
            "buddy",
            "model_2",
            "task_planner",
        ] {
            let model_id = format!("test/{slot}");
            caps.chat_models.insert(
                model_id.clone(),
                Arc::new(ChatModelRecord {
                    base: BaseModelRecord {
                        id: model_id.clone(),
                        name: model_id.clone(),
                        n_ctx: 200_000,
                        endpoint: "https://example.com/v1/chat/completions".to_string(),
                        ..Default::default()
                    },
                    supports_tools: true,
                    supports_agent: true,
                    max_output_tokens: Some(16_000),
                    ..Default::default()
                }),
            );
        }
        caps.defaults.chat_default_model = "test/default".to_string();
        caps.defaults.chat_light_model = "test/light".to_string();
        caps.defaults.chat_thinking_model = "test/thinking".to_string();
        caps.defaults.chat_buddy_model = "test/buddy".to_string();
        caps.defaults.chat_model_2 = "test/model_2".to_string();
        caps.defaults.task_planner_agent_model = "test/task_planner".to_string();
        let mut caps_state = gcx.caps_state.write().await;
        caps_state.caps = Some(Arc::new(caps));
        caps_state.last_attempted_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
    }

    async fn test_context(parent_chat_id: &str) -> Arc<AMutex<AtCommandsContext>> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        install_test_caps(gcx.clone()).await;
        let app = refact_runtime_api::app_state_from_gcx(gcx).await;
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app,
                4096,
                20,
                false,
                vec![],
                parent_chat_id.to_string(),
                Some("root-chat".to_string()),
                "test/default".to_string(),
                None,
                None,
            )
            .await,
        ))
    }

    fn message(contexts: Vec<ContextEnum>) -> ChatMessage {
        match contexts.into_iter().next().expect("tool message") {
            ContextEnum::ChatMessage(message) => message,
            _ => panic!("expected chat message"),
        }
    }

    async fn release_agents(
        release: &watch::Sender<HashSet<String>>,
        agent_ids: HashSet<String>,
    ) {
        release.send(agent_ids).unwrap();
        tokio::task::yield_now().await;
    }

    async fn wait_for_runner_start(started: &Barrier) {
        tokio::time::timeout(Duration::from_secs(30), started.wait())
            .await
            .expect("runners did not start");
    }

    #[test]
    fn schema_is_background_only() {
        let schema = ToolSubagent {
            config_path: String::new(),
        }
        .tool_description()
        .input_schema;
        let properties = schema["properties"].as_object().unwrap();
        for absent in ["max_steps", "notify_parent"] {
            assert!(
                !properties.contains_key(absent),
                "{absent} must not be exposed"
            );
        }
        for present in [
            "task",
            "expected_result",
            "target_files",
            "tools",
            "model_type",
            "model_name",
            "goal",
            "plan",
            "worktree",
            "auto_merge",
            "push",
            "wait",
        ] {
            assert!(
                properties.contains_key(present),
                "{present} must be exposed"
            );
        }
    }

    #[test]
    fn wait_is_strictly_boolean_and_defaults_to_false() {
        assert!(!parse_subagent_args(&args()).unwrap().wait);
        let mut values = waiting_args("blocking");
        assert!(parse_subagent_args(&values).unwrap().wait);
        for invalid in [json!("true"), json!(1), json!({})] {
            values.insert("wait".to_string(), invalid);
            assert!(parse_subagent_args(&values).is_err());
        }
    }

    #[test]
    fn completion_push_defaults_to_append_and_validates_modes() {
        assert_eq!(
            parse_subagent_args(&args()).unwrap().completion_push,
            PushMode::Append
        );
        let mut values = args();
        values.insert("push".to_string(), json!("preempt"));
        assert_eq!(
            parse_subagent_args(&values).unwrap().completion_push,
            PushMode::Preempt
        );
        values.insert("push".to_string(), json!({}));
        assert!(parse_subagent_args(&values).is_err());
    }

    #[test]
    fn explicit_tools_accept_editing_aliases_and_force_tools() {
        let available = HashSet::from([
            "apply_patch".to_string(),
            "search_pattern".to_string(),
            "tasks_set".to_string(),
            "validate_goal".to_string(),
        ]);
        let tools = normalize_explicit_tools(
            &["t_apply".to_string(), "regex_search".to_string()],
            &available,
        )
        .unwrap();
        assert_eq!(&tools[..2], ["apply_patch", "search_pattern"]);
        for required in SUBAGENT_FORCE_TOOLS {
            assert!(tools.contains(&required.to_string()));
        }
    }

    #[tokio::test]
    async fn force_tools_are_registered() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let registered = get_available_tools(gcx)
            .await
            .into_iter()
            .map(|tool| tool.tool_description().name)
            .collect::<HashSet<_>>();

        for tool in SUBAGENT_FORCE_TOOLS {
            assert!(registered.contains(*tool), "{tool} must be registered");
        }
    }

    #[test]
    fn unknown_tool_lists_registry_tools() {
        let error =
            normalize_explicit_tools(&["wat".to_string()], &HashSet::from(["cat".to_string()]))
                .unwrap_err();
        assert!(error.contains("Unknown tool `wat`"));
        assert!(error.contains("cat"));
    }

    #[test]
    fn goal_string_and_object_parse() {
        let mut string = args();
        string.insert("goal".to_string(), json!("Ship frogs"));
        let parsed = parse_subagent_args(&string).unwrap();
        assert_eq!(parsed.goal.unwrap().content, "Ship frogs");

        let mut object = args();
        object.insert(
            "goal".to_string(),
            json!({
                "content": "Ship frogs",
                "criteria": [{"id": "C1", "text": "tests", "verify_hint": "cargo test"}],
                "budget": {"max_turns": 3, "max_tokens": 200}
            }),
        );
        let parsed = parse_subagent_args(&object).unwrap();
        let goal = parsed.goal.unwrap();
        assert_eq!(goal.criteria[0].id, "C1");
        assert_eq!(goal.budget.unwrap().max_turns, Some(3));
    }

    #[test]
    fn worktree_auto_merge_validation() {
        let parsed = parse_subagent_args(&args()).unwrap();
        assert!(matches!(parsed.worktree_mode, SpawnWorktreeMode::Inherit));
        let mut isolated = args();
        isolated.insert("worktree".to_string(), json!("isolated"));
        assert!(matches!(
            parse_subagent_args(&isolated).unwrap().worktree_mode,
            SpawnWorktreeMode::Isolated { auto_merge: true }
        ));
        let mut inherited = args();
        inherited.insert("auto_merge".to_string(), json!(true));
        assert!(matches!(
            parse_subagent_args(&inherited),
            Err(error) if error.contains("only valid")
        ));
    }

    #[tokio::test]
    async fn all_model_type_slots_resolve_and_model_name_wins() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        install_test_caps(gcx.clone()).await;
        for model_type in MODEL_TYPES {
            let (model, selected) = resolve_model(
                &gcx.chat_ctx(gcx.clone()).caps,
                None,
                Some(model_type),
                "parent",
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(model, format!("test/{model_type}"));
            assert_eq!(selected.as_deref(), Some(*model_type));
        }
        let (model, selected) = resolve_model(
            &gcx.chat_ctx(gcx.clone()).caps,
            Some("test/light"),
            Some("thinking"),
            "parent",
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(model, "test/light");
        assert_eq!(selected, None);
    }

    #[tokio::test]
    async fn empty_model_slot_is_a_hard_error() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        install_test_caps(gcx.clone()).await;
        let mut caps = (*gcx.caps_state.read().await.caps.as_ref().unwrap())
            .as_ref()
            .clone();
        caps.defaults.chat_buddy_model.clear();
        gcx.caps_state.write().await.caps = Some(Arc::new(caps));
        let error = resolve_model(
            &gcx.chat_ctx(gcx.clone()).caps,
            None,
            Some("buddy"),
            "parent",
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("not configured"));
        assert!(error.contains("Configured model slots"));
    }

    #[serial(test_runner)]
    #[tokio::test]
    async fn default_spawn_returns_start_result_before_child_terminal() {
        let ccx = test_context("default-async-parent").await;
        let app = ccx.lock().await.app.clone();
        let (release_tx, release_rx) = watch::channel(false);
        let _runner = refact_runtime_api::spawn::install_test_runner(Arc::new(
            move |_gcx, messages, _config| {
                let mut release_rx = release_rx.clone();
                Box::pin(async move {
                    while !*release_rx.borrow() {
                        release_rx.changed().await.unwrap();
                    }
                    Ok(subchat_result(messages, "eventual done"))
                })
            },
        ));
        let (_, contexts) = ToolSubagent {
            config_path: String::new(),
        }
        .tool_execute(ccx, &"default-call".to_string(), &args())
        .await
        .unwrap();
        let message = message(contexts);
        let text = message.content.content_text_only();
        let agent_id = message.extra["background_agent_id"].as_str().unwrap();
        assert!(text.contains("✓ Started background subagent"));
        assert!(text.contains("- status: running"));
        assert!(!text.contains("# Agent Result"));
        assert_eq!(message.extra["background_agent_status"], "running");

        release_tx.send(true).unwrap();
        let record = app
            .agents
            .wait("default-async-parent", agent_id, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(record.status, BgAgentStatus::Completed);
    }

    #[serial(test_runner)]
    #[tokio::test]
    async fn blocking_spawn_returns_terminal_agent_result() {
        let ccx = test_context("blocking-parent").await;
        let _runner = refact_runtime_api::spawn::install_test_runner(Arc::new(
            |_gcx, messages, _config| {
                Box::pin(async move { Ok(subchat_result(messages, "terminal frog report")) })
            },
        ));
        let (_, contexts) = ToolSubagent {
            config_path: String::new(),
        }
        .tool_execute(
            ccx,
            &"blocking-call".to_string(),
            &waiting_args("blocking task"),
        )
        .await
        .unwrap();
        let message = message(contexts);
        let text = message.content.content_text_only();
        assert!(text.contains("# Agent Result"));
        assert!(text.contains("- Status: ✅ completed"));
        assert!(text.contains("terminal frog report"));
        assert!(!text.contains("✓ Started background subagent"));
        assert_eq!(message.extra["background_agent_status"], "completed");
        assert!(text.contains(message.extra["background_agent_id"].as_str().unwrap()));
    }

    #[serial(test_runner)]
    #[tokio::test]
    async fn blocking_spawn_ignores_unrelated_and_goal_deliveries() {
        let ccx = test_context("wake-parent").await;
        let app = ccx.lock().await.app.clone();
        let session = Arc::new(AMutex::new(ChatSession::new("wake-parent".to_string())));
        app.chat
            .sessions
            .write()
            .await
            .insert("wake-parent".to_string(), session.clone());
        {
            let mut session_lock = session.lock().await;
            session_lock.turn_depth = 1;
            session_lock.draft_message = Some(ChatMessage::new(
                "assistant".to_string(),
                "waiting".to_string(),
            ));
            session_lock.begin_interruptible_wait();
        }
        let started = Arc::new(Barrier::new(2));
        let (release_tx, _) = watch::channel(HashSet::new());
        let runner_started = started.clone();
        let runner_release = release_tx.clone();
        let _runner = refact_runtime_api::spawn::install_test_runner(Arc::new(
            move |_gcx, messages, config| {
                let started = runner_started.clone();
                let agent_id = config.background_agent_id.unwrap();
                let mut release = runner_release.subscribe();
                Box::pin(async move {
                    started.wait().await;
                    while !release.borrow().contains(&agent_id) {
                        release.changed().await.unwrap();
                    }
                    Ok(subchat_result(messages, "finished after wakes"))
                })
            },
        ));
        let run = tokio::spawn({
            let ccx = ccx.clone();
            async move {
                ToolSubagent {
                    config_path: String::new(),
                }
                .tool_execute(
                    ccx,
                    &"wake-call".to_string(),
                    &waiting_args("ignore wakes"),
                )
                .await
            }
        });
        wait_for_runner_start(&started).await;
        let agent_id = app.agents.list_all().await[0].agent_id.clone();
        for delivery in [
            PendingDelivery::with_id(
                "message-wake",
                vec![ChatMessage::new("user".to_string(), "sibling update".to_string())],
                PushMode::Append,
                "test.sibling",
                true,
            ),
            PendingDelivery::with_id(
                "goal-wake",
                vec![event(
                    EventSubkind::GoalPursuit,
                    "chat.goal_monitor",
                    json!({"kind": "nudge"}),
                    "continue the goal",
                )],
                PushMode::Append,
                "chat.goal_monitor",
                true,
            ),
        ] {
            session.lock().await.enqueue_delivery(delivery).unwrap();
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            assert!(!run.is_finished());
        }
        assert!(session.lock().await.wait_interrupt_flag.load(Ordering::SeqCst));

        release_agents(&release_tx, HashSet::from([agent_id.clone()])).await;
        let (_, contexts) = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("blocking spawn did not finish")
            .unwrap()
            .unwrap();
        let text = message(contexts).content.content_text_only();
        assert!(text.contains(&agent_id));
        assert!(text.contains("finished after wakes"));
        assert!(session.lock().await.wait_interrupt_flag.load(Ordering::SeqCst));
    }

    #[serial(test_runner)]
    #[tokio::test]
    async fn arbitrary_blocking_spawns_wait_for_every_terminal_result() {
        for fanout in [1, 2, 6, 12] {
            assert_blocking_fanout(fanout).await;
        }
    }

    async fn assert_blocking_fanout(fanout: usize) {
        let parent_chat_id = format!("fanout-parent-{fanout}");
        let ccx = test_context(&parent_chat_id).await;
        let app = ccx.lock().await.app.clone();
        let started = Arc::new(Barrier::new(fanout + 1));
        let runner_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let completed_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stages = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stopwatch = Instant::now();
        let (release_tx, _) = watch::channel(HashSet::new());
        let runner_started = started.clone();
        let runner_calls_observer = runner_calls.clone();
        let runner_release = release_tx.clone();
        let runner_stages = stages.clone();
        let runner_stopwatch = stopwatch;
        let _runner = refact_runtime_api::spawn::install_test_runner_with_stages(
            Arc::new(move |_gcx, messages, config| {
                let started = runner_started.clone();
                let runner_calls = runner_calls_observer.clone();
                let call_id = config.parent_tool_call_id.clone().unwrap_or_default();
                let agent_id = config.background_agent_id.unwrap();
                let mut release = runner_release.subscribe();
                let stages = runner_stages.clone();
                Box::pin(async move {
                    runner_calls.fetch_add(1, Ordering::SeqCst);
                    stages.lock().unwrap().push(format!(
                        "{}ms {call_id}:test_runner_entered",
                        runner_stopwatch.elapsed().as_millis()
                    ));
                    started.wait().await;
                    while !release.borrow().contains(&agent_id) {
                        release.changed().await.unwrap();
                    }
                    Ok(subchat_result(messages, "parallel terminal report"))
                })
            }),
            Some(Arc::new({
                let stages = stages.clone();
                move |stage| {
                    stages.lock().unwrap().push(format!(
                        "{}ms {stage}",
                        stopwatch.elapsed().as_millis()
                    ));
                }
            })),
        );
        let exercise = async {
            let calls = (0..fanout)
                .map(|index| {
                    let ccx = ccx.clone();
                    let completed_calls = completed_calls.clone();
                    async move {
                        let call_id = format!("fanout-{fanout}-call-{index}");
                        let args = waiting_args(&format!("parallel task {index}"));
                        let result = ToolSubagent {
                            config_path: String::new(),
                        }
                        .tool_execute(ccx, &call_id, &args)
                        .await;
                        completed_calls.lock().unwrap().push(match &result {
                            Ok(_) => format!("{call_id}: ok"),
                            Err(error) => format!("{call_id}: error: {error}"),
                        });
                        result
                    }
                })
                .collect::<Vec<_>>();
            let run = tokio::spawn(futures::future::join_all(calls));
            started.wait().await;
            refact_runtime_api::spawn::emit_test_spawn_stage(format!(
                "fanout-{fanout}:all_runners_started"
            ));
            assert!(!run.is_finished());
            let agent_ids = app
                .agents
                .list_all()
                .await
                .into_iter()
                .map(|record| record.agent_id)
                .collect::<HashSet<_>>();
            assert_eq!(agent_ids.len(), fanout);
            if fanout > 1 {
                let first = agent_ids.iter().next().unwrap().clone();
                refact_runtime_api::spawn::emit_test_spawn_stage(format!(
                    "fanout-{fanout}:release_first:{first}"
                ));
                release_agents(&release_tx, HashSet::from([first.clone()])).await;
                let first_record = app
                    .agents
                    .wait(&parent_chat_id, &first, Duration::from_secs(60))
                    .await
                    .unwrap();
                assert!(first_record.status.is_terminal());
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                }
                assert!(!run.is_finished());
            }

            refact_runtime_api::spawn::emit_test_spawn_stage(format!(
                "fanout-{fanout}:release_all"
            ));
            release_agents(&release_tx, agent_ids.clone()).await;
            let results = run.await.unwrap();
            assert_eq!(results.len(), fanout);
            let outputs = results
                .into_iter()
                .map(|result| message(result.unwrap().1).content.content_text_only())
                .collect::<Vec<_>>();
            for agent_id in agent_ids {
                assert!(outputs.iter().any(|output| output.contains(&agent_id)));
            }
            for output in outputs {
                assert!(output.contains("- Status: ✅ completed"));
                assert!(output.contains("parallel terminal report"));
            }
        };
        tokio::pin!(exercise);
        if let Err(error) = tokio::time::timeout(Duration::from_secs(30), &mut exercise).await {
            let records = app
                .agents
                .list_all()
                .await
                .into_iter()
                .map(|record| {
                    format!(
                        "{}:{}:{}",
                        record.agent_id,
                        record.status.as_str(),
                        record.error.unwrap_or_else(|| "-".to_string())
                    )
                })
                .collect::<Vec<_>>();
            panic!(
                "arbitrary fan-out watchdog: {error:?}; N={fanout}; runner calls={}/{fanout}; completed tool calls={:?}; records={:?}; stages={:?}",
                runner_calls.load(Ordering::SeqCst),
                completed_calls.lock().unwrap(),
                records,
                stages.lock().unwrap(),
            );
        }
    }

    #[serial(test_runner)]
    #[tokio::test]
    async fn parallel_blocking_failure_is_a_result_not_a_group_error() {
        let ccx = test_context("failure-parent").await;
        let _runner = refact_runtime_api::spawn::install_test_runner(Arc::new(
            |_gcx, messages, config| {
                let failed = config.title.unwrap().contains("fail child");
                Box::pin(async move {
                    if failed {
                        Err("intentional child failure".to_string())
                    } else {
                        Ok(subchat_result(messages, "successful sibling report"))
                    }
                })
            },
        ));
        let calls = ["fail child", "succeed child"].map(|task| {
            let ccx = ccx.clone();
            async move {
                ToolSubagent {
                    config_path: String::new(),
                }
                .tool_execute(ccx, &format!("{task}-call"), &waiting_args(task))
                .await
            }
        });
        let results = tokio::time::timeout(
            Duration::from_secs(2),
            futures::future::join_all(calls),
        )
        .await
        .expect("mixed fan-out did not finish");
        assert_eq!(results.len(), 2);
        let mut results = results.into_iter();
        let failed = message(results.next().unwrap().unwrap().1)
            .content
            .content_text_only();
        let succeeded = message(results.next().unwrap().unwrap().1)
            .content
            .content_text_only();
        assert!(failed.contains("- Status: ❌ failed"));
        assert!(failed.contains("intentional child failure"));
        assert!(succeeded.contains("- Status: ✅ completed"));
        assert!(succeeded.contains("successful sibling report"));
    }

    #[tokio::test]
    async fn blocking_wait_returns_every_already_terminal_status_immediately() {
        let ccx = test_context("terminal-parent").await;
        let app = ccx.lock().await.app.clone();
        for status in [
            BgAgentStatus::Completed,
            BgAgentStatus::Failed,
            BgAgentStatus::Cancelled,
            BgAgentStatus::Interrupted,
        ] {
            let (record, _, _) = app
                .agents
                .create(CreateAgentRequest {
                    parent_chat_id: "terminal-parent".to_string(),
                    parent_root_chat_id: Some("root-chat".to_string()),
                    parent_tool_call_id: None,
                    kind: BgAgentKind::Subagent,
                    config_name: "subagent".to_string(),
                    title: format!("Already {}", status.as_str()),
                    prompt: String::new(),
                    target_files: Vec::new(),
                    model: "test/default".to_string(),
                    model_type: None,
                    goal_summary: None,
                    plan_present: false,
                    worktree_id: None,
                    worktree_branch: None,
                })
                .await
                .unwrap();
            match status {
                BgAgentStatus::Completed => {
                    app.agents
                        .mark_completed(
                            &record.agent_id,
                            AgentCompletion {
                                result_summary: "already done".to_string(),
                                edited_files: Vec::new(),
                                diff_summary: None,
                                conflict_summary: None,
                                child_chat_id: None,
                            },
                        )
                        .await
                        .unwrap();
                }
                BgAgentStatus::Failed => {
                    app.agents
                        .mark_failed(&record.agent_id, "already failed".to_string())
                        .await
                        .unwrap();
                }
                BgAgentStatus::Cancelled => {
                    app.agents
                        .mark_cancelled(&record.agent_id, Some("already cancelled".to_string()))
                        .await
                        .unwrap();
                }
                BgAgentStatus::Interrupted => {
                    app.agents
                        .mark_interrupted(&record.agent_id, "already interrupted".to_string())
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let record = app.agents.get_any(&record.agent_id).await.unwrap();
            let interrupt = refact_runtime_api::WaitInterrupt::from_context(&ccx).await;
            let (completion_tx, completion_rx) = oneshot::channel();
            completion_tx.send(record.clone()).unwrap();
            let waited = tokio::time::timeout(
                Duration::from_secs(1),
                wait_for_subagent_terminal(&record.agent_id, completion_rx, interrupt),
            )
            .await
            .expect("already terminal wait blocked")
            .unwrap();
            assert_eq!(waited.status, status);
        }
    }

    #[serial(test_runner)]
    #[tokio::test]
    async fn blocking_spawn_abort_does_not_cancel_child() {
        let ccx = test_context("abort-parent").await;
        let app = ccx.lock().await.app.clone();
        let session = Arc::new(AMutex::new(ChatSession::new("abort-parent".to_string())));
        app.chat
            .sessions
            .write()
            .await
            .insert("abort-parent".to_string(), session.clone());
        let started = Arc::new(Barrier::new(2));
        let (release_tx, release_rx) = watch::channel(false);
        let runner_started = started.clone();
        let _runner = refact_runtime_api::spawn::install_test_runner(Arc::new(
            move |_gcx, messages, _config| {
                let started = runner_started.clone();
                let mut release_rx = release_rx.clone();
                Box::pin(async move {
                    started.wait().await;
                    while !*release_rx.borrow() {
                        release_rx.changed().await.unwrap();
                    }
                    Ok(subchat_result(messages, "child survived parent abort"))
                })
            },
        ));
        let run = tokio::spawn({
            let ccx = ccx.clone();
            async move {
                ToolSubagent {
                    config_path: String::new(),
                }
                .tool_execute(
                    ccx,
                    &"abort-call".to_string(),
                    &waiting_args("abort blocking wait"),
                )
                .await
            }
        });
        wait_for_runner_start(&started).await;
        let agent_id = app.agents.list_all().await[0].agent_id.clone();
        ccx.lock().await.abort_flag.store(true, Ordering::SeqCst);
        session.lock().await.abort_notify.notify_waiters();
        let error = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("abort did not interrupt blocking wait")
            .unwrap()
            .unwrap_err();
        assert!(error.contains("aborted by user"));
        assert!(error.contains(&agent_id));
        let record = app.agents.get("abort-parent", &agent_id).await.unwrap();
        assert!(!record.status.is_terminal());

        release_tx.send(true).unwrap();
        let record = app
            .agents
            .wait("abort-parent", &agent_id, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(record.status, BgAgentStatus::Completed);
    }

    #[serial(test_runner)]
    #[tokio::test]
    async fn omitted_tools_spawn_with_all_tools_and_peer_prompt() {
        let ccx = test_context("parent").await;
        let app = ccx.lock().await.app.clone();
        let (peer, _, _) = app
            .agents
            .create(crate::agents::types::CreateAgentRequest {
                parent_chat_id: "parent".to_string(),
                parent_root_chat_id: Some("root-chat".to_string()),
                parent_tool_call_id: None,
                kind: BgAgentKind::Subagent,
                config_name: "subagent".to_string(),
                title: "Peer frog task".to_string(),
                prompt: String::new(),
                target_files: vec!["src/frog.rs".to_string()],
                model: "test/default".to_string(),
                model_type: None,
                goal_summary: None,
                plan_present: false,
                worktree_id: None,
                worktree_branch: None,
            })
            .await
            .unwrap();
        app.agents
            .mark_running(&peer.agent_id, "child-peer".to_string())
            .await
            .unwrap();
        let captured = Arc::new(std::sync::Mutex::new(None));
        let captured_runner = captured.clone();
        let _runner = refact_runtime_api::spawn::install_test_runner(Arc::new(
            move |_gcx, mut messages, config| {
                *captured_runner.lock().unwrap() = Some((
                    config.tools,
                    messages
                        .iter()
                        .map(|message| message.content.content_text_only())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ));
                Box::pin(async move {
                    messages.push(ChatMessage::new(
                        "assistant".to_string(),
                        "done".to_string(),
                    ));
                    Ok(SubchatResult {
                        messages,
                        metering: Map::new(),
                        chat_id: Some("ignored".to_string()),
                        aborted: false,
                    })
                })
            },
        ));
        let mut args = args();
        args.insert("target_files".to_string(), json!(["src/frog.rs"]));
        args.insert("push".to_string(), json!("preempt"));
        let mut tool = ToolSubagent {
            config_path: String::new(),
        };
        let (_, contexts) = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap();
        let result = app
            .agents
            .list_all()
            .await
            .into_iter()
            .find(|record| record.title.starts_with("Subagent:"))
            .unwrap();
        app.agents
            .wait("parent", &result.agent_id, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(result.completion_push, PushMode::Preempt);
        let (tools, prompt) = captured.lock().unwrap().clone().unwrap();
        assert!(matches!(tools, ToolsPolicy::All));
        assert!(prompt.contains("# Active peers"));
        assert!(prompt.contains("Peer frog task"));
        assert!(prompt.contains("Overlap warning"));
        let message = message(contexts);
        assert_eq!(message.extra["model"], "test/default");
        assert!(message
            .content
            .content_text_only()
            .contains("refact://chat/subchat-"));
    }

    #[serial(test_runner)]
    #[tokio::test]
    async fn omitted_tools_inherit_parent_mode_tools_and_force_tools() {
        let ccx = test_context("restricted-parent").await;
        let app = ccx.lock().await.app.clone();
        let parent_session = Arc::new(AMutex::new(refact_chat_api::ChatSession::new(
            "restricted-parent".to_string(),
        )));
        parent_session.lock().await.thread.mode = "NO_TOOLS".to_string();
        app.chat
            .sessions
            .write()
            .await
            .insert("restricted-parent".to_string(), parent_session);
        let expected = inherit_parent_tools(&app, "restricted-parent", "test/default")
            .await
            .expect("parent mode tools");
        let captured = Arc::new(std::sync::Mutex::new(None));
        let captured_runner = captured.clone();
        let _runner =
            refact_runtime_api::spawn::install_test_runner(Arc::new(move |_gcx, messages, config| {
                *captured_runner.lock().unwrap() = Some(config.tools);
                Box::pin(async move {
                    Ok(SubchatResult {
                        messages,
                        metering: Map::new(),
                        chat_id: Some("ignored".to_string()),
                        aborted: false,
                    })
                })
            }));
        let mut tool = ToolSubagent {
            config_path: String::new(),
        };
        tool.tool_execute(ccx, &"call".to_string(), &args())
            .await
            .unwrap();
        let record = app
            .agents
            .list_all()
            .await
            .into_iter()
            .find(|record| record.title.starts_with("Subagent:"))
            .expect("spawned agent");
        app.agents
            .wait(
                "restricted-parent",
                &record.agent_id,
                Duration::from_secs(2),
            )
            .await
            .expect("agent completion");

        assert!(matches!(
            captured.lock().unwrap().clone(),
            Some(ToolsPolicy::Only(tools)) if tools == expected
        ));
    }
}
