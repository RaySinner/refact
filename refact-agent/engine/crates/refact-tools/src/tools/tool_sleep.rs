use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use refact_core::chat_types::{PendingDelivery, PushMode};
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use tokio::time::{sleep_until, Instant as TokioInstant};

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_postprocessing::pp_command_output::OutputFilter;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType, WaitInterrupt};

pub const MIN_DURATION_MS: u64 = 100;
const MAX_DURATION_MS: u64 = 3_600_000;
pub const MIN_TICK_INTERVAL_MS: u64 = 5_000;
pub struct ToolSleep {
    pub config_path: String,
}

#[derive(Clone)]
pub struct SleepRequest {
    pub duration_ms: u64,
    pub tick_interval_ms: Option<u64>,
    pub description: String,
    pub push: PushMode,
}

pub struct SleepOutcome {
    pub slept_ms: u64,
    pub interrupted: bool,
    pub ticks: Vec<ChatMessage>,
}

#[async_trait]
impl Tool for ToolSleep {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "sleep".to_string(),
            display_name: "Sleep".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Wait for the specified duration. Interruptible by user or incoming messages at any time. Use when you have nothing to do, when waiting for something, or when the user asks you to pause. Prefer this over Bash(sleep ...) — it doesn't hold a shell process. You can call this concurrently with other tools.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "push": PushMode::schema(),
                    "duration_ms": {
                        "type": "integer",
                        "minimum": 100,
                        "maximum": 3600000,
                        "description": "Sleep duration in ms (max 1h)."
                    },
                    "tick_interval_ms": {
                        "type": "integer",
                        "minimum": 5000,
                        "description": "Optional. If set, tick markers are collected during sleep and enqueued after the sleep tool result to preserve provider tool-result ordering. Sleep remains user-interruptible at any time."
                    },
                    "description": {
                        "type": "string",
                        "description": "Short description (≤80 chars)."
                    }
                },
                "required": ["duration_ms", "description"]
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
        let request = parse_sleep_request(args)?;
        let (app, chat_id) = {
            let ccx = ccx.lock().await;
            (ccx.app.clone(), ccx.chat_id.clone())
        };
        let interrupt = WaitInterrupt::from_context(&ccx).await;
        let outcome = sleep_with_ticks_interruptible(
            request.duration_ms,
            request.tick_interval_ms,
            interrupt.clone(),
        )
        .await;
        let SleepOutcome {
            slept_ms,
            interrupted,
            ticks,
        } = outcome;
        queue_ticks(
            app.clone(),
            chat_id.clone(),
            ticks,
            request.push,
            tool_call_id,
        )
        .await?;

        let mut body = json!({
            "slept_ms": slept_ms,
            "interrupted": interrupted,
        });
        if interrupted {
            body["reason"] = json!(interrupt.reason(slept_ms as f64 / 1000.0));
        }
        let mut extra = serde_json::Map::new();
        extra.insert("sleep".to_string(), body.clone());
        let messages = vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(body.to_string()),
            tool_calls: None,
            tool_call_id: tool_call_id.clone(),
            tool_failed: Some(false),
            output_filter: Some(OutputFilter::no_limits()),
            extra,
            ..Default::default()
        })];
        tracing::info!(
            slept_ms = slept_ms,
            interrupted = interrupted,
            description = %request.description,
            "sleep tool completed"
        );
        Ok((false, messages))
    }
}

pub fn parse_sleep_request(args: &HashMap<String, Value>) -> Result<SleepRequest, String> {
    let duration_ms = required_u64(args, "duration_ms")?;
    if !(MIN_DURATION_MS..=MAX_DURATION_MS).contains(&duration_ms) {
        return Err(format!(
            "duration_ms must be between {MIN_DURATION_MS} and {MAX_DURATION_MS}"
        ));
    }

    let tick_interval_ms = optional_u64(args, "tick_interval_ms")?;
    if let Some(tick_interval_ms) = tick_interval_ms {
        if tick_interval_ms < MIN_TICK_INTERVAL_MS {
            return Err(format!(
                "tick_interval_ms must be at least {MIN_TICK_INTERVAL_MS}"
            ));
        }
    }

    let description = match args.get("description") {
        Some(Value::String(description)) => description.trim().to_string(),
        Some(_) => return Err("description must be a string".to_string()),
        None => return Err("Missing required argument 'description'".to_string()),
    };
    if description.is_empty() {
        return Err("description must be a non-empty string".to_string());
    }
    if description.chars().count() > 80 {
        return Err("description must be at most 80 chars".to_string());
    }

    Ok(SleepRequest {
        duration_ms,
        tick_interval_ms,
        description,
        push: PushMode::from_args(args)?,
    })
}

fn required_u64(args: &HashMap<String, Value>, name: &str) -> Result<u64, String> {
    args.get(name)
        .ok_or_else(|| format!("Missing required argument '{name}'"))
        .and_then(|value| {
            value
                .as_u64()
                .ok_or_else(|| format!("{name} must be an integer"))
        })
}

fn optional_u64(args: &HashMap<String, Value>, name: &str) -> Result<Option<u64>, String> {
    args.get(name)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| format!("{name} must be an integer"))
        })
        .transpose()
}

pub async fn sleep_with_ticks_interruptible(
    duration_ms: u64,
    tick_interval_ms: Option<u64>,
    interrupt: WaitInterrupt,
) -> SleepOutcome {
    let started = TokioInstant::now();
    let end = started + Duration::from_millis(duration_ms);
    let mut next_tick = tick_interval_ms.map(|ms| started + Duration::from_millis(ms));
    let mut ticks = Vec::new();

    loop {
        if interrupt.interrupted() {
            return SleepOutcome {
                slept_ms: elapsed_ms(started),
                interrupted: true,
                ticks,
            };
        }

        let now = TokioInstant::now();
        if now >= end {
            return SleepOutcome {
                slept_ms: elapsed_ms(started),
                interrupted: false,
                ticks,
            };
        }

        let tick_sleep = next_tick.filter(|at| *at < end).map(sleep_until);
        tokio::pin!(tick_sleep);

        tokio::select! {
            _ = sleep_until(end) => {
                return SleepOutcome {
                    slept_ms: elapsed_ms(started),
                    interrupted: false,
                    ticks,
                };
            }
            _ = interrupt.wait() => {
                return SleepOutcome {
                    slept_ms: elapsed_ms(started),
                    interrupted: true,
                    ticks,
                };
            }
            _ = async {
                if let Some(tick_sleep) = tick_sleep.as_mut().as_pin_mut() {
                    tick_sleep.await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                let elapsed_ms = elapsed_ms(started).min(duration_ms);
                let remaining_ms = duration_ms.saturating_sub(elapsed_ms);
                ticks.push(tick_event(elapsed_ms, remaining_ms));
                next_tick = next_tick.zip(tick_interval_ms)
                    .map(|(at, ms)| at + Duration::from_millis(ms));
            }
        }
    }
}

fn elapsed_ms(started: TokioInstant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

pub fn tick_event(elapsed_ms: u64, remaining_ms: u64) -> ChatMessage {
    event(
        EventSubkind::Tick,
        "tool.sleep",
        json!({
            "elapsed_ms": elapsed_ms,
            "remaining_ms": remaining_ms,
        }),
        "tick",
    )
}

pub async fn queue_ticks(
    app: refact_runtime_api::AppState,
    chat_id: String,
    ticks: Vec<ChatMessage>,
    push: PushMode,
    tool_call_id: &str,
) -> Result<(), String> {
    if ticks.is_empty() {
        return Ok(());
    }
    let session = {
        let sessions = app.chat.sessions.read().await;
        sessions.get(&chat_id).cloned()
    };
    if let Some(session) = session {
        let mut session = session.lock().await;
        session.queue_post_tool_delivery(PendingDelivery::with_id(
            format!("sleep-ticks-{tool_call_id}"),
            ticks,
            push,
            "tool.sleep",
            false,
        ))?;
        Ok(())
    } else {
        Err(format!("sleep tick target chat '{chat_id}' is unavailable"))
    }
}

