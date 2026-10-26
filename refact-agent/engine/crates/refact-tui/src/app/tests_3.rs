use super::*;
use super::tests_util::*;

#[test]
fn goal_overlay_uses_goal_key_context_and_dispatches_controls() {
    let _surface_lock = goal_dock::test_surface_lock();
    let previous = std::env::var_os("REFACT_TUI_SURFACES");
    std::env::set_var("REFACT_TUI_SURFACES", "1");
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({
            "runtime": {"state": "idle"},
            "messages": [{
                "role": "goal",
                "content": "Ship it",
                "extra": {"goal": {"status": "active", "active": true}}
            }]
        }),
    });
    assert_eq!(app.execute_command_name("goal"), AppAction::None);
    assert!(app.test_goal_overlay_open());
    assert_eq!(app.focused_key_context(), KeyContext::Goal);
    assert_eq!(
        app.handle_key(key(KeyCode::Char('p'))),
        AppAction::GoalControl {
            action: GoalControlAction::Pause
        }
    );
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::None);
    assert!(!app.test_goal_overlay_open());
    match previous {
        Some(value) => std::env::set_var("REFACT_TUI_SURFACES", value),
        None => std::env::remove_var("REFACT_TUI_SURFACES"),
    }
}

#[tokio::test]
async fn goal_commands_post_all_four_goal_wire_commands() {
    let _surface_lock = goal_dock::test_surface_lock();
    let previous = std::env::var_os("REFACT_TUI_SURFACES");
    std::env::set_var("REFACT_TUI_SURFACES", "1");
    let state = CommandState::default();
    let base_url = spawn_command_server(state.clone());
    let client = DaemonClient::new(base_url, None).unwrap();
    let mut app = App::new(project());
    let (tx, mut rx) = mpsc::channel(4);
    let mut subscriptions = SubscriptionManager::new();
    let mut daemon_events = DaemonEventSubscription::new();

    for action in [
        app.execute_command_name("goal set Ship it"),
        app.execute_command_name("goal update Add tests"),
        app.execute_command_name("goal budget"),
    ] {
        run_action(
            &mut app,
            action,
            &client,
            &tx,
            &mut subscriptions,
            &mut daemon_events,
            crate::client::DaemonUrlSource::Discovery,
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeEvent::CommandFinished { result: Ok(()), .. })
        ));
    }
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({
            "runtime": {"state": "idle"},
            "messages": [{
                "role": "goal",
                "content": "Ship it",
                "extra": {"goal": {"status": "active", "active": true}}
            }]
        }),
    });
    let pause_action = app.execute_command_name("goal pause");
    run_action(
        &mut app,
        pause_action,
        &client,
        &tx,
        &mut subscriptions,
        &mut daemon_events,
        crate::client::DaemonUrlSource::Discovery,
    )
    .await;
    assert!(matches!(
        rx.recv().await,
        Some(RuntimeEvent::CommandFinished { result: Ok(()), .. })
    ));

    assert!(state.find_command("set_goal").is_some());
    assert!(state.find_command("update_goal").is_some());
    assert!(state.find_command("set_goal_budget").is_some());
    assert_eq!(
        state
            .find_command("goal_control")
            .and_then(|command| command.get("action").cloned()),
        Some(json!("pause"))
    );
    match previous {
        Some(value) => std::env::set_var("REFACT_TUI_SURFACES", value),
        None => std::env::remove_var("REFACT_TUI_SURFACES"),
    }
}

#[test]
fn review_and_compact_insert_structured_prompts() {
    let mut app = App::new(project());
    assert!(matches!(
        app.execute_command_name("review"),
        AppAction::SendMessage { prompt, .. } if prompt.contains("Review the current project changes")
    ));

    let mut app = App::new(project());
    assert!(matches!(
        app.execute_command_name("compact"),
        AppAction::SendMessage { prompt, .. } if prompt.contains("ctx_probe")
    ));
}

#[test]
fn agent_command_emits_mode_switch_patch() {
    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("agent"),
        AppAction::SetParams {
            patch: json!({"mode": "agent", "tool_use": "agent"})
        }
    );
    assert_eq!(app.mode(), Some("agent"));
}

#[test]
fn command_availability_blocks_active_idle_only_and_allows_idle() {
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);

    assert_eq!(app.execute_command_name("agent"), AppAction::None);
    assert_eq!(app.mode(), None);
    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Notice(text) if text.contains("/agent is available between turns only"))
        }));

    app.set_session_state(SessionState::Idle);
    assert_eq!(
        app.execute_command_name("agent"),
        AppAction::SetParams {
            patch: json!({"mode": "agent", "tool_use": "agent"})
        }
    );
}

#[test]
fn reasoning_command_emits_set_params_and_updates_footer_state() {
    let mut app = App::new(project());
    app.apply_caps(&json!({
        "chat_models": {
            "openai/gpt-demo": {"reasoning_effort_options": ["low", "medium", "high"]}
        }
    }));
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"model": "openai/gpt-demo", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": []}),
        });

    let action = app.execute_command_name("reasoning high");

    assert_eq!(
        action,
        AppAction::SetParams {
            patch: json!({"boost_reasoning": true, "reasoning_effort": "high", "thinking_budget": null})
        }
    );
    assert_eq!(app.reasoning_effort_label(), "high");
    let footer = crate::ui::footer::footer_text(&crate::ui::footer::FooterData::from_app(&app));
    assert!(footer.contains("reason:high"));
}

#[test]
fn reasoning_command_reports_unsupported_model_without_set_params() {
    let mut app = App::new(project());
    app.apply_caps(&json!({"chat_models": {"gpt-basic": {"reasoning_effort_options": null}}}));
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"model": "gpt-basic", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": []}),
        });

    assert_eq!(app.execute_command_name("reasoning high"), AppAction::None);
    assert_eq!(app.reasoning_effort_label(), "off");
    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Notice(text) if text.contains("Reasoning capability is not confirmed"))
        }));
}

#[test]
fn reasoning_command_is_guarded_mid_turn() {
    let mut app = App::new(project());
    app.apply_caps(&json!({
        "chat_models": {
            "gpt-demo": {"reasoning_effort_options": ["high"]}
        }
    }));
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"model": "gpt-demo", "mode": "agent"}, "runtime": {"state": "generating"}, "messages": []}),
        });

    assert_eq!(app.execute_command_name("reasoning high"), AppAction::None);
    assert_eq!(app.reasoning_effort_label(), "off");
    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Notice(text) if text.contains("between turns only"))
    }));
}

#[test]
fn reasoning_command_supports_boost_only_models() {
    let mut app = App::new(project());
    app.apply_caps(&json!({
        "chat_models": {
            "gpt-boost": {"supports_thinking_budget": true}
        }
    }));
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"model": "gpt-boost", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": []}),
        });

    let action = app.execute_command_name("reasoning on");

    assert_eq!(
        action,
        AppAction::SetParams {
            patch: json!({"boost_reasoning": true, "reasoning_effort": null, "thinking_budget": null})
        }
    );
    assert_eq!(app.reasoning_effort_label(), "on");
}

#[test]
fn reasoning_command_failure_rolls_back_optimistic_state() {
    let mut app = App::new(project());
    app.apply_caps(&json!({
        "chat_models": {
            "gpt-demo": {"reasoning_effort_options": ["low", "high"]}
        }
    }));
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"model": "gpt-demo", "mode": "agent", "boost_reasoning": true, "reasoning_effort": "low"}, "runtime": {"state": "idle"}, "messages": []}),
        });
    let action = app.execute_command_name("reasoning high");
    let AppAction::SetParams { patch } = action else {
        panic!("expected set params action");
    };
    assert_eq!(app.reasoning_effort_label(), "high");
    let context = app.set_params_context(&patch);

    let result = app.handle_command_finished(context, Err("backend rejected".to_string()));

    assert_eq!(result, AppAction::None);
    assert_eq!(app.reasoning_effort_label(), "low");
    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Notice(text) if text.contains("/reasoning failed") && text.contains("backend rejected"))
        }));
}

#[test]
fn reasoning_picker_lists_only_supported_effort_levels() {
    let mut app = App::new(project());
    app.apply_caps(&json!({
        "chat_models": {
            "gpt-demo": {"reasoning_effort_options": ["low", "high", "xhigh"]}
        }
    }));
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"model": "gpt-demo", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": []}),
        });

    assert_eq!(app.execute_command_name("reasoning"), AppAction::None);

    let ids = app
        .modal_picker()
        .unwrap()
        .filtered_items()
        .into_iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["off", "on", "low", "high"]);
}

#[test]
fn diff_command_loads_local_project_diff() {
    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("diff"),
        AppAction::LoadDiff {
            root: PathBuf::from("/tmp/demo")
        }
    );
}

#[test]
fn resume_picker_accept_switches_chat_and_requests_snapshot() {
    let mut app = App::new(project());
    let previous = app.chat_id().to_string();
    app.open_session_picker(vec![PickerItem {
        id: "chat-resume".to_string(),
        title: "Saved chat".to_string(),
        description: "1h ago · gpt-demo · agent".to_string(),
    }]);

    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SubscribeCurrent
    );

    assert_eq!(app.chat_id(), "chat-resume");
    assert_ne!(app.chat_id(), previous);
    assert_eq!(app.session_title(), Some("Saved chat"));
    assert!(matches!(
        app.visible_transcript().first(),
        Some(TranscriptItem::Session { title, .. }) if title == "Saved chat"
    ));
    assert!(session_text(&app).contains("directory: /tmp/demo"));
    assert!(app
        .session_tabs()
        .iter()
        .any(|tab| { tab.id == "chat-resume" && tab.is_current }));
}

#[test]
fn fork_command_emits_branch_from_chat_action() {
    let mut app = App::new(project());
    let source = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
            chat_id: Some(source.clone()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"id": source, "title": "Base", "model": "gpt-demo", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": [
                {"message_id": "u1", "role": "user", "content": "hello"},
                {"message_id": "a1", "role": "assistant", "content": "hi"}
            ]}),
        });

    let action = app.execute_command_name("fork");

    assert!(matches!(
        action,
        AppAction::ForkChat { source_chat_id, up_to_message_id, .. }
            if source_chat_id == source && up_to_message_id == "a1"
    ));
    assert_eq!(app.chat_id(), source);
}

#[test]
fn rename_and_archive_commands_emit_session_actions() {
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();

    assert_eq!(
        app.execute_command_name("rename Better title"),
        AppAction::RenameChat {
            title: "Better title".to_string()
        }
    );
    assert_eq!(app.session_title(), None);

    let action = app.execute_command_name("archive");
    assert!(matches!(
        action,
        AppAction::ArchiveChat { chat_id: archived, .. } if archived == chat_id
    ));
    assert_eq!(app.chat_id(), chat_id);
}

#[test]
fn command_finished_from_stale_generation_is_ignored() {
    let mut app = App::new(project());
    let mut subscriptions = SubscriptionManager::new();
    subscriptions.generation = 3;
    let old_chat_id = app.chat_id().to_string();
    let stale_title = "stale title".to_string();
    let origin = app.command_origin();

    assert!(subscriptions
        .apply_command_finished(
            &mut app,
            2,
            CommandContextTag::Rename {
                origin: origin.clone(),
                title: stale_title.clone(),
            },
            Ok(()),
        )
        .is_none());
    assert_eq!(app.session_title(), None);

    assert!(subscriptions
        .apply_command_finished(
            &mut app,
            3,
            CommandContextTag::Rename {
                origin,
                title: stale_title,
            },
            Ok(()),
        )
        .is_some());
    assert_eq!(app.chat_id(), old_chat_id);
    assert_eq!(app.session_title(), Some("stale title"));
}

#[test]
fn daemon_event_from_stale_generation_is_ignored() {
    let mut app = App::new(project());
    let mut daemon_events = DaemonEventSubscription::new();
    daemon_events.generation = 2;
    let stale = DaemonEventRecord {
        ts_ms: Some(1),
        kind: "worker_ready".to_string(),
        project_id: Some("old".to_string()),
        payload: Value::Null,
    };
    let fresh = DaemonEventRecord {
        ts_ms: Some(2),
        kind: "worker_ready".to_string(),
        project_id: Some("p1".to_string()),
        payload: Value::Null,
    };

    assert!(!daemon_events.apply_event(&mut app, 1, stale));
    assert!(app.events_pane().events().is_empty());
    assert!(daemon_events.apply_event(&mut app, 2, fresh));
    assert_eq!(app.events_pane().events().len(), 1);
}

#[test]
fn daemon_event_disconnect_notice_reports_bounded_reconnect() {
    let mut app = App::new(project());

    app.handle_daemon_events_disconnected("network down".to_string(), true);
    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Notice(text) if text.contains("bounded backoff"))
    }));

    app.handle_daemon_events_disconnected("network down".to_string(), false);
    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Notice(text) if text.contains("reconnect limit reached"))
    }));
}

#[test]
fn ctrl_c_abort_failure_restores_approval_and_ask_form() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-approval", "shell"));
    let request = AskQuestionsRequest::from_tool_content(
            r#"{"type":"ask_questions","tool_call_id":"call-ask","questions":[{"id":"confirm","type":"yes_no","text":"Proceed?"}]}"#,
            None,
        )
        .unwrap();
    app.test_set_ask_questions_form(AskQuestionsForm::new(request));
    assert!(app.approval_modal().is_some());
    assert!(app.ask_questions_form().is_some());

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::Abort
    );
    assert!(app.approval_modal().is_some());
    assert!(app.ask_questions_form().is_some());

    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Abort {
                origin: app.command_origin(),
            },
            Err("backend unavailable".to_string()),
        ),
        AppAction::None
    );
    assert!(!app.abort_in_flight);
    assert!(app.approval_modal().is_some());
    assert!(app.ask_questions_form().is_some());
}

#[test]
fn stale_command_completions_are_silent_and_current_origin_applies() {
    let mut app = App::new(project());
    let origin = app.command_origin();
    app.abort_in_flight = true;
    app.set_session_state(SessionState::Generating);
    app.set_project(project_b());
    app.abort_in_flight = true;
    app.set_session_state(SessionState::Generating);
    let before = app.visible_transcript().len();

    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Abort {
                origin: origin.clone(),
            },
            Ok(()),
        ),
        AppAction::None
    );
    assert!(app.abort_in_flight);
    assert_eq!(app.session_state(), SessionState::Generating);
    assert_eq!(app.visible_transcript().len(), before);
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Abort {
                origin: origin.clone(),
            },
            Err("stale abort".to_string()),
        ),
        AppAction::None
    );
    assert!(app.abort_in_flight);
    assert_eq!(app.visible_transcript().len(), before);

    let stale_title = "Stale".to_string();
    for result in [Ok(()), Err("stale rename".to_string())] {
        assert_eq!(
            app.handle_command_finished(
                CommandContextTag::Rename {
                    origin: origin.clone(),
                    title: stale_title.clone(),
                },
                result,
            ),
            AppAction::None
        );
    }
    assert_eq!(app.session_title(), None);

    let stale_fork = "stale-fork".to_string();
    for result in [Ok(()), Err("stale fork".to_string())] {
        assert_eq!(
            app.handle_command_finished(
                CommandContextTag::Fork {
                    origin: origin.clone(),
                    target_chat_id: stale_fork.clone(),
                    title: None,
                },
                result,
            ),
            AppAction::None
        );
    }
    assert_ne!(app.chat_id(), stale_fork);

    let stale_archive = "stale-archive".to_string();
    for result in [Ok(()), Err("stale archive".to_string())] {
        assert_eq!(
            app.handle_command_finished(
                CommandContextTag::Archive {
                    origin: origin.clone(),
                    chat_id: stale_archive.clone(),
                },
                result,
            ),
            AppAction::None
        );
    }
    assert_ne!(app.chat_id(), stale_archive);
    assert_eq!(app.visible_transcript().len(), before);

    let current = app.command_origin();
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Abort {
                origin: current.clone(),
            },
            Ok(()),
        ),
        AppAction::None
    );
    assert_eq!(app.session_state(), SessionState::Idle);
    assert!(!app.abort_in_flight);

    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Rename {
                origin: current,
                title: "Current".to_string(),
            },
            Ok(()),
        ),
        AppAction::None
    );
    assert_eq!(app.session_title(), Some("Current"));
}

#[test]
fn failed_session_mutations_do_not_commit_local_state() {
    let mut app = App::new(project());
    let source_chat_id = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
            chat_id: Some(source_chat_id.clone()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"id": source_chat_id, "title": "Base", "model": "gpt-demo", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": [
                {"message_id": "u1", "role": "user", "content": "hello"},
                {"message_id": "a1", "role": "assistant", "content": "hi"}
            ]}),
        });
    let action = app.execute_command_name("fork");
    let (target_chat_id, title) = match action {
        AppAction::ForkChat {
            target_chat_id,
            title,
            ..
        } => (target_chat_id, title),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(app.chat_id(), source_chat_id);
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Fork {
                origin: app.command_origin(),
                target_chat_id: target_chat_id.clone(),
                title: title.clone(),
            },
            Err("branch failed".to_string()),
        ),
        AppAction::None
    );
    assert_eq!(app.chat_id(), source_chat_id);

    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Fork {
                origin: app.command_origin(),
                target_chat_id: target_chat_id.clone(),
                title,
            },
            Ok(()),
        ),
        AppAction::SubscribeCurrent
    );
    assert_eq!(app.chat_id(), target_chat_id);

    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    assert_eq!(
        app.execute_command_name("rename Better title"),
        AppAction::RenameChat {
            title: "Better title".to_string()
        }
    );
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Rename {
                origin: app.command_origin(),
                title: "Better title".to_string(),
            },
            Err("rename failed".to_string()),
        ),
        AppAction::None
    );
    assert_eq!(app.session_title(), None);

    let action = app.execute_command_name("archive");
    let new_chat_id = match action {
        AppAction::ArchiveChat { new_chat_id, .. } => new_chat_id,
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(app.chat_id(), chat_id);
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Archive {
                origin: app.command_origin(),
                chat_id: new_chat_id.clone(),
            },
            Err("archive failed".to_string()),
        ),
        AppAction::None
    );
    assert_eq!(app.chat_id(), chat_id);
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Archive {
                origin: app.command_origin(),
                chat_id: new_chat_id.clone(),
            },
            Ok(()),
        ),
        AppAction::SubscribeCurrent
    );
    assert_eq!(app.chat_id(), new_chat_id);
}

#[test]
fn project_and_chat_switch_clear_pending_params() {
    let mut app = App::new(project());
    app.pending_model = Some("model-a".to_string());
    app.pending_mode = Some("mode-a".to_string());
    app.pending_send_retry = Some(PendingSendRetry {
        prompt: "retry".to_string(),
        params: json!({"model": "old"}),
        correlation: ClientMessageCorrelation {
            client_request_id: "old-request".to_string(),
            client_message_id: "old-message".to_string(),
        },
    });

    app.new_chat();
    app.composer.set_text("hello");
    let action = app.handle_key(key(KeyCode::Enter));
    match action {
        AppAction::SendMessage { params, .. } => {
            assert_eq!(params.get("model"), None);
            assert_eq!(
                params.get("mode"),
                Some(&Value::String("agent".to_string()))
            );
        }
        other => panic!("unexpected action: {other:?}"),
    }

    app.pending_model = Some("model-b".to_string());
    app.pending_mode = Some("mode-b".to_string());
    app.pending_send_retry = Some(PendingSendRetry {
        prompt: "retry".to_string(),
        params: json!({"model": "old"}),
        correlation: ClientMessageCorrelation {
            client_request_id: "old-request".to_string(),
            client_message_id: "old-message".to_string(),
        },
    });
    app.set_project(OpenProjectResponse {
        project_id: "p2".to_string(),
        slug: "demo2".to_string(),
        root: PathBuf::from("/tmp/demo2"),
        pinned: Some(false),
        worker: None,
        cron_pending: None,
    });
    app.composer.set_text("next");
    let action = app.handle_key(key(KeyCode::Enter));
    match action {
        AppAction::SendMessage { params, .. } => {
            assert_eq!(params.get("model"), None);
            assert_eq!(
                params.get("tool_use"),
                Some(&Value::String("agent".to_string()))
            );
        }
        other => panic!("unexpected action: {other:?}"),
    }
}

#[test]
fn show_diff_result_renders_diff_cell() {
    let mut app = App::new(project());
    app.show_diff_result("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new".to_string());
    assert!(app
        .visible_transcript()
        .iter()
        .any(|item| { matches!(item, TranscriptItem::Diff(text) if text.contains("+new")) }));
}

#[tokio::test]
async fn mode_switch_command_posts_set_params() {
    let state = CommandState::default();
    let base_url = spawn_command_server(state.clone());
    let client = DaemonClient::new(base_url, None).unwrap();
    let mut app = App::new(project());
    let action = app.execute_command_name("agent");
    let (tx, mut rx) = mpsc::channel(1);
    let mut subscriptions = SubscriptionManager::new();
    let mut daemon_events = DaemonEventSubscription::new();
    run_action(
        &mut app,
        action,
        &client,
        &tx,
        &mut subscriptions,
        &mut daemon_events,
        crate::client::DaemonUrlSource::Discovery,
    )
    .await;
    assert!(matches!(
        rx.recv().await,
        Some(RuntimeEvent::CommandFinished {
            generation: 0,
            context: CommandContextTag::Other,
            result: Ok(())
        })
    ));

    let command = state.find_command("set_params").unwrap();
    assert_eq!(command["patch"]["mode"], "agent");
    assert_eq!(command["patch"]["tool_use"], "agent");
}

#[tokio::test]
async fn send_message_action_posts_its_client_request_id() {
    let state = CommandState::default();
    let base_url = spawn_command_server(state.clone());
    let client = DaemonClient::new(base_url, None).unwrap();
    let mut app = App::new(project());
    app.composer.set_text("hello");
    let action = app.handle_key(key(KeyCode::Enter));
    let correlation = match &action {
        AppAction::SendMessage { correlation, .. } => correlation.clone(),
        other => panic!("unexpected action: {other:?}"),
    };
    let (tx, mut rx) = mpsc::channel(1);
    let mut subscriptions = SubscriptionManager::new();
    let mut daemon_events = DaemonEventSubscription::new();

    run_action(
        &mut app,
        action,
        &client,
        &tx,
        &mut subscriptions,
        &mut daemon_events,
        crate::client::DaemonUrlSource::Discovery,
    )
    .await;

    assert!(matches!(
        rx.recv().await,
        Some(RuntimeEvent::CommandFinished {
            context: CommandContextTag::SendMessage { correlation: sent, .. },
            result: Ok(()),
            ..
        }) if sent == correlation
    ));
    let command = state.find_command("user_message").unwrap();
    assert_eq!(command["client_request_id"], correlation.client_request_id);
    assert_eq!(command["client_message_id"], correlation.client_message_id);
}

#[tokio::test]
async fn reasoning_command_posts_set_params() {
    let state = CommandState::default();
    let base_url = spawn_command_server(state.clone());
    let client = DaemonClient::new(base_url, None).unwrap();
    let mut app = App::new(project());
    app.apply_caps(&json!({
        "chat_models": {
            "gpt-demo": {"reasoning_effort_options": ["low", "medium", "high"]}
        }
    }));
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {"model": "gpt-demo", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": []}),
        });
    let action = app.execute_command_name("reasoning high");
    let (tx, mut rx) = mpsc::channel(1);
    let mut subscriptions = SubscriptionManager::new();
    let mut daemon_events = DaemonEventSubscription::new();
    run_action(
        &mut app,
        action,
        &client,
        &tx,
        &mut subscriptions,
        &mut daemon_events,
        crate::client::DaemonUrlSource::Discovery,
    )
    .await;
    assert!(matches!(
        rx.recv().await,
        Some(RuntimeEvent::CommandFinished {
            generation: 0,
            context: CommandContextTag::Reasoning { .. },
            result: Ok(())
        })
    ));

    let command = state.find_command("set_params").unwrap();
    assert_eq!(command["patch"]["boost_reasoning"], true);
    assert_eq!(command["patch"]["reasoning_effort"], "high");
    assert!(command["patch"]["thinking_budget"].is_null());
}

#[test]
fn browser_events_drive_browser_state_and_reconnect_snapshot() {
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();

    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({
            "type": "snapshot",
            "thread": {},
            "runtime": {"state": "idle"},
            "messages": [],
            "browser": {
                "runtime_id": "browser-1",
                "connected": true,
                "active_tab": "tab-1",
                "url": "https://example.test",
                "title": "Example",
                "tabs": [{"tab_id": "tab-1", "url": "https://example.test", "title": "Example"}]
            }
        }),
    });
    assert!(app.browser_state().is_open);
    assert_eq!(
        app.browser_state().current_url.as_deref(),
        Some("https://example.test")
    );
    assert_eq!(app.browser_state().tabs.len(), 1);

    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "browser_frame".to_string(),
        raw: json!({
            "type": "browser_frame",
            "tab_id": "tab-1",
            "mime": "image/jpeg",
            "data": "frame-data",
            "changed_text": "Saved"
        }),
    });
    assert_eq!(
        app.browser_state()
            .latest_frame
            .as_ref()
            .map(|frame| frame.changed_text.as_deref()),
        Some(Some("Saved"))
    );

    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "browser_timeline".to_string(),
        raw: json!({
            "type": "browser_timeline",
            "events": [{"type": "click", "summary": "Saved"}]
        }),
    });
    assert_eq!(app.browser_state().timeline.len(), 1);

    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "browser_toolbar_action".to_string(),
        raw: json!({"type": "browser_toolbar_action", "action": "screenshot"}),
    });
    assert_eq!(
        app.browser_state().toolbar_actions,
        vec!["screenshot".to_string()]
    );

    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "browser_status".to_string(),
        raw: json!({
            "type": "browser_status",
            "runtime_id": "browser-1",
            "connected": true,
            "active_tab": "tab-2",
            "url": "https://second.example.test",
            "title": "Second",
            "tabs": [{"tab_id": "tab-2", "url": "https://second.example.test", "title": "Second"}]
        }),
    });
    assert_eq!(app.browser_state().active_tab.as_deref(), Some("tab-2"));
    assert_eq!(app.browser_state().current_title.as_deref(), Some("Second"));

    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "browser_context_oversize".to_string(),
        raw: json!({
            "type": "browser_context_oversize",
            "total_bytes": 1000,
            "action_count": 2,
            "action_bytes": 200,
            "console_count": 3,
            "console_bytes": 300,
            "network_count": 4,
            "network_bytes": 400,
            "mutation_bytes": 100,
            "pending_message_id": "pending-1"
        }),
    });
    assert_eq!(
        app.browser_state()
            .context_prompt
            .as_ref()
            .map(|prompt| prompt.event.pending_message_id.as_str()),
        Some("pending-1")
    );

    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id),
        seq: None,
        kind: "browser_closed".to_string(),
        raw: json!({
            "type": "browser_closed",
            "runtime_id": "browser-1",
            "reason": "user_closed"
        }),
    });
    assert!(!app.browser_state().is_open);
    assert!(app.browser_state().current_url.is_none());
    assert!(app.browser_state().context_prompt.is_none());
    assert_eq!(app.browser_state().timeline.len(), 1);
    assert_eq!(
        app.browser_state()
            .last_closed
            .as_ref()
            .map(|closed| closed.reason.as_str()),
        Some("user_closed")
    );
}

#[test]
fn browser_snapshot_replaces_stale_event_state_on_reconnect() {
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "browser_frame".to_string(),
        raw: json!({
            "type": "browser_frame",
            "tab_id": "tab-1",
            "mime": "image/jpeg",
            "data": "frame-data"
        }),
    });
    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "browser_timeline".to_string(),
        raw: json!({"type": "browser_timeline", "events": [{"type": "click"}]}),
    });
    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id.clone()),
        seq: None,
        kind: "browser_context_oversize".to_string(),
        raw: json!({
            "type": "browser_context_oversize",
            "pending_message_id": "pending-1"
        }),
    });

    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({
            "type": "snapshot",
            "thread": {},
            "runtime": {"state": "idle"},
            "messages": [],
            "browser": {
                "runtime_id": "browser-2",
                "connected": true,
                "active_tab": "tab-2",
                "url": "https://reconnected.example.test",
                "title": "Reconnected",
                "tabs": []
            }
        }),
    });

    assert_eq!(app.browser_state().runtime_id.as_deref(), Some("browser-2"));
    assert!(app.browser_state().latest_frame.is_none());
    assert!(app.browser_state().timeline.is_empty());
    assert!(app.browser_state().context_prompt.is_none());
}

#[tokio::test]
async fn browser_context_prompt_sends_decision_and_restores_after_failure() {
    let state = CommandState::default();
    let base_url = spawn_command_server(state.clone());
    let client = DaemonClient::new(base_url, None).unwrap();
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    app.apply_chat_event(ChatEvent {
        chat_id: Some(chat_id),
        seq: None,
        kind: "browser_context_oversize".to_string(),
        raw: json!({
            "type": "browser_context_oversize",
            "total_bytes": 1000,
            "pending_message_id": "pending-1"
        }),
    });

    let action = app.submit_browser_context_decision(BrowserContextDecisionOptions {
        include_actions: true,
        include_console: false,
        include_network: true,
        include_mutations: false,
        include_screenshot: true,
        last_n_actions: Some(2),
        last_n_console: None,
        last_n_network: Some(3),
    });
    assert!(app.browser_state().context_prompt.is_none());
    let (tx, mut rx) = mpsc::channel(1);
    let mut subscriptions = SubscriptionManager::new();
    let mut daemon_events = DaemonEventSubscription::new();
    run_action(
        &mut app,
        action,
        &client,
        &tx,
        &mut subscriptions,
        &mut daemon_events,
        crate::client::DaemonUrlSource::Discovery,
    )
    .await;

    assert!(matches!(
        rx.recv().await,
        Some(RuntimeEvent::CommandFinished {
            context: CommandContextTag::BrowserContextDecision { .. },
            result: Ok(()),
            ..
        })
    ));
    let command = state.find_command("browser_context_decision").unwrap();
    assert_eq!(command["pending_message_id"], "pending-1");
    assert_eq!(command["include_actions"], true);
    assert_eq!(command["include_console"], false);
    assert_eq!(command["include_network"], true);
    assert_eq!(command["include_mutations"], false);
    assert_eq!(command["include_screenshot"], true);
    assert_eq!(command["last_n_actions"], 2);
    assert_eq!(command["last_n_network"], 3);

    let failed = app.submit_browser_context_decision(BrowserContextDecisionOptions::include_all());
    assert_eq!(failed, AppAction::None);
    app.apply_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "browser_context_oversize".to_string(),
        raw: json!({
            "type": "browser_context_oversize",
            "total_bytes": 1000,
            "pending_message_id": "pending-2"
        }),
    });
    let action = app.submit_browser_context_decision(BrowserContextDecisionOptions::include_all());
    let AppAction::SendBrowserContextDecision { prompt, .. } = action else {
        panic!("expected browser context action");
    };
    let origin = app.command_origin();
    app.handle_command_finished(
        CommandContextTag::BrowserContextDecision { origin, prompt },
        Err("offline".to_string()),
    );
    assert_eq!(
        app.browser_state()
            .context_prompt
            .as_ref()
            .map(|prompt| prompt.event.pending_message_id.as_str()),
        Some("pending-2")
    );
}

#[tokio::test]
async fn load_git_diff_reads_project_root() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(dir.path().join("file.txt"), "old\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    assert!(Command::new("git")
        .args([
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=T",
            "commit",
            "-m",
            "init"
        ])
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(dir.path().join("file.txt"), "new\n").unwrap();

    let diff = load_git_diff(dir.path().to_path_buf()).await.unwrap();
    assert!(diff.contains("--- a/file.txt"));
    assert!(diff.contains("+++ b/file.txt"));
    assert!(diff.contains("-old"));
    assert!(diff.contains("+new"));
}

#[test]
fn esc_esc_backtrack_loads_user_message_and_retries_from_index() {
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
        chat_id: Some(chat_id),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"runtime": {"state": "idle"}, "messages": [
            {"message_id": "u1", "role": "user", "content": "first"},
            {"message_id": "a1", "role": "assistant", "content": "one"},
            {"message_id": "u2", "role": "user", "content": "second"},
            {"message_id": "a2", "role": "assistant", "content": "two"}
        ]}),
    });

    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::None);
    assert_eq!(app.selected_backtrack_index(), None);
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::None);
    assert_eq!(app.selected_backtrack_index(), Some(2));
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::None);
    assert_eq!(app.selected_backtrack_index(), Some(0));
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert_eq!(app.composer(), "first");
    app.composer.set_text("edited first");

    let action = app.handle_key(key(KeyCode::Enter));

    assert!(matches!(
        action,
        AppAction::RetryFromIndex { index: 0, content }
            if content == json!("edited first")
    ));
    assert_eq!(app.session_state(), SessionState::Generating);
    assert!(matches!(
        app.transcript_state().messages().get(0),
        Some(message) if message.role == TranscriptRole::User && message.content == "edited first"
    ));
    assert_eq!(app.transcript_state().messages().len(), 2);
}

#[test]
fn retry_from_index_failure_restores_transcript_and_leaves_generating() {
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
        chat_id: Some(chat_id),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"runtime": {"state": "idle"}, "messages": [
            {"message_id": "u1", "role": "user", "content": "first"},
            {"message_id": "a1", "role": "assistant", "content": "one"},
            {"message_id": "u2", "role": "user", "content": "second"},
            {"message_id": "a2", "role": "assistant", "content": "two"}
        ]}),
    });
    let before = app.transcript_state().messages().to_vec();

    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Enter));
    app.composer.set_text("edited first");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::RetryFromIndex { .. }
    ));
    assert_eq!(app.session_state(), SessionState::Generating);

    app.handle_command_finished(
        CommandContextTag::RetryFromIndex {
            origin: app.command_origin(),
            rollback: app.pending_backtrack_rollback.clone(),
        },
        Err("retry rejected".to_string()),
    );

    assert_eq!(
        &app.transcript_state().messages()[..before.len()],
        before.as_slice()
    );
    assert_eq!(app.session_state(), SessionState::Idle);
    assert_eq!(app.composer(), "edited first");
}

#[test]
fn stale_retry_from_index_failure_is_inert_after_chat_switch() {
    let mut app = App::new(project());
    let origin = app.command_origin();
    let chat_id = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
        chat_id: Some(chat_id),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"runtime": {"state": "idle"}, "messages": [
            {"message_id": "u1", "role": "user", "content": "first"},
            {"message_id": "a1", "role": "assistant", "content": "one"}
        ]}),
    });
    let rollback = Some(BacktrackRollback {
        transcript_state: app.transcript_state.clone(),
        session_state: app.session_state,
        usage: app.usage,
        selected_backtrack_index: app.selected_backtrack_index,
        backtrack_target: app.backtrack_target.clone(),
        backtrack_pending: app.backtrack_pending.clone(),
        last_escape_at: app.last_escape_at,
        prompt: "old retry".to_string(),
    });

    app.resume_chat("chat-next".to_string(), "Next chat".to_string(), None);
    app.handle_chat_event(tool_call_delta_event(&app, "call-current"));
    let status = tool_cards(&app)[0].status;
    let notice_count = app.visible_transcript().len();

    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::RetryFromIndex { origin, rollback },
            Err("retry rejected".to_string()),
        ),
        AppAction::None
    );

    assert!(app.approval_modal().is_none());
    assert_eq!(tool_cards(&app)[0].status, status);
    assert_eq!(app.visible_transcript().len(), notice_count);
}

#[test]
fn current_origin_retry_from_index_failure_restores_transcript() {
    let mut app = App::new(project());
    let origin = app.command_origin();
    let rollback = Some(BacktrackRollback {
        transcript_state: app.transcript_state.clone(),
        session_state: app.session_state,
        usage: app.usage,
        selected_backtrack_index: app.selected_backtrack_index,
        backtrack_target: app.backtrack_target.clone(),
        backtrack_pending: app.backtrack_pending.clone(),
        last_escape_at: app.last_escape_at,
        prompt: "retry draft".to_string(),
    });
    app.set_session_state(SessionState::Generating);

    app.handle_command_finished(
        CommandContextTag::RetryFromIndex { origin, rollback },
        Err("retry rejected".to_string()),
    );

    assert_eq!(app.session_state(), SessionState::Idle);
    assert_eq!(app.composer(), "retry draft");
    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Notice(text) if text.contains("Retry failed: retry rejected"))
        }));
}

#[test]
fn ctrl_t_overlay_opens_searches_and_enters_copy_mode() {
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
        chat_id: Some(chat_id),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"runtime": {"state": "idle"}, "messages": [
            {"message_id": "u1", "role": "user", "content": "find the thing"},
            {"message_id": "a1", "role": "assistant", "content": "the thing is here"}
        ]}),
    });

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(app.transcript_overlay().is_some());
    app.handle_key(key(KeyCode::Char('/')));
    app.handle_key(key(KeyCode::Char('t')));
    app.handle_key(key(KeyCode::Char('h')));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.transcript_overlay().unwrap().query(), "th");
    assert!(app.transcript_overlay().unwrap().match_count() > 0);
    app.handle_key(key(KeyCode::Char('c')));
    assert!(app.transcript_overlay().unwrap().is_copy_mode());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.transcript_overlay().is_none());
}

#[test]
#[cfg_attr(
    windows,
    ignore = "Windows artifact runners crash when this test spawns cmd"
)]
fn editor_round_trip_appends_with_fake_editor_script() {
    // On Windows the editor is a literal `cmd /C` string, so the temp dir is only
    // read by the `#[cfg(not(windows))]` script-writing branch below.
    #[cfg_attr(windows, allow(unused_variables))]
    let dir = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let editor = "cmd /C echo extra>>".to_string();
    #[cfg(not(windows))]
    let editor = {
        let path = dir.path().join("fake-editor.sh");
        std::fs::write(&path, "#!/bin/sh\necho extra >> \"$1\"\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path.to_string_lossy().to_string()
    };

    let text = edit_text_with_editor_command(&editor, "base\n".to_string()).unwrap();

    assert_eq!(text.replace("\r\n", "\n"), "base\nextra\n");
}

#[test]
fn editor_command_parser_handles_flags_quotes_and_empty_command() {
    let parsed = parse_editor_command("nano --wait 'two words'").unwrap();
    assert_eq!(parsed.program, "nano");
    assert_eq!(parsed.args, vec!["--wait", "two words"]);
    assert!(parse_editor_command("   ").is_err());
}

#[test]
fn editor_command_selection_prefers_editor_then_visual_then_vi() {
    assert_eq!(
        external_editor_command_from_values(Some(" nvim --wait "), Some("code"), |_| false)
            .unwrap(),
        "nvim --wait"
    );
    assert_eq!(
        external_editor_command_from_values(Some(" "), Some("code --reuse-window"), |_| false)
            .unwrap(),
        "code --reuse-window"
    );
    assert_eq!(
        external_editor_command_from_values(None, None, |command| command == "vi").unwrap(),
        "vi"
    );
    assert!(external_editor_command_from_values(None, None, |_| false).is_err());
}

#[test]
fn app_fast_typing_submits_on_enter() {
    let mut app = App::new(project());
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::empty()));
    let action = app.handle_key(key(KeyCode::Enter));
    assert!(matches!(action, AppAction::SendMessage { prompt, .. } if prompt == "ab"));
    assert!(app.composer().is_empty());
}

#[test]
fn app_history_persists_and_restores_draft() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.json");
    let mut app = App::with_history_path(project(), Some(path.clone()));
    app.composer.set_text("first");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { prompt, .. } if prompt == "first"
    ));
    let request = app.take_pending_history_save().unwrap();
    execute_history_save_request(request).unwrap();
    app.handle_history_save_result(Ok(()));
    let mut app = App::with_history_path(project(), Some(path));
    app.composer.set_text("draft");
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.composer(), "first");
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.composer(), "draft");
}

#[test]
fn history_save_failure_emits_one_notice() {
    let dir = tempfile::tempdir().unwrap();
    let parent_file = dir.path().join("not-a-dir");
    std::fs::write(&parent_file, "blocked").unwrap();
    let path = parent_file.join("history.json");
    let mut app = App::with_history_path(project(), Some(path));

    app.composer.set_text("first");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { .. }
    ));
    let request = app.take_pending_history_save().unwrap();
    let result = execute_history_save_request(request);
    app.handle_history_save_result(result);

    app.set_session_state(SessionState::Idle);
    app.composer.set_text("second");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { .. }
    ));
    let request = app.take_pending_history_save().unwrap();
    let result = execute_history_save_request(request);
    app.handle_history_save_result(result);

    let notices = app
            .visible_transcript()
            .iter()
            .filter(|item| {
                matches!(item, TranscriptItem::Notice(text) if text.contains("Failed to save composer history"))
            })
            .count();
    assert_eq!(notices, 1);
}

#[test]
fn app_ctrl_r_reverse_history_search_accepts_and_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.json");
    let mut app = App::with_history_path(project(), Some(path.clone()));
    app.composer.set_text("alpha one");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { .. }
    ));
    let request = app.take_pending_history_save().unwrap();
    execute_history_save_request(request).unwrap();
    app.handle_history_save_result(Ok(()));
    let mut app = App::with_history_path(project(), Some(path.clone()));
    app.composer.set_text("beta two");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { .. }
    ));
    let request = app.take_pending_history_save().unwrap();
    execute_history_save_request(request).unwrap();
    app.handle_history_save_result(Ok(()));

    let mut app = App::with_history_path(project(), Some(path));
    app.composer.set_text("draft");
    app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(app.composer(), "beta two");
    for ch in "alpha".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()));
    }
    assert_eq!(app.composer(), "alpha one");
    assert_eq!(app.composer_history_search().unwrap().query, "alpha");
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.composer(), "draft");

    app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::empty()));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.composer(), "beta two");
    assert!(app.composer_history_search().is_none());
}

#[test]
fn app_kill_yank_undo_redo_bindings_edit_composer() {
    let mut app = App::new(project());
    app.composer.set_text("alpha beta\ngamma");
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(app.composer(), "alpha beta\n");
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(app.composer(), "alpha beta\ngamma");
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(app.composer(), "alpha beta\n");
    app.handle_key(KeyEvent::new(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(app.composer(), "alpha beta\ngamma");
}

#[test]
fn app_cancel_and_double_ctrl_c_behaviour() {
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
        AppAction::Abort
    );
    assert_eq!(app.session_state(), SessionState::Generating);
    assert!(app.abort_in_flight);
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Abort {
                origin: app.command_origin(),
            },
            Ok(()),
        ),
        AppAction::None
    );
    assert_eq!(app.session_state(), SessionState::Idle);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(!app.should_quit());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(app.should_quit());
}

#[test]
fn normal_key_between_idle_ctrl_c_presses_resets_quit_arming() {
    let mut app = App::new(project());

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(app.last_ctrl_c.is_some());
    assert!(!app.should_quit());

    assert_eq!(app.handle_key(key(KeyCode::Char('x'))), AppAction::None);
    assert_eq!(app.composer(), "x");
    assert!(app.last_ctrl_c.is_none());

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert_eq!(app.composer(), "");
    assert!(!app.should_quit());
    assert!(app.last_ctrl_c.is_none());

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(!app.should_quit());
    assert!(app.last_ctrl_c.is_some());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(app.should_quit());
}

#[test]
fn ctrl_c_during_generation_aborts_without_arming_quit_countdown() {
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::Abort
    );
    assert_eq!(app.session_state(), SessionState::Generating);
    assert!(app.abort_in_flight);
    assert!(!app.should_quit());
    assert!(app.last_ctrl_c.is_none());

    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Abort {
                origin: app.command_origin(),
            },
            Ok(()),
        ),
        AppAction::None
    );
    assert_eq!(app.session_state(), SessionState::Idle);

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(!app.should_quit());
}

#[test]
fn ctrl_c_after_completed_turn_still_aborts_active_generation() {
    let mut app = App::new(project());
    app.composer.set_text("first");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { prompt, .. } if prompt == "first"
    ));
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    app.composer.set_text("second");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { prompt, .. } if prompt == "second"
    ));

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::Abort
    );
    assert!(app.abort_in_flight);
    assert!(!app.should_quit());
}

#[test]
fn ctrl_c_is_global_for_overlay_picker_approval_and_ask_form() {
    let mut app = App::new(project());
    app.open_transcript_overlay();
    app.set_session_state(SessionState::Generating);
    assert!(app.transcript_overlay().is_some());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::Abort
    );
    assert!(app.transcript_overlay().is_none());

    let mut app = App::new(project());
    app.open_model_picker(json!({"chat_models": {"m1": {"name": "Model One"}}}));
    assert!(app.modal_picker().is_some());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(app.modal_picker().is_none());
    assert!(!app.should_quit());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(app.should_quit());

    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));
    assert!(app.approval_modal().is_some());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::Abort
    );
    assert!(app.approval_modal().is_some());

    let mut app = App::new(project());
    app.handle_chat_event(ask_questions_tool_event(
        &app,
        "call-ask",
        json!([
            {"id": "confirm", "type": "yes_no", "text": "Proceed?"}
        ]),
    ));
    app.handle_chat_event(waiting_user_input_event(&app));
    assert!(app.ask_questions_form().is_some());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::Abort
    );
    assert!(app.ask_questions_form().is_some());
}

#[tokio::test]
async fn active_generation_quit_sends_abort_command() {
    let state = CommandState::default();
    let base_url = spawn_command_server(state.clone());
    let client = DaemonClient::new(base_url, None).unwrap();
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
    assert_eq!(action, AppAction::Quit { abort_active: true });
    assert!(!app.should_quit());
    assert_eq!(app.session_state(), SessionState::Generating);

    let (tx, _rx) = mpsc::channel(1);
    let mut subscriptions = SubscriptionManager::new();
    let mut daemon_events = DaemonEventSubscription::new();
    run_action(
        &mut app,
        action,
        &client,
        &tx,
        &mut subscriptions,
        &mut daemon_events,
        crate::client::DaemonUrlSource::Discovery,
    )
    .await;

    assert!(state.wait_for("abort"));
    assert!(app.should_quit());
    assert_eq!(app.session_state(), SessionState::Idle);
}
