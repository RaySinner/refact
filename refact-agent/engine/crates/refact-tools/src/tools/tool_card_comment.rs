use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::ChatCtx;
use refact_runtime_api::task_comments::create_card_comment_for_ctx;
use refact_tasks::comments::CreateCardComment;
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::CardComment;
use crate::tools::task_tool_helpers::{human_age, optional_string, required_string};
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

const DEFAULT_LIMIT: usize = 20;

pub struct ToolCardCommentAdd;
pub struct ToolCardCommentList;

struct CommentScope {
    ctx: ChatCtx,
    task_id: String,
    author_role: String,
    author_id: Option<String>,
    bound_card_id: Option<String>,
}

impl ToolCardCommentAdd {
    pub fn new() -> Self {
        Self
    }
}

impl ToolCardCommentList {
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

fn normalize_role(role: &str) -> Option<&'static str> {
    match role {
        "planner" => Some("planner"),
        "agent" | "agents" => Some("agents"),
        _ => None,
    }
}

async fn comment_scope(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
    tool_name: &str,
) -> Result<CommentScope, String> {
    let ccx_lock = ccx.lock().await;
    let meta = ccx_lock
        .task_meta
        .clone()
        .ok_or_else(|| format!("{} requires task context", tool_name))?;
    let author_role = normalize_role(&meta.role)
        .ok_or_else(|| {
            format!(
                "{} can only be called by task planners or task agents",
                tool_name
            )
        })?
        .to_string();
    if let Some(task_id) = optional_string(args, "task_id") {
        if task_id != meta.task_id {
            return Err("task_id override is not allowed from this task chat".to_string());
        }
    }
    let author_id = if author_role == "agents" {
        Some(
            meta.agent_id
                .clone()
                .unwrap_or_else(|| ccx_lock.chat_id.clone()),
        )
    } else {
        None
    };
    Ok(CommentScope {
        ctx: ccx_lock.app.ctx.clone(),
        task_id: meta.task_id,
        author_role,
        author_id,
        bound_card_id: meta.card_id,
    })
}

fn required_card_id(
    args: &HashMap<String, Value>,
    scope: &CommentScope,
    tool_name: &str,
) -> Result<String, String> {
    let card_id = required_string(args, "card_id")?;
    if scope.author_role == "agents" && scope.bound_card_id.as_deref() != Some(card_id.as_str()) {
        return Err(format!(
            "{} can only be called for the agent's bound card",
            tool_name
        ));
    }
    Ok(card_id)
}

fn parse_limit(args: &HashMap<String, Value>) -> Result<usize, String> {
    match args.get("limit") {
        None | Some(Value::Null) => Ok(DEFAULT_LIMIT),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(|value| value as usize)
            .ok_or_else(|| "limit must be a non-negative integer".to_string()),
        Some(Value::String(text)) => text
            .trim()
            .parse::<usize>()
            .map_err(|_| "limit must be a non-negative integer".to_string()),
        Some(_) => Err("limit must be a non-negative integer".to_string()),
    }
}

fn parse_since(args: &HashMap<String, Value>) -> Result<Option<DateTime<Utc>>, String> {
    let Some(since) = optional_string(args, "since") else {
        return Ok(None);
    };
    let parsed = DateTime::parse_from_rfc3339(&since)
        .map_err(|_| "since must be an RFC3339 timestamp".to_string())?;
    Ok(Some(parsed.with_timezone(&Utc)))
}

fn comment_is_newer_than(comment: &CardComment, since: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(&comment.timestamp)
        .map(|timestamp| timestamp.with_timezone(&Utc) > since)
        .unwrap_or(false)
}

fn author_label(comment: &CardComment) -> String {
    match comment.author_id.as_deref() {
        Some(author_id) if !author_id.is_empty() => {
            format!("**{} ({})**", comment.author_role, author_id)
        }
        _ => format!("**{}**", comment.author_role),
    }
}

fn age_label(timestamp: &str) -> String {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|timestamp| human_age(timestamp.with_timezone(&Utc)))
        .unwrap_or_else(|_| timestamp.to_string())
}

pub fn format_comments(
    card_id: &str,
    comments: &[CardComment],
    limit: usize,
    since: Option<DateTime<Utc>>,
) -> String {
    let mut selected = comments
        .iter()
        .filter(|comment| {
            since
                .as_ref()
                .map(|since| comment_is_newer_than(comment, *since))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if selected.len() > limit {
        selected = selected[selected.len() - limit..].to_vec();
    }

    let mut output = format!("# Comments on {} ({})\n", card_id, selected.len());
    if selected.is_empty() {
        output.push_str("\nNo comments.");
        return output;
    }

    for comment in selected {
        let reply = comment
            .reply_to
            .as_ref()
            .map(|reply_to| format!(", reply_to={}", reply_to))
            .unwrap_or_default();
        output.push_str("\n");
        output.push_str(&format!(
            "{} · {} [id={}{}]\n",
            author_label(comment),
            age_label(&comment.timestamp),
            comment.id,
            reply
        ));
        output.push_str(&comment.body);
        output.push('\n');
    }
    while output.ends_with('\n') {
        output.pop();
    }
    output
}

#[async_trait]
impl Tool for ToolCardCommentAdd {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "card_comment_add".to_string(),
            display_name: "Card Comment Add".to_string(),
            source: make_source(),
            experimental: false,
            allow_parallel: false,
            description: "Add a persistent discussion comment to a task card.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "card_id": {
                        "type": "string",
                        "description": "Card ID to comment on"
                    },
                    "body": {
                        "type": "string",
                        "description": "Comment body"
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "Optional parent comment id for threading"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID (optional if chat is bound to a task)"
                    }
                },
                "required": ["card_id", "body"]
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
        let scope = comment_scope(&ccx, args, "card_comment_add").await?;
        let card_id = required_card_id(args, &scope, "card_comment_add")?;
        let body = required_string(args, "body")?;
        let reply_to = optional_string(args, "reply_to");
        let (_, comment) = create_card_comment_for_ctx(
            &scope.ctx,
            &scope.task_id,
            CreateCardComment {
                card_id: card_id.clone(),
                body,
                author_role: scope.author_role.clone(),
                author_id: scope.author_id.clone(),
                reply_to,
            },
        )
        .await?;
        let comment_id = comment.id;

        let output = format!(
            "Comment added to card `{}`.\n\n- comment_id: `{}`",
            card_id, comment_id
        );
        Ok((false, vec![tool_message(tool_call_id, output)]))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolCardCommentList {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "card_comment_list".to_string(),
            display_name: "Card Comment List".to_string(),
            source: make_source(),
            experimental: false,
            allow_parallel: true,
            description: "List persistent discussion comments on a task card.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "card_id": {
                        "type": "string",
                        "description": "Card ID to list comments for"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum comments to return. Default: 20"
                    },
                    "since": {
                        "type": "string",
                        "description": "Only show comments newer than this RFC3339 timestamp"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID (optional if chat is bound to a task)"
                    }
                },
                "required": ["card_id"]
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
        let scope = comment_scope(&ccx, args, "card_comment_list").await?;
        let card_id = required_card_id(args, &scope, "card_comment_list")?;
        let limit = parse_limit(args)?;
        let since = parse_since(args)?;
        let board = storage::load_board_for_ctx(&scope.ctx, &scope.task_id).await?;
        let card = board
            .get_card(&card_id)
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        let output = format_comments(&card_id, &card.comments, limit, since);
        Ok((false, vec![tool_message(tool_call_id, output)]))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}
