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

use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{ChatContent, ChatMessage, ContextEnum};
use crate::global_context::GlobalContext;
use crate::tasks::events::{TaskEvent, emit_task_event};
use crate::tasks::rooms;
use crate::tasks::storage;
use crate::tasks::types::{BoardCard, StatusUpdate, TeamMember};
use crate::tools::tools_description::{
    json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType,
};
use crate::yaml_configs::customization_types::SubagentConfig;

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
fn unknown_config_name_error(
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
fn parse_agents(args: &HashMap<String, Value>) -> Result<Vec<RoomRequest>, String> {
    let raw = args
        .get("agents")
        .ok_or("Missing 'agents': the names of the agent definitions that should work on the card")?;
    let items = raw
        .as_array()
        .ok_or("'agents' must be an array of objects, each with a 'config_name'")?;

    let mut requests = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(object) = item.as_object() else {
            return Err(format!("agents[{index}] must be an object with a 'config_name'"));
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
    items
        .iter()
        .enumerate()
        .map(|(position, item)| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("agents[{index}].{key}[{position}] must be a string"))
        })
        .collect()
}

async fn planner_room_context(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
) -> Result<(Arc<GlobalContext>, String), String> {
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
            .ok_or_else(|| {
                "Missing 'task_id' (and chat is not bound to a task)".to_string()
            })?,
    };
    Ok((ccx_lock.app.gcx.clone(), task_id))
}

/// The registry as a sorted name → definition list, so an error lists agents in a stable order.
async fn sorted_subagent_registry(
    gcx: Arc<GlobalContext>,
) -> Result<Vec<(String, SubagentConfig)>, String> {
    let registry = crate::yaml_configs::customization_registry::get_project_registry(gcx)
        .await
        .ok_or_else(|| {
            "Could not read the agent registry; a room can only be staffed from agent definitions."
                .to_string()
        })?;
    let mut entries: Vec<(String, SubagentConfig)> = registry.subagents.into_iter().collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(entries)
}

fn roster_summary(card_id: &str, members: &[TeamMember]) -> String {
    let lines = members
        .iter()
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
                line.push_str(&format!(
                    " files: {}",
                    member.target_files.join(", ")
                ));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "# Room created on card {card_id}\n\n{lines}\n\n\
         Nobody has started yet. Call `spawn_agent(card_id=\"{card_id}\", role=...)` once per member, \
         using exactly the role names above; each spawn claims its reserved slot."
    )
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
            description: "Staff a card with several of the agents the user has prepared. Takes the agents' registry names (config_name), not job titles, and reserves a slot per agent with its mandate and the files it owns. This is the room tool: it writes the roster and starts nobody — follow it with one spawn_agent per member, using the same role name, and each spawn claims its reserved slot. Use spawn_agent alone when one agent is enough. A name that is not in the registry is an error listing the available agents; there is no default agent."
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
        let (gcx, task_id) = planner_room_context(&ccx, args).await?;
        let card_id = args
            .get("card_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|card_id| !card_id.is_empty())
            .ok_or("Missing 'card_id'")?
            .to_string();
        let requests = parse_agents(args)?;
        let registry = sorted_subagent_registry(gcx.clone()).await?;

        let card = storage::load_board(gcx.clone(), &task_id)
            .await?
            .get_card(&card_id)
            .cloned()
            .ok_or_else(|| format!("Card {} not found", card_id))?;
        let members = build_room_members(&card, &requests, &registry)?;

        let roster = members.clone();
        let card_id_for_update = card_id.clone();
        let (board, _) = storage::update_board_atomic(gcx.clone(), &task_id, move |board| {
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
            card.team_members = roster.clone();
            card.status_updates.push(StatusUpdate {
                timestamp: chrono::Utc::now().to_rfc3339(),
                message: format!("Room created with {} reserved members", roster.len()),
            });
            Ok(Some(()))
        })
        .await?;

        emit_task_event(
            gcx.clone(),
            TaskEvent::BoardChanged {
                task_id: task_id.clone(),
                rev: board.rev,
                board: board.clone(),
            },
        )
        .await;
        storage::update_task_stats(gcx.clone(), &task_id).await?;

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(roster_summary(&card_id, &members)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::types::TeamStatus;

    fn config(id: &str) -> SubagentConfig {
        SubagentConfig {
            schema_version: 1,
            id: id.to_string(),
            title: id.to_string(),
            description: String::new(),
            specific: false,
            expose_as_tool: false,
            has_code: false,
            tool: None,
            subchat: Default::default(),
            messages: Default::default(),
            prompts: Default::default(),
            gather_files: Default::default(),
            tools: Vec::new(),
            writes: true,
            decision_maker: false,
            room_role_hint: None,
            base: None,
            match_models: None,
            extra: Default::default(),
        }
    }

    fn registry() -> Vec<(String, SubagentConfig)> {
        vec![
            ("code_edit".to_string(), config("code_edit")),
            ("design_review".to_string(), config("design_review")),
            ("visual_qa".to_string(), config("visual_qa")),
        ]
    }

    fn card() -> BoardCard {
        BoardCard {
            id: "T-1".to_string(),
            title: "Card T-1".to_string(),
            column: "planned".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    fn request(config_name: &str) -> RoomRequest {
        RoomRequest {
            config_name: config_name.to_string(),
            role: None,
            mandate: None,
            target_files: Vec::new(),
        }
    }

    #[test]
    fn room_is_created_from_agent_config_names() {
        let members = build_room_members(
            &card(),
            &[request("code_edit"), request("design_review")],
            &registry(),
        )
        .unwrap();

        assert_eq!(members.len(), 2);
        assert_eq!(members[0].role, "code_edit");
        assert_eq!(members[1].role, "design_review");
        for member in &members {
            assert_eq!(member.member_status, Some(TeamStatus::Pending));
            assert!(
                member.agent_chat_id.is_none(),
                "a reserved slot has no chat until it is spawned"
            );
        }
        assert!(members[0].writes(), "code_edit has no writes:false in the test registry");
    }

    #[test]
    fn room_rejects_unknown_config_name() {
        let error = build_room_members(&card(), &[request("code_edti")], &registry()).unwrap_err();

        assert!(error.contains("agents[0]"), "{error}");
        assert!(error.contains("code_edti"), "{error}");
        assert!(
            error.contains("code_edit —"),
            "the error must name what is available: {error}"
        );
        assert!(error.contains("visual_qa"), "{error}");
    }

    #[test]
    fn unknown_config_name_error_lists_every_agent_when_the_registry_is_small() {
        let error = unknown_config_name_error(1, "nope", &registry());

        assert!(error.contains("agents[1]"), "{error}");
        assert!(error.contains("code_edit"), "{error}");
        assert!(error.contains("design_review"), "{error}");
        assert!(error.contains("visual_qa"), "{error}");
        assert!(
            !error.contains("more"),
            "a small registry must be listed in full: {error}"
        );
    }

    #[test]
    fn unknown_config_name_error_says_so_when_there_is_nobody() {
        let error = unknown_config_name_error(0, "code_edit", &[]);

        assert!(error.contains("registry is empty"), "{error}");
    }

    #[test]
    fn room_members_get_behaviour_stamped_from_definition() {
        let mut registry = registry();
        registry[0].1.writes = false;
        registry[0].1.room_role_hint = Some("  edits the parser  ".to_string());
        registry[0].1.tools = vec!["cat".to_string(), "apply_patch".to_string()];
        registry[1].1.decision_maker = true;
        registry[1].1.writes = false;
        registry[1].1.room_role_hint = Some("decides the room".to_string());

        let members =
            build_room_members(&card(), &[request("code_edit"), request("design_review")], &registry)
                .unwrap();

        let editor = &members[0];
        assert!(!editor.writes(), "writes: false must survive the stamp");
        assert_eq!(editor.room_role_hint.as_deref(), Some("edits the parser"));
        assert_eq!(
            editor.tools,
            vec!["cat".to_string(), "apply_patch".to_string()],
            "the roster must show peers what the definition allows"
        );
        assert_eq!(editor.role, "edits the parser", "role falls back to the hint");

        let decider = &members[1];
        assert!(decider.is_decision_maker());
        assert!(!decider.writes(), "a decision maker must not write");
        assert_eq!(decider.role, "decides the room");
    }

    #[test]
    fn room_honours_mandate_per_member() {
        let mut requests = vec![request("code_edit"), request("design_review")];
        requests[0].mandate = Some("  write the parser  ".to_string());
        requests[1].mandate = Some("read-only verdict on the UI".to_string());
        requests[1].role = Some("ui-gate".to_string());

        let members = build_room_members(&card(), &requests, &registry()).unwrap();

        assert_eq!(members[0].mandate.as_deref(), Some("write the parser"));
        assert_eq!(
            members[1].mandate.as_deref(),
            Some("read-only verdict on the UI")
        );
        assert_eq!(members[1].role, "ui-gate", "an explicit role wins");
    }

    #[test]
    fn room_honours_target_files_disjointness() {
        let mut ok_requests = vec![request("code_edit"), request("code_edit")];
        ok_requests[0].role = Some("parser".to_string());
        ok_requests[0].target_files = vec!["src/parser.rs".to_string()];
        ok_requests[1].role = Some("store".to_string());
        ok_requests[1].target_files = vec!["src/store.rs".to_string()];

        let members = build_room_members(&card(), &ok_requests, &registry()).unwrap();

        assert_eq!(members[0].target_files, vec!["src/parser.rs".to_string()]);
        assert_eq!(members[1].target_files, vec!["src/store.rs".to_string()]);

        let mut clashing = ok_requests.clone();
        clashing[1].target_files = vec![" src/parser.rs ".to_string(), "src/parser.rs".to_string()];

        let error = build_room_members(&card(), &clashing, &registry()).unwrap_err();

        assert!(error.contains("both write"), "{error}");
        assert!(error.contains("src/parser.rs"), "{error}");
    }

    #[test]
    fn room_rejects_more_than_max_size() {
        let requests: Vec<RoomRequest> = (0..rooms::MAX_ROOM_SIZE + 1)
            .map(|index| RoomRequest {
                config_name: "code_edit".to_string(),
                role: Some(format!("role-{index}")),
                mandate: None,
                target_files: Vec::new(),
            })
            .collect();

        let error = build_room_members(&card(), &requests, &registry()).unwrap_err();

        assert!(error.contains(&rooms::MAX_ROOM_SIZE.to_string()), "{error}");
    }

    #[test]
    fn room_without_decision_maker_is_allowed() {
        let members = build_room_members(
            &card(),
            &[request("code_edit"), request("design_review")],
            &registry(),
        )
        .unwrap();

        assert!(members.iter().all(|member| !member.is_decision_maker()));
    }

    #[test]
    fn two_decision_makers_rejected() {
        let mut registry = registry();
        for entry in registry.iter_mut() {
            entry.1.decision_maker = true;
            entry.1.writes = false;
        }

        let error = build_room_members(
            &card(),
            &[request("code_edit"), request("design_review")],
            &registry,
        )
        .unwrap_err();

        assert!(error.contains("decision maker"), "{error}");
    }

    #[test]
    fn room_refuses_a_terminal_card() {
        let mut card = card();
        card.column = "done".to_string();

        let error = build_room_members(&card(), &[request("code_edit")], &registry()).unwrap_err();

        assert!(error.contains("terminal column"), "{error}");
    }

    #[test]
    fn empty_agent_list_is_an_error() {
        let error = build_room_members(&card(), &[], &registry()).unwrap_err();

        assert!(error.contains("at least one"), "{error}");
    }

    #[test]
    fn parse_agents_reads_names_mandates_and_files() {
        let args = HashMap::from([(
            "agents".to_string(),
            json!([
                {"config_name": "code_edit", "target_files": ["src/a.rs", " src/a.rs ", ""]},
                {"config_name": "design_review", "role": "ui-gate", "mandate": "verdict"}
            ]),
        )]);

        let requests = parse_agents(&args).unwrap();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].config_name, "code_edit");
        assert_eq!(
            requests[0].target_files,
            vec!["src/a.rs".to_string(), "".to_string()],
            "parsing keeps the raw list; normalization is the builder's job"
        );
        assert_eq!(requests[1].role.as_deref(), Some("ui-gate"));
        assert_eq!(requests[1].mandate.as_deref(), Some("verdict"));
    }

    #[test]
    fn parse_agents_rejects_malformed_entries() {
        assert!(parse_agents(&HashMap::new())
            .unwrap_err()
            .contains("Missing 'agents'"));
        assert!(parse_agents(&HashMap::from([("agents".to_string(), json!("code_edit"))]))
            .unwrap_err()
            .contains("must be an array"));
        assert!(
            parse_agents(&HashMap::from([("agents".to_string(), json!([{"role": "x"}]))]))
                .unwrap_err()
                .contains("agents[0]")
        );
        assert!(parse_agents(&HashMap::from([(
            "agents".to_string(),
            json!([{"config_name": "code_edit", "target_files": [42]}])
        )]))
        .unwrap_err()
        .contains("agents[0].target_files[0]"));
    }

    #[test]
    fn roster_summary_names_every_member_and_how_to_start_them() {
        let mut registry = registry();
        registry[1].1.writes = false;
        registry[1].1.decision_maker = true;
        let mut requests = vec![request("code_edit"), request("design_review")];
        requests[0].mandate = Some("write the parser".to_string());
        requests[0].target_files = vec!["src/parser.rs".to_string()];

        let members = build_room_members(&card(), &requests, &registry).unwrap();
        let summary = roster_summary("T-1", &members);

        assert!(summary.contains("code_edit — pending: write the parser"), "{summary}");
        assert!(summary.contains("src/parser.rs"), "{summary}");
        assert!(summary.contains("[writes]"), "{summary}");
        assert!(summary.contains("[read-only]"), "{summary}");
        assert!(summary.contains("[decides]"), "{summary}");
        assert!(summary.contains("spawn_agent(card_id=\"T-1\""), "{summary}");
    }

    #[test]
    fn reserved_slot_is_claimed_by_a_later_spawn_of_the_same_role() {
        let mut card = card();
        let members = build_room_members(&card(), &[request("code_edit")], &registry()).unwrap();
        card.team_members = members;

        let claimed = rooms::claim_planned_slot(&mut card, "code_edit").expect("slot is reserved");
        claimed.agent_chat_id = Some("agent-T-1-1234".to_string());
        assert!(claimed.writes(), "the definition's behaviour survives the spawn");

        assert!(
            rooms::claim_planned_slot(&mut card, "code_edit").is_none(),
            "a claimed slot must not be handed out twice"
        );
        assert_eq!(card.team_members.len(), 1);
    }

    #[test]
    fn a_spawn_of_an_unreserved_role_does_not_claim_someone_elses_slot() {
        let mut card = card();
        card.team_members =
            build_room_members(&card(), &[request("code_edit")], &registry()).unwrap();

        assert!(
            rooms::claim_planned_slot(&mut card, "reviewer").is_none(),
            "claiming must match the role exactly"
        );
    }
}
