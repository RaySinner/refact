// The four chat command handlers now live in `refact_http::chat_handlers`; this
// file re-exports them so `crate::chat::handle_v1_chat_*` keeps resolving.
//
// The tests stay in the engine: they build a real `AppState` from
// `make_test_gcx`, which only the engine can construct.
pub use refact_http::chat_handlers::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::perf_diagnostics::{
        self, MemoryPerfSink, PerfClock, PerfOutcome, PerfRecorder,
    };
    use crate::call_validation::{ChatContent, ChatMessage, ChatToolCall, ChatToolFunction};
    use crate::chat::types::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    struct TestClock {
        now: AtomicU64,
    }

    impl PerfClock for TestClock {
        fn now_us(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }
    }

    fn install_perf_recorder() -> (
        perf_diagnostics::TestRecorderGuard,
        Arc<MemoryPerfSink>,
        Arc<PerfRecorder>,
    ) {
        let sink = Arc::new(MemoryPerfSink::new());
        let recorder = Arc::new(PerfRecorder::with_salt(
            Arc::new(TestClock {
                now: AtomicU64::new(0),
            }),
            sink.clone(),
            [11; 32],
        ));
        (
            perf_diagnostics::install_test_recorder(recorder.clone()),
            sink,
            recorder,
        )
    }

    #[test]
    fn perf_diagnostics_lagged_recovery_records_skip_count() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink, _) = install_perf_recorder();

        record_lagged_recovery("chat-lag", PerfOutcome::Success, 47, 3);

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].component, "sse.lagged");
        assert_eq!(events[0].elapsed_us, 47);
        assert_eq!(events[0].item_count, Some(3));
    }

    #[test]
    fn perf_diagnostics_initial_and_recovery_sse_serialization_record_outcomes() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink, recorder) = install_perf_recorder();

        record_sse_serialize("initial-chat", PerfOutcome::Success, 17, Some(23));
        record_sse_serialize("recovery-chat", PerfOutcome::Failure, 19, None);

        let initial_chat_hash = recorder.hash_identity_for_test("initial-chat");
        let recovery_chat_hash = recorder.hash_identity_for_test("recovery-chat");
        let events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| {
                event.component == "sse.serialize"
                    && matches!(
                        event.chat_id_hash.as_deref(),
                        Some(hash) if hash == initial_chat_hash || hash == recovery_chat_hash
                    )
            })
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].component, "sse.serialize");
        assert_eq!(events[0].outcome, "success");
        assert_eq!(events[0].elapsed_us, 17);
        assert_eq!(events[0].size_bytes, Some(23));
        assert_eq!(events[1].component, "sse.serialize");
        assert_eq!(events[1].outcome, "failure");
        assert_eq!(events[1].elapsed_us, 19);
        assert_eq!(events[1].size_bytes, None);
    }

    async fn test_app_with_workspace(root: &std::path::Path) -> crate::app_state::AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        *app.workspace
            .documents_state
            .workspace_folders
            .lock()
            .unwrap() = vec![root.to_path_buf()];
        app
    }

    #[tokio::test]
    async fn delivery_command_edits_bypass_busy_processor() {
        let workspace = tempfile::tempdir().unwrap();
        let app = test_app_with_workspace(workspace.path()).await;
        let chat_id = "live-delivery-edit";
        let session = Arc::new(tokio::sync::Mutex::new(ChatSession::new(chat_id.into())));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.into(), session.clone());
        session.lock().await.start_stream().unwrap();
        let delivery = PendingDelivery::with_id(
            "live",
            vec![ChatMessage::new("user".into(), "notice".into())],
            PushMode::Append,
            "test",
            false,
        );
        for command in [
            ChatCommand::DeliverMessages { delivery },
            ChatCommand::UpdatePendingDelivery {
                delivery_id: "live".into(),
                push: Some(PushMode::Preempt),
                cancel: false,
            },
        ] {
            let request = CommandRequest {
                client_request_id: uuid::Uuid::new_v4().to_string(),
                priority: false,
                command,
            };
            let response = handle_v1_chat_command(
                axum::extract::State(app.clone()),
                axum::extract::Path(chat_id.into()),
                serde_json::to_vec(&request).unwrap().into(),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
        }
        let session = session.lock().await;
        assert!(session.command_queue.is_empty());
        assert!(session.pending_deliveries.is_empty());
        assert!(session.draft_message.is_none());
    }

    #[tokio::test]
    async fn abort_command_clears_and_persists_running_sleep_tool_call() {
        let workspace = tempfile::tempdir().unwrap();
        let app = test_app_with_workspace(workspace.path()).await;
        let chat_id = "abort-running-sleep";
        let session_arc = Arc::new(tokio::sync::Mutex::new(ChatSession::new(
            chat_id.to_string(),
        )));
        let mut events = {
            let mut session = session_arc.lock().await;
            session.set_runtime_state(SessionState::ExecutingTools, None);
            session.messages.push(ChatMessage::new(
                "user".to_string(),
                "Wait briefly".to_string(),
            ));
            session.messages.push(ChatMessage {
                message_id: "assistant-with-sleep".to_string(),
                role: "assistant".to_string(),
                content: ChatContent::SimpleText("Waiting briefly.".to_string()),
                tool_calls: Some(vec![ChatToolCall {
                    id: "sleep-call".to_string(),
                    index: Some(0),
                    tool_type: "function".to_string(),
                    function: ChatToolFunction {
                        name: "sleep".to_string(),
                        arguments: json!({
                            "duration_ms": 30_000,
                            "description": "Wait briefly",
                        })
                        .to_string(),
                    },
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                }]),
                ..Default::default()
            });
            session.subscribe()
        };
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        let request = CommandRequest {
            client_request_id: "wake-up-request".to_string(),
            priority: false,
            command: ChatCommand::Abort {},
        };
        let body = hyper::body::Bytes::from(serde_json::to_vec(&request).unwrap());

        let response = handle_v1_chat_command(
            axum::extract::State(app.clone()),
            axum::extract::Path(chat_id.to_string()),
            body,
        )
        .await
        .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let session = session_arc.lock().await;
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert!(session.messages[1].tool_calls.is_none());
        assert!(!session.trajectory_dirty);
        drop(session);

        let mut saw_sleep_removed = false;
        while let Ok(json) = events.try_recv() {
            let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
            if let ChatEvent::MessageUpdated { message, .. } = envelope.event {
                saw_sleep_removed =
                    message.message_id == "assistant-with-sleep" && message.tool_calls.is_none();
            }
        }
        assert!(saw_sleep_removed);

        let trajectory_path = workspace
            .path()
            .join(".refact")
            .join("trajectories")
            .join(chat_id)
            .join(format!("{chat_id}.json"));
        let trajectory: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(trajectory_path).await.unwrap())
                .unwrap();
        assert_eq!(
            trajectory["messages"][1]["tool_calls"],
            serde_json::Value::Null
        );
    }

    #[tokio::test]
    async fn cancel_queued_command_clears_queue_wait_timestamp() {
        let workspace = tempfile::tempdir().unwrap();
        let app = test_app_with_workspace(workspace.path()).await;
        let chat_id = "cancel-queued";
        let session_arc = Arc::new(tokio::sync::Mutex::new(ChatSession::new(
            chat_id.to_string(),
        )));
        {
            let mut session = session_arc.lock().await;
            session.command_queue.push_back(CommandRequest {
                client_request_id: "queued-request".to_string(),
                priority: false,
                command: ChatCommand::Regenerate {},
            });
            session
                .command_enqueued_at
                .insert("queued-request".to_string(), std::time::Instant::now());
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        let response = handle_v1_chat_cancel_queued(
            axum::extract::State(app),
            axum::extract::Path((chat_id.to_string(), "queued-request".to_string())),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let session = session_arc.lock().await;
        assert!(session.command_queue.is_empty());
        assert!(session.command_enqueued_at.is_empty());
    }

    #[tokio::test]
    async fn reprioritize_queued_multimodal_command_preserves_payload_and_timestamp() {
        let workspace = tempfile::tempdir().unwrap();
        let app = test_app_with_workspace(workspace.path()).await;
        let chat_id = "reprioritize-queued";
        let session_arc = Arc::new(tokio::sync::Mutex::new(ChatSession::new(
            chat_id.to_string(),
        )));
        let enqueued_at = std::time::Instant::now();
        {
            let mut session = session_arc.lock().await;
            session.command_queue.push_back(CommandRequest {
                client_request_id: "priority-first".to_string(),
                priority: true,
                command: ChatCommand::Regenerate {},
            });
            session.command_queue.push_back(CommandRequest {
                client_request_id: "normal-last".to_string(),
                priority: false,
                command: ChatCommand::Regenerate {},
            });
            session.command_queue.push_back(CommandRequest {
                client_request_id: "multimodal".to_string(),
                priority: false,
                command: ChatCommand::UserMessage {
                    content: json!([
                        {"type": "text", "text": "describe this"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,image"}}
                    ]),
                    attachments: vec![json!({"name": "diagram.png", "mime": "image/png"})],
                    context_files: vec![
                        json!({"file_name": "notes.md", "file_content": "context"}),
                    ],
                    suppress_auto_enrichment: true,
                    client_message_id: None,
                    push: None,
                },
            });
            session
                .command_enqueued_at
                .insert("multimodal".to_string(), enqueued_at);
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        let response = handle_v1_chat_reprioritize_queued(
            axum::extract::State(app.clone()),
            axum::extract::Path((chat_id.to_string(), "multimodal".to_string())),
            hyper::body::Bytes::from(r#"{"priority":true}"#),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let session = session_arc.lock().await;
        assert_eq!(
            session
                .command_queue
                .iter()
                .map(|request| request.client_request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["priority-first", "multimodal", "normal-last"]
        );
        let request = &session.command_queue[1];
        assert!(request.priority);
        match &request.command {
            ChatCommand::UserMessage {
                content,
                attachments,
                context_files,
                suppress_auto_enrichment,
                client_message_id,
                ..
            } => {
                assert_eq!(content[0], json!({"type": "text", "text": "describe this"}));
                assert_eq!(content[1]["type"], "image_url");
                assert_eq!(
                    content[1]["image_url"]["url"],
                    "data:image/png;base64,image"
                );
                assert_eq!(
                    attachments,
                    &vec![json!({"name": "diagram.png", "mime": "image/png"})]
                );
                assert_eq!(
                    context_files,
                    &vec![json!({"file_name": "notes.md", "file_content": "context"})]
                );
                assert!(*suppress_auto_enrichment);
                assert_eq!(client_message_id, &None);
            }
            command => panic!("expected user message, got {command:?}"),
        }
        assert_eq!(
            session.command_enqueued_at.get("multimodal"),
            Some(&enqueued_at)
        );
        drop(session);

        let response = handle_v1_chat_reprioritize_queued(
            axum::extract::State(app.clone()),
            axum::extract::Path((chat_id.to_string(), "multimodal".to_string())),
            hyper::body::Bytes::from(r#"{"priority":false}"#),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let session = session_arc.lock().await;
        assert_eq!(
            session
                .command_queue
                .iter()
                .map(|request| request.client_request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["priority-first", "normal-last", "multimodal"]
        );
        assert!(!session.command_queue[2].priority);
        assert_eq!(
            session.command_enqueued_at.get("multimodal"),
            Some(&enqueued_at)
        );
        drop(session);

        let response = handle_v1_chat_reprioritize_queued(
            axum::extract::State(app.clone()),
            axum::extract::Path((chat_id.to_string(), "missing".to_string())),
            hyper::body::Bytes::from(r#"{"priority":false}"#),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);

        let unknown_chat_id = "missing-chat".to_string();
        let response = handle_v1_chat_reprioritize_queued(
            axum::extract::State(app.clone()),
            axum::extract::Path((unknown_chat_id.clone(), "missing".to_string())),
            hyper::body::Bytes::from(r#"{"priority":false}"#),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        assert!(!app
            .chat
            .sessions
            .read()
            .await
            .contains_key(&unknown_chat_id));
    }

    #[tokio::test]
    async fn command_rejects_oversized_client_message_id() {
        let workspace = tempfile::tempdir().unwrap();
        let app = test_app_with_workspace(workspace.path()).await;
        let request = CommandRequest {
            client_request_id: "oversized-client-message-id".to_string(),
            priority: false,
            command: ChatCommand::UserMessage {
                content: json!("hello"),
                attachments: vec![],
                context_files: vec![],
                suppress_auto_enrichment: false,
                client_message_id: Some(
                    "a".repeat(refact_chat_api::MAX_CLIENT_MESSAGE_ID_CHARS + 1),
                ),
                push: None,
            },
        };

        let response = handle_v1_chat_command(
            axum::extract::State(app),
            axum::extract::Path("oversized-client-message-id".to_string()),
            hyper::body::Bytes::from(serde_json::to_vec(&request).unwrap()),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn reprioritize_queued_command_rejects_invalid_json() {
        let workspace = tempfile::tempdir().unwrap();
        let app = test_app_with_workspace(workspace.path()).await;

        let error = handle_v1_chat_reprioritize_queued(
            axum::extract::State(app),
            axum::extract::Path(("reprioritize-queued".to_string(), "request".to_string())),
            hyper::body::Bytes::from(r#"{"priority":"now"}"#),
        )
        .await
        .unwrap_err();

        assert_eq!(error.status_code, axum::http::StatusCode::BAD_REQUEST);
        assert!(error.message.starts_with("Invalid JSON:"));
    }
}
