use super::*;
use super::tests_support::*;
use super::super::types::{ChatCommand, CommandRequest};
use crate::chat::perf_diagnostics;
use crate::chat::perf_telemetry::PerformanceTelemetry;
use serde_json::json;
use std::time::Instant;
use std::sync::Arc;

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
