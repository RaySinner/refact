pub use refact_tools::tools::tool_agent_planner_qna::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
use refact_tasks::types::StatusUpdate;
#[cfg(test)]
use crate::tools::tools_description::Tool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus};
    use crate::tools::settings_guard::SettingsGuard;
    use crate::tools::tools_description::Tool;
    use serial_test::serial;

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn test_card(id: &str, title: &str, status_updates: Vec<StatusUpdate>) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: title.to_string(),
            column: "doing".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: Some("agent-1".to_string()),
            agent_chat_id: Some(format!("agent-chat-{}", id)),
            retry_count: 0,
            status_updates,
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: Utc::now().to_rfc3339(),
            started_at: Some(Utc::now().to_rfc3339()),
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

    fn task_meta() -> TaskMeta {
        let now = Utc::now().to_rfc3339();
        TaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        }
    }

    async fn write_task(
        root: &std::path::Path,
        cards: Vec<BoardCard>,
    ) -> Arc<crate::global_context::GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        storage::save_task_meta(gcx.clone(), "task-1", &task_meta())
            .await
            .unwrap();
        storage::save_board(
            gcx.clone(),
            "task-1",
            &TaskBoard {
                cards,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        gcx
    }

    async fn task_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
        role: &str,
        card_id: Option<&str>,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                format!("{}-chat", role),
                None,
                "model".to_string(),
                Some(ThreadTaskMeta {
                    task_id: "task-1".to_string(),
                    role: role.to_string(),
                    agent_id: Some("agent-1".to_string()),
                    card_id: card_id.map(str::to_string),
                    planner_chat_id: Some("planner-chat".to_string()),
                }),
                None,
            )
            .await,
        ))
    }

    fn output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    #[tokio::test]
    async fn agent_ask_planner_rejects_non_agent_role() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-40", "QnA", vec![])]).await;
        let ccx = task_ccx(gcx, "planner", None).await;

        let err = ToolAgentAskPlanner::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("question", json!("What should I do?")),
                    ("urgency", json!("info")),
                ]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by task agents"));
    }

    #[test]
    fn question_id_format_8_hex() {
        let question_id = make_question_id();

        assert_eq!(question_id.len(), 8);
        assert!(question_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    }

    #[tokio::test]
    async fn planner_reply_rejects_non_planner_role() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card(
                "T-40",
                "QnA",
                vec![StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: "[ASK:1234abcd] Need direction (urgency=info)".to_string(),
                }],
            )],
        )
        .await;
        let ccx = task_ccx(gcx, "agents", Some("T-40")).await;

        let err = ToolPlannerReply::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("card_id", json!("T-40")),
                    ("question_id", json!("1234abcd")),
                    ("answer", json!("Use the smaller fix.")),
                ]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("planner_reply can only be called by the task planner"));
    }

    #[tokio::test]
    async fn planner_reply_rejects_unknown_question_id() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-40", "QnA", vec![])]).await;
        let ccx = task_ccx(gcx, "planner", None).await;

        let err = ToolPlannerReply::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[
                    ("card_id", json!("T-40")),
                    ("question_id", json!("1234abcd")),
                    ("answer", json!("Use the smaller fix.")),
                ]),
            )
            .await
            .unwrap_err();

        assert_eq!(err, "Question 1234abcd not found on card T-40");
    }

    #[test]
    fn task_questions_list_filters_unanswered_correctly() {
        let cards = vec![
            test_card(
                "T-1",
                "first card",
                vec![
                    StatusUpdate {
                        timestamp: Utc::now().to_rfc3339(),
                        message: "[ASK:aaaaaaaa] Answered question (urgency=info)".to_string(),
                    },
                    StatusUpdate {
                        timestamp: Utc::now().to_rfc3339(),
                        message: "[REPLY:aaaaaaaa] Answer".to_string(),
                    },
                    StatusUpdate {
                        timestamp: Utc::now().to_rfc3339(),
                        message: "[ASK:bbbbbbbb] Blocking question (urgency=info)".to_string(),
                    },
                    StatusUpdate {
                        timestamp: Utc::now().to_rfc3339(),
                        message: "[ASK:bbbbbbbb:block] agent flagged for planner attention"
                            .to_string(),
                    },
                ],
            ),
            test_card(
                "T-2",
                "second card",
                vec![StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: "[ASK:cccccccc] Open question (urgency=info)".to_string(),
                }],
            ),
        ];

        let questions = collect_unanswered_questions(&cards);
        let output = format_unanswered_questions(&questions);

        assert_eq!(questions.len(), 2);
        assert!(!output.contains("aaaaaaaa"));
        assert!(output.contains("bbbbbbbb"));
        assert!(output.contains("cccccccc"));
        assert!(output.contains("| block | Blocking question |"));
        assert!(output.contains("| info | Open question |"));
    }

    #[tokio::test]
    async fn agent_ask_planner_records_question_and_block_marker() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-40", "QnA", vec![])]).await;
        let ccx = task_ccx(gcx.clone(), "agents", Some("T-40")).await;

        let output = output_text(
            ToolAgentAskPlanner::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[
                        ("question", json!("Should I choose implementation A or B?")),
                        ("urgency", json!("block")),
                    ]),
                )
                .await
                .unwrap(),
        );

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-40").unwrap();
        // Two [ASK:...] updates plus a [NOTIFY-FAILED:...] trace: the fixture's
        // planner_chat_id ("planner-chat") is not a known planner trajectory, so
        // the blocking wake-up records its failure durably on the card.
        assert_eq!(card.status_updates.len(), 3);
        assert!(card.status_updates[0].message.starts_with("[ASK:"));
        assert!(card.status_updates[0]
            .message
            .contains("Should I choose implementation A or B? (urgency=block)"));
        assert!(card.status_updates[1]
            .message
            .ends_with(":block] agent flagged for planner attention"));
        assert!(card.status_updates[2]
            .message
            .starts_with("[NOTIFY-FAILED:blocking_question]"));
        assert!(output.contains("question_id"));
        assert!(output.contains("Planner wake-up failed"));
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn planner_qna_question_limit_setting_truncates_and_is_loud() {
        let _guard = SettingsGuard::install(|settings| {
            settings.planner_qna_question_limit = 32;
        });
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-41", "QnA", vec![])]).await;
        let ccx = task_ccx(gcx.clone(), "agents", Some("T-41")).await;
        let long_question = "q".repeat(200);

        let output = output_text(
            ToolAgentAskPlanner::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[
                        ("question", json!(long_question)),
                        ("urgency", json!("info")),
                    ]),
                )
                .await
                .unwrap(),
        );

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-41").unwrap();
        let recorded = &card.status_updates[0].message;
        assert!(
            recorded.matches('q').count() == 32,
            "expected the question truncated to 32 chars, got: {}",
            recorded
        );
        assert!(
            output.contains("showing 32 of 200 question characters")
                && output.contains("planner_qna_question_limit = 32"),
            "expected a quantified question-truncation notice, got: {}",
            output
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn planner_qna_answer_limit_setting_truncates_and_is_loud() {
        let _guard = SettingsGuard::install(|settings| {
            settings.planner_qna_answer_limit = 16;
        });
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card(
                "T-42",
                "QnA",
                vec![StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: "[ASK:abcdabcd] Which one? (urgency=info)".to_string(),
                }],
            )],
        )
        .await;
        let ccx = task_ccx(gcx.clone(), "planner", Some("T-42")).await;
        let long_answer = "a".repeat(120);

        let output = output_text(
            ToolPlannerReply::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[
                        ("card_id", json!("T-42")),
                        ("question_id", json!("abcdabcd")),
                        ("answer", json!(long_answer)),
                    ]),
                )
                .await
                .unwrap(),
        );

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-42").unwrap();
        let recorded = card
            .status_updates
            .iter()
            .find(|update| update.message.starts_with("[REPLY:"))
            .expect("reply update");
        assert!(recorded.message.ends_with(&"a".repeat(16)));
        assert!(!recorded.message.ends_with(&"a".repeat(17)));
        assert!(
            output.contains("showing 16 of 120 answer characters")
                && output.contains("planner_qna_answer_limit = 16"),
            "expected a quantified answer-truncation notice, got: {}",
            output
        );
    }

    #[test]
    fn qna_truncation_notice_quantifies_shown_and_total() {
        let notice = qna_truncation_notice("question", 800, 2_048, "planner_qna_question_limit");
        assert!(
            notice.contains("showing 800 of 2048 question characters"),
            "{}",
            notice
        );
        assert!(
            notice.contains("planner_qna_question_limit = 800"),
            "{}",
            notice
        );
    }
    #[tokio::test]
    async fn planner_reply_records_once_and_delivers_live_with_each_push_mode() {
        use refact_chat_api::types::{ChatSession, SessionState, PushMode};
        for (raw, expected) in [
            (None, PushMode::Append),
            (Some("append"), PushMode::Append),
            (Some("preempt"), PushMode::Preempt),
            (Some("when_idle"), PushMode::WhenIdle),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let gcx = write_task(
                temp.path(),
                vec![test_card(
                    "T-1",
                    "QnA",
                    vec![StatusUpdate {
                        timestamp: Utc::now().to_rfc3339(),
                        message: "[ASK:1234abcd] Help (urgency=block)".into(),
                    }],
                )],
            )
            .await;
            let ccx = task_ccx(gcx.clone(), "planner", None).await;
            let mut session = ChatSession::new("agent-chat-T-1".into());
            session.runtime.state = SessionState::Generating;
            session.draft_message = Some(ChatMessage {
                role: "assistant".into(),
                content: ChatContent::SimpleText("partial answer".into()),
                ..Default::default()
            });
            session
                .queue_processor_running
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let session = Arc::new(AMutex::new(session));
            ccx.lock()
                .await
                .app
                .chat
                .sessions
                .write()
                .await
                .insert("agent-chat-T-1".into(), session.clone());
            let mut arguments = args(&[
                ("card_id", json!("T-1")),
                ("question_id", json!("1234abcd")),
                ("answer", json!("Proceed")),
            ]);
            if let Some(raw) = raw {
                arguments.insert("push".into(), json!(raw));
            }
            let output = output_text(
                ToolPlannerReply::new()
                    .tool_execute(ccx.clone(), &"call".into(), &arguments)
                    .await
                    .unwrap(),
            );
            assert!(output.contains("Reply recorded"));
            {
                let session = session.lock().await;
                if expected == PushMode::Preempt {
                    assert!(output.contains("message delivered"));
                    assert!(session
                        .messages
                        .iter()
                        .any(|m| m.content.content_text_only()
                            == "[Planner REPLY to 1234abcd] Proceed"));
                } else {
                    assert!(output.contains("message queued"));
                    assert_eq!(session.pending_deliveries.len(), 1);
                    let pending = session.pending_deliveries.front().unwrap();
                    assert_eq!(pending.push, expected);
                    assert_eq!(pending.id, "planner-reply:task-1:T-1:1234abcd");
                }
            }
            let duplicate = output_text(
                ToolPlannerReply::new()
                    .tool_execute(ccx, &"retry".into(), &arguments)
                    .await
                    .unwrap(),
            );
            assert!(duplicate.contains("duplicate answer not delivered"));
            let board = storage::load_board(gcx, "task-1").await.unwrap();
            assert_eq!(
                board
                    .get_card("T-1")
                    .unwrap()
                    .status_updates
                    .iter()
                    .filter(|u| u.message.starts_with("[REPLY:"))
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn planner_reply_records_but_does_not_claim_delivery_without_active_card_chat() {
        for (column, chat) in [
            ("doing", None),
            ("done", Some("agent-chat-T-1")),
            ("doing", Some("missing-session")),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut card = test_card(
                "T-1",
                "QnA",
                vec![StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: "[ASK:1234abcd] Help".into(),
                }],
            );
            card.column = column.into();
            card.agent_chat_id = chat.map(str::to_string);
            let gcx = write_task(temp.path(), vec![card]).await;
            let ccx = task_ccx(gcx, "planner", None).await;
            let output = output_text(
                ToolPlannerReply::new()
                    .tool_execute(
                        ccx,
                        &"call".into(),
                        &args(&[
                            ("card_id", json!("T-1")),
                            ("question_id", json!("1234abcd")),
                            ("answer", json!("Proceed")),
                        ]),
                    )
                    .await
                    .unwrap(),
            );
            assert!(output.contains("Reply recorded"));
            assert!(output.to_lowercase().contains("not delivered"));
        }
    }

    #[tokio::test]
    async fn info_question_is_board_only_even_with_explicit_preempt() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-1", "QnA", vec![])]).await;
        let ccx = task_ccx(gcx.clone(), "agents", Some("T-1")).await;
        ToolAgentAskPlanner::new()
            .tool_execute(
                ccx.clone(),
                &"call".into(),
                &args(&[
                    ("question", json!("Help")),
                    ("urgency", json!("info")),
                    ("push", json!("preempt")),
                ]),
            )
            .await
            .unwrap();
        assert!(ccx.lock().await.app.chat.sessions.read().await.is_empty());
        assert_eq!(
            storage::load_board(gcx, "task-1")
                .await
                .unwrap()
                .get_card("T-1")
                .unwrap()
                .status_updates
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn blocking_question_defaults_to_append_and_propagates_explicit_modes() {
        use refact_chat_api::types::{ChatSession, SessionState, PushMode};
        for (raw, expected) in [
            (None, PushMode::Append),
            (Some("preempt"), PushMode::Preempt),
            (Some("when_idle"), PushMode::WhenIdle),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let gcx = write_task(temp.path(), vec![test_card("T-1", "QnA", vec![])]).await;
            let ccx = task_ccx(gcx, "agents", Some("T-1")).await;
            let mut planner = ChatSession::new("planner-chat".into());
            planner.runtime.state = SessionState::Generating;
            planner.draft_message = Some(ChatMessage {
                role: "assistant".into(),
                content: ChatContent::SimpleText("partial answer".into()),
                ..Default::default()
            });
            planner.thread.task_meta = Some(ThreadTaskMeta {
                task_id: "task-1".into(),
                role: "planner".into(),
                agent_id: None,
                card_id: None,
                planner_chat_id: None,
            });
            planner
                .queue_processor_running
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let planner = Arc::new(AMutex::new(planner));
            ccx.lock()
                .await
                .app
                .chat
                .sessions
                .write()
                .await
                .insert("planner-chat".into(), planner.clone());
            let mut arguments = args(&[("question", json!("Help")), ("urgency", json!("block"))]);
            if let Some(raw) = raw {
                arguments.insert("push".into(), json!(raw));
            }
            let output = output_text(
                ToolAgentAskPlanner::new()
                    .tool_execute(ccx, &"call".into(), &arguments)
                    .await
                    .unwrap(),
            );
            let planner = planner.lock().await;
            assert!(planner.command_queue.is_empty());
            if expected == PushMode::Preempt {
                assert!(output.contains("question delivered"));
            } else {
                assert!(output.contains("question queued"));
                assert_eq!(planner.pending_deliveries.front().unwrap().push, expected);
                assert!(!planner.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
            }
        }
    }
    #[tokio::test]
    async fn planner_reply_default_append_lands_on_idle_live_session() {
        use refact_chat_api::ChatSession;
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card(
                "T-1",
                "QnA",
                vec![StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: "[ASK:1234abcd] Help".into(),
                }],
            )],
        )
        .await;
        let ccx = task_ccx(gcx, "planner", None).await;
        let session = ChatSession::new("agent-chat-T-1".into());
        session
            .queue_processor_running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let session = Arc::new(AMutex::new(session));
        ccx.lock()
            .await
            .app
            .chat
            .sessions
            .write()
            .await
            .insert("agent-chat-T-1".into(), session.clone());
        let output = output_text(
            ToolPlannerReply::new()
                .tool_execute(
                    ccx,
                    &"call".into(),
                    &args(&[
                        ("card_id", json!("T-1")),
                        ("question_id", json!("1234abcd")),
                        ("answer", json!("Proceed")),
                    ]),
                )
                .await
                .unwrap(),
        );
        assert!(output.contains("Reply message delivered"));
        let session = session.lock().await;
        assert!(session.command_queue.is_empty());
        assert!(session.pending_deliveries.is_empty());
        assert_eq!(
            session.messages.last().unwrap().content.content_text_only(),
            "[Planner REPLY to 1234abcd] Proceed"
        );
    }
    #[tokio::test]
    async fn planner_reply_to_idle_agent_is_delivered_and_missing_card_is_rejected() {
        use refact_chat_api::ChatSession;
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(
            temp.path(),
            vec![test_card(
                "T-1",
                "QnA",
                vec![StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: "[ASK:1234abcd] Help".into(),
                }],
            )],
        )
        .await;
        let ccx = task_ccx(gcx.clone(), "planner", None).await;
        let agent = ChatSession::new("agent-chat-T-1".into());
        agent
            .queue_processor_running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let agent = Arc::new(AMutex::new(agent));
        ccx.lock()
            .await
            .app
            .chat
            .sessions
            .write()
            .await
            .insert("agent-chat-T-1".into(), agent.clone());
        let mut arguments = args(&[
            ("card_id", json!("T-1")),
            ("question_id", json!("1234abcd")),
            ("answer", json!("Proceed")),
        ]);
        let output = output_text(
            ToolPlannerReply::new()
                .tool_execute(ccx.clone(), &"call".into(), &arguments)
                .await
                .unwrap(),
        );
        assert!(output.contains("Reply message delivered"));
        {
            let agent = agent.lock().await;
            assert!(agent.pending_deliveries.is_empty());
            assert_eq!(
                agent.messages.last().unwrap().content.content_text_only(),
                "[Planner REPLY to 1234abcd] Proceed"
            );
        }
        arguments.insert("card_id".into(), json!("missing"));
        let error = ToolPlannerReply::new()
            .tool_execute(ccx, &"call".into(), &arguments)
            .await
            .unwrap_err();
        assert_eq!(error, "Card missing not found");
    }
}
