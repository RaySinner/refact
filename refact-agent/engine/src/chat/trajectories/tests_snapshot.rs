use super::*;
use super::test_support::*;
    use crate::chat::diagnostics::{is_ui_only_message, make_ui_only_error_message};
    use crate::chat::perf_diagnostics::{self, MemoryPerfSink, PerfClock, PerfRecorder};
    use crate::chat::types::{
        ActiveCommandContext, ChatEvent, CompressionPhase, CompressionReason, EventEnvelope,
    };
    use crate::vecdb::vdb_structs::{EmbeddingModelConfig, SearchResult, VecDbStatus, VecdbRecord};
    use async_trait::async_trait;
    use refact_core::vecdb_types::VecdbSearch;
    use refact_chat_api::{
        BuddyThreadMeta, ClaudeCodeIdentity, FrozenRequestPrefix, GoalBudget, GoalProgress,
        GoalStatus,
    };
    use serial_test::serial;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex as StdMutex;
    #[test]
    fn test_trajectory_snapshot_from_session_captures_fields() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use tokio::sync::{broadcast, Mutex as AMutex, Notify};
        use std::collections::VecDeque;

        let (tx, _rx) = broadcast::channel(16);
        let session = ChatSession {
            chat_id: "test-123".to_string(),
            interruptible_waits: 0,
            wait_delivery_boundary: false,
            wait_interrupt_flag: Arc::new(AtomicBool::new(false)),
            derived_privacy_zones: crate::privacy::records::new_derived_privacy_zones(),
            tool_loop_guard: crate::tools::tool_call_loop_guard::ToolCallLoopGuard::new(),
            turn_investigation_tracker:
                crate::tools::tool_call_loop_guard::TurnInvestigationTracker::new(),
            thread: ThreadParams {
                id: "test-123".to_string(),
                title: "Test Thread".to_string(),
                model: "gpt-4".to_string(),
                mode: "AGENT".to_string(),
                tool_use: "agent".to_string(),
                boost_reasoning: Some(true),
                reasoning_effort: None,
                thinking_budget: None,
                temperature: None,
                frequency_penalty: None,
                max_tokens: None,
                parallel_tool_calls: None,
                context_tokens_cap: Some(8000),
                auto_compression_cap: Some(7000),
                auto_compression_cap_pending: false,
                include_project_info: false,
                checkpoints_enabled: true,
                is_title_generated: true,
                auto_approve_editing_tools: false,
                auto_approve_dangerous_commands: false,
                autonomous_no_confirm: false,
                task_meta: None,
                worktree: None,
                parent_id: Some("parent-chat-id".to_string()),
                link_type: Some("subagent".to_string()),
                root_chat_id: Some("root-chat-id".to_string()),
                previous_response_id: None,
                browser_meta: None,
                active_skill: None,
                auto_enrichment_enabled: None,
                buddy_meta: None,
                auto_compact_enabled: None,
                agent_nudge_enabled: None,
                strip_reasoning_from_prompt: None,
                frozen_request_prefix: None,
                claude_code_identity: None,
                reactive_compact_attempts: None,
            },
            messages: vec![ChatMessage::new("user".to_string(), "Hello".to_string())],
            runtime: super::super::types::RuntimeState::default(),
            goal: None,
            goal_active: false,
            goal_status: None,
            goal_turns_used: 0,
            goal_tokens_used: 0,
            goal_no_progress_turns: 0,
            is_compressing: false,
            compression_phase: None,
            compression_reason: None,
            compression_estimated_tokens: None,
            compression_effective_cap: None,
            compression_cap_source: None,
            compression_attempt_generation: 0,
            active_compression_attempt: None,
            compression_attempt_started_at_ms: None,
            compression_abort_flag: None,
            draft_message: None,
            draft_usage: None,
            command_queue: VecDeque::new(),
            pending_deliveries: VecDeque::new(),
            runner_pending_deliveries: Vec::new(),
            delivered_delivery_ids: Default::default(),
            turn_depth: 0,
            delivery_wake_sources: Default::default(),
            event_seq: 0,
            event_tx: tx,
            recent_request_ids: VecDeque::new(),
            recent_request_ids_set: std::collections::HashSet::new(),
            abort_flag: Arc::new(AtomicBool::new(false)),
            abort_notify: Arc::new(Notify::new()),
            user_interrupt_flag: Arc::new(AtomicBool::new(false)),
            queue_processor_running: Arc::new(AtomicBool::new(false)),
            queue_notify: Arc::new(Notify::new()),
            queue_processor_counters: Arc::new(
                crate::chat::types::QueueProcessorCounters::default(),
            ),
            last_activity: Instant::now(),
            last_stream_delta_at: None,
            command_enqueued_at: std::collections::HashMap::new(),
            stream_started_at: None,
            confirmation_paused_at: None,
            last_tool_started_at: None,
            last_tool_progress_at: None,
            trajectory_dirty: false,
            trajectory_version: 5,
            trajectory_committed_version: 0,
            trajectory_save_in_flight: false,
            trajectory_save_queued: false,
            trajectory_save_mutex: Arc::new(AMutex::new(())),
            trajectory_commit_notify: Arc::new(Notify::new()),
            trajectory_save_error: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            closed: false,
            closed_flag: Arc::new(AtomicBool::new(false)),
            external_reload_pending: None,
            last_prompt_messages: Vec::new(),
            tool_catalog: None,
            turn_tool_pool: None,
            pending_context_rebuild: None,
            pending_mode_handoff: None,
            last_rebuild_attempt_version: None,
            pending_max_new_tokens_boost: None,
            cache_guard_snapshot: None,
            cache_guard_request_generation: 0,
            cache_guard_snapshot_generation: 0,
            cache_guard_reset_generation: 0,
            cache_guard_force_next: false,
            provider_usage_stale: false,
            task_agent_error: None,
            trajectory_events_tx: None,
            pending_browser_message: None,
            post_tool_side_effects: VecDeque::new(),
            active_command: ActiveCommandContext::default(),
            skills_available_count: 0,
            skills_included: Vec::new(),
            pending_skill_deactivation: None,
            post_turn_task_handles: Vec::new(),
            openai_codex_websocket: Default::default(),
            suppress_auto_enrichment_for_next_turn: false,
            enrichment_identities: std::collections::HashSet::new(),
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
            background_agents: std::collections::HashMap::new(),
            goal_stopped_by_abort: false,
            goal_ledger: Vec::new(),
            goal_turn_evidence: false,
            goal_verification_blocked_until_ms: None,
        };

        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.chat_id, "test-123");
        assert_eq!(snapshot.title, "Test Thread");
        assert_eq!(snapshot.model, "gpt-4");
        assert_eq!(snapshot.mode, "AGENT");
        assert!(snapshot.boost_reasoning);
        assert_eq!(snapshot.context_tokens_cap, Some(8000));
        assert_eq!(snapshot.auto_compression_cap, Some(7000));
        assert!(!snapshot.include_project_info);
        assert!(snapshot.is_title_generated);
        assert_eq!(snapshot.version, 5);
        assert_eq!(snapshot.messages.len(), 1);
    }

    #[test]
    fn test_trajectory_roundtrip_active_skill() {
        use super::super::types::*;
        use super::super::types::ActiveCommandContext;
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use tokio::sync::{broadcast, Mutex as AMutex, Notify};
        use std::collections::VecDeque;
        use std::time::Instant;

        let (tx, _rx) = broadcast::channel(16);
        let mut session = ChatSession {
            chat_id: "skill-test".to_string(),
            interruptible_waits: 0,
            wait_delivery_boundary: false,
            wait_interrupt_flag: Arc::new(AtomicBool::new(false)),
            derived_privacy_zones: crate::privacy::records::new_derived_privacy_zones(),
            tool_loop_guard: crate::tools::tool_call_loop_guard::ToolCallLoopGuard::new(),
            turn_investigation_tracker:
                crate::tools::tool_call_loop_guard::TurnInvestigationTracker::new(),
            thread: ThreadParams {
                id: "skill-test".to_string(),
                active_skill: Some("my-skill".to_string()),
                ..Default::default()
            },
            messages: vec![ChatMessage::new("user".to_string(), "Hello".to_string())],
            runtime: RuntimeState::default(),
            goal: None,
            goal_active: false,
            goal_status: None,
            goal_turns_used: 0,
            goal_tokens_used: 0,
            goal_no_progress_turns: 0,
            is_compressing: false,
            compression_phase: None,
            compression_reason: None,
            compression_estimated_tokens: None,
            compression_effective_cap: None,
            compression_cap_source: None,
            compression_attempt_generation: 0,
            active_compression_attempt: None,
            compression_attempt_started_at_ms: None,
            compression_abort_flag: None,
            draft_message: None,
            draft_usage: None,
            command_queue: VecDeque::new(),
            pending_deliveries: VecDeque::new(),
            runner_pending_deliveries: Vec::new(),
            delivered_delivery_ids: Default::default(),
            turn_depth: 0,
            delivery_wake_sources: Default::default(),
            event_seq: 0,
            event_tx: tx,
            recent_request_ids: VecDeque::new(),
            recent_request_ids_set: std::collections::HashSet::new(),
            abort_flag: Arc::new(AtomicBool::new(false)),
            abort_notify: Arc::new(Notify::new()),
            user_interrupt_flag: Arc::new(AtomicBool::new(false)),
            queue_processor_running: Arc::new(AtomicBool::new(false)),
            queue_notify: Arc::new(Notify::new()),
            queue_processor_counters: Arc::new(
                crate::chat::types::QueueProcessorCounters::default(),
            ),
            last_activity: Instant::now(),
            last_stream_delta_at: None,
            command_enqueued_at: std::collections::HashMap::new(),
            stream_started_at: None,
            confirmation_paused_at: None,
            last_tool_started_at: None,
            last_tool_progress_at: None,
            trajectory_dirty: false,
            trajectory_version: 1,
            trajectory_committed_version: 0,
            trajectory_save_in_flight: false,
            trajectory_save_queued: false,
            trajectory_save_mutex: Arc::new(AMutex::new(())),
            trajectory_commit_notify: Arc::new(Notify::new()),
            trajectory_save_error: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            closed: false,
            closed_flag: Arc::new(AtomicBool::new(false)),
            external_reload_pending: None,
            last_prompt_messages: Vec::new(),
            tool_catalog: None,
            turn_tool_pool: None,
            pending_context_rebuild: None,
            pending_mode_handoff: None,
            last_rebuild_attempt_version: None,
            pending_max_new_tokens_boost: None,
            cache_guard_snapshot: None,
            cache_guard_request_generation: 0,
            cache_guard_snapshot_generation: 0,
            cache_guard_reset_generation: 0,
            cache_guard_force_next: false,
            provider_usage_stale: false,
            task_agent_error: None,
            trajectory_events_tx: None,
            pending_browser_message: None,
            post_tool_side_effects: VecDeque::new(),
            active_command: ActiveCommandContext::default(),
            skills_available_count: 0,
            skills_included: Vec::new(),
            pending_skill_deactivation: None,
            post_turn_task_handles: Vec::new(),
            openai_codex_websocket: Default::default(),
            suppress_auto_enrichment_for_next_turn: false,
            enrichment_identities: std::collections::HashSet::new(),
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
            background_agents: std::collections::HashMap::new(),
            goal_stopped_by_abort: false,
            goal_ledger: Vec::new(),
            goal_turn_evidence: false,
            goal_verification_blocked_until_ms: None,
        };

        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.active_skill, Some("my-skill".to_string()));

        session.thread.active_skill = None;
        let snapshot_none = trajectory_snapshot_from_session(&session);
        assert!(snapshot_none.active_skill.is_none());
    }

    #[test]
    fn trajectory_snapshot_from_session_captures_wake_up_at() {
        let wake_up_at = chrono::Utc::now() + chrono::Duration::minutes(5);
        let mut session = ChatSession::new("wake-snapshot".to_string());
        session.wake_up_at = Some(wake_up_at);

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.wake_up_at, Some(wake_up_at));
    }

    #[tokio::test]
    async fn reopen_preserves_pinned_plan() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let mut plan_extra = serde_json::Map::new();
        plan_extra.insert(
            "plan".to_string(),
            json!({
                "mode": "agent",
                "version": 1,
                "created_at_ms": 123,
                "supersedes": null,
            }),
        );
        let plan = ChatMessage {
            role: "plan".to_string(),
            content: ChatContent::SimpleText("base plan".to_string()),
            preserve: Some(true),
            extra: plan_extra,
            ..Default::default()
        };
        let delta = crate::chat::internal_roles::plan_delta(
            "tool.set_plan",
            json!({"seq": 1}),
            "append update",
        );
        let mut session = ChatSession::new("plan-reopen".to_string());
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.add_message(plan);
        session.add_message(delta);

        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "plan-reopen").await.unwrap();
        let plans: Vec<_> = loaded
            .messages
            .iter()
            .filter(|message| message.role == "plan")
            .collect();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].content.content_text_only(), "base plan");
        assert_eq!(plans[0].preserve, Some(true));
        assert_eq!(plans[0].extra["plan"]["version"], json!(1));
        let deltas: Vec<_> = loaded
            .messages
            .iter()
            .filter(|message| {
                message.role == "event" && message.extra["event"]["subkind"] == json!("plan_delta")
            })
            .collect();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].content.content_text_only(), "append update");
    }

    #[tokio::test]
    async fn legacy_fallback_ids_are_distinct_for_repeated_hidden_events() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        tokio::fs::write(
            trajectories_dir.join("legacy-hidden-events.json"),
            serde_json::to_string(&json!({
                "id": "legacy-hidden-events",
                "title": "Legacy Hidden Events",
                "model": "model",
                "mode": "agent",
                "tool_use": "agent",
                "messages": [
                    {
                        "role": "event",
                        "content": "append update",
                        "extra": {
                            "event": {
                                "subkind": "plan_delta",
                                "source": "tool.update_plan",
                                "payload": {"seq": 1}
                            }
                        }
                    },
                    {
                        "role": "event",
                        "content": "append update",
                        "extra": {
                            "event": {
                                "subkind": "plan_delta",
                                "source": "tool.update_plan",
                                "payload": {"seq": 2}
                            }
                        }
                    },
                    {
                        "message_id": "kept-existing-id",
                        "role": "user",
                        "content": "visible"
                    }
                ],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z",
                "include_project_info": true,
                "checkpoints_enabled": true
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "legacy-hidden-events")
            .await
            .unwrap();

        assert_eq!(loaded.messages.len(), 3);
        assert!(loaded.messages[0].message_id.starts_with("legacy:event:"));
        assert!(loaded.messages[1].message_id.starts_with("legacy:event:"));
        assert_ne!(loaded.messages[0].message_id, loaded.messages[1].message_id);
        assert_eq!(loaded.messages[2].message_id, "kept-existing-id");
    }

    #[tokio::test]
    async fn wake_up_at_round_trips_through_trajectory_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let wake_up_at = chrono::Utc::now() + chrono::Duration::minutes(5);
        let mut session = ChatSession::new("wake-roundtrip".to_string());
        session.thread.title = "Wake Roundtrip".to_string();
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.wake_up_at = Some(wake_up_at);
        session.add_message(ChatMessage::new("user".to_string(), "wait".to_string()));

        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        drop(session);

        let loaded = load_trajectory_for_chat(gcx, "wake-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.wake_up_at, Some(wake_up_at));
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_goal_projection() {
        let mut session = ChatSession::new("goal-snapshot".to_string());
        session.install_goal("agent", "ship the goal", true, GoalBudget::default());
        session.goal.as_mut().unwrap().progress = GoalProgress {
            turns_used: 2,
            tokens_used: 345,
            started_at_ms: 123,
            no_progress_turns: 1,
            last_nudge_at_ms: 456,
            cost_used_cents: 0,
        };
        session.refresh_goal_runtime_mirror();

        let snapshot = trajectory_snapshot_from_session(&session);
        let goal = snapshot.goal.expect("goal snapshot");

        assert_eq!(goal.content, "ship the goal");
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.progress.turns_used, 2);
        assert_eq!(goal.progress.tokens_used, 345);
        assert_eq!(goal.progress.no_progress_turns, 1);
    }

    #[test]
    fn clamp_goal_snapshot_for_load_migrates_implicit_legacy_budget() {
        let goal = GoalSnapshot {
            content: "ship legacy".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::BudgetExhausted,
            budget: GoalBudget::legacy_default_hard_limits(),
            progress: GoalProgress {
                turns_used: 10,
                ..Default::default()
            },
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };

        let loaded = clamp_goal_snapshot_for_load(goal);

        assert_eq!(loaded.budget, GoalBudget::default());
        assert_eq!(loaded.status, GoalStatus::Active);
    }

    #[test]
    fn clamp_goal_snapshot_for_load_heals_stale_verifying() {
        let goal = GoalSnapshot {
            content: "ship it".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::Verifying,
            budget: GoalBudget::default(),
            progress: GoalProgress::default(),
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };
        assert_eq!(
            clamp_goal_snapshot_for_load(goal).status,
            GoalStatus::Active
        );

        let inactive = GoalSnapshot {
            content: "ship it".to_string(),
            version: 1,
            active: false,
            status: GoalStatus::Verifying,
            budget: GoalBudget::default(),
            progress: GoalProgress::default(),
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };
        assert_eq!(
            clamp_goal_snapshot_for_load(inactive).status,
            GoalStatus::Paused
        );
    }

    #[test]
    fn clamp_goal_snapshot_for_load_keeps_stopped_goals_stopped() {
        let goal = GoalSnapshot {
            content: "ship it".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::Stopped,
            budget: GoalBudget::default(),
            progress: GoalProgress::default(),
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };
        assert_eq!(
            clamp_goal_snapshot_for_load(goal).status,
            GoalStatus::Stopped
        );
    }

    #[test]
    fn clamp_goal_snapshot_for_load_preserves_explicit_legacy_budget() {
        let budget = GoalBudget {
            explicit: true,
            ..GoalBudget::legacy_default_hard_limits()
        };
        let goal = GoalSnapshot {
            content: "ship explicit".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::BudgetExhausted,
            budget,
            progress: GoalProgress {
                turns_used: 12,
                ..Default::default()
            },
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };

        let loaded = clamp_goal_snapshot_for_load(goal);

        assert_eq!(loaded.budget.max_turns, Some(10));
        assert_eq!(loaded.budget.max_minutes, Some(15));
        assert_eq!(loaded.budget.max_tokens, Some(200_000));
        assert_eq!(loaded.budget.no_progress_turns, Some(2));
        assert!(loaded.budget.explicit);
        assert_eq!(loaded.progress.turns_used, 10);
        assert_eq!(loaded.status, GoalStatus::BudgetExhausted);
    }

    #[tokio::test]
    async fn goal_round_trips_through_trajectory_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let mut session = ChatSession::new("goal-roundtrip".to_string());
        session.thread.title = "Goal Roundtrip".to_string();
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.install_goal("agent", "finish the card", true, GoalBudget::default());
        session.add_message(crate::chat::internal_roles::goal_delta(
            "tool.update_goal",
            json!({"seq": 1, "at_ms": 999}),
            "add tests",
        ));
        session.goal.as_mut().unwrap().progress = GoalProgress {
            turns_used: 3,
            tokens_used: 456,
            started_at_ms: 111,
            no_progress_turns: 1,
            last_nudge_at_ms: 222,
            cost_used_cents: 0,
        };
        session.refresh_goal_runtime_mirror();

        let started_at_ms = session.goal.as_ref().unwrap().progress.started_at_ms;
        let snapshot = trajectory_snapshot_from_session(&session);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "goal-roundtrip")
            .await
            .unwrap();
        let loaded_goal = loaded.goal.clone().expect("loaded goal");

        assert_eq!(
            loaded_goal.content,
            "finish the card\n\n---\n\n## Goal updates\n\nadd tests"
        );
        assert_eq!(loaded_goal.status, GoalStatus::Active);
        assert_eq!(loaded_goal.progress.turns_used, 3);
        assert_eq!(loaded_goal.progress.tokens_used, 456);
        assert_eq!(loaded_goal.progress.no_progress_turns, 1);
        assert_eq!(loaded_goal.progress.started_at_ms, started_at_ms);
        assert_eq!(loaded_goal.transferred_from, None);
        assert_eq!(loaded_goal.transferred_to, None);
        assert!(loaded.messages.iter().any(|message| message.role == "goal"));
        assert!(loaded.messages.iter().any(|message| {
            message.role == "event" && message.extra["event"]["subkind"] == json!("goal_delta")
        }));

        let rehydrated = ChatSession::new_with_trajectory(
            "goal-roundtrip".to_string(),
            loaded.messages,
            loaded.thread,
            loaded.created_at,
            loaded.wake_up_at,
            loaded.waiting_for_card_ids,
            loaded.goal,
        );
        assert_eq!(rehydrated.goal_turns_used, 3);
        assert_eq!(rehydrated.goal_tokens_used, 456);
        assert_eq!(rehydrated.goal_no_progress_turns, 1);
        assert_eq!(rehydrated.event_seq, 0);
    }

    #[tokio::test]
    async fn wake_up_at_is_none_in_trajectories_created_before_field_existed() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let trajectories_dir = dir.path().join(".refact").join("trajectories");
        tokio::fs::create_dir_all(&trajectories_dir).await.unwrap();
        tokio::fs::write(
            trajectories_dir.join("legacy-wake.json"),
            r#"{
                "id":"legacy-wake",
                "title":"Legacy",
                "created_at":"2024-01-01T00:00:00Z",
                "updated_at":"2024-01-01T00:00:00Z",
                "model":"model",
                "mode":"agent",
                "tool_use":"agent",
                "messages":[{"role":"user","content":"hello"}],
                "include_project_info":true,
                "checkpoints_enabled":true
            }"#,
        )
        .await
        .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "legacy-wake").await.unwrap();
        assert!(loaded.wake_up_at.is_none());
    }

    #[tokio::test]
    async fn goal_ledger_round_trips_and_replay_wins_over_missing_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let mut session = ChatSession::new("goal-ledger-roundtrip".to_string());
        session.thread.title = "Ledger Roundtrip".to_string();
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.install_goal("agent", "ship the card", true, GoalBudget::default());
        session.goal_record_progress_with_cost(50, false, 2);
        session.goal_set_status_reason(GoalStatus::Stopped, "goal_control");
        session.goal_verification_blocked_until_ms = Some(4_242);

        let mut snapshot = trajectory_snapshot_from_session(&session);
        assert!(!snapshot.goal_ledger.is_empty());
        snapshot.goal = None;
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "goal-ledger-roundtrip")
            .await
            .unwrap();
        assert!(!loaded.goal_ledger.is_empty());
        let loaded_goal = loaded.goal.expect("goal restored from ledger replay");
        assert_eq!(loaded_goal.status, GoalStatus::Stopped);
        assert_eq!(loaded_goal.stop_reason.as_deref(), Some("goal_control"));
        assert_eq!(loaded_goal.progress.turns_used, 1);
        assert_eq!(loaded_goal.progress.tokens_used, 50);
        assert_eq!(loaded_goal.progress.cost_used_cents, 2);
        assert_eq!(loaded_goal.progress.no_progress_turns, 1);
        assert_eq!(loaded.goal_verification_blocked_until_ms, Some(4_242));
    }

    #[tokio::test]
    async fn trajectory_snapshot_roundtrip_preserves_waiting_for_card_ids() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }

        let card_ids = vec!["T-1".to_string(), "T-10".to_string(), "T-2".to_string()];
        let mut session = ChatSession::new("wait-card-roundtrip".to_string());
        session.thread.title = "Wait Card Roundtrip".to_string();
        session.created_at = "2024-01-01T00:00:00Z".to_string();
        session.waiting_for_card_ids = card_ids.clone();
        session.add_message(ChatMessage::new("user".to_string(), "waiting".to_string()));

        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.waiting_for_card_ids, card_ids);
        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let loaded = load_trajectory_for_chat(gcx, "wait-card-roundtrip")
            .await
            .unwrap();
        assert_eq!(loaded.waiting_for_card_ids, card_ids);
    }

    #[test]
    fn test_trajectory_load_without_active_skill_field() {
        let json_str = r#"{"id":"chat-1","title":"T","model":"m","mode":"agent","tool_use":"agent","messages":[],"created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","include_project_info":true,"checkpoints_enabled":true}"#;
        let t: serde_json::Value = serde_json::from_str(json_str).unwrap();
        let active_skill = t
            .get("active_skill")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        assert!(
            active_skill.is_none(),
            "Old trajectories must load with active_skill = None"
        );
    }

    #[test]
    fn trajectory_worktree_snapshot_from_session_captures_thread_worktree() {
        let worktree = trajectory_worktree_sample();
        let mut session = ChatSession::new("wt-snapshot".to_string());
        session.thread.worktree = Some(worktree.clone());
        let snapshot = trajectory_snapshot_from_session(&session);
        assert_eq!(snapshot.worktree, Some(worktree));
    }

    #[test]
    fn trajectory_snapshot_from_session_filters_empty_assistant_messages() {
        let mut session = ChatSession::new("empty-assistant-snapshot".to_string());
        session.messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("hello".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("   \n".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "error".to_string(),
            content: ChatContent::SimpleText("LLM error".to_string()),
            ..Default::default()
        });

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "error");
    }

    #[test]
    fn trajectory_snapshot_from_session_preserves_ui_only_messages() {
        let mut session = ChatSession::new("ui-only-snapshot".to_string());
        session
            .messages
            .push(ChatMessage::new("user".to_string(), "visible".to_string()));
        session
            .messages
            .push(make_ui_only_error_message("context_length_exceeded"));

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "error");
        assert!(is_ui_only_message(&snapshot.messages[1]));
    }

    #[test]
    fn trajectory_snapshot_from_session_filters_metadata_only_assistant_messages() {
        use crate::call_validation::ChatUsage;

        let mut session = ChatSession::new("metadata-only-assistant-snapshot".to_string());
        session.messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("hello".to_string()),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            usage: Some(ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 0,
                total_tokens: 10,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                metering_usd: None,
            }),
            extra: serde_json::Map::from_iter([(
                "openai_response_id".to_string(),
                json!("resp_123"),
            )]),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("visible".to_string()),
            ..Default::default()
        });

        let snapshot = trajectory_snapshot_from_session(&session);

        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "assistant");
        assert_eq!(snapshot.messages[1].content.content_text_only(), "visible");
    }

    #[test]
    fn trajectory_worktree_meta_creation_omits_unvalidated_worktree() {
        let worktree = trajectory_worktree_sample();
        let mut extra = serde_json::Map::new();
        extra.insert(
            "worktree".to_string(),
            serde_json::to_value(&worktree).unwrap(),
        );
        let data = TrajectoryData {
            id: "meta-chat".to_string(),
            title: "Meta".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages: Vec::new(),
            extra,
        };
        let meta = trajectory_data_to_meta(&data);
        assert!(meta.worktree.is_none());
    }

    #[test]
    fn trajectory_worktree_invalid_extra_is_not_preserved() {
        let mut extra = serde_json::Map::new();
        extra.insert("worktree".to_string(), json!({"root":"/tmp/untrusted"}));
        let worktree = sanitize_worktree_extra(&mut extra);
        assert!(worktree.is_none());
        assert!(extra.get("worktree").is_none());
    }

    #[tokio::test]
    async fn trajectory_save_removes_malformed_worktree_extra_from_json() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = AppState::from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }
        let chat_id = "malformed-worktree-save";
        let payload = json!({
            "id": chat_id,
            "title": "Malformed Worktree",
            "model": "m",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "worktree": {"root":"/tmp/untrusted"}
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let path = nested_trajectory_path(dir.path(), chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert!(saved.get("worktree").is_none());
    }

    #[tokio::test]
    async fn trajectory_save_preserves_valid_worktree_extra_in_json() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let app = AppState::from_gcx(gcx.clone()).await;
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/chat/save-preserve".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let worktree = created.worktree.meta.clone();
        let chat_id = "valid-worktree-save";
        let payload = json!({
            "id": chat_id,
            "title": "Valid Worktree",
            "model": "m",
            "mode": "agent",
            "tool_use": "agent",
            "messages": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "include_project_info": true,
            "checkpoints_enabled": true,
            "worktree": serde_json::to_value(&worktree).unwrap()
        });

        handle_v1_trajectories_save(
            State(app),
            AxumPath(chat_id.to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&payload).unwrap()),
        )
        .await
        .unwrap();

        let path = nested_trajectory_path(&source, chat_id, chat_id);
        let saved: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(saved["worktree"]["id"], worktree.id);
        assert_eq!(saved["worktree"]["branch"], "refact/chat/save-preserve");
    }

    #[tokio::test]
    async fn trajectory_worktree_save_load_roundtrips_top_level_field() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let app = AppState::from_gcx(gcx.clone()).await;
        let service = WorktreeService::new(cache, source.clone()).unwrap();
        let created = service
            .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/chat/roundtrip".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        let worktree = created.worktree.meta.clone();
        let chat_id = "wt-roundtrip".to_string();
        let snapshot = TrajectorySnapshot {
            goal: None,
            goal_ledger: Vec::new(),
            pending_deliveries: Vec::new(),
            goal_verification_blocked_until_ms: None,
            chat_id: chat_id.clone(),
            title: "Worktree Chat".to_string(),
            model: "model".to_string(),
            mode: "agent".to_string(),
            tool_use: "agent".to_string(),
            messages: vec![ChatMessage::new("user".to_string(), "Hello".to_string())],
            created_at: "2024-01-01T00:00:00Z".to_string(),
            boost_reasoning: false,
            checkpoints_enabled: true,
            context_tokens_cap: None,
            auto_compression_cap: None,
            auto_compression_cap_pending: true,
            include_project_info: true,
            is_title_generated: true,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            autonomous_no_confirm: false,
            version: 1,
            task_meta: None,
            worktree: Some(worktree.clone()),
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
            auto_enrichment_enabled: None,
            buddy_meta: None,
            auto_compact_enabled: None,
            frozen_request_prefix: None,
            claude_code_identity: None,
            reactive_compact_attempts: None,
            wake_up_at: None,
            waiting_for_card_ids: Vec::new(),
        };

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();
        let path = nested_trajectory_path(&source, &chat_id, &chat_id);
        let raw = tokio::fs::read_to_string(path).await.unwrap();
        let raw_json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(raw_json["worktree"]["id"], worktree.id);
        assert_eq!(raw_json["worktree"]["branch"], "refact/chat/roundtrip");

        let loaded = load_trajectory_for_chat(gcx.clone(), &chat_id)
            .await
            .unwrap();
        assert_eq!(loaded.thread.worktree, Some(worktree.clone()));
        let listed = list_all_trajectories_meta(app, false).await.unwrap();
        let listed_worktree = listed
            .iter()
            .find(|item| item.id == chat_id)
            .and_then(|item| item.worktree.clone())
            .unwrap();
        assert_eq!(listed_worktree.id, worktree.id);
    }

    #[tokio::test]
    async fn frozen_request_prefix_and_claude_code_identity_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let (gcx, _) = make_app_with_workspace(dir.path()).await;
        let tools_canonical = json!([
            {
                "type": "function",
                "function": {
                    "name": "cat",
                    "description": "Read a file",
                    "parameters": {"type": "object"}
                }
            }
        ]);
        let frozen_request_prefix = FrozenRequestPrefix {
            schema_version: 1,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            system_prompt: Some("verbatim system".to_string()),
            tools_canonical: Some(tools_canonical.clone()),
        };
        let claude_code_identity = ClaudeCodeIdentity {
            device_id: "device-123".to_string(),
            session_id: "session-456".to_string(),
        };
        let mut snapshot = test_snapshot(
            "frozen-prefix-roundtrip",
            "Frozen Prefix",
            vec![ChatMessage::new("user".to_string(), "hello".to_string())],
        );
        snapshot.frozen_request_prefix = Some(frozen_request_prefix.clone());
        snapshot.claude_code_identity = Some(claude_code_identity.clone());

        save_trajectory_snapshot(gcx.clone(), snapshot)
            .await
            .unwrap();

        let path = nested_trajectory_path(
            dir.path(),
            "frozen-prefix-roundtrip",
            "frozen-prefix-roundtrip",
        );
        let raw: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        assert_eq!(
            raw["frozen_request_prefix"]["tools_canonical"],
            tools_canonical
        );
        assert_eq!(raw["claude_code_identity"]["device_id"], "device-123");

        let loaded = load_trajectory_for_chat(gcx, "frozen-prefix-roundtrip")
            .await
            .unwrap();
        assert_eq!(
            loaded.thread.frozen_request_prefix,
            Some(frozen_request_prefix)
        );
        assert_eq!(
            loaded.thread.claude_code_identity,
            Some(claude_code_identity)
        );
    }

