use refact_runtime_api::scheduler::active_durable_cron_store_for_services;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_scheduler_core::schedule::parse_schedule;
use refact_scheduler_core::{
 push_from_value, push_schema, session_cron_store,
    CronStore, Job, Trigger,
};
use refact_core::chat_types::PushMode;
use crate::tools::tool_cron_create::human_schedule_for_trigger;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub struct ToolCronUpdate {
    pub config_path: String,
    #[cfg(any(test, feature = "test-util"))]
    pub test_session_store: Option<Arc<dyn CronStore>>,
    #[cfg(any(test, feature = "test-util"))]
    pub test_durable_store: Option<Arc<dyn CronStore>>,
    #[cfg(any(test, feature = "test-util"))]
    pub test_now_ms: Option<u64>,
}

impl ToolCronUpdate {
    pub fn new(config_path: String) -> Self {
        Self {
            config_path,
            #[cfg(any(test, feature = "test-util"))]
            test_session_store: None,
            #[cfg(any(test, feature = "test-util"))]
            test_durable_store: None,
            #[cfg(any(test, feature = "test-util"))]
            test_now_ms: None,
        }
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn with_stores(
        config_path: String,
        session_store: Arc<dyn CronStore>,
        durable_store: Option<Arc<dyn CronStore>>,
        now_ms: u64,
    ) -> Self {
        Self {
            config_path,
            test_session_store: Some(session_store),
            test_durable_store: durable_store,
            test_now_ms: Some(now_ms),
        }
    }

    pub fn session_store(&self) -> Arc<dyn CronStore> {
        #[cfg(any(test, feature = "test-util"))]
        if let Some(store) = &self.test_session_store {
            return store.clone();
        }
        session_cron_store()
    }

    pub async fn durable_store(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
    ) -> Result<Option<Arc<dyn CronStore>>, String> {
        #[cfg(any(test, feature = "test-util"))]
        if self.test_session_store.is_some() {
            return Ok(self.test_durable_store.clone());
        }
        let (gcx, paths, workspace) = {
            let ccx_lock = ccx.lock().await;
            (ccx_lock.app.gcx.clone(), ccx_lock.app.paths.clone(), ccx_lock.app.workspace.clone())
        };
        active_durable_cron_store_for_services(&gcx, &paths, &workspace).await
    }

    fn now_ms(&self) -> u64 {
        #[cfg(any(test, feature = "test-util"))]
        if let Some(now_ms) = self.test_now_ms {
            return now_ms;
        }
        unix_now_ms()
    }
}

#[async_trait]
impl Tool for ToolCronUpdate {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "cron_update".to_string(),
            display_name: "Update Scheduled Task".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Update, pause, resume, or run a scheduled task by ID.".to_string(),
            input_schema: json!({
                "type":"object",
                "properties": {
                    "id": {"type":"string"},
                    "cron": {"type":"string"},
                    "every": {"type":"string"},
                    "at": {"type":"string"},
                    "tz": {"type":"string"},
                    "prompt": {"type":"string"},
                    "description": {"type":"string"},
                    "enabled": {"type":"boolean"},
                    "run_now": {"type":"boolean"},
                    "push": push_schema()
                },
                "required": ["id"]
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
        let input = CronUpdateInput::from_args(args)?;
        let session_store = self.session_store();
        let durable_store = self.durable_store(ccx).await?;
        let (mut job, store) = load_job_store(&input.id, session_store, durable_store).await?;
        let now_ms = self.now_ms();

        apply_update(&mut job, input, now_ms)?;
        let human_schedule = human_schedule_for_trigger(&job.trigger);
        if !store.replace(job.clone()).await? {
            return Err(format!("Scheduled task `{}` not found", job.id));
        }
        refact_scheduler_core::runner_change_notify().notify_waiters();

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(
                    json!({
                        "id": job.id,
                        "updated": true,
                        "human_schedule": human_schedule,
                        "push": job.push,
                    })
                    .to_string(),
                ),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                tool_failed: Some(false),
                ..Default::default()
            })],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }

    fn has_config_path(&self) -> Option<String> {
        Some(self.config_path.clone())
    }
}

#[derive(Debug)]
pub struct CronUpdateInput {
    pub id: String,
    pub cron: Option<String>,
    pub every: Option<String>,
    pub at: Option<String>,
    pub tz: Option<String>,
    pub prompt: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub run_now: bool,
    /// `None` keeps the saved policy; omission must never reset it to append.
    pub push: Option<PushMode>,
}

impl CronUpdateInput {
    pub fn from_args(args: &HashMap<String, Value>) -> Result<Self, String> {
        let id = required_string_arg(args, "id")?;
        let description = optional_string_arg(args, "description")?;
        if let Some(description) = &description {
            if description.chars().count() > 80 {
                return Err("description must be at most 80 characters".to_string());
            }
        }
        Ok(Self {
            id,
            cron: optional_string_arg(args, "cron")?,
            every: optional_string_arg(args, "every")?,
            at: optional_string_arg(args, "at")?,
            tz: optional_string_arg(args, "tz")?,
            prompt: optional_string_arg(args, "prompt")?,
            description,
            enabled: optional_bool_arg(args, "enabled")?,
            run_now: optional_bool_arg(args, "run_now")?.unwrap_or(false),
            push: push_from_value(args.get("push"))?,
        })
    }

    fn has_schedule_update(&self) -> bool {
        self.cron.is_some() || self.every.is_some() || self.at.is_some() || self.tz.is_some()
    }
}

async fn load_job_store(
    id: &str,
    session_store: Arc<dyn CronStore>,
    durable_store: Option<Arc<dyn CronStore>>,
) -> Result<(Job, Arc<dyn CronStore>), String> {
    if let Some(job) = session_store.get(id).await {
        return Ok((job, session_store));
    }
    if let Some(store) = durable_store {
        if let Some(job) = store.get(id).await {
            return Ok((job, store));
        }
    }
    Err(format!("Scheduled task `{id}` not found"))
}

pub fn apply_update(job: &mut Job, input: CronUpdateInput, now_ms: u64) -> Result<(), String> {
    if input.has_schedule_update() {
        let trigger = parse_schedule_update(&input, now_ms)?;
        if matches!(trigger, Trigger::Once { .. }) {
            job.recurring = false;
        }
        job.set_trigger(trigger);
    }
    if let Some(prompt) = input.prompt {
        set_prompt(job, prompt);
    }
    if let Some(description) = input.description {
        job.description = description;
    }
    if let Some(enabled) = input.enabled {
        job.enabled = enabled;
        job.paused_at_ms = if enabled { None } else { Some(now_ms) };
    }
    if let Some(push) = input.push {
        job.push = push;
    }
    if input.run_now {
        job.trigger_at_ms = Some(now_ms);
    }
    Ok(())
}

fn parse_schedule_update(input: &CronUpdateInput, now_ms: u64) -> Result<Trigger, String> {
    parse_schedule(
        input.cron.as_deref(),
        input.every.as_deref(),
        input.at.as_deref(),
        input.tz.as_deref(),
        now_ms,
    )
    .map_err(|error| format!("Invalid schedule: {error}"))
}

fn set_prompt(job: &mut Job, prompt: String) {
    if let refact_scheduler_core::Action::AgentTurn { prompt: target, .. } = &mut job.action {
        *target = prompt;
    }
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

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
