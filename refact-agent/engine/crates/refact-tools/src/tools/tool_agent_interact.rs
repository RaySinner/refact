use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_core::chat_types::{PendingDelivery, PushMode};
use refact_agents_core::types::{BackgroundAgent, BgAgentStatus};
use refact_runtime_api::AppState;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::global_context::GlobalContext;
use refact_postprocessing::pp_command_output::OutputFilter;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

pub struct ToolAgentsOverview {
    pub config_path: String,
}

pub struct ToolAgentMessage {
    pub config_path: String,
}

pub struct ToolProgressReport {
    pub config_path: String,
}

fn tool_desc(
    config_path: String,
    name: &str,
    display_name: &str,
    description: &str,
    schema: Value,
) -> ToolDesc {
    ToolDesc {
        name: name.to_string(),
        display_name: display_name.to_string(),
        source: ToolSource {
            source_type: ToolSourceType::Builtin,
            config_path,
        },
        experimental: false,
        allow_parallel: true,
        description: description.to_string(),
        input_schema: schema,
        output_schema: None,
        annotations: None,
    }
}

fn output(tool_call_id: &str, text: String) -> (bool, Vec<ContextEnum>) {
    (
        false,
        vec![ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(text),
            tool_call_id: tool_call_id.to_string(),
            preserve: Some(true),
            output_filter: Some(OutputFilter::no_limits()),
            ..Default::default()
        })],
    )
}

fn required_string(args: &HashMap<String, Value>, name: &str) -> Result<String, String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Missing '{name}'"))
}

fn optional_bool(args: &HashMap<String, Value>, name: &str) -> Result<bool, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(value) => {
            refact_tool_api::coerce_bool(value).ok_or_else(|| format!("{name} must be a boolean"))
        }
    }
}

fn optional_string(args: &HashMap<String, Value>, name: &str) -> Option<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

async fn context(
    ccx: &Arc<AMutex<AtCommandsContext>>,
) -> (AppState, String, String, Option<String>) {
    let ccx = ccx.lock().await;
    (
        ccx.app.clone(),
        ccx.chat_id.clone(),
        ccx.root_chat_id.clone(),
        ccx.background_agent_id.clone(),
    )
}

fn include_overview_record(record: &BackgroundAgent) -> bool {
    !record.status.is_terminal()
        || record
            .finished_at
            .map(|finished_at| finished_at >= Utc::now() - Duration::hours(1))
            .unwrap_or(false)
}

pub async fn records_for_root(app: &AppState, root_chat_id: &str) -> Vec<(BackgroundAgent, usize)> {
    let all = app.agents.list_all().await;
    let mut by_parent: HashMap<&str, Vec<&BackgroundAgent>> = HashMap::new();
    for record in &all {
        by_parent
            .entry(record.parent_chat_id.as_str())
            .or_default()
            .push(record);
    }
    for records in by_parent.values_mut() {
        records.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.agent_id.cmp(&b.agent_id))
        });
    }
    let mut pending = VecDeque::from([(root_chat_id.to_string(), 0usize)]);
    let mut seen_chats = HashSet::new();
    let mut seen_agents = HashSet::new();
    let mut result = Vec::new();
    while let Some((parent_chat_id, depth)) = pending.pop_front() {
        if !seen_chats.insert(parent_chat_id.clone()) {
            continue;
        }
        for record in by_parent.get(parent_chat_id.as_str()).into_iter().flatten() {
            if !seen_agents.insert(record.agent_id.clone()) {
                continue;
            }
            if let Some(child_chat_id) = &record.child_chat_id {
                pending.push_back((child_chat_id.clone(), depth + 1));
            }
            result.push(((*record).clone(), depth));
        }
    }
    result
}

async fn can_message_agent(
    app: &AppState,
    caller_chat_id: &str,
    root_chat_id: &str,
    caller_agent_id: Option<&str>,
    target_agent_id: &str,
) -> Result<BackgroundAgent, String> {
    let target = app.agents.get_any(target_agent_id).await?;
    if target.parent_chat_id == caller_chat_id {
        return Ok(target);
    }
    if let Some(caller_agent_id) = caller_agent_id {
        if app
            .agents
            .list_descendants(caller_agent_id)
            .await
            .iter()
            .any(|record| record.agent_id == target_agent_id)
        {
            return Ok(target);
        }
    } else {
        let in_root_tree = app.agents.list_all().await.iter().any(|record| {
            record.agent_id == target_agent_id
                && (record.parent_chat_id == root_chat_id
                    || record.parent_root_chat_id.as_deref() == Some(root_chat_id))
        });
        if in_root_tree {
            return Ok(target);
        }
    }
    Err(OUT_OF_REACH.to_string())
}

/// The rejection every out-of-reach target gets, whether it was excluded by the
/// agent tree or by the room it belongs to.
const OUT_OF_REACH: &str = "You may only message your direct children or descendants.";

/// True when `to` names a member of a *different* card of the same task.
///
/// Such a peer exists, so "agent not found" would be misleading: the room
/// scope, not the agent tree, is what makes it unreachable.
async fn target_is_foreign_room_member(
    gcx: Arc<GlobalContext>,
    task_id: &str,
    caller_card_id: &str,
    to: &str,
) -> bool {
    let Ok(board) = refact_runtime_api::task_storage::load_board(gcx, task_id).await else {
        return false;
    };
    board
        .cards
        .iter()
        .filter(|card| card.id != caller_card_id)
        .flat_map(|card| card.team())
        .any(|member| {
            member.agent_chat_id.as_deref() == Some(to) || member.agent_id.as_deref() == Some(to)
        })
}

/// The `(task_id, card_id)` the calling chat works on, if it is a task agent bound to a card.
async fn room_card_id(ccx: &Arc<AMutex<AtCommandsContext>>) -> Option<(String, String)> {
    let guard = ccx.lock().await;
    let task_id = guard.task_meta.as_ref()?.task_id.clone();
    let card_id = guard.task_meta.as_ref()?.card_id.clone()?;
    if task_id.is_empty() || card_id.is_empty() {
        return None;
    }
    Some((task_id, card_id))
}

pub fn short_id(agent_id: &str) -> &str {
    agent_id.get(..agent_id.len().min(16)).unwrap_or(agent_id)
}

fn status_chip(status: BgAgentStatus) -> &'static str {
    match status {
        BgAgentStatus::Queued => "⏳ queued",
        BgAgentStatus::Running => "🟢 running",
        BgAgentStatus::WaitingForApproval => "✋ waiting",
        BgAgentStatus::Completed => "✅ completed",
        BgAgentStatus::Failed => "❌ failed",
        BgAgentStatus::Cancelled => "⏹ cancelled",
        BgAgentStatus::Interrupted => "⚠ interrupted",
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!(
        "{}…",
        text.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

fn overview_line(record: &BackgroundAgent, depth: usize) -> String {
    let targets = if record.target_files.is_empty() {
        "-".to_string()
    } else {
        truncate(&record.target_files.join(", "), 80)
    };
    let questions = record
        .questions
        .iter()
        .filter(|question| question.answer.is_none())
        .count();
    let model = record
        .model_type
        .as_deref()
        .map(|model_type| format!("{model_type} ({})", record.model))
        .unwrap_or_else(|| record.model.clone());
    let cost = record
        .cost_usd
        .map(|cost| format!(", ${cost:.4}"))
        .unwrap_or_default();
    format!(
        "{}- {} — {} — {} — step {} — tool: {} — targets: {} — branch: {} — merge: {} — model: {} — tokens: {}{} — questions: {}",
        "  ".repeat(depth),
        short_id(&record.agent_id),
        record.title,
        status_chip(record.status),
        record.step_count,
        record.current_tool.as_deref().unwrap_or("-"),
        targets,
        record.worktree_branch.as_deref().unwrap_or("-"),
        record.merge_status.as_deref().unwrap_or("-"),
        model,
        record.tokens_used,
        cost,
        questions,
    )
}

#[async_trait]
impl Tool for ToolAgentsOverview {
    fn tool_description(&self) -> ToolDesc {
        tool_desc(
            self.config_path.clone(),
            "agents_overview",
            "Agents Overview",
            "Show the active background-agent tree rooted at this chat, including recent finished agents, their work, and pending questions.",
            json!({"type": "object", "properties": {}, "required": []}),
        )
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        _args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let (app, _chat_id, root_chat_id, agent_id) = context(&ccx).await;
        let records = records_for_root(&app, &root_chat_id)
            .await
            .into_iter()
            .filter(|(record, _)| include_overview_record(record))
            .collect::<Vec<_>>();
        let mut text = String::from("# Agents Overview\n\n");
        if let Some(agent_id) = agent_id {
            text.push_str(&format!("You are agent {agent_id}.\n\n"));
        }
        if records.is_empty() {
            text.push_str("No active or recently finished agents in this tree.");
        } else {
            for (record, depth) in records {
                text.push_str(&overview_line(&record, depth));
                text.push('\n');
            }
        }
        Ok(output(tool_call_id, text))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolAgentMessage {
    fn tool_description(&self) -> ToolDesc {
        tool_desc(
            self.config_path.clone(),
            "agent_message",
            "Agent Message",
            "Send a message to a child or descendant agent, to a peer in the same agent room, or \
             to every peer in the room (omit `to` from a room chat), or use to=parent to ask or \
             notify the parent agent. Keep the text short: it is a summary for the room, never a \
             pasted tool output.",
            json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "A child/descendant agent id, a room peer's chat id, 'all' for every peer in your room, or 'parent'. Omit only when you are in a room and mean every peer."
                    },
                    "text": {"type": "string", "description": "Message text."},
                    "expects_reply": {"type": "boolean", "description": "When messaging parent, create a tracked question."},
                    "push": PushMode::schema(),
                    "reply_to": {"type": "string", "description": "Question id being answered when messaging a child."}
                },
                "required": ["text"]
            }),
        )
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let to = optional_string(args, "to");
        let text = required_string(args, "text")?;
        let expects_reply = optional_bool(args, "expects_reply")?;
        let push = PushMode::from_args(args)?;
        let reply_to = optional_string(args, "reply_to");
        let (app, chat_id, root_chat_id, caller_agent_id) = context(&ccx).await;

        if let Some((task_id, peer_card_id)) = room_card_id(&ccx).await {
            let is_broadcast = to.is_none() || to.as_deref() == Some("all");
            let is_parent = to.as_deref() == Some("parent");
            if !is_parent {
                let target = if is_broadcast { None } else { to.as_deref() };
                let session_arc = app
                    .chat
                    .sessions
                    .read()
                    .await
                    .get(&chat_id)
                    .cloned()
                    .ok_or_else(|| format!("Room chat '{chat_id}' is unavailable"))?;
                match refact_runtime_api::room_delivery::publish_explicit_message(
                    app.clone(),
                    session_arc,
                    tool_call_id,
                    &text,
                    target,
                    push,
                )
                .await
                {
                    Ok(report) => {
                        if report.delivered.is_empty() && report.failed.is_empty() {
                            return Err("No other room member to message on this card.".to_string());
                        }
                        let mut result = format!(
                            "Room event {} queued for: {}.",
                            report.event_id,
                            report.delivered.join(", ")
                        );
                        if !report.failed.is_empty() {
                            result.push_str(&format!(
                                " Failed recipients: {}.",
                                report
                                    .failed
                                    .iter()
                                    .map(|(recipient, error)| format!("{recipient}: {error}"))
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            ));
                        }
                        return Ok(output(tool_call_id, result));
                    }
                    Err(error) if is_broadcast => return Err(error),
                    Err(error) => {
                        if let Some(target) = target {
                            if target_is_foreign_room_member(
                                app.gcx.clone(),
                                &task_id,
                                &peer_card_id,
                                target,
                            )
                            .await
                            {
                                return Err(OUT_OF_REACH.to_string());
                            }
                        }
                        return Err(error);
                    }
                }
            }
        }

        let to = to.ok_or_else(|| "Missing 'to'".to_string())?;

        if to == "parent" {
            let Some(agent_id) = caller_agent_id else {
                return Err(
                    "You are not a subagent; use an agent id to message a child.".to_string(),
                );
            };
            let caller = app.agents.get_any(&agent_id).await?;
            let parent_notice_chat_id = caller.parent_chat_id.clone();
            if expects_reply {
                let (updated, question_id) =
                    app.agents.add_question(&agent_id, text.clone()).await?;
                refact_agents_core::spawn::emit_background_agent_update(app.clone(), &updated)
                    .await;
                {
                    refact_agents_core::push::push_notice_to_chat_with_mode(
                        app,
                        &parent_notice_chat_id,
                        format!(
                            "[subagent question] agent {agent_id} asks: {text} — answer with agent_message(to=\"{agent_id}\", reply_to=\"{question_id}\", text=...)"
                        ),
                        push,
                    )
                    .await?;
                }
                return Ok(output(
                    tool_call_id,
                    format!("Question {question_id} sent to parent."),
                ));
            }
            refact_agents_core::push::push_notice_to_chat_with_mode(
                app.clone(),
                &parent_notice_chat_id,
                format!("[subagent note] agent {agent_id}: {text}"),
                push,
            )
            .await?;
            return Ok(output(tool_call_id, "Note sent to parent.".to_string()));
        }

        let target = can_message_agent(
            &app,
            &chat_id,
            &root_chat_id,
            caller_agent_id.as_deref(),
            &to,
        )
        .await?;
        if let Some(question_id) = reply_to.as_deref() {
            let updated = app
                .agents
                .answer_question(&to, &question_id, text.clone())
                .await?;
            refact_agents_core::spawn::emit_background_agent_update(app.clone(), &updated).await;
            if target.status.is_terminal() {
                return Ok(output(
                    tool_call_id,
                    format!("Answer recorded for finished agent {to}."),
                ));
            }
        }
        let message_text = reply_to
            .as_deref()
            .map(|question_id| format!("Answer to {question_id}: {text}"))
            .unwrap_or(text);
        refact_agents_core::delivery::deliver_to_agent(
            app,
            &to,
            PendingDelivery::new(
                vec![refact_chat_api::internal_roles::event(
                    refact_chat_api::internal_roles::EventSubkind::SystemNotice,
                    "agents.message",
                    json!({"from": "parent"}),
                    format!("[message from parent]\n{message_text}"),
                )],
                push,
                "agents.message".to_string(),
                true,
            ),
        )
        .await?;
        Ok(output(tool_call_id, format!("Message queued for {to}.")))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolProgressReport {
    fn tool_description(&self) -> ToolDesc {
        tool_desc(
            self.config_path.clone(),
            "progress_report",
            "Progress Report",
            "Publish a concise progress update for this background agent.",
            json!({
                "type": "object",
                "properties": {"status_line": {"type": "string"}},
                "required": ["status_line"]
            }),
        )
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let status_line = required_string(args, "status_line")?;
        let (app, _chat_id, _root_chat_id, agent_id) = context(&ccx).await;
        let Some(agent_id) = agent_id else {
            return Err(
                "You are not a subagent; progress_report is only available to background agents."
                    .to_string(),
            );
        };
        let updated = app
            .agents
            .update_activity(&agent_id, Some(status_line.clone()), None, None)
            .await?;
        refact_agents_core::spawn::emit_background_agent_update(app, &updated).await;
        Ok(output(
            tool_call_id,
            format!("Progress updated: {status_line}"),
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
