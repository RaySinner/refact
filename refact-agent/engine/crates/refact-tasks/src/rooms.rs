//! Agent rooms: a card may carry several `team_members` instead of one `assignee`.
//!
//! The contract lives in `refact-tasks` (`TeamRole`, `TeamStatus`, `BoardCard::validate_team`).
//! This module owns the engine-side rules for *filling* a room and *collecting* its reports:
//! which cards may take a new member, what that member looks like, and when the room is
//! complete. Keeping these as pure functions is what makes them testable without spawning
//! git worktrees.

use serde_json::Value;
use refact_room::{validate_roster, validate_spawn, RosterMember};

use crate::types::{BoardCard, TeamMember, TeamStatus};

/// Upper bound on members in one room.
///
/// Each member is a full chat session in its own git worktree, so a room is N times the
/// process, disk and LLM spend of a single agent. `MAX_SPAWN_BATCH` (10) is the largest
/// fan-out one batch call can already produce, so 12 keeps a legitimate batch legal while
/// still refusing a runaway "room".
pub const MAX_ROOM_SIZE: usize = 12;

fn neutral_roster(members: &[TeamMember]) -> Vec<RosterMember<'_>> {
    members
        .iter()
        .map(|member| RosterMember {
            role: &member.role,
            writes: member.writes(),
            claimed: member.agent_id.is_some() || member.agent_chat_id.is_some(),
        })
        .collect()
}

pub fn validate_writer_roster(card: &BoardCard) -> Result<(), String> {
    validate_roster(&neutral_roster(&card.team_members)).map_err(|error| error.to_string())
}

pub fn validate_spawn_roster(card: &BoardCard, role: &str) -> Result<(), String> {
    validate_spawn(&neutral_roster(&card.team_members), role).map_err(|error| error.to_string())
}

pub fn writer_first_indices(members: &[TeamMember]) -> Result<Vec<usize>, String> {
    refact_room::spawn_order(&neutral_roster(members)).map_err(|error| error.to_string())
}

/// Columns that can never take a new member: the card is finished.
pub fn is_terminal_column(column: &str) -> bool {
    matches!(column, "done" | "failed" | "regressed")
}

/// Why a card cannot take another member right now.
pub fn reject_spawn_into_card(card: &BoardCard) -> Option<String> {
    if is_terminal_column(&card.column) {
        return Some(format!(
            "Card {} is in terminal column '{}'; reset it before spawning a room member.",
            card.id, card.column
        ));
    }
    if card.team_members.len() >= MAX_ROOM_SIZE {
        return Some(format!(
            "Card {} already has {} room members (limit is {MAX_ROOM_SIZE}).",
            card.id,
            card.team_members.len()
        ));
    }
    None
}

/// Build the member that a spawn is about to add to `card`.
///
/// The member starts as `Pending`, not `Running`. A spawn *claims a slot* in the room; `Running`
/// is what the member reaches once it is actually working. This matters because `validate_team`
/// refuses a running sequential role alongside any other active member — if a spawn marked itself
/// `Running`, an architect could never staff its own room. Starting as `Pending` keeps the
/// reserved roster valid while still letting `validate_team` enforce its structural rules.
///
/// The card's existing members are left untouched: appending is the caller's job inside the same
/// `update_board_atomic` critical section, so a second agent can never observe a half-written room.
pub fn new_room_member(
    role: &str,
    agent_id: &str,
    agent_chat_id: &str,
    worktree_branch: Option<String>,
    worktree_path: Option<String>,
    mandate: Option<String>,
    tools: Vec<String>,
) -> TeamMember {
    TeamMember {
        role: role.to_string(),
        agent_id: Some(agent_id.to_string()),
        agent_chat_id: Some(agent_chat_id.to_string()),
        agent_branch: worktree_branch,
        agent_worktree: worktree_path,
        status: None,
        member_status: Some(TeamStatus::Pending),
        mandate,
        report: None,
        tools,
        // Left unstamped on purpose: a member whose behaviour nobody recorded must behave as a
        // writer, so `spawn_agent` failing to resolve an agent definition can only ever be too
        // permissive, never silently read-only.
        writes: None,
        decision_maker: None,
        room_role_hint: None,
        target_files: Vec::new(),
    }
}

/// A room member reserved from an agent definition, before its chat exists.
///
/// The planner writes the roster first and spawns each member afterwards, so a planned member has
/// no identity yet. It must still be a `Pending` member with its behaviour already recorded:
/// whether it writes and whether it decides are properties of the *definition*, and a roster that
/// waited for the spawn to learn them could no longer reject a conflicting room.
pub struct RoomSlot {
    pub role: String,
    pub mandate: Option<String>,
    pub writes: bool,
    pub decision_maker: bool,
    pub room_role_hint: Option<String>,
    pub tools: Vec<String>,
    pub target_files: Vec<String>,
}

/// The member record for a reserved slot.
pub fn planned_room_member(slot: RoomSlot) -> TeamMember {
    TeamMember {
        role: slot.role,
        agent_chat_id: None,
        agent_branch: None,
        status: None,
        agent_id: None,
        agent_worktree: None,
        member_status: Some(TeamStatus::Pending),
        mandate: slot.mandate,
        report: None,
        tools: slot.tools,
        writes: Some(slot.writes),
        decision_maker: Some(slot.decision_maker),
        room_role_hint: slot.room_role_hint,
        target_files: slot.target_files,
    }
}

/// Refuse a roster that the card cannot take, counting the members about to be added.
pub fn reject_room_growth(card: &BoardCard, adding: usize) -> Option<String> {
    if is_terminal_column(&card.column) {
        return Some(format!(
            "Card {} is in terminal column '{}'; reset it before filling its room.",
            card.id, card.column
        ));
    }
    let total = card.team_members.len().saturating_add(adding);
    if total > MAX_ROOM_SIZE {
        return Some(format!(
            "A room on card {} would hold {total} members (limit is {MAX_ROOM_SIZE}).",
            card.id
        ));
    }
    None
}

/// Fill the reserved slot a spawn was promised, or report that the roster has no free slot for it.
///
/// A roster written by the create-room tool already names who works on the card and what each of
/// them may do. A later spawn must *claim* that slot rather than append a second member with the
/// same role, or the roster would list one agent twice.
pub fn claim_planned_slot<'a>(card: &'a mut BoardCard, role: &str) -> Option<&'a mut TeamMember> {
    let role = role.trim();
    card.team_members.iter_mut().find(|member| {
        member.agent_chat_id.is_none() && member.agent_id.is_none() && member.role.trim() == role
    })
}

/// Reports of every member that produced one, plus whether the room is finished.
///
/// `room_complete` means every member reached a terminal status — it deliberately does *not* mean
/// the room succeeded: a failed member is terminal too, and the architect is the one who reads
/// `failed_members` and decides what to do next.
pub struct RoomReports {
    pub reports: Vec<(String, String)>,
    pub finished: Vec<String>,
    pub failed: Vec<String>,
    pub outstanding: Vec<String>,
}

impl RoomReports {
    pub fn room_complete(&self) -> bool {
        self.outstanding.is_empty()
    }
}

/// Collect per-member reports and readiness. One report slot per member, keyed by `report_key()`
/// (ADK `output_key`) so parallel members cannot overwrite each other.
pub fn collect_room_reports(card: &BoardCard) -> RoomReports {
    let mut reports = Vec::new();
    let mut finished = Vec::new();
    let mut failed = Vec::new();
    let mut outstanding = Vec::new();

    for member in card.team() {
        let role = member.role.clone();
        if let Some(report) = member.report.as_ref() {
            reports.push((member.report_key(), report.summary.clone()));
            if report.partial || !report.success {
                failed.push(role.clone());
            }
        }
        if member.is_terminal() {
            finished.push(role);
        } else {
            outstanding.push(role);
        }
    }

    RoomReports {
        reports,
        finished,
        failed,
        outstanding,
    }
}

/// Parse a room roster from tool/HTTP JSON.
///
/// Accepts either a list of objects or a comma-separated `role:agent_chat_id` string, so a model
/// can spell the room either way. Status and mandate are optional; identity is not — a member
/// nobody can address is not a room member.
pub fn parse_team_members(value: Option<&Value>) -> Result<Vec<TeamMember>, String> {
    let Some(value) = value else {
        return Ok(vec![]);
    };

    match value {
        Value::Array(items) => {
            let mut members = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                members.push(parse_team_member(item).ok_or_else(|| {
                    format!(
                        "team_members[{index}] must be an object with a role and an \
                         agent_chat_id, or a \"role:agent_chat_id\" string"
                    )
                })?);
            }
            Ok(members)
        }
        Value::String(spec) => parse_team_members_from_string(spec),
        _ => Err("team_members must be an array or a comma-separated string".to_string()),
    }
}

fn parse_team_member(value: &Value) -> Option<TeamMember> {
    let (role, chat_id) = match value {
        Value::Object(map) => {
            let role = map.get("role").and_then(Value::as_str)?.trim().to_string();
            let chat_id = map
                .get("agent_chat_id")
                .or_else(|| map.get("chat_id"))
                .and_then(Value::as_str)?
                .trim()
                .to_string();
            (role, chat_id)
        }
        Value::String(text) => split_role_and_chat(text)?,
        _ => return None,
    };
    if role.is_empty() || chat_id.is_empty() {
        return None;
    }

    let object = value.as_object();
    let field = |key: &str| {
        object
            .and_then(|map| map.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let member_status = field("member_status")
        .or_else(|| field("status"))
        .and_then(|raw| TeamStatus::parse(&raw));

    let flag = |key: &str| object.and_then(|map| map.get(key)).and_then(Value::as_bool);
    let writes = flag("writes");
    let decision_maker = flag("decision_maker");
    let room_role_hint = field("room_role_hint");

    let tools = value
        .as_object()
        .and_then(|map| map.get("tools"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    Some(TeamMember {
        role,
        agent_chat_id: Some(chat_id),
        agent_branch: field("agent_branch"),
        status: field("status"),
        agent_id: field("agent_id"),
        agent_worktree: field("agent_worktree"),
        member_status,
        mandate: field("mandate"),
        report: None,
        tools,
        writes,
        decision_maker,
        room_role_hint,
        target_files: value
            .as_object()
            .and_then(|map| map.get("target_files"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|file| !file.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn split_role_and_chat(text: &str) -> Option<(String, String)> {
    let (role, chat_id) = text.split_once(':')?;
    let role = role.trim();
    let chat_id = chat_id.trim();
    if role.is_empty() || chat_id.is_empty() {
        return None;
    }
    Some((role.to_string(), chat_id.to_string()))
}

fn parse_team_members_from_string(spec: &str) -> Result<Vec<TeamMember>, String> {
    let mut members = Vec::new();
    for (index, entry) in spec.split(',').enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (role, chat_id) = split_role_and_chat(entry).ok_or_else(|| {
            format!("team_members[{index}] '{entry}' must be in \"role:agent_chat_id\" form")
        })?;
        members.push(TeamMember {
            role,
            agent_chat_id: Some(chat_id),
            agent_branch: None,
            status: None,
            agent_id: None,
            agent_worktree: None,
            member_status: None,
            mandate: None,
            report: None,
            tools: Vec::new(),
            writes: None,
            decision_maker: None,
            room_role_hint: None,
            target_files: Vec::new(),
        });
    }
    Ok(members)
}

/// The one rule that turns a room member into a name.
///
/// Role first, because the role is the part a reader wants; the ordinal appears only when the role
/// is not unique in the room, which is exactly the case where the role alone would be ambiguous
/// (`coder` twice in one room would draw two identical bylines and read as one author).
pub fn member_display_name(role: &str, position_in_role: usize, same_role_count: usize) -> String {
    let role = role.trim();
    let role = if role.is_empty() { "agent" } else { role };
    if same_role_count < 2 {
        return role.to_string();
    }
    format!("{role} #{}", position_in_role.max(1))
}

/// Stamp the definition's behaviour onto a member that has not been spawned yet.
pub fn apply_agent_behavior(
    member: &mut TeamMember,
    writes: bool,
    decision_maker: bool,
    room_role_hint: Option<String>,
) {
    member.writes = Some(writes);
    member.decision_maker = Some(decision_maker);
    if let Some(hint) = room_role_hint {
        member.room_role_hint = Some(hint);
    }
}

/// True when two chats belong to the same room, i.e. the same card.
pub fn same_room(card: &BoardCard, left_chat_id: &str, right_chat_id: &str) -> bool {
    card.team_member_by_chat_id(left_chat_id).is_some()
        && card.team_member_by_chat_id(right_chat_id).is_some()
}

/// Members of the same room, as one line each, for a prompt or tool output.
pub fn describe_room(card: &BoardCard) -> Vec<String> {
    let mut lines = Vec::with_capacity(card.team_members.len());
    for member in card.team() {
        let identity = member
            .agent_chat_id
            .as_deref()
            .or(member.agent_id.as_deref())
            .unwrap_or("<unassigned>");
        lines.push(format!(
            "{} [{}] {}",
            member.role,
            member.typed_status().as_str(),
            identity
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(role: &str, writes: bool) -> TeamMember {
        planned_room_member(RoomSlot {
            role: role.to_string(),
            mandate: None,
            writes,
            decision_maker: false,
            room_role_hint: None,
            tools: vec![],
            target_files: vec![],
        })
    }

    #[test]
    fn board_mapping_delegates_writer_order_and_claim_state() {
        let mut card = BoardCard {
            id: "T-1".to_string(),
            title: "Room".to_string(),
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
            team_members: vec![slot("reviewer", false), slot("coder", true)],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        };
        assert_eq!(
            writer_first_indices(&card.team_members).unwrap(),
            vec![1, 0]
        );
        assert!(validate_spawn_roster(&card, "reviewer").is_err());
        assert!(validate_spawn_roster(&card, "coder").is_ok());

        card.team_members[1].agent_id = Some("writer-1".to_string());
        assert!(validate_spawn_roster(&card, "reviewer").is_ok());
        assert!(validate_writer_roster(&card).is_ok());
    }
}
