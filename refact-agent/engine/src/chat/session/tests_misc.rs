use super::*;
use super::tests_support::*;
use serde_json::json;

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
    fn test_model_switch_invalidates_provider_usage() {
        let mut session = make_session();
        session.provider_usage_stale = false;
        session.invalidate_usage_on_model_switch();
        assert!(session.provider_usage_stale);
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
    fn test_create_sessions_map() {
        let map = create_sessions_map();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let read = map.read().await;
            assert!(read.is_empty());
        });
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
