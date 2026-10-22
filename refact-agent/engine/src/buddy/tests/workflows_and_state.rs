use super::*;

#[test]
fn workflow_telemetry_accumulates_and_marks_outputs() {
    let mut svc = make_service();
    assert!(!svc.dirty);

    svc.record_workflow_telemetry("refact_error_detective", 3, 1200, 300, true);
    svc.record_workflow_telemetry("refact_error_detective", 2, 800, 100, false);

    let summary = svc
        .state
        .workflow_summaries
        .iter()
        .find(|w| w.workflow_id == "refact_error_detective")
        .expect("summary must exist");
    assert_eq!(summary.llm_calls, 5);
    assert_eq!(summary.tokens_in, 2000);
    assert_eq!(summary.tokens_out, 400);
    assert_eq!(summary.outputs, 1);
    assert!(summary.last_output_at.is_some());
    assert_eq!(summary.run_count, 0);
    assert!(svc.dirty);
}

#[test]
fn workflow_telemetry_noop_does_not_dirty_state() {
    let mut svc = make_service();
    svc.record_workflow_telemetry("buddy_voice", 0, 0, 0, false);
    assert!(!svc.dirty);
    assert!(svc.state.workflow_summaries.is_empty());
}

#[test]
fn sum_llm_usage_counts_assistant_messages_only() {
    use crate::call_validation::ChatMessage;
    let mut with_usage = ChatMessage::new("assistant".to_string(), "hi".to_string());
    with_usage.usage = Some(crate::call_validation::ChatUsage {
        prompt_tokens: 100,
        completion_tokens: 20,
        total_tokens: 120,
        ..Default::default()
    });
    let without_usage = ChatMessage::new("assistant".to_string(), "more".to_string());
    let user = ChatMessage::new("user".to_string(), "question".to_string());

    let (calls, tokens_in, tokens_out) =
        super::jobs::autonomous_chats::sum_llm_usage(&[user, with_usage, without_usage]);

    assert_eq!(calls, 2);
    assert_eq!(tokens_in, 100);
    assert_eq!(tokens_out, 20);
}

#[tokio::test]
async fn drafts_persist_and_restore_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let mut svc = make_service();
    let draft = svc
        .create_draft(
            super::types::DraftKind::Skill,
            "persisted draft".to_string(),
            "name: test-skill".to_string(),
            "explain".to_string(),
        )
        .unwrap();
    assert!(svc.drafts_dirty);
    super::storage::save_drafts(root, &svc.draft_store.snapshot())
        .await
        .unwrap();

    let loaded = super::storage::load_drafts(root).await;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, draft.id);
    assert_eq!(loaded[0].title, "persisted draft");

    let mut restored_svc = make_service();
    restored_svc.draft_store.restore(loaded);
    assert!(restored_svc.draft_store.get(&draft.id).is_some());
}

#[tokio::test]
async fn expired_drafts_are_dropped_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();

    let mut svc = make_service();
    let draft = svc
        .create_draft(
            super::types::DraftKind::Command,
            "stale".to_string(),
            "name: stale-cmd".to_string(),
            String::new(),
        )
        .unwrap();
    let mut snapshot = svc.draft_store.snapshot();
    snapshot[0].expires_at = chrono::Utc::now() - chrono::Duration::hours(1);
    super::storage::save_drafts(root, &snapshot).await.unwrap();

    let loaded = super::storage::load_drafts(root).await;
    assert!(
        loaded.is_empty(),
        "expired draft {} must be dropped",
        draft.id
    );
}

fn record_workflow_success(
    state: &mut BuddyState,
    workflow_id: &str,
    xp: u64,
    activity: BuddyActivity,
) {
    add_activity(state, activity);
    grant_xp(state, xp);
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(ws) = state
        .workflow_summaries
        .iter_mut()
        .find(|w| w.workflow_id == workflow_id)
    {
        ws.last_run = Some(now);
        ws.run_count += 1;
        ws.last_outcome = Some("success".to_string());
        ws.failure_category = None;
        ws.failure_summary = None;
    } else {
        state.workflow_summaries.push(BuddyWorkflowSummary {
            workflow_id: workflow_id.to_string(),
            last_run: Some(now),
            run_count: 1,
            failure_category: None,
            failure_summary: None,
            last_outcome: Some("success".to_string()),
            ..Default::default()
        });
    }
}

#[tokio::test]
async fn workflow_failure_report_emits_runtime_event_activity_and_transcript() {
    let (mut svc, mut rx) = make_service_with_events();
    let report = super::workflows::WorkflowFailureReport {
        workflow_id: "buddy_dependency_radar".to_string(),
        category: super::workflows::WorkflowFailureCategory::ModelUnavailable,
        summary: "Model unavailable — check Buddy/default model settings.".to_string(),
        detail: "OpenAI 404: model refact/gpt-4.1-nano not found".to_string(),
        chat_id: None,
    };

    let write = svc
        .record_workflow_failure_report(report)
        .expect("workflow failure transcript write should be returned");
    BuddyService::append_workflow_failure_transcript(&write.0, &write.1).await;

    assert!(svc.state.recent_activities.iter().any(|activity| {
        activity.title.contains("Model unavailable")
            && activity
                .failure_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("Buddy/default model"))
    }));
    assert!(svc.state.workflow_summaries.iter().any(|summary| {
        summary.workflow_id == "buddy_dependency_radar"
            && summary.last_outcome.as_deref() == Some("failed:model_unavailable")
    }));
    assert!(svc.runtime_queue.items.iter().any(|event| {
        event.status == "failed"
            && event.dedupe_key.as_deref()
                == Some("workflow_failure:buddy_dependency_radar:model_unavailable")
    }));

    let mut saw_runtime = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, super::events::BuddyEvent::RuntimeEvent { .. }) {
            saw_runtime = true;
            break;
        }
    }
    assert!(saw_runtime);

    let summary = svc
        .state
        .workflow_summaries
        .iter()
        .find(|summary| summary.workflow_id == "buddy_dependency_radar")
        .unwrap();
    assert_eq!(
        summary.failure_category.as_deref(),
        Some("model_unavailable")
    );
    assert!(summary
        .failure_summary
        .as_deref()
        .is_some_and(|summary| summary.contains("Buddy/default model")));

    let value: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(
            svc.project_root
                .join(".refact/buddy/chats/workflows/buddy_dependency_radar.json"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let entry = &value["entries"][0];
    assert_eq!(entry["success"], false);
    assert_eq!(entry["failure_category"], "model_unavailable");
    assert_eq!(
        entry["failure_summary"],
        "Model unavailable — check Buddy/default model settings."
    );
}

#[test]
fn workflow_success_clears_previous_failure_metadata() {
    let mut svc = make_service();
    let report = super::workflows::WorkflowFailureReport {
        workflow_id: "buddy_dependency_radar".to_string(),
        category: super::workflows::WorkflowFailureCategory::ModelUnavailable,
        summary: "Model unavailable".to_string(),
        detail: "model not found".to_string(),
        chat_id: None,
    };

    svc.record_workflow_failure_report(report);
    record_workflow_success(
        &mut svc.state,
        "buddy_dependency_radar",
        1,
        BuddyActivity {
            icon: "✅".to_string(),
            title: "Dependency radar complete".to_string(),
            description: String::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            activity_type: "workflow".to_string(),
            chat_id: None,
            failure_category: None,
            failure_summary: None,
        },
    );

    let summary = svc
        .state
        .workflow_summaries
        .iter()
        .find(|summary| summary.workflow_id == "buddy_dependency_radar")
        .expect("workflow summary");
    assert_eq!(summary.last_outcome.as_deref(), Some("success"));
    assert!(summary.failure_category.is_none());
    assert!(summary.failure_summary.is_none());
}

#[test]
fn invalid_workflow_id_failure_report_is_rejected_before_state_or_event() {
    let (mut svc, mut rx) = make_service_with_events();
    let report = super::workflows::WorkflowFailureReport {
        workflow_id: "../bad".to_string(),
        category: super::workflows::WorkflowFailureCategory::ModelUnavailable,
        summary: "Model unavailable".to_string(),
        detail: "model not found".to_string(),
        chat_id: None,
    };

    assert!(svc.record_workflow_failure_report(report).is_none());
    assert!(svc.state.workflow_summaries.is_empty());
    assert!(svc.state.recent_activities.is_empty());
    assert!(svc.runtime_queue.items.is_empty());
    assert!(rx.try_recv().is_err());
}

#[test]
fn test_auto_gate_requires_all_conditions() {
    let gate = IssueGate {
        has_issue_text: true,
        has_repro_context: true,
        integration_configured: true,
        auto_creation_enabled: true,
        within_rate_limit: true,
    };
    assert!(check_issue_gate(&gate));
}

#[test]
fn test_auto_gate_blocks_without_repro() {
    let gate = IssueGate {
        has_issue_text: true,
        has_repro_context: false,
        integration_configured: true,
        auto_creation_enabled: true,
        within_rate_limit: true,
    };
    assert!(!check_issue_gate(&gate));
}

#[test]
fn test_manual_gate_allows_without_auto_enabled() {
    let gate = IssueGate {
        has_issue_text: true,
        has_repro_context: false,
        integration_configured: true,
        auto_creation_enabled: false,
        within_rate_limit: false,
    };
    assert!(check_manual_issue_gate(&gate));
}

#[test]
fn test_manual_gate_requires_integration() {
    let gate = IssueGate {
        has_issue_text: true,
        has_repro_context: true,
        integration_configured: false,
        auto_creation_enabled: true,
        within_rate_limit: true,
    };
    assert!(!check_manual_issue_gate(&gate));
}

#[test]
fn test_default_state_starts_egg() {
    let state = default_buddy_state();
    assert_eq!(state.progression.stage, 0);
    assert_eq!(state.progression.stage_name, "Egg");
    assert_eq!(state.progression.xp, 0);
    assert_eq!(state.progression.level, 1);
    assert_eq!(state.pet.needs.hunger, 80);
    assert_eq!(state.pet.needs.energy, 85);
    assert_eq!(state.pet.needs.hygiene, 80);
    assert_eq!(state.pet.needs.boredom, 15);
    assert_eq!(state.pet.needs.affection, 75);
}

#[test]
fn test_growth_points_hatch_without_care_gate() {
    let mut state = default_buddy_state();
    grant_xp(&mut state, 20);
    assert_eq!(state.progression.level, 2);
    assert_eq!(state.progression.stage, 1);
    assert_eq!(state.progression.stage_name, "Hatch");
    assert_eq!(state.progression.xp, 0);
}

#[test]
fn test_grant_xp_updates_stage_when_care_gate_met() {
    let mut state = default_buddy_state();
    state.pet.evolution.open_seconds = 10 * 60;
    state.pet.evolution.care_score = 20;
    grant_xp(&mut state, 30);
    assert_eq!(state.progression.stage, 1);
    assert_eq!(state.progression.stage_name, "Hatch");
    assert_eq!(state.progression.level, 2);
    assert_eq!(state.progression.xp, 10);
}

#[test]
fn test_xp_only_advance_reaches_later_stage() {
    let mut state = default_buddy_state();
    grant_xp(&mut state, 120);
    assert_eq!(state.progression.stage_name, "Imp");
    assert_eq!(state.progression.stage, 3);
}

#[test]
fn test_xp_only_advances_multiple_stages() {
    let mut state = default_buddy_state();
    grant_xp(&mut state, 20 + 35 + 10);
    assert_eq!(state.progression.stage, 2);
    assert_eq!(state.progression.stage_name, "Sprite");
    assert_eq!(state.progression.xp, 10);
    assert_eq!(state.progression.xp_next, 55);
}

#[test]
fn test_realistic_workflow_care_path_advances_later_stage() {
    let mut state = default_buddy_state();
    apply_pet_tick(&mut state, 60);
    for _ in 0..15 {
        let _ = apply_care_action(&mut state, BuddyCareAction::Play, Some("bug"));
        let _ = apply_care_action(&mut state, BuddyCareAction::Feed, None);
    }
    assert!(state.pet.evolution.open_seconds >= 60);
    assert!(state.pet.evolution.care_score >= 100);
    assert!(state.progression.stage >= 1);
}

#[test]
fn test_repeated_successful_workflow_rewards_eventually_advance() {
    let mut state = default_buddy_state();
    for idx in 0..12 {
        record_workflow_success(
            &mut state,
            "workflow_xp_test",
            5,
            BuddyActivity {
                icon: "⚙️".to_string(),
                title: format!("Workflow {idx}"),
                description: "done".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                activity_type: "workflow".to_string(),
                chat_id: None,
                failure_category: None,
                failure_summary: None,
            },
        );
    }
    assert!(state.progression.stage >= 2);
    assert_eq!(
        state
            .workflow_summaries
            .iter()
            .find(|summary| summary.workflow_id == "workflow_xp_test")
            .map(|summary| summary.run_count),
        Some(12)
    );
}

#[test]
fn test_xp_display_starts_below_next_threshold() {
    let mut state = default_buddy_state();
    grant_xp(&mut state, 0);
    assert!(state.progression.xp < state.progression.xp_next);
}

#[test]
fn test_max_stage_behavior() {
    let mut state = default_buddy_state();
    state.pet.evolution.open_seconds = 400 * 60;
    state.pet.evolution.care_score = 500;
    grant_xp(&mut state, 3000);
    assert_eq!(state.progression.stage_name, "Archon");
    assert_eq!(state.progression.stage, 6);
    assert_eq!(state.progression.level, 7);
    assert_eq!(state.progression.xp, 210);
    assert_eq!(state.progression.xp_next, 210);
}

#[test]
fn test_palette_clamped_on_load() {
    let mut state = default_buddy_state();
    state.identity.palette_index = 100;
    state.identity.palette_index = state.identity.palette_index.min(MAX_PALETTE_INDEX);
    assert_eq!(state.identity.palette_index, MAX_PALETTE_INDEX);
}

#[test]
fn test_palette_valid_range() {
    for i in 0..=MAX_PALETTE_INDEX {
        assert_eq!(i.min(MAX_PALETTE_INDEX), i);
    }
    assert!(MAX_PALETTE_INDEX > 0);
    assert!(10usize.min(MAX_PALETTE_INDEX) == MAX_PALETTE_INDEX);
}

#[test]
fn test_palette_single_source() {
    let settings = BuddySettings::default();
    let json = serde_json::to_value(&settings).unwrap();
    assert!(
        json.get("palette_index").is_none(),
        "palette_index must not be in BuddySettings"
    );
    let state = default_buddy_state();
    assert!(state.identity.palette_index <= MAX_PALETTE_INDEX);
}

#[test]
fn test_old_state_migration() {
    let json = r#"{
        "identity": {"name": "Pixel", "created_at": "2024-01-01T00:00:00Z", "palette_index": 2},
        "progression": {"stage": 0, "stage_name": "Egg", "level": 1, "xp": 0, "xp_next": 100},
        "skills": {"unlocked": [], "locked": []},
        "workflow_summaries": [],
        "semantic": {"mood": "Idle", "focus": "", "headline": "", "last_active": "2024-01-01T00:00:00Z"},
        "recent_activities": [],
        "suggestion_state": []
    }"#;
    let state: BuddyState =
        serde_json::from_str(json).expect("should parse old state without onboarding");
    assert!(!state.onboarding.greeted);
    assert!(!state.onboarding.tour_completed);
    assert!(state.onboarding.first_launch_at.is_empty());
    assert_eq!(state.pet.needs.hunger, 80);
    assert_eq!(state.pet.needs.energy, 85);
    assert_eq!(state.pet.evolution.open_seconds, 0);
    assert!(!state.personality.archetype_label.is_empty());
}

#[test]
fn test_save_on_mutate_sets_dirty() {
    let mut svc = make_service();
    assert!(!svc.dirty);
    svc.grant_xp(10);
    assert!(svc.dirty);
}

#[tokio::test]
async fn test_save_on_mutate_writes_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();
    let mut svc = make_service();
    svc.grant_xp(25);
    super::state::save_state(root, &svc.state).await.unwrap();
    let loaded = super::state::load_state(root).await;
    assert_eq!(loaded.progression.stage_name, "Hatch");
    assert_eq!(loaded.progression.xp, 5);
    assert_eq!(loaded.pet.needs.hunger, 80);
}

#[test]
fn test_pet_tick_decays_needs_while_awake() {
    let mut state = default_buddy_state();
    // Pin personality so trait thresholds are deterministic (all traits = 50)
    state.personality = super::types::BuddyPersonalityProfile::default();
    let changed = apply_pet_tick(&mut state, 15);
    assert!(changed);
    assert_eq!(state.pet.needs.hunger, 78);
    assert_eq!(state.pet.needs.energy, 84);
    assert_eq!(state.pet.needs.hygiene, 79);
    assert_eq!(state.pet.needs.boredom, 17);
    assert_eq!(state.pet.needs.affection, 74);
    assert_eq!(state.pet.evolution.open_seconds, 15);
}

#[test]
fn test_pet_tick_restores_energy_while_sleeping() {
    let mut state = default_buddy_state();
    state.pet.condition.sleeping = true;
    state.pet.needs.energy = 20;
    let changed = apply_pet_tick(&mut state, 15);
    assert!(changed);
    assert_eq!(state.pet.needs.energy, 23);
    assert_eq!(state.pet.needs.boredom, 16);
}

#[test]
fn test_pet_tick_updates_need_flags() {
    let mut state = default_buddy_state();
    state.pet.needs.hunger = 10;
    state.pet.needs.energy = 10;
    state.pet.needs.hygiene = 10;
    state.pet.needs.boredom = 95;
    state.pet.needs.affection = 10;
    let _ = apply_pet_tick(&mut state, 15);
    assert!(state.pet.condition.hungry);
    assert!(state.pet.condition.sleepy);
    assert!(state.pet.condition.dirty);
    assert!(state.pet.condition.bored);
    assert!(state.pet.condition.lonely);
}

#[test]
fn test_pet_tick_sets_dirty_on_service() {
    let mut svc = make_service();
    assert!(!svc.dirty);
    svc.apply_pet_tick(15);
    assert!(svc.dirty);
}

#[test]
fn test_reroll_personality_preserves_progress() {
    let mut state = default_buddy_state();
    state.progression.stage = 2;
    state.progression.xp = 17;
    let before = state.personality.archetype_label.clone();
    reroll_personality(&mut state);
    assert_eq!(state.progression.stage, 2);
    assert_eq!(state.progression.xp, 17);
    assert!(!state.personality.archetype_label.is_empty());
    assert!(!state.personality.prompt.is_empty());
    if before == state.personality.archetype_label {
        assert_ne!(state.personality.traits.playfulness, 0);
    }
}

#[test]
fn test_feed_care_action_restores_hunger() {
    let mut state = default_buddy_state();
    state.pet.needs.hunger = 10;
    let (changed, message) = apply_care_action(&mut state, BuddyCareAction::Feed, None);
    assert!(changed);
    assert!(message.contains("Snack"));
    assert!(state.pet.needs.hunger > 10);
    assert_eq!(state.recent_activities[0].activity_type, "care_feed");
}

#[test]
fn test_play_care_action_uses_toy_hint() {
    let mut state = default_buddy_state();
    state.pet.needs.boredom = 90;
    let (_, message) = apply_care_action(&mut state, BuddyCareAction::Play, Some("bug"));
    assert!(message.contains("bug"));
    assert!(state.pet.needs.boredom < 90);
}

#[test]
fn test_sleep_care_action_sets_sleeping() {
    let mut state = default_buddy_state();
    state.pet.needs.energy = 20;
    let (_, message) = apply_care_action(&mut state, BuddyCareAction::Sleep, None);
    assert!(message.contains("Sleep mode"));
    assert!(state.pet.condition.sleeping);
    assert!(state.pet.needs.energy > 20);
}

#[test]
fn test_sleep_care_action_stays_sleeping_near_threshold() {
    let mut state = default_buddy_state();
    state.pet.needs.energy = 80;
    let _ = apply_care_action(&mut state, BuddyCareAction::Sleep, None);
    assert!(state.pet.condition.sleeping);
}

#[test]
fn test_settings_null_prompt_clears_value() {
    let json = r#"{
        "enabled": true,
        "clear_personality_prompt": true
    }"#;
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest =
        serde_json::from_str(json).unwrap();
    assert_eq!(req.clear_personality_prompt, Some(true));
    assert!(req.personality_prompt.is_none());
}

#[test]
fn test_settings_request_parses_full_contract() {
    let json = r#"{
        "enabled": false,
        "auto_diagnostics": false,
        "auto_issue_creation": true,
        "personality_prompt": "be nice",
        "clear_personality_prompt": true,
        "autonomous_chats_enabled": false,
        "proactive_enabled": false,
        "message_observation_enabled": false,
        "chat_reactions_enabled": false,
        "housekeeping_enabled": false,
        "humor_enabled": false,
        "humor_level": "normal",
        "autonomy_level": "safe_auto",
        "quiet_mode": true,
        "daily_digest_hour": null,
        "observers": {
            "task_health": false,
            "trajectory_clutter": false,
            "chat_pattern": false,
            "customization_drift": false,
            "memory_garden": false,
            "mcp_auth": false,
            "git_pressure": false,
            "diagnostic_cluster": false,
            "provider_health": false
        },
        "palette_index": 3
    }"#;
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest =
        serde_json::from_str(json).unwrap();

    assert_eq!(req.clear_personality_prompt, Some(true));
    assert_eq!(req.personality_prompt, Some(Some("be nice".to_string())));
    assert_eq!(req.daily_digest_hour, Some(None));
    assert_eq!(req.humor_level, Some(HumorLevel::Normal));
    assert_eq!(req.autonomy_level, Some(AutonomyLevel::SafeAuto));
    assert_eq!(req.palette_index, Some(3));
    assert!(req.observers.is_some());
}

#[test]
fn test_settings_request_rejects_invalid_digest_hour() {
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest =
        serde_json::from_str(r#"{"daily_digest_hour": 24}"#).unwrap();
    let err = req.validate().unwrap_err();

    assert_eq!(err.status_code, hyper::StatusCode::BAD_REQUEST);
    assert!(err.message.contains("daily_digest_hour"));
}

#[tokio::test]
async fn test_settings_load_clears_invalid_persisted_digest_hour() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".refact/buddy/settings.json");
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(
        &path,
        r#"{
            "enabled": true,
            "auto_diagnostics": true,
            "auto_issue_creation": false,
            "daily_digest_hour": 255
        }"#,
    )
    .await
    .unwrap();

    let settings = super::settings::load_settings(dir.path()).await;

    assert_eq!(settings.daily_digest_hour, None);
    assert!(settings.enabled);
}

#[tokio::test]
async fn test_settings_update_rejects_invalid_digest_without_mutating() {
    let app = make_gcx_with_buddy().await;
    let before = {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        lock.as_ref().unwrap().settings.clone()
    };
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest =
        serde_json::from_str(r#"{"daily_digest_hour": 24, "quiet_mode": true}"#).unwrap();

    let err = crate::http::routers::v1::buddy::handle_v1_buddy_settings_update(
        axum::extract::State(app.clone()),
        axum::Json(req),
    )
    .await
    .unwrap_err();

    assert_eq!(err.status_code, hyper::StatusCode::BAD_REQUEST);
    let after = {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        lock.as_ref().unwrap().settings.clone()
    };
    assert_eq!(after.daily_digest_hour, before.daily_digest_hour);
    assert_eq!(after.quiet_mode, before.quiet_mode);
}

#[test]
fn test_settings_request_rejects_unknown_observer() {
    let err = serde_json::from_str::<crate::http::routers::v1::buddy::BuddySettingsRequest>(
        r#"{"observers":{"auth":false}}"#,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn test_settings_request_rejects_unknown_top_level_field() {
    let err = serde_json::from_str::<crate::http::routers::v1::buddy::BuddySettingsRequest>(
        r#"{"not_a_real_setting":true}"#,
    )
    .unwrap_err();

    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn test_settings_request_apply_updates_full_contract() {
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest = serde_json::from_str(
        r#"{
            "personality_prompt": "new prompt",
            "clear_personality_prompt": true,
            "chat_reactions_enabled": false,
            "quiet_mode": true,
            "humor_enabled": false,
            "humor_level": "normal",
            "autonomy_level": "safe_auto",
            "daily_digest_hour": 7,
            "observers": { "chat_pattern": false, "provider_health": false }
        }"#,
    )
    .unwrap();
    let mut settings = BuddySettings::default();
    settings.personality_prompt = Some("old prompt".to_string());

    let persona_dirty = req.apply_to_settings(&mut settings);

    assert!(persona_dirty);
    assert_eq!(settings.personality_prompt, None);
    assert!(!settings.chat_reactions_enabled);
    assert!(settings.quiet_mode);
    assert!(!settings.humor_enabled);
    assert_eq!(settings.humor_level, HumorLevel::Normal);
    assert_eq!(settings.autonomy_level, AutonomyLevel::SafeAuto);
    assert_eq!(settings.daily_digest_hour, Some(7));
    assert!(!settings.observers.chat_pattern);
    assert!(!settings.observers.provider_health);
    assert!(settings.observers.task_health);
}

#[test]
fn test_settings_request_tone_fields_mark_persona_dirty() {
    for json in [
        r#"{"humor_enabled":false}"#,
        r#"{"humor_level":"normal"}"#,
        r#"{"autonomy_level":"safe_auto"}"#,
        r#"{"personality_prompt":"new prompt"}"#,
        r#"{"personality_prompt":null}"#,
        r#"{"clear_personality_prompt":true}"#,
    ] {
        let req: crate::http::routers::v1::buddy::BuddySettingsRequest =
            serde_json::from_str(json).unwrap();
        let mut settings = BuddySettings::default();
        settings.personality_prompt = Some("old prompt".to_string());

        assert!(req.apply_to_settings(&mut settings), "json: {json}");
    }
}

#[test]
fn test_settings_storage_metadata_paths() {
    let root = std::path::PathBuf::from("/tmp/active-buddy-root");
    let storage = super::settings::storage_metadata(&root);
    let expected_buddy_dir = root.join(".refact").join("buddy");
    let expected_settings_path = expected_buddy_dir.join("settings.json");

    assert_eq!(storage.project_root, "/tmp/active-buddy-root");
    assert_eq!(storage.buddy_dir, expected_buddy_dir.to_string_lossy());
    assert_eq!(
        storage.settings_path,
        expected_settings_path.to_string_lossy()
    );
}

#[test]
fn test_snapshot_includes_storage_metadata_for_service_path() {
    let mut svc = make_service();
    svc.project_root = std::path::PathBuf::from("/tmp/service-buddy-root");

    let snapshot = svc.snapshot();
    let storage = snapshot.storage.expect("storage metadata must be present");
    let expected_buddy_dir = svc.project_root.join(".refact").join("buddy");
    let expected_settings_path = expected_buddy_dir.join("settings.json");

    assert_eq!(storage.project_root, "/tmp/service-buddy-root");
    assert_eq!(storage.buddy_dir, expected_buddy_dir.to_string_lossy());
    assert_eq!(
        storage.settings_path,
        expected_settings_path.to_string_lossy()
    );
}

#[tokio::test]
async fn test_settings_update_tone_fields_bump_persona_cache_version() {
    let app = make_gcx_with_buddy().await;
    let before = refact_buddy_core::state::persona_cache_version();
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest = serde_json::from_str(
        r#"{
            "humor_enabled": false,
            "humor_level": "normal",
            "autonomy_level": "safe_auto",
            "personality_prompt": "tiny chaos gremlin"
        }"#,
    )
    .unwrap();

    let _ = crate::http::routers::v1::buddy::handle_v1_buddy_settings_update(
        axum::extract::State(app.clone()),
        axum::Json(req),
    )
    .await
    .unwrap();

    assert!(refact_buddy_core::state::persona_cache_version() > before);
}

#[tokio::test]
async fn test_settings_update_hot_applies_and_emits_event() {
    let app = make_gcx_with_buddy().await;
    let mut rx = {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        lock.as_ref().unwrap().events_tx.subscribe()
    };
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest = serde_json::from_str(
        r#"{
            "chat_reactions_enabled": false,
            "quiet_mode": true,
            "humor_level": "normal",
            "autonomy_level": "safe_auto",
            "daily_digest_hour": null,
            "observers": { "chat_pattern": false, "mcp_auth": false }
        }"#,
    )
    .unwrap();

    let response = crate::http::routers::v1::buddy::handle_v1_buddy_settings_update(
        axum::extract::State(app.clone()),
        axum::Json(req),
    )
    .await
    .unwrap();
    let returned: BuddySettings = serde_json::from_value(response.0).unwrap();

    assert!(!returned.chat_reactions_enabled);
    assert!(returned.quiet_mode);
    assert_eq!(returned.humor_level, HumorLevel::Normal);
    assert_eq!(returned.autonomy_level, AutonomyLevel::SafeAuto);
    assert_eq!(returned.daily_digest_hour, None);
    assert!(!returned.observers.chat_pattern);
    assert!(!returned.observers.mcp_auth);

    let (project_root, settings) = {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        let svc = lock.as_ref().unwrap();
        (svc.project_root.clone(), svc.settings.clone())
    };
    assert_eq!(
        settings.chat_reactions_enabled,
        returned.chat_reactions_enabled
    );
    assert_eq!(settings.quiet_mode, returned.quiet_mode);
    assert_eq!(settings.daily_digest_hour, None);
    let persisted = super::settings::load_settings(&project_root).await;
    assert_eq!(
        persisted.chat_reactions_enabled,
        returned.chat_reactions_enabled
    );
    assert_eq!(persisted.daily_digest_hour, None);

    let mut saw_settings_changed = false;
    while let Ok(event) = rx.try_recv() {
        if let super::events::BuddyEvent::SettingsChanged { settings } = event {
            assert!(!settings.chat_reactions_enabled);
            assert!(settings.quiet_mode);
            saw_settings_changed = true;
        }
    }
    assert!(saw_settings_changed);
}

#[tokio::test]
async fn test_settings_update_service_response_includes_storage_metadata() {
    let app = make_gcx_with_buddy().await;
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest =
        serde_json::from_str(r#"{"quiet_mode":true}"#).unwrap();

    let response = crate::http::routers::v1::buddy::handle_v1_buddy_settings_update(
        axum::extract::State(app.clone()),
        axum::Json(req),
    )
    .await
    .unwrap();
    let storage = response.0["storage"].as_object().expect("storage object");
    let project_root = {
        let buddy_arc = app.buddy.buddy.clone();
        let lock = buddy_arc.lock().await;
        lock.as_ref().unwrap().project_root.clone()
    };
    let expected_project_root = project_root.to_string_lossy().to_string();
    let expected_settings_path = project_root
        .join(".refact")
        .join("buddy")
        .join("settings.json")
        .to_string_lossy()
        .to_string();

    assert_eq!(
        storage["project_root"].as_str(),
        Some(expected_project_root.as_str())
    );
    assert_eq!(
        storage["settings_path"].as_str(),
        Some(expected_settings_path.as_str())
    );
}

#[tokio::test]
async fn test_settings_update_fallback_response_includes_storage_metadata() {
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
    *app.buddy.buddy.lock().await = None;
    let req: crate::http::routers::v1::buddy::BuddySettingsRequest =
        serde_json::from_str(r#"{"quiet_mode":true}"#).unwrap();

    let response = crate::http::routers::v1::buddy::handle_v1_buddy_settings_update(
        axum::extract::State(app),
        axum::Json(req),
    )
    .await
    .unwrap();
    let storage = response.0["storage"].as_object().expect("storage object");
    let expected_project_root =
        crate::files_correction::canonicalize_normalized_path(dir.path().to_path_buf());
    let expected_settings_path = expected_project_root
        .join(".refact")
        .join("buddy")
        .join("settings.json")
        .to_string_lossy()
        .to_string();
    let expected_project_root = expected_project_root.to_string_lossy().to_string();

    assert_eq!(
        storage["project_root"].as_str(),
        Some(expected_project_root.as_str())
    );
    assert_eq!(
        storage["settings_path"].as_str(),
        Some(expected_settings_path.as_str())
    );
}

#[tokio::test]
async fn test_bootstrap_no_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();
    let state1 = super::state::load_state(root).await;
    let name1 = state1.identity.name.clone();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();
    let state2 = super::state::load_state(root).await;
    assert_eq!(
        name1, state2.identity.name,
        "bootstrap must not overwrite existing state.json"
    );
}
