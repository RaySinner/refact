pub use refact_tools::tools::tool_task_create_room::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use refact_tasks::rooms;
#[cfg(test)]
use refact_tasks::types::BoardCard;
#[cfg(test)]
use refact_yaml_configs::customization_types::SubagentConfig;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_tasks::types::TeamStatus;

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
        let mut entries = vec![
            ("code_edit".to_string(), config("code_edit")),
            ("design_review".to_string(), config("design_review")),
            ("visual_qa".to_string(), config("visual_qa")),
        ];
        entries[1].1.writes = false;
        entries[2].1.writes = false;
        entries
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
        assert!(
            members[0].writes(),
            "code_edit has no writes:false in the test registry"
        );
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
        registry[0].1.writes = true;
        registry[0].1.room_role_hint = Some("  edits the parser  ".to_string());
        registry[0].1.tools = vec!["cat".to_string(), "apply_patch".to_string()];
        registry[1].1.decision_maker = true;
        registry[1].1.writes = false;
        registry[1].1.room_role_hint = Some("decides the room".to_string());

        let members = build_room_members(
            &card(),
            &[request("code_edit"), request("design_review")],
            &registry,
        )
        .unwrap();

        let editor = &members[0];
        assert!(editor.writes(), "writes: true must survive the stamp");
        assert_eq!(editor.room_role_hint.as_deref(), Some("edits the parser"));
        assert_eq!(
            editor.tools,
            vec!["cat".to_string(), "apply_patch".to_string()],
            "the roster must show peers what the definition allows"
        );
        assert_eq!(
            editor.role, "edits the parser",
            "role falls back to the hint"
        );

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
    fn room_rejects_multiple_writers_even_with_disjoint_files() {
        let mut requests = vec![request("code_edit"), request("code_edit")];
        requests[0].role = Some("parser".to_string());
        requests[0].target_files = vec!["src/parser.rs".to_string()];
        requests[1].role = Some("store".to_string());
        requests[1].target_files = vec!["src/store.rs".to_string()];

        let error = build_room_members(&card(), &requests, &registry()).unwrap_err();

        assert!(error.contains("exactly one writer"), "{error}");
        assert!(error.contains("parser"), "{error}");
        assert!(error.contains("store"), "{error}");
    }

    #[test]
    fn room_rejects_advisors_without_a_writer() {
        let error = build_room_members(
            &card(),
            &[request("design_review"), request("visual_qa")],
            &registry(),
        )
        .unwrap_err();

        assert!(error.contains("exactly one writer"), "{error}");
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

        let error = build_room_members(&card, &[request("code_edit")], &registry()).unwrap_err();

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
        assert!(
            parse_agents(&HashMap::from([("agents".to_string(), json!("code_edit"))]))
                .unwrap_err()
                .contains("must be an array")
        );
        assert!(parse_agents(&HashMap::from([(
            "agents".to_string(),
            json!([{"role": "x"}])
        )]))
        .unwrap_err()
        .contains("agents[0]"));
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
        let summary = roster_summary("T-1", &members).unwrap();

        assert!(
            summary.contains("code_edit — pending: write the parser"),
            "{summary}"
        );
        assert!(summary.contains("src/parser.rs"), "{summary}");
        assert!(summary.contains("[writes]"), "{summary}");
        assert!(summary.contains("[read-only]"), "{summary}");
        assert!(summary.contains("[decides]"), "{summary}");
        assert!(summary.contains("spawn_agent(card_id=\"T-1\""), "{summary}");
        assert!(summary.contains("Launch order (writer first)"), "{summary}");
        assert!(summary.find("code_edit").unwrap() < summary.find("design_review").unwrap());
    }

    #[test]
    fn reserved_slot_is_claimed_by_a_later_spawn_of_the_same_role() {
        let mut card = card();
        let members = build_room_members(&card, &[request("code_edit")], &registry()).unwrap();
        card.team_members = members;

        let claimed = rooms::claim_planned_slot(&mut card, "code_edit").expect("slot is reserved");
        claimed.agent_chat_id = Some("agent-T-1-1234".to_string());
        assert!(
            claimed.writes(),
            "the definition's behaviour survives the spawn"
        );

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
            build_room_members(&card, &[request("code_edit")], &registry()).unwrap();

        assert!(
            rooms::claim_planned_slot(&mut card, "reviewer").is_none(),
            "claiming must match the role exactly"
        );
    }
}
