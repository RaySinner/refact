//! `create_room` — staff a card from the agent definitions the user already prepared.
//!
//! Two tools, two jobs. `spawn_agent` puts one agent on a card and starts it; `create_room` writes
//! the roster — who is on this card, what each of them owns, which files each of them edits — without
//! starting anybody. The planner then calls `spawn_agent` once per member, and each spawn claims the
//! reserved slot instead of appending a second entry.
//!
//! The members come from the subagent registry by name. A name that is not in the registry is an
//! error listing what is available: silently falling back to a default agent would staff the room
//! with someone the user never chose.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::AppState;
use refact_tasks::rooms;
use refact_runtime_api::task_storage as storage;
use refact_tasks::types::{BoardCard, StatusUpdate, TeamMember};
use crate::tools::tools_description::{
    json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType,
};
use refact_yaml_configs::customization_types::SubagentConfig;

/// Longest description quoted for one available agent in an unknown-name error.
const AVAILABLE_DESCRIPTION_LIMIT: usize = 80;
/// How many available agents an unknown-name error names before it just says how many there are.
const AVAILABLE_LIST_LIMIT: usize = 20;

/// One agent the planner asked to put in the room.
#[derive(Clone, Debug)]
pub struct RoomRequest {
    pub config_name: String,
    pub role: Option<String>,
    pub mandate: Option<String>,
    pub target_files: Vec<String>,
}

/// The members a `create_room` call asks for, built from the definitions it resolved.
///
/// Pure: it reads the card and the registry and returns either the roster or the reason there
/// isn't one. The tool writes the roster; everything that can be decided without touching the board
/// is decided here, so a bad call fails before it changes anything.
pub fn build_room_members(
    card: &BoardCard,
    requests: &[RoomRequest],
    registry: &[(String, SubagentConfig)],
) -> Result<Vec<TeamMember>, String> {
    if requests.is_empty() {
        return Err("'agents' must name at least one agent definition".to_string());
    }
    if let Some(error) = rooms::reject_room_growth(card, requests.len()) {
        return Err(error);
    }

    let mut members = Vec::with_capacity(requests.len());
    for (index, request) in requests.iter().enumerate() {
        let config_name = request.config_name.trim();
        let Some((_, config)) = registry
            .iter()
            .find(|(id, _)| id == config_name)
            .map(|(id, config)| (id, config))
        else {
            return Err(unknown_config_name_error(index, config_name, registry));
        };

        // Roles are free text. The planner may name a member for this card; failing that the
        // definition's own hint says why the agent belongs here, and failing that its name does.
        let role = request
            .role
            .as_deref()
            .map(str::trim)
            .filter(|role| !role.is_empty())
            .map(str::to_string)
            .or_else(|| config.room_hint().map(str::to_string))
            .unwrap_or_else(|| config.id.clone());

        members.push(rooms::planned_room_member(rooms::RoomSlot {
            role,
            mandate: request
                .mandate
                .clone()
                .map(|mandate| mandate.trim().to_string())
                .filter(|mandate| !mandate.is_empty()),
            writes: config.writes(),
            decision_maker: config.is_decision_maker(),
            room_role_hint: config.room_hint().map(str::to_string),
            tools: config.tools.clone(),
            target_files: normalized_files(&request.target_files),
        }));
    }

    // The room contract reads the *whole* roster — one decision maker, no two writers on one file —
    // so it can only be checked against the finished shape, never member by member.
    let mut candidate = card.clone();
    candidate.team_members.extend(members.iter().cloned());
    candidate
        .validate_team()
        .map_err(|error| format!("Card {}: {error}", card.id))?;

    Ok(members)
}

fn normalized_files(files: &[String]) -> Vec<String> {
    let mut seen = Vec::new();
    for file in files {
        let file = file.trim();
        if file.is_empty() || seen.iter().any(|kept| kept == file) {
            continue;
        }
        seen.push(file.to_string());
    }
    seen
}

/// A typo must never quietly become a default agent, so the error lists the registry instead.
pub fn unknown_config_name_error(
    index: usize,
    config_name: &str,
    registry: &[(String, SubagentConfig)],
) -> String {
    let mut message = format!(
        "agents[{index}]: no agent definition named '{config_name}' in this project's agent registry."
    );
    if registry.is_empty() {
        message.push_str(
            " The registry is empty, so there is nobody to staff a room with. Define an agent first.",
        );
        return message;
    }
    message.push_str("\nAvailable agents:");
    for (id, config) in registry.iter().take(AVAILABLE_LIST_LIMIT) {
        let description = config.description.trim();
        let description = if description.is_empty() {
            config.title.trim()
        } else {
            description
        };
        let mut chars = description.chars();
        let mut quoted: String = chars.by_ref().take(AVAILABLE_DESCRIPTION_LIMIT).collect();
        if chars.next().is_some() {
            quoted.push('…');
        }
        if quoted.is_empty() {
            message.push_str(&format!("\n- {id}"));
        } else {
            message.push_str(&format!("\n- {id} — {quoted}"));
        }
    }
    if registry.len() > AVAILABLE_LIST_LIMIT {
        message.push_str(&format!(
            "\n- …and {} more",
            registry.len() - AVAILABLE_LIST_LIMIT
        ));
    }
    message
}

/// Read the `agents` argument: a list of `config_name` plus optional per-member detail.
pub fn parse_agents(args: &HashMap<String, Value>) -> Result<Vec<RoomRequest>, String> {
    let raw = args.get("agents").ok_or(
        "Missing 'agents': the names of the agent definitions that should work on the card",
    )?;
    let items = raw
        .as_array()
        .ok_or("'agents' must be an array of objects, each with a 'config_name'")?;

    let mut requests = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(object) = item.as_object() else {
            return Err(format!(
                "agents[{index}] must be an object with a 'config_name'"
            ));
        };
        let config_name = object
            .get("config_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("agents[{index}] needs a non-empty 'config_name'"))?;
        requests.push(RoomRequest {
            config_name: config_name.to_string(),
            role: optional_text(object.get("role"), index, "role")?,
            mandate: optional_text(object.get("mandate"), index, "mandate")?,
            target_files: string_list(object.get("target_files"), index, "target_files")?,
        });
    }
    Ok(requests)
}

fn optional_text(value: Option<&Value>, index: usize, key: &str) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.trim().to_string())),
        Some(_) => Err(format!("agents[{index}].{key} must be a string")),
    }
}

fn string_list(value: Option<&Value>, index: usize, key: &str) -> Result<Vec<String>, String> {
    let Some(Value::Array(items)) = value else {
        if value.is_none() || matches!(value, Some(Value::Null)) {
            return Ok(Vec::new());
        }
        return Err(format!(
            "agents[{index}].{key} must be an array of file paths"
        ));
    };
    let mut paths: Vec<String> = Vec::new();
    for (position, item) in items.iter().enumerate() {
        let path = item
            .as_str()
            .ok_or_else(|| format!("agents[{index}].{key}[{position}] must be a string"))?
            .trim();
        if !paths.iter().any(|seen| seen == path) {
            paths.push(path.to_string());
        }
    }
    Ok(paths)
}

async fn planner_room_context(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
) -> Result<(AppState, String), String> {
    let ccx_lock = ccx.lock().await;
    let is_planner = ccx_lock
        .task_meta
        .as_ref()
        .map(|meta| meta.role == "planner")
        .unwrap_or(false);
    if !is_planner {
        return Err(
            "create_room can only be called by the task planner. Switch to the planner chat to staff a room."
                .to_string(),
        );
    }
    let task_id = match args.get("task_id").and_then(Value::as_str) {
        Some(task_id) if !task_id.trim().is_empty() => task_id.trim().to_string(),
        Some(_) => return Err("'task_id' must be a non-empty string".to_string()),
        None => ccx_lock
            .task_meta
            .as_ref()
            .map(|meta| meta.task_id.clone())
            .filter(|task_id| !task_id.is_empty())
            .ok_or_else(|| "Missing 'task_id' (and chat is not bound to a task)".to_string())?,
    };
    Ok((ccx_lock.app.clone(), task_id))
}

/// The registry as a sorted name → definition list, so an error lists agents in a stable order.
async fn sorted_subagent_registry(
    tool: &refact_runtime_api::tool_ctx::ToolCtx,
) -> Result<Vec<(String, SubagentConfig)>, String> {
    let registry = tool
        .mode_config
        .get_project_registry()
        .await
        .ok_or_else(|| {
            "Could not read the agent registry; a room can only be staffed from agent definitions."
                .to_string()
        })?;
    let mut entries: Vec<(String, SubagentConfig)> = registry.subagents.into_iter().collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(entries)
}

pub fn roster_summary(card_id: &str, members: &[TeamMember]) -> Result<String, String> {
    let order = rooms::writer_first_indices(members)?;
    let lines = order
        .into_iter()
        .map(|index| &members[index])
        .map(|member| {
            let mut line = format!("- {} — pending", member.role);
            if let Some(mandate) = member.mandate.as_deref() {
                line.push_str(&format!(": {mandate}"));
            }
            line.push_str(if member.writes() {
                " [writes]"
            } else {
                " [read-only]"
            });
            if member.is_decision_maker() {
                line.push_str(" [decides]");
            }
            if !member.target_files.is_empty() {
                line.push_str(&format!(" files: {}", member.target_files.join(", ")));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "# Room created on card {card_id}\n\nLaunch order (writer first):\n{lines}\n\n\
         Nobody has started yet. Call `spawn_agent(card_id=\"{card_id}\", role=...)` once per member \
         in this order, using exactly the role names above; each spawn claims its reserved slot."
    ))
}

pub struct ToolTaskCreateRoom;

impl ToolTaskCreateRoom {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ToolTaskCreateRoom {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "create_room".to_string(),
            display_name: "Task Create Room".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Staff a card with agents from the registry. Multi-member rooms require exactly one writer and any number of advisors. This atomically reserves every slot and returns writer-first launch order; follow it with spawn_agent in that order, using the same role names. Use spawn_agent alone when one agent is enough."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task UUID (optional in the planner chat)"
                    },
                    "card_id": {
                        "type": "string",
                        "description": "Card ID to staff (e.g., T-1)"
                    },
                    "agents": {
                        "type": "array",
                        "description": "One entry per agent, each naming an existing agent definition.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "config_name": {
                                    "type": "string",
                                    "description": "Registry id of the agent definition, exactly as the registry spells it (e.g. code_edit, design_review)"
                                },
                                "role": {
                                    "type": "string",
                                    "description": "Free-text label for this member on this card (default: the definition's room_role_hint, else its id). Use the same string again in spawn_agent so the spawn claims this slot."
                                },
                                "mandate": {
                                    "type": "string",
                                    "description": "What this member is responsible for here; peers read it on the roster."
                                },
                                "target_files": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Files this member owns. Two members that both write the same file are rejected."
                                }
                            },
                            "required": ["config_name"]
                        }
                    }
                },
                "required": ["card_id", "agents"]
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
        let (app, task_id) = planner_room_context(&ccx, args).await?;
        let card_id = args
            .get("card_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|card_id| !card_id.is_empty())
            .ok_or("Missing 'card_id'")?
            .to_string();
        let requests = parse_agents(args)?;
        let registry = sorted_subagent_registry(&app.tool).await?;

        let card = storage::load_board_for_ctx(&app.chat_ctx(), &task_id)
            .await?
            .get_card(&card_id)
            .cloned()
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        let members = build_room_members(&card, &requests, &registry)?;

        let roster = members.clone();
        let card_id_for_update = card_id.clone();
        storage::update_board_atomic_for_ctx(&app.chat_ctx(), &task_id, move |board| {
            let card = board
                .get_card_mut(&card_id_for_update)
                .ok_or_else(|| format!("Card {} not found", card_id_for_update))?;
            if !card.team_members.is_empty() {
                return Err(format!(
                    "Card {} already has a room of {} members. Change the existing roster with \
                     board_update instead of replacing it.",
                    card_id_for_update,
                    card.team_members.len()
                ));
            }
            if let Some(error) = rooms::reject_room_growth(card, roster.len()) {
                return Err(error);
            }
            let mut candidate = card.clone();
            candidate.team_members = roster.clone();
            candidate
                .validate_team()
                .map_err(|error| format!("Card {}: {error}", candidate.id))?;
            card.team_members = candidate.team_members;
            card.status_updates.push(StatusUpdate {
                timestamp: chrono::Utc::now().to_rfc3339(),
                message: format!("Room created with {} reserved members", roster.len()),
            });
            Ok(Some(()))
        })
        .await?;

        storage::update_task_stats_for_ctx(&app.chat_ctx(), &task_id).await?;

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(roster_summary(&card_id, &members)?),
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

// The schema helper is the shared shape every builtin tool uses; referencing it keeps the two
// schema builders on this tool from drifting apart.
#[allow(dead_code)]
fn _flat_schema_reference() -> Value {
    json_schema_from_params(&[], &[])
}
