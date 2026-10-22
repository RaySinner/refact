use super::*;

// =============================================================================
// F-A: Humor off-lock + task health semantics + pulse completeness
// =============================================================================
#[tokio::test]
async fn humor_attach_does_not_hold_buddy_lock() {
    use super::humor::{HumorGenerator, HumorService};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SlowGen {
        started: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl HumorGenerator for SlowGen {
        async fn generate(
            &self,
            _kind: BuddyFactKind,
            _summary: String,
            _gcx: AppState,
        ) -> Vec<String> {
            self.started.store(true, Ordering::SeqCst);
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            vec!["slow joke".to_string()]
        }
    }

    let started = Arc::new(AtomicBool::new(false));
    let mut svc = make_service();
    svc.humor_service = Arc::new(tokio::sync::Mutex::new(HumorService::new_with_generator(
        Arc::new(SlowGen {
            started: started.clone(),
        }),
    )));

    let humor_arc = svc.humor_service.clone();
    let buddy_arc: Arc<tokio::sync::Mutex<Option<BuddyService>>> =
        Arc::new(tokio::sync::Mutex::new(Some(svc)));

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let pulse = BuddyPulse::default();

    // Simulate the background loop: extract humor_arc under buddy lock then release.
    let _ = {
        let guard = buddy_arc.lock().await;
        guard.as_ref().unwrap().humor_service.clone()
    };

    // Task A: hold humor lock for 200ms (simulates attach_humor outside buddy lock).
    let humor_clone = humor_arc.clone();
    let app_clone = app.clone();
    let pulse_clone = pulse.clone();
    let humor_task = tokio::spawn(async move {
        let mut humor = humor_clone.lock().await;
        let mut opp = make_opportunity("h1", "ck-h1");
        apply_humor_plan(
            &mut humor,
            &mut opp,
            BuddyFactKind::TaskStuck,
            &pulse_clone,
            app_clone,
        )
        .await;
    });

    // Wait until SlowGen starts running.
    while !started.load(Ordering::SeqCst) {
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }

    // add_activity must NOT be blocked by the humor lock — buddy lock is free.
    let start = std::time::Instant::now();
    {
        let mut guard = buddy_arc.lock().await;
        if let Some(svc) = guard.as_mut() {
            svc.add_activity(super::types::BuddyActivity {
                icon: "🔧".to_string(),
                title: "test".to_string(),
                description: "test".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                activity_type: "test".to_string(),
                chat_id: None,
                failure_category: None,
                failure_summary: None,
            });
        }
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 50,
        "add_activity must complete fast while humor runs, took {}ms",
        elapsed.as_millis()
    );
    humor_task.await.unwrap();
}

#[test]
fn task_stuck_uses_heartbeat_not_started_at() {
    use super::observers::task_health::{detect_task_health_facts, TaskHealthEntry};
    let now = chrono::Utc::now();
    let meta = make_task_meta("t1", "Fix bug", TaskStatus::Active, &now.to_rfc3339());
    let board = TaskBoard {
        schema_version: 1,
        rev: 0,
        columns: vec![],
        cards: vec![make_board_card(
            "c1",
            "doing",
            Some("agent-1"),
            Some(&(now - Duration::hours(1)).to_rfc3339()),
        )],
    };

    // Fresh heartbeat (1 min ago) — no stuck fact even though started_at is 1h old.
    let fresh = vec![TaskHealthEntry {
        meta: meta.clone(),
        board: board.clone(),
        last_heartbeat: Some(now - Duration::minutes(1)),
        touched_files: vec![],
    }];
    let facts = detect_task_health_facts(&fresh, now);
    assert!(
        facts.iter().all(|f| f.kind != BuddyFactKind::TaskStuck),
        "fresh heartbeat must not emit TaskStuck"
    );

    // Stale heartbeat (5h ago) — stuck fact emitted.
    let stale = vec![TaskHealthEntry {
        meta,
        board,
        last_heartbeat: Some(now - Duration::hours(5)),
        touched_files: vec![],
    }];
    let facts = detect_task_health_facts(&stale, now);
    assert!(
        facts.iter().any(|f| f.kind == BuddyFactKind::TaskStuck),
        "stale heartbeat must emit TaskStuck"
    );
}

#[test]
fn task_abandoned_requires_no_heartbeat_ever() {
    use super::observers::task_health::{detect_task_health_facts, TaskHealthEntry};
    let now = chrono::Utc::now();
    let old_created = (now - Duration::days(8)).to_rfc3339();
    let board = TaskBoard {
        schema_version: 1,
        rev: 0,
        columns: vec![],
        cards: vec![],
    };

    // Has a heartbeat — NOT abandoned even if old.
    let with_hb = vec![TaskHealthEntry {
        meta: make_task_meta("t1", "Old task", TaskStatus::Active, &old_created),
        board: board.clone(),
        last_heartbeat: Some(now - Duration::days(7)),
        touched_files: vec![],
    }];
    let facts = detect_task_health_facts(&with_hb, now);
    assert!(
        facts.iter().all(|f| f.kind != BuddyFactKind::TaskAbandoned),
        "task with heartbeat must not be abandoned"
    );

    // No heartbeat + old enough — Abandoned fact emitted.
    let no_hb = vec![TaskHealthEntry {
        meta: make_task_meta("t2", "Old task 2", TaskStatus::Active, &old_created),
        board,
        last_heartbeat: None,
        touched_files: vec![],
    }];
    let facts = detect_task_health_facts(&no_hb, now);
    assert!(
        facts.iter().any(|f| f.kind == BuddyFactKind::TaskAbandoned),
        "task with no heartbeat and age>7d must be abandoned"
    );
}

#[test]
fn task_cluster_requires_file_overlap() {
    use super::observers::task_health::{detect_task_health_facts, TaskHealthEntry};
    let now = chrono::Utc::now();
    let board = TaskBoard {
        schema_version: 1,
        rev: 0,
        columns: vec![],
        cards: vec![],
    };

    // Similar names but disjoint files — no duplicate fact.
    let disjoint = vec![
        TaskHealthEntry {
            meta: make_task_meta("t1", "Fix auth bug", TaskStatus::Active, &now.to_rfc3339()),
            board: board.clone(),
            last_heartbeat: None,
            touched_files: vec!["src/auth.rs".to_string()],
        },
        TaskHealthEntry {
            meta: make_task_meta(
                "t2",
                "Fix auth issue",
                TaskStatus::Active,
                &now.to_rfc3339(),
            ),
            board: board.clone(),
            last_heartbeat: None,
            touched_files: vec!["src/session.rs".to_string()],
        },
    ];
    let facts = detect_task_health_facts(&disjoint, now);
    assert!(
        facts
            .iter()
            .all(|f| f.kind != BuddyFactKind::TaskClusterDuplicate),
        "disjoint file sets must not produce cluster duplicate"
    );

    // Similar names with shared file — duplicate fact emitted.
    let overlap = vec![
        TaskHealthEntry {
            meta: make_task_meta("t3", "Fix auth bug", TaskStatus::Active, &now.to_rfc3339()),
            board: board.clone(),
            last_heartbeat: None,
            touched_files: vec!["src/auth.rs".to_string(), "src/common.rs".to_string()],
        },
        TaskHealthEntry {
            meta: make_task_meta(
                "t4",
                "Fix auth issue",
                TaskStatus::Active,
                &now.to_rfc3339(),
            ),
            board,
            last_heartbeat: None,
            touched_files: vec!["src/common.rs".to_string(), "src/session.rs".to_string()],
        },
    ];
    let facts = detect_task_health_facts(&overlap, now);
    assert!(
        facts
            .iter()
            .any(|f| f.kind == BuddyFactKind::TaskClusterDuplicate),
        "shared file must produce cluster duplicate for similar tasks"
    );
}

#[tokio::test]
async fn pulse_populates_all_subpulse_counts() {
    use super::facts::FactStore;
    use super::pulse::build_pulse;
    use crate::caps::CodeAssistantCaps;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;

    {
        let mut caps = CodeAssistantCaps::default();
        caps.defaults.chat_default_model = "openai/gpt-4o".to_string();
        caps.defaults.chat_model_2 = "openai/gpt-4o-mini".to_string();
        caps.defaults.task_planner_agent_model = "openai/gpt-4o-mini".to_string();
        caps.defaults.chat_light_model = "openai/gpt-4o-mini".to_string();
        caps.defaults.chat_thinking_model = "openai/o1".to_string();
        caps.defaults.chat_buddy_model = "openai/gpt-4o-mini".to_string();
        app.model.caps.write().await.caps = Some(Arc::new(caps));
    }

    let config_dir = app.paths.config_dir.clone();
    let skill_dir = config_dir.join("skills/pulse_skill");
    tokio::fs::create_dir_all(&skill_dir).await.unwrap();
    tokio::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: pulse_skill\ndescription: Pulse skill\n---\nBody\n",
    )
    .await
    .unwrap();
    tokio::fs::write(
        config_dir.join("hooks.yaml"),
        "hooks:\n  SessionStart:\n    - hooks:\n        - type: command\n          command: echo pulse\n",
    )
    .await
    .unwrap();

    // Inject recent stuck + abandoned alert facts into the FactStore.
    let mut store = FactStore::new();
    let now = chrono::Utc::now();
    store.ingest(make_fact("task:stuck:t1", BuddyFactKind::TaskStuck, now));
    store.ingest(make_fact(
        "task:abandoned:t2",
        BuddyFactKind::TaskAbandoned,
        now,
    ));

    let pulse = build_pulse(app.clone(), std::path::Path::new("/tmp"), &store).await;

    assert!(pulse.generated_at.is_some(), "generated_at must be set");
    assert!(
        pulse.providers.defaults_ok,
        "defaults_ok must be true when all chat-family models are set"
    );
    assert_eq!(
        pulse.mcp.total,
        gcx.integration_sessions.lock().await.len() as u32,
        "mcp.total must match integration_sessions count"
    );
    assert!(pulse.customization.skills > 0, "skills must be populated");
    assert!(pulse.customization.hooks > 0, "hooks must be populated");
    assert_eq!(
        pulse.tasks.recent_stuck_alerts_1h, 1,
        "recent stuck alert count must reflect injected fact"
    );
    assert_eq!(
        pulse.tasks.recent_abandoned_alerts_24h, 1,
        "recent abandoned alert count must reflect injected fact"
    );
    assert_eq!(
        pulse.tasks.stuck, 1,
        "legacy stuck count must stay compatible"
    );
    assert_eq!(
        pulse.tasks.abandoned, 1,
        "legacy abandoned count must stay compatible"
    );
}

// =============================================================================
// F-C: Opportunity resolution correctness
// =============================================================================
#[tokio::test]
async fn accept_dismiss_action_via_accept_route_is_single_resolution() {
    use crate::http::routers::v1::buddy_opportunities::dispatch_action;

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

    let mut opp = make_opportunity("opp-acc-dm", "ck-acc-dm");
    opp.proposed_actions = vec![BuddyAction::Dismiss];
    svc.add_opportunity(opp);
    let _ = rx.try_recv();

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let outcome = dispatch_action(app.clone(), "opp-acc-dm", &BuddyAction::Dismiss)
        .await
        .unwrap();

    assert_eq!(
        outcome.status,
        OpportunityStatus::Dismissed,
        "Dismiss action must produce Dismissed status"
    );

    svc.resolve_opportunity("opp-acc-dm", outcome.status);

    let resolved = svc.opportunity_queue.get("opp-acc-dm").unwrap();
    assert_eq!(
        resolved.status,
        OpportunityStatus::Dismissed,
        "opp must be Dismissed, not Accepted"
    );
    assert!(
        svc.opportunity_queue
            .recently_dismissed("ck-acc-dm", Duration::hours(24)),
        "dismissed_history must contain the cooldown key"
    );

    let event = rx.try_recv().expect("must have OpportunityResolved event");
    assert!(
        matches!(event, super::events::BuddyEvent::OpportunityResolved { .. }),
        "event must be OpportunityResolved"
    );
    assert!(
        rx.try_recv().is_err(),
        "must be exactly one OpportunityResolved event"
    );
}

#[tokio::test]
async fn draft_customization_change_dispatches() {
    use crate::http::routers::v1::buddy_opportunities::dispatch_action;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let buddy_svc = make_service();
    *app.buddy.buddy.lock().await = Some(buddy_svc);

    let outcome = dispatch_action(
        app.clone(),
        "opp-customization",
        &BuddyAction::DraftCustomizationChange {
            customization_kind: CustomizationKind::Mode,
            id: "mode-x".to_string(),
            patch: serde_json::json!({}),
        },
    )
    .await
    .unwrap();

    assert_eq!(outcome.status, OpportunityStatus::Accepted);
    assert_eq!(
        outcome.result.get("draft_kind").and_then(|v| v.as_str()),
        Some("mode")
    );
    assert!(
        outcome
            .result
            .get("draft_id")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "draft_id must be present"
    );
}

#[tokio::test]
async fn direct_dismiss_persists_dismissed_history_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let mut svc = make_service();
    let opp = make_opportunity("opp-dm-persist", "ck-dm-persist");
    svc.add_opportunity(opp);

    svc.resolve_opportunity("opp-dm-persist", OpportunityStatus::Dismissed);

    let state = svc.state.clone();
    super::state::save_state(root, &state).await.unwrap();

    let loaded = super::state::load_state(root).await;
    let queue = super::opportunities::OpportunityQueue::from_state(
        loaded.opportunities,
        loaded.dismissed_history,
    );

    assert!(
        queue.recently_dismissed("ck-dm-persist", Duration::hours(24)),
        "dismissed history must survive save/load round-trip"
    );
}

#[tokio::test]
async fn accept_route_response_shape_for_defaults_draft() {
    use crate::http::routers::v1::buddy_opportunities::dispatch_action;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    {
        let (etx, _) = broadcast::channel(16);
        let buddy_svc = BuddyService::new(
            std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
            default_buddy_state(),
            BuddySettings::default(),
            Vec::new(),
            super::runtime_queue::RuntimeQueue::new(),
            etx,
            None,
        );
        *app.buddy.buddy.lock().await = Some(buddy_svc);
    }

    let action = BuddyAction::DraftDefaultsChange {
        defaults_kind: DefaultsKind::ChatBuddyModel,
        patch: serde_json::json!({}),
    };

    let outcome = dispatch_action(app.clone(), "irrelevant-id", &action)
        .await
        .unwrap();

    assert_eq!(outcome.status, OpportunityStatus::Accepted);

    let result = &outcome.result;
    assert_eq!(
        result.get("draft_kind").and_then(|v| v.as_str()),
        Some("defaults_model"),
        "draft_kind must always be the DraftKind value"
    );
    assert_eq!(
        result.get("defaults_kind").and_then(|v| v.as_str()),
        Some("chat_buddy_model"),
        "defaults_kind must be a separate field with the DefaultsKind value"
    );
    let draft_id = result
        .get("draft_id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let draft = lock.as_ref().unwrap().draft_store.get(&draft_id).unwrap();
    let content: serde_json::Value = serde_json::from_str(&draft.yaml_or_json).unwrap();
    assert_eq!(
        content
            .get("chat_buddy")
            .and_then(|v| v.get("model"))
            .and_then(|v| v.as_str()),
        Some("your-provider/model-name")
    );

    assert!(
        result
            .get("draft_id")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "draft_id must be present and non-empty"
    );
}

#[tokio::test]
async fn defaults_update_with_valid_draft_consumes_after_save() {
    use crate::providers::config::ProviderDefaults;
    use crate::providers::http::handle_v1_defaults_update;
    use hyper::body::Bytes;
    use hyper::StatusCode;

    let dir = tempfile::tempdir().unwrap();
    let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
        std::env::temp_dir().join(format!("refact-test-{}", uuid::Uuid::new_v4())),
        dir.path().to_path_buf(),
    )
    .await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;

    let mut svc = make_service();
    let draft = svc
        .create_draft(
            DraftKind::DefaultsModel,
            "Default Models".to_string(),
            r#"{"chat":{"model":"openai/gpt-4o"}}"#.to_string(),
            String::new(),
        )
        .unwrap();
    let draft_id = draft.id.clone();
    *app.buddy.buddy.lock().await = Some(svc);

    let body = serde_json::json!({
        "chat": { "model": "openai/gpt-4o" },
        "chat_light": { "model": "openai/gpt-4o-mini" },
        "chat_thinking": {},
        "chat_buddy": {},
        "draft_id": draft_id.clone(),
    });
    let response = handle_v1_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(body.to_string()),
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let saved = ProviderDefaults::load(dir.path()).await.unwrap();
    assert_eq!(saved.chat.model.as_deref(), Some("openai/gpt-4o"));
    assert_eq!(
        saved.chat_light.model.as_deref(),
        Some("openai/gpt-4o-mini")
    );
    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    assert!(lock.as_ref().unwrap().draft_store.get(&draft_id).is_none());
}

#[tokio::test]
async fn defaults_update_wrong_draft_kind_returns_conflict_and_keeps_draft() {
    use crate::providers::http::handle_v1_defaults_update;
    use hyper::body::Bytes;
    use hyper::StatusCode;

    let dir = tempfile::tempdir().unwrap();
    let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
        std::env::temp_dir().join(format!("refact-test-{}", uuid::Uuid::new_v4())),
        dir.path().to_path_buf(),
    )
    .await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;

    let mut svc = make_service();
    let draft = svc
        .create_draft(
            DraftKind::Skill,
            "Skill Draft".to_string(),
            "---\nname: skill\n---\nbody".to_string(),
            String::new(),
        )
        .unwrap();
    let draft_id = draft.id.clone();
    *app.buddy.buddy.lock().await = Some(svc);

    let body = serde_json::json!({
        "chat": { "model": "openai/gpt-4o" },
        "chat_light": {},
        "chat_thinking": {},
        "chat_buddy": {},
        "draft_id": draft_id.clone(),
    });
    let err = handle_v1_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(body.to_string()),
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, StatusCode::CONFLICT);
    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    assert!(lock.as_ref().unwrap().draft_store.get(&draft_id).is_some());
}

#[tokio::test]
async fn defaults_update_parse_invalid_draft_returns_422_and_keeps_draft() {
    use crate::providers::http::handle_v1_defaults_update;
    use hyper::body::Bytes;
    use hyper::StatusCode;

    let dir = tempfile::tempdir().unwrap();
    let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
        std::env::temp_dir().join(format!("refact-test-{}", uuid::Uuid::new_v4())),
        dir.path().to_path_buf(),
    )
    .await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;

    let mut svc = make_service();
    let draft = svc
        .create_draft(
            DraftKind::DefaultsModel,
            "Default Models".to_string(),
            r#"{"chat_default_model":"openai/gpt-4o"}"#.to_string(),
            String::new(),
        )
        .unwrap();
    let draft_id = draft.id.clone();
    *app.buddy.buddy.lock().await = Some(svc);

    let body = serde_json::json!({
        "chat": { "model": "openai/gpt-4o" },
        "chat_light": {},
        "chat_thinking": {},
        "chat_buddy": {},
        "draft_id": draft_id.clone(),
    });
    let err = handle_v1_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(body.to_string()),
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, StatusCode::UNPROCESSABLE_ENTITY);
    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    assert!(lock.as_ref().unwrap().draft_store.get(&draft_id).is_some());
}

#[tokio::test]
async fn defaults_update_without_draft_id_still_saves() {
    use crate::providers::config::ProviderDefaults;
    use crate::providers::http::handle_v1_defaults_update;
    use hyper::body::Bytes;
    use hyper::StatusCode;

    let dir = tempfile::tempdir().unwrap();
    let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
        std::env::temp_dir().join(format!("refact-test-{}", uuid::Uuid::new_v4())),
        dir.path().to_path_buf(),
    )
    .await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;

    let body = serde_json::json!({
        "chat": { "model": "openai/gpt-4o" },
        "chat_light": {},
        "chat_thinking": {},
        "chat_buddy": {}
    });
    let response = handle_v1_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(body.to_string()),
    )
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let saved = ProviderDefaults::load(dir.path()).await.unwrap();
    assert_eq!(saved.chat.model.as_deref(), Some("openai/gpt-4o"));
}

async fn make_project_defaults_app(
    project_dir: Option<&std::path::Path>,
) -> (tempfile::TempDir, crate::app_state::AppState) {
    let dir = tempfile::tempdir().unwrap();
    let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
        std::env::temp_dir().join(format!("refact-test-{}", uuid::Uuid::new_v4())),
        dir.path().to_path_buf(),
    )
    .await;
    if let Some(project_dir) = project_dir {
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![project_dir.to_path_buf()];
    }
    let app = crate::app_state::AppState::from_gcx(gcx).await;
    (dir, app)
}

async fn read_project_defaults_body(response: hyper::Response<hyper::Body>) -> serde_json::Value {
    let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn project_defaults_update_writes_file_and_get_returns_it() {
    use crate::providers::http::{handle_v1_project_defaults_get, handle_v1_project_defaults_update};
    use hyper::body::Bytes;
    use hyper::StatusCode;

    let project = tempfile::tempdir().unwrap();
    let (_config, app) = make_project_defaults_app(Some(project.path())).await;

    let body = serde_json::json!({
        "chat": { "model": "openai/gpt-4o" },
        "chat_light": { "model": "openai/gpt-4o-mini" }
    });
    let response = handle_v1_project_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(body.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let saved_path = project.path().join(".refact").join("model_defaults.yaml");
    assert!(saved_path.exists());

    let response = handle_v1_project_defaults_get(axum::extract::State(app.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_project_defaults_body(response).await;
    assert_eq!(payload["project_available"], serde_json::json!(true));
    assert_eq!(
        payload["project_root"].as_str(),
        Some(
            dunce::simplified(&std::fs::canonicalize(project.path()).unwrap())
                .to_string_lossy()
                .as_ref()
        )
    );
    assert_eq!(
        payload["path"].as_str(),
        Some(
            dunce::simplified(&std::fs::canonicalize(&saved_path).unwrap())
                .to_string_lossy()
                .as_ref()
        )
    );
    assert_eq!(
        payload["defaults"]["chat"]["model"].as_str(),
        Some("openai/gpt-4o")
    );
    assert_eq!(
        payload["defaults"]["chat_light"]["model"].as_str(),
        Some("openai/gpt-4o-mini")
    );
}

#[tokio::test]
async fn project_defaults_update_with_empty_defaults_removes_file() {
    use crate::providers::http::{handle_v1_project_defaults_get, handle_v1_project_defaults_update};
    use hyper::body::Bytes;
    use hyper::StatusCode;

    let project = tempfile::tempdir().unwrap();
    let (_config, app) = make_project_defaults_app(Some(project.path())).await;
    let saved_path = project.path().join(".refact").join("model_defaults.yaml");

    handle_v1_project_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(serde_json::json!({"chat": {"model": "openai/gpt-4o"}}).to_string()),
    )
    .await
    .unwrap();
    assert!(saved_path.exists());

    let response = handle_v1_project_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(serde_json::json!({}).to_string()),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!saved_path.exists());

    let response = handle_v1_project_defaults_get(axum::extract::State(app.clone()))
        .await
        .unwrap();
    let payload = read_project_defaults_body(response).await;
    assert_eq!(payload["project_available"], serde_json::json!(true));
    assert_eq!(payload["defaults"], serde_json::json!({}));
}

#[tokio::test]
async fn project_defaults_update_drops_slot_without_model() {
    use crate::providers::http::handle_v1_project_defaults_get;
    use crate::providers::http::handle_v1_project_defaults_update;
    use hyper::body::Bytes;

    let project = tempfile::tempdir().unwrap();
    let (_config, app) = make_project_defaults_app(Some(project.path())).await;

    let body = serde_json::json!({
        "chat": { "model": "openai/gpt-4o" },
        "chat_thinking": { "temperature": 0.4 },
        "chat_buddy": { "model": "  " }
    });
    handle_v1_project_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(body.to_string()),
    )
    .await
    .unwrap();

    let response = handle_v1_project_defaults_get(axum::extract::State(app.clone()))
        .await
        .unwrap();
    let payload = read_project_defaults_body(response).await;
    assert_eq!(
        payload["defaults"]["chat"]["model"].as_str(),
        Some("openai/gpt-4o")
    );
    assert!(payload["defaults"].get("chat_thinking").is_none());
    assert!(payload["defaults"].get("chat_buddy").is_none());
}

#[tokio::test]
async fn project_defaults_get_without_workspace_reports_unavailable() {
    use crate::providers::http::handle_v1_project_defaults_get;
    use hyper::StatusCode;

    let (_config, app) = make_project_defaults_app(None).await;

    let response = handle_v1_project_defaults_get(axum::extract::State(app.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = read_project_defaults_body(response).await;
    assert_eq!(payload["project_available"], serde_json::json!(false));
    assert!(payload["project_root"].is_null());
    assert!(payload["path"].is_null());
    assert_eq!(payload["defaults"], serde_json::json!({}));
}

#[tokio::test]
async fn project_defaults_update_with_invalid_json_returns_422() {
    use crate::providers::http::handle_v1_project_defaults_update;
    use hyper::body::Bytes;
    use hyper::StatusCode;

    let project = tempfile::tempdir().unwrap();
    let (_config, app) = make_project_defaults_app(Some(project.path())).await;

    let err = handle_v1_project_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from("{not json"),
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!project
        .path()
        .join(".refact")
        .join("model_defaults.yaml")
        .exists());
}

#[tokio::test]
async fn project_defaults_update_without_workspace_returns_bad_request() {
    use crate::providers::http::handle_v1_project_defaults_update;
    use hyper::body::Bytes;
    use hyper::StatusCode;

    let (_config, app) = make_project_defaults_app(None).await;

    let err = handle_v1_project_defaults_update(
        axum::extract::State(app.clone()),
        Bytes::from(serde_json::json!({"chat": {"model": "openai/gpt-4o"}}).to_string()),
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
}

#[test]
fn accepted_opportunity_does_not_become_expired() {
    use super::opportunities::OpportunityQueue;
    let now = chrono::Utc::now();
    let mut q = OpportunityQueue::new();
    let mut opp = make_opportunity("opp-accepted-expire", "ck-accepted-expire");
    opp.created_at = now - Duration::hours(25);
    opp.expires_at = now - Duration::hours(24);
    push_opportunity(&mut q, opp);
    q.mark_status("opp-accepted-expire", OpportunityStatus::Accepted);
    q.expire_old(now);
    assert_eq!(
        q.get("opp-accepted-expire").map(|o| o.status),
        Some(OpportunityStatus::Accepted)
    );
}

#[test]
fn accepted_opportunity_has_resolved_at() {
    use super::opportunities::OpportunityQueue;
    let mut q = OpportunityQueue::new();
    push_opportunity(
        &mut q,
        make_opportunity("opp-accepted-resolved", "ck-accepted-resolved"),
    );
    q.mark_status("opp-accepted-resolved", OpportunityStatus::Accepted);
    assert!(q
        .get("opp-accepted-resolved")
        .and_then(|o| o.resolved_at)
        .is_some());
}

#[tokio::test]
async fn accept_route_terminal_status_returns_409() {
    use axum::extract::Path;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, AcceptRequest,
    };
    use hyper::StatusCode;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    let mut opp = make_opportunity("opp-terminal-accept", "ck-terminal-accept");
    opp.status = OpportunityStatus::Dismissed;
    opp.resolved_at = Some(chrono::Utc::now());
    opp.proposed_actions = vec![BuddyAction::OpenPage {
        page: BuddyPage::Buddy,
    }];
    push_opportunity(&mut svc.opportunity_queue, opp);
    *app.buddy.buddy.lock().await = Some(svc);

    let err = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-terminal-accept".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_code, StatusCode::CONFLICT);
}

#[tokio::test]
async fn accept_after_dismiss_returns_409() {
    use axum::extract::Path;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, handle_v1_buddy_opportunity_dismiss, AcceptRequest,
    };
    use hyper::StatusCode;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    let mut opp = make_opportunity("opp-dismiss-then-accept", "ck-dismiss-then-accept");
    opp.proposed_actions = vec![BuddyAction::OpenPage {
        page: BuddyPage::Buddy,
    }];
    svc.add_opportunity(opp);
    *app.buddy.buddy.lock().await = Some(svc);

    let _ = handle_v1_buddy_opportunity_dismiss(
        axum::extract::State(app.clone()),
        Path("opp-dismiss-then-accept".to_string()),
        None,
    )
    .await
    .unwrap();
    let accept_err = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-dismiss-then-accept".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap_err();
    assert_eq!(accept_err.status_code, StatusCode::CONFLICT);
    let dismiss_err = handle_v1_buddy_opportunity_dismiss(
        axum::extract::State(app.clone()),
        Path("opp-dismiss-then-accept".to_string()),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(dismiss_err.status_code, StatusCode::CONFLICT);
}

#[tokio::test]
async fn expired_opportunity_cannot_be_accepted() {
    use axum::extract::Path;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, AcceptRequest,
    };
    use hyper::StatusCode;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    let mut opp = make_opportunity("opp-expired-accept", "ck-expired-accept");
    opp.status = OpportunityStatus::Expired;
    opp.resolved_at = Some(chrono::Utc::now());
    opp.proposed_actions = vec![BuddyAction::OpenPage {
        page: BuddyPage::Buddy,
    }];
    push_opportunity(&mut svc.opportunity_queue, opp);
    *app.buddy.buddy.lock().await = Some(svc);

    let err = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-expired-accept".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_code, StatusCode::CONFLICT);
}

#[test]
fn resolve_opportunity_missing_id_returns_false() {
    let mut svc = make_service();
    assert!(!svc.resolve_opportunity("missing-opp", OpportunityStatus::Accepted));
}

#[tokio::test]
async fn resolve_opportunity_missing_id_emits_no_event() {
    let (tx, mut rx) = broadcast::channel(16);
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    assert!(!svc.resolve_opportunity("missing-opp", OpportunityStatus::Accepted));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn concurrent_accepts_only_one_succeeds() {
    use axum::extract::Path;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, AcceptRequest,
    };
    use hyper::StatusCode;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    let mut opp = make_opportunity("opp-concurrent-accept", "ck-concurrent-accept");
    opp.proposed_actions = vec![BuddyAction::OpenPage {
        page: BuddyPage::Buddy,
    }];
    svc.add_opportunity(opp);
    *app.buddy.buddy.lock().await = Some(svc);

    let app1 = app.clone();
    let app2 = app.clone();
    let task1 = tokio::spawn(async move {
        match handle_v1_buddy_opportunity_accept(
            axum::extract::State(app1.clone()),
            Path("opp-concurrent-accept".to_string()),
            Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
        )
        .await
        {
            Ok(_) => StatusCode::OK,
            Err(err) => err.status_code,
        }
    });
    let task2 = tokio::spawn(async move {
        match handle_v1_buddy_opportunity_accept(
            axum::extract::State(app2.clone()),
            Path("opp-concurrent-accept".to_string()),
            Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
        )
        .await
        {
            Ok(_) => StatusCode::OK,
            Err(err) => err.status_code,
        }
    });
    let (status1, status2) = tokio::join!(task1, task2);
    let mut statuses = vec![status1.unwrap(), status2.unwrap()];
    statuses.sort_by_key(|s| s.as_u16());
    assert_eq!(statuses, vec![StatusCode::OK, StatusCode::CONFLICT]);
}

#[tokio::test]
async fn dismiss_action_through_accept_route_results_in_dismissed_not_accepted() {
    use axum::extract::Path;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, AcceptRequest,
    };

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    let mut opp = make_opportunity("opp-accept-dismiss-action", "ck-accept-dismiss-action");
    opp.proposed_actions = vec![BuddyAction::Dismiss];
    svc.add_opportunity(opp);
    *app.buddy.buddy.lock().await = Some(svc);

    let _ = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-accept-dismiss-action".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap();

    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock.as_ref().unwrap();
    let opp = svc
        .opportunity_queue
        .get("opp-accept-dismiss-action")
        .unwrap();
    assert_eq!(opp.status, OpportunityStatus::Dismissed);
    assert!(svc
        .opportunity_queue
        .recently_dismissed("ck-accept-dismiss-action", Duration::hours(24)));
}

#[tokio::test]
async fn accept_route_with_action_index_1_returns_second_action_without_navigation_event() {
    use axum::extract::Path;
    use crate::buddy::events::BuddyEvent;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, AcceptRequest,
    };

    let (tx, mut rx) = broadcast::channel(16);
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    let mut opp = make_opportunity("opp-action-index", "ck-action-index");
    opp.proposed_actions = vec![
        BuddyAction::OpenPage {
            page: BuddyPage::Buddy,
        },
        BuddyAction::OpenPage {
            page: BuddyPage::Stats,
        },
    ];
    svc.add_opportunity(opp);
    while rx.try_recv().is_ok() {}

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    *app.buddy.buddy.lock().await = Some(svc);

    let response = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-action-index".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 1 })),
    )
    .await
    .unwrap();

    assert_eq!(response.0["action_result"]["kind"], "open_page");
    assert_eq!(response.0["action_result"]["navigate_to"]["type"], "stats");

    let mut navigation_events = 0;
    while let Ok(event) = rx.try_recv() {
        if let BuddyEvent::NavigationRequest { .. } = event {
            navigation_events += 1;
        }
    }
    assert_eq!(navigation_events, 0);
}

#[tokio::test]
async fn failed_dispatch_leaves_opportunity_retryable_and_clears_claim() {
    use axum::extract::Path;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, AcceptRequest,
    };
    use hyper::StatusCode;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    let mut opp = make_opportunity("opp-dispatch-fails", "ck-dispatch-fails");
    opp.proposed_actions = vec![BuddyAction::DraftCustomizationChange {
        customization_kind: CustomizationKind::Mode,
        id: "mode-dispatch-fails".to_string(),
        patch: serde_json::json!("not-an-object"),
    }];
    svc.add_opportunity(opp);
    *app.buddy.buddy.lock().await = Some(svc);

    let err = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-dispatch-fails".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_code, StatusCode::UNPROCESSABLE_ENTITY);

    {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        let svc = lock.as_ref().unwrap();
        let opp = svc.opportunity_queue.get("opp-dispatch-fails").unwrap();
        assert_eq!(opp.status, OpportunityStatus::New);
        assert!(!svc.is_opportunity_accept_claimed("opp-dispatch-fails"));
    }

    let err = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-dispatch-fails".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_code, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn failed_marketplace_install_leaves_opportunity_retryable() {
    use axum::extract::Path;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, AcceptRequest,
    };
    use hyper::StatusCode;

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    let mut opp = make_opportunity("opp-marketplace-fails", "ck-marketplace-fails");
    opp.proposed_actions = vec![BuddyAction::OfferMarketplaceInstall {
        market_kind: MarketKind::Mcp,
        item_id: "../evil".to_string(),
    }];
    svc.add_opportunity(opp);
    *app.buddy.buddy.lock().await = Some(svc);

    let err = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-marketplace-fails".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_code, StatusCode::BAD_GATEWAY);
    assert!(err.message.contains("marketplace_install_failed"));

    {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        let svc = lock.as_ref().unwrap();
        let opp = svc.opportunity_queue.get("opp-marketplace-fails").unwrap();
        assert_eq!(opp.status, OpportunityStatus::New);
        assert!(!svc.is_opportunity_accept_claimed("opp-marketplace-fails"));
    }

    let err = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-marketplace-fails".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status_code, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn successful_marketplace_install_accepts_opportunity() {
    use axum::extract::Path;
    use crate::http::routers::v1::buddy_opportunities::{
        handle_v1_buddy_opportunity_accept, AcceptRequest,
    };

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    let mut opp = make_opportunity("opp-marketplace-ok", "ck-marketplace-ok");
    opp.proposed_actions = vec![BuddyAction::OfferMarketplaceInstall {
        market_kind: MarketKind::Mcp,
        item_id: "github".to_string(),
    }];
    svc.add_opportunity(opp);
    *app.buddy.buddy.lock().await = Some(svc);

    let response = handle_v1_buddy_opportunity_accept(
        axum::extract::State(app.clone()),
        Path("opp-marketplace-ok".to_string()),
        Some(axum::extract::Json(AcceptRequest { action_index: 0 })),
    )
    .await
    .unwrap();
    assert_eq!(response.0["action_result"]["kind"], "marketplace_install");
    assert_eq!(response.0["action_result"]["success"], true);

    let buddy_arc = app.buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock.as_ref().unwrap();
    let opp = svc.opportunity_queue.get("opp-marketplace-ok").unwrap();
    assert_eq!(opp.status, OpportunityStatus::Accepted);
}

#[tokio::test]
async fn opportunity_expiry_persists_and_noops_when_unchanged() {
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
    let mut opp = make_opportunity("opp-expiry-persist", "ck-expiry-persist");
    opp.expires_at = chrono::Utc::now() - Duration::seconds(1);
    svc.add_opportunity(opp);
    svc.dirty = false;

    svc.expire_opportunities();
    assert!(svc.dirty);
    super::state::save_state(root, &svc.state).await.unwrap();
    let loaded = super::state::load_state(root).await;
    assert_eq!(
        loaded
            .opportunities
            .iter()
            .find(|o| o.id == "opp-expiry-persist")
            .map(|o| o.status),
        Some(OpportunityStatus::Expired)
    );

    svc.dirty = false;
    svc.expire_opportunities();
    assert!(!svc.dirty);
}

#[test]
fn batch_surface_honors_max_unread() {
    use super::opportunities::MAX_UNREAD;
    let mut svc = make_service();
    let now = chrono::Utc::now();
    for i in 0..(MAX_UNREAD + 2) {
        let mut fact = make_fact(&format!("task-stuck-{}", i), BuddyFactKind::TaskStuck, now);
        fact.payload = serde_json::json!({ "task_id": format!("task-{}", i) });
        svc.fact_store.ingest(fact);
    }

    svc.detect_and_surface();
    assert_eq!(svc.opportunity_queue.unread_count(), MAX_UNREAD);
}

#[test]
fn humor_delayed_opportunity_is_rechecked_before_add() {
    use super::opportunities::MAX_UNREAD;
    let mut svc = make_service();
    for i in 0..MAX_UNREAD {
        svc.add_opportunity(make_opportunity(
            &format!("pre-humor-{}", i),
            &format!("ck-pre-humor-{}", i),
        ));
    }
    let mut opp = make_opportunity("opp-humor-delayed", "ck-humor-delayed");
    opp.humor_allowed = true;
    opp.humor = Some("joke".to_string());

    assert!(!svc.surface_opportunity_with_cooldown(opp, 1800));
    assert!(svc.opportunity_queue.get("opp-humor-delayed").is_none());
    assert_eq!(svc.opportunity_queue.unread_count(), MAX_UNREAD);
}

#[test]
fn dismissed_history_prunes_old_entries() {
    use super::opportunities::{OpportunityQueue, DISMISS_MEMORY};
    let now = chrono::Utc::now();
    let mut q = OpportunityQueue::new();
    q.dismissed_history.insert(
        "old".to_string(),
        now - DISMISS_MEMORY - Duration::seconds(1),
    );
    q.dismissed_history.insert("fresh".to_string(), now);

    assert!(q.expire_old(now));
    assert!(!q.dismissed_history.contains_key("old"));
    assert!(q.dismissed_history.contains_key("fresh"));
}

#[test]
fn from_state_caps_oversized_opportunities() {
    use super::opportunities::{OpportunityQueue, MAX_OPPORTUNITIES};
    let now = chrono::Utc::now();
    let mut opps = vec![];
    for i in 0..(MAX_OPPORTUNITIES + 25) {
        let mut opp = make_opportunity(&format!("opp-cap-{}", i), &format!("ck-cap-{}", i));
        opp.created_at = now - Duration::minutes(i as i64);
        opps.push(opp);
    }

    let queue = OpportunityQueue::from_state(opps, vec![]);
    assert_eq!(queue.snapshot().len(), MAX_OPPORTUNITIES);
}
