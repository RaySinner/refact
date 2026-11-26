#[cfg(test)]
mod progress_collector_tests {
    use crate::subchat::SubchatProgressCollector;
    use serde_json::json;

    #[test]
    fn test_extract_thinking_preview_skips_non_objects() {
        let blocks = vec![json!({"thinking": "hello"}), json!(123)];
        let preview = SubchatProgressCollector::extract_thinking_preview(&blocks);
        assert_eq!(preview.as_deref(), Some("hello"));
    }

    #[test]
    fn test_append_tail_unicode_no_panic() {
        let mut s = String::new();
        // Force truncation in the middle of multibyte chars.
        for _ in 0..50 {
            SubchatProgressCollector::append_tail(&mut s, "вњ…", 10);
        }
        assert!(s.is_char_boundary(s.len()));
    }

    #[test]
    fn tool_arg_preview_uses_first_safe_scalar_and_sanitizes_whitespace() {
        assert_eq!(
            crate::subchat::tool_arg_preview(r#"{"token":"hidden","path":"a\n b"}"#),
            Some("a b".to_string())
        );
        assert_eq!(crate::subchat::tool_arg_preview(r#"{"password":"hidden"}"#), None);
    }
}

#[cfg(test)]
mod convert_results_tests {
    use crate::call_validation::ChatMessage;
    use crate::chat::stream_core::ChoiceFinal;
    use crate::subchat::convert_results_to_messages;

    #[test]
    fn empty_results_error_instead_of_masking_failure() {
        let messages = vec![ChatMessage::new(
            "user".to_string(),
            "review this".to_string(),
        )];
        let result = convert_results_to_messages(vec![], messages);
        assert!(result.is_err());
    }

    #[test]
    fn empty_content_without_tool_calls_errors() {
        let messages = vec![ChatMessage::new(
            "user".to_string(),
            "review this".to_string(),
        )];
        let choice = ChoiceFinal {
            content: "  ".to_string(),
            finish_reason: Some("length".to_string()),
            ..Default::default()
        };
        let error = convert_results_to_messages(vec![choice], messages).unwrap_err();
        assert!(error.contains("no visible content"));
        assert!(error.contains("length"));
    }

    #[test]
    fn empty_choice_is_skipped_when_another_has_content() {
        let messages = vec![ChatMessage::new(
            "user".to_string(),
            "review this".to_string(),
        )];
        let empty = ChoiceFinal {
            content: String::new(),
            ..Default::default()
        };
        let full = ChoiceFinal {
            content: "all good".to_string(),
            ..Default::default()
        };
        let result = convert_results_to_messages(vec![empty, full], messages).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0][1].content.content_text_only(), "all good");
    }

    #[test]
    fn choices_extend_original_messages() {
        let messages = vec![ChatMessage::new(
            "user".to_string(),
            "review this".to_string(),
        )];
        let choice = ChoiceFinal {
            content: "all good".to_string(),
            ..Default::default()
        };

        let result = convert_results_to_messages(vec![choice], messages).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 2);
        assert_eq!(result[0][0].role, "user");
        assert_eq!(result[0][1].role, "assistant");
        assert_eq!(result[0][1].content.content_text_only(), "all good");
    }
}

#[cfg(test)]
mod subchat_tests {
    use crate::subchat::{
        apply_subchat_reactive_compaction, apply_subchat_report_policy,
        emit_parent_compaction_diagnostics, gate_subchat_boundary,
        parent_compaction_diagnostic_status, parent_thread_worktree, parse_subchat_cache_control,
        partial_output_stream_error_message, prepare_subchat_messages,
        register_stateful_subchat_worktree, resolve_subchat_config_with_parent,
        resolve_subagent_confirmation_defaults, resolve_subchat_model, resolve_subchat_params,
        resolve_subchat_worktree, safe_context_limit_error_for_log, stable_stream_chat_id,
        stable_subchat_chat_id, should_compact_context_limit_error,
        should_persist_subchat_trajectory, stateful_thread_from_config, subchat_retries_allowed,
        subchat_trajectory_commit_intent, trace_thread_from_config, save_failed_subchat_trajectory,
        SubchatConfig, SubchatProgress, SubchatProgressCollector, SubchatTrajectoryCommitPhase,
        ToolsPolicy, TraceParent, GUARDED_REPORT_INSTRUCTION,
        PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS,
        PARENT_COMPACTION_DIAGNOSTIC_REDACTION_LOOKAHEAD_CHARS,
        PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED, PARTIAL_OUTPUT_STREAM_ERROR,
    };
    use crate::subchat::{final_step_wrap_up_message, needs_forced_final_answer};
    use refact_runtime_api::GlobalContextCtx;
    use crate::chat::diagnostics::{
        SAFE_PROVIDER_ERROR_DIAGNOSTIC_MAX_CHARS, SAFE_PROVIDER_ERROR_DIAGNOSTIC_TRUNCATED,
    };

    use crate::chat::trajectory_ops::sanitize_messages_for_new_thread;
    use crate::chat::trajectories::save_trajectory_as_with_intent;
    use crate::call_validation::{
        ChatContent, ChatMessage, ChatModelType, ReasoningEffort, SubchatParameters,
    };
    use crate::chat::types::{TaskMeta, ThreadParams, TrajectoryCommitIntent};
    use crate::caps::{BaseModelRecord, ChatModelRecord, CodeAssistantCaps};
    use crate::global_context::tests::make_test_gcx;
    use crate::llm::params::CacheControl;
    use crate::worktrees::types::WorktreeMeta;
    use crate::yaml_configs::project_configs_bootstrap::global_configs_try_create_all;
    use refact_privacy::{
        Attribution, FileRecord, PolicyLoad, PrivacyPolicy, PrivacyRecord, ShellBehavior,
        SubagentPolicy, Zone,
    };
    use crate::chat::stream_core::StreamCollector;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn ctx_of(gcx: &Arc<crate::global_context::GlobalContext>) -> crate::chat::ctx::ChatCtx {
        gcx.chat_ctx(gcx.clone())
    }

    fn chat_model_record(id: &str, n_ctx: usize, endpoint: &str) -> Arc<ChatModelRecord> {
        Arc::new(ChatModelRecord {
            base: BaseModelRecord {
                id: id.to_string(),
                name: id.to_string(),
                n_ctx,
                endpoint: endpoint.to_string(),
                ..Default::default()
            },
            max_output_tokens: Some(128_000),
            ..Default::default()
        })
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(root: &Path) {
        run_git(root, &["init"]);
        run_git(root, &["checkout", "-b", "main"]);
        run_git(root, &["config", "core.autocrlf", "false"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        fs::write(root.join("file.txt"), "hello\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    #[tokio::test]
    async fn runner_preempt_wait_observes_queued_a_without_aborting_runner() {
        let ccx = abort_test_ccx().await;
        let app = ccx.lock().await.app.clone();
        let request = crate::agents::types::CreateAgentRequest {
            parent_chat_id: "parent".into(),
            parent_root_chat_id: None,
            parent_tool_call_id: None,
            kind: crate::agents::types::BgAgentKind::Subagent,
            config_name: "subagent".into(),
            title: "delivery test".into(),
            prompt: "test".into(),
            target_files: vec![],
            model: "test".into(),
            model_type: None,
            goal_summary: None,
            plan_present: false,
            worktree_id: None,
            worktree_branch: None,
        };
        let (agent, _, _) = app.agents.create(request).await.unwrap();
        ccx.lock().await.background_agent_id = Some(agent.agent_id.clone());
        for push in [
            refact_chat_api::PushMode::WhenIdle,
            refact_chat_api::PushMode::Append,
            refact_chat_api::PushMode::Preempt,
        ] {
            app.agents
                .enqueue_delivery(
                    &agent.agent_id,
                    refact_chat_api::PendingDelivery::new(
                        vec![ChatMessage::new("user".into(), push.as_str().into())],
                        push,
                        "test",
                        true,
                    ),
                )
                .await
                .unwrap();
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::subchat::wait_for_runner_preempt(&app, &agent.agent_id),
        )
        .await
        .unwrap();
        assert!(!ccx
            .lock()
            .await
            .abort_flag
            .load(std::sync::atomic::Ordering::SeqCst));
        let mut messages = vec![];
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        let pending = app.agents.pending_deliveries(&agent.agent_id).await;
        assert_eq!(pending.len(), 3); // Claimed A/B remain durable until trajectory ack.
        assert_eq!(pending[0].push, refact_chat_api::PushMode::WhenIdle);
        assert!(!app.agents.has_preempt(&agent.agent_id).await);
        assert_eq!(messages.len(), 3);
        let mut config = test_subchat_config();
        config.stateful = true;
        config.background_agent_id = Some(agent.agent_id.clone());
        let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
        let gcx = ccx.lock().await.app.gcx.clone();
        assert_eq!(
            crate::chat::trajectories::get_trajectories_dir(&gcx.chat_ctx(gcx.clone()))
                .await
                .unwrap_err(),
            "No workspace folder found"
        );
        crate::subchat::persist_subchat_progress(&ccx, &config, &progress, &messages).await;
        assert_eq!(
            app.agents.pending_deliveries(&agent.agent_id).await.len(),
            3
        );
        let workspace = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        crate::subchat::persist_subchat_progress(&ccx, &config, &progress, &messages).await;
        let pending = app.agents.pending_deliveries(&agent.agent_id).await;
        assert_eq!(
            pending.len(),
            1,
            "required trajectory commit acknowledges A/B only"
        );
        assert_eq!(pending[0].push, refact_chat_api::PushMode::WhenIdle);
        let (gcx, chat_id) = {
            let context = ccx.lock().await;
            (context.app.gcx.clone(), context.chat_id.clone())
        };
        let saved = crate::chat::trajectories::load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), &chat_id)
            .await
            .unwrap();
        assert_eq!(saved.messages.len(), messages.len());
    }

    #[tokio::test]
    async fn runner_wait_boundary_releases_idle_after_all_results_and_local_ticks() {
        use refact_chat_api::{PendingDelivery, PushMode};
        let ccx = abort_test_ccx().await;
        let (app, chat_id) = {
            let ccx = ccx.lock().await;
            (ccx.app.clone(), ccx.chat_id.clone())
        };
        let (agent, _, _) = app
            .agents
            .create(crate::agents::types::CreateAgentRequest {
                parent_chat_id: "parent".into(),
                parent_root_chat_id: None,
                parent_tool_call_id: None,
                kind: crate::agents::types::BgAgentKind::Subagent,
                config_name: "subagent".into(),
                title: "wait boundary test".into(),
                prompt: "test".into(),
                target_files: vec![],
                model: "test".into(),
                model_type: None,
                goal_summary: None,
                plan_present: false,
                worktree_id: None,
                worktree_branch: None,
            })
            .await
            .unwrap();
        ccx.lock().await.background_agent_id = Some(agent.agent_id.clone());
        let delivery = PendingDelivery::with_id(
            "runner-idle",
            vec![ChatMessage::new("user".into(), "continue".into())],
            PushMode::WhenIdle,
            "parent",
            true,
        );
        app.agents
            .enqueue_delivery(&agent.agent_id, delivery.clone())
            .await
            .unwrap();
        let mut session = crate::chat::types::ChatSession::new(chat_id.clone());
        session.set_runner_pending_deliveries(vec![delivery]);
        let session = Arc::new(tokio::sync::Mutex::new(session));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id, session.clone());
        let mut messages = vec![];
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert!(messages.is_empty(), "ordinary runner boundaries retain C");

        let assistant: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": "", "tool_calls": [
                {"id":"sleep", "type":"function", "function":{"name":"sleep", "arguments":"{}"}},
                {"id":"other", "type":"function", "function":{"name":"sleep", "arguments":"{}"}}
            ]
        }))
        .unwrap();
        messages.push(assistant);
        {
            let mut session = session.lock().await;
            session.messages = messages.clone();
            session.turn_depth = 1;
            session.interruptible_waits = 1;
            session.interrupt_wait_for_delivery(
                crate::tools::tools_description::WaitInterruptCause::MessageArrived,
            );
            assert!(session.wait_delivery_boundary);
            assert!(!session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
            session.interruptible_waits = 0;
            session
                .queue_post_tool_delivery(PendingDelivery::with_id(
                    "local-tick",
                    vec![ChatMessage::new("event".into(), "tick".into())],
                    PushMode::WhenIdle,
                    "sleep",
                    false,
                ))
                .unwrap();
        }
        for id in ["sleep", "other"] {
            crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
            assert!(session.lock().await.wait_delivery_boundary);
            assert!(!messages.iter().any(|message| message.role == "user"));
            messages.push(ChatMessage {
                role: "tool".into(),
                tool_call_id: id.into(),
                content: ChatContent::SimpleText("yielded".into()),
                ..Default::default()
            });
        }
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert!(!session.lock().await.wait_delivery_boundary);
        assert!(session.lock().await.pending_deliveries.is_empty());
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[3].extra["delivery"]["id"], "runner-idle");
        assert_eq!(messages[4].extra["delivery"]["id"], "local-tick");
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(
            messages.len(),
            5,
            "claimed deliveries must not be duplicated"
        );
        app.agents
            .enqueue_delivery(
                &agent.agent_id,
                PendingDelivery::with_id(
                    "later-idle",
                    vec![ChatMessage::new("user".into(), "later".into())],
                    PushMode::WhenIdle,
                    "parent",
                    true,
                ),
            )
            .await
            .unwrap();
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(messages.len(), 5, "wait boundary is consumed once");

        session.lock().await.wait_delivery_boundary = true;
        ccx.lock().await.background_agent_id = Some("missing-agent".into());
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert!(
            session.lock().await.wait_delivery_boundary,
            "failed drains retain the boundary"
        );
    }

    #[tokio::test]
    async fn runner_local_sleep_deliveries_follow_result_and_survive_ack() {
        use refact_chat_api::{PendingDelivery, PushMode};
        let ccx = abort_test_ccx().await;
        let app = ccx.lock().await.app.clone();
        let request = crate::agents::types::CreateAgentRequest {
            parent_chat_id: "parent".into(),
            parent_root_chat_id: None,
            parent_tool_call_id: None,
            kind: crate::agents::types::BgAgentKind::Subagent,
            config_name: "subagent".into(),
            title: "delivery test".into(),
            prompt: "test".into(),
            target_files: vec![],
            model: "test".into(),
            model_type: None,
            goal_summary: None,
            plan_present: false,
            worktree_id: None,
            worktree_branch: None,
        };
        let (agent, _, _) = app.agents.create(request).await.unwrap();
        ccx.lock().await.background_agent_id = Some(agent.agent_id.clone());
        let chat_id = ccx.lock().await.chat_id.clone();
        let assistant: ChatMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": "", "tool_calls": [
                {"id":"sleep", "type":"function", "function":{"name":"sleep", "arguments":"{}"}}
            ]
        }))
        .unwrap();
        let mut messages = vec![assistant.clone()];
        let mut session = crate::chat::types::ChatSession::new(chat_id.clone());
        session.messages = messages.clone();
        session.turn_depth = 1;
        let mut originals = Vec::new();
        for push in [PushMode::Preempt, PushMode::Append, PushMode::WhenIdle] {
            let delivery = PendingDelivery::with_id(
                push.as_str(),
                vec![ChatMessage::new("event".into(), "sleep tick".into())],
                push,
                "sleep",
                false,
            );
            assert_eq!(
                session.queue_post_tool_delivery(delivery).unwrap(),
                refact_chat_api::DeliveryOutcome::Queued
            );
            if push == PushMode::Append {
                session
                    .pending_deliveries
                    .back_mut()
                    .unwrap()
                    .after_tool_call_id = Some(String::new());
            }
            originals.push(session.pending_deliveries.back().unwrap().clone());
        }
        let serialized = serde_json::to_value(session.pending_deliveries_for_snapshot()).unwrap();
        session.pending_deliveries.clear();
        session.restore_pending_deliveries(serde_json::from_value(serialized).unwrap());
        session.set_runner_pending_deliveries(vec![PendingDelivery::with_id(
            "mirror-only",
            vec![ChatMessage::new("event".into(), "not input".into())],
            PushMode::Append,
            "display",
            false,
        )]);
        let session = Arc::new(tokio::sync::Mutex::new(session));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.clone(), session.clone());
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(messages.len(), 1);
        assert!(!app.agents.has_preempt(&agent.agent_id).await);
        assert_eq!(session.lock().await.pending_deliveries.len(), 3);
        messages.push(ChatMessage {
            role: "tool".into(),
            tool_call_id: "sleep".into(),
            content: ChatContent::SimpleText("slept".into()),
            ..Default::default()
        });
        crate::subchat::import_runner_local_deliveries(&app, "missing-agent", &chat_id, &messages).await;
        assert_eq!(session.lock().await.pending_deliveries.len(), 3);
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert!(session.lock().await.pending_deliveries.is_empty());
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[1].role, "tool");
        assert_eq!(messages.len(), 5);
        assert_eq!(
            messages[3].extra["delivery"]["id"],
            PushMode::Preempt.as_str()
        );
        assert_eq!(
            messages[4].extra["delivery"]["id"],
            PushMode::Append.as_str()
        );
        assert_eq!(
            app.agents.pending_deliveries(&agent.agent_id).await.len(),
            3
        );
        session
            .lock()
            .await
            .pending_deliveries
            .extend(originals.clone());
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(messages.len(), 5);
        let mut config = test_subchat_config();
        config.stateful = true;
        config.background_agent_id = Some(agent.agent_id.clone());
        let workspace = tempfile::tempdir().unwrap();
        let gcx = ccx.lock().await.app.gcx.clone();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let progress = Arc::new(StdMutex::new(Vec::new()));
        crate::subchat::persist_subchat_progress(&ccx, &config, &progress, &messages).await;
        assert_eq!(
            app.agents.pending_deliveries(&agent.agent_id).await.len(),
            1
        );
        session.lock().await.pending_deliveries.extend(originals);
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(messages.len(), 5);
        messages.push(ChatMessage::new("assistant".into(), "final".into()));
        let deliveries = app
            .agents
            .finish_delivery_turn(&agent.agent_id)
            .await
            .unwrap();
        assert!(!crate::subchat::append_runner_deliveries(&mut messages, deliveries));
        assert_eq!(messages[5].content.content_text_only(), "final");
        assert_eq!(
            messages[6].extra["delivery"]["id"],
            PushMode::WhenIdle.as_str()
        );
        for (index, message) in messages.iter_mut().enumerate() {
            if message.message_id.is_empty() {
                message.message_id = format!("runner-sleep-message-{index}");
            }
            if let Some(calls) = message.tool_calls.as_mut() {
                for (call_index, call) in calls.iter_mut().enumerate() {
                    call.index = Some(call_index);
                }
            }
        }
        crate::subchat::persist_subchat_progress(&ccx, &config, &progress, &messages).await;
        assert!(app
            .agents
            .pending_deliveries(&agent.agent_id)
            .await
            .is_empty());
        let gcx = ccx.lock().await.app.gcx.clone();
        let saved = crate::chat::trajectories::load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), &chat_id)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(saved.messages).unwrap(),
            serde_json::to_value(messages).unwrap()
        );
    }

    #[test]
    fn runner_delivery_preserves_prefix_and_puts_cancellation_before_preempt() {
        let original = ChatMessage::new("assistant".into(), "completed answer".into());
        let mut messages = vec![original.clone()];
        let delivery = refact_chat_api::PendingDelivery::with_id(
            "urgent",
            vec![ChatMessage::new("user".into(), "new instruction".into())],
            refact_chat_api::PushMode::Preempt,
            "user",
            true,
        );
        assert!(crate::subchat::append_runner_deliveries(
            &mut messages,
            vec![delivery]
        ));
        assert_eq!(
            serde_json::to_value(&messages[0]).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(messages[1].extra["event"]["source"], "runner");
        assert_eq!(messages[1].extra["event"]["subkind"], "cancellation_note");
        assert_eq!(messages[1].extra["event"]["payload"]["source"], "user");
        assert_eq!(messages[2].extra["delivery"]["id"], "urgent");
        assert!(!crate::subchat::has_final_answer(&messages));
    }

    #[test]
    fn runner_idle_delivery_wakes_only_when_requested() {
        for wake in [false, true] {
            let mut messages = vec![ChatMessage::new("assistant".into(), "done".into())];
            let delivery = refact_chat_api::PendingDelivery::with_id(
                "idle",
                vec![ChatMessage::new("event".into(), "notice".into())],
                refact_chat_api::PushMode::WhenIdle,
                "test",
                wake,
            );
            assert_eq!(
                crate::subchat::append_runner_deliveries(&mut messages, vec![delivery]),
                wake
            );
            assert_eq!(messages.len(), 2);
            assert!(!crate::subchat::has_final_answer(&messages));
        }
    }

    #[test]
    fn runner_boundary_requires_every_parallel_tool_result() {
        let mut assistant = ChatMessage::new("assistant".into(), String::new());
        assistant.tool_calls = Some(vec![
            serde_json::from_value(serde_json::json!({"id":"one", "type":"function", "function":{"name":"cat", "arguments":"{}"}})).unwrap(),
            serde_json::from_value(serde_json::json!({"id":"two", "type":"function", "function":{"name":"cat", "arguments":"{}"}})).unwrap(),
        ]);
        let mut messages = vec![assistant];
        assert!(!crate::subchat::runner_tool_window_closed(&messages));
        for id in ["one", "two"] {
            let mut result = ChatMessage::new("tool".into(), "result".into());
            result.tool_call_id = id.into();
            messages.push(result);
            assert_eq!(crate::subchat::runner_tool_window_closed(&messages), id == "two");
        }
    }

    #[test]
    fn runner_boundary_accepts_diff_results_for_edit_tools() {
        let mut assistant = ChatMessage::new("assistant".into(), String::new());
        assistant.tool_calls = Some(vec![serde_json::from_value(serde_json::json!({
            "id":"edit", "type":"function", "function":{"name":"update_textdoc", "arguments":"{}"}
        }))
        .unwrap()]);
        let mut diff = ChatMessage::new("diff".into(), "[]".into());
        diff.tool_call_id = "edit".into();
        let messages = vec![assistant, diff];
        assert!(crate::subchat::runner_tool_window_closed(&messages));
    }

    #[test]
    fn needs_forced_final_answer_gating() {
        assert!(needs_forced_final_answer(true, false, false));
        assert!(!needs_forced_final_answer(false, false, false));
        assert!(!needs_forced_final_answer(true, true, false));
        assert!(!needs_forced_final_answer(true, false, true));
    }

    #[test]
    fn final_step_wrap_up_message_is_tool_free_user_instruction() {
        let message = final_step_wrap_up_message(50);
        assert_eq!(message.role, "user");
        let text = message.content.content_text_only();
        assert!(text.contains("50/50"));
        assert!(text.contains("Do not call any tools"));
        assert!(text.contains("never return an empty result"));
    }

    fn test_subchat_config() -> SubchatConfig {
        SubchatConfig {
            tool_name: "subagent".to_string(),
            stateful: false,
            autonomous_no_confirm: false,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            chat_id: None,
            title: None,
            parent_id: None,
            link_type: None,
            root_chat_id: None,
            tools: ToolsPolicy::None,
            max_steps: 1,
            prepend_system_prompt: false,
            wrap_up: None,
            task_meta: None,
            worktree: None,
            model: "model".to_string(),
            mode: "agent".to_string(),
            n_ctx: 4096,
            max_new_tokens: 512,
            temperature: None,
            reasoning_effort: None,
            cache_control: crate::llm::params::CacheControl::Ephemeral,
            parent_tool_call_id: None,
            parent_subchat_tx: None,
            abort_flag: None,
            soft_abort: false,
            activity_stamp: None,
            background_agent_id: None,
            subchat_depth: 1,
            final_step_force_answer: false,
            buddy_meta: None,
            step_progress: None,
            trace_parent: TraceParent::unattributed(),
        }
    }

    async fn abort_test_ccx(
    ) -> Arc<tokio::sync::Mutex<crate::at_commands::at_commands::AtCommandsContext>> {
        let gcx = make_test_gcx().await;
        Arc::new(tokio::sync::Mutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                crate::app_state::app_state_from_gcx(gcx).await,
                4096,
                1,
                false,
                vec![],
                "subchat-abort-test".to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ))
    }

    async fn runner_missing_goal_fixture(
        push: refact_chat_api::PushMode,
    ) -> (
        Arc<tokio::sync::Mutex<crate::at_commands::at_commands::AtCommandsContext>>,
        String,
        Arc<tokio::sync::Mutex<crate::chat::types::ChatSession>>,
        refact_chat_api::PendingDelivery,
        refact_chat_api::PendingDelivery,
    ) {
        use refact_chat_api::PendingDelivery;
        let ccx = abort_test_ccx().await;
        let (app, chat_id) = {
            let ccx = ccx.lock().await;
            (ccx.app.clone(), ccx.chat_id.clone())
        };
        let (agent, _, _) = app
            .agents
            .create(crate::agents::types::CreateAgentRequest {
                parent_chat_id: "parent".into(),
                parent_root_chat_id: None,
                parent_tool_call_id: None,
                kind: crate::agents::types::BgAgentKind::Subagent,
                config_name: "subagent".into(),
                title: "missing goal delivery test".into(),
                prompt: "test".into(),
                target_files: vec![],
                model: "test".into(),
                model_type: None,
                goal_summary: None,
                plan_present: false,
                worktree_id: None,
                worktree_branch: None,
            })
            .await
            .unwrap();
        app.agents
            .mark_running(&agent.agent_id, chat_id.clone())
            .await
            .unwrap();
        ccx.lock().await.background_agent_id = Some(agent.agent_id.clone());
        let session = Arc::new(tokio::sync::Mutex::new(
            crate::chat::types::ChatSession::new(chat_id.clone()),
        ));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id, session.clone());
        let stale = PendingDelivery::with_id(
            "missing-goal",
            vec![crate::chat::internal_roles::event(
                crate::chat::internal_roles::EventSubkind::SystemNotice,
                "buddy.supervision",
                serde_json::json!({"trigger": "missing_goal"}),
                "set a goal".into(),
            )],
            push,
            "buddy.supervision",
            false,
        );
        let unrelated = PendingDelivery::with_id(
            "unrelated",
            vec![crate::chat::internal_roles::event(
                crate::chat::internal_roles::EventSubkind::SystemNotice,
                "buddy.supervision",
                serde_json::json!({"trigger": "off_goal"}),
                "keep me".into(),
            )],
            push,
            "buddy.supervision",
            true,
        );
        for delivery in [stale.clone(), unrelated.clone()] {
            app.agents
                .enqueue_delivery(&agent.agent_id, delivery)
                .await
                .unwrap();
        }
        (ccx, agent.agent_id, session, stale, unrelated)
    }

    #[tokio::test]
    async fn runner_missing_goal_is_revalidated_at_every_live_drain() {
        use refact_chat_api::PushMode;

        let (ccx, agent_id, session, stale, unrelated) =
            runner_missing_goal_fixture(PushMode::WhenIdle).await;
        let app = ccx.lock().await.app.clone();
        {
            let mut session = session.lock().await;
            session.install_goal(
                "agent",
                "ship it",
                false,
                refact_chat_api::GoalBudget::default(),
            );
            session.goal_set_status(refact_chat_api::GoalStatus::Stopped);
            session.begin_interruptible_wait();
            session.set_runner_pending_deliveries(vec![stale.clone()]);
        }
        assert!(
            session
                .lock()
                .await
                .runner_pending_deliveries
                .iter()
                .any(|delivery| delivery.id == stale.id)
        );
        assert!(!session
            .lock()
            .await
            .wait_interrupt_flag
            .load(Ordering::SeqCst));
        crate::agents::delivery::deliver_to_agent(app.clone(), &agent_id, stale.clone())
            .await
            .unwrap();
        assert!(
            session.lock().await.wait_delivery_boundary,
            "the unrelated delivery must retain its wait-opening behavior"
        );
        let mut messages = Vec::new();
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].extra["delivery"]["id"], unrelated.id);
        assert_eq!(
            app.agents
                .pending_deliveries(&agent_id)
                .await
                .iter()
                .map(|delivery| delivery.id.as_str())
                .collect::<Vec<_>>(),
            vec![unrelated.id.as_str()]
        );
        assert!(app.agents.get_any(&agent_id).await.unwrap().delivery_ids.is_empty());
        session.lock().await.end_interruptible_wait();
        session.lock().await.begin_interruptible_wait();
        let mut advisor = stale.clone();
        advisor.push = PushMode::Append;
        assert_eq!(
            crate::agents::delivery::deliver_to_agent(app.clone(), &agent_id, advisor)
                .await
                .unwrap(),
            refact_chat_api::DeliveryOutcome::Queued
        );
        assert!(!session
            .lock()
            .await
            .wait_interrupt_flag
            .load(Ordering::SeqCst));
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(
            app.agents
                .pending_deliveries(&agent_id)
                .await
                .iter()
                .map(|delivery| delivery.id.as_str())
                .collect::<Vec<_>>(),
            vec![unrelated.id.as_str()]
        );
        session.lock().await.end_interruptible_wait();
        session.lock().await.set_goal_projection(None);
        let mut future = stale.clone();
        future.push = PushMode::Append;
        crate::agents::delivery::deliver_to_agent(app.clone(), &agent_id, future)
            .await
            .unwrap();
        crate::subchat::drain_background_agent_inbox(&ccx, &mut messages).await;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].extra["delivery"]["id"], stale.id);

        let (_ccx, agent_id, session, stale, unrelated) =
            runner_missing_goal_fixture(PushMode::WhenIdle).await;
        let app = _ccx.lock().await.app.clone();
        session.lock().await.install_goal(
            "agent",
            "ship it",
            true,
            refact_chat_api::GoalBudget::default(),
        );
        let goal_session = session.lock().await;
        let deliveries = crate::subchat::drain_runner_deliveries(
            &app,
            &agent_id,
            Some(&*goal_session),
            true,
            true,
        )
        .await
        .unwrap();
        drop(goal_session);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].id, unrelated.id);
        let mut messages = Vec::new();
        assert!(crate::subchat::append_runner_deliveries(&mut messages, deliveries));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].extra["delivery"]["id"], unrelated.id);
        assert_eq!(
            app.agents
                .pending_deliveries(&agent_id)
                .await
                .iter()
                .map(|delivery| delivery.id.as_str())
                .collect::<Vec<_>>(),
            vec![unrelated.id.as_str()]
        );
        session.lock().await.set_goal_projection(None);
        crate::agents::delivery::deliver_to_agent(app.clone(), &agent_id, stale.clone())
            .await
            .unwrap();
        let goal_session = session.lock().await;
        let deliveries = crate::subchat::drain_runner_deliveries(
            &app,
            &agent_id,
            Some(&*goal_session),
            true,
            true,
        )
        .await
        .unwrap();
        drop(goal_session);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].id, stale.id);
        assert!(!crate::subchat::append_runner_deliveries(&mut messages, deliveries));

        let (_ccx, agent_id, session, stale, unrelated) =
            runner_missing_goal_fixture(PushMode::Preempt).await;
        let app = _ccx.lock().await.app.clone();
        session.lock().await.install_goal(
            "agent",
            "ship it",
            true,
            refact_chat_api::GoalBudget::default(),
        );
        let goal_session = session.lock().await;
        let deliveries = crate::subchat::drain_runner_deliveries(
            &app,
            &agent_id,
            Some(&*goal_session),
            false,
            false,
        )
        .await
        .unwrap();
        drop(goal_session);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].id, unrelated.id);
        let mut messages = Vec::new();
        assert!(crate::subchat::append_runner_deliveries(&mut messages, deliveries));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].extra["delivery"]["id"], unrelated.id);
        assert!(app.agents.has_preempt(&agent_id).await);
        assert_eq!(
            app.agents
                .pending_deliveries(&agent_id)
                .await
                .iter()
                .map(|delivery| delivery.id.as_str())
                .collect::<Vec<_>>(),
            vec![unrelated.id.as_str()]
        );
        assert!(app.agents.get_any(&agent_id).await.unwrap().delivery_ids.is_empty());
        assert!(messages.iter().all(|message| {
            refact_core::chat_types::delivery_id_of_message(message)
                != Some(stale.id.as_str())
        }));
    }

    fn aborted_config(soft_abort: bool) -> SubchatConfig {
        let mut config = test_subchat_config();
        config.abort_flag = Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
        config.soft_abort = soft_abort;
        config
    }

    #[test]
    fn streaming_deltas_stamp_watchdog_activity() {
        let stamp = Arc::new(AtomicU64::new(0));
        let mut collector = SubchatProgressCollector::new(None, None, Some(stamp.clone()));

        collector.on_delta_ops(
            0,
            vec![crate::chat::types::DeltaOp::AppendContent {
                text: "token".to_string(),
            }],
        );

        assert!(stamp.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn abort_error_detection_matches_stream_and_loop_variants() {
        assert!(crate::subchat::is_abort_error("Aborted"));
        assert!(crate::subchat::is_abort_error("aborted"));
        assert!(!crate::subchat::is_abort_error("aborted by user"));
        assert!(!crate::subchat::is_abort_error("context length exceeded"));
    }

    #[tokio::test]
    async fn soft_abort_returns_accumulated_messages_with_aborted_flag() {
        let ccx = abort_test_ccx().await;
        let config = aborted_config(true);
        let messages = vec![ChatMessage::new("user".to_string(), "hi".to_string())];
        let mut usage = crate::call_validation::ChatUsage::default();
        let progress = Arc::new(StdMutex::new(Vec::new()));
        let result = crate::subchat::run_subchat_loop(
            ccx,
            &config,
            messages,
            &ToolsPolicy::None,
            &mut usage,
            &progress,
        )
        .await;
        let (out, aborted) = result.expect("soft abort must return Ok");
        assert!(aborted);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "user");
        assert_eq!(out[0].content.content_text_only(), "hi");
    }

    #[tokio::test]
    async fn hard_abort_still_returns_aborted_error() {
        let ccx = abort_test_ccx().await;
        let config = aborted_config(false);
        let messages = vec![ChatMessage::new("user".to_string(), "hi".to_string())];
        let mut usage = crate::call_validation::ChatUsage::default();
        let progress = Arc::new(StdMutex::new(Vec::new()));
        let result = crate::subchat::run_subchat_loop(
            ccx,
            &config,
            messages,
            &ToolsPolicy::None,
            &mut usage,
            &progress,
        )
        .await;
        assert_eq!(result.err(), Some("Aborted".to_string()));
    }

    #[tokio::test]
    async fn wrap_up_soft_abort_returns_accumulated_messages() {
        let ccx = abort_test_ccx().await;
        let config = aborted_config(true);
        let wrap_up = crate::subchat::WrapUpConfig {
            depth: 1,
            tokens_cnt: 1000,
            prompt: "wrap up".to_string(),
        };
        let messages = vec![ChatMessage::new("user".to_string(), "hi".to_string())];
        let mut usage = crate::call_validation::ChatUsage::default();
        let progress = Arc::new(StdMutex::new(Vec::new()));
        let result = crate::subchat::run_subchat_with_wrap_up(
            ccx,
            &config,
            messages,
            &ToolsPolicy::None,
            &wrap_up,
            &mut usage,
            &progress,
        )
        .await;
        let (out, aborted) = result.expect("soft abort must return Ok");
        assert!(aborted);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content.content_text_only(), "hi");
    }

    #[tokio::test]
    async fn wrap_up_hard_abort_still_returns_aborted_error() {
        let ccx = abort_test_ccx().await;
        let config = aborted_config(false);
        let wrap_up = crate::subchat::WrapUpConfig {
            depth: 1,
            tokens_cnt: 1000,
            prompt: "wrap up".to_string(),
        };
        let messages = vec![ChatMessage::new("user".to_string(), "hi".to_string())];
        let mut usage = crate::call_validation::ChatUsage::default();
        let progress = Arc::new(StdMutex::new(Vec::new()));
        let result = crate::subchat::run_subchat_with_wrap_up(
            ccx,
            &config,
            messages,
            &ToolsPolicy::None,
            &wrap_up,
            &mut usage,
            &progress,
        )
        .await;
        assert_eq!(result.err(), Some("Aborted".to_string()));
    }

    #[test]
    fn stable_chat_id_prefers_the_configured_id() {
        let mut config = test_subchat_config();
        config.chat_id = Some("subchat-stable".to_string());
        config.background_agent_id = Some("bgagent-1".to_string());
        assert_eq!(
            stable_subchat_chat_id(&config, || "fresh".to_string()),
            "subchat-stable"
        );
    }

    #[test]
    fn stable_chat_id_falls_back_to_the_background_agent_id() {
        let mut config = test_subchat_config();
        config.background_agent_id = Some("bgagent-1".to_string());
        let first = stable_subchat_chat_id(&config, || "fresh-1".to_string());
        let second = stable_subchat_chat_id(&config, || "fresh-2".to_string());
        assert_eq!(first, "subchat-bgagent-1");
        assert_eq!(first, second);
    }

    #[test]
    fn stable_chat_id_still_mints_a_fresh_id_for_one_shot_subchats() {
        let config = test_subchat_config();
        assert_eq!(
            stable_subchat_chat_id(&config, || "fresh".to_string()),
            "fresh"
        );
        let mut blank = test_subchat_config();
        blank.chat_id = Some("   ".to_string());
        assert_eq!(
            stable_subchat_chat_id(&blank, || "fresh".to_string()),
            "fresh"
        );
    }

    #[test]
    fn stream_chat_id_reuses_the_context_chat_id_across_turns() {
        assert_eq!(
            stable_stream_chat_id("subchat-abc", || "fresh-1".to_string()),
            "subchat-abc"
        );
        assert_eq!(
            stable_stream_chat_id("subchat-abc", || "fresh-2".to_string()),
            "subchat-abc"
        );
        assert_eq!(stable_stream_chat_id("  ", || "fresh".to_string()), "fresh");
    }

    #[tokio::test]
    async fn subchat_confirmation_defaults_follow_yaml_settings() {
        let gcx = make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        global_configs_try_create_all(&config_dir).await.unwrap();

        assert_eq!(
            resolve_subagent_confirmation_defaults(&ctx_of(&gcx), "subagent").await,
            (true, true, true)
        );
        assert_eq!(
            resolve_subagent_confirmation_defaults(&ctx_of(&gcx), "title_generation").await,
            (false, false, false)
        );

        fs::write(
            config_dir.join("subagents/subagent.yaml"),
            "schema_version: 7\nid: subagent\nsubchat:\n  autonomous_no_confirm: false\n  auto_approve_editing_tools: false\n  auto_approve_dangerous_commands: false\n",
        )
        .unwrap();
        crate::yaml_configs::customization_registry::invalidate_all_registry_caches(gcx.clone())
            .await;

        assert_eq!(
            resolve_subagent_confirmation_defaults(&ctx_of(&gcx), "subagent").await,
            (false, false, false)
        );
    }

    #[tokio::test]
    async fn usage_progress_prefers_metering_and_omits_unknown_cost() {
        let gcx = make_test_gcx().await;
        let progress = Arc::new(StdMutex::new(Vec::new()));
        let mut config = test_subchat_config();
        config.step_progress = Some({
            let progress = progress.clone();
            Arc::new(move |update| progress.lock().unwrap().push(update))
        });
        let mut metered = ChatMessage::new("assistant".to_string(), "done".to_string());
        metered.usage = Some(crate::call_validation::ChatUsage {
            total_tokens: 12,
            metering_usd: Some(crate::call_validation::MeteringUsd {
                total_usd: 0.42,
                ..Default::default()
            }),
            ..Default::default()
        });

        crate::subchat::emit_usage_progress(&ctx_of(&gcx), &config, &[vec![metered]]).await;

        assert_eq!(
            progress.lock().unwrap().as_slice(),
            [SubchatProgress::Usage {
                tokens_delta: 12,
                cost_delta: Some(0.42),
            }]
        );

        progress.lock().unwrap().clear();
        let mut unmetered = ChatMessage::new("assistant".to_string(), "done".to_string());
        unmetered.usage = Some(crate::call_validation::ChatUsage {
            total_tokens: 7,
            ..Default::default()
        });

        crate::subchat::emit_usage_progress(&ctx_of(&gcx), &config, &[vec![unmetered]]).await;

        assert_eq!(
            progress.lock().unwrap().as_slice(),
            [SubchatProgress::Usage {
                tokens_delta: 7,
                cost_delta: None,
            }]
        );
    }

    #[test]
    fn context_reconstruction_is_the_only_ephemeral_subchat_trajectory() {
        let mut config = test_subchat_config();
        assert!(should_persist_subchat_trajectory(&config));

        config.tool_name = "title_generation".to_string();
        assert!(should_persist_subchat_trajectory(&config));

        config.tool_name = "mode_transition".to_string();
        assert!(!should_persist_subchat_trajectory(&config));

        config.stateful = true;
        assert!(should_persist_subchat_trajectory(&config));
    }

    #[test]
    fn subchat_trajectory_commit_phases_have_explicit_intents() {
        let cases = [
            (
                SubchatTrajectoryCommitPhase::Seed,
                TrajectoryCommitIntent::Checkpoint,
            ),
            (
                SubchatTrajectoryCommitPhase::Progress,
                TrajectoryCommitIntent::Checkpoint,
            ),
            (
                SubchatTrajectoryCommitPhase::Failed,
                TrajectoryCommitIntent::Required,
            ),
            (
                SubchatTrajectoryCommitPhase::Final,
                TrajectoryCommitIntent::Required,
            ),
        ];

        for (phase, expected) in cases {
            assert_eq!(subchat_trajectory_commit_intent(phase), expected);
        }
    }

    #[tokio::test]
    async fn final_and_failed_subchat_snapshots_use_required_durable_commits() {
        let workspace = tempfile::tempdir().unwrap();
        let gcx = make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let chat_id = "subchat-required-final-failed";
        let config = test_subchat_config();
        let thread = trace_thread_from_config(chat_id, &config);
        let final_messages = vec![ChatMessage::new(
            "assistant".to_string(),
            "final".to_string(),
        )];

        save_trajectory_as_with_intent(&gcx.chat_ctx(gcx.clone()),
            &thread,
            &final_messages,
            subchat_trajectory_commit_intent(SubchatTrajectoryCommitPhase::Final),
        )
        .await;
        let final_saved = crate::chat::trajectories::load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), chat_id)
            .await
            .expect("final subchat snapshot should be durable");
        assert_eq!(final_saved.messages[0].content.content_text_only(), "final");

        let progress = Arc::new(StdMutex::new(final_messages));
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        save_failed_subchat_trajectory(&app, chat_id, &config, &progress, "failed").await;
        let failed_saved = crate::chat::trajectories::load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), chat_id)
            .await
            .expect("failed subchat snapshot should be durable");
        assert!(failed_saved.messages.iter().any(|message| message
            .content
            .content_text_only()
            .contains("Subchat run failed")));
    }

    #[test]
    fn context_reconstruction_never_retries_provider_calls() {
        let mut config = test_subchat_config();
        assert!(subchat_retries_allowed(&config));

        config.tool_name = "mode_transition".to_string();
        assert!(!subchat_retries_allowed(&config));
    }

    fn sample_worktree() -> (tempfile::TempDir, WorktreeMeta) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worktree");
        let source = temp.path().join("source");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let source = fs::canonicalize(source).unwrap();
        (
            temp,
            WorktreeMeta {
                id: "wt-subchat".to_string(),
                kind: "task_agent".to_string(),
                root,
                source_workspace_root: source.clone(),
                repo_root: source,
                branch: Some("feature".to_string()),
                base_branch: Some("main".to_string()),
                base_commit: Some("base".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("card-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                enforce: true,
            },
        )
    }

    async fn install_caps(gcx: Arc<crate::global_context::GlobalContext>, caps: CodeAssistantCaps) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_add(60);
        let caps_state = gcx.caps_state.clone();
        let mut caps_state = caps_state.write().await;
        caps_state.caps = Some(Arc::new(caps));
        caps_state.last_attempted_ts = now;
    }

    fn privacy_record(path: &str, zone: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            zone: zone.to_string(),
            attribution: Attribution::Declared,
        }
    }

    fn message_with_privacy(role: &str, content: &str, records: Vec<FileRecord>) -> ChatMessage {
        let mut message = ChatMessage::new(role.to_string(), content.to_string());
        message.extra.insert(
            "privacy".to_string(),
            serde_json::to_value(PrivacyRecord { files: records }).unwrap(),
        );
        message
    }

    fn install_privacy_policy(
        gcx: &Arc<crate::global_context::GlobalContext>,
        allowed_destination: &str,
        report_declassifies: bool,
    ) {
        *gcx.privacy_policy_load.write().unwrap() = PolicyLoad {
            policy: Arc::new(PrivacyPolicy {
                blocked: Vec::new(),
                zones: vec![
                    Zone {
                        name: "secrets".to_string(),
                        patterns: vec![".env*".to_string()],
                        send_to: vec![allowed_destination.to_string()],
                        on_shell_read: ShellBehavior::Withhold,
                    },
                    Zone {
                        name: "normal".to_string(),
                        patterns: vec!["*".to_string()],
                        send_to: vec!["*".to_string()],
                        on_shell_read: ShellBehavior::Withhold,
                    },
                ],
                subagents: SubagentPolicy {
                    report_declassifies,
                },
                ..Default::default()
            }),
            error: None,
            source_paths: Vec::new(),
        };
    }

    #[tokio::test]
    async fn subchat_projects_archived_secrets_before_privacy_gate() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        install_privacy_policy(&gcx, "trusted", true);
        let archived = message_with_privacy(
            "user",
            "archived secret",
            vec![privacy_record(".env", "secrets")],
        );
        let ctx = ctx_of(&gcx);
        assert!(gate_subchat_boundary(&ctx, &[archived.clone()], "untrusted/model").is_err());
        let mut current = ChatMessage::new("user".into(), "public current request".into());
        current.message_id = "public-current-request".into();
        let report = refact_core::active_context::make_reconstruction_report(
            vec![current],
            Default::default(),
        )
        .unwrap();
        let prepared =
            prepare_subchat_messages(&ctx_of(&gcx), vec![archived, report], "untrusted/model")
                .unwrap();
        assert_eq!(prepared.len(), 1);
        assert_eq!(
            prepared[0].content.content_text_only(),
            "public current request"
        );
    }

    #[tokio::test]
    async fn subchat_privacy_in_gate_checks_subagent_model_destination() {
        let gcx = make_test_gcx().await;
        install_privacy_policy(&gcx, "trusted", true);
        let messages = vec![message_with_privacy(
            "user",
            "guarded context",
            vec![privacy_record(".env", "secrets")],
        )];

        let error =
            gate_subchat_boundary(&ctx_of(&gcx), &messages, "untrusted/model").unwrap_err();

        assert!(error.starts_with("Output withheld by user privacy policy"));
        assert!(error.contains("zone \""));
    }

    #[tokio::test]
    async fn subchat_sanitization_preserves_records_for_destination_gate() {
        let gcx = make_test_gcx().await;
        install_privacy_policy(&gcx, "trusted", true);
        let messages = vec![message_with_privacy(
            "user",
            "guarded context",
            vec![privacy_record(".env", "secrets")],
        )];

        let sanitized = sanitize_messages_for_new_thread(&messages);
        let error =
            prepare_subchat_messages(&ctx_of(&gcx), sanitized, "untrusted/model").unwrap_err();

        assert!(error.starts_with("Output withheld by user privacy policy"));
        assert!(error.contains("zone \""));
    }

    #[tokio::test]
    async fn subchat_privacy_out_gate_checks_parent_model_destination() {
        let gcx = make_test_gcx().await;
        install_privacy_policy(&gcx, "child", false);
        let mut messages = vec![
            message_with_privacy(
                "tool",
                "guarded result",
                vec![privacy_record(".env", "secrets")],
            ),
            ChatMessage::new("assistant".to_string(), "safe report".to_string()),
        ];
        let policy = gcx
            .privacy_policy_load
            .read()
            .unwrap()
            .policy
            .subagents
            .clone();
        apply_subchat_report_policy(&policy, &mut messages).unwrap();

        let error =
            gate_subchat_boundary(&ctx_of(&gcx), &messages[1..], "parent/model").unwrap_err();

        assert!(error.starts_with("Output withheld by user privacy policy"));
    }

    #[test]
    fn subchat_report_declassifies_by_default() {
        let mut messages = vec![
            message_with_privacy(
                "tool",
                "guarded result",
                vec![privacy_record(".env", "secrets")],
            ),
            message_with_privacy(
                "assistant",
                "final report",
                vec![privacy_record("generated.txt", "normal")],
            ),
        ];

        apply_subchat_report_policy(&SubagentPolicy::default(), &mut messages).unwrap();

        assert!(!messages[1].extra.contains_key("privacy"));
    }

    #[test]
    fn subchat_report_inherits_union_when_declassification_is_disabled() {
        let secret = privacy_record(".env", "secrets");
        let normal = privacy_record("src/lib.rs", "normal");
        let mut messages = vec![
            message_with_privacy("tool", "first", vec![secret.clone(), normal.clone()]),
            message_with_privacy("tool", "second", vec![secret.clone()]),
            ChatMessage::new("assistant".to_string(), "final report".to_string()),
        ];

        apply_subchat_report_policy(
            &SubagentPolicy {
                report_declassifies: false,
            },
            &mut messages,
        )
        .unwrap();

        let report: PrivacyRecord =
            serde_json::from_value(messages[2].extra["privacy"].clone()).unwrap();
        assert_eq!(report.files, vec![secret, normal]);
    }

    #[tokio::test]
    async fn guarded_subchat_context_gets_non_quotation_instruction() {
        let gcx = make_test_gcx().await;
        install_privacy_policy(&gcx, "trusted", true);
        let messages = vec![message_with_privacy(
            "user",
            "guarded context",
            vec![privacy_record(".env", "secrets")],
        )];

        let prepared = prepare_subchat_messages(&ctx_of(&gcx), messages, "trusted/model").unwrap();

        assert_eq!(prepared[0].role, "system");
        assert_eq!(
            prepared[0].content.content_text_only(),
            GUARDED_REPORT_INSTRUCTION
        );
    }

    #[test]
    fn subchat_context_limit_compaction_gate_allows_wrapped_partial_output_error() {
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let error = format!(
            "{} Original error: context_length_exceeded",
            PARTIAL_OUTPUT_STREAM_ERROR,
        );

        assert!(should_compact_context_limit_error(&error, 0, &Some(abort)));
    }

    #[test]
    fn subchat_context_limit_compaction_gate_respects_abort() {
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        assert!(!should_compact_context_limit_error(
            "context_length_exceeded",
            0,
            &Some(abort),
        ));
    }

    #[test]
    fn partial_output_stream_error_message_redacts_provider_secret() {
        let output = partial_output_stream_error_message(
            "provider failed: Authorization: Bearer sk-test-secret",
        );

        assert!(output.contains(PARTIAL_OUTPUT_STREAM_ERROR));
        assert!(!output.contains("sk-test-secret"));
        assert!(!output.contains("Authorization: Bearer sk-test-secret"));
        assert!(output.contains("[REDACTED"));
    }

    #[test]
    fn partial_output_stream_error_message_bounds_huge_provider_error() {
        let far_tail = "FAR_TAIL_MARKER";
        let input = format!(
            "provider failed: Authorization: Bearer sk-test-secret {} {}",
            "x".repeat(200_000),
            far_tail,
        );

        let output = partial_output_stream_error_message(&input);
        let max_len = PARTIAL_OUTPUT_STREAM_ERROR.len()
            + " Original error: ".len()
            + SAFE_PROVIDER_ERROR_DIAGNOSTIC_MAX_CHARS;

        assert!(output.len() <= max_len);
        assert!(output.contains(SAFE_PROVIDER_ERROR_DIAGNOSTIC_TRUNCATED));
        assert!(!output.contains("sk-test-secret"));
        assert!(!output.contains("Authorization: Bearer sk-test-secret"));
        assert!(output.contains("[REDACTED"));
        assert!(!output.contains(far_tail));
    }

    #[tokio::test]
    async fn subchat_reactive_compaction_unavailable_model_has_no_fallback_and_preserves_archive() {
        let gcx = make_test_gcx().await;
        install_caps(gcx.clone(), CodeAssistantCaps::default()).await;
        let config = test_subchat_config();
        let mut messages = vec![
            ChatMessage::new("user".to_string(), "first".to_string()),
            ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText("x".repeat(500)),
                tool_call_id: "tc1".to_string(),
                ..Default::default()
            },
            ChatMessage::new("user".to_string(), "wrap up".to_string()),
        ];

        let original = serde_json::to_value(&messages).unwrap();
        let compacted = apply_subchat_reactive_compaction(
            gcx,
            &config,
            &mut messages,
            "context_length_exceeded",
            1,
            true,
        )
        .await;

        assert!(!compacted);
        assert_eq!(messages.len(), 4);
        let diagnostic = messages.remove(2);
        assert_eq!(diagnostic.role, "error");
        assert!(crate::chat::diagnostics::is_ui_only_message(&diagnostic));
        assert!(diagnostic
            .content
            .content_text_only()
            .contains("context_length_exceeded"));
        assert_eq!(serde_json::to_value(&messages).unwrap(), original);
        assert_eq!(
            messages.last().unwrap().content.content_text_only(),
            "wrap up"
        );
        assert!(!messages
            .iter()
            .any(|message| message.role
                == refact_chat_history::trajectory_ops::COMPRESSION_REPORT_ROLE));
        assert!(!messages
            .iter()
            .any(refact_core::active_context::is_legacy_summary));
        assert!(!messages.iter().any(|message| {
            message
                .content
                .content_text_only()
                .contains("Previous non-user subchat activity was summarized")
        }));
    }

    #[tokio::test]
    async fn subchat_reactive_compaction_emits_parent_diagnostics() {
        let gcx = make_test_gcx().await;
        install_caps(gcx.clone(), CodeAssistantCaps::default()).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = test_subchat_config();
        config.parent_tool_call_id = Some("call_1".to_string());
        config.parent_subchat_tx = Some(std::sync::Arc::new(tokio::sync::Mutex::new(tx)));
        let mut messages = vec![
            ChatMessage::new("user".to_string(), "first".to_string()),
            ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText("x".repeat(500)),
                tool_call_id: "tc1".to_string(),
                ..Default::default()
            },
            ChatMessage::new("user".to_string(), "second".to_string()),
        ];

        let original = serde_json::to_value(&messages).unwrap();
        let error = "context_length_exceeded: Authorization: Bearer sk-test-secret";
        let compacted =
            apply_subchat_reactive_compaction(gcx, &config, &mut messages, error, 1, false).await;

        assert!(!compacted);
        assert_eq!(messages.len(), 4);
        let diagnostic = messages.pop().unwrap();
        assert_eq!(diagnostic.role, "error");
        assert!(crate::chat::diagnostics::is_ui_only_message(&diagnostic));
        let diagnostic_json = serde_json::to_string(&diagnostic).unwrap();
        assert!(!diagnostic_json.contains("sk-test-secret"));
        assert!(diagnostic_json.contains("[REDACTED"));
        assert_eq!(serde_json::to_value(&messages).unwrap(), original);
        let first = rx.try_recv().unwrap();
        assert_eq!(
            first.get("tool_call_id").and_then(|v| v.as_str()),
            Some("call_1")
        );
        let status = first.get("subchat_id").and_then(|v| v.as_str()).unwrap();
        assert!(status.contains("could not rebuild active context (attempt 1)"));
        assert!(status.contains("context_length_exceeded"));
        assert!(!status.contains("sk-test-secret"));
        assert!(status.contains("[REDACTED"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn subchat_parent_compaction_diagnostic_redacts_provider_secret() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = test_subchat_config();
        config.parent_tool_call_id = Some("call_1".to_string());
        config.parent_subchat_tx = Some(std::sync::Arc::new(tokio::sync::Mutex::new(tx)));
        let error = "context_length_exceeded: Authorization: Bearer sk-test-secret request failed";

        emit_parent_compaction_diagnostics(&config, error, 1, true).await;

        let first = rx.try_recv().unwrap();
        let status = first.get("subchat_id").and_then(|v| v.as_str()).unwrap();
        assert!(status.contains("handled by rebuilding active context"));
        assert!(status.contains("attempt 1"));
        assert!(!status.contains("sk-test-secret"));
        assert!(!status.contains("Authorization: Bearer sk-test-secret"));
        assert!(status.contains("[REDACTED"));
    }

    #[test]
    fn subchat_context_limit_log_error_redacts_provider_secret() {
        let error = "context_length_exceeded: Authorization: Bearer sk-test-secret";

        let log_error = safe_context_limit_error_for_log(error);

        assert!(!log_error.contains("sk-test-secret"));
        assert!(!log_error.contains("Authorization: Bearer sk-test-secret"));
        assert!(log_error.contains("[REDACTED"));
        assert!(log_error.len() <= PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS);
    }

    #[test]
    fn subchat_parent_compaction_diagnostic_caps_long_provider_error() {
        let error = format!("context_length_exceeded: {}", "x".repeat(4_000));

        let status = parent_compaction_diagnostic_status(&error, 9, false);

        assert!(status.contains("could not rebuild active context"));
        assert!(status.contains("attempt 9"));
        assert!(status.len() <= PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS);
        assert!(status.ends_with(PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED));
    }

    #[test]
    fn subchat_parent_compaction_diagnostic_windows_huge_provider_error() {
        let far_tail = "FAR_TAIL_MARKER";
        let error = format!(
            "context_length_exceeded: Authorization: Bearer sk-test-secret {} {}",
            "x".repeat(200_000),
            far_tail,
        );

        let status = parent_compaction_diagnostic_status(&error, 1, true);

        assert!(status.len() <= PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS);
        assert!(status.ends_with(PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED));
        assert!(!status.contains("sk-test-secret"));
        assert!(!status.contains("Authorization: Bearer sk-test-secret"));
        assert!(status.contains("[REDACTED"));
        assert!(!status.contains(far_tail));
    }

    #[test]
    fn subchat_context_limit_log_error_redacts_secret_near_lookahead_boundary() {
        let prefix_len = PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS.saturating_sub(80);
        let error = format!(
            "context_length_exceeded: {} Authorization: Bearer sk-boundarysecretvalue {}",
            "x".repeat(prefix_len),
            "y".repeat(PARENT_COMPACTION_DIAGNOSTIC_REDACTION_LOOKAHEAD_CHARS * 2),
        );

        let log_error = safe_context_limit_error_for_log(&error);

        assert!(log_error.len() <= PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS);
        assert!(log_error.ends_with(PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED));
        assert!(!log_error.contains("sk-boundarysecretvalue"));
        assert!(!log_error.contains("Authorization: Bearer sk-boundarysecretvalue"));
        assert!(log_error.contains("[REDACTED"));
    }

    #[test]
    fn subchat_worktree_stateful_thread_from_config_carries_scope() {
        let (_temp, worktree) = sample_worktree();
        let task_meta = TaskMeta {
            task_id: "task-1".to_string(),
            role: "agents".to_string(),
            agent_id: Some("agent-1".to_string()),
            card_id: Some("card-1".to_string()),
            planner_chat_id: Some("planner-task-1-1".to_string()),
        };
        let config = SubchatConfig {
            tool_name: "subagent".to_string(),
            stateful: true,
            autonomous_no_confirm: false,
            auto_approve_editing_tools: true,
            auto_approve_dangerous_commands: true,
            chat_id: None,
            title: Some("Subchat".to_string()),
            parent_id: Some("parent".to_string()),
            link_type: Some("subagent".to_string()),
            root_chat_id: Some("root".to_string()),
            tools: ToolsPolicy::Only(vec!["cat".to_string()]),
            max_steps: 3,
            prepend_system_prompt: false,
            wrap_up: None,
            task_meta: Some(task_meta.clone()),
            worktree: Some(worktree.clone()),
            model: "model".to_string(),
            mode: "agent".to_string(),
            n_ctx: 4096,
            max_new_tokens: 512,
            temperature: None,
            reasoning_effort: Some(ReasoningEffort::Low),
            cache_control: crate::llm::params::CacheControl::Ephemeral,
            parent_tool_call_id: None,
            parent_subchat_tx: None,
            abort_flag: None,
            soft_abort: false,
            activity_stamp: None,
            background_agent_id: None,
            subchat_depth: 1,
            final_step_force_answer: false,
            buddy_meta: None,
            step_progress: None,
            trace_parent: TraceParent::unattributed(),
        };

        let thread = stateful_thread_from_config("subchat-1", &config);

        assert_eq!(thread.id, "subchat-1");
        assert_eq!(thread.context_tokens_cap, Some(4096));
        assert_eq!(thread.auto_compression_cap, Some(3686));
        assert!(!thread.auto_compression_cap_pending);
        assert_eq!(thread.auto_compression_cap, Some(3686));
        assert!(!thread.auto_compression_cap_pending);
        assert_eq!(thread.task_meta, Some(task_meta));
        assert_eq!(thread.worktree, Some(worktree));
        assert_eq!(thread.tool_use, "cat");
        assert_eq!(thread.parent_id.as_deref(), Some("parent"));
        assert_eq!(thread.root_chat_id.as_deref(), Some("root"));
        assert!(thread.auto_approve_editing_tools);
        assert!(thread.auto_approve_dangerous_commands);
    }

    #[test]
    fn stateful_thread_from_config_preserves_disabled_approval_flags() {
        let config = test_subchat_config();

        let thread = stateful_thread_from_config("subchat-1", &config);

        assert!(!thread.auto_approve_editing_tools);
        assert!(!thread.auto_approve_dangerous_commands);
    }

    #[test]
    fn stateful_subchat_from_planner_uses_hidden_task_role() {
        let task_meta = TaskMeta {
            task_id: "task-1".to_string(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: Some("planner-task-1-1".to_string()),
        };
        let config = SubchatConfig {
            tool_name: "subagent".to_string(),
            stateful: true,
            autonomous_no_confirm: false,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            chat_id: None,
            title: Some("Subagent: Gathering Files".to_string()),
            parent_id: Some("planner-task-1-1".to_string()),
            link_type: Some("gather_files".to_string()),
            root_chat_id: Some("planner-task-1-1".to_string()),
            tools: ToolsPolicy::Only(vec!["cat".to_string(), "tree".to_string()]),
            max_steps: 3,
            prepend_system_prompt: false,
            wrap_up: None,
            task_meta: Some(task_meta),
            worktree: None,
            model: "model".to_string(),
            mode: "agent".to_string(),
            n_ctx: 4096,
            max_new_tokens: 512,
            temperature: None,
            reasoning_effort: None,
            cache_control: crate::llm::params::CacheControl::Ephemeral,
            parent_tool_call_id: None,
            parent_subchat_tx: None,
            abort_flag: None,
            soft_abort: false,
            activity_stamp: None,
            background_agent_id: None,
            subchat_depth: 1,
            final_step_force_answer: false,
            buddy_meta: None,
            step_progress: None,
            trace_parent: TraceParent::unattributed(),
        };

        let thread = stateful_thread_from_config("subchat-1", &config);

        assert_eq!(
            thread.task_meta.as_ref().map(|m| m.role.as_str()),
            Some("subchats")
        );
        assert_eq!(
            thread.task_meta.as_ref().map(|m| m.task_id.as_str()),
            Some("task-1")
        );
        assert_eq!(thread.parent_id.as_deref(), Some("planner-task-1-1"));
        assert_eq!(thread.link_type.as_deref(), Some("gather_files"));
        assert_eq!(thread.root_chat_id.as_deref(), Some("planner-task-1-1"));
    }

    #[test]
    fn subchat_worktree_config_fields_carry_parent_scope() {
        let (_temp, worktree) = sample_worktree();
        let task_meta = TaskMeta {
            task_id: "task-1".to_string(),
            role: "agents".to_string(),
            agent_id: Some("agent-1".to_string()),
            card_id: Some("card-1".to_string()),
            planner_chat_id: Some("planner-task-1-1".to_string()),
        };
        let config = SubchatConfig {
            tool_name: "subagent".to_string(),
            stateful: false,
            autonomous_no_confirm: false,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            chat_id: None,
            title: None,
            parent_id: None,
            link_type: None,
            root_chat_id: None,
            tools: ToolsPolicy::All,
            max_steps: 1,
            prepend_system_prompt: false,
            wrap_up: None,
            task_meta: Some(task_meta.clone()),
            worktree: Some(worktree.clone()),
            model: "model".to_string(),
            mode: "agent".to_string(),
            n_ctx: 4096,
            max_new_tokens: 512,
            temperature: None,
            reasoning_effort: None,
            cache_control: crate::llm::params::CacheControl::Ephemeral,
            parent_tool_call_id: None,
            parent_subchat_tx: None,
            abort_flag: None,
            soft_abort: false,
            activity_stamp: None,
            background_agent_id: None,
            subchat_depth: 1,
            final_step_force_answer: false,
            buddy_meta: None,
            step_progress: None,
            trace_parent: TraceParent::unattributed(),
        };

        assert_eq!(config.task_meta, Some(task_meta));
        assert_eq!(config.worktree, Some(worktree));
    }

    #[tokio::test]
    async fn subchat_worktree_config_inherits_scope_from_parent_session() {
        let gcx = make_test_gcx().await;
        let (_temp, worktree) = sample_worktree();
        let parent_chat_id = "parent-session-chat".to_string();
        let sessions = gcx.chat_sessions.clone();
        {
            let mut sessions_write = sessions.write().await;
            let mut parent_session = crate::chat::types::ChatSession::new(parent_chat_id.clone());
            parent_session.thread.worktree = Some(worktree.clone());
            sessions_write.insert(
                parent_chat_id.clone(),
                Arc::new(tokio::sync::Mutex::new(parent_session)),
            );
        }

        let resolved = resolve_subchat_worktree(&ctx_of(&gcx), Some(&parent_chat_id), None).await;

        assert_eq!(resolved, Some(worktree));
    }

    #[tokio::test]
    async fn subchat_worktree_config_prefers_current_parent_scope_over_parent_session() {
        let gcx = make_test_gcx().await;
        let (_stored_temp, stored_worktree) = sample_worktree();
        let (_current_temp, mut current_worktree) = sample_worktree();
        current_worktree.id = "wt-current-parent-scope".to_string();
        let parent_chat_id = "parent-explicit-chat".to_string();
        let sessions = gcx.chat_sessions.clone();
        {
            let mut sessions_write = sessions.write().await;
            let mut parent_session = crate::chat::types::ChatSession::new(parent_chat_id.clone());
            parent_session.thread.worktree = Some(stored_worktree);
            sessions_write.insert(
                parent_chat_id.clone(),
                Arc::new(tokio::sync::Mutex::new(parent_session)),
            );
        }

        let resolved =
            resolve_subchat_worktree(
                &ctx_of(&gcx),
                Some(&parent_chat_id),
                Some(current_worktree.clone()),
            )
            .await;

        assert_eq!(resolved, Some(current_worktree));
    }

    #[tokio::test]
    async fn subchat_worktree_config_does_not_invent_scope_when_parent_has_none() {
        let gcx = make_test_gcx().await;
        let parent_chat_id = "parent-without-worktree".to_string();
        let sessions = gcx.chat_sessions.clone();
        {
            let mut sessions_write = sessions.write().await;
            sessions_write.insert(
                parent_chat_id.clone(),
                Arc::new(tokio::sync::Mutex::new(
                    crate::chat::types::ChatSession::new(parent_chat_id.clone()),
                )),
            );
        }

        let resolved = resolve_subchat_worktree(&ctx_of(&gcx), Some(&parent_chat_id), None).await;

        assert_eq!(resolved, None);
    }

    #[tokio::test]
    async fn subchat_worktree_lookup_falls_back_to_parent_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let service = crate::worktrees::service::WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/chat/subchat-parent".to_string()),
                chat_id: Some("parent-trajectory-chat".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let worktree = created.worktree.meta.clone();
        let parent_chat_id = "parent-trajectory-chat".to_string();
        let thread = ThreadParams {
            id: parent_chat_id.clone(),
            title: "Parent".to_string(),
            model: "model".to_string(),
            worktree: Some(worktree.clone()),
            ..Default::default()
        };
        let messages = vec![ChatMessage::new("user".to_string(), "hello".to_string())];
        save_trajectory_as_with_intent(&gcx.chat_ctx(gcx.clone()),
            &thread,
            &messages,
            TrajectoryCommitIntent::Checkpoint,
        )
        .await;

        let ctx = ctx_of(&gcx);
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if parent_thread_worktree(&ctx, &parent_chat_id).await.is_some() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("checkpoint should persist the parent worktree");
        assert_eq!(parent_thread_worktree(&ctx, &parent_chat_id).await, Some(worktree));
    }

    #[tokio::test]
    async fn subchat_worktree_active_detached_session_does_not_restore_stale_trajectory_scope() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.clone()];
        }
        let service = crate::worktrees::service::WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/chat/subchat-detached-parent".to_string()),
                chat_id: Some("parent-detached-chat".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let parent_chat_id = "parent-detached-chat".to_string();
        let persisted_thread = ThreadParams {
            id: parent_chat_id.clone(),
            title: "Parent".to_string(),
            model: "model".to_string(),
            worktree: Some(created.worktree.meta.clone()),
            ..Default::default()
        };
        let messages = vec![ChatMessage::new("user".to_string(), "hello".to_string())];
        save_trajectory_as_with_intent(&gcx.chat_ctx(gcx.clone()),
            &persisted_thread,
            &messages,
            TrajectoryCommitIntent::Checkpoint,
        )
        .await;
        let sessions = gcx.chat_sessions.clone();
        {
            let mut sessions_write = sessions.write().await;
            sessions_write.insert(
                parent_chat_id.clone(),
                Arc::new(tokio::sync::Mutex::new(
                    crate::chat::types::ChatSession::new(parent_chat_id.clone()),
                )),
            );
        }

        assert_eq!(parent_thread_worktree(&ctx_of(&gcx), &parent_chat_id).await, None);
    }

    #[tokio::test]
    async fn subchat_worktree_registers_stateful_child_reference() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("repo");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        init_repo(&source);
        let gcx = crate::global_context::tests::make_test_gcx_with_dirs(
            cache.clone(),
            std::env::temp_dir().join(format!("refact-cfg-{}", uuid::Uuid::new_v4())),
        )
        .await;
        let service = crate::worktrees::service::WorktreeService::new(cache, source).unwrap();
        let created = service
            .create_worktree(crate::worktrees::types::CreateWorktreeRequest {
                branch: Some("refact/chat/subchat-child-reference".to_string()),
                chat_id: Some("parent-ref-chat".to_string()),
                kind: Some("chat".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let config = SubchatConfig {
            tool_name: "subagent".to_string(),
            stateful: true,
            autonomous_no_confirm: false,
            auto_approve_editing_tools: false,
            auto_approve_dangerous_commands: false,
            chat_id: Some("child-ref-chat".to_string()),
            title: Some("Subchat".to_string()),
            parent_id: Some("parent-ref-chat".to_string()),
            link_type: Some("subagent".to_string()),
            root_chat_id: Some("parent-ref-chat".to_string()),
            tools: ToolsPolicy::Only(vec!["cat".to_string()]),
            max_steps: 1,
            prepend_system_prompt: false,
            wrap_up: None,
            task_meta: None,
            worktree: Some(created.worktree.meta.clone()),
            model: "model".to_string(),
            mode: "agent".to_string(),
            n_ctx: 4096,
            max_new_tokens: 512,
            temperature: None,
            reasoning_effort: None,
            cache_control: crate::llm::params::CacheControl::Ephemeral,
            parent_tool_call_id: None,
            parent_subchat_tx: None,
            abort_flag: None,
            soft_abort: false,
            activity_stamp: None,
            background_agent_id: None,
            subchat_depth: 1,
            final_step_force_answer: false,
            buddy_meta: None,
            step_progress: None,
            trace_parent: TraceParent::unattributed(),
        };
        let mut thread = stateful_thread_from_config("child-ref-chat", &config);

        register_stateful_subchat_worktree(&ctx_of(&gcx), "child-ref-chat", &mut thread).await;
        assert_eq!(thread.worktree, Some(created.worktree.meta.clone()));

        let view = service
            .get_worktree(&created.worktree.meta.id)
            .await
            .unwrap();
        assert_eq!(view.reference_count, 2);
        assert!(view
            .references
            .iter()
            .any(|reference| reference.chat_id.as_deref() == Some("parent-ref-chat")));
        assert!(view
            .references
            .iter()
            .any(|reference| reference.chat_id.as_deref() == Some("child-ref-chat")));
    }

    #[tokio::test]
    async fn test_resolve_subchat_params_normalizes_review_agents_for_smaller_model() {
        let gcx = make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        global_configs_try_create_all(&config_dir).await.unwrap();

        let thinking_model_id = "claude_code/claude-opus-4-6".to_string();
        let mut caps = CodeAssistantCaps::default();
        caps.chat_models.insert(
            thinking_model_id.clone(),
            chat_model_record(
                &thinking_model_id,
                200_000,
                "https://api.anthropic.com/v1/messages",
            ),
        );
        caps.defaults.chat_default_model = thinking_model_id.clone();
        caps.defaults.chat_light_model = thinking_model_id.clone();
        caps.defaults.chat_thinking_model = thinking_model_id;

        install_caps(gcx.clone(), caps).await;

        let params = resolve_subchat_params(&ctx_of(&gcx), "review_agents")
            .await
            .unwrap();
        let extra_budget = (params.subchat_n_ctx as f32 * 0.06) as usize;

        assert_eq!(params.subchat_n_ctx, 200_000);
        assert_eq!(params.subchat_cache_control, CacheControl::Off);
        assert!(
            params.subchat_max_new_tokens + params.subchat_tokens_for_rag + extra_budget
                < params.subchat_n_ctx,
            "normalized review_agents budget must fit the clamped model context window"
        );
    }

    #[tokio::test]
    async fn test_resolve_subchat_config_carries_cache_control() {
        let gcx = make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        global_configs_try_create_all(&config_dir).await.unwrap();

        let thinking_model_id = "claude_code/claude-opus-4-6".to_string();
        let mut caps = CodeAssistantCaps::default();
        caps.chat_models.insert(
            thinking_model_id.clone(),
            chat_model_record(
                &thinking_model_id,
                200_000,
                "https://api.anthropic.com/v1/messages",
            ),
        );
        caps.defaults.chat_default_model = thinking_model_id.clone();
        caps.defaults.chat_light_model = thinking_model_id.clone();
        caps.defaults.chat_thinking_model = thinking_model_id;

        install_caps(gcx.clone(), caps).await;

        let config = resolve_subchat_config_with_parent(
            gcx,
            "review_agents",
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            1,
            true,
            None,
            "agent".to_string(),
            None,
            None,
            None,
            None,
            None,
            0,
        )
        .await
        .unwrap();

        assert_eq!(config.cache_control, CacheControl::Off);
    }

    #[test]
    fn test_parse_subchat_cache_control_rejects_invalid_values() {
        let err = parse_subchat_cache_control("bad_cache_agent", Some("forever")).unwrap_err();

        assert!(err.contains("invalid cache_control 'forever'"));
        assert!(err.contains("bad_cache_agent"));
        assert!(err.contains("expected: off, ephemeral"));
    }

    #[tokio::test]
    async fn test_resolve_subchat_params_defaults_cache_control_to_ephemeral() {
        let gcx = make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        global_configs_try_create_all(&config_dir).await.unwrap();

        let light_model_id = "openai/gpt-4o-mini".to_string();
        let mut caps = CodeAssistantCaps::default();
        caps.chat_models.insert(
            light_model_id.clone(),
            chat_model_record(
                &light_model_id,
                200_000,
                "https://api.openai.com/v1/chat/completions",
            ),
        );
        caps.defaults.chat_default_model = light_model_id.clone();
        caps.defaults.chat_light_model = light_model_id.clone();
        caps.defaults.chat_thinking_model = light_model_id;

        install_caps(gcx.clone(), caps).await;

        let params = resolve_subchat_params(&ctx_of(&gcx), "subagent").await.unwrap();

        assert_eq!(params.subchat_cache_control, CacheControl::Ephemeral);
    }

    #[tokio::test]
    async fn test_resolve_subchat_model_errors_when_light_model_missing() {
        let gcx = make_test_gcx().await;
        let default_model_id = "openai/gpt-4o".to_string();

        let mut caps = CodeAssistantCaps::default();
        caps.chat_models.insert(
            default_model_id.clone(),
            chat_model_record(
                &default_model_id,
                128_000,
                "https://api.openai.com/v1/chat/completions",
            ),
        );
        caps.defaults.chat_default_model = default_model_id;

        install_caps(gcx.clone(), caps).await;

        let params = SubchatParameters {
            subchat_model_type: ChatModelType::Light,
            subchat_model: String::new(),
            subchat_n_ctx: 128_000,
            subchat_max_new_tokens: 8_192,
            subchat_temperature: None,
            subchat_tokens_for_rag: 0,
            subchat_reasoning_effort: None,
            subchat_cache_control: crate::llm::params::CacheControl::Ephemeral,
        };

        let err = resolve_subchat_model(&ctx_of(&gcx), &params).await.unwrap_err();
        assert!(err.contains("Light model is not set up"));
        assert!(err.contains("Default model settings"));
    }

    #[tokio::test]
    async fn test_resolve_subchat_model_errors_when_endpoint_empty() {
        let gcx = make_test_gcx().await;
        let default_model_id = "openai/gpt-4o".to_string();
        let thinking_model_id = "broken/o1".to_string();

        let mut caps = CodeAssistantCaps::default();
        caps.chat_models.insert(
            default_model_id.clone(),
            chat_model_record(
                &default_model_id,
                128_000,
                "https://api.openai.com/v1/chat/completions",
            ),
        );
        caps.chat_models.insert(
            thinking_model_id.clone(),
            chat_model_record(&thinking_model_id, 128_000, ""),
        );
        caps.defaults.chat_default_model = default_model_id;
        caps.defaults.chat_light_model = "openai/gpt-4o-mini".to_string();
        caps.defaults.chat_thinking_model = thinking_model_id.clone();

        install_caps(gcx.clone(), caps).await;

        let params = SubchatParameters {
            subchat_model_type: ChatModelType::Thinking,
            subchat_model: String::new(),
            subchat_n_ctx: 128_000,
            subchat_max_new_tokens: 8_192,
            subchat_temperature: None,
            subchat_tokens_for_rag: 0,
            subchat_reasoning_effort: None,
            subchat_cache_control: crate::llm::params::CacheControl::Ephemeral,
        };

        let err = resolve_subchat_model(&ctx_of(&gcx), &params).await.unwrap_err();
        assert!(err.contains("Thinking model 'broken/o1' is misconfigured"));
        assert!(err.contains("an empty LLM endpoint URL"));
        assert!(err.contains("Default model settings"));
    }

    #[tokio::test]
    async fn test_resolve_subchat_model_errors_when_endpoint_relative() {
        let gcx = make_test_gcx().await;
        let default_model_id = "openai/gpt-4o".to_string();
        let thinking_model_id = "openai/o1".to_string();

        let mut caps = CodeAssistantCaps::default();
        caps.chat_models.insert(
            default_model_id.clone(),
            chat_model_record(
                &default_model_id,
                128_000,
                "https://api.openai.com/v1/chat/completions",
            ),
        );
        caps.chat_models.insert(
            thinking_model_id.clone(),
            chat_model_record(&thinking_model_id, 128_000, "/v1/chat/completions"),
        );
        caps.defaults.chat_default_model = default_model_id;
        caps.defaults.chat_thinking_model = thinking_model_id.clone();

        install_caps(gcx.clone(), caps).await;

        let params = SubchatParameters {
            subchat_model_type: ChatModelType::Thinking,
            subchat_model: String::new(),
            subchat_n_ctx: 128_000,
            subchat_max_new_tokens: 8_192,
            subchat_temperature: None,
            subchat_tokens_for_rag: 0,
            subchat_reasoning_effort: None,
            subchat_cache_control: crate::llm::params::CacheControl::Ephemeral,
        };

        let err = resolve_subchat_model(&ctx_of(&gcx), &params).await.unwrap_err();
        assert!(err.contains("Thinking model 'openai/o1' is misconfigured"));
        assert!(err.contains("an invalid LLM endpoint URL"));
        assert!(err.contains("Default model settings"));
    }

    #[tokio::test]
    async fn test_resolve_subchat_params_names_gather_files_light_model_requirement() {
        let gcx = make_test_gcx().await;
        let config_dir = gcx.config_dir.clone();
        global_configs_try_create_all(&config_dir).await.unwrap();

        let thinking_model_id = "openai/gpt-5".to_string();
        let mut caps = CodeAssistantCaps::default();
        caps.chat_models.insert(
            thinking_model_id.clone(),
            chat_model_record(
                &thinking_model_id,
                128_000,
                "https://api.openai.com/v1/chat/completions",
            ),
        );
        caps.defaults.chat_default_model = thinking_model_id.clone();
        caps.defaults.chat_thinking_model = thinking_model_id;

        install_caps(gcx.clone(), caps).await;

        let err = resolve_subchat_params(&ctx_of(&gcx), "title_generation")
            .await
            .unwrap_err();

        assert!(err.contains("Light model required by subagent 'title_generation'"));
        assert!(err.contains("model_type: light"));
        assert!(err.contains("Default model settings"));
    }
}
