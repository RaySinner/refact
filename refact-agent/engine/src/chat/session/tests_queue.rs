use super::*;
use super::tests_support::*;
use super::super::types::{ChatCommand, CommandRequest};
use crate::chat::perf_diagnostics;
use crate::call_validation::{ChatToolCall, ChatToolFunction};
use serde_json::json;
use std::time::Instant;

    #[test]
    fn delivery_goal_purge_preserves_unrelated_landed_wake() {
        for reverse in [false, true] {
            let mut session = make_session();
            let mut sources = vec!["chat.goal_monitor", "process.subscribe"];
            if reverse {
                sources.reverse();
            }
            for source in sources {
                let mut delivery = delivery_fixture(source, PushMode::Append);
                delivery.source = source.into();
                delivery.wake = true;
                session.enqueue_delivery(delivery).unwrap();
            }
            session.purge_goal_generated_wakes();
            assert_eq!(session.delivery_wake_sources, HashSet::from(["external"]));
            assert_eq!(session.messages.len(), 2);
        }
        let mut session = make_session();
        let mut delivery = delivery_fixture("goal", PushMode::Append);
        delivery.source = "chat.goal_verifier".into();
        delivery.wake = true;
        session.enqueue_delivery(delivery).unwrap();
        session.purge_goal_generated_wakes();
        assert!(session.delivery_wake_sources.is_empty());
        assert_eq!(session.messages.len(), 1);
    }

    #[test]
    fn delivery_manual_abort_suppresses_existing_wakes_but_accepts_future_wake() {
        let mut session = make_session();
        let mut landed = delivery_fixture("landed", PushMode::Append);
        landed.wake = true;
        session.enqueue_delivery(landed).unwrap();
        session.turn_depth = 1;
        let mut queued = delivery_fixture("queued", PushMode::WhenIdle);
        queued.wake = true;
        session.enqueue_delivery(queued.clone()).unwrap();
        session.suppress_delivery_wakes();
        session.abort_stream();
        assert!(session.delivery_wake_sources.is_empty());
        assert_eq!(session.pending_deliveries.len(), 1);
        assert!(!session.pending_deliveries[0].wake);
        session.turn_depth = 0;
        assert_eq!(
            session.drain_pending_deliveries(),
            (vec!["queued".into()], false)
        );
        assert_eq!(session.messages.len(), 2);
        assert_eq!(
            session.enqueue_delivery(queued).unwrap(),
            DeliveryOutcome::Duplicate
        );
        assert!(session.delivery_wake_sources.is_empty());
        let mut future = delivery_fixture("future", PushMode::Append);
        future.wake = true;
        session.enqueue_delivery(future).unwrap();
        assert!(!session.delivery_wake_sources.is_empty());
    }

    #[test]
    fn delivery_post_tool_policies_and_ui_edits_preserve_protocol() {
        for push in [PushMode::Preempt, PushMode::Append, PushMode::WhenIdle] {
            let mut session = make_session();
            session.turn_depth = 1;
            session.start_stream().unwrap();
            session.emit_stream_delta(vec![DeltaOp::SetToolCalls {
                tool_calls: vec![json!({"id":"sleep","type":"function","function":{"name":"sleep","arguments":"{}"}})],
            }]);
            session.finish_stream_with_next_state(
                Some("tool_calls".into()),
                SessionState::ExecutingTools,
            );
            assert_eq!(
                session
                    .queue_post_tool_delivery(delivery_fixture("tick", push))
                    .unwrap(),
                DeliveryOutcome::Queued
            );
            let saved = serde_json::to_value(session.pending_deliveries_for_snapshot()).unwrap();
            assert_eq!(saved[0]["after_tool_call_id"], "sleep");
            assert!(!session
                .update_pending_delivery("tick", Some(PushMode::Preempt), false)
                .unwrap());
            assert!(session.drain_pending_deliveries().0.is_empty());
            assert!(!session.abort_flag.load(Ordering::SeqCst));
            session
                .update_pending_delivery("tick", Some(push), false)
                .unwrap();
            session.add_message(ChatMessage {
                role: "tool".into(),
                tool_call_id: "sleep".into(),
                content: ChatContent::SimpleText("slept".into()),
                ..Default::default()
            });
            if push == PushMode::WhenIdle {
                assert!(session.drain_pending_deliveries().0.is_empty());
                session.turn_depth = 0;
                session.set_runtime_state(SessionState::Idle, None);
            }
            assert_eq!(session.drain_pending_deliveries().0, vec!["tick"]);
            assert_eq!(session.messages[1].role, "tool");
            assert_eq!(session.messages[1].content.content_text_only(), "slept");
            assert!(session.pending_deliveries.is_empty());
        }
    }

    #[test]
    fn interruptible_wait_delivery_closes_window_before_fifo_release() {
        for push in [PushMode::Append, PushMode::WhenIdle] {
            for already_queued in [false, true] {
                let mut session = make_session();
                session.turn_depth = 1;
                session.start_stream().unwrap();
                session.emit_stream_delta(vec![DeltaOp::SetToolCalls {
                    tool_calls: vec![
                        json!({"id":"wait","type":"function","function":{"name":"sleep","arguments":"{}"}}),
                        json!({"id":"work","type":"function","function":{"name":"shell","arguments":"{}"}}),
                    ],
                }]);
                session.finish_stream_with_next_state(
                    Some("tool_calls".into()),
                    SessionState::ExecutingTools,
                );
                if already_queued {
                    session
                        .enqueue_delivery(delivery_fixture("first", push))
                        .unwrap();
                }
                session.begin_interruptible_wait();
                assert!(session.runtime.waiting_interruptible);
                assert_eq!(
                    serde_json::to_value(&session.runtime).unwrap()["waiting_interruptible"],
                    true
                );
                if !already_queued {
                    session
                        .enqueue_delivery(delivery_fixture("first", push))
                        .unwrap();
                }
                session
                    .enqueue_delivery(delivery_fixture("second", PushMode::Append))
                    .unwrap();
                assert!(session.wait_interrupt_flag.load(Ordering::SeqCst));
                assert!(!session.abort_flag.load(Ordering::SeqCst));
                assert!(!session.user_interrupt_flag.load(Ordering::SeqCst));
                session.end_interruptible_wait();
                assert!(!session.runtime.waiting_interruptible);
                for id in ["wait", "work"] {
                    session.add_message(ChatMessage {
                        role: "tool".into(),
                        tool_call_id: id.into(),
                        content: ChatContent::SimpleText("result".into()),
                        ..Default::default()
                    });
                    if id == "wait" {
                        assert!(session.drain_pending_deliveries().0.is_empty());
                    }
                }
                assert_eq!(
                    session.drain_pending_deliveries(),
                    (vec!["first".into(), "second".into()], true)
                );
                assert_eq!(session.drain_pending_deliveries(), (vec![], false));
                assert!(!session.wait_delivery_boundary);
                assert_eq!(
                    session
                        .messages
                        .iter()
                        .map(|m| m.role.as_str())
                        .collect::<Vec<_>>(),
                    vec!["assistant", "tool", "tool", "event", "event"]
                );
                session
                    .enqueue_delivery(delivery_fixture("later", PushMode::WhenIdle))
                    .unwrap();
                assert!(session.drain_pending_deliveries().0.is_empty());
            }
        }
    }

    #[test]
    fn fresh_wait_not_interrupted_by_preexisting_pending_delivery() {
        for push in [PushMode::Append, PushMode::WhenIdle] {
            let mut session = make_session();
            session.turn_depth = 1;
            session.start_stream().unwrap();
            session.emit_stream_delta(vec![DeltaOp::SetToolCalls {
                tool_calls: vec![json!({"id":"wait","type":"function","function":{"name":"sleep","arguments":"{}"}})],
            }]);
            session.finish_stream_with_next_state(
                Some("tool_calls".into()),
                SessionState::ExecutingTools,
            );
            let mut preexisting = delivery_fixture("preexisting", push);
            preexisting.wake = false;
            assert_eq!(
                session.enqueue_delivery(preexisting).unwrap(),
                DeliveryOutcome::Queued
            );
            session.begin_interruptible_wait();
            assert!(!session.wait_interrupt_flag.load(Ordering::SeqCst));
            assert!(!session.wait_delivery_boundary);
            session
                .enqueue_delivery(delivery_fixture("later", PushMode::Append))
                .unwrap();
            assert!(session.wait_interrupt_flag.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn runner_mirror_update_does_not_interrupt_wait() {
        let mut session = make_session();
        let initial = vec![PendingDelivery::with_id(
            "runner-1",
            vec![ChatMessage::new("user".into(), "queued to runner".into())],
            PushMode::Append,
            "test",
            false,
        )];
        session.set_runner_pending_deliveries(initial.clone());
        session.begin_interruptible_wait();
        assert!(!session.wait_interrupt_flag.load(Ordering::SeqCst));
        session.set_runner_pending_deliveries(initial);
        assert!(!session.wait_interrupt_flag.load(Ordering::SeqCst));
        session.set_runner_pending_deliveries(vec![PendingDelivery::with_id(
            "runner-1",
            vec![ChatMessage::new("user".into(), "progress update".into())],
            PushMode::Append,
            "test",
            false,
        )]);
        assert!(!session.wait_interrupt_flag.load(Ordering::SeqCst));
        session.set_runner_pending_deliveries(Vec::new());
        assert!(!session.wait_interrupt_flag.load(Ordering::SeqCst));
        session.set_runner_pending_deliveries(vec![PendingDelivery::with_id(
            "runner-2",
            vec![ChatMessage::new("user".into(), "fresh agent message".into())],
            PushMode::Append,
            "test",
            true,
        )]);
        assert!(session.wait_interrupt_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn new_wake_delivery_still_interrupts_wait() {
        let mut session = make_session();
        session.turn_depth = 1;
        session.start_stream().unwrap();
        session.emit_stream_delta(vec![DeltaOp::SetToolCalls {
            tool_calls: vec![json!({"id":"wait","type":"function","function":{"name":"sleep","arguments":"{}"}})],
        }]);
        session.finish_stream_with_next_state(
            Some("tool_calls".into()),
            SessionState::ExecutingTools,
        );
        session.begin_interruptible_wait();
        assert!(!session.wait_interrupt_flag.load(Ordering::SeqCst));
        let mut wake = delivery_fixture("wake", PushMode::Append);
        wake.wake = true;
        assert_eq!(
            session.enqueue_delivery(wake).unwrap(),
            DeliveryOutcome::Queued
        );
        assert!(session.wait_interrupt_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn parallel_wait_runtime_remains_true_until_last_wait_finishes() {
        let mut session = make_session();
        session.begin_interruptible_wait();
        session.begin_interruptible_wait();
        session.end_interruptible_wait();
        assert!(session.runtime.waiting_interruptible);
        session.end_interruptible_wait();
        assert!(!session.runtime.waiting_interruptible);
    }

    #[test]
    fn delivery_append_waits_for_stream_and_all_tool_results() {
        let mut session = make_session();
        session.start_stream().unwrap();
        session.emit_stream_delta(vec![DeltaOp::SetToolCalls {
            tool_calls: vec![
                json!({"id":"one","type":"function","function":{"name":"cat","arguments":"{}"}}),
                json!({"id":"two","type":"function","function":{"name":"cat","arguments":"{}"}}),
            ],
        }]);
        let delivery = delivery_fixture("append", PushMode::Append);
        assert_eq!(
            session.enqueue_delivery(delivery).unwrap(),
            DeliveryOutcome::Queued
        );
        assert!(!session.abort_flag.load(Ordering::SeqCst));
        assert!(session.messages.is_empty());
        session
            .finish_stream_with_next_state(Some("tool_calls".into()), SessionState::ExecutingTools);
        assert!(session.drain_pending_deliveries().0.is_empty());
        for id in ["one", "two"] {
            session.add_message(ChatMessage {
                role: "tool".into(),
                tool_call_id: id.into(),
                content: ChatContent::SimpleText("result".into()),
                ..Default::default()
            });
            if id == "one" {
                assert!(session.drain_pending_deliveries().0.is_empty());
            }
        }
        assert_eq!(
            session.drain_pending_deliveries(),
            (vec!["append".to_string()], true)
        );
        assert_eq!(
            session
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["assistant", "tool", "tool", "event"]
        );
    }

    #[test]
    fn delivery_when_idle_waits_for_whole_turn_and_edits_apply_now() {
        let mut session = make_session();
        session.turn_depth = 1;
        session.start_stream().unwrap();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "answer".into(),
        }]);
        session
            .enqueue_delivery(delivery_fixture("idle", PushMode::WhenIdle))
            .unwrap();
        session.finish_stream(Some("stop".into()));
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert!(session.drain_pending_deliveries().0.is_empty());
        session.turn_depth = 0;
        assert_eq!(session.drain_pending_deliveries().0, vec!["idle"]);
        session.start_stream().unwrap();
        session
            .enqueue_delivery(delivery_fixture("cancel", PushMode::Append))
            .unwrap();
        assert!(!session
            .update_pending_delivery("cancel", None, true)
            .unwrap());
        assert!(session.pending_deliveries.is_empty());
        session
            .enqueue_delivery(delivery_fixture("edit", PushMode::WhenIdle))
            .unwrap();
        assert!(session
            .update_pending_delivery("edit", Some(PushMode::Preempt), false)
            .unwrap());
        assert_eq!(session.drain_pending_deliveries().0, vec!["edit"]);
        assert!(session.draft_message.is_none());
        assert!(session.abort_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn delivery_preempt_discards_draft_once_and_preserves_accepted_work() {
        let mut session = make_session();
        session.turn_depth = 1;
        session.start_stream().unwrap();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "discard me".into(),
        }]);
        session
            .enqueue_delivery(delivery_fixture("b", PushMode::Append))
            .unwrap();
        session
            .enqueue_delivery(delivery_fixture("c", PushMode::WhenIdle))
            .unwrap();
        session.queue_post_tool_side_effect(event(
            EventSubkind::CancellationNote,
            "legacy",
            json!({}),
            "keep me",
        ));
        let urgent = delivery_fixture("a", PushMode::Preempt);
        assert_eq!(
            session.enqueue_delivery(urgent.clone()).unwrap(),
            DeliveryOutcome::Delivered
        );
        let count = session.messages.len();
        let seq = session.event_seq;
        assert_eq!(
            session.enqueue_delivery(urgent).unwrap(),
            DeliveryOutcome::Duplicate
        );
        assert_eq!(session.event_seq, seq);
        assert_eq!(session.messages.len(), count);
        assert!(session.draft_message.is_none());
        assert_eq!(session.pending_deliveries.len(), 2);
        assert_eq!(session.post_tool_side_effects.len(), 1);
        assert_eq!(
            session
                .messages
                .iter()
                .filter(|m| m.extra.get("delivery").is_none())
                .count(),
            1
        );
        assert_eq!(session.drain_pending_deliveries().0, vec!["b"]);
        session.turn_depth = 0;
        assert_eq!(session.drain_pending_deliveries().0, vec!["c"]);
    }

    #[test]
    fn delivery_thread_patch_is_validated_and_applied_only_at_boundary() {
        let mut session = make_session();
        session.thread.model = "old".into();
        session.start_stream().unwrap();
        let mut delivery = delivery_fixture("patch", PushMode::Append);
        delivery.thread_patch = Some(json!({"model": "new"}));
        session.enqueue_delivery(delivery).unwrap();
        assert_eq!(session.thread.model, "old");
        session.finish_stream(Some("stop".into()));
        session.drain_pending_deliveries();
        assert_eq!(session.thread.model, "new");
        let mut invalid = delivery_fixture("invalid", PushMode::Preempt);
        invalid.thread_patch = Some(json!({"model": 123}));
        let seq = session.event_seq;
        assert!(session.enqueue_delivery(invalid).is_err());
        assert_eq!(session.event_seq, seq);
    }

    #[test]
    fn delivery_full_and_invalid_rejections_have_no_side_effects() {
        let mut session = make_session();
        session.start_stream().unwrap();
        for n in 0..max_queue_size() {
            session
                .enqueue_delivery(delivery_fixture(&format!("{n}"), PushMode::Append))
                .unwrap();
        }
        let seq = session.event_seq;
        assert!(session
            .enqueue_delivery(delivery_fixture("overflow", PushMode::Append))
            .is_err());
        assert_eq!(session.event_seq, seq);
        assert!(!session.abort_flag.load(Ordering::SeqCst));
        assert_eq!(session.pending_deliveries.len(), max_queue_size());
        let mut empty = delivery_fixture("empty", PushMode::Preempt);
        empty.messages.clear();
        assert!(session.enqueue_delivery(empty).is_err());
        assert_eq!(session.event_seq, seq);
    }

    #[test]
    fn delivery_legacy_event_cannot_retroactively_change_provider_prefix() {
        let mut session = make_session();
        session.add_message(ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hello".into()),
            ..Default::default()
        });
        let prefix = serde_json::to_value(&session.messages).unwrap();
        session.start_stream().unwrap();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "answer".into(),
        }]);
        session.add_message(event(
            EventSubkind::CancellationNote,
            "legacy",
            json!({}),
            "notice",
        ));
        assert_eq!(serde_json::to_value(&session.messages).unwrap(), prefix);
        assert_eq!(session.pending_deliveries_for_snapshot().len(), 1);
        let delivery_id = session.pending_deliveries[0].id.clone();
        session.finish_stream(Some("stop".into()));
        assert_eq!(
            session.drain_pending_deliveries().0,
            vec![delivery_id.clone()]
        );
        assert_eq!(
            delivery_id_of_message(session.messages.last().unwrap()),
            Some(delivery_id.as_str())
        );
        assert_eq!(
            session
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            vec!["user", "assistant", "event"]
        );
    }

    #[test]
    fn perf_diagnostics_queue_wait_records_once_with_controlled_timestamp() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let mut session = make_session();
        let now = Instant::now();
        session
            .command_enqueued_at
            .insert("queued".to_string(), now);

        session.record_command_queue_wait_at(
            "queued",
            now.checked_add(std::time::Duration::from_micros(42))
                .unwrap(),
        );
        session.record_command_queue_wait_at(
            "queued",
            now.checked_add(std::time::Duration::from_micros(84))
                .unwrap(),
        );

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].component, "command.queue_wait");
        assert_eq!(events[0].elapsed_us, 42);
    }

    #[test]
    fn reprioritize_queued_command_is_idempotent_without_side_effects() {
        let mut session = make_session();
        session.command_queue.extend([
            CommandRequest {
                client_request_id: "first".to_string(),
                priority: true,
                command: ChatCommand::Regenerate {},
            },
            CommandRequest {
                client_request_id: "target".to_string(),
                priority: true,
                command: ChatCommand::Regenerate {},
            },
            CommandRequest {
                client_request_id: "last".to_string(),
                priority: false,
                command: ChatCommand::Regenerate {},
            },
        ]);
        let last_activity = session.last_activity;
        let event_seq = session.event_seq;
        let mut events = session.subscribe();

        assert!(session.reprioritize_queued_command("target", true));

        assert_eq!(
            session
                .command_queue
                .iter()
                .map(|request| request.client_request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "target", "last"]
        );
        assert_eq!(session.last_activity, last_activity);
        assert_eq!(session.event_seq, event_seq);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn promoting_user_message_interrupts_active_tools_and_preserves_request() {
        let mut session = make_session();
        session.runtime.state = SessionState::ExecutingTools;
        session.messages.push(ChatMessage {
            message_id: "assistant-with-tool".to_string(),
            role: "assistant".to_string(),
            tool_calls: Some(vec![ChatToolCall {
                id: "pending-tool".to_string(),
                index: Some(0),
                tool_type: "function".to_string(),
                function: ChatToolFunction {
                    name: "shell".to_string(),
                    arguments: "{}".to_string(),
                },
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        });
        let content = json!([
            {"type": "text", "text": "look"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,payload"}}
        ]);
        let attachments = vec![json!({"name": "image.png", "mime": "image/png"})];
        let context_files = vec![json!({"file_name": "context.md", "file_content": "details"})];
        session.command_queue.push_back(CommandRequest {
            client_request_id: "promoted-request".to_string(),
            priority: false,
            command: ChatCommand::UserMessage {
                content: content.clone(),
                attachments: attachments.clone(),
                context_files: context_files.clone(),
                suppress_auto_enrichment: true,
                client_message_id: None,
                push: None,
            },
        });

        assert!(session.reprioritize_queued_command("promoted-request", true));

        assert_eq!(session.runtime.state, SessionState::Idle);
        assert!(session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert!(session.messages[0].tool_calls.is_none());
        let request = session.command_queue.front().unwrap();
        assert_eq!(request.client_request_id, "promoted-request");
        assert!(request.priority);
        match &request.command {
            ChatCommand::UserMessage {
                content: actual_content,
                attachments: actual_attachments,
                context_files: actual_context_files,
                suppress_auto_enrichment,
                ..
            } => {
                assert_eq!(actual_content, &content);
                assert_eq!(actual_attachments, &attachments);
                assert_eq!(actual_context_files, &context_files);
                assert!(*suppress_auto_enrichment);
            }
            command => panic!("expected user message, got {command:?}"),
        }
    }

    #[test]
    fn perf_diagnostics_ordinary_and_priority_queue_waits_are_cleaned_up() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let mut session = make_session();

        session.enqueue_accepted_command(CommandRequest {
            client_request_id: "ordinary".to_string(),
            priority: false,
            command: ChatCommand::Regenerate {},
        });
        session.enqueue_accepted_priority_command(CommandRequest {
            client_request_id: "priority".to_string(),
            priority: true,
            command: ChatCommand::Regenerate {},
        });

        assert_eq!(session.command_enqueued_at.len(), 2);
        let now = Instant::now();
        session.record_command_queue_wait_at("priority", now);
        session.record_command_queue_wait_at("ordinary", now);

        assert!(session.command_enqueued_at.is_empty());
        let events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event.component == "command.queue_wait")
            .collect();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_is_duplicate_request_detects_duplicates() {
        let mut session = make_session();
        assert!(!session.is_duplicate_request("req-1"));
        assert!(session.is_duplicate_request("req-1"));
        assert!(!session.is_duplicate_request("req-2"));
        assert!(session.is_duplicate_request("req-2"));
    }

    #[test]
    fn test_is_duplicate_request_caps_at_100() {
        let mut session = make_session();
        for i in 0..100 {
            session.is_duplicate_request(&format!("req-{}", i));
        }
        assert_eq!(session.recent_request_ids.len(), 100);
        session.is_duplicate_request("req-100");
        assert_eq!(session.recent_request_ids.len(), 100);
        assert!(!session.recent_request_ids.contains(&"req-0".to_string()));
        assert!(session.recent_request_ids.contains(&"req-100".to_string()));
    }

    #[test]
    fn enqueue_priority_command_interrupts_active_generation() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "partial".into(),
        }]);
        session.enqueue_priority_command(CommandRequest {
            client_request_id: "priority-regenerate".into(),
            priority: false,
            command: ChatCommand::Regenerate {},
        });

        assert!(session.draft_message.is_none());
        assert!(session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert!(session
            .user_interrupt_flag
            .load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_eq!(session.command_queue.len(), 1);
        assert!(session.command_queue[0].priority);
        assert!(matches!(
            session.command_queue[0].command,
            ChatCommand::Regenerate {}
        ));
    }

    #[test]
    fn duplicate_priority_command_does_not_interrupt_active_generation() {
        let mut session = make_session();
        session.is_duplicate_request("priority-regenerate");
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "partial".into(),
        }]);

        let outcome = session.enqueue_priority_command(CommandRequest {
            client_request_id: "priority-regenerate".into(),
            priority: false,
            command: ChatCommand::Regenerate {},
        });

        assert_eq!(outcome, EnqueueCommandOutcome::Duplicate);
        assert!(!session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.runtime.state, SessionState::Generating);
        assert!(session.command_queue.is_empty());
    }

    #[test]
    fn full_priority_command_does_not_interrupt_active_generation() {
        let mut session = make_session();
        for i in 0..max_queue_size() {
            session.command_queue.push_back(CommandRequest {
                client_request_id: format!("queued-{i}"),
                priority: true,
                command: ChatCommand::SetParams {
                    patch: json!({"temperature": i}),
                },
            });
        }
        session.emit_queue_update();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "partial".into(),
        }]);

        let outcome = session.enqueue_priority_command(CommandRequest {
            client_request_id: "priority-regenerate-full".into(),
            priority: false,
            command: ChatCommand::Regenerate {},
        });

        assert_eq!(outcome, EnqueueCommandOutcome::Full);
        assert!(!session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.runtime.state, SessionState::Generating);
        assert_eq!(session.command_queue.len(), max_queue_size());
        assert!(!session
            .recent_request_ids_set
            .contains("priority-regenerate-full"));
    }

    #[test]
    fn full_queue_abort_still_interrupts_active_generation() {
        let mut session = make_session();
        for i in 0..max_queue_size() {
            session.command_queue.push_back(CommandRequest {
                client_request_id: format!("queued-{i}"),
                priority: true,
                command: ChatCommand::SetParams {
                    patch: json!({"temperature": i}),
                },
            });
        }
        session.emit_queue_update();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "partial".into(),
        }]);

        let outcome = session.enqueue_priority_command(CommandRequest {
            client_request_id: "priority-abort-full".into(),
            priority: false,
            command: ChatCommand::Abort {},
        });

        assert_eq!(outcome, EnqueueCommandOutcome::Accepted);
        assert!(session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_eq!(session.command_queue.len(), max_queue_size() + 1);
        assert!(session
            .recent_request_ids_set
            .contains("priority-abort-full"));
    }

    #[test]
    fn enqueue_priority_abort_interrupts_active_generation() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "partial".into(),
        }]);

        session.enqueue_priority_command(CommandRequest {
            client_request_id: "priority-abort".into(),
            priority: false,
            command: ChatCommand::Abort {},
        });

        assert!(session.draft_message.is_none());
        assert!(session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert!(matches!(
            session.command_queue[0].command,
            ChatCommand::Abort {}
        ));
    }

    #[test]
    fn enqueue_priority_command_clears_unanswered_tool_calls() {
        let mut session = make_session();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session.messages.push(ChatMessage {
            message_id: "assistant-with-tools".into(),
            role: "assistant".into(),
            tool_calls: Some(vec![ChatToolCall {
                id: "tool-pending".into(),
                index: Some(0),
                function: ChatToolFunction {
                    name: "cat".into(),
                    arguments: "{}".into(),
                },
                tool_type: "function".into(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        });

        session.enqueue_priority_command(CommandRequest {
            client_request_id: "priority-user".into(),
            priority: false,
            command: ChatCommand::UserMessage {
                content: json!("interrupt"),
                attachments: vec![],
                context_files: vec![],
                suppress_auto_enrichment: false,
                client_message_id: None,
                push: None,
            },
        });

        assert!(session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert!(session.messages[0].tool_calls.is_none());
        assert!(session.command_queue[0].priority);
    }

    #[test]
    fn enqueue_priority_command_preserves_answered_tool_calls() {
        let mut session = make_session();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session.messages.push(ChatMessage {
            message_id: "assistant-with-tools".into(),
            role: "assistant".into(),
            tool_calls: Some(vec![
                ChatToolCall {
                    id: "tool-answered".into(),
                    index: Some(0),
                    function: ChatToolFunction {
                        name: "cat".into(),
                        arguments: "{}".into(),
                    },
                    tool_type: "function".into(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                },
                ChatToolCall {
                    id: "tool-pending".into(),
                    index: Some(1),
                    function: ChatToolFunction {
                        name: "tree".into(),
                        arguments: "{}".into(),
                    },
                    tool_type: "function".into(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                },
            ]),
            ..Default::default()
        });
        session.messages.push(crate::chat::internal_roles::event(
            crate::chat::internal_roles::EventSubkind::Tick,
            "tool.sleep",
            json!({"elapsed_ms": 5_000, "remaining_ms": 25_000}),
            "tick".to_string(),
        ));
        session.messages.push(ChatMessage {
            role: "tool".into(),
            tool_call_id: "tool-answered".into(),
            content: ChatContent::SimpleText("done".into()),
            ..Default::default()
        });

        session.enqueue_priority_command(CommandRequest {
            client_request_id: "priority-regenerate".into(),
            priority: false,
            command: ChatCommand::Regenerate {},
        });

        let tool_calls = session.messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "tool-answered");
    }

    #[test]
    fn test_build_queued_items() {
        let mut session = make_session();
        session.command_queue.push_back(CommandRequest {
            client_request_id: "req-1".into(),
            priority: false,
            command: ChatCommand::UserMessage {
                content: json!("hello"),
                attachments: vec![],
                context_files: vec![],
                suppress_auto_enrichment: false,
                client_message_id: None,
                push: None,
            },
        });
        session.command_queue.push_back(CommandRequest {
            client_request_id: "req-2".into(),
            priority: true,
            command: ChatCommand::Abort {},
        });
        let items = session.build_queued_items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].client_request_id, "req-1");
        assert!(!items[0].priority);
        assert_eq!(items[0].command_type, "user_message");
        assert_eq!(items[1].client_request_id, "req-2");
        assert!(items[1].priority);
        assert_eq!(items[1].command_type, "abort");
    }

    #[test]
    fn test_emit_queue_update_syncs_runtime() {
        let mut session = make_session();
        session.command_queue.push_back(CommandRequest {
            client_request_id: "req-1".into(),
            priority: false,
            command: ChatCommand::Abort {},
        });
        session.emit_queue_update();
        assert_eq!(session.runtime.queue_size, 1);
        assert_eq!(session.runtime.queued_items.len(), 1);
    }

    #[test]
    fn test_set_runtime_state_syncs_queued_items() {
        let mut session = make_session();
        session.command_queue.push_back(CommandRequest {
            client_request_id: "req-1".into(),
            priority: true,
            command: ChatCommand::Abort {},
        });
        session.set_runtime_state(SessionState::Generating, None);
        assert_eq!(session.runtime.queued_items.len(), 1);
        assert_eq!(session.runtime.queued_items[0].client_request_id, "req-1");
    }

    #[test]
    fn test_duplicate_request_hashset_stays_in_sync() {
        let mut session = make_session();
        assert!(!session.is_duplicate_request("req-x"));
        assert!(session.recent_request_ids.contains(&"req-x".to_string()));
        assert!(session.recent_request_ids_set.contains("req-x"));
        assert!(session.is_duplicate_request("req-x"));
    }

    #[test]
    fn test_duplicate_request_hashset_eviction_in_sync() {
        let mut session = make_session();
        for i in 0..100 {
            session.is_duplicate_request(&format!("req-{}", i));
        }
        session.is_duplicate_request("req-100");
        assert!(!session.recent_request_ids_set.contains("req-0"));
        assert!(session.recent_request_ids_set.contains("req-100"));
        assert_eq!(
            session.recent_request_ids.len(),
            session.recent_request_ids_set.len()
        );
    }
