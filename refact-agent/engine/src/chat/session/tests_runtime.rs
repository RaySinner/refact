use super::*;
use super::tests_support::*;
use super::super::types::{ChatCommand, CommandRequest};
use crate::chat::perf_diagnostics;
use std::time::Instant;

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
    fn test_increment_version() {
        let mut session = make_session();
        assert_eq!(session.trajectory_version, 0);
        assert!(!session.trajectory_dirty);
        session.increment_version();
        assert_eq!(session.trajectory_version, 1);
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn test_touch_updates_last_activity() {
        let mut session = make_session();
        let before = session.last_activity;
        std::thread::sleep(std::time::Duration::from_millis(10));
        session.touch();
        assert!(session.last_activity > before);
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
