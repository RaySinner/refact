use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::task_storage as storage;
use crate::tools::planner_delivery::{self, CardGuard, CardTarget, DeliveryReport};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_core::chat_types::PushMode;
use refact_runtime_api::SessionState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSteerPriority {
    Info,
    Steer,
    Urgent,
}

impl AgentSteerPriority {
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

pub struct ToolAgentSteer;

impl ToolAgentSteer {
    pub fn new() -> Self {
        Self
    }
}

fn required_string(args: &HashMap<String, Value>, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Missing '{}'", key))
}

async fn planner_task_id(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
) -> Result<String, String> {
    let ccx_lock = ccx.lock().await;
    let meta = ccx_lock
        .task_meta
        .as_ref()
        .ok_or_else(|| "agent_steer can only be called by the task planner.".to_string())?;
    if meta.role != "planner" {
        return Err("agent_steer can only be called by the task planner.".to_string());
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

fn state_label(state: SessionState) -> String {
    match state {
        SessionState::Starting => "⏳ Starting".to_string(),
        SessionState::Idle => "💤 Idle".to_string(),
        SessionState::Generating => "🔄 Generating response".to_string(),
        SessionState::ExecutingTools => "⚙️ Executing tools".to_string(),
        SessionState::Paused => "⏸️ Paused".to_string(),
        SessionState::WaitingIde => "⏳ Waiting for IDE".to_string(),
        SessionState::WaitingUserInput => "⏳ Waiting for user input".to_string(),
        SessionState::Completed => "✅ Completed".to_string(),
        SessionState::Error => "❌ Error".to_string(),
    }
}

fn tool_output(
    card_id: &str,
    message: &str,
    state: SessionState,
    compacted: bool,
    push: PushMode,
    report: &DeliveryReport,
) -> String {
    let compaction_note = if compacted {
        "\nContext rebuild: queued before resumed generation."
    } else {
        ""
    };
    let headline = if report.reached_agent() {
        format!("✅ Steered {}", card_id)
    } else {
        format!("⚠️ Did not steer {}", card_id)
    };
    format!(
        "{}\n\nMessage: \"{}\"\nDelivery: {} (push={})\nAgent state: {}{}",
        headline,
        message,
        report.describe("Steer"),
        planner_delivery::push_mode_label(push),
        state_label(state),
        compaction_note
    )
}

fn should_autocompact_before_steer(
    state: SessionState,
    thread: &refact_chat_api::ThreadParams,
) -> bool {
    thread.auto_compact_enabled_effective()
        && matches!(state, SessionState::Completed | SessionState::Error)
}

fn validate_agent_snapshot(
    snapshot: &refact_runtime_api::ChatSessionSnapshot,
    task_id: &str,
    card_id: &str,
    assignee: Option<&str>,
) -> Result<(), String> {
    let Some(task_meta) = &snapshot.thread.task_meta else {
        return Err(format!(
            "Agent session for card {card_id} is missing task metadata; it may be stale or deleted."
        ));
    };
    if task_meta.role != "agents" || task_meta.task_id != task_id {
        return Err(format!(
            "Agent session for card {card_id} is bound to a different task/role."
        ));
    }
    if task_meta.card_id.as_deref() != Some(card_id) {
        return Err(format!(
            "Agent session metadata does not match card {card_id}."
        ));
    }
    if let Some(assignee) = assignee {
        if task_meta.agent_id.as_deref() != Some(assignee) {
            return Err(format!(
                "Agent session for card {card_id} is assigned to a different agent."
            ));
        }
    }
    Ok(())
}

#[async_trait]
impl Tool for ToolAgentSteer {
    fn tool_description(&self) -> ToolDesc {
        let push_mode_schema = planner_delivery::push_mode_schema();
        ToolDesc {
            name: "agent_steer".to_string(),
            display_name: "Agent Steer".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Planner-only tool that injects a steering user message into a running task agent chat through the chat command queue.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "card_id": {
                        "type": "string",
                        "description": "Doing card whose running agent should receive the steering message"
                    },
                    "message": {
                        "type": "string",
                        "description": "What to tell the agent"
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
                "required": ["card_id", "message"]
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
        let card_id = required_string(args, "card_id")?;
        let message = required_string(args, "message")?;
        let priority = AgentSteerPriority::parse(args.get("priority"))?;
        // priority only shapes the wording; delivery timing comes from `push`,
        // which defaults to append even for priority=urgent.
        let push = planner_delivery::parse_push_mode(args)?;
        let steer_message = priority.format_message(&message);

        let (ctx, chat_facade) = {
            let ccx_lock = ccx.lock().await;
            (ccx_lock.app.chat_ctx(), ccx_lock.app.chat.facade.clone())
        };

        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let card = board
            .get_card(&card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        if card.column != "doing" {
            return Err(format!(
                "Card {} must be in 'doing' column to steer its agent; current column is '{}'.",
                card_id, card.column
            ));
        }
        let agent_chat_id = card.agent_chat_id.clone().ok_or_else(|| {
            format!(
                "Card {} has no agent_chat_id; no running agent to steer.",
                card_id
            )
        })?;

        let snapshot = chat_facade.session_snapshot(&agent_chat_id).await?;
        validate_agent_snapshot(&snapshot, &task_id, &card_id, card.assignee.as_deref())?;
        let session_state = snapshot.session_state;
        let compacted = if should_autocompact_before_steer(session_state, &snapshot.thread) {
            let session = ctx
                .chat
                .sessions
                .read()
                .await
                .get(&agent_chat_id)
                .cloned()
                .ok_or("Agent session not found")?;
            refact_chat_api::pending_context_rebuild::request_rebuild(
                &mut *session.lock().await,
                refact_chat_api::pending_context_rebuild::PendingContextRebuild {
                    model: None,
                    trigger: "agent_steer".into(),
                },
            )?;
            true
        } else {
            false
        };

        let target = CardTarget {
            card_id: card_id.clone(),
            title: card.title.clone(),
            chat_id: agent_chat_id.clone(),
        };
        let status_message = format!(
            "Planner steered: {}",
            planner_delivery::truncate_chars(&message, 80)
        );
        if let Some(reason) = planner_delivery::reserve_card_status(
            &ctx,
            &task_id,
            &target,
            status_message,
            true,
            true,
        )
        .await?
        {
            return Err(format!(
                "Card {} changed before steering could be recorded: {}.",
                card_id, reason
            ));
        }

        let report = planner_delivery::deliver_to_card(
            &ctx,
            chat_facade.clone(),
            &task_id,
            &target,
            CardGuard::doing(),
            planner_delivery::single_message_delivery(
                steer_message.clone(),
                push,
                "tools.agent_steer",
                true,
            ),
        )
        .await;

        if !report.reached_agent() {
            planner_delivery::record_card_status(
                &ctx,
                &task_id,
                &target,
                format!("Planner steer {}", report.short_status()),
            )
            .await?;
        }

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(tool_output(
                    &card_id,
                    &message,
                    session_state,
                    compacted,
                    push,
                    &report,
                )),
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
