pub use refact_runtime_api::session_cleanup_runtime::{
    clean_background_processes_for_chat, cleanup_idle_session, close_all_chat_sessions,
    close_idle_session_for_cleanup, remove_idle_session_if_safe, snapshot_with_agents,
    start_session_cleanup_task, CleanupRemovalSnapshot,
};

pub use refact_runtime_api::session_runtime::{
    get_or_create_session_with_trajectory, try_restore_session_if_trajectory_exists,
};

#[cfg(test)]
use crate::exec::ExecMode;

#[cfg(test)]
use super::types::*;
#[cfg(test)]
use super::types::session_cleanup_interval;

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::session::create_sessions_map;
    use super::super::types::{
        ChatCommand, CommandRequest, PauseReason, SessionState, TaskMeta, session_idle_timeout,
    };
    use crate::call_validation::{ChatContent, ChatMessage, ChatToolCall, ChatToolFunction};
    use crate::exec::{ExecMode, ExecProcessId, ExecProcessMeta};
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Instant;
    use tokio::sync::Mutex as AMutex;

    fn make_session() -> ChatSession {
        ChatSession::new("test-chat".to_string())
    }

    async fn test_app_with_workspace(
        workspace: &std::path::Path,
    ) -> (crate::app_state::AppState, tempfile::TempDir) {
        let config_dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            workspace.join("cache"),
            config_dir.path().to_path_buf(),
        )
        .await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.to_path_buf()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        (app, config_dir)
    }

    fn background_completion_meta(chat_id: &str) -> ExecProcessMeta {
        ExecProcessMeta::new(ExecMode::Background, "true".to_string())
            .with_process_id(ExecProcessId("exec_cleanup_order".to_string()))
            .with_chat_id(chat_id)
            .with_short_description("cleanup order".to_string())
    }

    #[tokio::test]
    async fn cleanup_saves_before_session_removal() {
        let workspace = tempfile::tempdir().unwrap();
        let (app, _config_dir) = test_app_with_workspace(workspace.path()).await;
        let chat_id = "cleanup-saves-before-removal";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
            session.last_activity =
                Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        assert!(
            cleanup_idle_session(
                app.chat_ctx(),
                &app.chat.sessions,
                chat_id,
                session_arc.clone(),
            )
            .await
        );

        assert!(app.chat.sessions.read().await.get(chat_id).is_none());
        assert!(
            app.chat_ctx()
                .trajectories
                .load_trajectory_for_chat(chat_id)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn close_and_cleanup_retain_required_commit_boundaries() {
        let mut session = make_session();
        session.last_activity =
            Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);

        assert_eq!(
            super::super::trajectories::trajectory_commit_contract_for_test("close"),
            TrajectoryCommitIntent::Required
        );
        assert_eq!(
            super::super::trajectories::trajectory_commit_contract_for_test("graceful shutdown"),
            TrajectoryCommitIntent::Required
        );
        assert!(close_idle_session_for_cleanup(&mut session));
        assert!(session.closed);
    }

    #[tokio::test]
    async fn cleanup_save_failure_keeps_session_mapped_and_open() {
        let workspace = tempfile::tempdir().unwrap();
        let (app, _config_dir) = test_app_with_workspace(workspace.path()).await;
        let chat_id = "cleanup-save-failure-keeps-session";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.thread.task_meta = Some(TaskMeta {
                task_id: "missing-task-for-save-failure".to_string(),
                role: "agents".to_string(),
                agent_id: Some("agent".to_string()),
                card_id: Some("card".to_string()),
                planner_chat_id: None,
            });
            session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
            session.last_activity =
                Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        assert!(
            !cleanup_idle_session(
                app.chat_ctx(),
                &app.chat.sessions,
                chat_id,
                session_arc.clone(),
            )
            .await
        );

        assert!(app.chat.sessions.read().await.get(chat_id).is_some());
        let session = session_arc.lock().await;
        assert!(!session.closed);
        assert!(session.trajectory_dirty);
    }

    #[tokio::test]
    async fn cleanup_keeps_session_when_completion_arrives_during_save() {
        let workspace = tempfile::tempdir().unwrap();
        let (app, _config_dir) = test_app_with_workspace(workspace.path()).await;
        let chat_id = "cleanup-keeps-completion-during-save";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
            session
                .queue_processor_running
                .store(true, Ordering::SeqCst);
            session.last_activity =
                Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        app.chat_ctx()
            .trajectories
            .maybe_save_trajectory_with_intent(session_arc.clone(), TrajectoryCommitIntent::Required)
            .await;
        let process_id = app
            .runtime
            .exec_registry
            .register(background_completion_meta(chat_id), 4096)
            .await
            .meta
            .process_id;
        app.runtime
            .exec_registry
            .mark_started(&process_id)
            .await
            .unwrap();
        let mut completion_rx = app.runtime.exec_registry.subscribe_completion();
        app.runtime
            .exec_registry
            .mark_exited(&process_id, Some(0))
            .await
            .unwrap();
        let completion = completion_rx.recv().await.unwrap();
        crate::chat::notifications::handle_process_completion(app.chat_ctx(), completion).await;

        assert!(
            !cleanup_idle_session(
                app.chat_ctx(),
                &app.chat.sessions,
                chat_id,
                session_arc.clone(),
            )
            .await
        );

        let session = session_arc.lock().await;
        assert!(!session.closed);
        assert!(session.command_queue.is_empty());
        assert_eq!(session.delivery_wake_sources.len(), 1);
        assert!(session.last_activity.elapsed() <= session_idle_timeout());
        session
            .queue_processor_running
            .store(false, Ordering::SeqCst);
        drop(session);
        assert!(app.chat.sessions.read().await.get(chat_id).is_some());
        app.runtime.shutdown_flag.store(true, Ordering::Relaxed);
    }

    #[tokio::test]
    async fn cleanup_persists_pending_deliveries_before_removal() {
        let workspace = tempfile::tempdir().unwrap();
        let (app, _config_dir) = test_app_with_workspace(workspace.path()).await;
        let chat_id = "cleanup-pending-deliveries";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.into())));
        let delivery = PendingDelivery::with_id(
            "cleanup-id",
            vec![ChatMessage::new("user".into(), "queued".into())],
            PushMode::WhenIdle,
            "test",
            false,
        );
        {
            let mut session = session_arc.lock().await;
            session.last_activity =
                Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
            session.pending_deliveries.push_back(delivery.clone());
            session.increment_version();
        }
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.into(), session_arc.clone());
        assert!(cleanup_idle_session(app.chat_ctx(), &app.chat.sessions, chat_id, session_arc).await);
        let loaded = app
            .chat_ctx()
            .trajectories
            .load_trajectory_for_chat(chat_id)
            .await
            .unwrap();
        assert_eq!(loaded.pending_deliveries, vec![delivery.clone()]);
        let restored =
            get_or_create_session_with_trajectory(app.chat_ctx(), &app.chat.sessions, chat_id)
                .await;
        assert_eq!(
            restored.lock().await.pending_deliveries_for_snapshot(),
            vec![delivery]
        );
    }

    #[test]
    fn cleanup_rechecks_pending_deliveries() {
        let mut session = make_session();
        session.last_activity =
            Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
        assert!(close_idle_session_for_cleanup(&mut session));
        let snapshot = CleanupRemovalSnapshot::capture(&session);
        let delivery = PendingDelivery::new(
            vec![ChatMessage::new("user".into(), "queued".into())],
            PushMode::WhenIdle,
            "test",
            false,
        );
        session.pending_deliveries.push_back(delivery.clone());
        assert!(!snapshot.still_matches(&session));
        session.pending_deliveries.clear();
        session.runner_pending_deliveries.push(delivery);
        assert!(!snapshot.still_matches(&session));
    }

    #[test]
    fn cleanup_close_helper_rechecks_idle_state() {
        let mut session = make_session();
        session.last_activity =
            Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
        assert!(close_idle_session_for_cleanup(&mut session));
        assert!(session.closed);

        let mut already_closed = make_session();
        already_closed.close_event_channel();
        already_closed.last_activity =
            Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
        assert!(close_idle_session_for_cleanup(&mut already_closed));
        assert!(already_closed.closed);

        let mut touched = make_session();
        touched.last_activity = Instant::now();
        assert!(!close_idle_session_for_cleanup(&mut touched));
        assert!(!touched.closed);
    }

    #[tokio::test]
    async fn cleanup_final_removal_recheck_keeps_queued_command() {
        let sessions = create_sessions_map();
        let chat_id = "cleanup-final-recheck-queued";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
            session.last_activity =
                Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
        }
        sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());

        {
            let mut session = session_arc.lock().await;
            session.enqueue_priority_command(CommandRequest {
                client_request_id: "queued-before-removal".to_string(),
                priority: false,
                command: ChatCommand::Regenerate {},
            });
        }

        assert!(!remove_idle_session_if_safe(&sessions, chat_id, session_arc.clone(),).await);
        assert!(sessions.read().await.get(chat_id).is_some());
        let session = session_arc.lock().await;
        assert!(session
            .command_queue
            .iter()
            .any(|request| request.client_request_id == "queued-before-removal"));
    }

    #[tokio::test]
    async fn cleanup_final_removal_recheck_keeps_outstanding_session_ref() {
        let sessions = create_sessions_map();
        let chat_id = "cleanup-final-recheck-ref";
        let session_arc = Arc::new(AMutex::new(ChatSession::new(chat_id.to_string())));
        {
            let mut session = session_arc.lock().await;
            session.add_message(ChatMessage::new("user".to_string(), "hello".to_string()));
            session.last_activity =
                Instant::now() - session_idle_timeout() - std::time::Duration::from_secs(1);
        }
        sessions
            .write()
            .await
            .insert(chat_id.to_string(), session_arc.clone());
        let delayed_enqueue_ref = session_arc.clone();

        assert!(!remove_idle_session_if_safe(&sessions, chat_id, session_arc.clone(),).await);

        {
            let mut session = delayed_enqueue_ref.lock().await;
            session.enqueue_priority_command(CommandRequest {
                client_request_id: "queued-from-outstanding-ref".to_string(),
                priority: false,
                command: ChatCommand::Regenerate {},
            });
        }

        let session = sessions.read().await.get(chat_id).cloned().unwrap();
        let session = session.lock().await;
        assert!(session
            .command_queue
            .iter()
            .any(|request| request.client_request_id == "queued-from-outstanding-ref"));
    }

    fn make_assistant_with_tool_calls(ids: &[&str]) -> ChatMessage {
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

    fn default_openai_settings() -> crate::llm::adapter::AdapterSettings {
        crate::llm::adapter::AdapterSettings {
            api_key: "test-key".to_string(),
            auth_token: String::new(),
            endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
            extra_headers: Default::default(),
            model_name: "gpt-4.1".to_string(),
            supports_tools: true,
            supports_reasoning: true,
            reasoning_type: None,
            supports_temperature: true,
            supports_max_completion_tokens: false,
            eof_is_done: false,
            supports_web_search: false,
            supports_cache_control: false,
        }
    }

    fn assert_openai_tool_results_follow_assistant(messages: Vec<ChatMessage>) {
        use crate::llm::adapter::LlmWireAdapter;
        use crate::llm::adapters::openai_chat::OpenAiChatAdapter;

        let req = crate::llm::canonical::LlmRequest::new("gpt-4.1".to_string(), messages);
        let body = OpenAiChatAdapter
            .build_http(
                &refact_privacy::testing::cleared(req),
                &default_openai_settings(),
            )
            .unwrap()
            .body;
        let wire_messages = body["messages"].as_array().unwrap();
        for (idx, message) in wire_messages.iter().enumerate() {
            if message["role"] != "assistant" || message.get("tool_calls").is_none() {
                continue;
            }
            let tool_calls = message["tool_calls"].as_array().unwrap();
            let expected_ids: HashSet<String> = tool_calls
                .iter()
                .filter_map(|tool_call| tool_call["id"].as_str().map(str::to_string))
                .collect();
            let actual_ids: HashSet<String> = wire_messages
                .iter()
                .skip(idx + 1)
                .take(tool_calls.len())
                .map(|tool_result| {
                    assert_eq!(
                        tool_result["role"], "tool",
                        "wire messages: {wire_messages:?}"
                    );
                    tool_result["tool_call_id"].as_str().unwrap().to_string()
                })
                .collect();
            assert_eq!(actual_ids, expected_ids, "wire messages: {wire_messages:?}");
        }
    }

    #[test]
    fn openai_wire_order_valid_after_tool_decision() {
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
        session.add_message(make_tool_result("tc2", "accepted result"));
        session.drain_post_tool_side_effects();

        assert_openai_tool_results_follow_assistant(session.messages.clone());
    }

    #[test]
    fn openai_wire_order_valid_after_multi_ide_callbacks() {
        let mut session = make_session();
        session.add_message(make_assistant_with_tool_calls(&["tc1", "tc2"]));
        session.runtime.state = SessionState::WaitingIde;

        session.record_ide_tool_result("tc1".to_string(), "first".to_string(), false);
        session.record_ide_tool_result("tc2".to_string(), "second".to_string(), false);

        assert_openai_tool_results_follow_assistant(session.messages.clone());
    }
}
