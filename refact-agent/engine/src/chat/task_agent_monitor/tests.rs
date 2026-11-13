#[allow(unused_imports)]
use super::*;
    use crate::tasks::types::{
        BoardCard, StatusUpdate, TaskBoard, TaskMeta as StoredTaskMeta, TaskStatus,
    };
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    fn create_test_card(id: &str, column: &str, assignee: Option<String>) -> BoardCard {
        let agent_chat_id = assignee.as_ref().map(|a| format!("agent-{}", a));
        BoardCard {
            id: id.to_string(),
            title: format!("Card {}", id),
            column: column.to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: "Test instructions".to_string(),
            assignee,
            agent_chat_id,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            started_at: Some(chrono::Utc::now().to_rfc3339()),
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: vec![],
            scope_guard_mode: Default::default(),
        }
    }

    fn make_test_agent_session(
        task_id: &str,
        card_id: &str,
        agent_id: &str,
        agent_chat_id: &str,
        state: SessionState,
        idle_for: Duration,
    ) -> ChatSession {
        let mut session = ChatSession::new(agent_chat_id.to_string());
        session.thread.task_meta = Some(TaskMeta {
            task_id: task_id.to_string(),
            role: "agents".to_string(),
            agent_id: Some(agent_id.to_string()),
            card_id: Some(card_id.to_string()),
            planner_chat_id: Some("planner-test".to_string()),
        });
        session.runtime.state = state;
        let activity_at = Instant::now()
            .checked_sub(idle_for)
            .unwrap_or_else(Instant::now);
        session.last_activity = activity_at;
        match state {
            SessionState::Generating => session.last_stream_delta_at = Some(activity_at),
            SessionState::ExecutingTools => session.last_tool_started_at = Some(activity_at),
            _ => {}
        }
        session
            .queue_processor_running
            .store(true, Ordering::SeqCst);
        session
    }

    async fn setup_monitor_case(
        column: &str,
        state: SessionState,
        idle_for: Duration,
        status_updates: Vec<StatusUpdate>,
    ) -> (
        tempfile::TempDir,
        AppState,
        String,
        String,
        Arc<tokio::sync::Mutex<ChatSession>>,
        Arc<tokio::sync::Mutex<ChatSession>>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        *app.workspace
            .documents_state
            .workspace_folders
            .lock()
            .unwrap() = vec![temp.path().to_path_buf()];
        let task = crate::tasks::storage::create_task(gcx.clone(), "Stall task")
            .await
            .unwrap();
        let now = Utc::now().to_rfc3339();
        let meta = StoredTaskMeta {
            schema_version: 1,
            id: task.id.clone(),
            name: task.name.clone(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: if column == "doing" { 1 } else { 0 },
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: true,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        crate::tasks::storage::save_task_meta(gcx.clone(), &task.id, &meta)
            .await
            .unwrap();

        let card_id = "T-1".to_string();
        let agent_id = "agent-1".to_string();
        let agent_chat_id = "agent-T-1".to_string();
        let mut card = create_test_card(&card_id, column, Some(agent_id.clone()));
        card.agent_chat_id = Some(agent_chat_id.clone());
        card.status_updates = status_updates;
        crate::tasks::storage::save_board(
            gcx.clone(),
            &task.id,
            &TaskBoard {
                cards: vec![card],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let session = make_test_agent_session(
            &task.id,
            &card_id,
            &agent_id,
            &agent_chat_id,
            state,
            idle_for,
        );
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        app.chat
            .sessions
            .write()
            .await
            .insert(agent_chat_id.clone(), session_arc.clone());

        let planner_chat_id = "planner-test".to_string();
        let mut planner_session = ChatSession::new(planner_chat_id.clone());
        planner_session.thread.task_meta = Some(TaskMeta {
            task_id: task.id.clone(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: None,
        });
        planner_session.runtime.state = SessionState::Generating;
        planner_session.draft_message = Some(ChatMessage {
            role: "assistant".to_string(),
            content: crate::call_validation::ChatContent::SimpleText(
                "Unfinished planner draft".to_string(),
            ),
            ..Default::default()
        });
        planner_session
            .queue_processor_running
            .store(true, Ordering::SeqCst);
        let planner_arc = Arc::new(tokio::sync::Mutex::new(planner_session));
        app.chat
            .sessions
            .write()
            .await
            .insert(planner_chat_id, planner_arc.clone());

        (temp, app, task.id, agent_chat_id, session_arc, planner_arc)
    }

    async fn board_card(app: &AppState, task_id: &str) -> BoardCard {
        storage::load_board(app.gcx.clone(), task_id)
            .await
            .unwrap()
            .get_card("T-1")
            .unwrap()
            .clone()
    }

    #[tokio::test]
    async fn agent_in_error_state_retries_without_premature_failure() {
        let (_temp, app, task_id, _agent_chat_id, agent_arc, _planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Error,
            Duration::from_secs(10),
            vec![],
        )
        .await;
        {
            let mut session = agent_arc.lock().await;
            session.runtime.error = Some("Stream error test".to_string());
            session.task_agent_error = Some("Stream error test".to_string());
        }

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(
            card.column, "doing",
            "card should stay in doing on first error retry"
        );
        assert_eq!(card.retry_count, 1);
        assert!(card
            .status_updates
            .iter()
            .any(|u| u.message.contains("Retrying agent after streaming error")));

        let session = agent_arc.lock().await;
        assert_eq!(session.runtime.state, SessionState::Idle);
        assert!(session.task_agent_error.is_none());
    }

    #[tokio::test]
    async fn idle_doing_agent_without_finish_notifies_planner() {
        let (_temp, app, task_id, agent_chat_id, agent_arc, planner_arc) =
            setup_monitor_case("doing", SessionState::Idle, Duration::from_secs(90), vec![]).await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card.status_updates.iter().any(|update| {
            update
                .message
                .starts_with(STALL_PLANNER_NOTIFY_STATUS_PREFIX)
                && update.message.contains(&agent_chat_id)
        }));

        let agent = agent_arc.lock().await;
        assert!(agent.command_queue.is_empty());
        assert_eq!(
            agent.messages.len(),
            1,
            "the agent itself must be nudged, not only the planner"
        );
        assert_eq!(
            agent.messages[0].extra["delivery"]["source"],
            json!(TASK_AGENT_MONITOR_SOURCE),
            "the nudge must be provenance-stamped like any delivered message"
        );

        let planner = planner_arc.lock().await;
        assert_eq!(planner.pending_deliveries.len(), 1);
        assert!(planner.command_queue.is_empty());
        let queued = planner.pending_deliveries.front().unwrap();
        assert_eq!(queued.push, PushMode::Append);
        assert!(queued.wake);
        let notice = queued.messages.last().unwrap();
        assert_eq!(notice.role, "event");
        assert_eq!(
            notice.extra["event"]["source"],
            json!(TASK_AGENT_MONITOR_SOURCE)
        );
        assert_eq!(
            notice.extra["event"]["payload"]["kind"],
            json!("stalled_agent")
        );
        let text = notice.content.content_text_only();
        assert!(text.contains("appears stalled"));
        assert!(text.contains("T-1"));
        assert!(text.contains("restart_agent"));
        assert!(text.contains(&agent_chat_id));
    }

    #[tokio::test]
    async fn blocking_question_append_preserves_busy_draft_and_deduplicates() {
        let (_temp, app, task_id, _agent_chat_id, _agent_arc, planner_arc) =
            setup_monitor_case("doing", SessionState::Idle, Duration::from_secs(90), vec![]).await;
        let (draft, messages) = {
            let planner = planner_arc.lock().await;
            (
                serde_json::to_value(&planner.draft_message).unwrap(),
                serde_json::to_value(&planner.messages).unwrap(),
            )
        };
        for expected in [DeliveryOutcome::Queued, DeliveryOutcome::Duplicate] {
            assert_eq!(
                notify_planner_blocking_question(
app.chat_ctx(),
                    &task_id,
                    "T-1",
                    "planner-test",
                    "q-1",
                    "Which approach?",
                    PushMode::Append,
                )
                .await
                .unwrap(),
                expected
            );
        }
        let mut planner = planner_arc.lock().await;
        assert_eq!(serde_json::to_value(&planner.draft_message).unwrap(), draft);
        assert_eq!(serde_json::to_value(&planner.messages).unwrap(), messages);
        assert_eq!(planner.runtime.state, SessionState::Generating);
        assert!(!planner.user_interrupt_flag.load(Ordering::SeqCst));
        assert!(planner.command_queue.is_empty());
        assert_eq!(planner.pending_deliveries.len(), 1);
        let delivery = planner.pending_deliveries.front().unwrap();
        assert_eq!(delivery.push, PushMode::Append);
        assert!(delivery.wake);
        assert_eq!(
            delivery.id,
            format!("task-agent-blocking-question:{task_id}:T-1:q-1")
        );
        let draft = planner.draft_message.take().unwrap();
        planner.add_message(draft);
        let (delivered, wake) = planner.drain_pending_deliveries();
        assert_eq!(delivered.len(), 1);
        assert!(wake);
        assert!(planner.pending_deliveries.is_empty());
        assert_eq!(planner.messages.len(), 2);
        assert_eq!(planner.messages[0].role, "assistant");
        assert_eq!(
            planner.messages[1].extra["event"]["payload"]["kind"],
            json!("blocking_question")
        );
    }

    #[tokio::test]
    async fn active_agent_does_not_notify_planner() {
        for state in [SessionState::WaitingUserInput, SessionState::WaitingIde] {
            let (_temp, app, task_id, _agent_chat_id, _agent_arc, planner_arc) =
                setup_monitor_case("doing", state, Duration::from_secs(90), vec![]).await;

            check_for_stuck_agents(app.clone()).await.unwrap();

            let board = storage::load_board(app.gcx.clone(), &task_id)
                .await
                .unwrap();
            let card = board.get_card("T-1").unwrap();
            assert!(card.status_updates.iter().all(|update| {
                !update
                    .message
                    .starts_with(STALL_PLANNER_NOTIFY_STATUS_PREFIX)
            }));
            assert!(planner_arc.lock().await.pending_deliveries.is_empty());
        }
    }

    #[tokio::test]
    async fn done_or_failed_card_does_not_notify_planner() {
        for column in ["done", "failed"] {
            let (_temp, app, task_id, _agent_chat_id, _agent_arc, planner_arc) =
                setup_monitor_case(column, SessionState::Idle, Duration::from_secs(90), vec![])
                    .await;

            check_for_stuck_agents(app.clone()).await.unwrap();

            let board = storage::load_board(app.gcx.clone(), &task_id)
                .await
                .unwrap();
            let card = board.get_card("T-1").unwrap();
            assert!(card.status_updates.iter().all(|update| {
                !update
                    .message
                    .starts_with(STALL_PLANNER_NOTIFY_STATUS_PREFIX)
            }));
            assert!(planner_arc.lock().await.pending_deliveries.is_empty());
        }
    }

    #[tokio::test]
    async fn planner_notify_cooldown_prevents_spam() {
        let recent = StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: format!("{} agent-T-1", STALL_PLANNER_NOTIFY_STATUS_PREFIX),
        };
        let (_temp, app, task_id, _agent_chat_id, _agent_arc, planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Idle,
            Duration::from_secs(90),
            vec![recent],
        )
        .await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(stall_planner_notifications(card).0, 1);
        assert!(planner_arc.lock().await.pending_deliveries.is_empty());
    }

    #[tokio::test]
    async fn completed_state_on_doing_card_notifies_planner() {
        let (_temp, app, task_id, _agent_chat_id, _agent_arc, planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Completed,
            Duration::from_secs(90),
            vec![],
        )
        .await;
        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        assert_eq!(
            stall_planner_notifications(board.get_card("T-1").unwrap()).0,
            1
        );
        let planner = planner_arc.lock().await;
        assert_eq!(planner.pending_deliveries.len(), 1);
        assert!(planner.command_queue.is_empty());
        assert_eq!(
            planner.pending_deliveries.front().unwrap().push,
            PushMode::Append
        );
        let text = planner
            .pending_deliveries
            .front()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .content
            .content_text_only();
        assert!(text.contains("Completed state but the card is still in `doing`"));
    }

    #[test]
    fn test_agent_stuck_timeout_constant() {
        assert_eq!(AGENT_STUCK_TIMEOUT.as_secs(), 20 * 60);
    }

    #[test]
    fn test_monitor_interval_constant() {
        assert_eq!(MONITOR_INTERVAL.as_secs(), 2 * 60);
    }

    #[test]
    fn test_tool_stall_timeout_constant() {
        assert_eq!(TOOL_STALL_TIMEOUT.as_secs(), 8 * 60);
    }

    #[test]
    fn stalled_agent_fail_after_notify_failure_waits_for_hard_timeout() {
        assert!(!stalled_agent_should_fail_after_notify_failure(
            AGENT_STUCK_TIMEOUT - Duration::from_secs(1)
        ));
        assert!(stalled_agent_should_fail_after_notify_failure(
            AGENT_STUCK_TIMEOUT
        ));
    }

    #[tokio::test]
    async fn agent_is_notified_when_silent_without_finish() {
        let (_temp, app, task_id, agent_chat_id, agent_arc, _planner_arc) =
            setup_monitor_case("doing", SessionState::Idle, Duration::from_secs(90), vec![]).await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(
            card.status_updates.iter().any(|update| {
                update.message.starts_with(STALL_AGENT_NOTIFY_STATUS_PREFIX)
                    && update.message.contains(&agent_chat_id)
            }),
            "the agent nudge must be recorded on the card, got {:?}",
            card.status_updates
        );

        let agent = agent_arc.lock().await;
        let notice = agent
            .messages
            .last()
            .expect("the stalled agent itself must receive a notice");
        assert_eq!(notice.role, "event");
        assert_eq!(
            notice.extra["event"]["source"],
            json!(TASK_AGENT_MONITOR_SOURCE)
        );
        assert_eq!(
            notice.extra["event"]["payload"]["kind"],
            json!("stalled_agent_self")
        );
        let text = notice.content.content_text_only();
        assert!(text.contains("agent_finish"), "{text}");
        assert!(text.contains("agent_ask_planner"), "{text}");
        assert!(text.contains("T-1"), "{text}");
    }

    #[tokio::test]
    async fn agent_notification_uses_when_idle_push_mode() {
        // A generating agent is the case where the push mode actually decides
        // something: the turn is still open, so `WhenIdle` must hold the nudge
        // instead of splicing it into the live turn.
        let (_temp, app, _task_id, _agent_chat_id, agent_arc, _planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Generating,
            Duration::from_secs(600),
            vec![],
        )
        .await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let agent = agent_arc.lock().await;
        let delivery = agent
            .pending_deliveries
            .front()
            .expect("an open turn must hold the nudge");
        assert_eq!(
            delivery.push,
            PushMode::WhenIdle,
            "Append could land inside an open assistant/tool window"
        );
        assert!(
            delivery.wake,
            "an idle agent must be woken, not just queued"
        );
    }

    /// An idle agent's turn is over, so the nudge lands at once; an agent still
    /// mid-turn holds it until the whole turn ends. Both states are the ones
    /// `WhenIdle` exists for, and neither leaves the message stranded.
    #[tokio::test]
    async fn agent_notification_waits_for_the_whole_turn() {
        let (_temp, app, _task_id, _agent_chat_id, agent_arc, _planner_arc) =
            setup_monitor_case("doing", SessionState::Idle, Duration::from_secs(90), vec![]).await;
        check_for_stuck_agents(app.clone()).await.unwrap();
        assert!(
            agent_arc.lock().await.messages.len() == 1,
            "an idle agent has no draft or tool window, so the nudge lands now"
        );

        let (_temp, app, _task_id, _agent_chat_id, agent_arc, _planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Generating,
            Duration::from_secs(600),
            vec![],
        )
        .await;
        check_for_stuck_agents(app.clone()).await.unwrap();
        let agent = agent_arc.lock().await;
        assert_eq!(
            agent.pending_deliveries.len(),
            1,
            "a still-running turn must hold the nudge"
        );
        assert!(
            agent.messages.is_empty(),
            "the nudge must not split an in-flight turn"
        );
    }

    #[tokio::test]
    async fn escalation_is_recorded_on_card_when_planner_missing() {
        let (_temp, app, task_id, agent_chat_id, _agent_arc, _planner_arc) =
            setup_monitor_case("doing", SessionState::Idle, Duration::from_secs(90), vec![]).await;

        assert!(!notify_planner_about_stalled_agent(
app.ctx.clone(),
            &task_id,
            &board_card(&app, &task_id).await,
            &agent_chat_id,
            None,
            StallKind::IdleNoFinish,
            Duration::from_secs(90),
        )
        .await
        .unwrap());

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(
            card.status_updates.iter().any(|update| {
                update.message.starts_with(STALL_ESCALATION_STATUS_PREFIX)
                    && update.message.contains("planner chat id unknown")
            }),
            "a missing planner must be recorded, not swallowed; got {:?}",
            card.status_updates
        );
        assert!(
            card.status_updates.iter().all(|update| !update
                .message
                .starts_with(STALL_PLANNER_NOTIFY_STATUS_PREFIX)),
            "nothing was delivered, so no success record may be written"
        );
    }

    /// "Nowhere to deliver" and "delivery broke" must not look the same: the
    /// second one means a real chat existed and the transport failed.
    #[tokio::test]
    async fn escalation_delivery_failure_is_distinguishable() {
        let (_temp, app, task_id, agent_chat_id, _agent_arc, planner_arc) =
            setup_monitor_case("doing", SessionState::Idle, Duration::from_secs(90), vec![]).await;
        // A real destination that cannot accept the batch: the planner chat
        // exists and is a valid planner, but its delivery queue is saturated.
        {
            let mut planner = planner_arc.lock().await;
            for n in 0..crate::chat::types::max_queue_size() {
                planner
                    .pending_deliveries
                    .push_back(PendingDelivery::with_id(
                        format!("filler-{n}"),
                        vec![ChatMessage::new("user".into(), "filler".into())],
                        PushMode::Append,
                        "test",
                        false,
                    ));
            }
        }

        let card = board_card(&app, &task_id).await;
        assert!(!notify_planner_about_stalled_agent(
app.ctx.clone(),
            &task_id,
            &card,
            &agent_chat_id,
            Some("planner-test"),
            StallKind::IdleNoFinish,
            Duration::from_secs(90),
        )
        .await
        .unwrap());

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(
            card.status_updates.iter().any(|update| {
                update
                    .message
                    .starts_with(STALL_ESCALATION_STATUS_PREFIX)
                    && update.message.contains("delivery to planner planner-test failed")
                    && update.message.contains("chat delivery queue is full")
                    && update.message.contains(STALL_PLANNER_NOTIFY_STATUS_PREFIX)
            }),
            "a broken delivery must name the transport failure and retract the premature 'notified' line; got {:?}",
            card.status_updates
        );
        assert!(
            card.status_updates
                .iter()
                .all(|update| !update.message.contains("planner chat id unknown")),
            "a failed delivery is not a missing planner chat"
        );
    }

    #[tokio::test]
    async fn agent_notification_does_not_consume_planner_notification_budget() {
        let recent = StatusUpdate {
            timestamp: Utc::now().to_rfc3339(),
            message: format!("{} agent-T-1", STALL_AGENT_NOTIFY_STATUS_PREFIX),
        };
        let (_temp, app, task_id, _agent_chat_id, agent_arc, planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Idle,
            Duration::from_secs(90),
            vec![recent],
        )
        .await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(stall_agent_notifications(card).0, 1);
        assert_eq!(
            stall_planner_notifications(card).0,
            1,
            "the planner escalation still has its own full budget"
        );
        assert_eq!(
            planner_arc.lock().await.pending_deliveries.len(),
            1,
            "a recent agent nudge must not silence the planner escalation"
        );
        assert!(
            agent_arc.lock().await.messages.is_empty(),
            "the agent cooldown still applies to the agent's own nudges"
        );
    }

    #[tokio::test]
    async fn exhausted_agent_nudges_do_not_mark_the_card_failed() {
        let spent = (0..MAX_STALL_AGENT_NOTIFICATIONS_PER_CARD)
            .map(|_| StatusUpdate {
                timestamp: (Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
                message: format!("{} agent-T-1", STALL_AGENT_NOTIFY_STATUS_PREFIX),
            })
            .collect();
        let (_temp, app, task_id, _agent_chat_id, agent_arc, _planner_arc) =
            setup_monitor_case("doing", SessionState::Idle, Duration::from_secs(90), spent).await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        assert_eq!(
            board.get_card("T-1").unwrap().column,
            "doing",
            "exhausting the agent's own nudge budget must not fail the card"
        );
        assert!(
            agent_arc.lock().await.messages.is_empty(),
            "no agent nudge may be sent once its own budget is spent"
        );
    }

    #[test]
    fn test_agents_active_count() {
        let cards = vec![
            create_test_card("T-1", "doing", Some("agent-1".to_string())),
            create_test_card("T-2", "doing", Some("agent-2".to_string())),
            create_test_card("T-3", "done", Some("agent-3".to_string())),
            create_test_card("T-4", "planned", None),
        ];

        let active_count = cards
            .iter()
            .filter(|c| c.column == "doing" && c.assignee.is_some())
            .count();

        assert_eq!(active_count, 2);
    }

    #[test]
    fn test_timestamp_fallback_logic() {
        let mut card = create_test_card("T-1", "doing", Some("agent-1".to_string()));

        card.status_updates.push(StatusUpdate {
            timestamp: "2024-01-01T10:00:00Z".to_string(),
            message: "Test update".to_string(),
        });

        let last_activity = card
            .status_updates
            .last()
            .map(|u| u.timestamp.as_str())
            .or(card.started_at.as_deref())
            .unwrap_or(&card.created_at);

        assert_eq!(last_activity, "2024-01-01T10:00:00Z");
    }

    #[test]
    fn test_timestamp_fallback_to_started_at() {
        let card = create_test_card("T-1", "doing", Some("agent-1".to_string()));

        let last_activity = card
            .status_updates
            .last()
            .map(|u| u.timestamp.as_str())
            .or(card.started_at.as_deref())
            .unwrap_or(&card.created_at);

        assert_eq!(last_activity, card.started_at.as_ref().unwrap());
    }

    #[test]
    fn test_timestamp_fallback_to_created_at() {
        let mut card = create_test_card("T-1", "doing", Some("agent-1".to_string()));
        card.started_at = None;

        let last_activity = card
            .status_updates
            .last()
            .map(|u| u.timestamp.as_str())
            .or(card.started_at.as_deref())
            .unwrap_or(&card.created_at);

        assert_eq!(last_activity, &card.created_at);
    }

    #[test]
    fn test_all_finished_when_no_doing_cards() {
        let cards = vec![
            create_test_card("T-1", "done", Some("agent-1".to_string())),
            create_test_card("T-2", "failed", Some("agent-2".to_string())),
        ];

        let agents_active = cards
            .iter()
            .filter(|c| c.column == "doing" && c.assignee.is_some())
            .count();

        assert_eq!(agents_active, 0);
    }

    #[test]
    fn test_elapsed_time_calculation() {
        let old_time = chrono::Utc::now() - chrono::Duration::seconds(25 * 60);
        let old_timestamp = old_time.to_rfc3339();

        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&old_timestamp) {
            let elapsed =
                chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
            let elapsed_secs = elapsed.num_seconds() as u64;

            assert!(elapsed_secs >= 25 * 60 - 5);
            assert!(elapsed_secs > AGENT_STUCK_TIMEOUT.as_secs());
        } else {
            panic!("Failed to parse timestamp");
        }
    }

    #[test]
    fn test_assignee_mismatch_detection() {
        let card = create_test_card("T-1", "doing", Some("agent-123".to_string()));
        let expected_agent = "agent-456";

        let mismatch = card.assignee.as_ref() != Some(&expected_agent.to_string());
        assert!(mismatch);
    }

    #[test]
    fn test_assignee_match() {
        let card = create_test_card("T-1", "doing", Some("agent-123".to_string()));
        let expected_agent = "agent-123";

        let matches = card.assignee.as_ref() == Some(&expected_agent.to_string());
        assert!(matches);
    }

    #[test]
    fn task_agent_monitor_transient_exhaustion_retains_worktree() {
        let kind = AgentFailureKind::from_error("LLM error (503 Service Unavailable): overloaded");
        let report = kind.final_report_reason("LLM error (503 Service Unavailable): overloaded");

        assert_eq!(kind, AgentFailureKind::TransientExhausted);
        assert!(!kind.should_cleanup_worktree());
        assert!(report.contains("worktree retained"));
        assert!(report.contains("Provider temporarily unavailable"));
        assert!(report.contains("Suggested action: Retry"));
    }

    #[test]
    fn task_agent_monitor_permanent_failure_retains_worktree() {
        let kind = AgentFailureKind::from_error("LLM error (401 Unauthorized): invalid api key");
        let report = kind.final_report_reason("LLM error (401 Unauthorized): invalid api key");

        assert_eq!(kind, AgentFailureKind::Permanent);
        assert!(!kind.should_cleanup_worktree());
        assert!(report.contains("retained"));
    }

    #[test]
    fn task_agent_monitor_only_cancelled_failure_cleans_up_worktree() {
        for kind in [
            AgentFailureKind::TransientExhausted,
            AgentFailureKind::ContextLimit,
            AgentFailureKind::Permanent,
        ] {
            assert!(
                !kind.should_cleanup_worktree(),
                "{:?} should retain worktree",
                kind
            );
        }
        assert!(AgentFailureKind::Cancelled.should_cleanup_worktree());
    }

    #[test]
    fn task_agent_monitor_context_limit_is_distinct_from_transient() {
        let kind = AgentFailureKind::from_error(
            "LLM error (413 Payload Too Large): context length exceeded",
        );
        let report =
            kind.final_report_reason("LLM error (413 Payload Too Large): context length exceeded");

        assert_eq!(kind, AgentFailureKind::ContextLimit);
        assert!(!kind.should_cleanup_worktree());
        assert!(report.contains("context limit"));
        assert!(report.contains("Context too large"));
        assert!(report.contains("Suggested action: Compact the chat"));
    }

    #[tokio::test]
    async fn cancelled_agent_failure_attempts_cleanup() {
        let (_temp, app, task_id, _agent_chat_id, _agent_arc, _planner_arc) =
            setup_monitor_case("doing", SessionState::Idle, Duration::from_secs(90), vec![]).await;

        mark_agent_as_failed(
app.ctx.clone(),
            &task_id,
            "T-1",
            Some("agent-1"),
            None,
            "cancelled by user",
            AgentFailureKind::Cancelled,
        )
        .await
        .unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "failed");
        let report = card.final_report.as_deref().unwrap_or("");
        assert!(report.contains("Automatic cleanup attempted"));
        assert!(report.contains("no worktree recorded"));
        assert!(report.contains("no branch recorded"));
        assert!(!report.contains("retained for inspection or retry"));
    }

    #[tokio::test]
    async fn stream_stall_notifies_planner() {
        let (_temp, app, task_id, agent_chat_id, agent_arc, planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Generating,
            STREAM_STALL_TIMEOUT + Duration::from_secs(10),
            vec![],
        )
        .await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(
            card.status_updates.iter().any(|u| {
                u.message.starts_with(STALL_PLANNER_NOTIFY_STATUS_PREFIX)
                    && u.message.contains(&agent_chat_id)
                    && u.message.contains("stream appears stalled")
            }),
            "expected stream-stall status update"
        );

        assert!(
            agent_arc.lock().await.command_queue.is_empty(),
            "agent itself should NOT receive a message"
        );

        let planner = planner_arc.lock().await;
        assert_eq!(
            planner.pending_deliveries.len(),
            1,
            "planner should be notified"
        );
        assert!(planner.command_queue.is_empty());
        assert_eq!(
            planner.pending_deliveries.front().unwrap().push,
            PushMode::Append
        );
        let text = planner
            .pending_deliveries
            .front()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .content
            .content_text_only();
        assert!(text.contains("stream stalled"));
        assert!(text.contains("Generating"));
    }

    #[tokio::test]
    async fn tool_stall_notifies_planner() {
        let (_temp, app, task_id, agent_chat_id, agent_arc, planner_arc) = setup_monitor_case(
            "doing",
            SessionState::ExecutingTools,
            TOOL_STALL_TIMEOUT + Duration::from_secs(10),
            vec![],
        )
        .await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(
            card.status_updates.iter().any(|u| {
                u.message.starts_with(STALL_PLANNER_NOTIFY_STATUS_PREFIX)
                    && u.message.contains(&agent_chat_id)
                    && u.message.contains("tool execution stalled")
            }),
            "expected tool-stall status update"
        );

        assert!(agent_arc.lock().await.command_queue.is_empty());
        let planner = planner_arc.lock().await;
        assert_eq!(planner.pending_deliveries.len(), 1);
        assert!(planner.command_queue.is_empty());
        assert_eq!(
            planner.pending_deliveries.front().unwrap().push,
            PushMode::Append
        );
        let text = planner
            .pending_deliveries
            .front()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .content
            .content_text_only();
        assert!(text.contains("tool execution stalled"));
    }

    #[tokio::test]
    async fn tool_state_with_recent_progress_is_not_notified() {
        let (_temp, app, task_id, _agent_chat_id, agent_arc, planner_arc) = setup_monitor_case(
            "doing",
            SessionState::ExecutingTools,
            TOOL_STALL_TIMEOUT + Duration::from_secs(10),
            vec![],
        )
        .await;
        {
            let mut session = agent_arc.lock().await;
            session.last_tool_progress_at = Some(Instant::now());
        }

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(card
            .status_updates
            .iter()
            .all(|u| { !u.message.starts_with(STALL_PLANNER_NOTIFY_STATUS_PREFIX) }));
        assert!(planner_arc.lock().await.pending_deliveries.is_empty());
    }

    #[tokio::test]
    async fn stream_stall_after_max_planner_notifications_marks_failed_transient() {
        let old_ts = (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339();
        let maxed = (0..MAX_STALL_PLANNER_NOTIFICATIONS_PER_CARD)
            .map(|_| StatusUpdate {
                timestamp: old_ts.clone(),
                message: format!(
                    "{} agent-T-1 (stream appears stalled, no token activity)",
                    STALL_PLANNER_NOTIFY_STATUS_PREFIX
                ),
            })
            .collect::<Vec<_>>();

        let (_temp, app, task_id, _agent_chat_id, _agent_arc, planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Generating,
            STREAM_STALL_TIMEOUT + Duration::from_secs(10),
            maxed,
        )
        .await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "failed", "card should be failed");
        assert!(
            card.final_report
                .as_deref()
                .unwrap_or("")
                .contains("stalled"),
            "report should mention stalled"
        );

        // Failure path goes through notify_planner_agents_finished which posts
        // the standard "agent finished" planner summary message.
        let planner = planner_arc.lock().await;
        assert!(
            !planner.pending_deliveries.is_empty(),
            "planner should receive a failure summary, not a stall notification"
        );
    }

    async fn setup_planner_wake_case(
        state: SessionState,
        wake_up_at: Option<chrono::DateTime<Utc>>,
        with_agent_card: bool,
    ) -> (
        tempfile::TempDir,
        AppState,
        String,
        Arc<tokio::sync::Mutex<ChatSession>>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        *app.workspace
            .documents_state
            .workspace_folders
            .lock()
            .unwrap() = vec![temp.path().to_path_buf()];

        let task = crate::tasks::storage::create_task(gcx.clone(), "Planner wake task")
            .await
            .unwrap();
        let now = Utc::now().to_rfc3339();
        let meta = crate::tasks::types::TaskMeta {
            schema_version: 1,
            id: task.id.clone(),
            name: task.name.clone(),
            status: crate::tasks::types::TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: if with_agent_card { 1 } else { 0 },
            cards_done: 0,
            cards_failed: 0,
            agents_active: if with_agent_card { 1 } else { 0 },
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: true,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        crate::tasks::storage::save_task_meta(gcx.clone(), &task.id, &meta)
            .await
            .unwrap();

        let cards = if with_agent_card {
            let mut c = create_test_card("T-1", "doing", Some("agent-1".to_string()));
            c.agent_chat_id = Some("agent-T-1".to_string());
            vec![c]
        } else {
            vec![]
        };
        crate::tasks::storage::save_board(
            gcx.clone(),
            &task.id,
            &crate::tasks::types::TaskBoard {
                cards,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let planner_chat_id = "planner-wake-chat".to_string();
        let mut planner_session = ChatSession::new(planner_chat_id.clone());
        planner_session.thread.task_meta = Some(TaskMeta {
            task_id: task.id.clone(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: None,
        });
        planner_session.runtime.state = state;
        planner_session.wake_up_at = wake_up_at;
        planner_session
            .queue_processor_running
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let planner_arc = Arc::new(tokio::sync::Mutex::new(planner_session));
        app.chat
            .sessions
            .write()
            .await
            .insert(planner_chat_id.clone(), planner_arc.clone());

        (temp, app, task.id, planner_arc)
    }

    #[tokio::test]
    async fn planner_wake_up_fires_when_deadline_passes() {
        let past = Utc::now() - chrono::Duration::seconds(10);
        let (_temp, app, _task_id, planner_arc) =
            setup_planner_wake_case(SessionState::WaitingUserInput, Some(past), false).await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let session = planner_arc.lock().await;
        assert_eq!(
            session.messages.len(),
            1,
            "wake-up message should be queued"
        );
        assert!(
            session.wake_up_at.is_none(),
            "wake_up_at cleared after fire"
        );
        assert!(session.command_queue.is_empty());
        assert!(session.pending_deliveries.is_empty());
        assert_eq!(
            session.messages.last().unwrap().extra["delivery"]["push"],
            json!("append")
        );
        assert!(
            session
                .messages
                .last()
                .unwrap()
                .content
                .content_text_only()
                .contains("[AUTO WAKE]"),
            "message should contain AUTO WAKE"
        );
    }

    #[tokio::test]
    async fn planner_wake_up_does_not_double_fire() {
        let past = Utc::now() - chrono::Duration::seconds(10);
        let (_temp, app, _task_id, planner_arc) =
            setup_planner_wake_case(SessionState::WaitingUserInput, Some(past), false).await;

        check_for_stuck_agents(app.clone()).await.unwrap();
        check_for_stuck_agents(app.clone()).await.unwrap();

        let session = planner_arc.lock().await;
        assert_eq!(
            session.messages.len(),
            1,
            "only one wake-up message after two sweeps"
        );
    }

    #[tokio::test]
    async fn planner_wake_up_skipped_when_state_changed() {
        let past = Utc::now() - chrono::Duration::seconds(10);
        let (_temp, app, _task_id, planner_arc) =
            setup_planner_wake_case(SessionState::Generating, Some(past), false).await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let session = planner_arc.lock().await;
        assert!(
            session.command_queue.is_empty(),
            "no wake message when state is not WaitingUserInput"
        );
    }

    #[test]
    fn card_accepts_agent_chat_id_matches_primary_and_room_members() {
        let mut card = create_test_card("T-1", "doing", Some("agent-1".to_string()));
        card.agent_chat_id = Some("primary-chat".to_string());
        card.team_members = vec![
            crate::tasks::types::TeamMember {
                role: "coder".to_string(),
                agent_chat_id: Some("primary-chat".to_string()),
                ..Default::default()
            },
            crate::tasks::types::TeamMember {
                role: "reviewer".to_string(),
                agent_chat_id: Some("reviewer-chat".to_string()),
                ..Default::default()
            },
        ];

        assert!(card_accepts_agent_chat_id(&card, "primary-chat"));
        assert!(card_accepts_agent_chat_id(&card, "reviewer-chat"));
        assert!(!card_accepts_agent_chat_id(&card, "stranger-chat"));
    }

    #[tokio::test]
    async fn room_member_idle_stall_triggers_notification() {
        let (_temp, app, task_id, _agent_chat_id, _agent_arc, _planner_arc) = setup_monitor_case(
            "doing",
            SessionState::Generating,
            Duration::from_secs(1),
            vec![],
        )
        .await;
        let room_member_chat_id = "agent-T-1-reviewer".to_string();
        {
            storage::update_board_atomic(app.gcx.clone(), &task_id, |board| {
                if let Some(card) = board.get_card_mut("T-1") {
                    card.team_members = vec![
                        crate::tasks::types::TeamMember {
                            role: "coder".to_string(),
                            agent_chat_id: Some("agent-T-1".to_string()),
                            member_status: Some(crate::tasks::types::TeamStatus::Running),
                            ..Default::default()
                        },
                        crate::tasks::types::TeamMember {
                            role: "reviewer".to_string(),
                            agent_chat_id: Some(room_member_chat_id.clone()),
                            member_status: Some(crate::tasks::types::TeamStatus::Running),
                            ..Default::default()
                        },
                    ];
                }
                Ok(())
            })
            .await
            .unwrap();
        }
        let member_session = make_test_agent_session(
            &task_id,
            "T-1",
            "agent-2",
            &room_member_chat_id,
            SessionState::Idle,
            AGENT_STUCK_TIMEOUT + Duration::from_secs(10),
        );
        let member_arc = Arc::new(tokio::sync::Mutex::new(member_session));
        app.chat
            .sessions
            .write()
            .await
            .insert(room_member_chat_id.clone(), member_arc.clone());

        check_for_stuck_agents(app.clone()).await.unwrap();

        let board = storage::load_board(app.gcx.clone(), &task_id)
            .await
            .unwrap();
        let card = board.get_card("T-1").unwrap();
        assert!(
            card.status_updates
                .iter()
                .any(|u| u.message.starts_with(STALL_AGENT_NOTIFY_STATUS_PREFIX)
                    && u.message.contains(&room_member_chat_id)),
            "room member stall must be recorded on the card; got {:?}",
            card.status_updates
        );
    }

    #[tokio::test]
    async fn planner_wake_up_message_contains_agent_status_snapshot() {
        let past = Utc::now() - chrono::Duration::seconds(10);
        let (_temp, app, _task_id, planner_arc) =
            setup_planner_wake_case(SessionState::WaitingUserInput, Some(past), true).await;

        check_for_stuck_agents(app.clone()).await.unwrap();

        let session = planner_arc.lock().await;
        assert_eq!(session.messages.len(), 1);
        assert!(session.command_queue.is_empty());
        let text = session.messages.last().unwrap().content.content_text_only();
        assert!(text.contains("T-1"), "message should contain card T-1");
    }