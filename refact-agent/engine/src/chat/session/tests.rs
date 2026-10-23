use super::*;
    use super::super::types::{ChatCommand, CommandRequest};
    use crate::chat::perf_diagnostics::{self, MemoryPerfSink, PerfClock, PerfRecorder};
    use crate::chat::perf_telemetry::PerformanceTelemetry;
    use crate::call_validation::{ChatToolCall, ChatToolFunction};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    fn make_session() -> ChatSession {
        ChatSession::new("test-chat".to_string())
    }

    fn turn_memory_catalog() -> Arc<refact_runtime_api::ToolCatalogSnapshot> {
        let tools = (0..8)
            .map(|index| refact_tool_api::ToolDesc {
                name: format!("turn_memory_tool_{index}"),
                experimental: false,
                allow_parallel: true,
                description: "descriptor".repeat(256),
                input_schema: json!({"type": "object", "properties": {}}),
                output_schema: None,
                annotations: None,
                display_name: format!("Turn memory tool {index}"),
                source: refact_tool_api::ToolSource {
                    source_type: refact_tool_api::ToolSourceType::Builtin,
                    config_path: String::new(),
                },
            })
            .collect::<Vec<_>>();
        let names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        Arc::new(refact_runtime_api::ToolCatalogSnapshot {
            index: refact_runtime_api::ToolRegistryIndex {
                tools,
                mcp_lazy_mode: false,
                mcp_total_count: 0,
                mcp_tool_index: Vec::new(),
            },
            policy: Vec::new(),
            aliases: refact_tool_api::build_registry_from_names(&names),
        })
    }

    fn install_turn_memory_state(session: &mut ChatSession) {
        session.last_prompt_messages = vec![ChatMessage::new(
            "user".to_string(),
            "prepared prompt".repeat(16_384),
        )];
        session.tool_catalog = Some(turn_memory_catalog());
        session.turn_tool_pool = Some(refact_runtime_api::TurnToolPool::new(()));
    }

    fn assert_turn_memory_released(session: &ChatSession) {
        assert!(session.last_prompt_messages.is_empty());
        assert!(session.tool_catalog.is_none());
        assert!(session.turn_tool_pool.is_none());
    }

    struct TestClock {
        now: AtomicU64,
    }

    impl TestClock {
        fn new(now: u64) -> Self {
            Self {
                now: AtomicU64::new(now),
            }
        }
    }

    impl PerfClock for TestClock {
        fn now_us(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }
    }

    fn install_perf_recorder() -> (perf_diagnostics::TestRecorderGuard, Arc<MemoryPerfSink>) {
        let sink = Arc::new(MemoryPerfSink::new());
        let recorder = Arc::new(PerfRecorder::with_salt(
            Arc::new(TestClock::new(0)),
            sink.clone(),
            [9; 32],
        ));
        (perf_diagnostics::install_test_recorder(recorder), sink)
    }

    fn delivery_fixture(id: &str, push: PushMode) -> PendingDelivery {
        PendingDelivery::with_id(
            id,
            vec![event(
                EventSubkind::CancellationNote,
                "test",
                json!({}),
                id.to_string(),
            )],
            push,
            "test",
            true,
        )
    }

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
    fn perf_diagnostics_first_delta_records_once_and_skips_empty_deltas() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let mut session = make_session();
        let started_at = Instant::now();
        session.start_stream();
        session.stream_started_at = Some(started_at);

        session.emit_stream_delta_at(Vec::new(), started_at);
        session.emit_stream_delta_at(
            vec![DeltaOp::SetReasoning {
                text: String::new(),
            }],
            started_at,
        );
        assert_eq!(session.stream_started_at, Some(started_at));
        session.emit_stream_delta_at(
            vec![DeltaOp::AppendContent {
                text: "first".to_string(),
            }],
            started_at
                .checked_add(std::time::Duration::from_micros(31))
                .unwrap(),
        );
        session.emit_stream_delta_at(
            vec![DeltaOp::AppendContent {
                text: "second".to_string(),
            }],
            started_at
                .checked_add(std::time::Duration::from_micros(62))
                .unwrap(),
        );

        let events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| event.component == "stream.first_delta")
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].elapsed_us, 31);

        session.finish_stream(None);
        session.start_stream();
        session.finish_stream(None);
        assert_eq!(
            sink.events()
                .iter()
                .filter(|event| event.component == "stream.first_delta")
                .count(),
            1
        );
    }

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
    fn abort_stream_preserves_surviving_queue_timestamps() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, _) = install_perf_recorder();
        let mut session = make_session();
        session.enqueue_accepted_command(CommandRequest {
            client_request_id: "queued".to_string(),
            priority: false,
            command: ChatCommand::Regenerate {},
        });

        session.abort_stream();

        assert!(session.command_enqueued_at.contains_key("queued"));
    }

    #[test]
    fn abort_stream_clears_stream_and_confirmation_timestamps() {
        let mut session = make_session();
        let now = Instant::now();
        session.stream_started_at = Some(now);
        session.confirmation_paused_at = Some((now, HashSet::from(["tc1".to_string()])));
        session.runtime.pause_reasons = vec![make_pause_reason("tc1")];
        session.runtime.state = SessionState::Paused;

        session.abort_stream();

        assert!(session.stream_started_at.is_none());
        assert!(session.confirmation_paused_at.is_none());
        assert!(session.runtime.pause_reasons.is_empty());
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
    fn reset_compaction_preserves_surviving_queue_timestamps() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, _) = install_perf_recorder();
        let mut session = make_session();
        session.enqueue_accepted_command(CommandRequest {
            client_request_id: "queued".to_string(),
            priority: false,
            command: ChatCommand::Regenerate {},
        });

        session.reset_compaction_runtime_state();

        assert!(session.command_enqueued_at.contains_key("queued"));
    }

    #[test]
    fn close_event_channel_clears_runtime_timestamps() {
        let mut session = make_session();
        let now = Instant::now();
        session
            .command_enqueued_at
            .insert("queued".to_string(), now);
        session.stream_started_at = Some(now);
        session.confirmation_paused_at = Some((now, HashSet::from(["tc1".to_string()])));
        session.close_event_channel();

        assert!(session.command_enqueued_at.is_empty());
        assert!(session.stream_started_at.is_none());
        assert!(session.confirmation_paused_at.is_none());
    }

    #[test]
    fn no_delta_abort_clears_stream_timestamp() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, _) = install_perf_recorder();
        let mut session = make_session();
        session.start_stream();
        assert!(session.stream_started_at.is_some());

        session.abort_stream();

        assert!(session.stream_started_at.is_none());
    }

    #[test]
    fn terminal_stream_paths_clear_stream_timestamp() {
        let mut finished = make_session();
        finished.start_stream();
        finished.finish_stream(None);
        assert!(finished.stream_started_at.is_none());

        let mut errored = make_session();
        errored.start_stream();
        errored.finish_stream_with_error("failed".to_string());
        assert!(errored.stream_started_at.is_none());

        let mut reset = make_session();
        reset.start_stream();
        reset.reset_compaction_runtime_state();
        assert!(reset.stream_started_at.is_none());
    }

    fn now_millis() -> u64 {
        chrono::Utc::now().timestamp_millis().max(0) as u64
    }

    fn reserve_compression(session: &mut ChatSession) {
        session.active_compression_attempt = Some(7);
        session.compression_attempt_started_at_ms = Some(now_millis());
        session.compression_abort_flag = Some(Arc::new(AtomicBool::new(false)));
    }

    /// A refused `start_stream` must leave a message the user can actually see.
    ///
    /// Asserts on `error_info`, not only on `content`: `ErrorMessage.tsx` renders
    /// `error_info.title`/`explanation` for a structured error and shows only a
    /// generic title otherwise, so a card without `error_info` would drop the
    /// cause. That is the swallow this test exists to prevent.
    #[test]
    fn refused_start_stream_surfaces_a_visible_error_naming_the_cause() {
        let mut session = make_session();
        reserve_compression(&mut session);

        assert!(session.start_stream().is_none());
        let refusal = session
            .stream_start_refusal()
            .expect("a live compression reservation must refuse");
        session.append_stream_start_refusal(&refusal);

        let card = session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "error")
            .expect("a refused start must append a visible error card");
        assert!(
            crate::chat::diagnostics::is_ui_only_message(card),
            "the notice must be a ui-only diagnostic, not a hidden `event` role"
        );
        let info = card
            .extra
            .get("error_info")
            .and_then(|value| value.as_object())
            .expect("the notice must carry error_info or the GUI hides the cause");
        assert_eq!(
            info.get("title").and_then(|value| value.as_str()),
            Some("Waiting for context compaction"),
            "a compression refusal must read as a compaction wait"
        );
        let explanation = info
            .get("explanation")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(
            explanation.contains("compaction"),
            "the card must name the blocker: {explanation}"
        );
        assert_eq!(
            info.get("category").and_then(|value| value.as_str()),
            Some("ProviderTransient"),
            "compression is transient; the card must not claim it is terminal"
        );
        assert_eq!(
            info.get("is_retryable").and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    /// Each blocker gets its own cause, and compression outranks the rest because
    /// it is the wait the user can actually see on screen.
    #[test]
    fn stream_start_refusal_distinguishes_every_blocker() {
        let mut session = make_session();
        assert_eq!(session.stream_start_refusal(), None);

        session.pending_mode_handoff = Some(Default::default());
        assert_eq!(
            session.stream_start_refusal(),
            Some(StreamStartRefusal::PendingModeHandoff)
        );
        session.pending_mode_handoff = None;

        session.pending_context_rebuild = Some(
            crate::chat::context_rebuild::PendingContextRebuild {
                model: None,
                trigger: "u4-test".to_string(),
            },
        );
        assert_eq!(
            session.stream_start_refusal(),
            Some(StreamStartRefusal::PendingContextRebuild)
        );
        session.pending_context_rebuild = None;

        session.set_runtime_state(SessionState::ExecutingTools, None);
        assert_eq!(
            session.stream_start_refusal(),
            Some(StreamStartRefusal::ExecutingTools)
        );
        session.runtime.state = SessionState::Idle;

        session.start_stream().expect("an idle session starts");
        assert_eq!(
            session.stream_start_refusal(),
            Some(StreamStartRefusal::DraftOpen)
        );

        reserve_compression(&mut session);
        assert_eq!(
            session.stream_start_refusal(),
            Some(StreamStartRefusal::Compression),
            "compression must outrank the open draft, since it is the visible wait"
        );
    }

    /// Repeated refusals for the same cause must collapse into one card, so a
    /// chat that keeps retrying does not grow a card per retry.
    #[test]
    fn repeated_stream_start_refusals_do_not_spam_messages() {
        let mut session = make_session();
        let refusal = StreamStartRefusal::PendingModeHandoff;
        for _ in 0..5 {
            session.append_stream_start_refusal(&refusal);
        }

        let errors: Vec<_> = session
            .messages
            .iter()
            .filter(|message| message.role == "error")
            .collect();
        assert_eq!(
            errors.len(),
            1,
            "five identical refusals must not append five cards"
        );
        assert_eq!(
            errors[0]
                .extra
                .get("repeat_count")
                .and_then(|value| value.as_u64()),
            Some(5),
            "the collapse must remain visible as a repeat count"
        );
    }

    /// Distinct causes must NOT collapse together, or a second blocker would be
    /// hidden behind the first.
    #[test]
    fn distinct_refusal_causes_each_get_their_own_card() {
        let mut session = make_session();
        session.append_stream_start_refusal(&StreamStartRefusal::PendingModeHandoff);
        session.append_stream_start_refusal(&StreamStartRefusal::ExecutingTools);

        assert_eq!(
            session
                .messages
                .iter()
                .filter(|message| message.role == "error")
                .count(),
            2
        );
    }

    /// Every cause must have its own user-facing phrasing.
    ///
    /// `as_str()` feeds the `refusal.cause` field and the `raw_error` of the card,
    /// and two causes reading identically would (a) make two distinct blockers
    /// present as one to the user and (b) collapse into a single dedup entry, so
    /// the second blocker would never get its own card.
    #[test]
    fn stream_start_refusal_as_str_is_non_empty_and_pairwise_distinct() {
        let causes = [
            StreamStartRefusal::Compression,
            StreamStartRefusal::PendingContextRebuild,
            StreamStartRefusal::PendingModeHandoff,
            StreamStartRefusal::InvalidActiveContext("detail".to_string()),
            StreamStartRefusal::ExecutingTools,
            StreamStartRefusal::DraftOpen,
        ];

        for (index, left) in causes.iter().enumerate() {
            assert!(!left.as_str().is_empty(), "{left:?} must render a phrase");
            for right in causes.iter().skip(index + 1) {
                assert_ne!(
                    left.as_str(),
                    right.as_str(),
                    "{left:?} and {right:?} read identically, so one blocker would hide the other"
                );
            }
        }
    }

    /// An unprojectable history is the one refusal cause whose detail text has no
    /// provider fingerprint, so its card MUST be built with an authored
    /// `error_info`.
    ///
    /// This arm used to early-return `make_ui_only_error_message(...)`, which
    /// re-runs `classify_user_error` over the prose. Four of the five real
    /// `ActiveContextError` detail strings classify as `UserErrorCategory::Unknown`,
    /// so the GUI rendered the generic "Unknown error" card — precisely the swallow
    /// this enum exists to prevent. The detail is asserted to survive, and the title
    /// to be authored, so a regression to the classifier is caught here rather than
    /// by a user staring at "Unknown error".
    #[test]
    fn invalid_active_context_refusal_keeps_an_authored_visible_card() {
        let mut session = make_session();
        session.add_message(ChatMessage {
            summarized_range: Some((0, 2)),
            ..ChatMessage::new("user".to_string(), "legacy summary".to_string())
        });

        let refusal = session
            .stream_start_refusal()
            .expect("a legacy summary must block generation");
        assert!(
            matches!(&refusal, StreamStartRefusal::InvalidActiveContext(detail) if detail.contains("explicit rebuild")),
            "expected an InvalidActiveContext refusal naming the rebuild, got {refusal:?}"
        );

        session.append_stream_start_refusal(&refusal);

        let card = session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "error")
            .expect("a refused start must append a visible error card");
        assert!(
            crate::chat::diagnostics::is_ui_only_message(card),
            "the notice must be a ui-only diagnostic"
        );

        let info = card
            .extra
            .get("error_info")
            .and_then(|value| value.as_object())
            .expect("the notice must carry error_info or the GUI falls back to plain text");

        let title = info
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            title.contains("rebuild"),
            "the card must name the action the user needs, not \"Unknown error\": {title}"
        );
        assert_ne!(
            title, "Unknown error",
            "this arm must not fall back to the classifier's Unknown card"
        );

        let explanation = info
            .get("explanation")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            explanation.contains("could not be projected"),
            "the card must carry an authored explanation, not the classifier's generic one: \
             {explanation}"
        );
        assert!(
            explanation.contains("Legacy compressed history needs an explicit rebuild"),
            "the authored explanation must keep the underlying reason: {explanation}"
        );
        assert_eq!(
            info.get("is_retryable").and_then(|v| v.as_bool()),
            Some(false),
            "a chat that needs an explicit rebuild will not fix itself on retry"
        );
    }

    /// The same detail string must always produce the same card text, because
    /// `append_error_message_deduped_content` collapses on content.
    ///
    /// An error detail is stable here (it is the `Display` of the projection
    /// failure), but this pins the property the dedup relies on: the reason is
    /// carried in `content`, not in a varying field.
    #[test]
    fn repeated_invalid_active_context_refusals_collapse_into_one_card() {
        let mut session = make_session();
        session.add_message(ChatMessage {
            summarized_range: Some((0, 2)),
            ..ChatMessage::new("user".to_string(), "legacy summary".to_string())
        });
        let refusal = session
            .stream_start_refusal()
            .expect("a legacy summary must block generation");

        for _ in 0..3 {
            session.append_stream_start_refusal(&refusal);
        }

        let errors: Vec<_> = session
            .messages
            .iter()
            .filter(|message| message.role == "error")
            .collect();
        assert_eq!(
            errors.len(),
            1,
            "three identical refusals must not append three cards"
        );
        assert_eq!(
            errors[0]
                .extra
                .get("repeat_count")
                .and_then(|value| value.as_u64()),
            Some(3)
        );
    }

    #[test]
    fn perf_diagnostics_sse_serialize_and_broadcast_are_distinct() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let session = make_session();

        session.record_sse_timing_for_test(7, 11, 13);

        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].component, "sse.serialize");
        assert_eq!(events[0].elapsed_us, 7);
        assert_eq!(events[0].size_bytes, Some(13));
        assert_eq!(events[1].component, "sse.broadcast");
        assert_eq!(events[1].elapsed_us, 11);
        assert_eq!(events[1].size_bytes, None);
    }

    #[test]
    fn perf_diagnostics_sse_broadcast_without_receivers_is_skipped() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let mut session = make_session();

        session.emit(ChatEvent::PauseCleared {});

        let events = sink.events();
        let broadcast = events
            .iter()
            .find(|event| event.component == PerfComponent::SseBroadcast.as_str())
            .unwrap();
        assert_eq!(broadcast.outcome, PerfOutcome::Skipped.as_str());

        let telemetry = PerformanceTelemetry::new(true);
        for event in &events {
            assert!(telemetry.record(event));
        }
        let advancement = telemetry.snapshot().rollups.advancement;
        assert_eq!(advancement.failure_count, 0);
        assert_eq!(advancement.skipped_count, 1);
    }

    #[test]
    fn perf_diagnostics_sse_broadcast_with_receiver_is_successful() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let mut session = make_session();
        let mut receiver = session.subscribe();

        session.emit(ChatEvent::PauseCleared {});

        assert!(receiver.try_recv().is_ok());
        let broadcast = sink
            .events()
            .into_iter()
            .find(|event| event.component == PerfComponent::SseBroadcast.as_str())
            .unwrap();
        assert_eq!(broadcast.outcome, PerfOutcome::Success.as_str());
    }

    #[test]
    fn perf_diagnostics_sse_broadcast_after_receiver_disconnect_is_failure() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let session = make_session();
        let (sender, _) = broadcast::channel(1);
        let receiver = sender.subscribe();
        let receiver_count = sender.receiver_count();
        drop(receiver);
        let result = sender.send(Arc::new("event".to_string()));

        assert!(result.is_err());
        session.record_sse_timing(0, 0, 0, sse_broadcast_outcome(receiver_count, &result));

        let broadcast = sink
            .events()
            .into_iter()
            .find(|event| event.component == PerfComponent::SseBroadcast.as_str())
            .unwrap();
        assert_eq!(broadcast.outcome, PerfOutcome::Failure.as_str());
    }

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

    /// Creates a session with a small broadcast channel capacity, useful for
    /// triggering `RecvError::Lagged` quickly in tests without emitting
    /// thousands of events.
    fn make_session_with_capacity(capacity: usize) -> ChatSession {
        let (event_tx, _) = broadcast::channel::<Arc<String>>(capacity);
        let mut session = ChatSession::new("test-chat-small".to_string());
        session.event_tx = event_tx;
        session
    }

    #[test]
    fn test_new_session_initial_state() {
        let session = make_session();
        assert_eq!(session.chat_id, "test-chat");
        assert_eq!(session.thread.id, "test-chat");
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert!(session.messages.is_empty());
        assert!(session.draft_message.is_none());
        assert_eq!(session.event_seq, 0);
        assert!(!session.trajectory_dirty);
    }

    #[test]
    fn test_new_with_trajectory() {
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hello".into()),
            ..Default::default()
        };
        let thread = ThreadParams {
            id: "traj-1".into(),
            title: "Old Chat".into(),
            ..Default::default()
        };
        let session = ChatSession::new_with_trajectory(
            "traj-1".into(),
            vec![msg.clone()],
            thread,
            "2024-01-01T00:00:00Z".into(),
            None,
            Vec::new(),
            None,
        );
        assert_eq!(session.chat_id, "traj-1");
        assert_eq!(session.thread.title, "Old Chat");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.created_at, "2024-01-01T00:00:00Z");
    }

    #[test]
    fn turn_memory_no_tool_completion_releases_without_mutating_history() {
        let mut session = make_session();
        session.add_message(ChatMessage::new(
            "user".to_string(),
            "canonical".to_string(),
        ));
        install_turn_memory_state(&mut session);

        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "completed".to_string(),
        }]);
        session.finish_stream(None);

        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_turn_memory_released(&session);
        assert_eq!(
            session
                .messages
                .iter()
                .map(|message| message.content.content_text_only())
                .collect::<Vec<_>>(),
            vec!["canonical", "completed"]
        );
    }

    #[test]
    fn turn_memory_active_tool_confirmation_and_ide_waits_preserve_state() {
        let mut session = make_session();
        install_turn_memory_state(&mut session);

        session.set_runtime_state(SessionState::ExecutingTools, None);
        assert!(session.tool_catalog.is_some());
        assert!(session.turn_tool_pool.is_some());
        assert!(!session.last_prompt_messages.is_empty());

        session.set_paused_with_reasons_and_auto_approved(
            vec![make_pause_reason("tool-1")],
            Vec::new(),
            None,
        );
        assert!(session.tool_catalog.is_some());
        assert!(session.turn_tool_pool.is_some());
        assert!(!session.last_prompt_messages.is_empty());

        session.set_runtime_state(SessionState::WaitingIde, None);
        assert!(session.tool_catalog.is_some());
        assert!(session.turn_tool_pool.is_some());
        assert!(!session.last_prompt_messages.is_empty());
    }

    #[test]
    fn turn_memory_terminal_abort_error_restore_and_replacement_release_state() {
        let mut aborted = make_session();
        install_turn_memory_state(&mut aborted);
        aborted.abort_stream();
        assert_turn_memory_released(&aborted);

        let mut errored = make_session();
        install_turn_memory_state(&mut errored);
        errored.finish_stream_with_error("failed".to_string());
        assert_turn_memory_released(&errored);

        let mut replaced = make_session();
        install_turn_memory_state(&mut replaced);
        replaced.replace_messages(vec![ChatMessage::new(
            "user".to_string(),
            "restored".to_string(),
        )]);
        assert_turn_memory_released(&replaced);
        assert_eq!(replaced.messages[0].content.content_text_only(), "restored");
    }

    #[test]
    fn turn_memory_retry_keeps_prepared_context_until_a_terminal_boundary() {
        let mut session = make_session();
        install_turn_memory_state(&mut session);
        session.start_stream();
        session.runtime.error = Some("stale error".to_string());
        session.task_agent_error = Some("stale agent error".to_string());

        session.clear_stream_for_retry();

        assert_eq!(session.runtime.state, SessionState::Idle);
        assert_eq!(session.runtime.error, None);
        assert_eq!(session.task_agent_error, None);
        assert!(session.tool_catalog.is_some());
        assert!(session.turn_tool_pool.is_some());
        assert!(!session.last_prompt_messages.is_empty());

        session.start_stream();
        session.finish_stream(None);
        assert_turn_memory_released(&session);
    }

    #[test]
    fn turn_memory_next_turn_reacquires_equivalent_context_and_catalog() {
        let mut session = make_session();
        session.add_message(ChatMessage::new(
            "user".to_string(),
            "canonical".to_string(),
        ));
        install_turn_memory_state(&mut session);
        let initial_prompt = session.last_prompt_messages.clone();
        let initial_catalog = session.tool_catalog.clone().unwrap();

        session.start_stream();
        session.finish_stream(None);
        assert_turn_memory_released(&session);

        session.last_prompt_messages = initial_prompt;
        session.tool_catalog = Some(initial_catalog.clone());
        session.turn_tool_pool = Some(refact_runtime_api::TurnToolPool::new(()));

        assert_eq!(
            session.last_prompt_messages[0].content.content_text_only(),
            "prepared prompt".repeat(16_384)
        );
        assert_eq!(
            session.tool_catalog.unwrap().index.tools.len(),
            initial_catalog.index.tools.len()
        );
        assert!(session.turn_tool_pool.is_some());
    }

    #[test]
    fn turn_memory_fleet_releases_at_least_ninety_percent_after_completion() {
        for chat_count in crate::chat::perf_harness::TURN_MEMORY_FLEET_CHAT_COUNTS {
            let sessions = (0..chat_count)
                .map(|index| {
                    let mut session = ChatSession::new(format!("turn-memory-{chat_count}-{index}"));
                    session.add_message(ChatMessage::new(
                        "user".to_string(),
                        "canonical history".repeat(256),
                    ));
                    install_turn_memory_state(&mut session);
                    session
                })
                .collect::<Vec<_>>();
            let before = crate::chat::perf_harness::aggregate_turn_memory_retained_bytes(
                sessions
                    .iter()
                    .map(|session| session.retained_bytes_for_turn_memory()),
            );
            let canonical_before = before.canonical_messages;
            let mut completed = sessions;
            for session in &mut completed {
                session.start_stream();
                session.finish_stream(None);
            }
            let after = crate::chat::perf_harness::aggregate_turn_memory_retained_bytes(
                completed
                    .iter()
                    .map(|session| session.retained_bytes_for_turn_memory()),
            );

            assert_eq!(
                after.canonical_messages, canonical_before,
                "{chat_count} chats"
            );
            assert_eq!(after.last_prompt_messages, 0, "{chat_count} chats");
            assert_eq!(
                after.catalog_descriptors_and_aliases, 0,
                "{chat_count} chats"
            );
            assert_eq!(after.pool_vectors, 0, "{chat_count} chats");
            assert!(after.total() * 10 <= before.total(), "{chat_count} chats");
        }
    }

    #[tokio::test]
    async fn turn_memory_release_prunes_finished_handles_without_aborting_live_tasks() {
        let mut session = make_session();
        let finished = tokio::spawn(async {});
        tokio::task::yield_now().await;
        assert!(finished.is_finished());
        let live = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });
        session.post_turn_task_handles.push(finished);
        session.post_turn_task_handles.push(live);

        session.release_turn_only_state();

        assert_eq!(session.post_turn_task_handles.len(), 1);
        assert!(!session.post_turn_task_handles[0].is_finished());
        session.post_turn_task_handles.pop().unwrap().abort();
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

    #[test]
    fn test_emit_increments_seq() {
        let mut session = make_session();
        let _rx = session.subscribe();
        assert_eq!(session.event_seq, 0);
        session.emit(ChatEvent::PauseCleared {});
        assert_eq!(session.event_seq, 1);
        session.emit(ChatEvent::PauseCleared {});
        assert_eq!(session.event_seq, 2);
    }

    #[test]
    fn test_emit_skips_serialization_without_receivers() {
        let mut session = make_session();
        assert_eq!(session.event_tx.receiver_count(), 0);

        session.emit(ChatEvent::PauseCleared {});
        session.emit(ChatEvent::PauseCleared {});

        assert_eq!(session.event_seq, 0);
    }

    #[test]
    fn test_emit_seq_stays_contiguous_across_subscribe_gap() {
        let mut session = make_session();

        session.emit(ChatEvent::PauseCleared {});
        assert_eq!(session.event_seq, 0);

        let mut rx = session.subscribe();
        let baseline = session.event_seq;

        session.emit(ChatEvent::PauseCleared {});
        let envelope: EventEnvelope =
            serde_json::from_str(rx.try_recv().unwrap().as_str()).unwrap();
        assert_eq!(envelope.seq, baseline + 1);

        drop(rx);
        session.emit(ChatEvent::PauseCleared {});
        assert_eq!(session.event_seq, baseline + 1);
    }

    #[test]
    fn test_emit_sends_correct_envelope() {
        let mut session = make_session();
        let mut rx = session.subscribe();
        session.emit(ChatEvent::PauseCleared {});
        let json = rx.try_recv().unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(envelope.chat_id, "test-chat");
        assert_eq!(envelope.seq, 1);
        assert!(matches!(envelope.event, ChatEvent::PauseCleared {}));
    }

    #[test]
    fn test_snapshot_without_draft() {
        let mut session = make_session();
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        });
        let snap = session.snapshot();
        match snap {
            ChatEvent::Snapshot { messages, .. } => {
                assert_eq!(messages.len(), 1);
            }
            _ => panic!("Expected Snapshot"),
        }
    }

    #[test]
    fn test_snapshot_includes_draft_when_generating() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "partial".into(),
        }]);
        let snap = session.snapshot();
        match snap {
            ChatEvent::Snapshot {
                messages, runtime, ..
            } => {
                assert_eq!(runtime.state, SessionState::Generating);
                assert_eq!(messages.len(), 1);
                match &messages[0].content {
                    ChatContent::SimpleText(s) => assert_eq!(s, "partial"),
                    _ => panic!("Expected SimpleText"),
                }
            }
            _ => panic!("Expected Snapshot"),
        }
    }

    #[test]
    fn test_snapshot_omits_empty_draft_when_generating() {
        let mut session = make_session();
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        });
        session.start_stream();

        let snap = session.snapshot();

        match snap {
            ChatEvent::Snapshot {
                messages, runtime, ..
            } => {
                assert_eq!(runtime.state, SessionState::Generating);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].role, "user");
            }
            _ => panic!("Expected Snapshot"),
        }
    }

    #[test]
    fn test_snapshot_omits_metadata_only_draft_when_generating() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![
            DeltaOp::SetUsage {
                usage: json!({
                    "prompt_tokens": 10,
                    "completion_tokens": 0,
                    "total_tokens": 10,
                }),
            },
            DeltaOp::MergeExtra {
                extra: serde_json::Map::from_iter([(
                    "openai_response_id".to_string(),
                    json!("resp_123"),
                )]),
            },
        ]);

        let snap = session.snapshot();

        match snap {
            ChatEvent::Snapshot { messages, .. } => {
                assert!(messages.is_empty());
            }
            _ => panic!("Expected Snapshot"),
        }
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
    fn test_add_message_generates_id_if_empty() {
        let mut session = make_session();
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        session.add_message(msg);
        assert!(!session.messages[0].message_id.is_empty());
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn test_add_message_preserves_existing_id() {
        let mut session = make_session();
        let msg = ChatMessage {
            message_id: "custom-id".into(),
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        session.add_message(msg);
        assert_eq!(session.messages[0].message_id, "custom-id");
    }

    #[test]
    fn test_add_message_stamps_created_at_ms() {
        let mut session = make_session();
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        session.add_message(msg);
        let created_at_ms = session
            .messages[0]
            .extra
            .get("created_at_ms")
            .and_then(|v| v.as_u64());
        assert!(created_at_ms.is_some_and(|ms| ms > 0));
    }

    #[test]
    fn test_add_message_preserves_existing_created_at_ms() {
        let mut session = make_session();
        let mut msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        msg.extra.insert("created_at_ms".into(), json!(42_000));
        session.add_message(msg);
        assert_eq!(
            session
                .messages[0]
                .extra
                .get("created_at_ms")
                .and_then(|v| v.as_u64()),
            Some(42_000)
        );
    }

    #[test]
    fn test_insert_message_stamps_created_at_ms() {
        let mut session = make_session();
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        session.insert_message(0, msg);
        assert!(session
            .messages[0]
            .extra
            .get("created_at_ms")
            .and_then(|v| v.as_u64())
            .is_some_and(|ms| ms > 0));
    }

    #[test]
    fn test_update_message_returns_index() {
        let mut session = make_session();
        let msg = ChatMessage {
            message_id: "m1".into(),
            role: "user".into(),
            content: ChatContent::SimpleText("original".into()),
            ..Default::default()
        };
        session.messages.push(msg);
        let updated = ChatMessage {
            message_id: "m1".into(),
            role: "user".into(),
            content: ChatContent::SimpleText("updated".into()),
            ..Default::default()
        };
        let idx = session.update_message("m1", updated);
        assert_eq!(idx, Some(0));
        match &session.messages[0].content {
            ChatContent::SimpleText(s) => assert_eq!(s, "updated"),
            _ => panic!("Expected SimpleText"),
        }
    }

    #[test]
    fn test_update_message_unknown_id_returns_none() {
        let mut session = make_session();
        let msg = ChatMessage::default();
        assert!(session.update_message("unknown", msg).is_none());
    }

    #[test]
    fn test_remove_message_returns_index() {
        let mut session = make_session();
        session.messages.push(ChatMessage {
            message_id: "m1".into(),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            message_id: "m2".into(),
            ..Default::default()
        });
        let idx = session.remove_message("m1");
        assert_eq!(idx, Some(0));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].message_id, "m2");
    }

    #[test]
    fn test_remove_message_unknown_id_returns_none() {
        let mut session = make_session();
        assert!(session.remove_message("unknown").is_none());
    }

    #[test]
    fn test_truncate_messages() {
        let mut session = make_session();
        for i in 0..5 {
            session.messages.push(ChatMessage {
                message_id: format!("m{}", i),
                ..Default::default()
            });
        }
        session.truncate_messages(2);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[1].message_id, "m1");
    }

    #[test]
    fn test_truncate_beyond_length_is_noop() {
        let mut session = make_session();
        session.messages.push(ChatMessage::default());
        let version_before = session.trajectory_version;
        session.truncate_messages(10);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.trajectory_version, version_before);
    }

    #[test]
    fn test_start_stream_returns_message_id() {
        let mut session = make_session();
        let result = session.start_stream();
        assert!(result.is_some());
        let (msg_id, abort_flag) = result.unwrap();
        assert!(!msg_id.is_empty());
        assert!(!abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.runtime.state, SessionState::Generating);
        assert!(session.draft_message.is_some());
    }

    #[test]
    fn test_start_stream_fails_if_already_generating() {
        let mut session = make_session();
        session.start_stream();
        let result = session.start_stream();
        assert!(result.is_none());
    }

    #[test]
    fn test_start_stream_fails_if_executing_tools() {
        let mut session = make_session();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        let result = session.start_stream();
        assert!(result.is_none());
    }

    #[test]
    fn test_emit_stream_delta_appends_content() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "Hello".into(),
        }]);
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: " World".into(),
        }]);
        let draft = session.draft_message.as_ref().unwrap();
        match &draft.content {
            ChatContent::SimpleText(s) => assert_eq!(s, "Hello World"),
            _ => panic!("Expected SimpleText"),
        }
    }

    #[test]
    fn test_emit_stream_delta_appends_reasoning() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendReasoning {
            text: "think".into(),
        }]);
        session.emit_stream_delta(vec![DeltaOp::AppendReasoning { text: "ing".into() }]);
        let draft = session.draft_message.as_ref().unwrap();
        assert_eq!(draft.reasoning_content.as_ref().unwrap(), "thinking");
    }

    #[test]
    fn test_emit_stream_delta_replaces_reasoning() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendReasoning {
            text: "partial header".into(),
        }]);
        session.emit_stream_delta(vec![DeltaOp::SetReasoning {
            text: "completed reasoning body".into(),
        }]);

        let draft = session.draft_message.as_ref().unwrap();
        assert_eq!(
            draft.reasoning_content.as_deref(),
            Some("completed reasoning body")
        );
    }

    #[test]
    fn test_emit_stream_delta_sets_tool_calls() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::SetToolCalls {
            tool_calls: vec![
                json!({"id":"tc1","type":"function","function":{"name":"test","arguments":"{}"}}),
            ],
        }]);
        let draft = session.draft_message.as_ref().unwrap();
        assert!(draft.tool_calls.is_some());
        assert_eq!(draft.tool_calls.as_ref().unwrap().len(), 1);
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
    fn direct_abort_clears_running_sleep_tool_card() {
        let mut session = make_session();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        session.messages.push(ChatMessage {
            message_id: "assistant-with-sleep".to_string(),
            role: "assistant".to_string(),
            tool_calls: Some(vec![ChatToolCall {
                id: "sleep-call".to_string(),
                index: Some(0),
                tool_type: "function".to_string(),
                function: ChatToolFunction {
                    name: "sleep".to_string(),
                    arguments: r#"{"duration_ms":30000,"description":"Wait briefly"}"#.to_string(),
                },
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        });
        let mut events = session.subscribe();

        session.abort_stream();
        session.clear_pending_tool_calls_for_interruption();

        assert_eq!(session.runtime.state, SessionState::Idle);
        assert!(session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert!(session.messages[0].tool_calls.is_none());
        assert!(session.trajectory_dirty);
        let mut saw_sleep_removed = false;
        while let Ok(json) = events.try_recv() {
            let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
            if let ChatEvent::MessageUpdated { message, .. } = envelope.event {
                saw_sleep_removed =
                    message.message_id == "assistant-with-sleep" && message.tool_calls.is_none();
            }
        }
        assert!(saw_sleep_removed);
    }

    #[test]
    fn test_emit_stream_delta_without_draft_is_noop() {
        let mut session = make_session();
        session.emit_stream_delta(vec![DeltaOp::AppendContent { text: "x".into() }]);
        assert!(session.draft_message.is_none());
    }

    #[test]
    fn test_finish_stream_adds_message() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "done".into(),
        }]);
        session.finish_stream(Some("stop".into()));
        assert!(session.draft_message.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].finish_reason, Some("stop".into()));
        assert_eq!(session.runtime.state, SessionState::Idle);
    }

    #[test]
    fn test_finish_stream_can_continue_to_tools_without_idle() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::SetToolCalls {
            tool_calls: vec![json!({
                "id": "call_123",
                "type": "function",
                "function": {"name": "cat", "arguments": "{}"}
            })],
        }]);

        session
            .finish_stream_with_next_state(Some("tool_calls".into()), SessionState::ExecutingTools);

        assert!(session.draft_message.is_none());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.runtime.state, SessionState::ExecutingTools);
        assert!(session.messages[0]
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty()));
    }

    #[test]
    fn test_finish_stream_with_error_keeps_content() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "partial".into(),
        }]);
        session.finish_stream_with_error("timeout".into());
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].finish_reason, Some("error".into()));
        assert_eq!(session.messages[1].role, "error");
        assert!(crate::chat::diagnostics::is_ui_only_message(
            &session.messages[1]
        ));
        assert_eq!(session.runtime.state, SessionState::Error);
        assert_eq!(session.runtime.error, Some("timeout".into()));
        assert_eq!(
            session.messages[1]
                .extra
                .get("error_info")
                .and_then(|info| info.get("category"))
                .and_then(|category| category.as_str()),
            Some("ProviderTransient")
        );
    }

    #[test]
    fn test_finish_stream_with_error_keeps_structured_data() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::SetToolCalls {
            tool_calls: vec![
                json!({"id":"tc1","type":"function","function":{"name":"test","arguments":"{}"}}),
            ],
        }]);
        session.finish_stream_with_error("error".into());
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, "assistant");
        assert_eq!(session.messages[1].role, "error");
    }

    #[test]
    fn test_finish_stream_with_error_removes_empty_draft() {
        let mut session = make_session();
        let mut rx = session.subscribe();
        session.start_stream();
        session.finish_stream_with_error("error".into());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].role, "error");
        assert_eq!(session.messages[0].content.content_text_only(), "error");
        let mut found_removed = false;
        while let Ok(json) = rx.try_recv() {
            if let Ok(env) = serde_json::from_str::<EventEnvelope>(&json) {
                if matches!(env.event, ChatEvent::MessageRemoved { .. }) {
                    found_removed = true;
                }
            }
        }
        assert!(found_removed);
    }

    #[test]
    fn test_finish_stream_with_error_trims_empty_draft() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "   \n".into(),
        }]);
        session.finish_stream_with_error("network failed".into());
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].role, "error");
        assert_eq!(
            session.messages[0].content.content_text_only(),
            "network failed"
        );
    }

    #[test]
    fn test_abort_stream() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "partial".into(),
        }]);
        session.abort_stream();
        assert!(session.draft_message.is_none());
        assert!(session.messages.is_empty());
        assert!(session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.runtime.state, SessionState::Idle);
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
    fn snapshot_includes_cached_background_agents() {
        let mut session = make_session();
        let agent = BackgroundAgentSummary {
            agent_id: "bgagent-cached".into(),
            parent_chat_id: session.chat_id.clone(),
            child_chat_id: Some("child-chat".into()),
            kind: "subagent".into(),
            status: "completed".into(),
            title: "Cached agent".into(),
            progress: None,
            step_count: 2,
            last_activity: None,
            target_files: vec![],
            edited_files: vec![],
            diff_summary: None,
            conflict_summary: None,
            result_summary: Some("done".into()),
            error: None,
            started_at: None,
            finished_at: Some("2026-05-28T00:00:00Z".into()),
            change_seq: 7,
            ..BackgroundAgentSummary::default()
        };
        session
            .background_agents
            .insert(agent.agent_id.clone(), agent.clone());

        let snapshot = session.snapshot();

        match snapshot {
            ChatEvent::Snapshot {
                background_agents, ..
            } => assert_eq!(background_agents, vec![agent]),
            _ => panic!("Expected Snapshot"),
        }
    }

    #[test]
    fn stale_background_agent_update_does_not_replace_terminal_status() {
        let mut session = make_session();
        let completed = BackgroundAgentSummary {
            agent_id: "bgagent-cached".into(),
            parent_chat_id: session.chat_id.clone(),
            child_chat_id: Some("child-chat".into()),
            kind: "subagent".into(),
            status: "completed".into(),
            title: "Cached agent".into(),
            progress: None,
            step_count: 2,
            last_activity: None,
            target_files: vec![],
            edited_files: vec![],
            diff_summary: None,
            conflict_summary: None,
            result_summary: Some("done".into()),
            error: None,
            started_at: None,
            finished_at: Some("2026-05-28T00:00:00Z".into()),
            change_seq: 7,
            ..BackgroundAgentSummary::default()
        };
        let stale_running = BackgroundAgentSummary {
            status: "running".into(),
            finished_at: None,
            result_summary: None,
            change_seq: 6,
            ..completed.clone()
        };
        session.upsert_background_agent(completed.clone());
        session.upsert_background_agent(stale_running);

        let cached = session.background_agents.get("bgagent-cached").unwrap();
        assert_eq!(cached.status, "completed");
        assert_eq!(cached.change_seq, 7);
    }

    #[test]
    fn same_sequence_background_agent_update_keeps_non_terminal_existing() {
        let mut session = make_session();
        let running = BackgroundAgentSummary {
            agent_id: "bgagent-cached".into(),
            parent_chat_id: session.chat_id.clone(),
            child_chat_id: Some("child-chat".into()),
            kind: "subagent".into(),
            status: "running".into(),
            title: "Cached agent".into(),
            progress: Some("Applying patch".into()),
            step_count: 2,
            last_activity: None,
            target_files: vec![],
            edited_files: vec![],
            diff_summary: None,
            conflict_summary: None,
            result_summary: None,
            error: None,
            started_at: None,
            finished_at: None,
            change_seq: 7,
            ..BackgroundAgentSummary::default()
        };
        let stale_running = BackgroundAgentSummary {
            progress: Some("Queued".into()),
            ..running.clone()
        };
        session.upsert_background_agent(running.clone());
        session.upsert_background_agent(stale_running);

        let cached = session.background_agents.get("bgagent-cached").unwrap();
        assert_eq!(cached.progress.as_deref(), Some("Applying patch"));
    }

    #[test]
    fn tool_execution_progress_updates_last_tool_progress_at() {
        let mut session = make_session();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        let started = session.last_tool_started_at.unwrap();
        assert!(session.last_tool_progress_at.is_none());

        std::thread::sleep(std::time::Duration::from_millis(10));
        session.mark_tool_progress();

        let progress = session.last_tool_progress_at.unwrap();
        assert!(progress > started);
        assert_eq!(session.last_activity, progress);

        session.set_runtime_state(SessionState::Idle, None);
        assert!(session.last_tool_started_at.is_none());
        assert!(session.last_tool_progress_at.is_none());
    }

    #[test]
    fn set_runtime_state_updates_last_activity_on_change() {
        let mut session = make_session();
        let before = session.last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));

        session.set_runtime_state(SessionState::Generating, None);

        assert!(session.last_activity > before);
    }

    #[test]
    fn set_runtime_state_no_op_does_not_touch_activity() {
        let mut session = make_session();
        let before = session.last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));

        session.set_runtime_state(SessionState::Idle, None);

        assert_eq!(session.last_activity, before);
        assert_eq!(session.event_seq, 0);
    }

    #[test]
    fn set_runtime_state_idle_clears_active_compression_same_state() {
        let mut session = make_session();
        session.is_compressing = false;
        session.runtime.is_compressing = false;
        session.compression_phase = Some(CompressionPhase::Checking);
        session.runtime.compression_phase = Some(CompressionPhase::Checking);
        session.compression_attempt_generation = 1;
        session.active_compression_attempt = Some(1);
        let mut rx = session.subscribe();
        let before = session.last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));

        session.set_runtime_state(SessionState::Idle, None);

        assert!(!session.is_compressing);
        assert!(!session.runtime.is_compressing);
        assert_eq!(session.compression_phase, None);
        assert_eq!(session.runtime.compression_phase, None);
        assert_eq!(session.active_compression_attempt, None);
        assert!(session.last_activity > before);

        let json = rx.try_recv().unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::RuntimeUpdated {
                state,
                is_compressing,
                compression_phase,
                ..
            } => {
                assert_eq!(state, SessionState::Idle);
                assert!(!is_compressing);
                assert_eq!(compression_phase, None);
            }
            other => panic!("expected RuntimeUpdated, got {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn set_runtime_state_terminal_clears_active_running_compression() {
        let mut session = make_session();
        session.runtime.state = SessionState::Generating;
        session.is_compressing = true;
        session.runtime.is_compressing = true;
        session.compression_phase = Some(CompressionPhase::Running);
        session.runtime.compression_phase = Some(CompressionPhase::Running);
        session.compression_attempt_generation = 7;
        session.active_compression_attempt = Some(7);
        let mut rx = session.subscribe();

        session.set_runtime_state(SessionState::Completed, None);

        assert_eq!(session.runtime.state, SessionState::Completed);
        assert!(!session.is_compressing);
        assert!(!session.runtime.is_compressing);
        assert_eq!(session.compression_phase, None);
        assert_eq!(session.runtime.compression_phase, None);
        assert_eq!(session.active_compression_attempt, None);

        let json = rx.try_recv().unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::RuntimeUpdated {
                state,
                is_compressing,
                compression_phase,
                ..
            } => {
                assert_eq!(state, SessionState::Completed);
                assert!(!is_compressing);
                assert_ne!(compression_phase, Some(CompressionPhase::Running));
                assert_ne!(compression_phase, Some(CompressionPhase::Checking));
            }
            other => panic!("expected RuntimeUpdated, got {other:?}"),
        }
    }

    #[test]
    fn set_runtime_state_terminal_preserves_terminal_compression_phase() {
        let mut session = make_session();
        session.runtime.state = SessionState::Generating;
        session.compression_phase = Some(CompressionPhase::Applied);
        session.compression_reason = None;
        session.runtime.compression_phase = Some(CompressionPhase::Applied);
        session.runtime.compression_reason = None;
        let mut rx = session.subscribe();

        session.set_runtime_state(SessionState::Idle, None);

        assert!(!session.is_compressing);
        assert!(!session.runtime.is_compressing);
        assert_eq!(session.compression_phase, Some(CompressionPhase::Applied));
        assert_eq!(
            session.runtime.compression_phase,
            Some(CompressionPhase::Applied)
        );

        let json = rx.try_recv().unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::RuntimeUpdated {
                is_compressing,
                compression_phase,
                ..
            } => {
                assert!(!is_compressing);
                assert_eq!(compression_phase, Some(CompressionPhase::Applied));
            }
            other => panic!("expected RuntimeUpdated, got {other:?}"),
        }
    }

    #[test]
    fn set_runtime_state_terminal_clears_active_compression_attempt_token() {
        let mut session = make_session();
        session.compression_attempt_generation = 3;
        session.active_compression_attempt = Some(3);
        let mut rx = session.subscribe();

        session.set_runtime_state(SessionState::Idle, None);

        assert_eq!(session.compression_attempt_generation, 3);
        assert_eq!(session.active_compression_attempt, None);
        let json = rx.try_recv().unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        match envelope.event {
            ChatEvent::RuntimeUpdated {
                state,
                is_compressing,
                ..
            } => {
                assert_eq!(state, SessionState::Idle);
                assert!(!is_compressing);
            }
            other => panic!("expected RuntimeUpdated, got {other:?}"),
        }
    }

    #[test]
    fn test_set_runtime_state_clears_pause_on_transition() {
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
        assert!(!session.runtime.pause_reasons.is_empty());
        session.set_runtime_state(SessionState::Idle, None);
        assert!(session.runtime.pause_reasons.is_empty());
    }

    #[test]
    fn test_set_runtime_state_clears_wake_up_at_and_marks_dirty() {
        let mut session = make_session();
        session.runtime.state = SessionState::WaitingUserInput;
        session.wake_up_at = Some(chrono::Utc::now());
        session.trajectory_dirty = false;

        session.set_runtime_state(SessionState::Idle, None);

        assert!(session.wake_up_at.is_none());
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn replace_messages_resets_compaction_runtime_state() {
        let mut session = make_session();
        session.last_prompt_messages =
            vec![ChatMessage::new("user".to_string(), "old".to_string())];
        session.thread.previous_response_id = Some("resp-old".to_string());
        session.cache_guard_force_next = false;
        session.trajectory_dirty = false;

        session.replace_messages(vec![ChatMessage::new(
            "user".to_string(),
            "new".to_string(),
        )]);

        assert!(session.last_prompt_messages.is_empty());
        assert!(session.thread.previous_response_id.is_none());
        assert!(session.cache_guard_force_next);
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn turn_tool_pool_clears_when_a_turn_is_replaced_or_terminated() {
        let mut session = make_session();
        let pool = refact_runtime_api::TurnToolPool::new(());
        session.turn_tool_pool = Some(pool.clone());
        session.start_stream();

        session.set_runtime_state(SessionState::ExecutingTools, None);
        assert!(session.turn_tool_pool.is_some());

        session.finish_stream(None);
        assert!(session.turn_tool_pool.is_none());

        session.turn_tool_pool = Some(pool);
        session.replace_messages(Vec::new());
        assert!(session.turn_tool_pool.is_none());
    }

    #[test]
    fn leaving_waiting_user_input_clears_waiting_for_card_ids() {
        let mut session = make_session();
        session.runtime.state = SessionState::WaitingUserInput;
        session.waiting_for_card_ids = vec!["T-1".to_string(), "T-2".to_string()];
        session.trajectory_dirty = false;

        session.set_runtime_state(SessionState::Idle, None);

        assert!(session.waiting_for_card_ids.is_empty());
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn waiting_planner_with_future_wake_up_survives_cleanup() {
        let mut session = make_session();
        session.runtime.state = SessionState::WaitingUserInput;
        session.wake_up_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
        session.last_activity =
            Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);

        assert!(session.is_pending_wake_up());
        assert!(!session.is_idle_for_cleanup());
    }

    #[test]
    fn waiting_planner_with_past_wake_up_can_be_cleaned_up() {
        let mut session = make_session();
        session.runtime.state = SessionState::WaitingUserInput;
        session.wake_up_at = Some(chrono::Utc::now() - chrono::Duration::minutes(10));
        session.last_activity =
            Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);

        assert!(!session.is_pending_wake_up());
        assert!(session.is_idle_for_cleanup());
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
    fn test_is_runner_owned_subagent_view() {
        let mut session = make_session();
        session.thread.parent_id = Some("parent-1".to_string());
        session.thread.link_type = Some("subagent".to_string());

        for active_state in [
            SessionState::Starting,
            SessionState::Generating,
            SessionState::ExecutingTools,
        ] {
            session.runtime.state = active_state;
            assert!(
                session.is_runner_owned_subagent_view(),
                "state {active_state:?} should be runner owned"
            );
        }

        for inactive_state in [
            SessionState::Idle,
            SessionState::Completed,
            SessionState::Error,
            SessionState::Paused,
            SessionState::WaitingIde,
            SessionState::WaitingUserInput,
        ] {
            session.runtime.state = inactive_state;
            assert!(
                !session.is_runner_owned_subagent_view(),
                "state {inactive_state:?} should not be runner owned"
            );
        }

        session.runtime.state = SessionState::Generating;
        session.thread.parent_id = None;
        assert!(!session.is_runner_owned_subagent_view());

        session.thread.parent_id = Some("parent-1".to_string());
        for excluded_link in ["handoff", "mode_transition", "branch"] {
            session.thread.link_type = Some(excluded_link.to_string());
            assert!(!session.is_runner_owned_subagent_view());
        }
    }

    #[test]
    fn test_set_title() {
        let mut session = make_session();
        session.set_title("New Title".into(), true);
        assert_eq!(session.thread.title, "New Title");
        assert!(session.thread.is_title_generated);
        assert!(session.trajectory_dirty);
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
    fn ide_tool_result_emits_event_not_user_message() {
        let mut session = make_session();
        session.runtime.state = SessionState::WaitingIde;
        let mut rx = session.subscribe();

        session.record_ide_tool_result(
            "call_ide_1".to_string(),
            "The user accepted the changes.".to_string(),
            false,
        );

        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, "tool");
        assert_eq!(session.messages[0].tool_call_id, "call_ide_1");
        assert_eq!(session.messages[0].tool_failed, Some(false));
        assert_eq!(
            session.messages[1].role,
            crate::chat::internal_roles::EVENT_ROLE
        );
        assert!(!session.messages.iter().any(|message| {
            message.role == "user"
                && message
                    .content
                    .content_text_only()
                    .contains("accepted the changes")
        }));
        assert_eq!(
            session.messages[1].extra["event"]["subkind"],
            json!("ide_callback")
        );
        assert_eq!(
            session.messages[1].extra["event"]["source"],
            json!("ide.bridge")
        );
        assert_eq!(
            session.messages[1].extra["event"]["payload"],
            json!({
                "tool_call_id": "call_ide_1",
                "ok": true,
                "summary": "The user accepted the changes."
            })
        );
        assert_eq!(
            session.messages[1].content.content_text_only(),
            "The user accepted the changes."
        );
        assert_eq!(session.runtime.state, SessionState::WaitingIde);

        let mut event_roles = Vec::new();
        while let Ok(json) = rx.try_recv() {
            let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
            if let ChatEvent::MessageAdded { message, .. } = envelope.event {
                event_roles.push(message.role);
            }
        }
        assert_eq!(
            event_roles,
            vec!["tool", crate::chat::internal_roles::EVENT_ROLE]
        );
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
    fn test_increment_version() {
        let mut session = make_session();
        assert_eq!(session.trajectory_version, 0);
        assert!(!session.trajectory_dirty);
        session.increment_version();
        assert_eq!(session.trajectory_version, 1);
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn test_create_sessions_map() {
        let map = create_sessions_map();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let read = map.read().await;
            assert!(read.is_empty());
        });
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
    fn test_snapshot_includes_queued_items() {
        let mut session = make_session();
        session.command_queue.push_back(CommandRequest {
            client_request_id: "req-1".into(),
            priority: false,
            command: ChatCommand::UserMessage {
                content: json!("test"),
                attachments: vec![],
                context_files: vec![],
                suppress_auto_enrichment: false,
                client_message_id: None,
                push: None,
            },
        });
        let snap = session.snapshot();
        match snap {
            ChatEvent::Snapshot { runtime, .. } => {
                assert_eq!(runtime.queue_size, 1);
                assert_eq!(runtime.queued_items.len(), 1);
                assert_eq!(runtime.queued_items[0].client_request_id, "req-1");
            }
            _ => panic!("Expected Snapshot"),
        }
    }

    #[test]
    fn test_snapshot_includes_is_compressing() {
        let mut session = make_session();
        session.is_compressing = true;

        let snap = session.snapshot();

        match snap {
            ChatEvent::Snapshot { runtime, .. } => {
                assert!(runtime.is_compressing);
            }
            _ => panic!("Expected Snapshot"),
        }
    }

    #[test]
    fn test_touch_updates_last_activity() {
        let mut session = make_session();
        let before = session.last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));
        session.touch();
        assert!(session.last_activity > before);
    }

    #[tokio::test]
    async fn stream_delta_updates_last_activity() {
        let mut session = make_session();
        session.start_stream();
        let before = session.last_activity;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "hello".into(),
        }]);

        assert!(session.last_activity > before);
    }

    #[tokio::test]
    async fn stream_delta_only_resets_stream_timestamp_not_tool_timestamp() {
        let mut session = make_session();
        session.set_runtime_state(SessionState::ExecutingTools, None);
        let tool_started = session.last_tool_started_at;
        session.mark_tool_progress();
        let tool_progress = session.last_tool_progress_at;
        session.set_runtime_state(SessionState::Idle, None);
        session.last_tool_started_at = tool_started;
        session.last_tool_progress_at = tool_progress;

        session.start_stream();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        session.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: "hello".into(),
        }]);

        assert!(session.last_stream_delta_at.is_some());
        assert_eq!(session.last_tool_started_at, tool_started);
        assert_eq!(session.last_tool_progress_at, tool_progress);
    }

    #[tokio::test]
    async fn empty_stream_delta_does_not_update_last_activity() {
        let mut session = make_session();
        session.start_stream();
        let before = session.last_activity;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        session.emit_stream_delta(vec![]);

        assert_eq!(session.last_activity, before);
    }

    #[test]
    fn test_finish_stream_keeps_server_content_blocks_only_message() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![
            DeltaOp::AddServerContentBlock {
                block: json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_test",
                    "name": "web_search",
                    "input": {"query": "test"}
                }),
            },
            DeltaOp::AddServerContentBlock {
                block: json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_test",
                    "content": [{"type": "web_search_result", "title": "Result", "url": "https://example.com"}]
                }),
            },
        ]);
        session.finish_stream(Some("stop".to_string()));

        assert_eq!(
            session.messages.len(),
            1,
            "Server-blocks-only assistant message should be preserved"
        );
        assert_eq!(session.messages[0].server_content_blocks.len(), 2);
        assert_eq!(session.messages[0].role, "assistant");
    }

    #[test]
    fn test_finish_stream_discards_truly_empty_message() {
        let mut session = make_session();
        session.start_stream();
        // No deltas at all
        session.finish_stream(Some("stop".to_string()));

        assert_eq!(
            session.messages.len(),
            0,
            "Truly empty assistant message should be discarded"
        );
    }

    #[test]
    fn test_finish_stream_discards_usage_only_message() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::SetUsage {
            usage: json!({
                "prompt_tokens": 10,
                "completion_tokens": 0,
                "total_tokens": 10,
            }),
        }]);

        session.finish_stream(Some("stop".to_string()));

        assert!(session.messages.is_empty());
    }

    #[test]
    fn test_finish_stream_discards_extra_only_message() {
        let mut session = make_session();
        session.start_stream();
        session.emit_stream_delta(vec![DeltaOp::MergeExtra {
            extra: serde_json::Map::from_iter([(
                "openai_response_id".to_string(),
                json!("resp_123"),
            )]),
        }]);

        session.finish_stream(Some("stop".to_string()));

        assert!(session.messages.is_empty());
    }

    #[test]
    fn test_finish_stream_emits_removal_for_metadata_only_message() {
        let mut session = make_session();
        let mut rx = session.subscribe();
        let (message_id, _) = session.start_stream().unwrap();
        session.emit_stream_delta(vec![DeltaOp::MergeExtra {
            extra: serde_json::Map::from_iter([(
                "openai_response_id".to_string(),
                json!("resp_123"),
            )]),
        }]);

        session.finish_stream(Some("stop".to_string()));

        assert!(session.messages.is_empty());
        let mut found_removed = false;
        while let Ok(json) = rx.try_recv() {
            if let Ok(env) = serde_json::from_str::<EventEnvelope>(&json) {
                if matches!(env.event, ChatEvent::MessageRemoved { message_id: id } if id == message_id)
                {
                    found_removed = true;
                }
            }
        }
        assert!(found_removed);
    }

    /// Regression test: after a broadcast::Receiver lags, the handler must
    /// re-subscribe (`rx = session.subscribe()`) before capturing `event_seq`
    /// for the recovery snapshot.  Without re-subscribing, the old receiver
    /// resumes from the oldest ring-buffer entry whose seq is *lower* than the
    /// snapshot seq, causing the frontend to silently drop every subsequent event.
    ///
    /// This test simulates the handler's Lagged recovery path and asserts that
    /// the first event received after the snapshot has seq == snapshot_seq + 1.
    #[tokio::test]
    async fn test_lagged_recovery_seq_monotonicity() {
        use tokio::sync::broadcast::error::RecvError;

        // Use a tiny channel capacity so we only need to emit a handful of
        // events to trigger Lagged rather than the default 4096+.
        const SMALL_CAP: usize = 8;
        let mut session = make_session_with_capacity(SMALL_CAP);

        // Subscribe a "slow" receiver that we will intentionally lag.
        let mut slow_rx = session.subscribe();

        // Emit capacity+1 events so slow_rx is guaranteed to lag.
        let overflow_count = SMALL_CAP + 1;
        for _ in 0..overflow_count {
            session.emit(ChatEvent::QueueUpdated {
                queue_size: 0,
                queued_items: vec![],
            });
        }

        // Confirm that slow_rx is lagged.
        assert!(
            matches!(slow_rx.recv().await, Err(RecvError::Lagged(_))),
            "slow_rx should be lagged after overflow"
        );

        // --- Simulate the handler's recovery path ---
        // After Lagged, the handler must:
        //   1. Lock the session
        //   2. Re-subscribe to get a fresh receiver
        //   3. Capture event_seq for the recovery snapshot
        //   4. Drop the lock
        //   5. Emit one more event (from some background task)
        //   6. Assert first recv() on fresh_rx has seq == snapshot_seq + 1

        // Step 2-3: re-subscribe while holding the "lock" (single-threaded here).
        let mut fresh_rx = session.subscribe();
        let snapshot_seq = session.event_seq;

        // Step 5: emit one more event (e.g. a RuntimeUpdated broadcast).
        session.emit(ChatEvent::QueueUpdated {
            queue_size: 0,
            queued_items: vec![],
        });

        // Step 6: the first event from fresh_rx must have seq == snapshot_seq + 1.
        let json = fresh_rx
            .recv()
            .await
            .expect("fresh_rx should receive an event");

        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();

        assert_eq!(
            envelope.seq,
            snapshot_seq + 1,
            "First event after re-subscribe must have seq == snapshot_seq + 1, \
             got {} (snapshot_seq={}). \
             If seq < snapshot_seq the frontend drops all events forever.",
            envelope.seq,
            snapshot_seq
        );
    }

    #[test]
    #[ignore]
    fn stress_emit_and_snapshot_large_history_baseline() {
        const MESSAGE_COUNT: usize = 2_000;
        const MESSAGE_SIZE: usize = 2_048;
        const SNAPSHOT_RUNS: usize = 200;

        let mut session = make_session();

        for i in 0..MESSAGE_COUNT {
            session.add_message(ChatMessage {
                message_id: format!("m{}", i),
                role: if i % 2 == 0 {
                    "user".to_string()
                } else {
                    "assistant".to_string()
                },
                content: ChatContent::SimpleText("x".repeat(MESSAGE_SIZE)),
                ..Default::default()
            });
        }

        let emit_start = Instant::now();
        for _ in 0..1_500 {
            session.emit(ChatEvent::QueueUpdated {
                queue_size: 0,
                queued_items: vec![],
            });
        }
        let emit_elapsed = emit_start.elapsed();

        let snapshot_start = Instant::now();
        for _ in 0..SNAPSHOT_RUNS {
            let snapshot = session.snapshot();
            if let ChatEvent::Snapshot { messages, .. } = snapshot {
                assert_eq!(messages.len(), MESSAGE_COUNT);
            } else {
                panic!("Expected Snapshot event");
            }
        }
        let snapshot_elapsed = snapshot_start.elapsed();

        println!(
            "STRESS_BASELINE session_emit_snapshot: messages={}, msg_size={}, emits=1500, snapshots={}, emit_ms={}, snapshot_ms={}",
            MESSAGE_COUNT,
            MESSAGE_SIZE,
            SNAPSHOT_RUNS,
            emit_elapsed.as_millis(),
            snapshot_elapsed.as_millis(),
        );
    }

    #[test]
    #[ignore]
    fn stress_broadcast_lag_recovery_baseline() {
        let event_count = limits().event_channel_capacity * 3;

        let mut session = make_session();
        let mut slow_rx = session.subscribe();

        let emit_start = Instant::now();
        for i in 0..event_count {
            session.emit(ChatEvent::MessageAdded {
                message: ChatMessage {
                    message_id: format!("lag-{}", i),
                    role: "assistant".to_string(),
                    content: ChatContent::SimpleText("delta".to_string()),
                    ..Default::default()
                },
                index: i,
            });
        }
        let emit_elapsed = emit_start.elapsed();

        let recv_start = Instant::now();
        let mut received = 0usize;
        let mut lagged = 0usize;
        loop {
            match slow_rx.try_recv() {
                Ok(_envelope) => {
                    received += 1;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_skipped)) => {
                    lagged += 1;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    break;
                }
            }
        }
        let recv_elapsed = recv_start.elapsed();

        assert!(lagged > 0, "Expected lagged receiver under saturation");

        println!(
            "STRESS_BASELINE broadcast_lag: emitted={}, received={}, lagged_events={}, emit_ms={}, drain_ms={}, channel_capacity={}",
            event_count,
            received,
            lagged,
            emit_elapsed.as_millis(),
            recv_elapsed.as_millis(),
            limits().event_channel_capacity,
        );
    }

    #[test]
    fn test_active_command_initial_default() {
        let session = make_session();
        assert!(session.active_command.context_fork.is_none());
        assert!(session.active_command.model_override.is_none());
        assert!(session.active_command.allowed_tools.is_empty());
        assert!(session.active_command.name.is_empty());
    }

    #[test]
    fn test_active_command_stored_and_cleared() {
        let mut session = make_session();
        session.active_command = ActiveCommandContext {
            name: "my-agent".to_string(),
            allowed_tools: vec!["cat".to_string()],
            model_override: Some("gpt-4".to_string()),
            context_fork: Some("subagent".to_string()),
            started_at_index: None,
            activation_tool_call_id: None,
        };
        assert_eq!(
            session.active_command.context_fork,
            Some("subagent".to_string())
        );
        assert_eq!(session.active_command.name, "my-agent");
        session.active_command = ActiveCommandContext::default();
        assert!(session.active_command.context_fork.is_none());
        assert!(session.active_command.name.is_empty());
    }

    #[test]
    fn test_new_with_trajectory_active_command_default() {
        use crate::call_validation::{ChatContent};
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hello".into()),
            ..Default::default()
        };
        let thread = ThreadParams {
            id: "traj-fork".into(),
            ..Default::default()
        };
        let session = ChatSession::new_with_trajectory(
            "traj-fork".into(),
            vec![msg],
            thread,
            "2024-01-01T00:00:00Z".into(),
            None,
            Vec::new(),
            None,
        );
        assert!(session.active_command.context_fork.is_none());
        assert!(session.active_command.model_override.is_none());
    }

    #[test]
    fn test_set_clear_active_skill() {
        let mut session = make_session();
        assert!(session.thread.active_skill.is_none());
        session.set_active_skill("test-skill".to_string());
        assert_eq!(session.thread.active_skill, Some("test-skill".to_string()));
        assert!(session.trajectory_dirty);
        session.clear_active_skill();
        assert!(session.thread.active_skill.is_none());
    }

    fn make_user_message(text: &str) -> ChatMessage {
        ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "user".to_string(),
            content: crate::call_validation::ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_skill_deactivation_cleanup_compacts_messages() {
        let mut session = make_session();

        // Add 2 pre-skill messages
        session.add_message(make_user_message("pre-skill message 1"));
        session.add_message(make_user_message("pre-skill message 2"));
        let anchor = session.messages.len(); // = 2

        // Add skill-run messages that should be removed
        session.add_message(make_user_message("skill run message A"));
        session.add_message(make_user_message("skill run message B"));
        session.add_message(make_user_message("skill run message C"));
        assert_eq!(session.messages.len(), 5);

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: anchor,
            skill_name: "my-skill".to_string(),
            report: "Did useful things.".to_string(),
            activation_tool_call_id: None,
        });

        session.perform_skill_deactivation_cleanup();

        // 2 pre-skill messages + 1 report = 3 total
        assert_eq!(session.messages.len(), 3, "Expected 2 pre-skill + 1 report");
        let last = session.messages.last().unwrap();
        assert_eq!(last.role, "plain_text");
        if let crate::call_validation::ChatContent::SimpleText(ref text) = last.content {
            assert!(
                text.contains("## Skill Report: my-skill"),
                "Report header missing: {}",
                text
            );
            assert!(
                text.contains("Skill 'my-skill' executed successfully"),
                "Report preface missing: {}",
                text
            );
            assert!(
                text.contains("Did useful things."),
                "Report body missing: {}",
                text
            );
        } else {
            panic!("Expected SimpleText content in report message");
        }
        // pending is consumed
        assert!(session.pending_skill_deactivation.is_none());
    }

    #[test]
    fn test_skill_deactivation_cleanup_noop_when_no_pending() {
        let mut session = make_session();
        session.add_message(make_user_message("msg1"));
        session.add_message(make_user_message("msg2"));

        session.perform_skill_deactivation_cleanup();
        // Nothing changed
        assert_eq!(session.messages.len(), 2);
    }

    #[test]
    fn test_skill_deactivation_keeps_activation_tool_message() {
        let mut session = make_session();

        session.add_message(make_user_message("pre-skill"));
        let anchor = session.messages.len();

        let tool_message = ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText("Skill activated".to_string()),
            tool_call_id: "call_activate_skill".to_string(),
            tool_failed: Some(false),
            ..Default::default()
        };
        session.add_message(tool_message);

        session.add_message(ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "context_file".to_string(),
            content: ChatContent::SimpleText("Skill body".to_string()),
            ..Default::default()
        });
        session.add_message(make_user_message("skill run"));

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: anchor,
            skill_name: "tool-skill".to_string(),
            report: "Wrapped up".to_string(),
            activation_tool_call_id: Some("call_activate_skill".to_string()),
        });

        session.perform_skill_deactivation_cleanup();

        assert_eq!(
            session.messages.len(),
            3,
            "Expected pre-skill + tool + report"
        );
        assert_eq!(
            session.messages[1].role, "tool",
            "Activation tool message must remain"
        );
        assert_eq!(session.messages[1].tool_call_id, "call_activate_skill");
        assert_eq!(session.messages.last().unwrap().role, "plain_text");
    }

    #[test]
    fn test_skill_deactivation_skips_exact_activation_tool_call_id() {
        let mut session = make_session();

        session.add_message(make_user_message("pre-skill"));
        let anchor = session.messages.len();

        let unrelated_tool = ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText("Unrelated tool".to_string()),
            tool_call_id: "call_other_tool".to_string(),
            tool_failed: Some(false),
            ..Default::default()
        };
        session.add_message(unrelated_tool);

        let activation_tool = ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText("Skill activated".to_string()),
            tool_call_id: "call_activate_skill".to_string(),
            tool_failed: Some(false),
            ..Default::default()
        };
        session.add_message(activation_tool);

        session.add_message(ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "cd_instruction".to_string(),
            content: ChatContent::SimpleText("Skill body".to_string()),
            ..Default::default()
        });
        session.add_message(make_user_message("skill run"));

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: anchor,
            skill_name: "tool-skill".to_string(),
            report: "Wrapped up".to_string(),
            activation_tool_call_id: Some("call_activate_skill".to_string()),
        });

        session.perform_skill_deactivation_cleanup();

        assert_eq!(
            session.messages.len(),
            3,
            "Expected pre-skill + activation tool + report"
        );
        assert_eq!(session.messages[1].tool_call_id, "call_activate_skill");
    }

    #[test]
    fn test_skill_deactivation_without_anchor_still_records_report() {
        let mut session = make_session();
        session.add_message(make_user_message("pre-skill"));

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: session.messages.len(),
            skill_name: "no-anchor".to_string(),
            report: "All done".to_string(),
            activation_tool_call_id: None,
        });

        session.perform_skill_deactivation_cleanup();

        assert_eq!(session.messages.len(), 2, "Expected pre-skill + report");
        let last = session.messages.last().unwrap();
        assert_eq!(last.role, "plain_text");
        if let crate::call_validation::ChatContent::SimpleText(ref text) = last.content {
            assert!(text.contains("## Skill Report: no-anchor"));
            assert!(text.contains("All done"));
        }
    }

    #[test]
    fn test_skill_deactivation_cleanup_rejects_out_of_range_index() {
        let mut session = make_session();
        session.add_message(make_user_message("only message"));
        assert_eq!(session.messages.len(), 1);

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: 99, // beyond messages.len()
            skill_name: "bad-skill".to_string(),
            report: "report".to_string(),
            activation_tool_call_id: None,
        });

        session.perform_skill_deactivation_cleanup();

        // No truncation, no report added — skipped with warning
        assert_eq!(
            session.messages.len(),
            1,
            "Messages must not be modified on bad index"
        );
        assert!(
            session.pending_skill_deactivation.is_none(),
            "pending must be consumed even on skip"
        );
    }

    #[test]
    fn test_new_with_trajectory_clears_active_skill() {
        let thread = ThreadParams {
            id: "t1".into(),
            active_skill: Some("leftover-skill".to_string()),
            ..Default::default()
        };
        let session = ChatSession::new_with_trajectory(
            "t1".into(),
            vec![],
            thread,
            "2024-01-01T00:00:00Z".into(),
            None,
            Vec::new(),
            None,
        );
        assert!(
            session.thread.active_skill.is_none(),
            "active_skill must be cleared on restore: compaction anchor is lost after restart"
        );
    }

    #[test]
    fn test_emit_broadcast_is_valid_json() {
        let mut session = make_session();
        let mut rx = session.subscribe();
        session.emit(ChatEvent::PauseCleared {});
        let json = rx.try_recv().unwrap();
        let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(envelope.chat_id, "test-chat");
        assert_eq!(envelope.seq, 1);
        assert!(matches!(envelope.event, ChatEvent::PauseCleared {}));
    }

    #[test]
    fn test_emit_broadcast_multiple_subscribers_identical_payload() {
        let mut session = make_session();
        let mut rx1 = session.subscribe();
        let mut rx2 = session.subscribe();
        session.emit(ChatEvent::PauseCleared {});
        let j1 = rx1.try_recv().unwrap();
        let j2 = rx2.try_recv().unwrap();
        assert_eq!(j1, j2);
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

    fn make_assistant_with_tool_calls(ids: &[&str]) -> ChatMessage {
        use crate::call_validation::{ChatToolCall, ChatToolFunction};
        ChatMessage {
            role: "assistant".to_string(),
            tool_calls: Some(
                ids.iter()
                    .map(|id| ChatToolCall {
                        id: id.to_string(),
                        index: None,
                        function: ChatToolFunction {
                            name: "shell".to_string(),
                            arguments: "{}".to_string(),
                        },
                        tool_type: "function".to_string(),
                        extra_content: None,
                        started_at_ms: None,
                        completed_at_ms: None,
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn make_pause_reason(tool_call_id: &str) -> PauseReason {
        PauseReason {
            reason_type: "confirmation".into(),
            tool_name: "shell".into(),
            command: "shell".into(),
            rule: "ask".into(),
            tool_call_id: tool_call_id.into(),
            integr_config_path: None,
        }
    }

    fn make_tool_result(tool_call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            tool_call_id: tool_call_id.to_string(),
            content: ChatContent::SimpleText(content.to_string()),
            tool_failed: Some(false),
            ..Default::default()
        }
    }

    fn role_sequence(session: &ChatSession) -> Vec<&str> {
        session
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect()
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

    #[test]
    fn update_legacy_source_requires_explicit_rebuild() {
        let mut session = make_session();
        session.messages = summary_fixture_messages();
        let before = serde_json::to_value(&session.messages).unwrap();
        let replacement = ChatMessage::new("assistant".into(), "edited".into());
        assert!(session.update_message("source-id", replacement).is_none());
        assert_eq!(serde_json::to_value(&session.messages).unwrap(), before);
    }

    fn summary_fixture_messages() -> Vec<ChatMessage> {
        let mut source = ChatMessage::new("assistant".to_string(), "original answer".to_string());
        source.message_id = "source-id".to_string();
        let mut summary = ChatMessage::new("assistant".to_string(), "summary".to_string());
        summary.message_id = "summary-id".to_string();
        summary.summarization_tier = Some("llm_segment_summary".to_string());
        summary.extra.insert(
            "compression".to_string(),
            json!({
                "kind": "llm_segment_summary",
                "insert_mode": "source_preserving",
                "source_hash": "hash-1",
                "summarized_source_message_ids": ["source-id"],
            }),
        );
        let mut report = ChatMessage::new("compression_report".to_string(), "report".to_string());
        report.message_id = "report-id".to_string();
        report.extra.insert(
            "compression_report".to_string(),
            json!({ "kind": "chat_compression_report", "source_hash": "hash-1" }),
        );
        let mut unrelated_summary =
            ChatMessage::new("assistant".to_string(), "other summary".to_string());
        unrelated_summary.message_id = "other-summary-id".to_string();
        unrelated_summary.summarization_tier = Some("llm_segment_summary".to_string());
        unrelated_summary.extra.insert(
            "compression".to_string(),
            json!({
                "kind": "llm_segment_summary",
                "source_hash": "hash-2",
                "summarized_source_message_ids": ["other-source-id"],
            }),
        );
        let mut other_source =
            ChatMessage::new("assistant".to_string(), "other answer".to_string());
        other_source.message_id = "other-source-id".to_string();
        let mut user_msg = ChatMessage::new("user".to_string(), "question".to_string());
        user_msg.message_id = "user-id".to_string();
        vec![
            user_msg,
            other_source,
            unrelated_summary,
            report,
            summary,
            source,
        ]
    }

    #[test]
    fn remove_legacy_source_preserves_archive() {
        let mut session = make_session();
        session.messages = summary_fixture_messages();
        let before = serde_json::to_value(&session.messages).unwrap();
        assert!(session.remove_message("source-id").is_none());
        assert_eq!(serde_json::to_value(&session.messages).unwrap(), before);
    }

    #[test]
    fn truncate_preserves_legacy_summaries_and_blocks_generation() {
        let mut session = make_session();
        session.messages = summary_fixture_messages();
        let prefix = serde_json::to_value(&session.messages[..5]).unwrap();
        session.truncate_messages(5);
        assert_eq!(serde_json::to_value(&session.messages).unwrap(), prefix);
        assert!(session.start_stream().is_none());
    }

    #[test]
    fn reset_compaction_runtime_state_clears_reactive_breaker_and_hashes() {
        let mut session = make_session();
        session.thread.reactive_compact_attempts = Some(2);
        session.pending_max_new_tokens_boost = Some(16_000);

        session.reset_compaction_runtime_state();

        assert_eq!(session.thread.reactive_compact_attempts, None);
        assert!(session.pending_max_new_tokens_boost.is_none());
    }

#[cfg(test)]
mod accepted_control_tests {
    use super::*;
    use crate::chat::{goal_role, internal_roles, plan_role};

    #[test]
    fn accepted_control_pending_base_delta_and_cancellation() {
        for kind in ["plan", "goal"] {
            let mut session = ChatSession::new("accepted".into());
            session.turn_depth = 1;
            let base = if kind == "plan" {
                internal_roles::plan("agent", 1, "base", None)
            } else {
                internal_roles::goal("agent", 1, "base", None, true, GoalBudget::default())
            };
            let base = PendingDelivery::new(vec![base], PushMode::WhenIdle, "test", false);
            let base_id = base.id.clone();
            session.enqueue_delivery(base).unwrap();
            if kind == "plan" {
                session.install_plan("agent", "replacement");
                assert!(plan_role::current_base_plan(&session).is_some());
            } else {
                session.install_goal("agent", "replacement", true, GoalBudget::default());
                assert!(goal_role::current_base_goal(&session).is_some());
            }
            assert!(session.messages.is_empty());
            let delta = if kind == "plan" {
                internal_roles::plan_delta("test", json!({"seq": 1}), "update")
            } else {
                internal_roles::goal_delta("test", json!({"seq": 1}), "update")
            };
            let delta = PendingDelivery::new(vec![delta], PushMode::WhenIdle, "test", false);
            let delta_id = delta.id.clone();
            session.enqueue_delivery(delta).unwrap();
            let projection = session.accepted_control_projection();
            let content = if kind == "plan" {
                plan_role::synthesize_current_plan(&projection)
            } else {
                goal_role::synthesize_current_goal(&projection)
            }
            .unwrap();
            assert!(content.starts_with("base"));
            assert!(content.ends_with("update"));
            assert!(session.drain_pending_deliveries().0.is_empty());
            assert!(session
                .update_pending_delivery(&base_id, None, true)
                .is_err());
            assert!(!session
                .update_pending_delivery(&delta_id, Some(PushMode::Preempt), false)
                .unwrap());
            assert!(session.messages.is_empty());
            session
                .update_pending_delivery(&delta_id, None, true)
                .unwrap();
            session
                .update_pending_delivery(&base_id, None, true)
                .unwrap();
            assert_eq!(session.accepted_control_messages().count(), 0);
            assert!(session.messages.is_empty());
        }
    }

    #[test]
    fn accepted_control_deduplicates_and_delivers_in_order() {
        let mut session = ChatSession::new("accepted".into());
        session.turn_depth = 1;
        let base = internal_roles::plan("agent", 1, "base", None);
        let mut delta = internal_roles::plan_delta("test", json!({"seq": 1}), "update");
        delta.message_id = "delta".into();
        session
            .enqueue_delivery(PendingDelivery::new(
                vec![base],
                PushMode::WhenIdle,
                "test",
                false,
            ))
            .unwrap();
        session
            .enqueue_delivery(PendingDelivery::new(
                vec![delta.clone()],
                PushMode::WhenIdle,
                "test",
                false,
            ))
            .unwrap();
        assert!(session.drain_pending_deliveries().0.is_empty());
        session.turn_depth = 0;
        assert_eq!(session.drain_pending_deliveries().0.len(), 2);
        session.queue_post_tool_side_effect(delta);
        session.queue_post_tool_side_effect(ChatMessage::new("user".into(), "not control".into()));
        assert_eq!(session.accepted_control_messages().count(), 2);
        assert_eq!(session.messages[0].content.content_text_only(), "base");
        assert_eq!(session.messages[1].content.content_text_only(), "update");
    }
}

#[cfg(test)]
mod archival_mutation_tests {
    use super::*;
    use refact_core::active_context::{
        active_context, make_reconstruction_report, writeback_active_context,
        ReconstructionMetadata,
    };

    fn message(id: &str, role: &str, text: &str) -> ChatMessage {
        let mut message = ChatMessage::new(role.into(), text.into());
        message.message_id = id.into();
        message
    }

    fn session() -> ChatSession {
        let mut session = ChatSession::new("archive".into());
        let report = make_reconstruction_report(
            vec![message("payload", "user", "active context")],
            ReconstructionMetadata::default(),
        )
        .unwrap();
        session.messages = vec![
            message("archive", "user", "historical"),
            report,
            message("suffix", "user", "latest"),
        ];
        session
    }

    #[test]
    fn authoritative_report_and_archive_reject_generic_mutations() {
        let mut session = session();
        let before = serde_json::to_value(&session.messages).unwrap();
        let version = session.trajectory_version;
        let report_id = session.messages[1].message_id.clone();
        for id in ["archive", report_id.as_str()] {
            assert!(session.remove_message(id).is_none());
            assert!(session
                .update_message(id, message(id, "user", "edited"))
                .is_none());
        }
        assert_eq!(serde_json::to_value(&session.messages).unwrap(), before);
        assert_eq!(session.trajectory_version, version);
        assert_eq!(
            active_context(&session.messages).unwrap().messages[0].message_id,
            "payload"
        );
    }

    #[test]
    fn suffix_edit_preserves_report_and_invalidates_captured_writeback() {
        let mut session = session();
        let original = active_context(&session.messages).unwrap();
        let archive = serde_json::to_value(&session.messages[..2]).unwrap();
        let version = session.trajectory_version;
        assert_eq!(
            session.update_message("suffix", message("suffix", "user", "edited")),
            Some(2)
        );
        assert!(session.trajectory_version > version);
        assert_eq!(
            serde_json::to_value(&session.messages[..2]).unwrap(),
            archive
        );
        assert!(
            writeback_active_context(&session.messages, &original, &original.messages).is_err()
        );
        assert_eq!(session.remove_message("suffix"), Some(2));
        assert_eq!(serde_json::to_value(&session.messages).unwrap(), archive);
    }

    #[test]
    fn explicit_truncate_can_resume_pre_report_history() {
        let mut session = session();
        session.truncate_messages(1);
        assert_eq!(
            active_context(&session.messages).unwrap().messages[0].message_id,
            "archive"
        );
        assert!(session.start_stream().is_some());
    }

    #[test]
    fn invalid_boundary_preserves_goal_projection_and_rejects_install() {
        let mut session = ChatSession::new("invalid".into());
        session.install_goal("agent", "live goal", true, GoalBudget::default());
        let prior = serde_json::to_value(&session.goal).unwrap();
        let mut invalid = message("invalid", "compression_report", "broken");
        invalid.extra.insert(
            "compression_report".into(),
            json!({"kind":"reconstructed_history", "schema_version":999}),
        );
        session.messages.push(invalid);
        let before = serde_json::to_value(&session.messages).unwrap();
        session.rebuild_goal_projection_from_messages();
        assert_eq!(serde_json::to_value(&session.goal).unwrap(), prior);
        assert!(session.try_accepted_control_projection().is_err());
        assert!(crate::chat::goal_role::try_current_base_goal(&session).is_err());
        assert!(session
            .enqueue_delivery(PendingDelivery::new(
                vec![crate::chat::internal_roles::goal_delta(
                    "test",
                    json!({"seq": 1}),
                    "must not enqueue",
                )],
                PushMode::WhenIdle,
                "test",
                false,
            ))
            .is_err());
        assert!(session.pending_deliveries.is_empty());
        session.install_goal("agent", "must not install", true, GoalBudget::default());
        assert_eq!(serde_json::to_value(&session.messages).unwrap(), before);
        assert!(session
            .enqueue_delivery(PendingDelivery::new(
                vec![crate::chat::internal_roles::goal_delta(
                    "test",
                    json!({"seq": 1}),
                    "must not enqueue",
                )],
                PushMode::Append,
                "test",
                false,
            ))
            .is_err());
        assert!(session.pending_deliveries.is_empty());
        assert!(session.start_stream().is_none());
    }

    #[test]
    fn payload_controls_and_queued_deltas_project_once_without_archived_controls() {
        use crate::chat::{goal_role, internal_roles, plan_role};
        for kind in ["goal", "plan"] {
            let mut session = session();
            let mut base = if kind == "goal" {
                internal_roles::goal("agent", 1, "active base", None, true, GoalBudget::default())
            } else {
                internal_roles::plan("agent", 1, "active base", None)
            };
            base.message_id = "base".into();
            let mut archived = base.clone();
            archived.message_id = "archived-control".into();
            archived.content = ChatContent::SimpleText("obsolete base".into());
            archived.extra.get_mut(kind).unwrap()["version"] = json!(99);
            let mut delta = if kind == "goal" {
                internal_roles::goal_delta("test", json!({"seq":1}), "payload delta")
            } else {
                internal_roles::plan_delta("test", json!({"seq":1}), "payload delta")
            };
            delta.message_id = "delta".into();
            session.messages = vec![
                archived,
                make_reconstruction_report(
                    vec![base, delta.clone()],
                    ReconstructionMetadata::default(),
                )
                .unwrap(),
            ];
            session.queue_post_tool_side_effect(delta);
            let mut queued = if kind == "goal" {
                internal_roles::goal_delta("test", json!({"seq":2}), "queued delta")
            } else {
                internal_roles::plan_delta("test", json!({"seq":2}), "queued delta")
            };
            queued.message_id = "queued".into();
            session.pending_deliveries.push_back(PendingDelivery::new(
                vec![queued],
                PushMode::WhenIdle,
                "test",
                false,
            ));
            let before = serde_json::to_value(&session.messages).unwrap();
            let projection = session.try_accepted_control_projection().unwrap();
            assert_eq!(projection.messages.len(), 3);
            let content = if kind == "goal" {
                assert_eq!(
                    goal_role::current_base_goal(&session).unwrap().message_id,
                    "base"
                );
                goal_role::synthesize_current_goal(&projection)
            } else {
                assert_eq!(
                    plan_role::try_current_base_plan(&session)
                        .unwrap()
                        .unwrap()
                        .message_id,
                    "base"
                );
                plan_role::synthesize_current_plan(&projection)
            }
            .unwrap();
            assert!(!content.contains("obsolete"));
            assert_eq!(content.matches("payload delta").count(), 1);
            assert_eq!(content.matches("queued delta").count(), 1);
            assert_eq!(serde_json::to_value(&session.messages).unwrap(), before);
        }
    }

    #[test]
    fn transferred_goal_payload_writeback_preserves_archived_original() {
        use crate::chat::internal_roles;
        let mut session = session();
        let mut goal = internal_roles::goal("agent", 1, "live", None, true, GoalBudget::default());
        goal.message_id = "goal".into();
        session.messages = vec![
            goal.clone(),
            make_reconstruction_report(vec![goal], ReconstructionMetadata::default()).unwrap(),
        ];
        let archive = serde_json::to_value(&session.messages[0]).unwrap();
        let original = active_context(&session.messages).unwrap();
        let mut transformed = original.messages.clone();
        transformed[0].extra.get_mut("goal").unwrap()["active"] = json!(false);
        transformed[0].extra.get_mut("goal").unwrap()["status"] = json!("transferred");
        transformed[0].extra.get_mut("goal").unwrap()["transferred_to"] = json!("target");
        session.replace_messages(
            writeback_active_context(&session.messages, &original, &transformed).unwrap(),
        );
        assert_eq!(serde_json::to_value(&session.messages[0]).unwrap(), archive);
        assert_eq!(
            session.goal.as_ref().unwrap().status,
            GoalStatus::Transferred
        );
        assert_eq!(
            session.goal.as_ref().unwrap().transferred_to.as_deref(),
            Some("target")
        );
    }
}
