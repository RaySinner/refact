use refact_scheduler_core::schedule::scheduler_timezone;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use tokio::sync::{Mutex as AMutex, Notify};

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_runtime_api::files_correction::get_active_project_path_for_services;
use refact_scheduler_core::schedule::parse_schedule;
use refact_scheduler_core::{
    delivery_kind, human_schedule_for_trigger as scheduler_human_schedule_for_trigger, next_run_ms,
    push_from_value, push_schema, session_cron_store, Action, AgentTarget,
    CronStore, Delivery, Job, JsonFileCronStore, Trigger, delivery_from_value,
};
use refact_core::chat_types::PushMode;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub const MAX_CRON_JOBS: usize = refact_scheduler_core::MAX_CRON_JOBS;
const ONE_YEAR_MS: u64 = 365 * 24 * 60 * 60 * 1000;

pub struct ToolCronCreate {
    pub config_path: String,
}

impl ToolCronCreate {
    pub fn new(config_path: String) -> Self {
        Self { config_path }
    }
}

#[derive(Clone, Debug)]
pub struct CronCreateInput {
    pub cron: Option<String>,
    pub every: Option<String>,
    pub at: Option<String>,
    pub tz: Option<String>,
    pub trigger: Option<Trigger>,
    pub prompt: Option<String>,
    pub command: Option<String>,
    pub command_argv: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub timeout_secs: Option<u64>,
    pub delivery: Delivery,
    pub push: PushMode,
    pub recurring: Option<bool>,
    pub durable: Option<bool>,
    pub isolated: bool,
    pub description: String,
}

#[derive(Clone)]
pub struct CronCreateRuntime {
    pub session_store: Arc<dyn CronStore>,
    pub durable_store: Option<Arc<dyn CronStore>>,
    pub change_notify: Arc<Notify>,
    pub now_ms: u64,
    pub timezone: Tz,
    pub chat_id: Option<String>,
    pub mode: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug)]
pub struct CronCreateOutcome {
    pub task: Job,
    pub human_schedule: String,
    pub summary: String,
}

#[async_trait]
impl Tool for ToolCronCreate {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "cron_create".to_string(),
            display_name: "Create Scheduled Prompt".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Schedule a prompt to be enqueued later. Use a standard 5-field cron expression (`minute hour day-of-month month day-of-week`) evaluated in the local timezone. Set `recurring` to true for repeated prompts or false for a one-shot prompt that is removed after it fires. Omit `durable` to persist in the current project when a project store exists; set it false for a session-only in-memory schedule. Scheduler jitter is applied automatically so jobs may run shortly after the exact cron instant. Recurring jobs auto-expire after 30 days unless canceled earlier.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cron": { "type": "string", "description": "Standard 5-field cron expression in local time. Required unless every or at is set." },
                    "every": { "type": "string", "description": "Interval such as 30m, 2h, or 1d. Mutually exclusive with cron and at." },
                    "at": { "type": "string", "description": "One-shot time as RFC3339 or relative duration such as in 30m. Mutually exclusive with cron and every." },
                    "trigger": {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "enum": ["webhook"] },
                            "hook_id": { "type": "string", "description": "Inbound daemon hook id that fires this job." }
                        },
                        "required": ["kind", "hook_id"],
                        "description": "Webhook trigger. Mutually exclusive with cron, every, and at. Webhook jobs never time-fire."
                    },
                    "hook_id": { "type": "string", "description": "Shortcut for trigger {kind:'webhook', hook_id}." },
                    "tz": { "type": "string", "description": "IANA timezone for cron schedules, such as UTC or Asia/Kolkata." },
                    "prompt": { "type": "string", "description": "Prompt enqueued at each fire time. Mutually exclusive with command and command_argv." },
                    "command": { "type": "string", "description": "Command line to shell-split and run without an agent turn. Mutually exclusive with prompt and command_argv." },
                    "command_argv": { "type": "array", "items": { "type": "string" }, "description": "Command argv to run without an agent turn. Mutually exclusive with prompt and command." },
                    "cwd": { "type": "string", "description": "Optional command working directory, resolved under the active project." },
                    "timeout_secs": { "type": "integer", "description": "Optional command timeout in seconds." },
                    "delivery": {
                        "oneOf": [
                            { "type": "string", "enum": ["chat", "none"] },
                            {
                                "type": "object",
                                "properties": {
                                    "kind": { "type": "string", "enum": ["webhook"] },
                                    "url": { "type": "string" },
                                    "token": { "type": "string" }
                                },
                                "required": ["url"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "kind": { "type": "string", "enum": ["notifier"] },
                                    "integration_id": { "type": "string" },
                                    "target": { "type": "string" }
                                },
                                "required": ["integration_id"]
                            }
                        ],
                        "description": "Delivery target: chat (default), none, webhook {url, token?}, or notifier {integration_id, target?}."
                    },
                    "push": push_schema(),
                    "recurring": { "type": "boolean", "default": true },
                    "durable": { "type": "boolean", "description": "Persist in the current project when true; stay session-only when false. Omitted defaults to durable when a project store exists." },
                    "isolated": { "type": "boolean", "default": false, "description": "Create a fresh isolated chat session for each fire instead of enqueueing into the current chat." },
                    "description": { "type": "string", "description": "Short description (≤80 chars) shown in cron_list UI." }
                },
                "required": ["description"]
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
        let input = input_from_args(args)?;
        let (app, chat_id, project_root, mode, model) = cron_tool_context(ccx).await;
        let runtime = runtime_for_project(
            project_root,
            Some(chat_id.clone()),
            mode,
            model,
            unix_now_ms(),
            scheduler_timezone(),
        )?;
        let outcome = create_cron_job(input, runtime).await?;

        emit_created_notice(app, &chat_id, &outcome.task, &outcome.summary).await;

        let output = json!({
            "id": outcome.task.id,
            "human_schedule": outcome.human_schedule,
            "recurring": outcome.task.recurring,
            "durable": outcome.task.durable,
            "action_kind": outcome.task.action_kind(),
            "delivery_kind": delivery_kind(&outcome.task.delivery),
            "delivery": delivery_output(&outcome.task.delivery),
            "push": outcome.task.push,
            "isolated": outcome.task.is_isolated(),
        });

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(output.to_string()),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

pub fn delivery_output(delivery: &Delivery) -> Value {
    match delivery {
        Delivery::Chat => json!({"kind": "chat"}),
        Delivery::Webhook { url, token } => json!({
            "kind": "webhook",
            "url": url,
            "has_token": token.as_ref().is_some_and(|token| !token.trim().is_empty()),
        }),
        Delivery::Notifier {
            integration_id,
            target,
        } => json!({
            "kind": "notifier",
            "integration_id": integration_id,
            "target": target,
        }),
        Delivery::None => json!({"kind": "none"}),
    }
}

pub fn input_from_args(args: &HashMap<String, Value>) -> Result<CronCreateInput, String> {
    let cron = optional_string_arg(args, "cron")?;
    let every = optional_string_arg(args, "every")?;
    let at = optional_string_arg(args, "at")?;
    let tz = optional_string_arg(args, "tz")?;
    let trigger = trigger_arg(args)?;
    if cron.is_none() && every.is_none() && at.is_none() && trigger.is_none() {
        return Err("argument `cron` is required".to_string());
    }
    let prompt = optional_string_arg(args, "prompt")?;
    let command = optional_string_arg(args, "command")?;
    let command_argv = optional_string_array_arg(args, "command_argv")?;
    validate_action_args(prompt.as_ref(), command.as_ref(), command_argv.as_ref())?;
    let description = required_string_arg(args, "description")?;
    if description.chars().count() > 80 {
        return Err("description must be at most 80 characters".to_string());
    }
    Ok(CronCreateInput {
        cron,
        every,
        at,
        tz,
        trigger,
        prompt,
        command,
        command_argv,
        cwd: optional_string_arg(args, "cwd")?,
        timeout_secs: optional_u64_arg(args, "timeout_secs")?,
        delivery: delivery_arg(args)?,
        push: push_from_value(args.get("push"))?.unwrap_or_default(),
        recurring: optional_bool_arg(args, "recurring")?,
        durable: optional_bool_arg(args, "durable")?,
        isolated: optional_bool_arg(args, "isolated")?.unwrap_or(false),
        description,
    })
}

fn delivery_arg(args: &HashMap<String, Value>) -> Result<Delivery, String> {
    args.get("delivery")
        .map(delivery_from_value)
        .transpose()
        .map(|delivery| delivery.unwrap_or(Delivery::Chat))
}

fn trigger_arg(args: &HashMap<String, Value>) -> Result<Option<Trigger>, String> {
    let shortcut = optional_string_arg(args, "hook_id")?;
    let trigger = match args.get("trigger") {
        Some(Value::Object(map)) => {
            let kind = map.get("kind").and_then(Value::as_str).unwrap_or("webhook");
            if kind != "webhook" {
                return Err(format!("unsupported trigger `{kind}`"));
            }
            let hook_id = map
                .get("hook_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|hook_id| !hook_id.is_empty())
                .ok_or_else(|| "trigger webhook requires `hook_id`".to_string())?;
            Some(Trigger::Webhook {
                hook_id: hook_id.to_string(),
            })
        }
        Some(Value::String(kind)) if kind == "webhook" => {
            let hook_id = shortcut
                .clone()
                .ok_or_else(|| "trigger webhook requires `hook_id`".to_string())?;
            Some(Trigger::Webhook { hook_id })
        }
        Some(Value::Null) | None => shortcut.map(|hook_id| Trigger::Webhook { hook_id }),
        Some(other) => return Err(format!("argument `trigger` is invalid: {other:?}")),
    };
    Ok(trigger)
}

fn required_string_arg(args: &HashMap<String, Value>, name: &str) -> Result<String, String> {
    match args.get(name) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(Value::String(_)) | Some(Value::Null) | None => {
            Err(format!("argument `{name}` is required"))
        }
        Some(value) => Err(format!("argument `{name}` is not a string: {value:?}")),
    }
}

fn optional_string_arg(
    args: &HashMap<String, Value>,
    name: &str,
) -> Result<Option<String>, String> {
    match args.get(name) {
        Some(Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.trim().to_string()))
        }
        Some(Value::String(_)) | Some(Value::Null) | None => Ok(None),
        Some(value) => Err(format!("argument `{name}` is not a string: {value:?}")),
    }
}

fn optional_bool_arg(args: &HashMap<String, Value>, name: &str) -> Result<Option<bool>, String> {
    match args.get(name) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => refact_tool_api::coerce_bool(value)
            .map(Some)
            .ok_or_else(|| format!("argument `{name}` is not a boolean: {value:?}")),
    }
}

fn optional_u64_arg(args: &HashMap<String, Value>, name: &str) -> Result<Option<u64>, String> {
    match args.get(name) {
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("argument `{name}` is not an unsigned integer: {value:?}")),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(format!("argument `{name}` is not an integer: {value:?}")),
    }
}

fn optional_string_array_arg(
    args: &HashMap<String, Value>,
    name: &str,
) -> Result<Option<Vec<String>>, String> {
    match args.get(name) {
        Some(Value::Array(values)) => {
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                match value {
                    Value::String(item) if !item.trim().is_empty() => out.push(item.clone()),
                    Value::String(_) => {
                        return Err(format!("argument `{name}` contains an empty string"));
                    }
                    other => {
                        return Err(format!(
                            "argument `{name}` contains a non-string value: {other:?}"
                        ));
                    }
                }
            }
            if out.is_empty() {
                Err(format!("argument `{name}` must not be empty"))
            } else {
                Ok(Some(out))
            }
        }
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(format!("argument `{name}` is not an array: {value:?}")),
    }
}

fn validate_action_args(
    prompt: Option<&String>,
    command: Option<&String>,
    command_argv: Option<&Vec<String>>,
) -> Result<(), String> {
    let agent_turn = prompt.is_some();
    let command_count = usize::from(command.is_some()) + usize::from(command_argv.is_some());
    match (agent_turn, command_count) {
        (true, 0) | (false, 1) => Ok(()),
        (false, 0) => Err("one of `prompt`, `command`, or `command_argv` is required".to_string()),
        (true, _) => Err("exactly one action is allowed: prompt XOR command".to_string()),
        (false, _) => Err("exactly one of `command` or `command_argv` is allowed".to_string()),
    }
}

fn validate_delivery(input: &CronCreateInput) -> Result<(), String> {
    if input.prompt.is_some() && !matches!(input.delivery, Delivery::Chat) {
        return Err("non-chat delivery is only supported for command jobs".to_string());
    }
    if input.isolated && !matches!(input.delivery, Delivery::Chat) {
        return Err("isolated jobs only support chat delivery".to_string());
    }
    Ok(())
}

fn runtime_for_project(
    project_root: Option<PathBuf>,
    chat_id: Option<String>,
    mode: Option<String>,
    model: Option<String>,
    now_ms: u64,
    timezone: Tz,
) -> Result<CronCreateRuntime, String> {
    let durable_store = project_root
        .map(|project_root| {
            JsonFileCronStore::new(project_root).map(|store| Arc::new(store) as Arc<dyn CronStore>)
        })
        .transpose()?;
    Ok(CronCreateRuntime {
        session_store: session_cron_store(),
        durable_store,
        change_notify: refact_scheduler_core::runner_change_notify(),
        now_ms,
        timezone,
        chat_id,
        mode,
        model,
    })
}

async fn cron_tool_context(
    ccx: Arc<AMutex<AtCommandsContext>>,
) -> (
    refact_runtime_api::AppState,
    String,
    Option<PathBuf>,
    Option<String>,
    Option<String>,
) {
    let (app, chat_id, scoped_root) = {
        let locked = ccx.lock().await;
        (
            locked.app.clone(),
            locked.chat_id.clone(),
            locked
                .execution_scope
                .as_ref()
                .map(|scope| scope.effective_root().to_path_buf()),
        )
    };
    let project_root = match scoped_root {
        Some(root) => Some(root),
        None => get_active_project_path_for_services(&app.paths, &app.workspace).await,
    };
    let session_arc = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(&chat_id).cloned()
    };
    let (mode, model) = if let Some(session_arc) = session_arc {
        let session = session_arc.lock().await;
        let mode = session.thread.mode.clone();
        let mode = if mode.is_empty() { None } else { Some(mode) };
        let model = session.thread.model.clone();
        let model = if model.is_empty() { None } else { Some(model) };
        (mode, model)
    } else {
        (None, None)
    };
    (app, chat_id, project_root, mode, model)
}

pub async fn create_cron_job(
    input: CronCreateInput,
    runtime: CronCreateRuntime,
) -> Result<CronCreateOutcome, String> {
    validate_delivery(&input)?;
    let trigger = parse_create_schedule(&input, runtime.now_ms)?;
    let recurring = if matches!(trigger, Trigger::Once { .. }) {
        false
    } else {
        input.recurring.unwrap_or(true)
    };

    let durable_count = match &runtime.durable_store {
        Some(store) => store.list().await.len(),
        None => 0,
    };
    let total_tasks = runtime.session_store.list().await.len() + durable_count;
    if total_tasks >= MAX_CRON_JOBS {
        return Err(format!(
            "Too many scheduled jobs (max {MAX_CRON_JOBS}). Cancel one first."
        ));
    }
    let durable = input
        .durable
        .unwrap_or_else(|| runtime.durable_store.is_some());
    if durable && runtime.durable_store.is_none() {
        return Err("No project root available for durable scheduled jobs".to_string());
    }

    let mut task = Job::new_cron_agent_chat(
        input.cron.clone().unwrap_or_default(),
        input.prompt.clone().unwrap_or_default(),
        input.description.clone(),
        recurring,
        durable,
        runtime.now_ms,
    );
    task.set_trigger(trigger);
    task.delivery = input.delivery.clone();
    task.push = input.push;
    apply_cron_create_action(&mut task, &input, runtime.chat_id, runtime.model)?;
    task.set_mode(runtime.mode);
    validate_next_run(&task, runtime.now_ms, runtime.timezone)?;
    let human = human_schedule_for_trigger(&task.trigger);
    let store = if task.durable {
        runtime.durable_store.as_ref().unwrap().clone()
    } else {
        runtime.session_store.clone()
    };
    store.add(task.clone()).await?;
    runtime.change_notify.notify_waiters();
    let summary = format!("Scheduled {}: {} ({})", task.id, task.description, human);
    Ok(CronCreateOutcome {
        task,
        human_schedule: human,
        summary,
    })
}

fn apply_cron_create_action(
    task: &mut Job,
    input: &CronCreateInput,
    chat_id: Option<String>,
    model: Option<String>,
) -> Result<(), String> {
    if let Some(prompt) = input.prompt.clone() {
        task.action = Action::AgentTurn {
            prompt,
            target: AgentTarget::ExistingChat {
                chat_id: String::new(),
            },
            mode: None,
            model: input.isolated.then_some(model).flatten(),
            tools: None,
        };
        if input.isolated {
            task.set_isolated();
        } else {
            task.set_existing_chat(chat_id);
        }
        return Ok(());
    }
    task.action = Action::Command {
        argv: command_argv_from_input(input)?,
        target: AgentTarget::ExistingChat {
            chat_id: String::new(),
        },
        cwd: input.cwd.clone(),
        env: None,
        timeout_secs: input.timeout_secs,
    };
    if input.isolated {
        task.set_isolated();
    } else {
        task.set_existing_chat(chat_id);
    }
    Ok(())
}

fn command_argv_from_input(input: &CronCreateInput) -> Result<Vec<String>, String> {
    if let Some(argv) = input.command_argv.clone() {
        return Ok(argv);
    }
    let command = input
        .command
        .as_deref()
        .ok_or_else(|| "command action requires `command` or `command_argv`".to_string())?;
    shell_words::split(command).map_err(|error| format!("failed to parse command: {error}"))
}

#[cfg(any(test, feature = "test-util"))]
pub fn job_model(task: &Job) -> Option<&str> {
    match &task.action {
        Action::AgentTurn { model, .. } => model.as_deref().filter(|model| !model.is_empty()),
        _ => None,
    }
}

pub fn parse_create_schedule(input: &CronCreateInput, now_ms: u64) -> Result<Trigger, String> {
    if let Some(trigger) = input.trigger.clone() {
        if input.cron.is_some() || input.every.is_some() || input.at.is_some() || input.tz.is_some()
        {
            return Err(
                "webhook trigger is mutually exclusive with cron, every, at, and tz".to_string(),
            );
        }
        return Ok(trigger);
    }
    parse_schedule(
        input.cron.as_deref(),
        input.every.as_deref(),
        input.at.as_deref(),
        input.tz.as_deref(),
        now_ms,
    )
    .map_err(|error| {
        if input.every.is_none() && input.at.is_none() {
            format!(
                "Invalid cron expression: {}",
                error.trim_start_matches("Invalid cron expression: ")
            )
        } else {
            format!("Invalid schedule: {error}")
        }
    })
}

fn validate_next_run(task: &Job, now_ms: u64, timezone: Tz) -> Result<(), String> {
    if matches!(
        task.trigger,
        Trigger::Webhook { .. } | Trigger::Manual | Trigger::OnProcessExit { .. }
    ) {
        return Ok(());
    }
    let next = next_run_ms(task, now_ms, timezone).ok_or_else(no_match_in_year_error)?;
    if matches!(task.trigger, Trigger::Cron { .. }) && next.saturating_sub(now_ms) > ONE_YEAR_MS {
        return Err(no_match_in_year_error());
    }
    Ok(())
}

fn no_match_in_year_error() -> String {
    "matches no calendar date in the next year".to_string()
}

pub fn human_schedule_for_trigger(trigger: &Trigger) -> String {
    match trigger {
        Trigger::Cron { expr, .. } => scheduler_human_schedule_for_trigger(&Trigger::Cron {
            expr: expr.clone(),
            tz: None,
        }),
        Trigger::Interval { every_ms } => format!("every {}", tool_human_duration(*every_ms)),
        Trigger::Once { at_ms } => Utc
            .timestamp_millis_opt(*at_ms as i64)
            .single()
            .map(|datetime| format!("once at {}", datetime.to_rfc3339()))
            .unwrap_or_else(|| format!("once at {at_ms}")),
        Trigger::Manual => "manual".to_string(),
        Trigger::Webhook { hook_id } => format!("webhook {hook_id}"),
        Trigger::OnProcessExit { match_kind } => format!("on process exit {match_kind}"),
    }
}

fn tool_human_duration(ms: u64) -> String {
    for (unit_ms, singular, plural) in [
        (24 * 60 * 60 * 1000, "day", "days"),
        (60 * 60 * 1000, "hour", "hours"),
        (60 * 1000, "minute", "minutes"),
        (1000, "second", "seconds"),
    ] {
        if ms >= unit_ms && ms % unit_ms == 0 {
            let amount = ms / unit_ms;
            let unit = if amount == 1 { singular } else { plural };
            return format!("{amount} {unit}");
        }
    }
    format!("{ms}ms")
}

async fn emit_created_notice(
    app: refact_runtime_api::AppState,
    chat_id: &str,
    task: &Job,
    summary: &str,
) {
    let session_arc = refact_runtime_api::session_runtime::get_or_create_session_with_trajectory(
        app.chat_ctx(),
        &app.chat.sessions,
        chat_id,
    )
    .await;
    let mut session = session_arc.lock().await;
    session.add_message(event(
        EventSubkind::SystemNotice,
        "scheduler.cron",
        json!({
            "id": task.id,
            "cron": task.cron_expr().unwrap_or_default(),
            "recurring": task.recurring,
            "durable": task.durable,
            "action_kind": task.action_kind(),
            "isolated": task.is_isolated(),
        }),
        summary.to_string(),
    ));
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
