use super::*;
use super::tests_support::*;
use super::super::types::{ChatCommand, CommandRequest};
use crate::chat::perf_diagnostics;
use crate::call_validation::{ChatToolCall, ChatToolFunction};
use serde_json::json;
use std::time::Instant;

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
