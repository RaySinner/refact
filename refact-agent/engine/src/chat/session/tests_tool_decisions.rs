use super::*;
use super::tests_support::*;
use crate::chat::perf_diagnostics;
use crate::call_validation::{ChatToolCall, ChatToolFunction};
use serde_json::json;
use std::time::Instant;

    #[test]
    fn perf_diagnostics_confirmation_wait_consumes_pause_once() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let mut session = make_session();
        let paused_at = Instant::now();
        session.confirmation_paused_at = Some((
            paused_at,
            HashSet::from(["tc1".to_string(), "tc2".to_string()]),
        ));

        session.complete_confirmation_wait_at(
            paused_at
                .checked_add(std::time::Duration::from_micros(55))
                .unwrap(),
        );
        session.complete_confirmation_wait_at(
            paused_at
                .checked_add(std::time::Duration::from_micros(110))
                .unwrap(),
        );

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].component, "tool.confirmation_wait");
        assert_eq!(events[0].elapsed_us, 55);
        assert_eq!(events[0].item_count, Some(2));
    }

    #[test]
    fn perf_diagnostics_partial_tool_decisions_preserve_pause_until_final_resolution() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let mut session = make_session();
        let paused_at = Instant::now();
        session.runtime.pause_reasons = vec![make_pause_reason("tc1"), make_pause_reason("tc2")];
        session.runtime.state = SessionState::Paused;
        let expected_ids = HashSet::from(["tc1".to_string(), "tc2".to_string()]);
        session.confirmation_paused_at = Some((paused_at, expected_ids.clone()));

        session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".to_string(),
            accepted: true,
        }]);

        assert_eq!(
            session.confirmation_paused_at,
            Some((paused_at, expected_ids))
        );
        assert!(sink
            .events()
            .iter()
            .all(|event| event.component != "tool.confirmation_wait"));

        session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc2".to_string(),
            accepted: false,
        }]);

        let events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event.component == "tool.confirmation_wait")
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].item_count, Some(2));
        assert!(session.confirmation_paused_at.is_none());
    }

    #[test]
    fn perf_diagnostics_overlapping_confirmation_uses_union_count() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let mut session = make_session();
        let paused_at = Instant::now();
        session.runtime.state = SessionState::Paused;
        session.runtime.pause_reasons = vec![make_pause_reason("tc1")];
        session.confirmation_paused_at = Some((paused_at, HashSet::from(["tc1".to_string()])));

        session.set_paused_with_reasons_and_auto_approved(
            vec![make_pause_reason("tc1"), make_pause_reason("tc2")],
            Vec::new(),
            None,
        );
        session.process_tool_decisions(&[
            ToolDecisionItem {
                tool_call_id: "tc1".to_string(),
                accepted: true,
            },
            ToolDecisionItem {
                tool_call_id: "tc2".to_string(),
                accepted: true,
            },
        ]);

        let waits: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event.component == PerfComponent::ToolConfirmationWait.as_str())
            .collect();
        assert_eq!(waits.len(), 1);
        assert_eq!(waits[0].item_count, Some(2));
    }

    #[test]
    fn unrelated_pause_starts_a_new_confirmation_episode() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, _) = install_perf_recorder();
        let mut session = make_session();
        let paused_at = Instant::now();
        session.runtime.state = SessionState::Paused;
        session.runtime.pause_reasons = vec![make_pause_reason("tc1")];
        session.confirmation_paused_at = Some((paused_at, HashSet::from(["tc1".to_string()])));

        session.set_paused_with_reasons_and_auto_approved(
            vec![make_pause_reason("tc2")],
            Vec::new(),
            None,
        );

        assert_ne!(
            session.confirmation_paused_at,
            Some((paused_at, HashSet::from(["tc1".to_string()])))
        );
        assert_eq!(
            session
                .confirmation_paused_at
                .map(|(_, tool_call_ids)| tool_call_ids.len()),
            Some(1)
        );
    }

    #[test]
    fn test_clear_pending_tool_calls_for_interruption_updates_last_assistant() {
        let mut session = make_session();
        session.add_message(ChatMessage {
            message_id: "assistant-with-tool".to_string(),
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("I'll use a tool".to_string()),
            tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                id: "call_1".to_string(),
                index: Some(0),
                tool_type: "function".to_string(),
                function: crate::call_validation::ChatToolFunction {
                    name: "shell".to_string(),
                    arguments: "{}".to_string(),
                },
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        });

        session.clear_pending_tool_calls_for_interruption();

        assert!(session.messages[0].tool_calls.is_none());
    }

    #[test]
    fn test_set_paused_with_reasons_and_auto_approved() {
        let mut session = make_session();
        let mut rx = session.subscribe();
        let reasons = vec![PauseReason {
            reason_type: "confirmation".into(),
            tool_name: "shell".into(),
            command: "shell".into(),
            rule: "ask".into(),
            tool_call_id: "tc1".into(),
            integr_config_path: None,
        }];
        session.set_paused_with_reasons_and_auto_approved(
            reasons.clone(),
            vec!["tc2".into()],
            Some(0),
        );
        assert_eq!(session.runtime.state, SessionState::Paused);
        assert_eq!(session.runtime.pause_reasons.len(), 1);
        assert_eq!(
            session.runtime.auto_approved_tool_ids,
            vec!["tc2".to_string()]
        );
        assert_eq!(session.runtime.paused_message_index, Some(0));
        let mut found_pause_required = false;
        while let Ok(json) = rx.try_recv() {
            if let Ok(env) = serde_json::from_str::<EventEnvelope>(&json) {
                if matches!(env.event, ChatEvent::PauseRequired { .. }) {
                    found_pause_required = true;
                }
            }
        }
        assert!(found_pause_required);
    }

    #[test]
    fn test_validate_tool_decision() {
        let mut session = make_session();
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc1".into(),
            integr_config_path: None,
        });
        assert!(session.validate_tool_decision("tc1"));
        assert!(!session.validate_tool_decision("unknown"));
    }

    #[test]
    fn test_process_tool_decisions_accepts() {
        let mut session = make_session();
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc1".into(),
            integr_config_path: None,
        });
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc2".into(),
            integr_config_path: None,
        });
        session.set_runtime_state(SessionState::Paused, None);
        let outcome = session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: true,
        }]);
        assert_eq!(outcome.accepted_ids, vec!["tc1"]);
        assert_eq!(session.runtime.pause_reasons.len(), 1);
        assert_eq!(session.runtime.state, SessionState::Paused);
    }

    #[test]
    fn process_tool_decisions_emits_runtime_updated_when_partial() {
        let mut session = make_session();
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc1".into(),
            integr_config_path: None,
        });
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc2".into(),
            integr_config_path: None,
        });
        session.set_runtime_state(SessionState::Paused, None);
        let mut rx = session.subscribe();
        let before = session.last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));

        session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: true,
        }]);

        assert_eq!(session.runtime.pause_reasons.len(), 1);
        assert!(session.last_activity > before);
        let mut saw_runtime_updated = false;
        let mut saw_pause_required = false;
        while let Ok(json) = rx.try_recv() {
            let env: EventEnvelope = serde_json::from_str(&json).unwrap();
            match env.event {
                ChatEvent::RuntimeUpdated { state, error, .. } => {
                    assert_eq!(state, SessionState::Paused);
                    assert_eq!(error, None);
                    saw_runtime_updated = true;
                }
                ChatEvent::PauseRequired { reasons } => {
                    assert_eq!(reasons.len(), 1);
                    assert_eq!(reasons[0].tool_call_id, "tc2");
                    saw_pause_required = true;
                }
                _ => {}
            }
        }
        assert!(saw_runtime_updated);
        assert!(saw_pause_required);
    }

    #[test]
    fn process_tool_decisions_no_change_no_event() {
        let mut session = make_session();
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc1".into(),
            integr_config_path: None,
        });
        session.set_runtime_state(SessionState::Paused, None);
        let mut rx = session.subscribe();
        let before = session.last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));

        let outcome = session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "unknown".into(),
            accepted: true,
        }]);

        assert!(outcome.accepted_ids.is_empty());
        assert_eq!(session.runtime.pause_reasons.len(), 1);
        assert_eq!(session.last_activity, before);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_process_tool_decisions_denies() {
        let mut session = make_session();
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc1".into(),
            integr_config_path: None,
        });
        session.set_runtime_state(SessionState::Paused, None);
        let outcome = session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: false,
        }]);
        assert!(outcome.accepted_ids.is_empty());
        assert_eq!(outcome.denied_ids, vec!["tc1"]);
        assert!(session.runtime.pause_reasons.is_empty());
        assert_eq!(session.runtime.state, SessionState::Paused);
    }

    #[test]
    fn test_process_tool_decisions_ignores_unknown() {
        let mut session = make_session();
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc1".into(),
            integr_config_path: None,
        });
        session.set_runtime_state(SessionState::Paused, None);
        let outcome = session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "unknown".into(),
            accepted: true,
        }]);
        assert!(outcome.accepted_ids.is_empty());
        assert_eq!(session.runtime.pause_reasons.len(), 1);
    }

    #[test]
    fn test_process_tool_decisions_leaves_final_state_to_caller() {
        let mut session = make_session();
        session.runtime.pause_reasons.push(PauseReason {
            reason_type: "test".into(),
            tool_name: "test_tool".into(),
            command: "cmd".into(),
            rule: "rule".into(),
            tool_call_id: "tc1".into(),
            integr_config_path: None,
        });
        session.set_runtime_state(SessionState::Paused, None);
        session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: true,
        }]);
        assert!(session.runtime.pause_reasons.is_empty());
        assert_eq!(session.runtime.state, SessionState::Paused);
    }

    #[test]
    fn denied_tool_call_produces_synthetic_tool_result_message() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1"]));
        session.runtime.pause_reasons.push(make_pause_reason("tc1"));
        session.set_runtime_state(SessionState::Paused, None);

        let outcome = session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: false,
        }]);

        assert_eq!(outcome.denied_ids, vec!["tc1"]);
        assert!(outcome.accepted_ids.is_empty());
        let tool_msgs: Vec<_> = session
            .messages
            .iter()
            .filter(|m| m.role == "tool")
            .collect();
        assert_eq!(tool_msgs.len(), 1);
        assert_eq!(tool_msgs[0].tool_call_id, "tc1");
        assert_eq!(
            tool_msgs[0].content,
            ChatContent::SimpleText("Tool call denied by user.".to_string())
        );
    }

    #[test]
    fn denied_then_accepted_in_same_decision_batch_keeps_accepted_running() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1", "tc2"]));
        session.runtime.pause_reasons.push(make_pause_reason("tc1"));
        session.runtime.pause_reasons.push(make_pause_reason("tc2"));
        session.set_runtime_state(SessionState::Paused, None);

        let outcome = session.process_tool_decisions(&[
            ToolDecisionItem {
                tool_call_id: "tc1".into(),
                accepted: false,
            },
            ToolDecisionItem {
                tool_call_id: "tc2".into(),
                accepted: true,
            },
        ]);

        assert_eq!(outcome.accepted_ids, vec!["tc2"]);
        assert_eq!(outcome.denied_ids, vec!["tc1"]);
        let tool_msgs: Vec<_> = session
            .messages
            .iter()
            .filter(|m| m.role == "tool")
            .collect();
        assert_eq!(tool_msgs.len(), 1);
        assert_eq!(tool_msgs[0].tool_call_id, "tc1");
    }

    #[test]
    fn all_denied_synthesizes_tool_result_and_leaves_state_to_caller() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1"]));
        session.runtime.pause_reasons.push(make_pause_reason("tc1"));
        session.set_runtime_state(SessionState::Paused, None);

        let outcome = session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: false,
        }]);

        assert_eq!(outcome.denied_ids, vec!["tc1"]);
        assert!(session.runtime.pause_reasons.is_empty());
        assert_eq!(session.runtime.state, SessionState::Paused);
        assert_eq!(
            session.messages.iter().filter(|m| m.role == "tool").count(),
            1
        );
    }

    #[test]
    fn denied_tool_call_unmatched_id_logs_warning_but_does_not_panic() {
        let mut session = make_session();
        session.runtime.pause_reasons.push(make_pause_reason("tc1"));
        session.set_runtime_state(SessionState::Paused, None);

        let outcome = session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: false,
        }]);

        assert_eq!(outcome.denied_ids, vec!["tc1"]);
        assert!(session.messages.iter().all(|m| m.role != "tool"));
        assert!(session.runtime.pause_reasons.is_empty());
        assert_eq!(session.runtime.state, SessionState::Paused);
    }

    #[test]
    fn synthesized_tool_result_message_has_correct_tool_call_id() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["unique-call-abc"]));
        session
            .runtime
            .pause_reasons
            .push(make_pause_reason("unique-call-abc"));
        session.set_runtime_state(SessionState::Paused, None);

        session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "unique-call-abc".into(),
            accepted: false,
        }]);

        let tool_msg = session
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .expect("no tool message synthesized");
        assert_eq!(tool_msg.tool_call_id, "unique-call-abc");
    }

    #[test]
    fn rejected_tool_decision_event_after_synthetic_tool_result() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1"]));
        session.runtime.pause_reasons.push(make_pause_reason("tc1"));
        session.set_runtime_state(SessionState::Paused, None);

        session.process_tool_decisions(&[ToolDecisionItem {
            tool_call_id: "tc1".into(),
            accepted: false,
        }]);

        assert_eq!(role_sequence(&session), vec!["assistant", "tool", "event"]);
        assert_eq!(session.messages[1].tool_call_id, "tc1");
        assert_eq!(
            session.messages[2].extra["event"]["subkind"],
            json!("tool_decision")
        );
        assert_eq!(
            session.messages[2].extra["event"]["payload"]["decision"],
            json!("reject")
        );
    }

    #[test]
    fn mixed_approve_reject_defers_event_until_accepted_tool_result() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1", "tc2"]));
        session.runtime.pause_reasons.push(make_pause_reason("tc1"));
        session.runtime.pause_reasons.push(make_pause_reason("tc2"));
        session.set_runtime_state(SessionState::Paused, None);

        session.process_tool_decisions(&[
            ToolDecisionItem {
                tool_call_id: "tc1".into(),
                accepted: false,
            },
            ToolDecisionItem {
                tool_call_id: "tc2".into(),
                accepted: true,
            },
        ]);

        assert_eq!(role_sequence(&session), vec!["assistant", "tool"]);
        assert_eq!(session.post_tool_side_effects.len(), 2);

        session.add_message(make_tool_result("tc2", "accepted result"));
        session.drain_post_tool_side_effects();

        assert_eq!(
            role_sequence(&session),
            vec!["assistant", "tool", "tool", "event", "event"]
        );
        assert_eq!(
            session.messages[3].extra["event"]["payload"]["decision"],
            json!("approve")
        );
        assert_eq!(
            session.messages[4].extra["event"]["payload"]["decision"],
            json!("reject")
        );
    }

    #[test]
    fn ide_callback_event_deferred_until_all_ide_tool_results_present() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1", "tc2"]));
        session.runtime.state = SessionState::WaitingIde;

        let completed =
            session.record_ide_tool_result("tc1".to_string(), "first".to_string(), false);

        assert!(!completed);
        assert_eq!(role_sequence(&session), vec!["assistant", "tool"]);
        assert_eq!(session.runtime.state, SessionState::WaitingIde);
        assert_eq!(session.post_tool_side_effects.len(), 1);

        let completed =
            session.record_ide_tool_result("tc2".to_string(), "second".to_string(), false);

        assert!(completed);
        assert_eq!(
            role_sequence(&session),
            vec!["assistant", "tool", "tool", "event", "event"]
        );
        assert_eq!(session.runtime.state, SessionState::WaitingIde);
        assert!(session.post_tool_side_effects.is_empty());
        assert_eq!(
            session.messages[3].extra["event"]["subkind"],
            json!("ide_callback")
        );
        assert_eq!(
            session.messages[4].extra["event"]["subkind"],
            json!("ide_callback")
        );
    }

    #[test]
    fn ide_tool_completion_ignores_stale_reused_tool_result_ids() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1"]));
        session.add_message(make_tool_result("tc1", "old result"));
        session.add_message(ChatMessage::new("user".to_string(), "again".to_string()));
        session.add_message(make_assistant_with_tool_calls(&["tc1", "tc2"]));
        session.runtime.state = SessionState::WaitingIde;

        let completed =
            session.record_ide_tool_result("tc2".to_string(), "second".to_string(), false);

        assert!(!completed);
        assert_eq!(session.post_tool_side_effects.len(), 1);
    }

    #[test]
    fn interruption_cleanup_ignores_stale_reused_tool_result_ids() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1"]));
        session.add_message(make_tool_result("tc1", "old result"));
        session.add_message(ChatMessage::new("user".to_string(), "again".to_string()));
        session.add_message(make_assistant_with_tool_calls(&["tc1", "tc2"]));
        session.add_message(make_tool_result("tc2", "current second result"));

        session.clear_pending_tool_calls_for_interruption();

        let latest_assistant = session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "assistant")
            .unwrap();
        let ids: Vec<_> = latest_assistant
            .tool_calls
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool_call| tool_call.id.as_str())
            .collect();
        assert_eq!(ids, vec!["tc2"]);
    }
