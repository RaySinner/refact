use super::*;
use super::tests_support::*;
use crate::call_validation::{ChatToolCall, ChatToolFunction};
use serde_json::json;

    mod goal_budget {
        use super::*;

        fn budget() -> GoalBudget {
            GoalBudget {
                max_turns: Some(5),
                max_minutes: Some(2),
                max_tokens: Some(100),
                max_cost_cents: None,
                cooldown_ms: 1_500,
                no_progress_token_threshold: 10,
                no_progress_turns: Some(2),
                explicit: false,
            }
        }

        fn snapshot() -> GoalSnapshot {
            GoalSnapshot {
                content: "ship it".to_string(),
                version: 1,
                active: true,
                status: GoalStatus::Active,
                budget: budget(),
                progress: GoalProgress {
                    started_at_ms: 1_000,
                    ..Default::default()
                },
                attempts: Vec::new(),
                events: Vec::new(),
                criteria: Vec::new(),
                snoozed_until_ms: None,
                stop_reason: None,
                transferred_from: None,
                transferred_to: None,
            }
        }

        fn usage(total_tokens: usize, completion_tokens: usize) -> ChatUsage {
            ChatUsage {
                prompt_tokens: total_tokens.saturating_sub(completion_tokens),
                completion_tokens,
                total_tokens,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                metering_usd: None,
            }
        }

        #[test]
        fn budget_exhaustion_by_turns_tokens_minutes_and_no_progress() {
            let mut by_turns = snapshot();
            by_turns.progress.turns_used = by_turns.budget.max_turns.unwrap();
            assert_eq!(
                by_turns.goal_budget_exhaustion_status_at(1_000),
                Some(GoalStatus::BudgetExhausted)
            );

            let mut by_tokens = snapshot();
            by_tokens.progress.tokens_used = by_tokens.budget.max_tokens.unwrap();
            assert_eq!(
                by_tokens.goal_budget_exhaustion_status_at(1_000),
                Some(GoalStatus::BudgetExhausted)
            );

            let by_minutes = snapshot();
            assert_eq!(
                by_minutes.goal_budget_exhaustion_status_at(121_000),
                Some(GoalStatus::BudgetExhausted)
            );

            let mut by_no_progress = snapshot();
            by_no_progress.progress.no_progress_turns =
                by_no_progress.budget.no_progress_turns.unwrap();
            assert_eq!(
                by_no_progress.goal_budget_exhaustion_status_at(1_000),
                Some(GoalStatus::NoProgress)
            );
        }

        #[test]
        fn default_budget_never_exhausts() {
            let mut goal = snapshot();
            goal.budget = GoalBudget::default();
            goal.progress.turns_used = u32::MAX;
            goal.progress.tokens_used = u64::MAX;
            goal.progress.no_progress_turns = u32::MAX;
            goal.progress.started_at_ms = 1;

            assert_eq!(goal.goal_budget_exhaustion_status_at(u64::MAX), None);
            assert!(goal.goal_can_pursue_at(u64::MAX));
        }

        #[test]
        fn zero_budget_limits_are_disabled() {
            let mut goal = snapshot();
            goal.budget.max_turns = Some(0);
            goal.budget.max_minutes = Some(0);
            goal.budget.max_tokens = Some(0);
            goal.budget.no_progress_turns = Some(0);
            goal.progress.turns_used = u32::MAX;
            goal.progress.tokens_used = u64::MAX;
            goal.progress.no_progress_turns = u32::MAX;
            goal.progress.started_at_ms = 1;

            assert_eq!(goal.goal_budget_exhaustion_status_at(u64::MAX), None);
            assert!(goal.goal_can_pursue_at(u64::MAX));
        }

        #[test]
        fn cooldown_gate_uses_last_nudge_plus_budget_cooldown() {
            let mut goal = snapshot();
            assert!(goal.goal_nudge_ready_at(1_000));

            goal.goal_record_nudge(2_000);

            assert!(!goal.goal_nudge_ready_at(3_499));
            assert!(goal.goal_nudge_ready_at(3_500));
        }

        #[test]
        fn goal_can_pursue_matrix_across_statuses() {
            let mut goal = snapshot();
            assert!(goal.goal_can_pursue_at(1_000));

            for status in [
                GoalStatus::Verifying,
                GoalStatus::Paused,
                GoalStatus::Completed,
                GoalStatus::Stopped,
                GoalStatus::BudgetExhausted,
                GoalStatus::NoProgress,
                GoalStatus::Transferred,
            ] {
                goal.status = status;
                assert!(!goal.goal_can_pursue_at(1_000), "{status:?}");
            }

            goal.status = GoalStatus::Active;
            goal.active = false;
            assert!(!goal.goal_can_pursue_at(1_000));

            goal.active = true;
            goal.progress.turns_used = goal.budget.max_turns.unwrap();
            assert!(!goal.goal_can_pursue_at(1_000));
        }

        #[test]
        fn no_progress_counts_low_output_and_resets_on_progress_or_edit() {
            let mut session = make_session();
            session.install_goal("agent", "ship it", true, budget());

            assert!(session.goal_record_progress_from_usage(&usage(20, 3)));
            assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 1);
            assert_eq!(session.goal_status, Some(GoalStatus::Active));

            session.goal_record_progress_from_usage(&usage(20, 4));
            assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 2);
            assert_eq!(session.goal_status, Some(GoalStatus::NoProgress));
            assert!(!session.goal_can_pursue());

            session.goal_record_progress_from_usage(&usage(20, 12));
            assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 0);
            assert_eq!(session.goal_status, Some(GoalStatus::Active));

            session.goal_record_progress_from_usage(&usage(20, 1));
            assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 1);
            let user = ChatMessage {
                message_id: "user-edit".to_string(),
                role: "user".to_string(),
                content: ChatContent::SimpleText("before".to_string()),
                ..Default::default()
            };
            session.add_message(user.clone());
            let mut updated = user;
            updated.content = ChatContent::SimpleText("after".to_string());
            session.update_message("user-edit", updated);

            assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 0);
        }

        #[test]
        fn manual_no_progress_reset_restores_active_gate() {
            let mut session = make_session();
            session.install_goal("agent", "ship it", true, budget());
            session.goal_record_progress_from_usage(&usage(20, 1));
            session.goal_record_progress_from_usage(&usage(20, 1));
            assert_eq!(session.goal_status, Some(GoalStatus::NoProgress));

            assert!(session.goal_reset_no_progress());

            assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 0);
            assert_eq!(session.goal_status, Some(GoalStatus::Active));
            assert!(session.goal_can_pursue());
        }

        #[test]
        fn stop_goal_on_manual_abort_is_noop_without_goal() {
            let mut session = make_session();
            assert!(!session.stop_goal_on_manual_abort());
        }

        #[test]
        fn stop_goal_on_manual_abort_stops_pursuable_statuses() {
            for status in [
                GoalStatus::Active,
                GoalStatus::Verifying,
                GoalStatus::BudgetExhausted,
                GoalStatus::NoProgress,
            ] {
                let mut session = make_session();
                session.install_goal("agent", "ship it", true, budget());
                session.goal_set_status(status);

                assert!(session.stop_goal_on_manual_abort(), "{status:?}");

                assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
                assert!(session.messages.iter().any(|message| {
                    message.role == "event"
                        && message
                            .extra
                            .get("event")
                            .and_then(|event| event.get("payload"))
                            .and_then(|payload| payload.get("kind"))
                            .and_then(|kind| kind.as_str())
                            == Some("stopped")
                }));
            }
        }

        #[test]
        fn stop_goal_on_manual_abort_leaves_held_or_terminal_statuses_untouched() {
            for status in [
                GoalStatus::Paused,
                GoalStatus::Stopped,
                GoalStatus::Completed,
                GoalStatus::Transferred,
            ] {
                let mut session = make_session();
                session.install_goal("agent", "ship it", true, budget());
                session.goal_set_status(status);

                assert!(!session.stop_goal_on_manual_abort(), "{status:?}");
                assert_eq!(session.goal_status, Some(status));
            }
        }

        #[test]
        fn reactivate_goal_stopped_by_manual_abort_restores_active_status() {
            let mut session = make_session();
            session.install_goal("agent", "ship it", true, budget());
            assert!(session.stop_goal_on_manual_abort());

            assert!(session.reactivate_goal_stopped_by_manual_abort());

            assert_eq!(session.goal_status, Some(GoalStatus::Active));
            assert!(!session.goal_stopped_by_abort);
            assert!(session.messages.iter().any(|message| {
                message.role == "event"
                    && message
                        .extra
                        .get("event")
                        .and_then(|event| event.get("payload"))
                        .and_then(|payload| payload.get("kind"))
                        .and_then(|kind| kind.as_str())
                        == Some("resumed")
            }));
        }

        #[test]
        fn reactivate_goal_stopped_by_manual_abort_is_noop_without_marker() {
            let mut session = make_session();
            session.install_goal("agent", "ship it", true, budget());
            session.goal_set_status(GoalStatus::Stopped);

            assert!(!session.reactivate_goal_stopped_by_manual_abort());
            assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
        }

        #[test]
        fn explicit_goal_control_clears_manual_abort_marker() {
            let mut session = make_session();
            session.install_goal("agent", "ship it", true, budget());
            assert!(session.stop_goal_on_manual_abort());
            assert!(session.goal_stopped_by_abort);

            session.clear_goal_stopped_by_abort_marker();

            assert!(!session.goal_stopped_by_abort);
            assert!(!session.reactivate_goal_stopped_by_manual_abort());
            assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
        }

        #[test]
        fn verifier_attempt_counts_turn_and_tokens_without_no_progress() {
            let mut session = make_session();
            session.install_goal("agent", "ship it", true, budget());

            assert!(session.goal_record_verifier_attempt(42));

            let goal = session.goal.as_ref().unwrap();
            assert_eq!(goal.progress.turns_used, 1);
            assert_eq!(goal.progress.tokens_used, 42);
            assert_eq!(goal.progress.no_progress_turns, 0);
        }

        #[test]
        fn attempts_and_events_append_through_helpers() {
            let mut session = make_session();
            session.install_goal("agent", "ship it", true, budget());

            session.goal_push_attempt(GoalAttempt {
                at_ms: 10,
                trigger: "done".to_string(),
                verdict: "needs_work".to_string(),
                gaps: vec!["tests".to_string()],
                verifier_reply: "run tests".to_string(),
                criteria_verdicts: Vec::new(),
            });
            session.goal_push_event(GoalEvent {
                at_ms: 11,
                kind: "nudge".to_string(),
                text: "keep going".to_string(),
            });

            let goal = session.goal.as_ref().unwrap();
            assert_eq!(goal.attempts.len(), 1);
            assert_eq!(goal.events.len(), 1);
            assert_eq!(goal.attempts[0].gaps, vec!["tests".to_string()]);
            assert_eq!(goal.events[0].text, "keep going");
        }
    }

    mod goal_projection {
        use super::*;

        fn pursuit_event(at_ms: u64) -> ChatMessage {
            event(
                EventSubkind::GoalPursuit,
                "chat.goal_monitor",
                json!({
                    "kind": "nudge",
                    "trigger": "monitor",
                    "reason": "idle",
                    "at_ms": at_ms,
                    "account_progress": true,
                }),
                "Goal pursuit nudge: continue the active goal (monitor, idle).".to_string(),
            )
        }

        fn install(session: &mut ChatSession) {
            session.install_goal("agent", "ship it", true, GoalBudget::default());
        }

        fn pin_transfer_style_meta(session: &mut ChatSession) {
            let index = session
                .messages
                .iter()
                .position(|message| message.role == GOAL_ROLE)
                .unwrap();
            let meta = session.messages[index]
                .extra
                .get_mut("goal")
                .unwrap()
                .as_object_mut()
                .unwrap();
            meta.insert("status".to_string(), json!("active"));
            meta.insert("active".to_string(), json!(true));
            meta.insert(
                "progress".to_string(),
                json!({
                    "turns_used": 0,
                    "tokens_used": 0,
                    "started_at_ms": 111,
                    "no_progress_turns": 0,
                    "last_nudge_at_ms": 0,
                }),
            );
            meta.insert("attempts".to_string(), json!([]));
            meta.insert(
                "events".to_string(),
                json!([{
                    "at_ms": 1,
                    "kind": "goal_pursuit",
                    "text": "Goal ownership transferred from a to b.",
                }]),
            );
        }

        #[test]
        fn rebuild_preserves_live_stop_over_pinned_meta() {
            let mut session = make_session();
            install(&mut session);
            pin_transfer_style_meta(&mut session);
            session.goal_set_status(GoalStatus::Stopped);

            session.add_message(pursuit_event(2_000));

            assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
            assert_eq!(session.goal.as_ref().unwrap().status, GoalStatus::Stopped);
        }

        #[test]
        fn rebuild_preserves_live_progress_counters_over_pinned_meta() {
            let mut session = make_session();
            install(&mut session);
            pin_transfer_style_meta(&mut session);
            session.goal_record_nudge(5_000);
            session.goal_note_no_progress_turn();
            session.goal_note_no_progress_turn();

            session.add_message(pursuit_event(6_000));

            let goal = session.goal.as_ref().unwrap();
            assert_eq!(goal.progress.no_progress_turns, 2);
            assert_eq!(goal.progress.last_nudge_at_ms, 5_000);
        }

        #[test]
        fn stopped_goal_survives_reload_with_persisted_snapshot() {
            let mut session = make_session();
            install(&mut session);
            pin_transfer_style_meta(&mut session);
            session.goal_set_status(GoalStatus::Stopped);
            let persisted = session.goal.clone();

            let reloaded =
                goal_snapshot_from_messages(&session.messages, persisted.as_ref()).unwrap();

            assert_eq!(reloaded.status, GoalStatus::Stopped);
        }

        #[test]
        fn fresh_load_seeds_from_pinned_meta_without_prior() {
            let mut session = make_session();
            install(&mut session);
            pin_transfer_style_meta(&mut session);

            let seeded = goal_snapshot_from_messages(&session.messages, None).unwrap();

            assert!(seeded.active);
            assert_eq!(seeded.status, GoalStatus::Active);
            assert_eq!(seeded.progress.started_at_ms, 111);
            assert_eq!(seeded.events.len(), 1);
        }

        #[test]
        fn goal_events_union_accumulates_new_messages_and_dedups() {
            let mut session = make_session();
            install(&mut session);
            session.add_message(pursuit_event(1_000));
            assert_eq!(session.goal.as_ref().unwrap().events.len(), 1);

            session.add_message(pursuit_event(2_000));
            assert_eq!(session.goal.as_ref().unwrap().events.len(), 2);

            session.rebuild_goal_projection_from_messages();
            assert_eq!(session.goal.as_ref().unwrap().events.len(), 2);
        }

        #[test]
        fn coalesce_tail_goal_nudge_event_updates_payload_in_place() {
            let mut session = make_session();
            install(&mut session);
            session.add_message(pursuit_event(1_000));
            let messages_before = session.messages.len();

            assert!(session.coalesce_tail_goal_nudge_event(3_000));

            assert_eq!(session.messages.len(), messages_before);
            let payload = session.messages.last().unwrap().extra["event"]["payload"].clone();
            assert_eq!(payload["count"], json!(2));
            assert_eq!(payload["last_at_ms"], json!(3_000));
            assert_eq!(payload["at_ms"], json!(1_000));

            assert!(session.coalesce_tail_goal_nudge_event(4_000));
            let payload = session.messages.last().unwrap().extra["event"]["payload"].clone();
            assert_eq!(payload["count"], json!(3));
        }

        #[test]
        fn coalesce_requires_tail_nudge_event() {
            let mut session = make_session();
            install(&mut session);
            assert!(!session.coalesce_tail_goal_nudge_event(1_000));

            session.add_message(pursuit_event(1_000));
            session.add_message(ChatMessage {
                role: "assistant".to_string(),
                content: ChatContent::SimpleText("Idle.".to_string()),
                ..Default::default()
            });

            assert!(!session.coalesce_tail_goal_nudge_event(2_000));
        }
    }

    mod goal_ledger {
        use super::*;
        use refact_chat_api::{reduce_goal_ledger, GoalLedgerOp};

        fn install(session: &mut ChatSession) {
            session.install_goal("agent", "ship it", true, GoalBudget::default());
        }

        #[test]
        fn ledger_records_mutations_and_replay_matches_projection() {
            let mut session = make_session();
            install(&mut session);
            session.goal_record_progress_with_cost(120, true, 7);
            session.goal_note_no_progress_turn();
            session.goal_set_status_reason(GoalStatus::Stopped, "user stop");

            let state = reduce_goal_ledger(&session.goal_ledger).expect("ledger state");
            let goal = session.goal.as_ref().unwrap();

            assert_eq!(state.status, goal.status);
            assert_eq!(state.status, GoalStatus::Stopped);
            assert_eq!(state.stop_reason.as_deref(), Some("user stop"));
            assert_eq!(goal.stop_reason.as_deref(), Some("user stop"));
            assert_eq!(state.progress.turns_used, goal.progress.turns_used);
            assert_eq!(state.progress.tokens_used, 120);
            assert_eq!(state.progress.cost_used_cents, 7);
            assert_eq!(
                state.progress.no_progress_turns,
                goal.progress.no_progress_turns
            );
        }

        #[test]
        fn ledger_auto_installs_when_goal_message_arrives_without_helper() {
            let mut session = make_session();
            session.add_message(crate::chat::internal_roles::goal(
                "agent",
                1,
                "ship it",
                None,
                true,
                GoalBudget::default(),
            ));

            assert!(session.goal.is_some());
            assert!(session
                .goal_ledger
                .iter()
                .any(|entry| matches!(entry.op, GoalLedgerOp::Installed { version: 1, .. })));
        }

        #[test]
        fn ledger_status_changed_since_guards_verification_epoch() {
            let mut session = make_session();
            install(&mut session);
            session.goal_set_status(GoalStatus::Verifying);
            let epoch = session.goal_ledger_last_seq();

            assert!(!session.goal_status_changed_since(epoch));
            session.goal_record_nudge(5_000);
            assert!(!session.goal_status_changed_since(epoch));

            session.goal_set_status_reason(GoalStatus::Stopped, "goal_control");
            assert!(session.goal_status_changed_since(epoch));
        }

        #[test]
        fn evidence_marks_turn_as_progress_despite_tiny_completion() {
            let mut session = make_session();
            install(&mut session);
            session.add_message(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText("edited file".to_string()),
                tool_call_id: "call-1".to_string(),
                ..Default::default()
            });
            assert!(session.goal_turn_evidence);

            session.goal_record_progress_from_usage(&ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 3,
                total_tokens: 13,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                metering_usd: None,
            });

            let goal = session.goal.as_ref().unwrap();
            assert_eq!(goal.progress.no_progress_turns, 0);
            assert!(!session.goal_turn_evidence);
        }

        #[test]
        fn exempt_or_failed_tool_results_do_not_mark_evidence() {
            let mut session = make_session();
            install(&mut session);
            let tool_call = |id: &str, name: &str| crate::call_validation::ChatToolCall {
                id: id.to_string(),
                index: None,
                function: crate::call_validation::ChatToolFunction {
                    arguments: "{}".to_string(),
                    name: name.to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            };
            session.add_message(ChatMessage {
                role: "assistant".to_string(),
                content: ChatContent::SimpleText(String::new()),
                tool_calls: Some(vec![
                    tool_call("call-sleep", "sleep"),
                    tool_call("call-edit", "patch"),
                ]),
                ..Default::default()
            });

            session.add_message(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText("slept".to_string()),
                tool_call_id: "call-sleep".to_string(),
                ..Default::default()
            });
            assert!(!session.goal_turn_evidence);

            session.add_message(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText("boom".to_string()),
                tool_call_id: "call-edit".to_string(),
                tool_failed: Some(true),
                ..Default::default()
            });
            assert!(!session.goal_turn_evidence);

            session.add_message(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText("edited".to_string()),
                tool_call_id: "call-edit".to_string(),
                ..Default::default()
            });
            assert!(session.goal_turn_evidence);
        }

        #[test]
        fn repeated_nudges_coalesce_ledger_tail() {
            let mut session = make_session();
            install(&mut session);
            let before = session.goal_ledger.len();
            assert!(session.goal_record_nudge(1_000));
            assert!(session.goal_record_nudge(2_000));
            assert!(session.goal_record_nudge(3_000));
            assert_eq!(session.goal_ledger.len(), before + 1);
            let entry = session.goal_ledger.last().unwrap();
            assert!(matches!(entry.op, GoalLedgerOp::NudgeRecorded));
            assert_eq!(entry.at_ms, 3_000);
            assert_eq!(
                session.goal.as_ref().unwrap().progress.last_nudge_at_ms,
                3_000
            );
        }

        #[test]
        fn no_evidence_and_tiny_completion_counts_no_progress() {
            let mut session = make_session();
            install(&mut session);

            session.goal_record_progress_from_usage(&ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 3,
                total_tokens: 13,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                metering_usd: None,
            });

            assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 1);
        }

        #[test]
        fn max_cost_budget_exhausts_via_cost_accounting() {
            let mut session = make_session();
            session.install_goal(
                "agent",
                "ship it",
                true,
                GoalBudget {
                    max_cost_cents: Some(10),
                    ..Default::default()
                },
            );

            session.goal_record_progress_with_cost(5, true, 12);

            assert_eq!(session.goal_status, Some(GoalStatus::BudgetExhausted));
        }

        #[test]
        fn criteria_install_reaches_projection_and_ledger() {
            let mut session = make_session();
            session.install_goal_with_criteria(
                "agent",
                "ship it",
                true,
                GoalBudget::default(),
                vec![GoalCriterion {
                    id: "C1".to_string(),
                    text: "tests pass".to_string(),
                    verify_hint: None,
                }],
            );

            let goal = session.goal.as_ref().unwrap();
            assert_eq!(goal.criteria.len(), 1);
            assert_eq!(goal.criteria[0].id, "C1");
            let state = reduce_goal_ledger(&session.goal_ledger).unwrap();
            assert_eq!(state.criteria.len(), 1);

            session.rebuild_goal_projection_from_messages();
            assert_eq!(session.goal.as_ref().unwrap().criteria.len(), 1);
        }
    }

    #[test]
    fn install_goal_populates_snapshot_and_runtime_fields() {
        let mut session = make_session();
        let budget = GoalBudget {
            max_turns: Some(7),
            max_minutes: Some(8),
            max_tokens: Some(9),
            max_cost_cents: None,
            cooldown_ms: 10,
            no_progress_token_threshold: 11,
            no_progress_turns: Some(12),
            explicit: false,
        };

        session.install_goal("agent", "ship the card", true, budget.clone());

        let goal = session.goal.clone().expect("goal projection");
        assert_eq!(goal.content, "ship the card");
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.budget, budget);
        assert!(session.goal_active);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert_eq!(session.goal_turns_used, 0);
        match session.snapshot() {
            ChatEvent::Snapshot {
                goal: snapshot_goal,
                runtime,
                ..
            } => {
                assert_eq!(snapshot_goal, Some(goal));
                assert!(runtime.goal_active);
                assert_eq!(runtime.goal_status, Some(GoalStatus::Active));
            }
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    #[test]
    fn new_with_trajectory_rehydrates_goal_without_transfer() {
        let goal_message = crate::chat::internal_roles::goal(
            "agent",
            1,
            "finish the migration",
            None,
            true,
            GoalBudget::default(),
        );
        let persisted_goal = GoalSnapshot {
            content: "persisted content".to_string(),
            version: 1,
            active: true,
            status: GoalStatus::Active,
            budget: GoalBudget::default(),
            progress: GoalProgress {
                turns_used: 4,
                tokens_used: 1234,
                started_at_ms: 55,
                no_progress_turns: 1,
                last_nudge_at_ms: 77,
                cost_used_cents: 0,
            },
            attempts: Vec::new(),
            events: Vec::new(),
            criteria: Vec::new(),
            snoozed_until_ms: None,
            stop_reason: None,
            transferred_from: None,
            transferred_to: None,
        };

        let session = ChatSession::new_with_trajectory(
            "goal-reload".to_string(),
            vec![goal_message],
            ThreadParams::default(),
            "2024-01-01T00:00:00Z".to_string(),
            None,
            Vec::new(),
            Some(persisted_goal.clone()),
        );

        let goal = session.goal.expect("goal projection");
        assert_eq!(goal.content, "finish the migration");
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.progress, persisted_goal.progress);
        assert_eq!(goal.transferred_from, None);
        assert_eq!(goal.transferred_to, None);
        assert!(session.goal_active);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert_eq!(session.goal_turns_used, 4);
        assert_eq!(session.goal_tokens_used, 1234);
        assert_eq!(session.goal_no_progress_turns, 1);
        assert_eq!(session.event_seq, 0);
    }

    #[test]
    fn restored_active_goal_without_progress_keeps_unstarted_clock() {
        let goal_message = crate::chat::internal_roles::goal(
            "agent",
            1,
            "finish the migration",
            None,
            true,
            GoalBudget::default(),
        );

        let session = ChatSession::new_with_trajectory(
            "goal-unstarted-reload".to_string(),
            vec![goal_message],
            ThreadParams::default(),
            "2024-01-01T00:00:00Z".to_string(),
            None,
            Vec::new(),
            None,
        );

        let goal = session.goal.expect("goal projection");
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.progress.started_at_ms, 0);
    }

    #[test]
    fn restored_legacy_default_budget_heals_terminal_status() {
        let mut goal_message = crate::chat::internal_roles::goal(
            "agent",
            1,
            "finish the migration",
            None,
            true,
            GoalBudget::legacy_default_hard_limits(),
        );
        let goal_meta = goal_message.extra.get_mut("goal").unwrap();
        goal_meta["status"] = json!(GoalStatus::BudgetExhausted);

        let session = ChatSession::new_with_trajectory(
            "goal-legacy-budget-reload".to_string(),
            vec![goal_message],
            ThreadParams::default(),
            "2024-01-01T00:00:00Z".to_string(),
            None,
            Vec::new(),
            None,
        );

        let goal = session.goal.expect("goal projection");
        assert_eq!(goal.budget, GoalBudget::default());
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.progress.started_at_ms, 0);
    }

    #[test]
    fn restored_explicit_finite_exhausted_goal_stays_terminal() {
        let mut goal_message = crate::chat::internal_roles::goal(
            "agent",
            1,
            "finish the migration",
            None,
            true,
            GoalBudget {
                max_turns: Some(3),
                max_minutes: None,
                max_tokens: None,
                max_cost_cents: None,
                cooldown_ms: 1_500,
                no_progress_token_threshold: 50,
                no_progress_turns: None,
                explicit: true,
            },
        );
        let goal_meta = goal_message.extra.get_mut("goal").unwrap();
        goal_meta["status"] = json!(GoalStatus::BudgetExhausted);
        goal_meta["progress"] = json!(GoalProgress {
            turns_used: 3,
            ..Default::default()
        });

        let session = ChatSession::new_with_trajectory(
            "goal-explicit-budget-reload".to_string(),
            vec![goal_message],
            ThreadParams::default(),
            "2024-01-01T00:00:00Z".to_string(),
            None,
            Vec::new(),
            None,
        );

        let goal = session.goal.expect("goal projection");
        assert_eq!(goal.status, GoalStatus::BudgetExhausted);
        assert_eq!(goal.budget.max_turns, Some(3));
    }

    #[test]
    fn abort_preserves_goal_projection_and_runtime_mirror() {
        let mut session = make_session();
        session.install_goal("agent", "keep goal", true, GoalBudget::default());
        session.goal.as_mut().unwrap().progress.turns_used = 2;
        session.refresh_goal_runtime_mirror();

        session.abort_stream();

        assert!(session.goal.is_some());
        assert!(session.goal_active);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert_eq!(session.goal_turns_used, 2);
        assert_eq!(session.runtime.goal_turns_used, 2);
    }
