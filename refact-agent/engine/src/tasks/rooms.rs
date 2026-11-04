use crate::tasks::types::{BoardCard, TeamMember};

pub use refact_tasks::rooms::*;
pub use refact_chat_api::room_provenance::{member_provenance, room_member_provenance};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::types::{TeamMemberReport, TeamStatus};

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
