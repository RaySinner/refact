use super::*;
use super::tests_common::*;
use crate::exec::types::DEFAULT_EXEC_OUTPUT_LIMIT_BYTES;
use crate::exec::{ExecMode, ExecOwnerMeta, ExecProcessId, ExecProcessMeta};
use crate::yaml_configs::customization_types::{ModeOverride, ModeThreadDefaults, ProjectRegistry};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[test]
fn test_find_allowed_command_while_waiting_ide_empty_queue() {
    let queue = VecDeque::new();
    assert!(find_allowed_command_while_waiting_ide(&queue).is_none());
}

#[test]
fn test_find_allowed_command_while_waiting_ide_no_allowed() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::UserMessage {
        content: json!("hi"),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: None,
        push: None,
    }));
    queue.push_back(make_request(ChatCommand::ToolDecision {
        tool_call_id: "tc1".into(),
        accepted: true,
    }));
    assert!(find_allowed_command_while_waiting_ide(&queue).is_none());
}

#[test]
fn test_find_allowed_command_while_waiting_ide_finds_ide_tool_result() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::UserMessage {
        content: json!("hi"),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: None,
        push: None,
    }));
    queue.push_back(make_request(ChatCommand::IdeToolResult {
        tool_call_id: "tc1".into(),
        content: "result".into(),
        tool_failed: false,
    }));
    assert_eq!(find_allowed_command_while_waiting_ide(&queue), Some(1));
}

#[test]
fn test_find_allowed_command_while_waiting_ide_finds_abort() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::UserMessage {
        content: json!("hi"),
        attachments: vec![],
        context_files: vec![],
        suppress_auto_enrichment: false,
        client_message_id: None,
        push: None,
    }));
    queue.push_back(make_request(ChatCommand::Abort {}));
    assert_eq!(find_allowed_command_while_waiting_ide(&queue), Some(1));
}

#[test]
fn test_find_allowed_command_while_waiting_ide_returns_first_match() {
    let mut queue = VecDeque::new();
    queue.push_back(make_request(ChatCommand::Abort {}));
    queue.push_back(make_request(ChatCommand::IdeToolResult {
        tool_call_id: "tc1".into(),
        content: "result".into(),
        tool_failed: false,
    }));
    assert_eq!(find_allowed_command_while_waiting_ide(&queue), Some(0));
}

#[test]
fn test_priority_insertion_before_non_priority() {
    let mut queue = VecDeque::new();
    queue.push_back(CommandRequest {
        client_request_id: "req-1".into(),
        priority: false,
        command: ChatCommand::UserMessage {
            content: json!("first"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-2".into(),
        priority: false,
        command: ChatCommand::UserMessage {
            content: json!("second"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    let priority_req = CommandRequest {
        client_request_id: "req-priority".into(),
        priority: true,
        command: ChatCommand::UserMessage {
            content: json!("priority"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    };
    let insert_pos = queue
        .iter()
        .position(|r| !r.priority)
        .unwrap_or(queue.len());
    queue.insert(insert_pos, priority_req);
    assert_eq!(queue[0].client_request_id, "req-priority");
    assert_eq!(queue[1].client_request_id, "req-1");
    assert_eq!(queue[2].client_request_id, "req-2");
}

#[test]
fn test_priority_insertion_after_existing_priority() {
    let mut queue = VecDeque::new();
    queue.push_back(CommandRequest {
        client_request_id: "req-p1".into(),
        priority: true,
        command: ChatCommand::UserMessage {
            content: json!("p1"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-1".into(),
        priority: false,
        command: ChatCommand::UserMessage {
            content: json!("normal"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    let priority_req = CommandRequest {
        client_request_id: "req-p2".into(),
        priority: true,
        command: ChatCommand::UserMessage {
            content: json!("p2"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    };
    let insert_pos = queue
        .iter()
        .position(|r| !r.priority)
        .unwrap_or(queue.len());
    queue.insert(insert_pos, priority_req);
    assert_eq!(queue[0].client_request_id, "req-p1");
    assert_eq!(queue[1].client_request_id, "req-p2");
    assert_eq!(queue[2].client_request_id, "req-1");
}

#[test]
fn test_priority_insertion_into_empty_queue() {
    let mut queue: VecDeque<CommandRequest> = VecDeque::new();
    let priority_req = CommandRequest {
        client_request_id: "req-p".into(),
        priority: true,
        command: ChatCommand::Abort {},
    };
    let insert_pos = queue
        .iter()
        .position(|r| !r.priority)
        .unwrap_or(queue.len());
    queue.insert(insert_pos, priority_req);
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].client_request_id, "req-p");
}

#[test]
fn test_priority_insertion_all_priority() {
    let mut queue = VecDeque::new();
    queue.push_back(CommandRequest {
        client_request_id: "req-p1".into(),
        priority: true,
        command: ChatCommand::Abort {},
    });
    let priority_req = CommandRequest {
        client_request_id: "req-p2".into(),
        priority: true,
        command: ChatCommand::Abort {},
    };
    let insert_pos = queue
        .iter()
        .position(|r| !r.priority)
        .unwrap_or(queue.len());
    queue.insert(insert_pos, priority_req);
    assert_eq!(queue[0].client_request_id, "req-p1");
    assert_eq!(queue[1].client_request_id, "req-p2");
}

#[test]
fn test_drain_priority_user_messages_extracts_only_priority() {
    let mut queue = VecDeque::new();
    queue.push_back(CommandRequest {
        client_request_id: "req-p1".into(),
        priority: true,
        command: ChatCommand::UserMessage {
            content: json!("priority 1"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-1".into(),
        priority: false,
        command: ChatCommand::UserMessage {
            content: json!("normal"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-p2".into(),
        priority: true,
        command: ChatCommand::UserMessage {
            content: json!("priority 2"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-abort".into(),
        priority: true,
        command: ChatCommand::Abort {},
    });

    let drained = drain_priority_user_messages(&mut queue);
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].client_request_id, "req-p1");
    assert_eq!(drained[1].client_request_id, "req-p2");
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0].client_request_id, "req-1");
    assert_eq!(queue[1].client_request_id, "req-abort");
}

#[tokio::test]
async fn priority_user_message_resets_goal_no_progress() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new("priority-reset".to_string())));
    {
        let mut session = session_arc.lock().await;
        session.thread.checkpoints_enabled = false;
        session.install_goal("agent", "ship it", true, GoalBudget::default());
        session.goal.as_mut().unwrap().progress.no_progress_turns = 3;
        session.command_queue.push_back(CommandRequest {
            client_request_id: "priority-user".to_string(),
            priority: true,
            command: ChatCommand::UserMessage {
                content: json!("resume pursuit"),
                attachments: vec![],
                context_files: vec![],
                suppress_auto_enrichment: false,
                client_message_id: None,
                push: None,
            },
        });
    }

    assert!(inject_priority_messages_if_any(app, session_arc.clone()).await);

    let session = session_arc.lock().await;
    assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 0);
    assert!(session.messages.iter().any(|message| {
        message.role == "user"
            && message
                .content
                .content_text_only()
                .contains("resume pursuit")
    }));
}

#[tokio::test]
async fn priority_user_message_preserves_client_message_id_and_server_message_id() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "priority-client-message-id".to_string(),
    )));
    {
        let mut session = session_arc.lock().await;
        session.thread.checkpoints_enabled = false;
        session.command_queue.push_back(CommandRequest {
            client_request_id: "priority-user".to_string(),
            priority: true,
            command: user_message_command("same content", Some("optimistic-priority")),
        });
    }

    assert!(inject_priority_messages_if_any(app, session_arc.clone()).await);

    let session = session_arc.lock().await;
    let message = session
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("priority user message");
    assert_eq!(
        message.extra.get(CLIENT_MESSAGE_ID_EXTRA_KEY),
        Some(&json!("optimistic-priority"))
    );
    assert_ne!(message.message_id, "optimistic-priority");
    assert!(uuid::Uuid::parse_str(&message.message_id).is_ok());
}

#[tokio::test]
async fn ordinary_batched_user_messages_preserve_distinct_client_message_ids() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "ordinary-client-message-ids".to_string(),
    )));
    {
        let mut session = session_arc.lock().await;
        session.thread.checkpoints_enabled = false;
        session.command_queue.push_back(CommandRequest {
            client_request_id: "ordinary-first".to_string(),
            priority: false,
            command: user_message_command("same content", Some("optimistic-1")),
        });
        session.command_queue.push_back(CommandRequest {
            client_request_id: "ordinary-second".to_string(),
            priority: false,
            command: user_message_command("same content", Some("optimistic-2")),
        });
    }
    let processor_running = session_arc.lock().await.queue_processor_running.clone();
    processor_running.store(true, Ordering::SeqCst);
    let handle = tokio::spawn(process_command_queue(
        app,
        session_arc.clone(),
        processor_running,
    ));

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if session_arc
                .lock()
                .await
                .messages
                .iter()
                .filter(|message| message.role == "user")
                .count()
                == 2
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ordinary user messages were not added");

    {
        let session = session_arc.lock().await;
        let messages = session
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].extra.get(CLIENT_MESSAGE_ID_EXTRA_KEY),
            Some(&json!("optimistic-1"))
        );
        assert_eq!(
            messages[1].extra.get(CLIENT_MESSAGE_ID_EXTRA_KEY),
            Some(&json!("optimistic-2"))
        );
        assert_ne!(messages[0].message_id, messages[1].message_id);
        assert!(uuid::Uuid::parse_str(&messages[0].message_id).is_ok());
        assert!(uuid::Uuid::parse_str(&messages[1].message_id).is_ok());
    }
    close_processor(&session_arc, handle).await;
}

#[tokio::test]
async fn priority_user_message_suppresses_auto_enrichment_without_context_files() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "priority-suppress-enrichment".to_string(),
    )));
    {
        let mut session = session_arc.lock().await;
        session.thread.checkpoints_enabled = false;
        session.command_queue.push_back(CommandRequest {
            client_request_id: "priority-user".to_string(),
            priority: true,
            command: ChatCommand::UserMessage {
                content: json!("skip enrichment"),
                attachments: vec![],
                context_files: vec![],
                suppress_auto_enrichment: true,
                client_message_id: None,
                push: None,
            },
        });
    }

    assert!(inject_priority_messages_if_any(app, session_arc.clone()).await);

    let session = session_arc.lock().await;
    assert!(session.suppress_auto_enrichment_for_next_turn);
    assert!(session.messages.iter().any(|message| {
        message.role == "user"
            && message
                .content
                .content_text_only()
                .contains("skip enrichment")
    }));
}

#[tokio::test]
async fn promoted_multimodal_message_reaches_priority_injection_unchanged() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "priority-multimodal".to_string(),
    )));
    let image_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIW2Nk+M/wHwAF/gL+3MxZ5wAAAABJRU5ErkJggg==";
    {
        let mut session = session_arc.lock().await;
        session.thread.checkpoints_enabled = false;
        session.command_queue.push_back(CommandRequest {
            client_request_id: "multimodal-user".to_string(),
            priority: false,
            command: ChatCommand::UserMessage {
                content: json!([
                    {"type": "text", "text": "describe this"},
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{image_data}")}}
                ]),
                attachments: vec![],
                context_files: vec![],
                suppress_auto_enrichment: false,
                client_message_id: None,
                push: None,
            },
        });
        assert!(session.reprioritize_queued_command("multimodal-user", true));
    }

    assert!(inject_priority_messages_if_any(app, session_arc.clone()).await);

    let session = session_arc.lock().await;
    let user_message = session
        .messages
        .iter()
        .find(|message| message.role == "user")
        .expect("promoted user message");
    let ChatContent::Multimodal(elements) = &user_message.content else {
        panic!("expected multimodal user message");
    };
    assert_eq!(elements[0].m_type, "text");
    assert_eq!(elements[0].m_content, "describe this");
    assert_eq!(elements[1].m_type, "image/png");
    assert_eq!(elements[1].m_content, image_data);
}

#[test]
fn test_drain_non_priority_user_messages_extracts_all_non_priority() {
    let mut queue = VecDeque::new();
    queue.push_back(CommandRequest {
        client_request_id: "req-1".into(),
        priority: false,
        command: ChatCommand::UserMessage {
            content: json!("first"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-p".into(),
        priority: true,
        command: ChatCommand::UserMessage {
            content: json!("priority"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-2".into(),
        priority: false,
        command: ChatCommand::UserMessage {
            content: json!("second"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-3".into(),
        priority: false,
        command: ChatCommand::UserMessage {
            content: json!("third"),
            attachments: vec![],
            context_files: vec![],
            suppress_auto_enrichment: false,
            client_message_id: None,
            push: None,
        },
    });

    let drained = drain_non_priority_user_messages(&mut queue);
    assert_eq!(drained.len(), 3);
    assert_eq!(drained[0].client_request_id, "req-1");
    assert_eq!(drained[1].client_request_id, "req-2");
    assert_eq!(drained[2].client_request_id, "req-3");
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].client_request_id, "req-p");
}

#[test]
fn test_drain_priority_skips_non_user_messages() {
    let mut queue = VecDeque::new();
    queue.push_back(CommandRequest {
        client_request_id: "req-abort".into(),
        priority: true,
        command: ChatCommand::Abort {},
    });
    queue.push_back(CommandRequest {
        client_request_id: "req-params".into(),
        priority: true,
        command: ChatCommand::SetParams { patch: json!({}) },
    });

    let drained = drain_priority_user_messages(&mut queue);
    assert!(drained.is_empty());
    assert_eq!(queue.len(), 2);
}

#[test]
fn test_drain_empty_queue() {
    let mut queue: VecDeque<CommandRequest> = VecDeque::new();
    let priority_drained = drain_priority_user_messages(&mut queue);
    let non_priority_drained = drain_non_priority_user_messages(&mut queue);
    assert!(priority_drained.is_empty());
    assert!(non_priority_drained.is_empty());
}

#[test]
fn test_model_switch_clears_previous_response_id() {
    let mut thread = ThreadParams::default();
    thread.model = "openai/gpt-4".into();
    thread.previous_response_id = Some("resp_abc123".to_string());

    let patch = json!({"model": "anthropic/claude-3"});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);

    assert!(changed);
    assert_eq!(thread.model, "anthropic/claude-3");
    assert_eq!(
        thread.previous_response_id, None,
        "previous_response_id must be cleared on model switch"
    );
}

#[test]
fn test_same_model_preserves_previous_response_id() {
    let mut thread = ThreadParams::default();
    thread.model = "openai/gpt-4".into();
    thread.previous_response_id = Some("resp_abc123".to_string());

    let patch = json!({"model": "openai/gpt-4"});
    let (changed, _) = apply_setparams_patch(&mut thread, &patch);

    assert!(!changed);
    assert_eq!(
        thread.previous_response_id,
        Some("resp_abc123".to_string()),
        "previous_response_id should be preserved when model doesn't change"
    );
}

#[test]
fn test_skill_activation_sets_context_marker() {
    assert_eq!(SKILLS_CONTEXT_MARKER, "skills_context",
        "SKILLS_CONTEXT_MARKER must equal 'skills_context' so prompts.rs detects existing skills context");
}

#[test]
fn goal_budget_set_goal_command_installs_snapshot_and_runtime() {
    let mut session = ChatSession::new("goal-command".to_string());
    let mut rx = session.subscribe();

    let result =
        handle_set_goal_command(&mut session, "Ship the pond".to_string(), None, None).unwrap();

    assert_eq!(result, json!({"version": 1, "supersedes": null}));
    let goal = session.goal.as_ref().unwrap();
    assert_eq!(goal.content, "Ship the pond");
    assert!(goal.active);
    assert_eq!(goal.status, GoalStatus::Active);
    assert_eq!(goal.budget, GoalBudget::default());
    assert!(session.messages[0].extra["goal"]["budget"]
        .get("max_turns")
        .is_none());
    assert_eq!(goal.progress.turns_used, 0);
    assert_eq!(session.goal_status, Some(GoalStatus::Active));
    assert!(drain_events(&mut rx).into_iter().any(|event| matches!(
        event,
        ChatEvent::RuntimeUpdated {
            goal_active: true,
            goal_status: Some(GoalStatus::Active),
            ..
        }
    )));
}

#[test]
fn goal_budget_set_goal_command_installs_explicit_budget() {
    let mut session = ChatSession::new("goal-command".to_string());
    let budget = GoalBudget {
        max_turns: Some(3),
        max_minutes: Some(4),
        max_tokens: Some(5),
        max_cost_cents: None,
        cooldown_ms: 1_500,
        no_progress_token_threshold: 50,
        no_progress_turns: Some(6),
        explicit: false,
    };

    handle_set_goal_command(
        &mut session,
        "Ship the pond".to_string(),
        Some(budget.clone()),
        None,
    )
    .unwrap();

    let expected = GoalBudget {
        explicit: true,
        ..budget
    };
    assert_eq!(session.goal.as_ref().unwrap().budget, expected);
    assert_eq!(
        session.messages[0].extra["goal"]["budget"]["max_turns"],
        json!(3)
    );
    assert_eq!(
        session.messages[0].extra["goal"]["budget"]["explicit"],
        json!(true)
    );
}

#[test]
fn goal_budget_set_goal_command_rejects_when_exists() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "First".to_string(), None, None).unwrap();

    let error =
        handle_set_goal_command(&mut session, "Second".to_string(), None, None).unwrap_err();

    assert_eq!(error, "goal already exists; use update_goal");
    assert_eq!(session.goal.as_ref().unwrap().content, "First");
}

#[test]
fn goal_budget_set_goal_budget_command_replaces_budget_and_projection() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();
    let budget = GoalBudget {
        max_turns: Some(2),
        max_minutes: None,
        max_tokens: Some(1_000),
        max_cost_cents: None,
        cooldown_ms: 2_000,
        no_progress_token_threshold: 25,
        no_progress_turns: None,
        explicit: false,
    };

    let result = handle_set_goal_budget_command(&mut session, budget.clone()).unwrap();

    let expected = GoalBudget {
        explicit: true,
        ..budget
    };
    assert_eq!(result, json!({"budget": expected.clone()}));
    assert_eq!(session.goal.as_ref().unwrap().budget, expected);
    assert_eq!(session.messages[0].extra["goal"]["budget"], json!(expected));
    assert_eq!(session.goal_status, Some(GoalStatus::Active));
}

#[test]
fn goal_budget_set_goal_budget_legacy_quad_is_stamped_explicit() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();

    handle_set_goal_budget_command(&mut session, GoalBudget::legacy_default_hard_limits())
        .unwrap();

    let goal_meta = session.messages[0].extra.get_mut("goal").unwrap();
    assert_eq!(goal_meta["budget"]["explicit"], json!(true));
    goal_meta["status"] = json!(GoalStatus::BudgetExhausted);
    goal_meta["progress"] = json!(GoalProgress {
        turns_used: 10,
        ..Default::default()
    });

    let reloaded = ChatSession::new_with_trajectory(
        "goal-command".to_string(),
        session.messages.clone(),
        ThreadParams::default(),
        "2024-01-01T00:00:00Z".to_string(),
        None,
        Vec::new(),
        None,
    );

    let goal = reloaded.goal.expect("goal projection");
    assert_eq!(goal.budget.max_turns, Some(10));
    assert_eq!(goal.budget.max_minutes, Some(15));
    assert_eq!(goal.budget.max_tokens, Some(200_000));
    assert_eq!(goal.budget.no_progress_turns, Some(2));
    assert!(goal.budget.explicit);
    assert_eq!(goal.status, GoalStatus::BudgetExhausted);
}

#[test]
fn goal_budget_set_goal_budget_command_exhausts_when_already_over_limit() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();
    session.goal.as_mut().unwrap().progress.turns_used = 3;
    session.refresh_goal_runtime_mirror();
    let epoch = session.goal_ledger_last_seq();

    handle_set_goal_budget_command(
        &mut session,
        GoalBudget {
            max_turns: Some(2),
            max_minutes: None,
            max_tokens: None,
            max_cost_cents: None,
            cooldown_ms: 1_500,
            no_progress_token_threshold: 50,
            no_progress_turns: None,
            explicit: false,
        },
    )
    .unwrap();

    assert_eq!(
        session.goal.as_ref().unwrap().status,
        GoalStatus::BudgetExhausted
    );
    assert_eq!(session.goal_status, Some(GoalStatus::BudgetExhausted));
    assert!(session.goal_status_changed_since(epoch));
    assert!(matches!(
        session.goal_ledger.last().map(|entry| &entry.op),
        Some(GoalLedgerOp::StatusChanged {
            to: GoalStatus::BudgetExhausted,
            ..
        })
    ));
}

#[test]
fn goal_budget_set_goal_budget_command_clears_limits_and_heals_terminal_status() {
    let mut session = ChatSession::new("goal-command".to_string());
    let budget = GoalBudget {
        max_turns: Some(1),
        max_minutes: None,
        max_tokens: None,
        max_cost_cents: None,
        cooldown_ms: 1_500,
        no_progress_token_threshold: 50,
        no_progress_turns: None,
        explicit: false,
    };
    handle_set_goal_command(&mut session, "Base".to_string(), Some(budget), None).unwrap();
    session.goal.as_mut().unwrap().progress.turns_used = 1;
    session.goal_set_status(GoalStatus::BudgetExhausted);
    session.messages[0].extra["goal"]["progress"] =
        json!(session.goal.as_ref().unwrap().progress);
    session.messages[0].extra["goal"]["status"] = json!(GoalStatus::BudgetExhausted);

    handle_set_goal_budget_command(&mut session, GoalBudget::default()).unwrap();

    let goal = session.goal.as_ref().unwrap();
    let expected_budget = GoalBudget {
        explicit: true,
        ..GoalBudget::default()
    };
    assert_eq!(goal.budget, expected_budget);
    assert_eq!(goal.status, GoalStatus::Active);
    assert_eq!(session.goal_status, Some(GoalStatus::Active));
    assert!(session.messages[0].extra["goal"]["budget"]
        .get("max_turns")
        .is_none());
    assert_eq!(
        session.messages[0].extra["goal"]["budget"]["explicit"],
        json!(true)
    );
}

#[test]
fn goal_budget_set_goal_budget_command_rejects_without_goal() {
    let mut session = ChatSession::new("goal-command".to_string());

    let error =
        handle_set_goal_budget_command(&mut session, GoalBudget::default()).unwrap_err();

    assert_eq!(error, "no goal to set budget for; call set_goal first");
    assert!(session.goal.is_none());
}

#[test]
fn goal_budget_update_goal_command_appends_delta_and_emits_runtime() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();
    let mut rx = session.subscribe();

    let result = handle_update_goal_command(&mut session, "Add tests".to_string()).unwrap();

    assert_eq!(result, json!({"seq": 1, "truncated": false}));
    assert_eq!(crate::chat::goal_role::goal_delta_events(&session).len(), 1);
    assert_eq!(
        session.goal.as_ref().unwrap().content,
        "Base\n\n---\n\n## Goal updates\n\nAdd tests"
    );
    assert!(drain_events(&mut rx).into_iter().any(|event| matches!(
        event,
        ChatEvent::RuntimeUpdated {
            goal_active: true,
            goal_status: Some(GoalStatus::Active),
            ..
        }
    )));
}

#[test]
fn goal_budget_goal_control_command_transitions_status() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();

    let paused = handle_goal_control_command(&mut session, "pause".to_string()).unwrap();
    assert_eq!(paused, json!({"action": "pause", "status": "paused"}));
    assert_eq!(session.goal_status, Some(GoalStatus::Paused));
    assert!(!session.goal_can_pursue());

    let resumed = handle_goal_control_command(&mut session, "resume".to_string()).unwrap();
    assert_eq!(resumed, json!({"action": "resume", "status": "active"}));
    assert_eq!(session.goal_status, Some(GoalStatus::Active));
    assert!(session.goal_can_pursue());

    let stopped = handle_goal_control_command(&mut session, "stop".to_string()).unwrap();
    assert_eq!(stopped, json!({"action": "stop", "status": "stopped"}));
    assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
    assert!(!session.goal_can_pursue());
}

#[test]
fn explicit_goal_control_stop_is_not_reactivated_by_next_user_turn() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();

    handle_goal_control_command(&mut session, "stop".to_string()).unwrap();
    assert_eq!(session.goal_status, Some(GoalStatus::Stopped));

    note_user_turn_resets_goal_pursuit(&mut session);

    assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
}

#[test]
fn goal_control_resume_requires_paused_or_stopped() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();

    let error = handle_goal_control_command(&mut session, "resume".to_string()).unwrap_err();
    assert!(error.contains("paused or stopped"));
    assert_eq!(session.goal_status, Some(GoalStatus::Active));

    session.goal_set_status(GoalStatus::Completed);
    assert!(handle_goal_control_command(&mut session, "resume".to_string()).is_err());
    assert_eq!(session.goal_status, Some(GoalStatus::Completed));

    session.goal_set_status(GoalStatus::Stopped);
    handle_goal_control_command(&mut session, "resume".to_string()).unwrap();
    assert_eq!(session.goal_status, Some(GoalStatus::Active));
}

#[test]
fn goal_control_stop_purges_goal_generated_regenerates_and_records_event() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();
    session.command_queue.push_back(CommandRequest {
        client_request_id: "goal-nudge-1".to_string(),
        priority: true,
        command: ChatCommand::Regenerate {},
    });
    session.command_queue.push_back(CommandRequest {
        client_request_id: "goal-verifier-regenerate-1".to_string(),
        priority: true,
        command: ChatCommand::Regenerate {},
    });
    session.command_queue.push_back(CommandRequest {
        client_request_id: "user-regenerate".to_string(),
        priority: false,
        command: ChatCommand::Regenerate {},
    });
    let now = std::time::Instant::now();
    session
        .command_enqueued_at
        .insert("goal-nudge-1".to_string(), now);
    session
        .command_enqueued_at
        .insert("goal-verifier-regenerate-1".to_string(), now);
    session
        .command_enqueued_at
        .insert("user-regenerate".to_string(), now);

    handle_goal_control_command(&mut session, "stop".to_string()).unwrap();

    assert_eq!(session.command_queue.len(), 1);
    assert_eq!(
        session.command_queue.front().unwrap().client_request_id,
        "user-regenerate"
    );
    assert!(!session.command_enqueued_at.contains_key("goal-nudge-1"));
    assert!(!session
        .command_enqueued_at
        .contains_key("goal-verifier-regenerate-1"));
    assert!(session.command_enqueued_at.contains_key("user-regenerate"));
    assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
    let stop_events = session
        .messages
        .iter()
        .filter(|message| {
            message
                .extra
                .get("event")
                .and_then(|event| event.get("payload"))
                .and_then(|payload| payload.get("trigger"))
                .and_then(|trigger| trigger.as_str())
                == Some("goal_control")
        })
        .count();
    assert_eq!(stop_events, 1);
}

#[test]
fn goal_control_stop_survives_projection_rebuild() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();

    handle_goal_control_command(&mut session, "stop".to_string()).unwrap();
    session.rebuild_goal_projection_from_messages();

    assert_eq!(session.goal_status, Some(GoalStatus::Stopped));
    assert!(!session.goal_can_pursue());
}

#[test]
fn manual_abort_stop_is_reactivated_by_next_user_turn() {
    let mut session = ChatSession::new("goal-command".to_string());
    handle_set_goal_command(&mut session, "Base".to_string(), None, None).unwrap();

    assert!(session.stop_goal_on_manual_abort());
    assert_eq!(session.goal_status, Some(GoalStatus::Stopped));

    note_user_turn_resets_goal_pursuit(&mut session);

    assert_eq!(session.goal_status, Some(GoalStatus::Active));
}

#[test]
fn restore_keeps_plan_role() {
    let plan = make_plan_message("base plan");
    let sanitized = sanitize_message_for_restore(&plan);

    assert!(is_allowed_for_restore(&plan));
    assert_eq!(sanitized.role, "plan");
    assert_eq!(sanitized.content.content_text_only(), "base plan");
    assert_eq!(sanitized.preserve, Some(true));
    assert_eq!(sanitized.extra["plan"]["version"], json!(1));
}

#[test]
fn restore_keeps_goal_and_goal_events() {
    let goal = make_goal_message("ship the frog");
    let delta = make_goal_event("tighten acceptance", "goal_delta");
    let pursuit = make_goal_event("verifying", "goal_pursuit");

    for message in [&goal, &delta, &pursuit] {
        assert!(is_allowed_for_restore(message));
    }
    let restored_goal = sanitize_message_for_restore(&goal);
    let restored_delta = sanitize_message_for_restore(&delta);
    let restored_pursuit = sanitize_message_for_restore(&pursuit);

    assert_eq!(restored_goal.role, "goal");
    assert_eq!(restored_goal.extra["goal"]["version"], json!(1));
    assert_eq!(
        restored_delta.extra["event"]["subkind"],
        json!("goal_delta")
    );
    assert_eq!(
        restored_pursuit.extra["event"]["subkind"],
        json!("goal_pursuit")
    );
}

#[test]
fn branch_keeps_plan_and_plan_delta() {
    let plan = make_plan_message("base plan");
    let delta = make_plan_delta_event("update", 2);
    let other_event = crate::chat::internal_roles::event(
        crate::chat::internal_roles::EventSubkind::SystemNotice,
        "test",
        json!({}),
        "skip",
    );

    assert!(is_allowed_for_branch(&plan));
    assert!(is_allowed_for_branch(&delta));
    assert!(!is_allowed_for_branch(&other_event));
    let branched_plan = sanitize_message_for_branch(&plan);
    let branched_delta = sanitize_message_for_branch(&delta);

    assert_eq!(branched_plan.role, "plan");
    assert_eq!(branched_plan.extra["plan"]["version"], json!(1));
    assert_eq!(branched_delta.role, "event");
    assert_eq!(
        branched_delta.extra["event"]["subkind"],
        json!("plan_delta")
    );
    assert_eq!(branched_delta.extra["event"]["payload"]["seq"], json!(2));
}

#[test]
fn branch_keeps_goal_and_goal_events() {
    let goal = make_goal_message("base goal");
    let delta = make_goal_event("update", "goal_delta");
    let pursuit = make_goal_event("verifying", "goal_pursuit");
    let other_event = crate::chat::internal_roles::event(
        crate::chat::internal_roles::EventSubkind::SystemNotice,
        "test",
        json!({}),
        "skip",
    );

    assert!(is_allowed_for_branch(&goal));
    assert!(is_allowed_for_branch(&delta));
    assert!(is_allowed_for_branch(&pursuit));
    assert!(!is_allowed_for_branch(&other_event));
    let branched_goal = sanitize_message_for_branch(&goal);
    let branched_delta = sanitize_message_for_branch(&delta);
    let branched_pursuit = sanitize_message_for_branch(&pursuit);

    assert_eq!(branched_goal.role, "goal");
    assert_eq!(branched_goal.extra["goal"]["version"], json!(1));
    assert_eq!(
        branched_delta.extra["event"]["subkind"],
        json!("goal_delta")
    );
    assert_eq!(
        branched_pursuit.extra["event"]["subkind"],
        json!("goal_pursuit")
    );
}
