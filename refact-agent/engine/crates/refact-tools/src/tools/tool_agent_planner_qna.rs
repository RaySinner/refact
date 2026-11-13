use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;
use uuid::Uuid;

use refact_runtime_api::AtCommandsContext;
use refact_runtime_api::AppState;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{BoardCard, StatusUpdate};
use crate::tools::task_tool_helpers::require_bound_planner_task;
use crate::tools::planner_delivery::{self, CardTarget, CardGuard};
use refact_core::chat_types::DeliveryOutcome;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

const ASK_PREFIX: &str = "[ASK:";
const REPLY_PREFIX: &str = "[REPLY:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuestionUrgency {
    Info,
    Block,
}

impl QuestionUrgency {
    fn parse(value: Option<&Value>) -> Result<Self, String> {
        match value.and_then(|value| value.as_str()).unwrap_or("info") {
            "info" => Ok(Self::Info),
            "block" => Ok(Self::Block),
            other => Err(format!(
                "Invalid urgency '{}', must be one of: info, block",
                other
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Block => "block",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnansweredQuestion {
    card_id: String,
    card_title: String,
    question_id: String,
    urgency: String,
    question: String,
}

pub struct ToolAgentAskPlanner;
pub struct ToolPlannerReply;
pub struct ToolTaskQuestionsList;

impl ToolAgentAskPlanner {
    pub fn new() -> Self {
        Self
    }
}

impl ToolPlannerReply {
    pub fn new() -> Self {
        Self
    }
}

impl ToolTaskQuestionsList {
    pub fn new() -> Self {
        Self
    }
}

fn make_source() -> ToolSource {
    ToolSource {
        source_type: ToolSourceType::Builtin,
        config_path: String::new(),
    }
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

fn required_string(args: &HashMap<String, Value>, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Missing '{}'", key))
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub fn qna_truncation_notice(kind: &str, shown: usize, total: usize, setting: &str) -> String {
    format!(
        "⚠️ showing {} of {} {} characters (limit: {} = {}). 💡 Raise {} in trajectory settings or shorten the {}.",
        shown, total, kind, setting, shown, setting, kind
    )
}

fn truncate_chars_with_notice(
    value: &str,
    max_chars: usize,
    kind: &str,
    setting: &str,
) -> (String, Option<String>) {
    let total = value.chars().count();
    if total <= max_chars {
        return (value.to_string(), None);
    }
    (
        truncate_chars(value, max_chars),
        Some(qna_truncation_notice(kind, max_chars, total, setting)),
    )
}

pub fn make_question_id() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

fn is_question_id(value: &str) -> bool {
    value.len() == 8
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn required_question_id(args: &HashMap<String, Value>) -> Result<String, String> {
    let question_id = required_string(args, "question_id")?;
    if !is_question_id(&question_id) {
        return Err("question_id must be 8 lowercase hex characters".to_string());
    }
    Ok(question_id)
}

async fn agent_scope(
    ccx: &Arc<AMutex<AtCommandsContext>>,
) -> Result<(AppState, String, String, Option<String>), String> {
    let ccx_lock = ccx.lock().await;
    let meta = ccx_lock
        .task_meta
        .as_ref()
        .ok_or_else(|| "agent_ask_planner can only be called by task agents.".to_string())?;
    if meta.role != "agents" {
        return Err("agent_ask_planner can only be called by task agents.".to_string());
    }
    let card_id = meta
        .card_id
        .clone()
        .ok_or_else(|| "agent_ask_planner requires a bound card_id.".to_string())?;
    Ok((
        ccx_lock.app.clone(),
        meta.task_id.clone(),
        card_id,
        meta.planner_chat_id.clone(),
    ))
}

async fn require_planner_role(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    tool_name: &str,
) -> Result<(), String> {
    let ccx_lock = ccx.lock().await;
    let meta = ccx_lock
        .task_meta
        .as_ref()
        .ok_or_else(|| format!("{} can only be called by the task planner.", tool_name))?;
    if meta.role != "planner" {
        return Err(format!(
            "{} can only be called by the task planner.",
            tool_name
        ));
    }
    Ok(())
}

async fn planner_task_id(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
    tool_name: &str,
) -> Result<String, String> {
    require_planner_role(ccx, tool_name).await?;
    require_bound_planner_task(ccx, args).await
}

fn parse_status_id<'a>(message: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = message.strip_prefix(prefix)?;
    let end = rest.find(']')?;
    let id = &rest[..end];
    if !is_question_id(id) {
        return None;
    }
    Some((id, rest[end + 1..].trim_start()))
}

fn parse_ask(message: &str) -> Option<(&str, &str)> {
    parse_status_id(message, ASK_PREFIX)
}

fn parse_reply(message: &str) -> Option<(&str, &str)> {
    parse_status_id(message, REPLY_PREFIX)
}

fn parse_block_marker(message: &str) -> Option<&str> {
    let rest = message.strip_prefix(ASK_PREFIX)?;
    let id = rest.strip_suffix(":block] agent flagged for planner attention")?;
    if !is_question_id(id) {
        return None;
    }
    Some(id)
}

fn card_has_ask(card: &BoardCard, question_id: &str) -> bool {
    card.status_updates.iter().any(|update| {
        parse_ask(&update.message)
            .map(|(id, _)| id == question_id)
            .unwrap_or(false)
    })
}

fn split_question_and_urgency(text: &str) -> (String, String) {
    let trimmed = text.trim();
    for urgency in ["block", "info"] {
        let suffix = format!(" (urgency={})", urgency);
        if let Some(question) = trimmed.strip_suffix(&suffix) {
            return (question.to_string(), urgency.to_string());
        }
    }
    (trimmed.to_string(), "info".to_string())
}

pub fn collect_unanswered_questions(cards: &[BoardCard]) -> Vec<UnansweredQuestion> {
    let mut replied = HashSet::new();
    let mut block_markers = HashSet::new();
    for card in cards {
        for update in &card.status_updates {
            if let Some((id, _)) = parse_reply(&update.message) {
                replied.insert((card.id.clone(), id.to_string()));
            }
            if let Some(id) = parse_block_marker(&update.message) {
                block_markers.insert((card.id.clone(), id.to_string()));
            }
        }
    }

    let mut seen = HashSet::new();
    let mut questions = Vec::new();
    for card in cards {
        for update in &card.status_updates {
            let Some((id, text)) = parse_ask(&update.message) else {
                continue;
            };
            let key = (card.id.clone(), id.to_string());
            if replied.contains(&key) || !seen.insert(key.clone()) {
                continue;
            }
            let (question, mut urgency) = split_question_and_urgency(text);
            if block_markers.contains(&key) {
                urgency = "block".to_string();
            }
            questions.push(UnansweredQuestion {
                card_id: card.id.clone(),
                card_title: card.title.clone(),
                question_id: id.to_string(),
                urgency,
                question,
            });
        }
    }
    questions.sort_by(|a, b| {
        a.card_id
            .cmp(&b.card_id)
            .then_with(|| a.question_id.cmp(&b.question_id))
    });
    questions
}

fn markdown_table_cell(value: &str) -> String {
    value
        .replace('\r', "")
        .replace('\n', "<br>")
        .replace('|', "\\|")
}

pub fn format_unanswered_questions(questions: &[UnansweredQuestion]) -> String {
    if questions.is_empty() {
        return "# Unanswered Planner Questions\n\nNo unanswered planner questions.".to_string();
    }

    let mut lines = vec![
        "# Unanswered Planner Questions".to_string(),
        String::new(),
        "| Card | Title | Question ID | Urgency | Question |".to_string(),
        "|---|---|---|---|---|".to_string(),
    ];
    for question in questions {
        lines.push(format!(
            "| `{}` | {} | `{}` | {} | {} |",
            markdown_table_cell(&question.card_id),
            markdown_table_cell(&question.card_title),
            question.question_id,
            question.urgency,
            markdown_table_cell(&question.question)
        ));
    }
    lines.join("\n")
}

#[async_trait]
impl Tool for ToolAgentAskPlanner {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "agent_ask_planner".to_string(),
            display_name: "Agent Ask Planner".to_string(),
            source: make_source(),
            experimental: false,
            allow_parallel: false,
            description: "Task-agent-only tool for recording a question for the task planner on the current card. urgency=block additionally requests a planner wake (append by default, without preemption); urgency=info waits for the planner's next task_list poll.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "push": planner_delivery::push_mode_schema(),
                    "question": {
                        "type": "string",
                        "description": "Question for the planner"
                    },
                    "urgency": {
                        "type": "string",
                        "enum": ["info", "block"],
                        "description": "Question urgency. block requests a planner wake to answer without preemption by default; info waits for the planner's next task_list poll. Neither pauses the agent. Default: info"
                    }
                },
                "required": ["question", "urgency"]
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
        let (app, task_id, card_id, planner_chat_id) = agent_scope(&ccx).await?;
        let question_limit = refact_core::trajectory_settings::current().planner_qna_question_limit;
        let (question, question_notice) = truncate_chars_with_notice(
            &required_string(args, "question")?,
            question_limit,
            "question",
            "planner_qna_question_limit",
        );
        let urgency = QuestionUrgency::parse(args.get("urgency"))?;
        let push = planner_delivery::parse_push_mode(args)?;
        let question_id = make_question_id();
        let message = format!(
            "[ASK:{}] {} (urgency={})",
            question_id,
            question,
            urgency.as_str()
        );
        let block_message = if urgency == QuestionUrgency::Block {
            Some(format!(
                "[ASK:{}:block] agent flagged for planner attention",
                question_id
            ))
        } else {
            None
        };
        let card_id_for_update = card_id.clone();
        let timestamp = Utc::now().to_rfc3339();
        storage::update_board_atomic_for_ctx(&app.ctx, &task_id, move |board| {
            let card = board
                .get_card_mut(&card_id_for_update)
                .ok_or_else(|| format!("Card {} not found", card_id_for_update))?;
            card.status_updates.push(StatusUpdate {
                timestamp: timestamp.clone(),
                message: message.clone(),
            });
            if let Some(block_message) = &block_message {
                card.status_updates.push(StatusUpdate {
                    timestamp: timestamp.clone(),
                    message: block_message.clone(),
                });
            }
            Ok(())
        })
        .await?;

        let wake_note = if urgency == QuestionUrgency::Block {
            match planner_chat_id {
                Some(planner_chat_id) => {
                    let app = ccx.lock().await.app.clone();
                    match refact_chat_api::agent_notifications::notify_planner_blocking_question(
                        app.chat_ctx(),
                        &task_id,
                        &card_id,
                        &planner_chat_id,
                        &question_id,
                        &question,
                        push,
                    )
                    .await
                    {
                        Ok(DeliveryOutcome::Delivered) => "\n\nBlocking question delivered to planner; wake requested.",
                        Ok(DeliveryOutcome::Queued) => "\n\nBlocking question queued for planner; wake requested without overriding push mode.",
                        Ok(DeliveryOutcome::Duplicate) => "\n\nBlocking question already accepted; duplicate not delivered.",
                        Err(error) => {
                            tracing::warn!(
                                "agent_ask_planner: failed to wake planner for blocking question {} on card {}: {}",
                                question_id,
                                card_id,
                                error
                            );
                            "\n\nPlanner wake-up failed; the question is durably recorded and will surface on the planner's next task_list call."
                        }
                    }
                }
                None => {
                    "\n\nNo planner_chat_id bound to this agent; the question will surface on the planner's next task_list call."
                }
            }
        } else {
            ""
        };

        let mut output = format!(
            "Question recorded for planner.\n\n- card_id: `{}`\n- question_id: `{}`\n- urgency: `{}`\n\nPlanner reply instruction: call `planner_reply(card_id=\"{}\", question_id=\"{}\", answer=\"...\")`.{}",
            card_id,
            question_id,
            urgency.as_str(),
            card_id,
            question_id,
            wake_note
        );
        if let Some(notice) = question_notice {
            output = format!("{}\n\n{}", notice, output);
        }
        Ok((false, vec![tool_message(tool_call_id, output)]))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolPlannerReply {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "planner_reply".to_string(),
            display_name: "Planner Reply".to_string(),
            source: make_source(),
            experimental: false,
            allow_parallel: false,
            description: "Planner-only tool for answering an agent_ask_planner question recorded on a task card.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "push": planner_delivery::push_mode_schema(),
                    "card_id": {
                        "type": "string",
                        "description": "Card ID containing the question"
                    },
                    "question_id": {
                        "type": "string",
                        "description": "8 lowercase hex question ID returned by agent_ask_planner"
                    },
                    "answer": {
                        "type": "string",
                        "description": "Planner answer to record"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID (optional if planner chat is bound to a task)"
                    }
                },
                "required": ["card_id", "question_id", "answer"]
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
        let task_id = planner_task_id(&ccx, args, "planner_reply").await?;
        let card_id = required_string(args, "card_id")?;
        let question_id = required_question_id(args)?;
        let answer_limit = refact_core::trajectory_settings::current().planner_qna_answer_limit;
        let (answer, answer_notice) = truncate_chars_with_notice(
            &required_string(args, "answer")?,
            answer_limit,
            "answer",
            "planner_qna_answer_limit",
        );
        let app = ccx.lock().await.app.clone();
        let facade = app.chat.facade.clone();
        let push = planner_delivery::parse_push_mode(args)?;

        let card_id_for_update = card_id.clone();
        let question_id_for_update = question_id.clone();
        let answer_for_update = answer.clone();
        let (_, (recorded, target)) =
            storage::update_board_atomic_for_ctx(&app.ctx, &task_id, move |board| {
                let card = board
                    .get_card_mut(&card_id_for_update)
                    .ok_or_else(|| format!("Card {} not found", card_id_for_update))?;
                if !card_has_ask(card, &question_id_for_update) {
                    return Err(format!(
                        "Question {} not found on card {}",
                        question_id_for_update, card_id_for_update
                    ));
                }
                let target = card
                    .agent_chat_id
                    .clone()
                    .map(|chat_id| CardTarget::from_card(card, chat_id));
                if card.status_updates.iter().any(|update| {
                    parse_reply(&update.message)
                        .map(|(id, _)| id == question_id_for_update)
                        .unwrap_or(false)
                }) {
                    return Ok((false, target));
                }
                card.status_updates.push(StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: format!("[REPLY:{}] {}", question_id_for_update, answer_for_update),
                });
                Ok((true, target))
            })
            .await?;

        let delivery_note = if !recorded {
            "Reply already recorded; duplicate answer not delivered.".to_string()
        } else if let Some(target) = target {
            let pending = planner_delivery::with_dedupe_id(
                planner_delivery::single_message_delivery(
                    format!("[Planner REPLY to {}] {}", question_id, answer),
                    push,
                    "planner_reply",
                    true,
                ),
                format!("planner-reply:{}:{}:{}", task_id, card_id, question_id),
            );
            planner_delivery::deliver_to_card(
                &app.chat_ctx(),
                facade,
                &task_id,
                &target,
                CardGuard::doing_with_live_session(),
                pending,
            )
            .await
            .describe("Reply")
        } else {
            "Reply not delivered (card has no agent chat).".to_string()
        };
        let mut output = format!(
            "Reply {} on card `{}` for question `{}`.\n\n{}{}",
            if recorded {
                "recorded"
            } else {
                "already recorded"
            },
            card_id,
            question_id,
            delivery_note,
            if recorded {
                format!("\n\nAnswer: {}", answer)
            } else {
                String::new()
            }
        );
        if let Some(notice) = answer_notice {
            output = format!("{}\n\n{}", notice, output);
        }
        Ok((false, vec![tool_message(tool_call_id, output)]))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolTaskQuestionsList {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "task_questions_list".to_string(),
            display_name: "Task Questions List".to_string(),
            source: make_source(),
            experimental: false,
            allow_parallel: true,
            description: "Planner-only tool that lists unanswered agent questions recorded in card status updates.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task ID (optional if planner chat is bound to a task)"
                    }
                },
                "required": []
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
        let task_id = planner_task_id(&ccx, args, "task_questions_list").await?;
        let ctx = ccx.lock().await.app.ctx.clone();
        let board = storage::load_board_for_ctx(&ctx, &task_id).await?;
        let questions = collect_unanswered_questions(&board.cards);
        Ok((
            false,
            vec![tool_message(
                tool_call_id,
                format_unanswered_questions(&questions),
            )],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
