use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::GlobalContextCtx;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::ChatCtx;
use refact_runtime_api::task_storage as storage;
use crate::tools::planner_delivery::{self, CardGuard, CardTarget, DeliveryReport};
use crate::tools::task_tool_helpers::required_string;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_core::chat_types::PushMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastPriority {
    Info,
    Steer,
    Urgent,
}

impl BroadcastPriority {
    fn parse(value: Option<&Value>) -> Result<Self, String> {
        match value.and_then(|v| v.as_str()).unwrap_or("steer") {
            "info" => Ok(Self::Info),
            "steer" => Ok(Self::Steer),
            "urgent" => Ok(Self::Urgent),
            other => Err(format!(
                "Invalid priority '{}', must be one of: info, steer, urgent",
                other
            )),
        }
    }

    pub fn format_message(self, message: &str) -> String {
        match self {
            Self::Info => format!("[Planner FYI] {}", message),
            Self::Steer => format!(
                "[Planner STEER] {}\n\nPlease adapt based on this and continue.",
                message
            ),
            Self::Urgent => format!(
                "[Planner URGENT] {}\n\nStop current action and address this.",
                message
            ),
        }
    }
}

pub struct BroadcastResult {
    pub card_id: String,
    pub title: String,
    pub status: BroadcastStatus,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BroadcastStatus {
    Notified,
    SkippedExcluded,
    Skipped(String),
    Failed(String),
}

pub struct ToolTaskBroadcast;

impl ToolTaskBroadcast {
    pub fn new() -> Self {
        Self
    }
}

fn parse_exclude_cards(args: &HashMap<String, Value>) -> Result<HashSet<String>, String> {
    let Some(value) = args.get("exclude_cards") else {
        return Ok(HashSet::new());
    };
    let Some(items) = value.as_array() else {
        return Err("exclude_cards must be an array of card IDs".to_string());
    };
    let mut excluded = HashSet::new();
    for item in items {
        let card_id = item
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "exclude_cards must contain only non-empty strings".to_string())?;
        excluded.insert(card_id.to_string());
    }
    Ok(excluded)
}

async fn planner_task_id(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
) -> Result<String, String> {
    let ccx_lock = ccx.lock().await;
    let meta = ccx_lock
        .task_meta
        .as_ref()
        .ok_or_else(|| "task_broadcast can only be called by the task planner.".to_string())?;
    if meta.role != "planner" {
        return Err("task_broadcast can only be called by the task planner.".to_string());
    }
    if let Some(task_id) = args.get("task_id").and_then(|value| value.as_str()) {
        if task_id != meta.task_id {
            return Err(format!(
                "Supplied task_id '{}' does not match bound task_id '{}'",
                task_id, meta.task_id
            ));
        }
    }
    Ok(meta.task_id.clone())
}

fn tool_message(tool_call_id: &str, content: String) -> ContextEnum {
    ContextEnum::ChatMessage(ChatMessage {
        role: "tool".to_string(),
        content: ChatContent::SimpleText(content),
        tool_calls: None,
        tool_call_id: tool_call_id.to_string(),
        ..Default::default()
    })
}

pub fn format_output(results: &[BroadcastResult], notified_count: usize, message: &str) -> String {
    if results.is_empty() {
        return format!(
            "📢 No running agents to broadcast to.\n\nMessage: \"{}\"",
            message
        );
    }

    let agent_word = if notified_count == 1 {
        "agent"
    } else {
        "agents"
    };
    let mut lines = vec![format!(
        "📢 Broadcast sent to {} {}",
        notified_count, agent_word
    )];
    lines.push(String::new());
    for result in results {
        let status = match &result.status {
            BroadcastStatus::Notified => "notified".to_string(),
            BroadcastStatus::SkippedExcluded => "skipped (in exclude_cards)".to_string(),
            BroadcastStatus::Skipped(reason) => format!("skipped ({})", reason),
            BroadcastStatus::Failed(error) => format!("failed ({})", error),
        };
        lines.push(format!(
            "- {} ({}): {}",
            result.card_id, result.title, status
        ));
    }
    lines.push(String::new());
    lines.push(format!("Message: \"{}\"", message));
    lines.join("\n")
}

pub async fn broadcast_to_target(
    ctx: &ChatCtx,
    task_id: &str,
    target: CardTarget,
    message: &str,
    broadcast_message: &str,
    push: PushMode,
    chat_facade: Arc<dyn refact_runtime_api::ChatSessionFacade>,
) -> (BroadcastResult, bool) {
    let result = |status| BroadcastResult {
        card_id: target.card_id.clone(),
        title: target.title.clone(),
        status,
    };

    let pending_status = format!(
        "Broadcast pending: {}",
        planner_delivery::truncate_chars(message, 80)
    );
    match planner_delivery::reserve_card_status(
        ctx,
        task_id,
        &target,
        pending_status,
        true,
        false,
    )
    .await
    {
        Ok(None) => {}
        Ok(Some(reason)) => return (result(BroadcastStatus::Skipped(reason)), false),
        Err(error) => {
            return (
                result(BroadcastStatus::Failed(format!(
                    "status update failed: {}",
                    error
                ))),
                false,
            )
        }
    }

    let report = planner_delivery::deliver_to_card(
        ctx,
        chat_facade,
        task_id,
        &target,
        CardGuard::doing(),
        planner_delivery::single_message_delivery(
            broadcast_message.to_string(),
            push,
            "tools.task_broadcast",
            true,
        ),
    )
    .await;

    let reached = report.reached_agent();
    let follow_up = if reached {
        format!(
            "Broadcast delivered: {}",
            planner_delivery::truncate_chars(message, 80)
        )
    } else {
        format!("Broadcast {}", report.short_status())
    };
    let record_error = planner_delivery::record_card_status(ctx, task_id, &target, follow_up)
        .await
        .err();

    let status = match (&report, record_error) {
        (DeliveryReport::Delivered { .. }, None) if reached => BroadcastStatus::Notified,
        (DeliveryReport::Delivered { .. }, Some(error)) if reached => BroadcastStatus::Failed(
            format!("delivered, but delivery status update failed: {}", error),
        ),
        (DeliveryReport::Skipped { reason }, _) => BroadcastStatus::Skipped(reason.clone()),
        (report, Some(error)) => BroadcastStatus::Failed(format!(
            "{}; status update failed: {}",
            report.short_status(),
            error
        )),
        (report, None) => BroadcastStatus::Failed(report.short_status()),
    };

    (result(status), reached)
}

#[async_trait]
impl Tool for ToolTaskBroadcast {
    fn tool_description(&self) -> ToolDesc {
        let push_mode_schema = planner_delivery::push_mode_schema();
        ToolDesc {
            name: "task_broadcast".to_string(),
            display_name: "Task Broadcast".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Planner-only tool that broadcasts the same user message into all running task agent chats.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Message to send to every running task agent"
                    },
                    "exclude_cards": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Doing card IDs whose agents should not receive the broadcast"
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["info", "steer", "urgent"],
                        "description": "Wording of the message only (FYI/STEER/URGENT framing). It does NOT change delivery timing; use `push` for that. Default: steer"
                    },
                    "push": push_mode_schema,
                    "task_id": {
                        "type": "string",
                        "description": "Task ID (optional if chat is bound to a task)"
                    }
                },
                "required": ["message"]
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
        let task_id = planner_task_id(&ccx, args).await?;
        let message = required_string(args, "message")?;
        let excluded = parse_exclude_cards(args)?;
        let priority = BroadcastPriority::parse(args.get("priority"))?;
        // priority only shapes the wording; delivery timing comes from `push`.
        let push = planner_delivery::parse_push_mode(args)?;
        let broadcast_message = priority.format_message(&message);

        let (ctx, chat_facade) = {
            let ccx_lock = ccx.lock().await;
            (ccx_lock.app.chat_ctx(), ccx_lock.app.chat.facade.clone())
        };

        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let targets: Vec<CardTarget> = board
            .cards
            .iter()
            .filter(|card| card.column == "doing")
            .filter_map(|card| {
                card.agent_chat_id
                    .as_ref()
                    .map(|chat_id| CardTarget::from_card(card, chat_id.clone()))
            })
            .collect();

        let mut results = Vec::new();
        let mut notified_count = 0usize;
        for target in targets {
            if excluded.contains(&target.card_id) {
                results.push(BroadcastResult {
                    card_id: target.card_id,
                    title: target.title,
                    status: BroadcastStatus::SkippedExcluded,
                });
                continue;
            }

            let (result, delivered) = broadcast_to_target(
                &ctx,
                &task_id,
                target,
                &message,
                &broadcast_message,
                push,
                chat_facade.clone(),
            )
            .await;
            if delivered {
                notified_count += 1;
            }
            results.push(result);
        }

        Ok((
            false,
            vec![tool_message(
                tool_call_id,
                format_output(&results, notified_count, &message),
            )],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
