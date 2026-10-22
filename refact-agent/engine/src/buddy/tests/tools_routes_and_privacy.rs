use super::*;

// =============================================================================
// T-8: HTTP routes unit tests
// =============================================================================
#[tokio::test]
async fn accept_action_dispatch() {
    let (tx, mut rx) = broadcast::channel(32);
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    let mut opp = make_opportunity("opp-nav", "ck-nav");
    opp.proposed_actions = vec![BuddyAction::OpenPage {
        page: BuddyPage::Buddy,
    }];
    svc.add_opportunity(opp);
    let _ = rx.try_recv();
    svc.send_navigation(BuddyPage::Buddy);
    let event = rx.try_recv().expect("must receive NavigationRequest event");
    assert!(
        matches!(event, super::events::BuddyEvent::NavigationRequest { .. }),
        "must receive NavigationRequest after OpenPage action dispatch"
    );
}

#[test]
fn dismiss_marks_history() {
    use super::opportunities::OpportunityQueue;
    let mut q = OpportunityQueue::new();
    push_opportunity(&mut q, make_opportunity("opp-dm2", "key-dm2"));
    q.dismiss("opp-dm2");
    assert!(
        q.recently_dismissed("key-dm2", Duration::hours(24)),
        "recently_dismissed must be true after dismiss"
    );
}

#[test]
fn draft_create_get_consume_roundtrip() {
    use super::drafts::DraftStore;
    let mut store = DraftStore::new();
    let draft = store.create(
        DraftKind::Skill,
        "Test Skill".to_string(),
        "name: test".to_string(),
        "A test skill".to_string(),
    );
    let id = draft.id.clone();
    assert!(store.get(&id).is_some(), "draft must exist after create");
    let consumed = store.consume(&id);
    assert!(consumed.is_some(), "draft must be consumable");
    assert_eq!(consumed.unwrap().title, "Test Skill");
    assert!(store.get(&id).is_none(), "draft must be gone after consume");
}

#[test]
fn frontend_error_rate_limit() {
    use crate::http::routers::v1::buddy_frontend_error::FrontendErrorRateLimiter;
    let rl = FrontendErrorRateLimiter::new();
    for i in 0..60 {
        assert!(rl.check_and_record("test_ip"), "call {} should succeed", i);
    }
    assert!(
        !rl.check_and_record("test_ip"),
        "61st call must be blocked by rate limit"
    );
}

#[tokio::test]
async fn pulse_returns_current_state() {
    let (tx, _rx) = broadcast::channel(32);
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    let new_pulse = BuddyPulse {
        generated_at: Some(chrono::Utc::now()),
        tasks: super::types::TaskPulse {
            total: 7,
            ..Default::default()
        },
        ..BuddyPulse::default()
    };
    svc.set_pulse(new_pulse);
    assert_eq!(svc.pulse.tasks.total, 7, "pulse tasks.total must be 7");
    assert!(
        svc.pulse.generated_at.is_some(),
        "pulse generated_at must be set"
    );
    let json = serde_json::to_value(&svc.pulse).expect("must serialize");
    assert!(
        json.get("tasks").is_some(),
        "pulse JSON must have tasks sub-pulse"
    );
    assert!(
        json.get("generated_at").is_some(),
        "pulse JSON must have generated_at"
    );
}

// =============================================================================
// T-10: Tool tests — open_view, create_draft, launch_investigation, buddy_yaml
// =============================================================================
#[test]
fn tool_buddy_open_view_each_page() {
    use super::events::BuddyEvent;

    let pages: Vec<BuddyPage> = vec![
        BuddyPage::Buddy,
        BuddyPage::Stats,
        BuddyPage::Customization,
        BuddyPage::Providers,
        BuddyPage::DefaultModels,
        BuddyPage::Integrations,
        BuddyPage::Extensions,
        BuddyPage::MarketplaceHub,
        BuddyPage::Marketplace,
        BuddyPage::SkillsMarketplace,
        BuddyPage::CommandsMarketplace,
        BuddyPage::DelegatesMarketplace,
        BuddyPage::TasksList,
        BuddyPage::TaskWorkspace {
            task_id: "task-xyz".to_string(),
        },
        BuddyPage::KnowledgeGraph,
        BuddyPage::Worktrees,
        BuddyPage::SetupMode {
            mode: "setup_mcp".to_string(),
        },
    ];

    let svc = make_service();
    let mut rx = svc.events_tx.subscribe();

    for page in &pages {
        svc.send_navigation(page.clone());
        let event = rx.try_recv().expect("must receive NavigationRequest");
        match event {
            BuddyEvent::NavigationRequest { page: emitted } => {
                assert_eq!(&emitted, page, "emitted page must match sent page");
            }
            other => panic!("expected NavigationRequest, got {:?}", other),
        }
    }
}

#[test]
fn tool_buddy_create_draft_persists() {
    let mut svc = make_service();
    let mut rx = svc.events_tx.subscribe();

    let draft = svc
        .create_draft(
            DraftKind::Skill,
            "My Skill".to_string(),
            "yaml: {}".to_string(),
            "A test skill draft".to_string(),
        )
        .expect("draft must be created");

    let draft_id = draft.id.clone();
    assert!(
        svc.draft_store.get(&draft_id).is_some(),
        "draft must be stored"
    );
    assert_eq!(
        svc.draft_store.get(&draft_id).unwrap().kind,
        DraftKind::Skill
    );

    let event = rx.try_recv().expect("must receive DraftCreated event");
    assert!(
        matches!(event, super::events::BuddyEvent::DraftCreated { .. }),
        "event must be DraftCreated"
    );
    assert!(
        rx.try_recv().is_err(),
        "draft create must emit exactly once"
    );

    let _ = svc.consume_draft(&draft_id);
    assert!(
        svc.draft_store.get(&draft_id).is_none(),
        "consumed draft must be removed"
    );
}

#[test]
fn tool_buddy_create_draft_schema_accepts_pulse_report() {
    use crate::tools::tool_buddy_create_draft::ToolBuddyCreateDraft;
    use crate::tools::tools_description::Tool;
    let tool = ToolBuddyCreateDraft {
        config_path: String::new(),
    };
    let desc = tool.tool_description();
    let kinds = desc.input_schema["properties"]["kind"]["enum"]
        .as_array()
        .expect("kind enum must be an array");
    assert!(
        kinds.iter().any(|v| v.as_str() == Some("pulse_report")),
        "buddy_create_draft must accept pulse_report"
    );
}

#[tokio::test]
async fn tool_buddy_create_draft_rejects_oversized_content() {
    use crate::at_commands::at_commands::AtCommandsContext;
    use crate::tools::tool_buddy_create_draft::ToolBuddyCreateDraft;
    use crate::tools::tools_description::Tool;
    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let ccx = Arc::new(tokio::sync::Mutex::new(
        AtCommandsContext::new_from_app(
            app.clone(),
            4000,
            20,
            false,
            vec![],
            String::new(),
            None,
            String::new(),
            None,
            None,
        )
        .await,
    ));
    let mut tool = ToolBuddyCreateDraft {
        config_path: String::new(),
    };
    let mut args = std::collections::HashMap::new();
    args.insert("kind".to_string(), serde_json::json!("skill"));
    args.insert("title".to_string(), serde_json::json!("Skill"));
    let oversized_content = "x".repeat(super::drafts::DRAFT_CONTENT_MAX_BYTES + 1);
    args.insert(
        "yaml_or_json".to_string(),
        serde_json::json!(oversized_content),
    );
    args.insert("explanation".to_string(), serde_json::json!(""));
    let err = tool
        .tool_execute(ccx, &"tool-call".to_string(), &args)
        .await
        .unwrap_err();
    assert!(err.contains("draft content too large"));
}

#[tokio::test]
async fn draft_create_endpoint_emits_exactly_one_created() {
    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let mut rx = {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        lock.as_ref().unwrap().events_tx.subscribe()
    };
    let response = crate::http::routers::v1::buddy_drafts::handle_v1_buddy_draft_create_skill(
        axum::extract::State(app.clone()),
        axum::Json(crate::http::routers::v1::buddy_drafts::DraftCreateRequest {
            title: "Skill".to_string(),
            yaml_or_json: "---\nname: skill\n---\nBody".to_string(),
            explanation: "explain".to_string(),
        }),
    )
    .await
    .expect("draft create endpoint must succeed");
    let draft_id = response.0.id;
    let event = rx.try_recv().expect("must receive DraftCreated event");
    match event {
        super::events::BuddyEvent::DraftCreated { draft } => assert_eq!(draft.id, draft_id),
        other => panic!("expected DraftCreated, got {:?}", other),
    }
    assert!(rx.try_recv().is_err(), "endpoint must emit exactly once");
    assert!(draft_exists(&app, &draft_id).await);
}

#[tokio::test]
async fn draft_create_endpoint_rejects_oversized_title() {
    use hyper::StatusCode;
    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let err = crate::http::routers::v1::buddy_drafts::handle_v1_buddy_draft_create_skill(
        axum::extract::State(app.clone()),
        axum::Json(crate::http::routers::v1::buddy_drafts::DraftCreateRequest {
            title: "x".repeat(super::drafts::DRAFT_TITLE_MAX_CHARS + 1),
            yaml_or_json: "{}".to_string(),
            explanation: String::new(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_code, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(err.message.contains("draft title too large"));
}

#[tokio::test]
async fn draft_delete_emits_removed_event() {
    use axum::extract::Path;
    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let draft_id = add_draft_to_gcx(
        &app,
        DraftKind::Skill,
        "Skill",
        "---\nname: skill\n---\nBody",
    )
    .await;
    let mut rx = {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        lock.as_ref().unwrap().events_tx.subscribe()
    };
    let _ = crate::http::routers::v1::buddy_drafts::handle_v1_buddy_draft_delete(
        axum::extract::State(app.clone()),
        Path(draft_id.clone()),
    )
    .await
    .expect("draft delete must succeed");
    let event = rx.try_recv().expect("must receive DraftRemoved event");
    match event {
        super::events::BuddyEvent::DraftRemoved { draft_id: removed } => {
            assert_eq!(removed, draft_id)
        }
        other => panic!("expected DraftRemoved, got {:?}", other),
    }
    assert!(!draft_exists(&app, &draft_id).await);
}

#[test]
fn draft_expiry_emits_removed_event() {
    let mut svc = make_service();
    let mut rx = svc.events_tx.subscribe();
    let draft = svc
        .create_draft(
            DraftKind::PulseReport,
            "Report".to_string(),
            "# Report".to_string(),
            String::new(),
        )
        .expect("draft must be created");
    let _ = rx.try_recv();
    let draft_id = draft.id.clone();
    let expired = svc.expire_drafts(chrono::Utc::now() + Duration::hours(3));
    assert_eq!(expired, vec![draft_id.clone()]);
    let event = rx.try_recv().expect("must receive DraftRemoved event");
    match event {
        super::events::BuddyEvent::DraftRemoved { draft_id: removed } => {
            assert_eq!(removed, draft_id)
        }
        other => panic!("expected DraftRemoved, got {:?}", other),
    }
}

#[tokio::test]
async fn accept_agents_md_action_returns_content_draft_id() {
    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let content = "# AGENTS.md\n\nUse cargo test.";
    let outcome = crate::http::routers::v1::buddy_opportunities::dispatch_action(
        app.clone(),
        "opp-agents-md",
        &BuddyAction::DraftAgentsMdPatch {
            content: content.to_string(),
        },
    )
    .await
    .expect("agents md draft action must succeed");
    assert_eq!(outcome.result["draft_kind"], "agents_md");
    let draft_id = outcome.result["draft_id"].as_str().unwrap();
    let draft = draft_by_id(&app, draft_id).await;
    assert_eq!(draft.kind, DraftKind::AgentsMd);
    assert_eq!(draft.yaml_or_json, content);
}

#[tokio::test]
async fn accept_pulse_report_action_returns_report_draft_id() {
    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let outcome = crate::http::routers::v1::buddy_opportunities::dispatch_action(
        app.clone(),
        "opp-pulse-report",
        &BuddyAction::CreatePulseReport {
            scope: PulseScope::All,
        },
    )
    .await
    .expect("pulse report draft action must succeed");
    assert_eq!(outcome.result["draft_kind"], "pulse_report");
    let draft_id = outcome.result["draft_id"].as_str().unwrap();
    let draft = draft_by_id(&app, draft_id).await;
    assert_eq!(draft.kind, DraftKind::PulseReport);
    assert!(draft.yaml_or_json.contains("# Buddy Pulse Report"));
}

#[tokio::test]
async fn tool_buddy_launch_investigation_creates_chat() {
    use crate::chat::trajectories::save_trajectory_snapshot;
    use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;
    use crate::buddy::types::BuddyThreadMeta;
    use crate::call_validation::{ChatContent, ChatMessage};

    let dir = tempfile::tempdir().unwrap();
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    {
        *app.workspace
            .documents_state
            .workspace_folders
            .lock()
            .unwrap() = vec![dir.path().to_path_buf()];
    }

    let chat_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();

    let snapshot = TrajectorySnapshot {
        goal: None,
        goal_ledger: Vec::new(),
        goal_verification_blocked_until_ms: None,
        chat_id: chat_id.clone(),
        title: "Investigation".to_string(),
        model: String::new(),
        mode: "buddy".to_string(),
        tool_use: "agent".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("investigate this issue".to_string()),
            ..Default::default()
        }],
        created_at,
        boost_reasoning: false,
        checkpoints_enabled: false,
        context_tokens_cap: None,
        auto_compression_cap: None,
        auto_compression_cap_pending: true,
        include_project_info: true,
        is_title_generated: false,
        auto_approve_editing_tools: false,
        auto_approve_dangerous_commands: false,
        autonomous_no_confirm: false,
        version: 1,
        task_meta: None,
        worktree: None,
        parent_id: None,
        link_type: None,
        root_chat_id: None,
        reasoning_effort: None,
        thinking_budget: None,
        temperature: None,
        frequency_penalty: None,
        max_tokens: None,
        parallel_tool_calls: None,
        previous_response_id: None,
        active_skill: None,
        auto_enrichment_enabled: Some(true),
        buddy_meta: Some(BuddyThreadMeta {
            is_buddy_chat: true,
            buddy_chat_kind: "investigation".to_string(),
            workflow_id: None,
        }),
        auto_compact_enabled: None,
        frozen_request_prefix: None,
        claude_code_identity: None,
        reactive_compact_attempts: None,
        wake_up_at: None,
        waiting_for_card_ids: Vec::new(),
        pending_deliveries: Vec::new(),
    };

    let result = save_trajectory_snapshot(gcx, snapshot).await;
    assert!(
        result.is_ok(),
        "investigation chat must be created: {:?}",
        result.err()
    );
    let chat_file = dir
        .path()
        .join(".refact")
        .join("buddy")
        .join("chats")
        .join("conversations")
        .join(format!("{}.json", chat_id));
    assert!(
        chat_file.exists(),
        "investigation chat file must be written to disk"
    );
}

#[test]
fn buddy_yaml_parses() {
    let yaml_src =
        include_str!("../../../crates/refact-yaml-configs/src/defaults/modes/buddy.yaml");
    let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_src).expect("buddy.yaml must parse");
    let tools = parsed
        .get("tools")
        .and_then(|t| t.as_sequence())
        .expect("buddy.yaml must have tools list");
    let tool_names: Vec<&str> = tools.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        tool_names.contains(&"buddy_open_view"),
        "buddy.yaml must list buddy_open_view"
    );
    assert!(
        tool_names.contains(&"buddy_create_draft"),
        "buddy.yaml must list buddy_create_draft"
    );
    assert!(
        tool_names.contains(&"buddy_launch_investigation"),
        "buddy.yaml must list buddy_launch_investigation"
    );
}

// =============================================================================
// T-10/T-14: Editor draft_id integration tests
// =============================================================================
#[test]
fn customization_get_with_draft_id() {
    use super::drafts::DraftStore;
    let mut store = DraftStore::new();
    let draft = store.create(
        DraftKind::Mode,
        "My Mode".to_string(),
        r#"{"id": "my-mode", "schema_version": 1, "title": "My Mode"}"#.to_string(),
        "A test mode draft".to_string(),
    );
    let id = draft.id.clone();
    let found = store.get(&id).unwrap();
    assert_eq!(found.kind, DraftKind::Mode, "draft kind must be Mode");
    let data: serde_json::Value =
        serde_yaml::from_str(&found.yaml_or_json).expect("yaml_or_json must be parseable");
    assert_eq!(data["id"], "my-mode");
    assert_eq!(
        found.explanation, "A test mode draft",
        "explanation must be preserved"
    );
}

#[test]
fn customization_get_unknown_draft_id_404() {
    use super::drafts::DraftStore;
    let store = DraftStore::new();
    assert!(
        store.get("nonexistent-id").is_none(),
        "unknown draft_id must return None"
    );
}

#[test]
fn customization_get_kind_mismatch_404() {
    use super::drafts::DraftStore;
    let mut store = DraftStore::new();
    let draft = store.create(
        DraftKind::Skill,
        "My Skill".to_string(),
        "{}".to_string(),
        "explanation".to_string(),
    );
    let id = draft.id.clone();
    let found = store.get(&id).unwrap();
    assert_ne!(
        found.kind,
        DraftKind::Mode,
        "Skill draft used on modes route must be detectable as mismatch"
    );
}

#[test]
fn customization_save_consumes_draft() {
    let mut svc = make_service();
    let mut rx = svc.events_tx.subscribe();
    let draft = svc.draft_store.create(
        DraftKind::Mode,
        "My Mode".to_string(),
        "{}".to_string(),
        "explanation".to_string(),
    );
    let id = draft.id.clone();
    let consumed = svc.consume_draft(&id);
    assert!(consumed.is_some(), "consume_draft must return the draft");
    assert!(
        svc.draft_store.get(&id).is_none(),
        "draft must be gone after consume"
    );
    let event = rx.try_recv().expect("must receive DraftConsumed event");
    assert!(
        matches!(event, super::events::BuddyEvent::DraftConsumed { .. }),
        "event must be DraftConsumed"
    );
}

#[test]
fn customization_save_failed_does_not_consume() {
    use super::drafts::DraftStore;
    let mut store = DraftStore::new();
    let draft = store.create(
        DraftKind::Mode,
        "My Mode".to_string(),
        "{}".to_string(),
        "explanation".to_string(),
    );
    let id = draft.id.clone();
    assert!(
        store.get(&id).is_some(),
        "draft must remain unconsumed when save fails (not consumed)"
    );
}

#[test]
fn ext_skill_get_with_draft_id() {
    use super::drafts::DraftStore;
    let mut store = DraftStore::new();
    let draft = store.create(
        DraftKind::Skill,
        "My Skill".to_string(),
        "name: my-skill\ndescription: A test skill".to_string(),
        "explanation".to_string(),
    );
    let id = draft.id.clone();
    let found = store.get(&id).unwrap();
    assert_eq!(found.kind, DraftKind::Skill, "draft kind must be Skill");
    let data: serde_json::Value =
        serde_yaml::from_str(&found.yaml_or_json).expect("yaml_or_json must be parseable");
    assert_eq!(data["name"], "my-skill");
    assert_eq!(data["description"], "A test skill");
}

#[test]
fn ext_skill_save_consumes_draft() {
    let mut svc = make_service();
    let draft = svc.draft_store.create(
        DraftKind::Skill,
        "My Skill".to_string(),
        "name: my-skill\ndescription: A test skill".to_string(),
        "explanation".to_string(),
    );
    let id = draft.id.clone();
    let consumed = svc.consume_draft(&id);
    assert!(consumed.is_some(), "skill draft must be consumable");
    assert_eq!(consumed.unwrap().kind, DraftKind::Skill);
    assert!(
        svc.draft_store.get(&id).is_none(),
        "draft must be gone after consume"
    );
}

async fn draft_by_id(gcx: &AppState, id: &str) -> super::types::BuddyDraft {
    let buddy_arc = gcx.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    lock.as_ref()
        .and_then(|svc| svc.draft_store.get(id).cloned())
        .expect("draft must exist")
}

async fn draft_exists(gcx: &AppState, id: &str) -> bool {
    let buddy_arc = gcx.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    lock.as_ref()
        .and_then(|svc| svc.draft_store.get(id))
        .is_some()
}

async fn add_draft_to_gcx(gcx: &AppState, kind: DraftKind, title: &str, content: &str) -> String {
    let buddy_arc = gcx.buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    let svc = lock.as_mut().expect("buddy service must exist");
    svc.create_draft(kind, title.to_string(), content.to_string(), String::new())
        .expect("draft must be created")
        .id
}

#[tokio::test]
async fn draft_customization_change_reads_real_editor_storage() {
    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let config_dir = app.paths.config_dir.clone();
    tokio::fs::create_dir_all(config_dir.join("skills/real_skill"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(config_dir.join("commands"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(config_dir.join("subagents"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(config_dir.join("modes"))
        .await
        .unwrap();
    let skill_raw = "---\nname: real_skill\ndescription: Real skill\n---\nKeep exact skill body\n";
    let command_raw = "---\ndescription: Real command\n---\nKeep exact command body\n";
    let delegate_raw =
        "schema_version: 1\nid: real_delegate\ntitle: Real Delegate\nsubchat:\n  stateful: false\n";
    let mode_raw = "schema_version: 1\nid: real_mode\ntitle: Real Mode\nprompt: Keep mode prompt\n";
    let hooks_raw = "hooks:\n  SessionStart:\n    - hooks:\n        - type: command\n          command: echo start\n";
    tokio::fs::write(config_dir.join("skills/real_skill/SKILL.md"), skill_raw)
        .await
        .unwrap();
    tokio::fs::write(config_dir.join("commands/real_command.md"), command_raw)
        .await
        .unwrap();
    tokio::fs::write(
        config_dir.join("subagents/real_delegate.yaml"),
        delegate_raw,
    )
    .await
    .unwrap();
    tokio::fs::write(config_dir.join("modes/real_mode.yaml"), mode_raw)
        .await
        .unwrap();
    tokio::fs::write(config_dir.join("hooks.yaml"), hooks_raw)
        .await
        .unwrap();

    let cases = vec![
        (
            CustomizationKind::Skill,
            "real_skill",
            DraftKind::Skill,
            skill_raw,
        ),
        (
            CustomizationKind::Command,
            "real_command",
            DraftKind::Command,
            command_raw,
        ),
        (
            CustomizationKind::Delegate,
            "real_delegate",
            DraftKind::Delegate,
            delegate_raw,
        ),
        (
            CustomizationKind::Mode,
            "real_mode",
            DraftKind::Mode,
            mode_raw,
        ),
        (CustomizationKind::Hook, "hooks", DraftKind::Hook, hooks_raw),
    ];

    for (customization_kind, id, draft_kind, expected) in cases {
        let outcome = crate::http::routers::v1::buddy_opportunities::dispatch_action(
            app.clone(),
            "opp-real-storage",
            &BuddyAction::DraftCustomizationChange {
                customization_kind,
                id: id.to_string(),
                patch: serde_json::json!({}),
            },
        )
        .await
        .expect("draft action must succeed");
        let draft_id = outcome.result["draft_id"].as_str().unwrap();
        let draft = draft_by_id(&app, draft_id).await;
        assert_eq!(draft.kind, draft_kind);
        assert_eq!(draft.yaml_or_json, expected);
    }
}

#[tokio::test]
async fn ext_skill_save_with_command_draft_fails_and_keeps_draft() {
    use axum::extract::Path;
    use hyper::StatusCode;

    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let draft_id = add_draft_to_gcx(
        &app,
        DraftKind::Command,
        "Command Draft",
        "---\ndescription: Command\n---\nBody",
    )
    .await;
    let body = serde_json::to_vec(&serde_json::json!({
        "raw_content": "---\nname: target_skill\ndescription: Target\n---\nBody",
        "draft_id": draft_id.clone(),
        "scope": "global"
    }))
    .unwrap();
    let response = crate::http::routers::v1::ext_management::handle_v1_ext_skill_put(
        axum::extract::State(app.clone()),
        Path("target_skill".to_string()),
        hyper::body::Bytes::from(body),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(draft_exists(&app, &draft_id).await);
}

#[tokio::test]
async fn ext_skill_save_with_mismatched_draft_target_keeps_draft() {
    use axum::extract::Path;
    use hyper::StatusCode;

    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let draft_id = add_draft_to_gcx(
        &app,
        DraftKind::Skill,
        "Other Skill",
        "---\nname: other_skill\ndescription: Other\n---\nBody",
    )
    .await;
    let body = serde_json::to_vec(&serde_json::json!({
        "raw_content": "---\nname: target_skill\ndescription: Target\n---\nBody",

        "draft_id": draft_id.clone(),
        "scope": "global"
    }))
    .unwrap();
    let response = crate::http::routers::v1::ext_management::handle_v1_ext_skill_put(
        axum::extract::State(app.clone()),
        Path("target_skill".to_string()),
        hyper::body::Bytes::from(body),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(draft_exists(&app, &draft_id).await);
}

#[tokio::test]
async fn raw_skill_save_rejects_mismatched_frontmatter_name() {
    use axum::extract::Path;
    use hyper::StatusCode;

    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let body = serde_json::to_vec(&serde_json::json!({
        "raw_content": "---\nname: wrong_skill\ndescription: Wrong\n---\nBody",
        "scope": "global"
    }))
    .unwrap();
    let response = crate::http::routers::v1::ext_management::handle_v1_ext_skill_put(
        axum::extract::State(app.clone()),
        Path("target_skill".to_string()),
        hyper::body::Bytes::from(body),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn registry_validation_error_does_not_consume_draft() {
    use hyper::{Body, Request, StatusCode};
    use tower::ServiceExt;

    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let config_dir = app.paths.config_dir.clone();
    tokio::fs::create_dir_all(config_dir.join("modes"))
        .await
        .unwrap();
    tokio::fs::write(config_dir.join("modes/broken.yaml"), "schema_version: [")
        .await
        .unwrap();
    let draft_id = add_draft_to_gcx(
        &app,
        DraftKind::Mode,
        "Mode Draft",
        "schema_version: 1\nid: valid_mode\ntitle: Valid\nprompt: ok\n",
    )
    .await;
    let body = serde_json::to_vec(&serde_json::json!({
        "config": {
            "schema_version": 1,
            "id": "valid_mode",
            "title": "Valid",
            "prompt": "ok"
        },
        "scope": "global",
        "draft_id": draft_id.clone()
    }))
    .unwrap();
    let app_state = app.clone();
    let router = crate::http::routers::v1::make_v1_router(app_state.clone()).with_state(app_state);
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/customization/modes/valid_mode")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["ok"], false);
    assert!(draft_exists(&app, &draft_id).await);
}

#[tokio::test]
async fn customization_delegates_route_writes_subagent_storage_and_consumes() {
    use hyper::{Body, Request, StatusCode};
    use tower::ServiceExt;

    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let config_dir = app.paths.config_dir.clone();
    let draft_id = add_draft_to_gcx(
        &app,
        DraftKind::Delegate,
        "Delegate Draft",
        "schema_version: 1\nid: helper_delegate\ntitle: Helper\nsubchat:\n  stateful: false\n",
    )
    .await;
    let body = serde_json::to_vec(&serde_json::json!({
        "config": {
            "schema_version": 1,
            "id": "helper_delegate",
            "title": "Helper",
            "subchat": { "stateful": false }
        },
        "scope": "global",
        "draft_id": draft_id.clone()
    }))
    .unwrap();
    let app_state = app.clone();
    let router = crate::http::routers::v1::make_v1_router(app_state.clone()).with_state(app_state);
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/customization/delegates/helper_delegate")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        tokio::fs::metadata(config_dir.join("subagents/helper_delegate.yaml"))
            .await
            .is_ok()
    );
    assert!(!draft_exists(&app, &draft_id).await);
}

#[tokio::test]
async fn concurrent_ext_atomic_writes_use_unique_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("commands").join("same.md");
    let first = crate::http::routers::v1::ext_management::write_file_atomic(&path, "first");
    let second = crate::http::routers::v1::ext_management::write_file_atomic(&path, "second");
    let (a, b) = tokio::join!(first, second);
    assert!(a.is_ok());
    assert!(b.is_ok());
    let final_content = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(final_content == "first" || final_content == "second");
    let mut entries = tokio::fs::read_dir(path.parent().unwrap()).await.unwrap();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(!name.ends_with(".tmp"));
    }
}

#[tokio::test]
async fn customization_save_leaves_no_atomic_temp_file() {
    use hyper::{Body, Request, StatusCode};
    use tower::ServiceExt;

    let app = make_gcx_with_buddy().await;
    let _gcx = app.gcx.clone();
    let config_dir = app.paths.config_dir.clone();
    let body = serde_json::to_vec(&serde_json::json!({
        "config": {
            "schema_version": 1,
            "id": "atomic_mode",
            "title": "Atomic",
            "prompt": "ok"
        },
        "scope": "global"
    }))
    .unwrap();
    let app_state = app.clone();
    let router = crate::http::routers::v1::make_v1_router(app_state.clone()).with_state(app_state);
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/customization/modes/atomic_mode")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let modes_dir = config_dir.join("modes");
    let mut entries = tokio::fs::read_dir(&modes_dir).await.unwrap();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(!name.ends_with(".tmp"));
    }
}

// =============================================================================
// T-19: Privacy hardening + Opportunity lifecycle correctness
// =============================================================================
#[test]
fn chat_pattern_no_message_clone_in_observe() {
    use super::observers::chat_pattern::run_chat_pattern_observer_sync;
    let messages = vec![
        chat_msg("user", "token Bearer sk-VERY_SECRET_KEY_DONT_LEAK"),
        chat_msg("user", "actually undo that"),
        chat_msg("user", "wait try again"),
        chat_msg("user", "sorry revert"),
    ];
    let ptr_before = messages.as_ptr();
    let facts = run_chat_pattern_observer_sync(&messages, "chat-ptr-test");
    let ptr_after = messages.as_ptr();
    assert_eq!(ptr_before, ptr_after, "caller slice must not be moved");
    let json = serde_json::to_string(&facts).unwrap();
    assert!(
        !json.contains("VERY_SECRET_KEY"),
        "secret must not appear in facts"
    );
    assert!(
        !json.contains("Bearer"),
        "Bearer token must not appear in facts"
    );
    assert!(facts
        .iter()
        .any(|f| matches!(f.kind, BuddyFactKind::ChatRetryStreak)));
}

#[tokio::test]
async fn diagnostic_persisted_jsonl_is_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();
    let (tx, _rx) = broadcast::channel(16);
    let mut svc = BuddyService::new(
        root.to_path_buf(),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    svc.report_error(
        "test",
        "connection failed: Bearer sk-LEAK_THIS_TOKEN",
        None,
        None,
    );
    let jsonl_path = root.join(".refact/buddy/diagnostics.jsonl");
    let mut content = String::new();
    for _ in 0..100 {
        content = tokio::fs::read_to_string(&jsonl_path)
            .await
            .unwrap_or_default();
        if !content.is_empty() {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    assert!(
        !content.contains("sk-LEAK_THIS_TOKEN"),
        "secret must not be in jsonl: {}",
        content
    );
    assert!(
        content.contains("[REDACTED"),
        "redaction marker must be in jsonl: {}",
        content
    );
}

#[tokio::test]
async fn dismissed_history_survives_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();
    let mut svc = make_service();
    let opp = make_opportunity("opp-dm-rt", "ck-dm-rt");
    svc.add_opportunity(opp);
    svc.resolve_opportunity("opp-dm-rt", OpportunityStatus::Dismissed);
    assert!(
        svc.opportunity_queue
            .recently_dismissed("ck-dm-rt", Duration::hours(24)),
        "must be recently dismissed before save"
    );
    let state = svc.state.clone();
    super::state::save_state(root, &state).await.unwrap();
    let loaded = super::state::load_state(root).await;
    let queue = super::opportunities::OpportunityQueue::from_state(
        loaded.opportunities,
        loaded.dismissed_history,
    );
    assert!(
        queue.recently_dismissed("ck-dm-rt", Duration::hours(24)),
        "dismissed history must survive save/load round-trip"
    );
}

#[test]
fn per_rule_cooldown_honored() {
    use super::facts::FactStore;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    let now = chrono::Utc::now();
    let mut store = FactStore::new();
    store.ingest(BuddyFact {
        kind: BuddyFactKind::TaskStuck,
        key: "task:stuck:cooldown-test".to_string(),
        source: "test",
        payload: serde_json::json!({"task_id": "cooldown-test"}),
        seen_at: now,
        confidence: 1.0,
    });
    let pulse = BuddyPulse::default();
    let queue = OpportunityQueue::new();
    let results = OpportunityDetector::new().detect(&store, &pulse, &queue);
    assert!(!results.is_empty(), "must produce at least one opportunity");
    let (_, cooldown_secs) = &results[0];
    assert_eq!(
        *cooldown_secs, 3600,
        "task_stuck rule must use 3600s cooldown"
    );

    let mut q = OpportunityQueue::new();
    q.push_with_cooldown(make_opportunity("opp-zero", "ck-zero-cd"), 0);
    assert!(
        !q.cooldown_active("ck-zero-cd"),
        "0s cooldown must not block"
    );

    let mut q2 = OpportunityQueue::new();
    q2.push_with_cooldown(make_opportunity("opp-long", "ck-long-cd"), 3600);
    assert!(q2.cooldown_active("ck-long-cd"), "1h cooldown must block");
}

// =============================================================================
// Observer↔Detector schema contract tests
// =============================================================================
#[test]
fn provider_health_payload_keys_match_detector() {
    use super::facts::FactStore;
    use super::observers::provider_health::detect_provider_health_facts;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    use crate::caps::DefaultModels;
    let now = chrono::Utc::now();
    let defaults = DefaultModels {
        completion_default_model: String::new(),
        chat_model_2: String::new(),
        task_planner_agent_model: String::new(),
        chat_default_model: "openai/gpt-4o".to_string(),
        chat_thinking_model: String::new(),
        chat_light_model: String::new(),
        chat_buddy_model: String::new(),
    };
    let available = vec!["openai/gpt-4o".to_string()];
    let facts = detect_provider_health_facts(&defaults, &available, &[], now);
    assert!(facts
        .iter()
        .any(|f| f.kind == BuddyFactKind::DefaultModelMissing));
    let mut store = FactStore::new();
    for f in facts {
        store.ingest(f);
    }
    let defaults2 = DefaultModels {
        completion_default_model: String::new(),
        chat_model_2: String::new(),
        task_planner_agent_model: String::new(),
        chat_default_model: "openai/gpt-4o".to_string(),
        chat_thinking_model: String::new(),
        chat_light_model: String::new(),
        chat_buddy_model: String::new(),
    };
    let available2 = vec![];
    let facts2 = detect_provider_health_facts(&defaults2, &available2, &[], now);
    for f in facts2 {
        store.ingest(f);
    }
    let pulse = BuddyPulse::default();
    let queue = OpportunityQueue::new();
    let opps = OpportunityDetector::new().detect(&store, &pulse, &queue);
    let provider_opps: Vec<_> = opps
        .iter()
        .filter(|(o, _)| o.kind == BuddyOpportunityKind::ProviderTuning)
        .collect();
    assert!(
        !provider_opps.is_empty(),
        "must emit ProviderTuning opportunity"
    );
    for (opp, _) in &provider_opps {
        assert!(
            !opp.cooldown_key.is_empty(),
            "cooldown_key must not be empty"
        );
    }
    let broken_opp = opps.iter().find(|(o, _)| {
        o.kind == BuddyOpportunityKind::ProviderTuning && o.summary.contains("not available")
    });
    if let Some((opp, _)) = broken_opp {
        let action_has_model = opp.proposed_actions.iter().any(|a| {
            if let BuddyAction::DraftDefaultsChange { .. } = a {
                true
            } else if let BuddyAction::OpenPage { .. } = a {
                true
            } else {
                false
            }
        });
        assert!(
            action_has_model,
            "broken_ref opp must have DefaultsChange or OpenPage action"
        );
    }
}

#[test]
fn mcp_auth_payload_keys_match_detector() {
    use super::facts::FactStore;
    use super::observers::mcp_auth::{detect_mcp_auth_facts, McpSessionSnapshot};
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    use crate::integrations::mcp::session_mcp::MCPAuthStatus;
    let now = chrono::Utc::now();
    let expires_4m = now.timestamp_millis() + 4 * 60 * 1000;
    let snaps = vec![
        McpSessionSnapshot {
            id: "github-mcp".to_string(),
            auth_status: MCPAuthStatus::Authenticated,
            failed_calls: 0,
            expires_at_ms: Some(expires_4m),
            has_refresh_token: false,
        },
        McpSessionSnapshot {
            id: "linear-mcp".to_string(),
            auth_status: MCPAuthStatus::NotApplicable,
            failed_calls: 5,
            expires_at_ms: None,
            has_refresh_token: false,
        },
    ];
    let facts = detect_mcp_auth_facts(&snaps, now);
    assert!(facts
        .iter()
        .any(|f| f.kind == BuddyFactKind::McpAuthExpired));
    assert!(facts
        .iter()
        .any(|f| f.kind == BuddyFactKind::IntegrationFailing));
    for fact in &facts {
        match fact.kind {
            BuddyFactKind::McpAuthExpired | BuddyFactKind::IntegrationFailing => {
                let mcp_id = fact.payload.get("mcp_id").and_then(|v| v.as_str());
                assert!(
                    mcp_id.is_some() && !mcp_id.unwrap().is_empty(),
                    "mcp_id must be present and non-empty in {:?}",
                    fact.kind
                );
            }
            _ => {}
        }
    }
    let mut store = FactStore::new();
    for f in facts {
        store.ingest(f);
    }
    let pulse = BuddyPulse::default();
    let queue = OpportunityQueue::new();
    let opps = OpportunityDetector::new().detect(&store, &pulse, &queue);
    let fix_opps: Vec<_> = opps
        .iter()
        .filter(|(o, _)| o.kind == BuddyOpportunityKind::IntegrationFix)
        .collect();
    assert!(!fix_opps.is_empty(), "must emit IntegrationFix opportunity");
    for (opp, _) in &fix_opps {
        assert!(
            !opp.cooldown_key.is_empty() && opp.cooldown_key != "integration:mcp_auth:unknown",
            "cooldown_key must contain real mcp_id, got: {}",
            opp.cooldown_key
        );
    }
}

#[test]
fn mode_overlap_payload_keys_match_detector() {
    use super::facts::FactStore;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    let now = chrono::Utc::now();
    let mut store = FactStore::new();
    store.ingest(BuddyFact {
        kind: BuddyFactKind::ModePromptOverlap,
        key: "customization:mode_overlap:alpha:beta".to_string(),
        source: "test",
        payload: serde_json::json!({
            "mode_id": "beta",
            "peer_id": "alpha",
            "similarity": 0.92f32,
        }),
        seen_at: now,
        confidence: 0.8,
    });
    let pulse = BuddyPulse::default();
    let queue = OpportunityQueue::new();
    let opps = OpportunityDetector::new().detect(&store, &pulse, &queue);
    let drift_opps: Vec<_> = opps
        .iter()
        .filter(|(o, _)| o.kind == BuddyOpportunityKind::ConfigDrift)
        .collect();
    assert!(
        !drift_opps.is_empty(),
        "must emit ConfigDrift opportunity for ModePromptOverlap"
    );
    let (opp, _) = &drift_opps[0];
    assert!(
        !opp.cooldown_key.is_empty() && opp.cooldown_key != "config_drift:mode_overlap:",
        "cooldown_key must include real mode_id, got: {}",
        opp.cooldown_key
    );
    let has_customization_action = opp
        .proposed_actions
        .iter()
        .any(|a| matches!(a, BuddyAction::DraftCustomizationChange { .. }));
    assert!(
        has_customization_action,
        "opp must have DraftCustomizationChange action"
    );
    if let Some(BuddyAction::DraftCustomizationChange { id, .. }) = opp
        .proposed_actions
        .iter()
        .find(|a| matches!(a, BuddyAction::DraftCustomizationChange { .. }))
    {
        assert_eq!(id, "beta", "action id must match mode_id from payload");
    }
}

#[tokio::test]
async fn accept_synthesizes_real_draft() {
    let (tx, _rx) = broadcast::channel(32);
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    let mut opp = make_opportunity("opp-synth", "ck-synth");
    opp.proposed_actions = vec![BuddyAction::DraftAgentsMdPatch {
        content: String::new(),
    }];
    svc.add_opportunity(opp);
    let synth_opp = svc.opportunity_queue.get("opp-synth").cloned().unwrap();
    let action = &synth_opp.proposed_actions[0];
    if let BuddyAction::DraftAgentsMdPatch { content } = action {
        assert!(content.is_empty(), "action has empty content placeholder");
    }
    let draft = svc.draft_store.create(
        DraftKind::AgentsMd,
        "AGENTS.md".to_string(),
        "# AGENTS.md\n\nThis file provides guidance to AI agents.".to_string(),
        String::new(),
    );
    assert!(
        !draft.id.is_empty(),
        "synthesized draft must have non-empty id"
    );
    assert!(
        !draft.yaml_or_json.is_empty(),
        "synthesized draft must have non-empty content"
    );
    assert_eq!(draft.kind, DraftKind::AgentsMd);
    assert!(
        svc.draft_store.get(&draft.id).is_some(),
        "synthesized draft must be stored"
    );
}

#[test]
fn terminal_opp_retention_uses_resolved_at() {
    use super::opportunities::OpportunityQueue;
    let now = chrono::Utc::now();

    // Opp with old created_at but resolved recently — must NOT be evicted at now+23h
    let mut opp1 = make_opportunity("opp-rt1", "ck-rt1");
    opp1.created_at = now - Duration::hours(48);
    opp1.expires_at = now - Duration::hours(47);
    opp1.status = OpportunityStatus::Dismissed;
    opp1.resolved_at = Some(now);

    let mut q1 = OpportunityQueue::new();
    q1.items.push(opp1);
    q1.expire_old(now + Duration::minutes(23 * 60 + 59));
    assert!(
        q1.get("opp-rt1").is_some(),
        "opp resolved now must survive at now+23h59m"
    );

    // Same opp — must be evicted at now+24h01m
    let mut opp2 = make_opportunity("opp-rt2", "ck-rt2");
    opp2.created_at = now - Duration::hours(48);
    opp2.expires_at = now - Duration::hours(47);
    opp2.status = OpportunityStatus::Completed;
    opp2.resolved_at = Some(now);

    let mut q2 = OpportunityQueue::new();
    q2.items.push(opp2);
    q2.expire_old(now + Duration::minutes(24 * 60 + 1));
    assert!(
        q2.get("opp-rt2").is_none(),
        "opp resolved now must be evicted at now+24h01m"
    );
}
