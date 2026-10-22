use super::*;

#[test]
fn fact_store_dedup_by_key() {
    use super::facts::FactStore;
    let mut store = FactStore::new();
    let now = chrono::Utc::now();
    let f1 = BuddyFact {
        payload: serde_json::json!({"v": 1}),
        ..make_fact("k1", BuddyFactKind::TaskStuck, now)
    };
    let f2 = BuddyFact {
        payload: serde_json::json!({"v": 2}),
        ..make_fact("k1", BuddyFactKind::TaskStuck, now)
    };
    store.ingest(f1);
    store.ingest(f2);
    assert_eq!(store.iter().count(), 1);
    assert_eq!(store.iter().next().unwrap().payload["v"], 2);
}

#[test]
fn fact_store_ring_evicts_oldest() {
    use super::facts::{FactStore, FACT_RING_CAPACITY};
    let mut store = FactStore::new();
    let now = chrono::Utc::now();
    for i in 0..=FACT_RING_CAPACITY {
        store.ingest(make_fact(
            &format!("key-{}", i),
            BuddyFactKind::TaskStuck,
            now,
        ));
    }
    assert_eq!(store.iter().count(), FACT_RING_CAPACITY);
    assert!(
        !store.iter().any(|f| f.key == "key-0"),
        "first key must be evicted"
    );
}

#[test]
fn fact_store_count_within() {
    use super::facts::FactStore;
    let now = chrono::Utc::now();
    let mut store = FactStore::new();
    store.ingest(make_fact(
        "old",
        BuddyFactKind::DiagnosticCluster,
        now - Duration::hours(2),
    ));
    store.ingest(make_fact(
        "recent",
        BuddyFactKind::DiagnosticCluster,
        now - Duration::minutes(5),
    ));
    assert_eq!(
        store.count_within(BuddyFactKind::DiagnosticCluster, Duration::hours(1)),
        1
    );
    assert_eq!(
        store.count_within(BuddyFactKind::DiagnosticCluster, Duration::hours(3)),
        2
    );
}

// =============================================================================
// OpportunityQueue tests
// =============================================================================
#[test]
fn opportunity_queue_unread_cap_state() {
    use super::opportunities::OpportunityQueue;
    let mut q = OpportunityQueue::new();
    push_opportunity(&mut q, make_opportunity("opp1", "ck1"));
    assert_eq!(q.unread_count(), 1);
    q.mark_status("opp1", OpportunityStatus::Dismissed);
    assert_eq!(q.unread_count(), 0);
}

#[test]
fn opportunity_queue_cooldown_blocks_dup() {
    use super::opportunities::OpportunityQueue;
    let mut q = OpportunityQueue::new();
    push_opportunity(&mut q, make_opportunity("opp1", "ck1"));
    assert!(q.cooldown_active("ck1"));
}

#[test]
fn opportunity_queue_expire_old() {
    use super::opportunities::OpportunityQueue;
    let now = chrono::Utc::now();
    let mut q = OpportunityQueue::new();
    let mut opp = make_opportunity("opp1", "ck1");
    opp.expires_at = now - Duration::hours(1);
    opp.created_at = now - Duration::minutes(5);
    push_opportunity(&mut q, opp);
    q.expire_old(now);
    assert_eq!(
        q.get("opp1").map(|o| o.status),
        Some(OpportunityStatus::Expired)
    );
    q.expire_old(now + Duration::hours(25));
    assert!(q.get("opp1").is_none(), "must be removed after 24h");
}

#[test]
fn opportunity_queue_cap() {
    use super::opportunities::{OpportunityQueue, MAX_OPPORTUNITIES};
    let mut q = OpportunityQueue::new();
    for i in 0..=MAX_OPPORTUNITIES {
        push_opportunity(
            &mut q,
            make_opportunity(&format!("opp-{}", i), &format!("ck-{}", i)),
        );
    }
    assert!(q.iter().count() <= MAX_OPPORTUNITIES);
}

// =============================================================================
// DraftStore tests
// =============================================================================
#[test]
fn draft_store_create_get_consume() {
    use super::drafts::DraftStore;
    let mut store = DraftStore::new();
    let draft = store.create(
        DraftKind::Skill,
        "My Skill".to_string(),
        "yaml: {}".to_string(),
        "exp".to_string(),
    );
    let id = draft.id.clone();
    assert!(store.get(&id).is_some());
    let consumed = store.consume(&id);
    assert!(consumed.is_some());
    assert!(store.get(&id).is_none());
}

#[test]
fn draft_store_ttl() {
    use super::drafts::DraftStore;
    let mut store = DraftStore::new();
    let draft = store.create(
        DraftKind::Command,
        "Cmd".to_string(),
        "{}".to_string(),
        "".to_string(),
    );
    let id = draft.id.clone();
    store.expire_old(chrono::Utc::now() + Duration::hours(3));
    assert!(store.get(&id).is_none(), "draft must be removed after TTL");
}

// =============================================================================
// PulseBuilder tests
// =============================================================================
#[tokio::test]
async fn pulse_build_skeleton_sets_generated_at() {
    use super::facts::FactStore;
    use super::pulse::build_pulse;
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let store = FactStore::new();
    let pulse = build_pulse(app, std::path::Path::new("/tmp"), &store).await;
    assert!(pulse.generated_at.is_some());
}

#[test]
fn state_migration_loads_old_state_without_opportunities() {
    let json = r#"{
        "identity": {"name": "Pixel", "created_at": "2024-01-01T00:00:00Z", "palette_index": 2},
        "progression": {"stage": 0, "stage_name": "Egg", "level": 1, "xp": 0, "xp_next": 20},
        "skills": {"unlocked": [], "locked": []},
        "workflow_summaries": [],
        "semantic": {"mood": "Idle", "focus": "", "headline": "", "last_active": "2024-01-01T00:00:00Z"},
        "recent_activities": [],
        "suggestion_state": []
    }"#;
    let state: BuddyState =
        serde_json::from_str(json).expect("should parse old state without opportunities");
    assert!(
        state.opportunities.is_empty(),
        "opportunities must default to empty vec"
    );
}

#[test]
fn buddy_action_round_trip() {
    let actions: Vec<BuddyAction> = vec![
        BuddyAction::OpenPage {
            page: BuddyPage::Buddy,
        },
        BuddyAction::LaunchInvestigationChat {
            preload: InvestigationContext {
                fact_keys: vec![],
                diagnostic_ids: vec![],
                memory_key: None,
                log_excerpt: String::new(),
                config_summary: String::new(),
                initial_user_message: "investigate".to_string(),
            },
        },
        BuddyAction::DraftSkill {
            draft_id: "d1".to_string(),
            label: "My Skill".to_string(),
        },
        BuddyAction::DraftCommand {
            draft_id: "d2".to_string(),
            label: "My Command".to_string(),
        },
        BuddyAction::DraftDelegate {
            draft_id: "d3".to_string(),
            label: "My Delegate".to_string(),
        },
        BuddyAction::DraftMode {
            draft_id: "d4".to_string(),
            label: "My Mode".to_string(),
        },
        BuddyAction::DraftAgentsMdPatch {
            content: "--- a\n+++ b".to_string(),
        },
        BuddyAction::DraftDefaultsChange {
            defaults_kind: DefaultsKind::ChatModel,
            patch: serde_json::json!({}),
        },
        BuddyAction::DraftCustomizationChange {
            customization_kind: CustomizationKind::Mode,
            id: "m1".to_string(),
            patch: serde_json::json!({}),
        },
        BuddyAction::OfferMarketplaceInstall {
            market_kind: MarketKind::Mcp,
            item_id: "github-mcp".to_string(),
        },
        BuddyAction::CreatePulseReport {
            scope: PulseScope::All,
        },
        BuddyAction::ApplyMemoryBatch {
            batch_key: "merge_near_duplicate".to_string(),
            count_hint: 12,
        },
        BuddyAction::ApplyConfigPatch {
            draft_id: "d5".to_string(),
            target_path: ".refact/knowledge/handbook/testing.md".to_string(),
        },
        BuddyAction::AcceptQuest {
            suggestion_id: "sugg-1".to_string(),
        },
        BuddyAction::OpenBuddyConversation {
            chat_id: "chat-1".to_string(),
        },
        BuddyAction::Dismiss,
    ];
    for action in &actions {
        let json = serde_json::to_string(action).expect("serialize");
        let back: BuddyAction = serde_json::from_str(&json).expect("deserialize");
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(json, json2, "round-trip mismatch");
    }
    let batch_json = serde_json::to_string(&actions[11]).expect("serialize batch");
    assert!(batch_json.contains("\"kind\":\"apply_memory_batch\""));
    let kinds: Vec<BuddyOpportunityKind> = vec![
        BuddyOpportunityKind::MemoryOpsBatch,
        BuddyOpportunityKind::JobFinding,
        BuddyOpportunityKind::Quest,
    ];
    let tokens: Vec<String> = kinds
        .iter()
        .map(|kind| serde_json::to_string(kind).unwrap())
        .collect();
    assert_eq!(
        tokens,
        vec![
            "\"memory_ops_batch\"".to_string(),
            "\"job_finding\"".to_string(),
            "\"quest\"".to_string()
        ]
    );
}

#[test]
fn buddy_page_round_trip() {
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
            task_id: "task-abc".to_string(),
        },
        BuddyPage::KnowledgeGraph,
        BuddyPage::Worktrees,
        BuddyPage::SetupMode {
            mode: "setup_mcp".to_string(),
        },
    ];
    for page in &pages {
        let json = serde_json::to_string(page).expect("serialize");
        let back: BuddyPage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(page, &back, "round-trip mismatch for {:?}", page);
    }
    let task_json = serde_json::to_string(&BuddyPage::TaskWorkspace {
        task_id: "task-abc".to_string(),
    })
    .unwrap();
    assert!(task_json.contains("task-abc"), "task_id must be serialized");
    let setup_json = serde_json::to_string(&BuddyPage::SetupMode {
        mode: "setup_mcp".to_string(),
    })
    .unwrap();
    assert!(setup_json.contains("setup_mcp"), "mode must be serialized");
}

fn serialized_string<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .expect("serialize")
        .as_str()
        .expect("string")
        .to_string()
}

fn serialized_tag<T: serde::Serialize>(value: &T, tag: &str) -> String {
    serde_json::to_value(value)
        .expect("serialize")
        .get(tag)
        .and_then(|v| v.as_str())
        .expect("tag")
        .to_string()
}

#[test]
fn schema_contract_buddy_page_variants() {
    let cases = vec![
        (BuddyPage::Buddy, "buddy"),
        (BuddyPage::Stats, "stats"),
        (BuddyPage::Customization, "customization"),
        (BuddyPage::Providers, "providers"),
        (BuddyPage::DefaultModels, "default_models"),
        (BuddyPage::Integrations, "integrations"),
        (BuddyPage::Extensions, "extensions"),
        (BuddyPage::MarketplaceHub, "marketplace_hub"),
        (BuddyPage::Marketplace, "marketplace"),
        (BuddyPage::SkillsMarketplace, "skills_marketplace"),
        (BuddyPage::CommandsMarketplace, "commands_marketplace"),
        (BuddyPage::DelegatesMarketplace, "delegates_marketplace"),
        (BuddyPage::TasksList, "tasks_list"),
        (
            BuddyPage::TaskWorkspace {
                task_id: "task-1".to_string(),
            },
            "task_workspace",
        ),
        (BuddyPage::KnowledgeGraph, "knowledge_graph"),
        (BuddyPage::Worktrees, "worktrees"),
        (
            BuddyPage::SetupMode {
                mode: "setup_mcp".to_string(),
            },
            "setup_mode",
        ),
    ];
    for (page, expected) in cases {
        let json = serde_json::to_value(&page).expect("serialize");
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some(expected));
        if expected == "task_workspace" {
            assert_eq!(json.get("task_id").and_then(|v| v.as_str()), Some("task-1"));
        }
        if expected == "setup_mode" {
            assert_eq!(json.get("mode").and_then(|v| v.as_str()), Some("setup_mcp"));
        }
    }
}

#[test]
fn schema_contract_open_page_has_no_params() {
    let value = serde_json::to_value(BuddyAction::OpenPage {
        page: BuddyPage::Buddy,
    })
    .unwrap();
    assert!(value.get("params").is_none());
}

#[test]
fn schema_contract_buddy_action_variants() {
    let actions = vec![
        (
            BuddyAction::OpenPage {
                page: BuddyPage::Buddy,
            },
            "open_page",
        ),
        (
            BuddyAction::LaunchInvestigationChat {
                preload: InvestigationContext {
                    fact_keys: vec![],
                    diagnostic_ids: vec![],
                    memory_key: None,
                    log_excerpt: String::new(),
                    config_summary: String::new(),
                    initial_user_message: "investigate".to_string(),
                },
            },
            "launch_investigation_chat",
        ),
        (
            BuddyAction::DraftSkill {
                draft_id: "d1".to_string(),
                label: "Skill".to_string(),
            },
            "draft_skill",
        ),
        (
            BuddyAction::DraftCommand {
                draft_id: "d2".to_string(),
                label: "Command".to_string(),
            },
            "draft_command",
        ),
        (
            BuddyAction::DraftDelegate {
                draft_id: "d3".to_string(),
                label: "Delegate".to_string(),
            },
            "draft_delegate",
        ),
        (
            BuddyAction::DraftMode {
                draft_id: "d4".to_string(),
                label: "Mode".to_string(),
            },
            "draft_mode",
        ),
        (
            BuddyAction::DraftAgentsMdPatch {
                content: String::new(),
            },
            "draft_agents_md_patch",
        ),
        (
            BuddyAction::DraftDefaultsChange {
                defaults_kind: DefaultsKind::ChatModel,
                patch: serde_json::json!({}),
            },
            "draft_defaults_change",
        ),
        (
            BuddyAction::DraftCustomizationChange {
                customization_kind: CustomizationKind::Delegate,
                id: "delegate-1".to_string(),
                patch: serde_json::json!({}),
            },
            "draft_customization_change",
        ),
        (
            BuddyAction::OfferMarketplaceInstall {
                market_kind: MarketKind::Delegate,
                item_id: "item-1".to_string(),
            },
            "offer_marketplace_install",
        ),
        (
            BuddyAction::CreatePulseReport {
                scope: PulseScope::All,
            },
            "create_pulse_report",
        ),
        (BuddyAction::Dismiss, "dismiss"),
    ];
    for (action, expected) in actions {
        assert_eq!(serialized_tag(&action, "kind"), expected);
    }
}

#[test]
fn schema_contract_buddy_fact_kind_variants() {
    let cases = vec![
        (BuddyFactKind::TaskStuck, "task_stuck"),
        (BuddyFactKind::TaskAbandoned, "task_abandoned"),
        (
            BuddyFactKind::TaskClusterDuplicate,
            "task_cluster_duplicate",
        ),
        (BuddyFactKind::TrajectoryClutter, "trajectory_clutter"),
        (BuddyFactKind::ChatRetryStreak, "chat_retry_streak"),
        (BuddyFactKind::MemoryOrphan, "memory_orphan"),
        (BuddyFactKind::MemoryStaleConflict, "memory_stale_conflict"),
        (
            BuddyFactKind::MemoryRecurringLesson,
            "memory_recurring_lesson",
        ),
        (BuddyFactKind::ModePromptOverlap, "mode_prompt_overlap"),
        (BuddyFactKind::SkillTriggerWeak, "skill_trigger_weak"),
        (BuddyFactKind::AgentsMdGapDetected, "agents_md_gap_detected"),
        (BuddyFactKind::DefaultModelMissing, "default_model_missing"),
        (
            BuddyFactKind::BrokenModelReference,
            "broken_model_reference",
        ),
        (BuddyFactKind::McpAuthExpired, "mcp_auth_expired"),
        (BuddyFactKind::IntegrationFailing, "integration_failing"),
        (BuddyFactKind::DiagnosticCluster, "diagnostic_cluster"),
        (BuddyFactKind::FrontendErrorBurst, "frontend_error_burst"),
        (BuddyFactKind::GitDiffWidening, "git_diff_widening"),
        (BuddyFactKind::UncommittedPressure, "uncommitted_pressure"),
        (BuddyFactKind::WorktreeHygiene, "worktree_hygiene"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_buddy_opportunity_kind_variants() {
    let cases = vec![
        (BuddyOpportunityKind::TaskHealth, "task_health"),
        (
            BuddyOpportunityKind::TrajectoryCleanup,
            "trajectory_cleanup",
        ),
        (BuddyOpportunityKind::ChatRecap, "chat_recap"),
        (BuddyOpportunityKind::MemoryGarden, "memory_garden"),
        (BuddyOpportunityKind::ConfigDrift, "config_drift"),
        (BuddyOpportunityKind::AgentsMdGap, "agents_md_gap"),
        (BuddyOpportunityKind::ProviderTuning, "provider_tuning"),
        (BuddyOpportunityKind::IntegrationFix, "integration_fix"),
        (
            BuddyOpportunityKind::DiagnosticInvestigation,
            "diagnostic_investigation",
        ),
        (BuddyOpportunityKind::GitHygiene, "git_hygiene"),
        (BuddyOpportunityKind::WorktreeCleanup, "worktree_cleanup"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_buddy_priority_variants() {
    let cases = vec![
        (BuddyPriority::Low, "low"),
        (BuddyPriority::Normal, "normal"),
        (BuddyPriority::High, "high"),
        (BuddyPriority::Critical, "critical"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_opportunity_status_variants() {
    let cases = vec![
        (OpportunityStatus::New, "new"),
        (OpportunityStatus::Shown, "shown"),
        (OpportunityStatus::Dismissed, "dismissed"),
        (OpportunityStatus::Accepted, "accepted"),
        (OpportunityStatus::Completed, "completed"),
        (OpportunityStatus::Expired, "expired"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_defaults_kind_variants() {
    let cases = vec![
        (DefaultsKind::ChatModel, "chat_model"),
        (DefaultsKind::ChatModel2, "chat_model_2"),
        (
            DefaultsKind::TaskPlannerAgentModel,
            "task_planner_agent_model",
        ),
        (DefaultsKind::ChatLightModel, "chat_light_model"),
        (DefaultsKind::ChatBuddyModel, "chat_buddy_model"),
        (DefaultsKind::ChatThinkingModel, "chat_thinking_model"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_customization_kind_variants() {
    let cases = vec![
        (CustomizationKind::Mode, "mode"),
        (CustomizationKind::Skill, "skill"),
        (CustomizationKind::Command, "command"),
        (CustomizationKind::Delegate, "delegate"),
        (CustomizationKind::Hook, "hook"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_market_kind_variants() {
    let cases = vec![
        (MarketKind::Mcp, "mcp"),
        (MarketKind::Skill, "skill"),
        (MarketKind::Command, "command"),
        (MarketKind::Delegate, "delegate"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_draft_kind_variants() {
    let cases = vec![
        (DraftKind::Skill, "skill"),
        (DraftKind::Command, "command"),
        (DraftKind::Delegate, "delegate"),
        (DraftKind::Mode, "mode"),
        (DraftKind::AgentsMd, "agents_md"),
        (DraftKind::DefaultsModel, "defaults_model"),
        (DraftKind::Hook, "hook"),
        (DraftKind::PulseReport, "pulse_report"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_pulse_scope_variants() {
    let cases = vec![
        (PulseScope::All, "all"),
        (PulseScope::Tasks, "tasks"),
        (PulseScope::Trajectories, "trajectories"),
        (PulseScope::Memory, "memory"),
        (PulseScope::Providers, "providers"),
        (PulseScope::Mcp, "mcp"),
        (PulseScope::Customization, "customization"),
        (PulseScope::Diagnostics, "diagnostics"),
        (PulseScope::Git, "git"),
        (PulseScope::Worktrees, "worktrees"),
    ];
    for (value, expected) in cases {
        assert_eq!(serialized_string(&value), expected);
    }
}

#[test]
fn schema_contract_no_old_names_exist() {
    let values = vec![
        serde_json::to_value(BuddyPage::Marketplace).expect("serialize"),
        serde_json::to_value(BuddyPage::DelegatesMarketplace).expect("serialize"),
        serde_json::to_value(BuddyAction::DraftDelegate {
            draft_id: "d1".to_string(),
            label: "Delegate".to_string(),
        })
        .expect("serialize"),
        serde_json::to_value(CustomizationKind::Delegate).expect("serialize"),
        serde_json::to_value(MarketKind::Delegate).expect("serialize"),
        serde_json::to_value(DraftKind::Delegate).expect("serialize"),
    ];
    for value in values {
        let json = value.to_string();
        let old_names = [
            format!("\"{}{}\"", "mcp", "_marketplace"),
            format!("\"{}{}{}\"", "sub", "agents", "_marketplace"),
            format!("\"{}{}{}\"", "draft_", "sub", "agent"),
            format!("\"{}{}\"", "sub", "agent"),
        ];
        for old in old_names {
            assert!(
                !json.contains(&old),
                "old schema name present: {} in {}",
                old,
                json
            );
        }
    }
}

#[test]
fn buddy_pulse_default() {
    let pulse = BuddyPulse::default();
    let json = serde_json::to_string(&pulse).expect("serialize");
    let back: BuddyPulse = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.tasks.total, 0);
    assert_eq!(back.git.uncommitted_files, 0);
    assert!(back.generated_at.is_none());
}

#[test]
fn settings_default_observer_toggles() {
    let settings = BuddySettings::default();
    assert!(settings.observers.chat_pattern);
    assert!(settings.observers.task_health);
    assert!(settings.observers.trajectory_clutter);
    assert!(settings.observers.customization_drift);
    assert!(settings.observers.memory_garden);
    assert!(settings.observers.mcp_auth);
    assert!(settings.observers.git_pressure);
    assert!(settings.observers.diagnostic_cluster);
    assert!(settings.observers.provider_health);
}

#[test]
fn should_observe_thread_skips_buddy_task_setup_and_subchats() {
    let thread = crate::chat::types::ThreadParams::default();
    assert!(super::chat_reactions::should_observe_thread(&thread));

    let mut thread = crate::chat::types::ThreadParams::default();
    thread.buddy_meta = Some(refact_buddy_core::types::BuddyThreadMeta {
        is_buddy_chat: true,
        buddy_chat_kind: "investigation".to_string(),
        workflow_id: None,
    });
    assert!(!super::chat_reactions::should_observe_thread(&thread));

    let mut thread = crate::chat::types::ThreadParams::default();
    thread.task_meta = Some(crate::chat::types::TaskMeta::default());
    assert!(!super::chat_reactions::should_observe_thread(&thread));

    for mode in ["buddy", "setup", "task_agent", "task_planner"] {
        let mut thread = crate::chat::types::ThreadParams::default();
        thread.mode = mode.to_string();
        assert!(!super::chat_reactions::should_observe_thread(&thread));
    }

    let mut thread = crate::chat::types::ThreadParams::default();
    thread.mode = "setup_mcp".to_string();
    assert!(!super::chat_reactions::should_observe_thread(&thread));

    let mut thread = crate::chat::types::ThreadParams::default();
    thread.parent_id = Some("parent".to_string());
    assert!(!super::chat_reactions::should_observe_thread(&thread));

    let mut thread = crate::chat::types::ThreadParams::default();
    thread.parent_id = Some("parent".to_string());
    thread.link_type = Some("branch".to_string());
    assert!(super::chat_reactions::should_observe_thread(&thread));
}

#[test]
fn humor_level_and_autonomy_serde() {
    let hl = HumorLevel::Light;
    let json = serde_json::to_string(&hl).unwrap();
    let back: HumorLevel = serde_json::from_str(&json).unwrap();
    assert_eq!(hl, back);

    let al = AutonomyLevel::Suggest;
    let json = serde_json::to_string(&al).unwrap();
    let back: AutonomyLevel = serde_json::from_str(&json).unwrap();
    assert_eq!(al, back);

    let settings = BuddySettings::default();
    assert_eq!(settings.humor_level, HumorLevel::Light);
    assert_eq!(settings.autonomy_level, AutonomyLevel::Propose);
}
