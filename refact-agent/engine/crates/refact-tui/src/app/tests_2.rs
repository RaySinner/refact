use super::*;
use super::tests_util::*;

#[test]
fn new_chat_handoff_preserves_old_session_queue_for_ctrl_n_and_new() {
    let mut app = App::new(project());
    let chat_a = app.chat_id().to_string();
    app.input_queue
        .enqueue("queued for A".to_string(), Value::Null);
    app.composer.set_text("draft for A");

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        AppAction::SubscribeCurrent
    );
    assert!(app.input_queue().is_empty());
    assert!(app.composer().is_empty());
    assert_eq!(
        app.handle_chat_event(runtime_updated_event(&app, "idle")),
        AppAction::None
    );

    app.resume_chat(chat_a.clone(), "Chat A".to_string(), None);

    assert_eq!(app.composer(), "draft for A");
    assert_eq!(app.input_queue().items()[0].text, "queued for A");

    app.composer.set_text("/new");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SubscribeCurrent
    );
    assert!(app.input_queue().is_empty());
    assert!(app.composer().is_empty());
    assert_eq!(
        app.handle_chat_event(runtime_updated_event(&app, "idle")),
        AppAction::None
    );

    app.resume_chat(chat_a, "Chat A".to_string(), None);

    assert!(app.composer().is_empty());
    assert_eq!(app.input_queue().items()[0].text, "queued for A");
}

#[test]
fn queued_input_dispatches_only_for_explicit_idle_runtime_state() {
    let invalid_states = [
        ("missing", json!({}), "omitted state"),
        ("null", json!({"state": null}), "omitted state"),
        (
            "unknown",
            json!({"state": "not_a_runtime_state"}),
            "Unknown runtime state",
        ),
    ];

    for (event_name, snapshot) in [("runtime update", false), ("snapshot", true)] {
        for (state_name, raw, notice) in &invalid_states {
            let mut app = app_with_queued_input();
            let action = app.handle_chat_event(ChatEvent {
                chat_id: Some(app.chat_id().to_string()),
                seq: None,
                kind: if snapshot {
                    "snapshot".to_string()
                } else {
                    "runtime_updated".to_string()
                },
                raw: if snapshot {
                    json!({"runtime": raw, "messages": []})
                } else {
                    raw.clone()
                },
            });

            assert_eq!(action, AppAction::None, "{event_name} {state_name}");
            assert_eq!(app.session_state(), SessionState::Generating);
            assert_eq!(app.input_queue().len(), 1);
            assert!(app.visible_transcript().iter().any(|item| {
                matches!(item, TranscriptItem::Notice(text) if text.contains(notice))
            }));
        }

        let mut app = app_with_queued_input();
        let action = app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: if snapshot {
                "snapshot".to_string()
            } else {
                "runtime_updated".to_string()
            },
            raw: if snapshot {
                json!({"runtime": {"state": "idle"}, "messages": []})
            } else {
                json!({"state": "idle"})
            },
        });

        assert!(matches!(
            action,
            AppAction::SendMessage { prompt, .. } if prompt == "second"
        ));
        assert!(app.input_queue().is_empty());
        assert_eq!(app.session_state(), SessionState::Generating);
    }
}

#[test]
fn unacknowledged_send_timeout_rolls_back_and_restores_prompt() {
    let mut app = App::new(project());
    app.composer.set_text("hello");
    let action = app.handle_key(key(KeyCode::Enter));
    let (prompt, params, correlation) = match action {
        AppAction::SendMessage {
            prompt,
            params,
            correlation,
        } => (prompt, params, correlation),
        other => panic!("unexpected action: {other:?}"),
    };
    app.composer.set_text("draft");

    let result = app.handle_command_finished(
        CommandContextTag::SendMessage {
            prompt: prompt.clone(),
            params: params.clone(),
            correlation: correlation.clone(),
        },
        Err("request timed out".to_string()),
    );

    assert_eq!(result, AppAction::None);
    assert_eq!(app.session_state(), SessionState::Idle);
    assert_eq!(app.composer(), "hello");
    assert_eq!(app.input_queue().len(), 1);
    assert_eq!(app.input_queue().items()[0].text, "draft");
    assert!(!app
        .transcript_state()
        .messages()
        .iter()
        .any(|message| message.role == TranscriptRole::Assistant));
    assert!(!app
        .visible_transcript()
        .iter()
        .any(|item| matches!(item, TranscriptItem::Assistant(_))));
    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Notice(text) if text.contains("request timed out"))
    }));

    let retry = app.handle_key(key(KeyCode::Enter));
    match retry {
        AppAction::SendMessage {
            prompt: retry_prompt,
            params: retry_params,
            correlation: retry_correlation,
        } => {
            assert_eq!(retry_prompt, prompt);
            assert_eq!(retry_params, params);
            assert_eq!(retry_correlation, correlation);
        }
        other => panic!("unexpected retry action: {other:?}"),
    }
    assert_eq!(app.input_queue().len(), 1);
    assert_eq!(app.input_queue().items()[0].text, "draft");
}

#[test]
fn accepted_send_timeout_keeps_one_optimistic_turn() {
    let mut app = App::new(project());
    app.composer.set_text("hello");
    let (prompt, params, correlation) = match app.handle_key(key(KeyCode::Enter)) {
        AppAction::SendMessage {
            prompt,
            params,
            correlation,
        } => (prompt, params, correlation),
        other => panic!("unexpected action: {other:?}"),
    };

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "ack".to_string(),
        raw: json!({
            "client_request_id": correlation.client_request_id,
            "accepted": true,
        }),
    });
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::SendMessage {
                prompt,
                params,
                correlation,
            },
            Err("request timed out".to_string()),
        ),
        AppAction::None
    );

    assert_eq!(app.session_state(), SessionState::Generating);
    assert_eq!(app.composer(), "");
    assert!(app.pending_send_retry.is_none());
    assert!(app.in_flight_send.is_none());
    assert_eq!(
        app.transcript_state()
            .messages()
            .iter()
            .filter(|message| message.role == TranscriptRole::User)
            .count(),
        1
    );
    assert!(!app
        .visible_transcript()
        .iter()
        .any(|item| matches!(item, TranscriptItem::Notice(text) if text.contains("timed out"))));
}

#[test]
fn matching_client_message_id_reconciles_optimistic_user_echo() {
    let mut app = App::new(project());
    app.composer.set_text("hello");
    let client_message_id = match app.handle_key(key(KeyCode::Enter)) {
        AppAction::SendMessage { correlation, .. } => correlation.client_message_id,
        other => panic!("unexpected action: {other:?}"),
    };

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "message_id": "server-user-1",
            "role": "user",
            "content": "hello",
            "extra": {"client_message_id": client_message_id},
        }}),
    });

    let users = app
        .transcript_state()
        .messages()
        .iter()
        .filter(|message| message.role == TranscriptRole::User)
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].message_id.as_deref(), Some("server-user-1"));
    assert_eq!(
        users[0].client_message_id(),
        Some(client_message_id.as_str())
    );
    assert!(app
        .in_flight_send
        .as_ref()
        .is_some_and(|send| send.accepted));
}

#[test]
fn message_added_reconciliation_uses_authoritative_index_in_alternate_and_native_modes() {
    for native_scrollback in [false, true] {
        let mut app = App::new(project());
        app.set_native_scrollback(native_scrollback);
        app.handle_chat_event(snapshot_event(
                &app,
                vec![
                    json!({"message_id": "a", "role": "assistant", "content": "a", "stream_finished": true}),
                    json!({"message_id": "b", "role": "assistant", "content": "b", "stream_finished": true}),
                    json!({"message_id": "c", "role": "assistant", "content": "c", "stream_finished": true}),
                ],
            ));
        if native_scrollback {
            app.pending_history_insertions(80);
        }

        for (index, message_id, content) in [
            (0, "c", "c updated"),
            (1, "a", "a updated"),
            (2, "a", "a final"),
        ] {
            app.handle_chat_event(ChatEvent {
                chat_id: Some(app.chat_id().to_string()),
                seq: None,
                kind: "message_added".to_string(),
                raw: json!({"index": index, "message": {
                    "message_id": message_id,
                    "role": "assistant",
                    "content": content,
                    "stream_finished": true,
                }}),
            });
        }

        assert_eq!(
            app.transcript_state()
                .messages()
                .iter()
                .map(|message| message.message_id.as_deref())
                .collect::<Vec<_>>(),
            [Some("c"), Some("b"), Some("a")],
        );
        assert_eq!(
            app.transcript_state()
                .messages()
                .iter()
                .filter(|message| message.message_id.as_deref() == Some("a"))
                .count(),
            1,
        );
        let pending_before_snapshot = app.history_pending_count();
        app.handle_chat_event(snapshot_event(&app, vec![
                json!({"message_id": "c", "role": "assistant", "content": "c updated", "stream_finished": true}),
                json!({"message_id": "b", "role": "assistant", "content": "b", "stream_finished": true}),
                json!({"message_id": "a", "role": "assistant", "content": "a final", "stream_finished": true}),
            ]));
        assert_eq!(app.transcript_state().messages().len(), 3);
        if native_scrollback {
            assert_eq!(app.history_pending_count(), pending_before_snapshot);
            assert!(!app.resize_reflow_is_due());
            let text = pending_plain_text(&mut app);
            assert!(!text.contains("c updated"), "{text}");
            assert!(!text.contains("a final"), "{text}");
        } else {
            assert_eq!(assistant_cell_count(&app), 3);
            assert_eq!(assistant_text(&app), "c updatedba final");
        }
    }
}

#[test]
fn optimistic_message_added_reconciliation_uses_authoritative_index() {
    for native_scrollback in [false, true] {
        let mut app = App::new(project());
        app.transcript_state.reset();
        app.set_native_scrollback(native_scrollback);
        app.transcript_state
            .push_optimistic_user_message("same", "client-message-1");
        app.transcript_state
            .add_message(&json!({"message_id": "later", "role": "user", "content": "later"}));
        app.rebuild_render_transcript_from_state();
        if native_scrollback {
            app.pending_history_insertions(80);
        }

        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"index": 1, "message": {
                "message_id": "server-user-1",
                "role": "user",
                "content": "same",
                "extra": {"client_message_id": "client-message-1"},
            }}),
        });

        assert_eq!(
            app.transcript_state()
                .messages()
                .iter()
                .map(|message| message.message_id.as_deref())
                .collect::<Vec<_>>(),
            [Some("later"), Some("server-user-1")],
        );
        if native_scrollback {
            assert!(!app.resize_reflow_is_due());
            let text = pending_plain_text(&mut app);
            assert!(!text.contains("same"), "{text}");
            assert!(!text.contains("later"), "{text}");
        }
    }
}

#[test]
fn message_added_reconciliation_keeps_the_active_assistant_stream() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_started".to_string(),
        raw: json!({"message_id": "active"}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"message_id": "active", "ops": [{"op": "append_content", "text": "before"}]}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"index": 0, "message": {
            "message_id": "active",
            "role": "assistant",
            "content": "before",
            "stream_finished": false,
        }}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"message_id": "active", "ops": [{"op": "append_content", "text": " after"}]}),
    });

    assert_eq!(app.transcript_state().messages()[0].content, "before after");
    assert!(assistant_text(&app).contains("before after"));
}

#[test]
fn authoritative_insertions_enqueue_native_cells_in_server_order() {
    for native_scrollback in [false, true] {
        let mut app = App::new(project());
        app.set_native_scrollback(native_scrollback);
        if native_scrollback {
            app.pending_history_insertions(80);
        }
        app.handle_chat_event(snapshot_event(
                &app,
                vec![
                    json!({"message_id": "a1", "role": "assistant", "content": "one", "stream_finished": true}),
                    json!({"message_id": "a3", "role": "assistant", "content": "three", "stream_finished": true}),
                ],
            ));
        if native_scrollback {
            app.pending_history_insertions(80);
            app.resize_reflow.clear_pending_reflow();
        }

        for (index, message_id, content) in
            [(1, "a2", "middle"), (0, "a0", "start"), (4, "a4", "end")]
        {
            app.handle_chat_event(ChatEvent {
                chat_id: Some(app.chat_id().to_string()),
                seq: None,
                kind: "message_added".to_string(),
                raw: json!({"index": index, "message": {
                    "message_id": message_id,
                    "role": "assistant",
                    "content": content,
                    "stream_finished": true,
                }}),
            });
        }

        assert_eq!(
            app.transcript_state()
                .messages()
                .iter()
                .map(|message| message.message_id.as_deref())
                .collect::<Vec<_>>(),
            [Some("a0"), Some("a1"), Some("a2"), Some("a3"), Some("a4")],
        );
        if native_scrollback {
            assert!(!app.resize_reflow_is_due());
            let text = pending_plain_text(&mut app);
            for content in ["start", "middle", "end"] {
                assert_eq!(text.matches(content).count(), 1, "{content}: {text}");
            }
            for content in ["one", "three"] {
                assert_eq!(text.matches(content).count(), 0, "{content}: {text}");
            }
            assert_eq!(app.history_pending_count(), 0);
        } else {
            assert_eq!(assistant_cell_count(&app), 5);
            assert_eq!(assistant_text(&app), "startonemiddlethreeend");
        }
    }
}

#[test]
fn authoritative_correction_leaves_inserted_native_cell_untouched() {
    for native_scrollback in [false, true] {
        let mut app = App::new(project());
        app.set_native_scrollback(native_scrollback);
        if native_scrollback {
            app.pending_history_insertions(80);
        }
        app.handle_chat_event(snapshot_event(
            &app,
            vec![json!({
                "message_id": "a1",
                "role": "assistant",
                "content": "stale",
                "stream_finished": true,
            })],
        ));
        if native_scrollback {
            app.pending_history_insertions(80);
            app.resize_reflow.clear_pending_reflow();
        }

        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_updated".to_string(),
            raw: json!({"message_id": "a1", "message": {
                "message_id": "a1",
                "role": "assistant",
                "content": "corrected",
                "stream_finished": true,
            }}),
        });

        if native_scrollback {
            assert!(!app.resize_reflow_is_due());
            let text = pending_plain_text(&mut app);
            assert!(!text.contains("corrected"), "{text}");
            assert!(!text.contains("stale"), "{text}");
        } else {
            assert_eq!(assistant_cell_count(&app), 1);
            assert_eq!(assistant_text(&app), "corrected");
        }
    }
}

#[test]
fn identical_user_echoes_with_distinct_client_message_ids_remain_distinct() {
    let mut app = App::new(project());
    app.transcript_state
        .push_optimistic_user_message("same", "client-message-1");
    app.transcript_state
        .push_optimistic_user_message("same", "client-message-2");
    app.rebuild_render_transcript_from_state();

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "message_id": "server-user-1",
            "role": "user",
            "content": "same",
            "extra": {"client_message_id": "client-message-1"},
        }}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "message_id": "server-user-2",
            "role": "user",
            "content": "same",
            "extra": {"client_message_id": "client-message-2"},
        }}),
    });

    let users = app
        .transcript_state()
        .messages()
        .iter()
        .filter(|message| message.role == TranscriptRole::User)
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0].message_id.as_deref(), Some("server-user-1"));
    assert_eq!(users[1].message_id.as_deref(), Some("server-user-2"));
}

#[test]
fn user_echo_with_unknown_client_message_id_appends_normally() {
    let mut app = App::new(project());
    app.transcript_state
        .push_optimistic_user_message("same", "client-message-known");
    app.rebuild_render_transcript_from_state();

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "message_id": "server-user-unknown",
            "role": "user",
            "content": "same",
            "extra": {"client_message_id": "client-message-unknown"},
        }}),
    });

    assert_eq!(
        app.transcript_state()
            .messages()
            .iter()
            .filter(|message| message.role == TranscriptRole::User)
            .count(),
        2
    );
}

#[test]
fn user_echo_without_client_message_id_does_not_guess_by_content() {
    let mut app = App::new(project());
    app.transcript_state
        .push_optimistic_user_message("same", "client-message-known");
    app.rebuild_render_transcript_from_state();

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "message_id": "legacy-server-user",
            "role": "user",
            "content": "same",
        }}),
    });

    let users = app
        .transcript_state()
        .messages()
        .iter()
        .filter(|message| message.role == TranscriptRole::User)
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0].message_id, None);
    assert_eq!(users[1].message_id.as_deref(), Some("legacy-server-user"));
}

#[test]
fn unrelated_ack_does_not_complete_current_send() {
    let mut app = App::new(project());
    app.composer.set_text("hello");
    let (prompt, params, correlation) = match app.handle_key(key(KeyCode::Enter)) {
        AppAction::SendMessage {
            prompt,
            params,
            correlation,
        } => (prompt, params, correlation),
        other => panic!("unexpected action: {other:?}"),
    };

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "ack".to_string(),
        raw: json!({"client_request_id": "stale-request", "accepted": true}),
    });
    app.handle_command_finished(
        CommandContextTag::SendMessage {
            prompt,
            params,
            correlation,
        },
        Err("request timed out".to_string()),
    );

    assert_eq!(app.session_state(), SessionState::Idle);
    assert_eq!(app.composer(), "hello");
    assert!(app.pending_send_retry.is_some());
}

#[test]
fn new_logical_send_gets_a_new_correlation_pair() {
    let mut app = App::new(project());
    app.composer.set_text("first");
    let first = match app.handle_key(key(KeyCode::Enter)) {
        AppAction::SendMessage { correlation, .. } => correlation,
        other => panic!("unexpected action: {other:?}"),
    };
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    app.composer.set_text("second");
    let second = match app.handle_key(key(KeyCode::Enter)) {
        AppAction::SendMessage { correlation, .. } => correlation,
        other => panic!("unexpected action: {other:?}"),
    };

    assert_ne!(first.client_request_id, second.client_request_id);
    assert_ne!(first.client_message_id, second.client_message_id);
}

#[test]
fn abort_success_dispatches_next_queued_input_once() {
    let mut app = App::new(project());
    app.composer.set_text("first");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { prompt, .. } if prompt == "first"
    ));
    app.composer.set_text("second");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    app.composer.set_text("third");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert_eq!(app.input_queue().len(), 2);
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::Abort);
    assert_eq!(app.session_state(), SessionState::Generating);
    assert!(app.abort_in_flight);

    let action = app.handle_command_finished(
        CommandContextTag::Abort {
            origin: app.command_origin(),
        },
        Ok(()),
    );

    assert!(matches!(
        action,
        AppAction::SendMessage { prompt, .. } if prompt == "second"
    ));
    assert_eq!(app.session_state(), SessionState::Generating);
    assert!(!app.abort_in_flight);
    assert_eq!(app.input_queue().len(), 1);
    assert_eq!(app.input_queue().items()[0].text, "third");
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Abort {
                origin: app.command_origin(),
            },
            Ok(()),
        ),
        AppAction::None
    );
    assert_eq!(app.input_queue().len(), 1);
}

#[test]
fn abort_success_with_empty_queue_stays_idle() {
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::Abort);
    assert_eq!(app.session_state(), SessionState::Generating);
    assert!(app.abort_in_flight);

    let action = app.handle_command_finished(
        CommandContextTag::Abort {
            origin: app.command_origin(),
        },
        Ok(()),
    );

    assert_eq!(action, AppAction::None);
    assert_eq!(app.session_state(), SessionState::Idle);
    assert!(!app.abort_in_flight);
    assert!(app.input_queue().is_empty());
}

#[test]
fn queued_item_can_be_edited_and_removed() {
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);
    app.composer.set_text("draft");
    app.handle_key(key(KeyCode::Enter));
    app.composer.set_text("keep draft");
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.input_queue().selected_index(), Some(0));
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert_eq!(app.composer(), "draft");
    app.composer.set_text("edited");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert_eq!(app.composer(), "keep draft");
    assert_eq!(app.input_queue().items()[0].text, "edited");

    app.composer.clear();
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.handle_key(key(KeyCode::Delete)), AppAction::None);
    assert!(app.input_queue().is_empty());
}

#[test]
fn delete_with_draft_does_not_remove_selected_queue_item() {
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);
    app.composer.set_text("queued");
    app.handle_key(key(KeyCode::Enter));
    app.composer.set_text("draft");
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.input_queue().selected_index(), Some(0));

    assert_eq!(app.handle_key(key(KeyCode::Delete)), AppAction::None);

    assert_eq!(app.input_queue().len(), 1);
    assert_eq!(app.input_queue().items()[0].text, "queued");
    assert_eq!(app.composer(), "draf");
    assert_eq!(app.input_queue().selected_index(), None);
}

#[test]
fn abort_retains_queue_and_restores_edit_draft() {
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);
    app.composer.set_text("queued");
    app.handle_key(key(KeyCode::Enter));
    app.composer.set_text("draft");
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.composer(), "queued");

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
        AppAction::None
    );
    assert_eq!(app.composer(), "draft");
    assert_eq!(app.input_queue().len(), 1);

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
        AppAction::Abort
    );
    assert_eq!(app.composer(), "draft");
    assert_eq!(app.input_queue().len(), 1);
    assert_eq!(app.session_state(), SessionState::Generating);
    assert!(app.abort_in_flight);
}

#[test]
fn finishing_queue_edit_while_idle_dispatches_next_item() {
    let mut app = App::new(project());
    app.set_session_state(SessionState::Generating);
    app.composer.set_text("queued");
    app.handle_key(key(KeyCode::Enter));
    app.composer.set_text("draft");
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Enter));
    app.set_session_state(SessionState::Idle);
    app.composer.set_text("edited");

    let action = app.handle_key(key(KeyCode::Enter));

    assert!(matches!(
        action,
        AppAction::SendMessage { prompt, .. } if prompt == "edited"
    ));
    assert!(app.input_queue().is_empty());
    assert_eq!(app.composer(), "draft");
}

#[test]
fn queue_updated_events_are_passive_server_state() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "queue_updated".to_string(),
        raw: json!({"queue_size": 1, "queued_items": [{"preview": "server-side"}]}),
    });

    assert_eq!(app.server_queue_size(), 1);
    assert_eq!(app.server_queue_previews(), &["server-side".to_string()]);
    assert!(app.input_queue().is_empty());
}

#[test]
fn slash_popup_filters_and_executes_local_command() {
    let mut app = App::new(project());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::empty())),
        AppAction::None
    );
    assert_eq!(app.composer(), "/");
    assert!(app.modal_picker().is_some());
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::empty()));
    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::empty()));
    let picker = app.modal_picker().unwrap();
    assert_eq!(picker.filtered_items()[0].id, "status");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::LoadDaemonStatus
    );
    assert!(app
        .visible_transcript()
        .iter()
        .any(|item| matches!(item, TranscriptItem::Status(_, _))));
}

#[test]
fn at_opens_file_popup_and_accept_inserts_mention() {
    let mut app = App::new(project());
    app.composer.set_text("read ");
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::empty())),
        AppAction::LoadFileMentions { query, .. } if query == "read @"
    ));
    app.open_file_mention_picker(vec![PickerItem {
        id: "src/lib.rs".to_string(),
        title: "src/lib.rs".to_string(),
        description: "file mention".to_string(),
    }]);
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert_eq!(app.composer(), "read @src/lib.rs ");
}

#[test]
fn misc_command_group_parses_and_dispatches() {
    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("events"),
        AppAction::RefreshWorkers
    );
    assert!(app.events_pane().open);
    assert_eq!(
        app.execute_command_name("events"),
        AppAction::RefreshWorkers
    );
    assert!(!app.events_pane().open);

    assert_eq!(app.execute_command_name("help"), AppAction::None);
    assert!(app.help_open());

    let mut app = App::new(project());
    assert_eq!(app.execute_command_name("keymap"), AppAction::None);
    assert!(app.help_open());

    let mut app = App::new(project());
    assert_eq!(app.execute_command_name("vim"), AppAction::None);
    assert!(app.vim_enabled());

    let mut app = App::new(project());
    assert_eq!(app.execute_command_name("debug-config"), AppAction::None);
    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Info(lines) if lines.iter().any(|line| line.contains("Commands:")))
        }));

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("quit"),
        AppAction::Quit {
            abort_active: false
        }
    );
    assert!(app.should_quit());
}

#[test]
fn worker_list_with_null_ports_reaches_events_pane() {
    let mut app = App::new(project());
    let worker: WorkerInfo = serde_json::from_value(json!({
        "project_id": "stopping",
        "slug": "stopping-project",
        "root": "/tmp/stopping",
        "root_exists": false,
        "pinned": false,
        "last_active_ms": 0,
        "state": "stopping",
        "pid": null,
        "rss_bytes": null,
        "cpu_percent": null,
        "uptime_secs": null,
        "http_port": null,
        "lsp_port": null,
        "lsp_clients": 0,
        "busy_chats": 0,
        "exec_running": 0,
        "live_proxy_streams": 0,
        "cron_next_fire_ms": null,
        "idle_deadline_ms": null,
        "last_status_report_ms": null,
        "last_error": null,
        "log_path": "/tmp/stopping.log"
    }))
    .unwrap();

    app.set_workers(vec![worker]);

    assert_eq!(app.events_pane().workers().len(), 1);
    assert_eq!(app.events_pane().workers()[0].http_port, None);
    assert_eq!(app.events_pane().workers()[0].lsp_port, None);
}

#[test]
fn theme_command_applies_theme_live() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("tui.toml");
    let mut app = App::new(project());
    app.test_set_tui_config_path(config_path.clone());
    assert_eq!(app.theme().name(), "dark");
    assert_eq!(app.execute_command_name("theme light"), AppAction::None);
    assert_eq!(app.theme().name(), "light");
    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Notice(text) if text.contains("Theme set to light"))
    }));
    assert!(std::fs::read_to_string(&config_path)
        .unwrap()
        .contains("name = \"light\""));

    let mut app = App::new(project());
    assert_eq!(app.execute_command_name("theme"), AppAction::None);
    let picker = app.modal_picker().unwrap();
    assert_eq!(picker.kind, PickerKind::Theme);
    assert_eq!(picker.filtered_items()[0].id, "dark");
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.theme().name(), "light");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert_eq!(app.theme().name(), "light");
}

#[test]
fn theme_picker_cancel_restores_previewed_theme() {
    let mut app = App::new(project());
    assert_eq!(app.execute_command_name("theme"), AppAction::None);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.theme().name(), "light");

    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::None);

    assert!(app.modal_picker().is_none());
    assert_eq!(app.theme().name(), "dark");
}

#[test]
fn theme_config_helpers_update_or_create_theme_section() {
    assert_eq!(
        surfaces::apply_theme_name_to_config("", "light"),
        "[theme]\nname = \"light\"\n"
    );
    assert_eq!(
        surfaces::apply_theme_name_to_config("vim = true\n\n[theme]\naccent = \"cyan\"\n", "plain"),
        "vim = true\n\n[theme]\naccent = \"cyan\"\nname = \"plain\"\n"
    );
    assert_eq!(
        surfaces::apply_theme_name_to_config(
            "[theme]\nname = \"dark\"\n\n[bindings]\nsend = \"ctrl-s\"\n",
            "light"
        ),
        "[theme]\nname = \"light\"\n\n[bindings]\nsend = \"ctrl-s\"\n"
    );
}

#[test]
fn help_command_shows_generated_keymap_help() {
    let mut app = App::new(project());
    assert_eq!(app.execute_command_name("help"), AppAction::None);
    assert!(app.help_open());
    let rows = app.keymap_help_rows();
    assert!(rows
        .iter()
        .any(|row| row.action == Some(KeyAction::ShowHelp)));
    assert!(rows
        .iter()
        .any(|row| row.description.contains("keymap help")));
}

#[test]
fn read_only_view_commands_open_loading_overlays() {
    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("mcp"),
        AppAction::LoadReadOnlyView {
            view: ReadOnlyView::Mcp
        }
    );
    assert!(app.transcript_overlay().is_some_and(|overlay| {
        overlay.title() == "MCP" && overlay.lines().join("\n").contains("Loading /mcp")
    }));

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("skills"),
        AppAction::LoadReadOnlyView {
            view: ReadOnlyView::Skills
        }
    );
    assert!(app.transcript_overlay().is_some_and(|overlay| {
        overlay.title() == "Skills" && overlay.lines().join("\n").contains("Loading /skills")
    }));

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("memories"),
        AppAction::LoadReadOnlyView {
            view: ReadOnlyView::Memories
        }
    );
    assert!(app.transcript_overlay().is_some_and(|overlay| {
        overlay.title() == "Memories" && overlay.lines().join("\n").contains("Loading /memories")
    }));

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("hooks"),
        AppAction::LoadReadOnlyView {
            view: ReadOnlyView::Hooks
        }
    );
    assert!(app.transcript_overlay().is_some_and(|overlay| {
        overlay.title() == "Hooks" && overlay.lines().join("\n").contains("Loading /hooks")
    }));

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("import"),
        AppAction::LoadCompetitorImportSources
    );
    assert!(app.transcript_overlay().is_some_and(|overlay| {
        overlay.title() == "Import" && overlay.lines().join("\n").contains("Loading /import")
    }));
}

#[test]
fn board_command_is_feature_gated_and_uses_current_task() {
    let _surface_lock = goal_dock::test_surface_lock();
    let previous = std::env::var_os("REFACT_TUI_SURFACES");
    std::env::remove_var("REFACT_TUI_SURFACES");
    let mut app = App::new(project());
    app.test_set_task_context(Some("task-1".to_string()), Some("task_planner".to_string()));
    assert_eq!(app.execute_command_name("board"), AppAction::None);

    std::env::set_var("REFACT_TUI_SURFACES", "1");
    assert_eq!(
        app.execute_command_name("tasks"),
        AppAction::LoadTaskBoard {
            task_id: Some("task-1".to_string())
        }
    );
    match previous {
        Some(value) => std::env::set_var("REFACT_TUI_SURFACES", value),
        None => std::env::remove_var("REFACT_TUI_SURFACES"),
    }
}

#[test]
fn raw_command_opens_copy_mode_overlay() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"runtime": {"state": "idle"}, "messages": [
                {"message_id": "u1", "role": "user", "content": "hello"},
                {
                    "message_id": "a1",
                    "role": "assistant",
                    "content": "hi",
                    "tool_calls": [{"id": "call-1", "function": {"name": "t_process_start", "arguments": "{}"}}]
                }
            ]}),
        });
    assert_eq!(app.execute_command_name("raw"), AppAction::None);
    let overlay = app.transcript_overlay().unwrap();
    assert!(overlay.is_copy_mode());
    assert!(overlay.lines().join("\n").contains("t_process_start"));
}

#[test]
fn read_only_view_loaded_opens_rendered_overlay() {
    let mut app = App::new(project());
    app.handle_skills_view_loaded(Ok(SlashCommandsListResponse {
        commands: Vec::new(),
        skills: vec![crate::client::SkillInfo {
            name: "explain".to_string(),
            description: "Explain code".to_string(),
            user_invocable: Some(true),
            source: "project_refact".to_string(),
        }],
    }));

    let overlay = app.transcript_overlay().unwrap();
    assert_eq!(overlay.title(), "Skills");
    assert!(overlay.lines().join("\n").contains("/explain"));
}

#[test]
fn hooks_view_loaded_opens_rendered_overlay() {
    let mut app = App::new(project());
    app.handle_hooks_view_loaded(Ok(HooksResponse {
        hooks: vec![crate::client::HookInfo {
            event: "PreToolUse".to_string(),
            matcher: Some("Bash".to_string()),
            command: "./check.sh".to_string(),
            timeout: Some(30),
        }],
        raw_content: "hooks: {}".to_string(),
        file_path: "/repo/.refact/hooks.yaml".to_string(),
    }));

    let overlay = app.transcript_overlay().unwrap();
    assert_eq!(overlay.title(), "Hooks");
    assert!(overlay.lines().join("\n").contains("./check.sh"));
}

#[test]
fn logout_command_dispatches_arg_or_picker_load() {
    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("logout"),
        AppAction::LoadProviderLogoutChoices
    );

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("logout openai_codex"),
        AppAction::ProviderOAuthLogout {
            provider: "openai_codex".to_string()
        }
    );
}

#[test]
fn logout_provider_picker_filters_oauth_credentials() {
    let mut app = App::new(project());
    app.handle_provider_logout_choices_loaded(Ok(ProviderListResponse {
        providers: vec![
            crate::client::ProviderListItem {
                name: "openai_codex".to_string(),
                base_provider: "openai_codex".to_string(),
                display_name: "OpenAI Codex".to_string(),
                enabled: Some(true),
                readonly: Some(false),
                has_credentials: Some(true),
                status: "configured".to_string(),
                model_count: Some(1),
            },
            crate::client::ProviderListItem {
                name: "openrouter".to_string(),
                base_provider: "openrouter".to_string(),
                display_name: "OpenRouter".to_string(),
                enabled: Some(true),
                readonly: Some(false),
                has_credentials: Some(true),
                status: "configured".to_string(),
                model_count: Some(1),
            },
        ],
    }));

    let picker = app.modal_picker().unwrap();
    assert_eq!(picker.kind, PickerKind::ProviderLogout);
    assert_eq!(picker.filtered_items().len(), 1);
    assert_eq!(picker.filtered_items()[0].id, "openai_codex");
}

#[test]
fn provider_logout_finished_reports_status() {
    let mut app = App::new(project());
    app.handle_provider_logout_finished(
        "openai_codex".to_string(),
        Ok(ProviderOAuthLogoutResponse {
            success: Some(true),
            auth_status: "No credentials found".to_string(),
        }),
    );

    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Notice(text) if text.contains("Logged out openai_codex"))
    }));
}

#[test]
fn import_command_dispatches_list_and_run_forms() {
    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("import"),
        AppAction::LoadCompetitorImportSources
    );

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("import claude_code global"),
        AppAction::RunCompetitorImport {
            source: Some("claude_code".to_string()),
            scope: "global".to_string(),
        }
    );

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("import all"),
        AppAction::RunCompetitorImport {
            source: None,
            scope: "project".to_string(),
        }
    );
}

#[test]
fn import_sources_loaded_opens_picker_and_overlay() {
    let mut app = App::new(project());
    app.handle_competitor_import_sources_loaded(Ok(CompetitorImportInfoResponse {
        sources: vec![crate::client::CompetitorImportSourceInfo {
            id: "claude_code".to_string(),
            label: "Claude Code".to_string(),
            roots: vec!["~/.claude".to_string()],
        }],
    }));

    let picker = app.modal_picker().unwrap();
    assert_eq!(picker.kind, PickerKind::CompetitorImport);
    assert!(picker
        .filtered_items()
        .iter()
        .any(|item| item.id == "claude_code"));
    assert!(app.transcript_overlay().is_some_and(|overlay| {
        overlay.title() == "Import" && overlay.lines().join("\n").contains("claude_code")
    }));
}

#[test]
fn import_finished_reports_notice_and_overlay() {
    let mut status_counts = std::collections::BTreeMap::new();
    status_counts.insert(crate::client::ImportStatus::Created, 1);
    let mut app = App::new(project());
    app.handle_competitor_import_finished(Ok(CompetitorImportRunResponse {
        scope: "project".to_string(),
        source: Some("claude_code".to_string()),
        report: crate::client::ImportReport {
            completed_at: None,
            reported_sources: Vec::new(),
            discovered_candidates: Some(1),
            status_counts,
            competitor_counts: std::collections::BTreeMap::new(),
            kind_counts: std::collections::BTreeMap::new(),
            top_issues: Vec::new(),
        },
    }));

    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Notice(text) if text.contains("/import claude_code project complete"))
        }));
    assert!(app.transcript_overlay().is_some_and(|overlay| {
        overlay.title() == "Import" && overlay.lines().join("\n").contains("created 1")
    }));
}

#[test]
fn read_only_view_failure_reports_explicit_notice_and_overlay() {
    let mut app = App::new(project());
    app.handle_memories_view_loaded(Err("boom".to_string()));

    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Notice(text) if text.contains("Failed to load /memories: boom"))
        }));
    assert!(app.transcript_overlay().is_some_and(|overlay| {
        overlay.title() == "Memories" && overlay.lines().join("\n").contains("boom")
    }));
}

#[test]
fn copy_command_emits_last_assistant_as_osc52_payload() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"runtime": {"state": "idle"}, "messages": [
            {"message_id": "u1", "role": "user", "content": "hello"},
            {"message_id": "a1", "role": "assistant", "content": "old"},
            {"message_id": "a2", "role": "assistant", "content": "new"}
        ]}),
    });

    let action = app.execute_command_name("copy");

    let AppAction::CopyToClipboard { text, source } = action else {
        panic!("expected copy action");
    };
    assert_eq!(source, ClipboardCopySource::LastAssistant);
    assert_eq!(text, "new");
    let mut output = Vec::new();
    let report = crate::clipboard::write_osc52_copy(&mut output, &text, false).unwrap();
    assert_eq!(output, b"\x1b]52;c;bmV3\x07");
    assert_eq!(report.copied_bytes, 3);
    assert!(!report.truncated);
}

#[test]
fn overlay_y_yanks_visible_raw_text() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"runtime": {"state": "idle"}, "messages": [
            {"message_id": "u1", "role": "user", "content": "hello"},
            {"message_id": "a1", "role": "assistant", "content": "hi"}
        ]}),
    });
    app.execute_command_name("raw");
    app.set_transcript_overlay_visible_height(2);

    let action = app.handle_key(key(KeyCode::Char('y')));

    assert!(matches!(
        action,
        AppAction::CopyToClipboard { text, source }
            if source == ClipboardCopySource::OverlayVisible && text == "## user u1\nhello"
    ));
}

#[test]
fn permissions_command_uses_multi_select_picker() {
    let mut app = App::new(project());
    app.test_open_permissions_picker();
    let picker = app.modal_picker().unwrap();
    assert!(picker.is_multi());
    assert_eq!(picker.kind, PickerKind::Permissions);
    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SetParams {
            patch: json!({"auto_approve_editing_tools": true, "auto_approve_dangerous_commands": true})
        }
    );
    assert_eq!(
        app.permission_policy(),
        session::PermissionPolicy {
            auto_approve_editing_tools: true,
            auto_approve_dangerous_commands: true,
        }
    );
    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Notice(text) if text.contains("TUI sends Allow Once") && text.contains("server enforces"))
        }));
}

#[test]
fn session_command_group_parses_and_dispatches() {
    let _surface_lock = goal_dock::test_surface_lock();
    let mut app = App::new(project());
    assert_eq!(app.execute_command_name("new"), AppAction::SubscribeCurrent);
    assert_eq!(app.execute_command_name("resume"), AppAction::LoadSessions);
    assert_eq!(app.execute_command_name("model"), AppAction::LoadModels);
    assert_eq!(app.execute_command_name("mode"), AppAction::LoadModes);
    assert_eq!(app.execute_command_name("permissions"), AppAction::None);
    assert!(matches!(
        app.visible_transcript().last(),
        Some(TranscriptItem::Notice(text)) if text == "/permissions requires REFACT_TUI_SURFACES=1"
    ));

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
        raw: json!({"thread": {"model": "gpt-demo"}, "runtime": {"state": "idle"}, "messages": []}),
    });
    assert_eq!(app.execute_command_name("reasoning"), AppAction::None);
    assert!(app
        .modal_picker()
        .is_some_and(|picker| picker.kind == PickerKind::Reasoning));

    let mut app = App::new(project());
    assert_eq!(
        app.execute_command_name("status"),
        AppAction::LoadDaemonStatus
    );
    assert!(app
        .visible_transcript()
        .iter()
        .any(|item| matches!(item, TranscriptItem::Status(_, _))));

    let mut app = App::new(project());
    assert!(matches!(
        app.execute_command_name("init"),
        AppAction::SendMessage { prompt, .. } if prompt.contains("bootstrap this project")
    ));
}

#[test]
fn model_command_opens_picker_after_caps_load() {
    let mut app = App::new(project());
    assert_eq!(app.execute_command_name("model"), AppAction::LoadModels);
    app.open_model_picker(json!({"chat_models": {"m1": {"name": "Model One"}}}));
    let picker = app.modal_picker().unwrap();
    assert_eq!(picker.kind, PickerKind::Model);
    assert_eq!(picker.filtered_items()[0].id, "m1");
}

#[test]
fn caps_loaded_default_model_populates_footer_model_and_mode() {
    let mut app = App::new(project());

    app.apply_caps(&json!({
        "defaults": {
            "chat_default_model": "openai/gpt-demo",
            "chat_default_mode": "agent"
        },
        "chat_models": {
            "openai/gpt-demo": {"name": "GPT Demo", "n_ctx": 128_000}
        }
    }));

    assert_eq!(app.model(), Some("openai/gpt-demo"));
    assert_eq!(app.mode(), Some("agent"));
    let footer = crate::ui::footer::FooterData::from_app(&app);
    assert_eq!(footer.model, "openai/gpt-demo");
    assert_ne!(footer.model, "default");
    assert_eq!(footer.mode, "agent");
}

#[test]
fn empty_snapshot_keeps_caps_default_but_thread_updates_still_win() {
    let mut app = App::new(project());
    app.apply_caps(&json!({
        "defaults": {"chat_default_model": "openai/gpt-demo"},
        "chat_models": {"openai/gpt-demo": {"name": "GPT Demo"}}
    }));

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"thread": {"model": "", "mode": ""}, "messages": []}),
    });
    assert_eq!(app.model(), Some("openai/gpt-demo"));

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "thread_updated".to_string(),
        raw: json!({"params": {"model": "openai/gpt-other", "mode": "chat"}}),
    });
    assert_eq!(app.model(), Some("openai/gpt-other"));
    assert_eq!(app.mode(), Some("chat"));
}

#[test]
fn model_and_mode_pickers_preselect_current_values() {
    let mut app = App::new(project());
    app.handle_thread_updated(&json!({"model": "openai/gpt-small", "mode": "task_agent"}));

    app.open_model_picker(json!({"chat_models": {
        "openai/gpt-demo": {"name": "GPT Demo"},
        "openai/gpt-small": {"name": "GPT Small"}
    }}));
    assert_eq!(
        app.modal_picker().unwrap().selected_item().unwrap().id,
        "openai/gpt-small"
    );

    app.test_open_mode_picker(json!({"modes": [
        {"id": "agent", "title": "Agent"},
        {"id": "task_agent", "title": "Task Agent"}
    ]}));
    assert_eq!(
        app.modal_picker().unwrap().selected_item().unwrap().id,
        "task_agent"
    );
}

#[test]
fn status_command_snapshot_includes_daemon_worker_session_and_usage() {
    let mut app = App::new(project());
    app.test_set_daemon_status(
        DaemonStatus {
            pid: Some(7),
            version: Some("1.2.3".to_string()),
            port: Some(8488),
            started_at_ms: Some(10),
            uptime_secs: Some(20),
            workers: Some(1),
            cron_pending: Some(HashMap::new()),
        },
        "http://127.0.0.1:8488",
        crate::client::DaemonUrlSource::Cli,
    );
    app.set_workers(vec![WorkerInfo {
        project_id: "p1".to_string(),
        pid: Some(42),
        http_port: Some(9000),
        lsp_port: Some(9001),
        state: Value::String("ready".to_string()),
        last_error: None,
        ..WorkerInfo::default()
    }]);
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({
            "thread": {
                "title": "Status chat",
                "model": "gpt-demo",
                "mode": "agent",
                "auto_approve_editing_tools": true,
                "auto_approve_dangerous_commands": false
            },
            "runtime": {
                "state": "idle",
                "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
            },
            "messages": []
        }),
    });
    app.apply_caps(&json!({"chat_models": {"gpt-demo": {"n_ctx": 1000}}}));
    assert_eq!(
        app.execute_command_name("status"),
        AppAction::LoadDaemonStatus
    );
    let status = app
        .visible_transcript()
        .iter()
        .rev()
        .find_map(|item| match item {
            TranscriptItem::Status(snapshot, theme) => Some((snapshot, theme)),
            _ => None,
        })
        .unwrap();
    let text = crate::ui::status_card::render_lines(100, status.0, status.1)
        .iter()
        .map(line_to_plain_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("refact (v"));
    assert!(text.contains("Daemon:"));
    assert!(text.contains("v1.2.3 on port 8488"));
    assert!(text.contains("Worker:"));
    assert!(text.contains("ready · pid 42 · http 9000 · lsp 9001"));
    assert!(text.contains("Model:"));
    assert!(text.contains("gpt-demo"));
    assert!(text.contains("Mode:"));
    assert!(text.contains("agent"));
    assert!(text.contains("Reasoning:"));
    assert!(text.contains("off"));
    assert!(text.contains("Directory:"));
    assert!(text.contains("/tmp/demo"));
    assert!(text.contains("Permissions:"));
    assert!(text.contains("auto_approve_editing_tools=true"));
    assert!(text.contains("auto_approve_dangerous_commands=false"));
    assert!(text.contains("Token usage:"));
    assert!(text.contains("150 total (100 input + 50 output)"));
    assert!(text.contains("Context window:"));
    assert!(text.contains("85% left (150/1K)"));
    assert_eq!(
        app.permission_policy(),
        session::PermissionPolicy {
            auto_approve_editing_tools: true,
            auto_approve_dangerous_commands: false,
        }
    );
}

#[test]
fn plan_command_shows_current_plan_cell() {
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
            chat_id: Some(chat_id.clone()),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"message": {"role": "plan", "content": "base plan", "extra": {"plan": {"mode": "agent", "version": 1}}}}),
        });
    app.handle_chat_event(ChatEvent {
            chat_id: Some(chat_id),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"message": {"role": "event", "content": "delta one", "extra": {"event": {"subkind": "plan_delta", "payload": {"seq": 1}}}}}),
        });

    assert_eq!(app.execute_command_name("plan"), AppAction::None);
    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Plan(data) if data.content.contains("base plan") && data.content.contains("delta one"))
        }));
}

#[test]
fn goal_command_shows_current_goal_cell() {
    let _surface_lock = goal_dock::test_surface_lock();
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
            chat_id: Some(chat_id.clone()),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"message": {"role": "goal", "content": "base goal", "extra": {"goal": {"version": 1}}}}),
        });
    app.handle_chat_event(ChatEvent {
            chat_id: Some(chat_id),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"message": {"role": "event", "content": "delta one", "extra": {"event": {"subkind": "goal_delta", "payload": {"seq": 1}}}}}),
        });

    assert_eq!(app.execute_command_name("goal"), AppAction::None);
    assert!(app.visible_transcript().iter().any(|item| {
            matches!(item, TranscriptItem::Goal(data) if data.content.contains("base goal") && data.content.contains("delta one"))
        }));
}

#[test]
fn goal_commands_dispatch_all_four_wrappers_and_controls_respect_status() {
    let _surface_lock = goal_dock::test_surface_lock();
    let previous = std::env::var_os("REFACT_TUI_SURFACES");
    std::env::set_var("REFACT_TUI_SURFACES", "1");
    let mut app = App::new(project());
    assert!(matches!(
        app.execute_command_name("goal set Ship it"),
        AppAction::GoalCommand {
            kind: GoalCommandKind::Set,
            content: Some(content),
            budget: None,
        } if content == "Ship it"
    ));
    assert!(matches!(
        app.execute_command_name("goal update Tests added"),
        AppAction::GoalCommand {
            kind: GoalCommandKind::Update,
            content: Some(content),
            budget: None,
        } if content == "Tests added"
    ));
    assert!(matches!(
        app.execute_command_name("goal budget 3 5 1000 42 2"),
        AppAction::GoalCommand {
            kind: GoalCommandKind::SetBudget,
            budget: Some(budget),
            ..
        } if budget.max_turns == Some(3) && budget.max_tokens == Some(1000)
    ));

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
    assert_eq!(
        app.execute_command_name("goal pause"),
        AppAction::GoalControl {
            action: GoalControlAction::Pause
        }
    );
    assert_eq!(
        app.execute_command_name("goal stop"),
        AppAction::GoalControl {
            action: GoalControlAction::Stop
        }
    );
    assert_eq!(app.execute_command_name("goal resume"), AppAction::None);

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "runtime_updated".to_string(),
        raw: json!({"state": "idle", "goal_status": "paused", "goal_active": false}),
    });
    assert_eq!(
        app.execute_command_name("goal resume"),
        AppAction::GoalControl {
            action: GoalControlAction::Resume
        }
    );
    match previous {
        Some(value) => std::env::set_var("REFACT_TUI_SURFACES", value),
        None => std::env::remove_var("REFACT_TUI_SURFACES"),
    }
}
