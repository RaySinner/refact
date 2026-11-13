#[allow(unused_imports)]
use super::*;
use crate::call_validation::ChatContent;
use crate::chat::internal_roles;
    use crate::chat::goal_monitor::{
        try_apply_goal_nudge, GoalNudgeConfig, GoalNudgeOutcome, GoalNudgeSkip, GoalNudgeTrigger,
    };
    use refact_chat_api::FrozenRequestPrefix;
    use refact_tool_api::{ToolSource, ToolSourceType};
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn session_with_goal() -> ChatSession {
        let mut session = ChatSession::new("goal-verifier-test".to_string());
        session.install_goal(
            "agent",
            "ship feature\n- tests pass",
            true,
            GoalBudget::default(),
        );
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session
    }

    fn finite_goal_budget() -> GoalBudget {
        GoalBudget {
            max_turns: Some(3),
            max_minutes: None,
            max_tokens: None,
            max_cost_cents: None,
            cooldown_ms: 1_500,
            no_progress_token_threshold: 50,
            no_progress_turns: Some(2),
            explicit: false,
        }
    }

    fn use_finite_goal_budget(session: &mut ChatSession) -> GoalBudget {
        let budget = finite_goal_budget();
        session.goal.as_mut().unwrap().budget = budget.clone();
        budget
    }

    fn tool_desc(name: &str) -> ToolDesc {
        ToolDesc {
            name: name.to_string(),
            display_name: name.to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: format!("{name} tool"),
            input_schema: json!({"type": "object", "properties": {}}),
            output_schema: None,
            annotations: None,
        }
    }

    fn drain_runtime_states(session: &mut ChatSession) -> Vec<SessionState> {
        let mut rx = session.subscribe();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session.goal_set_status(GoalStatus::Verifying);
        apply_goal_verdict(
            session,
            "task_done",
            GoalVerifierReply {
                verdict: GoalVerdict::Met,
                verifier_reply: "GOAL: MET".to_string(),
                tokens: 7,
            },
            None,
        );
        let mut states = Vec::new();
        while let Ok(raw) = rx.try_recv() {
            let envelope: EventEnvelope = serde_json::from_str(&raw).unwrap();
            if let ChatEvent::RuntimeUpdated { state, .. } = envelope.event {
                states.push(state);
            }
        }
        states
    }

    fn goal_pursuit_event_count(session: &ChatSession) -> usize {
        session
            .goal
            .as_ref()
            .map(|goal| {
                goal.events
                    .iter()
                    .filter(|event| event.kind == "goal_pursuit")
                    .count()
            })
            .unwrap_or_default()
    }

    fn reloaded_session_from_messages(session: &ChatSession) -> ChatSession {
        ChatSession::new_with_trajectory(
            "goal-verifier-reloaded".to_string(),
            session.messages.clone(),
            session.thread.clone(),
            session.created_at.clone(),
            None,
            vec![],
            None,
        )
    }

    #[test]
    fn selected_verdict_delivery_waits_for_tool_result_and_whole_turn() {
        for push in [PushMode::Preempt, PushMode::Append, PushMode::WhenIdle] {
            let mut session = session_with_goal();
            session.turn_depth = 1;
            session.add_message(ChatMessage {
                role: "assistant".into(),
                tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                    id: "validate".into(),
                    index: Some(0),
                    function: crate::call_validation::ChatToolFunction {
                        name: "validate_goal".into(),
                        arguments: "{}".into(),
                    },
                    tool_type: "function".into(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                }]),
                ..Default::default()
            });
            session.goal_set_status(GoalStatus::Verifying);
            let version = session.trajectory_version;
            let outcome = apply_goal_verdict_guarded_with_delivery(
                &mut session,
                "validate_goal",
                GoalVerifierReply {
                    verdict: GoalVerdict::Met,
                    verifier_reply: "GOAL: MET".into(),
                    tokens: 1,
                },
                None,
                version,
                push,
                Some("validate"),
            );
            assert_eq!(outcome, GoalVerificationApplyOutcome::Finalized);
            assert_eq!(session.pending_deliveries[0].push, push);
            assert!(!session.abort_flag.load(Ordering::SeqCst));
            session.add_message(ChatMessage {
                role: "tool".into(),
                tool_call_id: "validate".into(),
                ..Default::default()
            });
            session.set_runtime_state(SessionState::Idle, None);
            session.drain_pending_deliveries();
            assert_eq!(
                session.pending_deliveries.is_empty(),
                push != PushMode::WhenIdle
            );
            session.turn_depth = 0;
            session.drain_pending_deliveries();
            assert!(session.pending_deliveries.is_empty());
            assert_eq!(
                session.messages.last().unwrap().extra["event"]["payload"]["kind"],
                json!("verified")
            );
        }
    }

    #[test]
    fn goal_verification_message_contains_required_contract() {
        let message = goal_verification_message("Do the thing", &[]);
        let text = message.content.content_text_only();
        assert_eq!(message.role, "user");
        assert!(text.contains("[goal verification]"));
        assert!(text.contains("Do the thing"));
        assert!(text.contains("GOAL: MET"));
        assert!(text.contains("GOAL: UNMET"));
        assert!(text.contains("call NO tools"));
    }

    #[test]
    fn parse_goal_verdict_met() {
        assert_eq!(parse_goal_verdict("GOAL: MET", false), GoalVerdict::Met);
    }

    #[test]
    fn parse_goal_verdict_unmet_bullets() {
        assert_eq!(
            parse_goal_verdict("GOAL: UNMET\n- missing tests\n* docs", false),
            GoalVerdict::Unmet(vec!["missing tests".to_string(), "docs".to_string()])
        );
    }

    #[test]
    fn parse_goal_verdict_unmet_excludes_criterion_statuses_from_bullets() {
        assert_eq!(
            parse_goal_verdict(
                "GOAL: UNMET\nCRITERION C1: MET\nCRITERION C2: UNMET — docs missing\n- missing tests",
                false,
            ),
            GoalVerdict::Unmet(vec!["missing tests".to_string()])
        );
    }

    #[test]
    fn parse_goal_verdict_unmet_synthesizes_unmet_criterion_gaps() {
        assert_eq!(
            parse_goal_verdict(
                "GOAL: UNMET\nCRITERION C1: MET\nCRITERION C2: UNMET — docs missing\nCRITERION C3: UNMET",
                false,
            ),
            GoalVerdict::Unmet(vec!["C2: docs missing".to_string(), "C3: unmet".to_string()])
        );
    }

    #[test]
    fn parse_goal_verdict_met_with_criterion_statuses_is_met() {
        assert_eq!(
            parse_goal_verdict("GOAL: MET\nCRITERION C1: MET\nCRITERION C2: MET", false),
            GoalVerdict::Met
        );
    }

    #[test]
    fn parse_goal_verdict_tool_call_or_empty_is_unmet() {
        assert_eq!(
            parse_goal_verdict("GOAL: MET", true),
            GoalVerdict::Unmet(vec![INCONCLUSIVE_GAP.to_string()])
        );
        assert_eq!(
            parse_goal_verdict("", false),
            GoalVerdict::Unmet(vec![INCONCLUSIVE_GAP.to_string()])
        );
        assert_eq!(
            parse_goal_verdict("maybe", false),
            GoalVerdict::Unmet(vec![INCONCLUSIVE_GAP.to_string()])
        );
    }

    #[test]
    fn begin_goal_verification_sets_verifying_without_completed() {
        let mut session = session_with_goal();
        let mut rx = session.subscribe();

        let begin = begin_goal_verification_if_needed(&mut session);

        assert_eq!(begin, GoalVerificationBegin::Started);
        assert_eq!(session.goal_status, Some(GoalStatus::Verifying));
        let mut saw_verifying = false;
        while let Ok(raw) = rx.try_recv() {
            let envelope: EventEnvelope = serde_json::from_str(&raw).unwrap();
            if let ChatEvent::RuntimeUpdated {
                goal_status, state, ..
            } = envelope.event
            {
                assert_ne!(state, SessionState::Completed);
                if goal_status == Some(GoalStatus::Verifying) {
                    saw_verifying = true;
                }
            }
        }
        assert!(saw_verifying);
    }

    #[test]
    fn begin_goal_verification_skips_budget_exhausted_and_no_goal() {
        let mut no_goal = ChatSession::new("no-goal".to_string());
        assert_eq!(
            begin_goal_verification_if_needed(&mut no_goal),
            GoalVerificationBegin::NoActiveGoal
        );

        let mut exhausted = session_with_goal();
        let budget = use_finite_goal_budget(&mut exhausted);
        exhausted.goal.as_mut().unwrap().progress.turns_used = budget.max_turns.unwrap();
        assert_eq!(
            begin_goal_verification_if_needed(&mut exhausted),
            GoalVerificationBegin::BudgetExhausted
        );
    }

    #[tokio::test]
    async fn budget_exhausted_finish_records_terminal_status() {
        let app = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let mut session = session_with_goal();
        let budget = use_finite_goal_budget(&mut session);
        session.goal.as_mut().unwrap().progress.turns_used = budget.max_turns.unwrap();
        assert!(should_verify_goal_on_done(&session));
        let session_arc = Arc::new(AMutex::new(session));

        let outcome = verify_goal_before_completion(app, session_arc.clone(), "task_done").await;

        assert_eq!(
            outcome,
            GoalCompletionGateOutcome::BudgetExhausted(GoalStatus::BudgetExhausted)
        );
        let session = session_arc.lock().await;
        let goal = session.goal.as_ref().unwrap();
        assert_eq!(goal.status, GoalStatus::BudgetExhausted);
        assert!(goal.attempts.is_empty());
        assert!(goal.events.iter().any(|event| {
            event.kind == "goal_pursuit" && event.text.contains("budget_exhausted")
        }));
        assert!(!session.messages.iter().any(|message| {
            message
                .extra
                .get("event")
                .and_then(|event| event.get("payload"))
                .and_then(|payload| payload.get("kind"))
                .and_then(|kind| kind.as_str())
                == Some("verified")
        }));
    }

    #[tokio::test]
    async fn no_progress_exhausted_finish_records_terminal_status() {
        let app = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let mut session = session_with_goal();
        let budget = use_finite_goal_budget(&mut session);
        session.goal.as_mut().unwrap().progress.no_progress_turns =
            budget.no_progress_turns.unwrap();
        assert!(should_verify_goal_on_done(&session));
        let session_arc = Arc::new(AMutex::new(session));

        let outcome = verify_goal_before_completion(app, session_arc.clone(), "task_done").await;

        assert_eq!(
            outcome,
            GoalCompletionGateOutcome::BudgetExhausted(GoalStatus::NoProgress)
        );
        let session = session_arc.lock().await;
        let goal = session.goal.as_ref().unwrap();
        assert_eq!(goal.status, GoalStatus::NoProgress);
        assert!(goal
            .events
            .iter()
            .any(|event| { event.kind == "goal_pursuit" && event.text.contains("no_progress") }));
    }

    #[test]
    fn verifier_goal_text_uses_synthesized_goal_with_deltas() {
        let mut session = session_with_goal();
        session.add_message(internal_roles::goal_delta(
            "tool.update_goal",
            json!({"seq": 1}),
            "also update docs",
        ));

        let inputs = goal_verifier_prepare_inputs(&session, vec![]).unwrap();
        let verifier_text = inputs.messages.last().unwrap().content.content_text_only();

        assert!(verifier_text.contains("ship feature"));
        assert!(verifier_text.contains("## Goal updates"));
        assert!(verifier_text.contains("also update docs"));
        assert_eq!(
            goal_text_for_verifier(&session).unwrap(),
            crate::chat::goal_role::synthesize_current_goal(&session).unwrap()
        );
    }

    #[tokio::test]
    async fn verifier_cancel_uses_user_interrupt_not_existing_finish_abort() {
        let session_arc = Arc::new(AMutex::new(session_with_goal()));
        {
            let session = session_arc.lock().await;
            session.abort_flag.store(true, Ordering::SeqCst);
        }

        assert!(!verifier_cancel_requested(&session_arc).await);
        {
            let session = session_arc.lock().await;
            assert!(session.abort_flag.load(Ordering::SeqCst));
            session.user_interrupt_flag.store(true, Ordering::SeqCst);
        }
        assert!(verifier_cancel_requested(&session_arc).await);
    }

    #[test]
    fn verifier_fork_uses_active_history_only() {
        let mut session = session_with_goal();
        session.add_message(ChatMessage::new(
            "assistant".into(),
            "ARCHIVED_VERIFIER_SENTINEL".into(),
        ));
        let mut active = ChatMessage::new("user".into(), "active evidence".into());
        active.message_id = "active-evidence".into();
        let boundary = refact_core::active_context::make_reconstruction_report(
            vec![active],
            Default::default(),
        )
        .unwrap();
        session.messages.push(boundary);
        let inputs = goal_verifier_prepare_inputs(&session, vec![]).unwrap();
        let serialized = serde_json::to_string(&inputs.messages).unwrap();
        assert!(serialized.contains("active evidence"));
        assert!(!serialized.contains("ARCHIVED_VERIFIER_SENTINEL"));
        assert!(session.messages.iter().any(|m| m
            .content
            .content_text_only()
            .contains("ARCHIVED_VERIFIER_SENTINEL")));
    }

    #[test]
    fn verifier_fork_rejects_legacy_without_migrating() {
        let mut session = session_with_goal();
        let mut legacy = ChatMessage::new("assistant".into(), "summary".into());
        legacy.summarized_range = Some((0, 0));
        session.messages.push(legacy);
        assert!(goal_verifier_prepare_inputs(&session, vec![]).is_err());
        assert_eq!(
            session.messages.last().unwrap().summarized_range,
            Some((0, 0))
        );
    }

    #[test]
    fn verifier_prepare_inputs_preserve_parent_cache_and_messages() {
        let mut session = session_with_goal();
        session.thread.frozen_request_prefix = Some(FrozenRequestPrefix {
            schema_version: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            system_prompt: Some("cached system".to_string()),
            tools_canonical: Some(json!([{"name": "finish"}])),
        });
        session.add_message(ChatMessage {
            message_id: "user-1".to_string(),
            role: "user".to_string(),
            content: ChatContent::SimpleText("please ship".to_string()),
            ..Default::default()
        });
        session.add_message(ChatMessage {
            message_id: "assistant-1".to_string(),
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("working".to_string()),
            thinking_blocks: Some(vec![json!({"type": "thinking", "signature": "sig-1"})]),
            ..Default::default()
        });
        let original_messages = session.messages.clone();
        let tools = vec![tool_desc("task_done")];

        let inputs = goal_verifier_prepare_inputs(&session, tools.clone()).unwrap();

        assert_eq!(inputs.messages.len(), original_messages.len() + 1);
        for (left, right) in inputs.messages.iter().zip(original_messages.iter()) {
            assert_eq!(left.message_id, right.message_id);
            assert_eq!(left.role, right.role);
            assert_eq!(
                left.content.content_text_only(),
                right.content.content_text_only()
            );
            assert_eq!(left.thinking_blocks, right.thinking_blocks);
        }
        let appended = inputs.messages.last().unwrap();
        assert_eq!(appended.role, "user");
        assert!(appended.extra.contains_key("goal_verification"));
        assert_eq!(
            inputs.options.frozen_request_prefix,
            session.thread.frozen_request_prefix
        );
        assert!(!inputs.options.prepend_system_prompt);
        assert!(!inputs.options.allow_at_commands);
        assert!(!inputs.options.allow_tool_prerun);
        assert!(matches!(inputs.options.tool_choice, Some(ToolChoice::None)));
        assert_eq!(inputs.options.parallel_tool_calls, Some(false));
        assert_eq!(
            serde_json::to_value(&inputs.tools).unwrap(),
            serde_json::to_value(&tools).unwrap()
        );
    }

    #[test]
    fn validate_delivery_uses_selected_push_after_own_result() {
        for push in [PushMode::Preempt, PushMode::Append, PushMode::WhenIdle] {
            let mut session = session_with_goal();
            session.turn_depth = 1;
            session.goal_set_status(GoalStatus::Verifying);
            session.add_message(ChatMessage {
                role: "assistant".into(),
                tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                    id: "validate".into(),
                    index: Some(0),
                    function: crate::call_validation::ChatToolFunction {
                        name: "validate_goal".into(),
                        arguments: "{}".into(),
                    },
                    tool_type: "function".into(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                }]),
                ..Default::default()
            });
            let version = session.trajectory_version;
            let outcome = apply_goal_verdict_guarded_with_delivery(
                &mut session,
                "validate_goal",
                GoalVerifierReply {
                    verdict: GoalVerdict::Unmet(vec!["tests".into()]),
                    verifier_reply: "GOAL: UNMET".into(),
                    tokens: 1,
                },
                None,
                version,
                push,
                Some("validate"),
            );
            assert_eq!(outcome, GoalVerificationApplyOutcome::Continued);
            assert_eq!(session.pending_deliveries.len(), 1);
            assert_eq!(session.pending_deliveries[0].push, push);
            assert!(!session.abort_flag.load(Ordering::SeqCst));
            session.add_message(ChatMessage {
                role: "tool".into(),
                tool_call_id: "validate".into(),
                ..Default::default()
            });
            session.set_runtime_state(SessionState::Idle, None);
            session.drain_pending_deliveries();
            assert_eq!(
                session.pending_deliveries.is_empty(),
                push != PushMode::WhenIdle
            );
            session.turn_depth = 0;
            session.drain_pending_deliveries();
            assert!(session.pending_deliveries.is_empty());
            assert_eq!(
                session.messages.last().unwrap().extra["event"]["payload"]["kind"],
                "verification_gaps"
            );
        }
    }

    #[test]
    fn apply_goal_verdict_met_finalizes_completed_and_records_attempt() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = apply_goal_verdict(
            &mut session,
            "task_done",
            GoalVerifierReply {
                verdict: GoalVerdict::Met,
                verifier_reply: "GOAL: MET".to_string(),
                tokens: 11,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Finalized);
        assert_eq!(session.runtime.state, SessionState::Completed);
        let goal = session.goal.as_ref().unwrap();
        assert_eq!(goal.status, GoalStatus::Completed);
        assert_eq!(goal.progress.turns_used, 1);
        assert_eq!(goal.progress.tokens_used, 11);
        assert_eq!(goal.attempts.len(), 1);
        assert_eq!(goal.attempts[0].verifier_reply, "GOAL: MET");
        assert!(session.messages.iter().any(|message| {
            message
                .extra
                .get("event")
                .and_then(|event| event.get("payload"))
                == Some(&json!({"kind": "verified", "at_ms": goal.attempts[0].at_ms, "gaps": []}))
        }));
    }

    #[test]
    fn apply_goal_verdict_met_from_validate_goal_keeps_chat_running() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = apply_goal_verdict(
            &mut session,
            "validate_goal",
            GoalVerifierReply {
                verdict: GoalVerdict::Met,
                verifier_reply: "GOAL: MET".to_string(),
                tokens: 11,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Finalized);
        assert_eq!(session.runtime.state, SessionState::ExecutingTools);
        assert_eq!(session.goal_status, Some(GoalStatus::Completed));
        assert!(session.command_queue.is_empty());
    }

    #[test]
    fn apply_goal_verdict_unmet_rearms_and_requests_delivery_wake() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = apply_goal_verdict(
            &mut session,
            "task_done",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["missing test".to_string()]),
                verifier_reply: "GOAL: UNMET\n- missing test".to_string(),
                tokens: 13,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Rearmed);
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert_eq!(
            session.goal.as_ref().unwrap().attempts[0].gaps,
            vec!["missing test"]
        );
        assert_eq!(
            session.goal.as_ref().unwrap().attempts[0].verifier_reply,
            "GOAL: UNMET\n- missing test"
        );
        assert!(!session.delivery_wake_sources.is_empty());
        assert!(session.command_queue.is_empty());
    }

    #[test]
    fn apply_goal_verdict_unmet_preserves_stop_requested_during_verification() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);
        session.goal_set_status(GoalStatus::Stopped);

        let outcome = apply_goal_verdict(
            &mut session,
            "finish",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["missing test".to_string()]),
                verifier_reply: "GOAL: UNMET\n- missing test".to_string(),
                tokens: 13,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Continued);
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
        assert!(!session
            .command_queue
            .iter()
            .any(|request| matches!(request.command, ChatCommand::Regenerate {})));
    }

    #[test]
    fn apply_goal_verdict_unmet_counts_no_progress_and_stalls_at_cap() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);

        apply_goal_verdict(
            &mut session,
            "finish",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["missing".to_string()]),
                verifier_reply: "GOAL: UNMET\n- missing".to_string(),
                tokens: 5,
            },
            None,
        );
        assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 1);

        session.command_queue.clear();
        session.goal.as_mut().unwrap().progress.no_progress_turns = 2;
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = apply_goal_verdict(
            &mut session,
            "finish",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["still missing".to_string()]),
                verifier_reply: "GOAL: UNMET\n- still missing".to_string(),
                tokens: 5,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Stalled);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 3);
        assert!(session.command_queue.is_empty());
    }

    #[test]
    fn verifier_failure_returns_unavailable_without_fabricated_unmet() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = handle_verifier_failure(&mut session, "finish", "model not found");

        assert_eq!(outcome, GoalCompletionGateOutcome::VerificationUnavailable);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        let goal = session.goal.as_ref().unwrap();
        assert!(goal.attempts.is_empty());
        assert!(session.command_queue.is_empty());
        let blocked_events = session
            .messages
            .iter()
            .filter(|message| {
                message
                    .extra
                    .get("event")
                    .and_then(|event| event.get("payload"))
                    .and_then(|payload| payload.get("kind"))
                    .and_then(|kind| kind.as_str())
                    == Some("verification_blocked")
            })
            .count();
        assert_eq!(blocked_events, 1);
        assert!(goal
            .events
            .iter()
            .any(|event| event.text.contains("Goal verification unavailable")));
    }

    #[test]
    fn verifier_failure_preserves_stop_requested_during_verification() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);
        session.goal_set_status(GoalStatus::Stopped);

        let outcome = handle_verifier_failure(&mut session, "finish", "network error");

        assert_eq!(outcome, GoalCompletionGateOutcome::VerificationUnavailable);
        assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
        assert!(session.command_queue.is_empty());
        assert!(session.goal_verification_blocked_until_ms.is_some());
    }

    #[test]
    fn apply_goal_verdict_discards_verdict_when_status_changed_after_epoch() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);
        let epoch = session.goal_ledger_last_seq();
        session.goal_set_status_reason(GoalStatus::Stopped, "goal_control");
        let attempts_before = session.goal.as_ref().unwrap().attempts.len();

        let outcome = apply_goal_verdict(
            &mut session,
            "finish",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["missing".to_string()]),
                verifier_reply: "GOAL: UNMET\n- missing".to_string(),
                tokens: 5,
            },
            Some(epoch),
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Superseded);
        assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
        assert_eq!(
            session.goal.as_ref().unwrap().attempts.len(),
            attempts_before
        );
        assert!(session.command_queue.is_empty());
    }

    #[test]
    fn parse_criteria_verdicts_extracts_met_and_unmet_lines() {
        let verdicts = parse_criteria_verdicts(
            "GOAL: UNMET\n- docs missing\nCRITERION C1: MET\nCRITERION C2: UNMET — docs not updated\nnoise line",
        );

        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].id, "C1");
        assert!(verdicts[0].met);
        assert_eq!(verdicts[1].id, "C2");
        assert!(!verdicts[1].met);
        assert_eq!(verdicts[1].note, "docs not updated");
    }

    #[test]
    fn goal_verification_message_lists_structured_criteria() {
        let criteria = vec![
            GoalCriterion {
                id: "C1".to_string(),
                text: "tests pass".to_string(),
                verify_hint: Some("cargo test".to_string()),
            },
            GoalCriterion {
                id: "C2".to_string(),
                text: "docs updated".to_string(),
                verify_hint: None,
            },
        ];

        let message = goal_verification_message("ship it", &criteria);
        let text = message.content.content_text_only();

        assert!(text.contains("Structured criteria:"));
        assert!(text.contains("- [C1] tests pass (verify: cargo test)"));
        assert!(text.contains("- [C2] docs updated"));
        assert!(text.contains("CRITERION <id>: MET"));

        let plain = goal_verification_message("ship it", &[]);
        assert!(!plain
            .content
            .content_text_only()
            .contains("Structured criteria:"));
    }

    #[test]
    fn apply_goal_verdict_unmet_preserves_pause_requested_during_verification() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);
        session.goal_set_status(GoalStatus::Paused);

        let outcome = apply_goal_verdict(
            &mut session,
            "finish",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["missing test".to_string()]),
                verifier_reply: "GOAL: UNMET\n- missing test".to_string(),
                tokens: 13,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Continued);
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_eq!(session.goal_status, Some(GoalStatus::Paused));
        assert!(!session
            .command_queue
            .iter()
            .any(|request| matches!(request.command, ChatCommand::Regenerate {})));
    }

    #[test]
    fn apply_goal_verdict_validate_goal_unmet_preserves_stop_requested_during_verification() {
        let mut session = session_with_goal();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session.goal_set_status(GoalStatus::Verifying);
        session.goal_set_status(GoalStatus::Stopped);

        let outcome = apply_goal_verdict(
            &mut session,
            "validate_goal",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["missing test".to_string()]),
                verifier_reply: "GOAL: UNMET\n- missing test".to_string(),
                tokens: 13,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Continued);
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
        assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 0);
    }

    #[test]
    fn apply_goal_verdict_validate_goal_unmet_continues_without_regenerate() {
        let mut session = session_with_goal();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = apply_goal_verdict(
            &mut session,
            "validate_goal",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["missing test".to_string()]),
                verifier_reply: "GOAL: UNMET\n- missing test".to_string(),
                tokens: 13,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Continued);
        assert_eq!(session.runtime.state, SessionState::ExecutingTools);
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
        let goal = session.goal.as_ref().unwrap();
        assert_eq!(goal.progress.turns_used, 1);
        assert_eq!(goal.progress.tokens_used, 13);
        assert_eq!(goal.progress.no_progress_turns, 1);
        assert!(session.command_queue.is_empty());
        assert!(session.post_tool_side_effects.is_empty());
        assert_eq!(
            session.messages.last().unwrap().extra["event"]["payload"]["kind"],
            json!("verification_gaps")
        );
    }

    #[test]
    fn validate_goal_unmet_records_single_goal_pursuit_event_after_rebuild() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = apply_goal_verdict(
            &mut session,
            "validate_goal",
            GoalVerifierReply {
                verdict: GoalVerdict::Unmet(vec!["missing test".to_string()]),
                verifier_reply: "GOAL: UNMET\n- missing test".to_string(),
                tokens: 13,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Continued);
        assert_eq!(goal_pursuit_event_count(&session), 1);
        session.drain_post_tool_side_effects();
        assert_eq!(goal_pursuit_event_count(&session), 1);
        session.rebuild_goal_projection_from_messages();
        assert_eq!(goal_pursuit_event_count(&session), 1);
        let reloaded = reloaded_session_from_messages(&session);
        assert_eq!(goal_pursuit_event_count(&reloaded), 1);
    }

    #[test]
    fn validate_goal_met_records_single_goal_pursuit_event_after_rebuild() {
        let mut session = session_with_goal();
        session.goal_set_status(GoalStatus::Verifying);

        let outcome = apply_goal_verdict(
            &mut session,
            "validate_goal",
            GoalVerifierReply {
                verdict: GoalVerdict::Met,
                verifier_reply: "GOAL: MET".to_string(),
                tokens: 11,
            },
            None,
        );

        assert_eq!(outcome, GoalVerificationApplyOutcome::Finalized);
        assert_eq!(goal_pursuit_event_count(&session), 1);
        session.drain_post_tool_side_effects();
        assert_eq!(goal_pursuit_event_count(&session), 1);
        session.rebuild_goal_projection_from_messages();
        assert_eq!(goal_pursuit_event_count(&session), 1);
        let reloaded = reloaded_session_from_messages(&session);
        assert_eq!(goal_pursuit_event_count(&reloaded), 1);
    }

    #[test]
    fn repeated_validate_goal_unmet_reaches_quiescence_without_regenerate() {
        let mut session = session_with_goal();
        for idx in 0..3 {
            session.goal_set_status(GoalStatus::Verifying);
            let outcome = apply_goal_verdict(
                &mut session,
                "validate_goal",
                GoalVerifierReply {
                    verdict: GoalVerdict::Unmet(vec![format!("missing test {idx}")]),
                    verifier_reply: "GOAL: UNMET\n- missing test".to_string(),
                    tokens: 0,
                },
                None,
            );
            assert_eq!(outcome, GoalVerificationApplyOutcome::Continued);
        }
        assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 3);
        assert!(session.command_queue.is_empty());

        let now = Instant::now();
        session.set_runtime_state(SessionState::Idle, None);
        session.last_activity = now - Duration::from_secs(10);
        let outcome = try_apply_goal_nudge(
            &mut session,
            GoalNudgeTrigger::Monitor,
            10_000,
            now,
            GoalNudgeConfig {
                stall_grace: Duration::from_secs(5),
                no_token_grace: Duration::from_secs(5),
            },
        );

        assert_eq!(outcome, GoalNudgeOutcome::Skipped(GoalNudgeSkip::Quiescent));
        assert!(session.command_queue.is_empty());
        assert_eq!(session.goal_status, Some(GoalStatus::Active));
    }

    #[test]
    fn no_completed_status_emitted_before_met_verdict() {
        let mut session = session_with_goal();
        let states = drain_runtime_states(&mut session);
        let completed_idx = states
            .iter()
            .position(|state| *state == SessionState::Completed)
            .unwrap();
        assert!(states[..completed_idx]
            .iter()
            .all(|state| *state != SessionState::Completed));
        assert!(states
            .iter()
            .any(|state| *state == SessionState::ExecutingTools));
    }

    #[test]
    fn finish_with_no_goal_is_passthrough() {
        let mut session = ChatSession::new("plain".to_string());
        assert_eq!(
            begin_goal_verification_if_needed(&mut session),
            GoalVerificationBegin::NoActiveGoal
        );
        assert!(!should_verify_goal_on_done(&session));
    }
