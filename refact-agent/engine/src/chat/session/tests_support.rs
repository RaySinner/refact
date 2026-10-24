use super::*;
use crate::chat::perf_diagnostics::{self, MemoryPerfSink, PerfClock, PerfRecorder};
use serde_json::json;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

    pub(super) fn make_session() -> ChatSession {
        ChatSession::new("test-chat".to_string())
    }

    pub(super) fn turn_memory_catalog() -> Arc<refact_runtime_api::ToolCatalogSnapshot> {
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

    pub(super) fn install_turn_memory_state(session: &mut ChatSession) {
        session.last_prompt_messages = vec![ChatMessage::new(
            "user".to_string(),
            "prepared prompt".repeat(16_384),
        )];
        session.tool_catalog = Some(turn_memory_catalog());
        session.turn_tool_pool = Some(refact_runtime_api::TurnToolPool::new(()));
    }

    pub(super) fn assert_turn_memory_released(session: &ChatSession) {
        assert!(session.last_prompt_messages.is_empty());
        assert!(session.tool_catalog.is_none());
        assert!(session.turn_tool_pool.is_none());
    }

    pub(super) struct TestClock {
        now: AtomicU64,
    }

    impl TestClock {
        pub(super) fn new(now: u64) -> Self {
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

    pub(super) fn install_perf_recorder() -> (
        perf_diagnostics::TestRecorderGuard,
        Arc<MemoryPerfSink>,
    ) {
        let sink = Arc::new(MemoryPerfSink::new());
        let recorder = Arc::new(PerfRecorder::with_salt(
            Arc::new(TestClock::new(0)),
            sink.clone(),
            [9; 32],
        ));
        (perf_diagnostics::install_test_recorder(recorder), sink)
    }

    pub(super) fn delivery_fixture(id: &str, push: PushMode) -> PendingDelivery {
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

    pub(super) fn now_millis() -> u64 {
        chrono::Utc::now().timestamp_millis().max(0) as u64
    }

    pub(super) fn reserve_compression(session: &mut ChatSession) {
        session.active_compression_attempt = Some(7);
        session.compression_attempt_started_at_ms = Some(now_millis());
        session.compression_abort_flag = Some(Arc::new(AtomicBool::new(false)));
    }

    /// Creates a session with a small broadcast channel capacity, useful for
    /// triggering `RecvError::Lagged` quickly in tests without emitting
    /// thousands of events.
    pub(super) fn make_session_with_capacity(capacity: usize) -> ChatSession {
        let (event_tx, _) = broadcast::channel::<Arc<String>>(capacity);
        let mut session = ChatSession::new("test-chat-small".to_string());
        session.event_tx = event_tx;
        session
    }

    pub(super) fn make_user_message(text: &str) -> ChatMessage {
        ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "user".to_string(),
            content: crate::call_validation::ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    pub(super) fn make_assistant_with_tool_calls(ids: &[&str]) -> ChatMessage {
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

    pub(super) fn make_pause_reason(tool_call_id: &str) -> PauseReason {
        PauseReason {
            reason_type: "confirmation".into(),
            tool_name: "shell".into(),
            command: "shell".into(),
            rule: "ask".into(),
            tool_call_id: tool_call_id.into(),
            integr_config_path: None,
        }
    }

    pub(super) fn make_tool_result(tool_call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            tool_call_id: tool_call_id.to_string(),
            content: ChatContent::SimpleText(content.to_string()),
            tool_failed: Some(false),
            ..Default::default()
        }
    }

    pub(super) fn role_sequence(session: &ChatSession) -> Vec<&str> {
        session
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect()
    }

    pub(super) fn summary_fixture_messages() -> Vec<ChatMessage> {
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
