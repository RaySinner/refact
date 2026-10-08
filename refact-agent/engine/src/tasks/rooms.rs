//! Agent rooms: a card may carry several `team_members` instead of one `assignee`.
//!
//! The contract lives in `refact-tasks` (`TeamRole`, `TeamStatus`, `BoardCard::validate_team`).
//! This module owns the engine-side rules for *filling* a room and *collecting* its reports:
//! which cards may take a new member, what that member looks like, and when the room is
//! complete. Keeping these as pure functions is what makes them testable without spawning
//! git worktrees.

use serde_json::Value;

use refact_chat_api::MessageProvenance;

use crate::tasks::types::{BoardCard, TeamMember, TeamStatus};

/// Upper bound on members in one room.
///
/// Each member is a full chat session in its own git worktree, so a room is N times the
/// process, disk and LLM spend of a single agent. `MAX_SPAWN_BATCH` (10) is the largest
/// fan-out one batch call can already produce, so 12 keeps a legitimate batch legal while
/// still refusing a runaway "room".
pub const MAX_ROOM_SIZE: usize = 12;

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
/// architect-first flow legal while still letting `validate_team` enforce the structural rules (a
/// coder must exist, identities must be unique).
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
        member.agent_chat_id.is_none()
            && member.agent_id.is_none()
            && member.role.trim() == role
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

/// True when two chats belong to the same room, i.e. the same card.
pub fn same_room(card: &BoardCard, left_chat_id: &str, right_chat_id: &str) -> bool {
    card.team_member_by_chat_id(left_chat_id).is_some()
        && card.team_member_by_chat_id(right_chat_id).is_some()
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

/// Provenance for `member` of `card` — who this is, in which room, wearing which accent.
///
/// The single place that reads a `TeamMember` to answer "who was that". Callers pass the card they
/// already loaded rather than re-reading the board: a delivery stamps provenance while holding
/// the roster it resolved the peer from, so a second read would only add latency and a window in
/// which the room changed mid-message.
pub fn member_provenance(card: &BoardCard, member: &TeamMember) -> MessageProvenance {
    let same_role_count = card
        .team()
        .iter()
        .filter(|peer| peer.role.trim() == member.role.trim())
        .count();
    let position_in_role = card
        .team()
        .iter()
        .filter(|peer| peer.role.trim() == member.role.trim())
        .position(|peer| {
            peer.agent_chat_id.as_deref() == member.agent_chat_id.as_deref()
                && peer.agent_id.as_deref() == member.agent_id.as_deref()
        })
        .map(|index| index + 1)
        .unwrap_or(1);

    MessageProvenance {
        chat_id: member
            .agent_chat_id
            .clone()
            .or_else(|| member.agent_id.clone())
            .unwrap_or_default(),
        role: member.role.trim().to_string(),
        display_name: member_display_name(&member.role, position_in_role, same_role_count),
        card_id: Some(card.id.clone()),
    }
}

/// Provenance for whoever is speaking from `caller_chat_id` in `card`, if that is a room member.
pub fn room_member_provenance(card: &BoardCard, caller_chat_id: &str) -> Option<MessageProvenance> {
    let provenance = member_provenance(card, card.team_member_by_chat_id(caller_chat_id)?);
    (!provenance.chat_id.is_empty()).then_some(provenance)
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
    use crate::tasks::types::TeamMemberReport;

    fn card(id: &str, column: &str) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: format!("Card {id}"),
            column: column.to_string(),
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

    fn member(role: &str, chat: &str, status: TeamStatus) -> TeamMember {
        let mut member = new_room_member(role, "agent-id", chat, None, None, None, vec![]);
        member.member_status = Some(status);
        member
    }

    #[test]
    fn new_room_member_claims_a_pending_slot_not_running() {
        let member = new_room_member(
            "coder",
            "agent-1",
            "agent-T-1-1111",
            Some("branch".into()),
            Some("/tmp/wt".into()),
            Some("write the parser".into()),
            vec!["cat".into(), "shell".into()],
        );

        assert_eq!(member.role, "coder");
        assert_eq!(member.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(member.agent_chat_id.as_deref(), Some("agent-T-1-1111"));
        assert_eq!(member.typed_status(), TeamStatus::Pending);
        assert!(!member.is_active(), "a claimed slot is not yet working");
        assert!(!member.is_terminal());
        assert_eq!(member.mandate.as_deref(), Some("write the parser"));
        assert_eq!(member.report_key(), "team/agent-T-1-1111");
        assert_eq!(member.tools, vec!["cat".to_string(), "shell".to_string()]);
    }

    #[test]
    fn spawn_into_terminal_column_is_rejected() {
        for column in ["done", "failed", "regressed"] {
            let card = card("T-1", column);
            let error = reject_spawn_into_card(&card).unwrap();
            assert!(error.contains("terminal column"), "{error}");
            assert!(error.contains(column), "{error}");
        }
        assert!(reject_spawn_into_card(&card("T-1", "planned")).is_none());
        assert!(reject_spawn_into_card(&card("T-1", "doing")).is_none());
    }

    #[test]
    fn room_size_limit_is_enforced() {
        let mut card = card("T-1", "doing");
        for index in 0..MAX_ROOM_SIZE {
            card.team_members
                .push(member("coder", &format!("agent-T-1-{index}"), TeamStatus::Done));
        }
        let error = reject_spawn_into_card(&card).unwrap();

        assert!(error.contains(&MAX_ROOM_SIZE.to_string()), "{error}");
        assert!(error.contains("already has"), "{error}");
    }

    #[test]
    fn one_below_the_room_limit_still_accepts() {
        let mut card = card("T-1", "doing");
        for index in 0..MAX_ROOM_SIZE - 1 {
            card.team_members
                .push(member("coder", &format!("agent-T-1-{index}"), TeamStatus::Done));
        }
        assert!(reject_spawn_into_card(&card).is_none());
    }

    #[test]
    fn room_reports_are_collectable_per_member() {
        let mut card = card("T-1", "doing");
        let mut architect = member("architect", "agent-T-1-arch", TeamStatus::Done);
        architect.report = Some(TeamMemberReport {
            summary: "split the work".into(),
            success: true,
            completed_at: "2026-01-01T01:00:00Z".into(),
            ..Default::default()
        });
        let mut coder = member("coder", "agent-T-1-code", TeamStatus::Done);
        coder.report = Some(TeamMemberReport {
            summary: "wrote the parser".into(),
            success: true,
            completed_at: "2026-01-01T02:00:00Z".into(),
            ..Default::default()
        });
        card.team_members = vec![architect, coder];

        let collected = collect_room_reports(&card);

        assert_eq!(collected.reports.len(), 2);
        assert!(collected.reports.iter().all(|(key, _)| key.starts_with("team/")));
        assert!(collected.reports.iter().any(|(_, s)| s == "wrote the parser"));
        assert!(collected.failed.is_empty());
        assert!(collected.room_complete());
    }

    #[test]
    fn partial_and_failed_members_are_reported_separately() {
        let mut card = card("T-1", "doing");
        let mut coder = member("coder", "agent-T-1-code", TeamStatus::Partial);
        coder.report = Some(TeamMemberReport {
            summary: "half done".into(),
            partial: true,
            completed_at: "2026-01-01T02:00:00Z".into(),
            ..Default::default()
        });
        let mut reviewer = member("reviewer", "agent-T-1-rev", TeamStatus::Failed);
        reviewer.report = Some(TeamMemberReport {
            summary: "could not run".into(),
            completed_at: "2026-01-01T03:00:00Z".into(),
            ..Default::default()
        });
        card.team_members = vec![coder, reviewer];

        let collected = collect_room_reports(&card);

        assert_eq!(collected.failed.len(), 2);
        assert!(collected.room_complete(), "a failed member is terminal too");
    }

    #[test]
    fn room_is_not_complete_while_a_member_is_outstanding() {
        let mut card = card("T-1", "doing");
        card.team_members = vec![
            member("architect", "agent-T-1-arch", TeamStatus::Done),
            member("coder", "agent-T-1-code", TeamStatus::Running),
        ];

        let collected = collect_room_reports(&card);

        assert!(!collected.room_complete());
        assert_eq!(collected.outstanding, vec!["coder".to_string()]);
        assert!(
            collected.reports.is_empty(),
            "no report yet means no slot filled"
        );
    }

    #[test]
    fn parse_team_members_accepts_object_array() {
        let value = serde_json::json!([
            {"role": "architect", "agent_chat_id": "agent-T-1-arch", "mandate": "design"},
            {"role": "coder", "chat_id": "agent-T-1-code", "member_status": "running"}
        ]);

        let members = parse_team_members(Some(&value)).unwrap();

        assert_eq!(members.len(), 2);
        assert_eq!(members[0].role, "architect");
        assert_eq!(members[0].mandate.as_deref(), Some("design"));
        assert_eq!(members[1].agent_chat_id.as_deref(), Some("agent-T-1-code"));
        assert_eq!(members[1].typed_status(), TeamStatus::Running);
    }

    #[test]
    fn parse_team_members_accepts_role_colon_chat_string() {
        let value = serde_json::json!("architect:agent-T-1-arch, coder:agent-T-1-code");

        let members = parse_team_members(Some(&value)).unwrap();

        assert_eq!(members.len(), 2);
        assert_eq!(members[1].role, "coder");
        assert_eq!(members[1].agent_chat_id.as_deref(), Some("agent-T-1-code"));
        assert_eq!(members[1].typed_status(), TeamStatus::Pending);
    }

    #[test]
    fn parse_team_members_reads_room_behaviour() {
        let value = serde_json::json!([
            {
                "role": "tech-lead",
                "chat_id": "agent-T-1-lead",
                "writes": false,
                "decision_maker": true,
                "room_role_hint": "arbitrates the room",
                "target_files": ["notes/decisions.md"]
            },
            {"role": "implementer", "chat_id": "agent-T-1-code"}
        ]);

        let members = parse_team_members(Some(&value)).unwrap();

        let lead = &members[0];
        assert!(!lead.writes());
        assert!(lead.is_decision_maker());
        assert!(lead.allows_parallel(), "a decision maker works alongside its peers");
        assert_eq!(lead.room_role_hint.as_deref(), Some("arbitrates the room"));
        assert_eq!(lead.target_files, vec!["notes/decisions.md".to_string()]);

        // An entry that says nothing keeps the historical defaults: writes, decides nothing.
        let implementer = &members[1];
        assert!(implementer.writes());
        assert!(!implementer.is_decision_maker());
        assert!(implementer.room_role_hint.is_none());
        assert!(implementer.target_files.is_empty());
    }

    #[test]
    fn parse_team_members_ignores_a_non_boolean_writes_flag() {
        // A model that passes "false" as a string must not silently make an agent read-only.
        let value = serde_json::json!([
            {"role": "coder", "chat_id": "agent-T-1-code", "writes": "false"}
        ]);

        let members = parse_team_members(Some(&value)).unwrap();

        assert!(members[0].writes());
    }

    #[test]
    fn parse_team_members_rejects_entries_without_identity() {
        let value = serde_json::json!([{"role": "coder"}]);
        let error = parse_team_members(Some(&value)).unwrap_err();
        assert!(error.contains("team_members[0]"), "{error}");

        let value = serde_json::json!("architect");
        let error = parse_team_members(Some(&value)).unwrap_err();
        assert!(error.contains("role:agent_chat_id"), "{error}");

        assert!(parse_team_members(Some(&serde_json::json!(42))).is_err());
        assert!(parse_team_members(None).unwrap().is_empty());
    }

    #[test]
    fn same_room_only_holds_within_one_card() {
        let mut card = card("T-1", "doing");
        card.team_members = vec![
            member("architect", "agent-T-1-arch", TeamStatus::Running),
            member("coder", "agent-T-1-code", TeamStatus::Running),
        ];

        assert!(same_room(&card, "agent-T-1-arch", "agent-T-1-code"));
        assert!(!same_room(&card, "agent-T-1-arch", "agent-T-9-other"));
        assert!(!same_room(&card, "agent-T-1-arch", "planner-task-1-1"));
    }

    #[test]
    fn describe_room_names_every_member_with_status() {
        let mut card = card("T-1", "doing");
        card.team_members = vec![
            member("architect", "agent-T-1-arch", TeamStatus::Running),
            member("coder", "agent-T-1-code", TeamStatus::Pending),
        ];

        let lines = describe_room(&card);

        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("architect"));
        assert!(lines[0].contains("running"));
        assert!(lines[0].contains("agent-T-1-arch"));
        assert!(lines[1].contains("pending"));
    }

    #[test]
    fn display_name_is_deterministic_for_same_member() {
        let mut card = card("T-1", "doing");
        card.team_members = vec![
            member("architect", "agent-T-1-arch", TeamStatus::Running),
            member("coder", "agent-T-1-code", TeamStatus::Running),
        ];

        let first = member_provenance(&card, &card.team_members[1]).display_name;
        let second = member_provenance(&card, &card.team_members[1]).display_name;

        assert_eq!(first, second);
        assert_eq!(first, "coder");
    }

    #[test]
    fn duplicate_roles_get_distinct_bylines() {
        let mut card = card("T-1", "doing");
        card.team_members = vec![
            member("coder", "agent-T-1-a", TeamStatus::Running),
            member("coder", "agent-T-1-b", TeamStatus::Running),
        ];

        let names = card
            .team()
            .iter()
            .map(|peer| member_provenance(&card, peer).display_name)
            .collect::<Vec<_>>();

        assert_ne!(
            names[0], names[1],
            "two same-role members must not share a byline"
        );
        assert!(names.iter().all(|name| name.starts_with("coder")));
    }

    #[test]
    fn messages_from_different_rooms_have_different_provenance() {
        let mut first = card("T-1", "doing");
        first.team_members = vec![
            member("architect", "agent-T-1-arch", TeamStatus::Running),
            member("coder", "agent-T-1-code", TeamStatus::Running),
        ];
        let mut second = card("T-2", "doing");
        second.team_members = vec![
            member("architect", "agent-T-2-arch", TeamStatus::Running),
            member("coder", "agent-T-2-code", TeamStatus::Running),
        ];

        let from_first = room_member_provenance(&first, "agent-T-1-code").unwrap();
        let from_second = room_member_provenance(&second, "agent-T-2-code").unwrap();

        assert_ne!(from_first.chat_id, from_second.chat_id);
        assert_ne!(from_first.card_id, from_second.card_id);
        assert_eq!(
            from_first.display_name, from_second.display_name,
            "same role in a different room is still the same byline"
        );
    }

    #[test]
    fn a_non_member_has_no_provenance() {
        let mut card = card("T-1", "doing");
        card.team_members = vec![member("coder", "agent-T-1-code", TeamStatus::Running)];

        assert!(room_member_provenance(&card, "agent-T-9-stranger").is_none());
        assert!(room_member_provenance(&card, "planner-task-1-1").is_none());
    }
}
