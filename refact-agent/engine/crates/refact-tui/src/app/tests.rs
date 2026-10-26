use super::*;
use super::tests_util::*;

#[test]
fn alternate_screen_mode_keeps_legacy_transcript_live() {
    let mut app = App::new(project());
    assert!(!app.native_scrollback());
    app.test_push_history_item(TranscriptItem::Notice("legacy".to_string()));
    assert_eq!(app.visible_transcript().len(), 2);
    assert_eq!(app.history_pending_count(), 0);
}

#[test]
fn live_transcript_drops_oldest_items_after_limit() {
    let mut app = App::new(project());
    for idx in 0..10_005 {
        app.test_push_history_item(TranscriptItem::Notice(format!("notice {idx}")));
    }

    assert_eq!(app.visible_transcript().len(), 10_000);
    assert!(matches!(
        app.visible_transcript().first(),
        Some(TranscriptItem::Notice(text)) if text == LIVE_TRANSCRIPT_RETENTION_NOTICE
    ));
    assert!(!app
        .visible_transcript()
        .iter()
        .any(|item| { matches!(item, TranscriptItem::Notice(text) if text == "notice 0") }));
    assert!(app
        .visible_transcript()
        .iter()
        .any(|item| { matches!(item, TranscriptItem::Notice(text) if text == "notice 10004") }));
}

#[test]
fn live_transcript_retention_preserves_running_tool() {
    let mut app = App::new(project());
    app.test_push_tool(ToolCard::from_tool_call(
        &json!({"id": "call-keep", "function": {"name": "shell", "arguments": "{}"}}),
    ));
    for idx in 0..10_005 {
        app.test_push_history_item(TranscriptItem::Notice(format!("notice {idx}")));
    }

    assert_eq!(app.visible_transcript().len(), 10_000);
    assert!(app
        .visible_transcript()
        .iter()
        .any(|item| { matches!(item, TranscriptItem::Tool(card) if card.id == "call-keep") }));
}

#[test]
fn native_scrollback_queues_finalized_history() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.test_push_history_item(TranscriptItem::Notice("native".to_string()));
    assert_eq!(app.visible_transcript().len(), 0);
    assert_eq!(app.history_pending_count(), 2);
}

#[test]
fn native_scrollback_prompt_start_does_not_enqueue_blank_assistant() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    app.composer.set_text("hello");

    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { .. }
    ));

    assert_eq!(app.history_pending_count(), 1);
    let insertions = app.pending_history_insertions(80);
    assert_eq!(
        insertions
            .iter()
            .map(|insertion| insertion.cell_ids.len())
            .sum::<usize>(),
        1
    );
    assert_eq!(app.visible_transcript(), &[]);

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    assert_eq!(app.history_pending_count(), 0);

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_started".to_string(),
        raw: json!({"message_id": "a1"}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"message_id": "a1", "ops": [{"op": "append_content", "text": "hi"}]}),
    });
    assert_eq!(app.history_pending_count(), 0);
    assert_eq!(assistant_text(&app), "hi");

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({"message_id": "a1"}),
    });
    assert_eq!(assistant_text(&app), "");
    assert_eq!(app.history_pending_count(), 1);
    let insertions = app.pending_history_insertions(80);
    assert_eq!(
        insertions
            .iter()
            .map(|insertion| insertion.cell_ids.len())
            .sum::<usize>(),
        1
    );
}

#[test]
fn stream_added_snapshot_reconciles_to_one_assistant_cell() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_started".to_string(),
        raw: json!({"message_id": "a1"}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"message_id": "a1", "ops": [{"op": "append_content", "text": "hello"}]}),
    });
    app.handle_chat_event(assistant_added_event(&app, "a1", "hello"));
    app.handle_chat_event(snapshot_event(
        &app,
        vec![json!({
            "message_id": "a1",
            "role": "assistant",
            "content": "hello",
            "stream_finished": true,
        })],
    ));

    assert_eq!(assistant_cell_count(&app), 1);
    assert_eq!(assistant_text(&app), "hello");
}

#[test]
fn repeated_identical_message_added_does_not_duplicate_assistant() {
    let mut app = App::new(project());
    let event = assistant_added_event(&app, "a1", "hello");

    app.handle_chat_event(event.clone());
    app.handle_chat_event(event);

    assert_eq!(assistant_cell_count(&app), 1);
    assert_eq!(assistant_text(&app), "hello");
}

#[test]
fn new_chat_shows_codex_style_session_header() {
    let mut app = App::new(project());

    app.new_chat();

    let text = session_text(&app);
    assert_eq!(session_cell_count(&app), 1);
    assert!(text.contains(">_ refact"));
    assert!(text.contains("model: default · /model to change"));
    assert!(text.contains("directory: /tmp/demo"));
    assert!(text.contains("Tips: type /help for shortcuts"));
}

#[test]
fn new_chat_session_header_updates_model_without_duplication() {
    let mut app = App::new(project());
    app.new_chat();

    app.apply_caps(&json!({
        "defaults": {
            "chat_default_model": "openai/gpt-demo",
            "chat_default_mode": "agent"
        },
        "chat_models": {
            "openai/gpt-demo": {"name": "GPT Demo"}
        }
    }));
    app.handle_chat_event(snapshot_event(&app, Vec::new()));

    let text = session_text(&app);
    assert_eq!(session_cell_count(&app), 1);
    assert!(text.contains("model: openai/gpt-demo · /model to change"));
    assert!(!text.contains("model: default"));
}

#[test]
fn ask_questions_message_and_waiting_snapshot_keep_one_questions_cell() {
    let mut app = App::new(project());
    let questions = json!([
        {"id": "confirm", "type": "yes_no", "text": "Proceed?"}
    ]);
    let assistant_message = json!({
        "message_id": "a1",
        "role": "assistant",
        "stream_finished": true,
        "tool_calls": [{
            "id": "call-ask",
            "function": {
                "name": "ask_questions",
                "arguments": json!({"questions": questions}).to_string(),
            }
        }]
    });
    let tool_message = tool_result_message(
        "call-ask",
        &json!({
            "type": "ask_questions",
            "tool_call_id": "call-ask",
            "questions": questions,
        })
        .to_string(),
    );

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": assistant_message.clone()}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": tool_message.clone()}),
    });
    app.handle_chat_event(waiting_user_input_event(&app));
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"runtime": {"state": "waiting_user_input"}, "messages": [assistant_message, tool_message]}),
        });

    assert_eq!(questions_cell_count(&app), 1);
    assert!(app.ask_questions_form().is_some());
}

#[test]
fn active_ask_questions_cell_stays_compact() {
    let mut app = App::new(project());
    let questions = json!([
        {"id": "file", "type": "free_text", "text": "Which file should I edit?"}
    ]);
    let assistant_message = json!({
        "message_id": "a1",
        "role": "assistant",
        "stream_finished": true,
        "tool_calls": [{
            "id": "call-ask",
            "function": {
                "name": "ask_questions",
                "arguments": json!({"questions": questions}).to_string(),
            }
        }]
    });
    let tool_message = tool_result_message(
        "call-ask",
        &json!({
            "type": "ask_questions",
            "tool_call_id": "call-ask",
            "questions": questions,
        })
        .to_string(),
    );

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": assistant_message}),
    });
    let tool_card = app
        .visible_transcript()
        .iter()
        .position(|item| matches!(item, TranscriptItem::Tool(card) if card.id == "call-ask"))
        .unwrap();
    app.selected_tool_index = Some(tool_card);
    app.toggle_selected_tool();
    assert!(
        matches!(app.visible_transcript().get(tool_card), Some(TranscriptItem::Tool(card)) if card.expanded)
    );

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": tool_message}),
    });
    app.handle_chat_event(waiting_user_input_event(&app));

    let rendered = app
        .visible_transcript()
        .iter()
        .map(rendered_item_plain_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(app.ask_questions_form().is_some());
    assert!(
        matches!(app.visible_transcript().get(tool_card), Some(TranscriptItem::Tool(card)) if !card.expanded)
    );
    assert_eq!(rendered.matches("Which file should I edit?").count(), 1);
}

#[test]
fn completed_tool_card_transitions_to_history_once() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);

    app.handle_chat_event(tool_call_delta_event(&app, "call-1"));
    app.handle_chat_event(tool_result_event(&app, "call-1", "done"));
    app.handle_chat_event(tool_result_event(&app, "call-1", "done"));
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });

    assert_eq!(tool_cards(&app).len(), 0);
    assert_eq!(app.history_pending_count(), 1);
    let inserted_before = app.history_inserted_cell_count();
    let insertions = app.pending_history_insertions(80);
    assert_eq!(
        insertions
            .iter()
            .map(|insertion| insertion.cell_ids.len())
            .sum::<usize>(),
        1
    );
    assert!(app.pending_history_insertions(80).is_empty());
    assert_eq!(app.history_inserted_cell_count() - inserted_before, 1);
}

#[test]
fn tool_result_uses_structured_exec_metadata() {
    let mut app = App::new(project());
    app.handle_chat_event(tool_call_delta_event(&app, "call-1"));
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "role": "tool",
            "tool_call_id": "call-1",
            "content": "The command was running 0.120s, finished with exit code 1",
            "tool_failed": false,
            "extra": {"exec": {"duration_ms": 8_000, "exit_code": 0}}
        }}),
    });

    let rendered = rendered_item_plain_text(&TranscriptItem::Tool(tool_cards(&app)[0].clone()));
    assert!(rendered.contains("exit 0 · 8.0s"));
    assert!(!rendered.contains("exit 1"));
}

#[test]
fn native_new_chat_session_header_updates_model_without_duplication() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    app.new_chat();

    let inserted_before = app.history_inserted_cell_count();
    let insertions = app.pending_history_insertions(80);
    assert_eq!(
        insertions
            .iter()
            .map(|insertion| insertion.cell_ids.len())
            .sum::<usize>(),
        1
    );
    assert_eq!(app.history_inserted_cell_count() - inserted_before, 1);

    app.apply_caps(&json!({
        "defaults": {"chat_default_model": "openai/gpt-demo"},
        "chat_models": {"openai/gpt-demo": {"name": "GPT Demo"}}
    }));
    assert!(!app.resize_reflow_is_due());
    app.handle_chat_event(snapshot_event(&app, Vec::new()));

    assert_eq!(app.history_pending_count(), 0);
    let reflow = app.resize_reflow_insertions(80, 100);
    let rendered = reflow
        .iter()
        .flat_map(|insertion| insertion.lines.iter())
        .map(|line| line_to_plain_string(&line.line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("model: openai/gpt-demo · /model to change"));
    assert!(!rendered.contains("model: default"));
}

#[test]
fn identical_snapshot_after_insertions_adds_no_history_entries() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    let messages = vec![
        json!({"message_id": "u1", "role": "user", "content": "hello"}),
        json!({"message_id": "a1", "role": "assistant", "content": "hi", "stream_finished": true}),
    ];

    app.handle_chat_event(snapshot_event(&app, messages.clone()));
    assert_eq!(app.history_pending_count(), 2);
    let inserted_before = app.history_inserted_cell_count();
    let insertions = app.pending_history_insertions(80);
    assert_eq!(
        insertions
            .iter()
            .map(|insertion| insertion.cell_ids.len())
            .sum::<usize>(),
        2
    );
    let inserted_after_first = app.history_inserted_cell_count();

    app.handle_chat_event(snapshot_event(&app, messages));

    assert_eq!(app.history_pending_count(), 0);
    assert!(app.pending_history_insertions(80).is_empty());
    assert_eq!(app.history_inserted_cell_count(), inserted_after_first);
    assert_eq!(inserted_after_first - inserted_before, 2);
}

#[test]
fn changed_snapshot_after_insertions_keeps_inserted_cells_immutable() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    let initial = vec![
        json!({"message_id": "u1", "role": "user", "content": "hello"}),
        json!({"message_id": "a1", "role": "assistant", "content": "stale answer", "stream_finished": true}),
    ];

    app.handle_chat_event(snapshot_event(&app, initial));
    assert!(!app.resize_reflow_is_due());
    let first_text = pending_plain_text(&mut app);
    assert!(first_text.contains("hello"));
    assert!(first_text.contains("stale answer"));
    let inserted_before = app.history_inserted_cell_count();

    let changed = vec![
        json!({"message_id": "u1", "role": "user", "content": "hello"}),
        json!({"message_id": "a1", "role": "assistant", "content": "fresh answer", "stream_finished": true}),
        json!({"message_id": "a2", "role": "assistant", "content": "follow-up", "stream_finished": true}),
    ];
    app.handle_chat_event(snapshot_event(&app, changed));

    assert!(!app.resize_reflow_is_due());
    let changed_text = pending_plain_text(&mut app);
    assert!(changed_text.contains("follow-up"), "{changed_text}");
    assert!(!changed_text.contains("fresh answer"), "{changed_text}");
    assert!(!changed_text.contains("stale answer"), "{changed_text}");
    assert!(!changed_text.contains("hello"), "{changed_text}");
    assert_eq!(app.history_inserted_cell_count(), inserted_before + 1);
    assert_eq!(app.history_pending_count(), 0);
}

#[test]
fn native_turn_renders_prompt_and_answer_exactly_once() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    app.composer.set_text("hello there");
    let AppAction::SendMessage { correlation, .. } = app.handle_key(key(KeyCode::Enter)) else {
        panic!("expected send");
    };
    let mut rendered = pending_plain_lines(&mut app);
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"index": 0, "message": {
            "message_id": "user-1",
            "role": "user",
            "content": "hello there",
            "extra": {"client_message_id": correlation.client_message_id},
        }}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "runtime_updated".to_string(),
        raw: json!({"state": "generating"}),
    });
    rendered.extend(pending_plain_lines(&mut app));
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_started".to_string(),
        raw: json!({"message_id": "assistant-1"}),
    });
    let answer = "You said: hello there.\n\nThis is the default reply.\n\nSend another prompt.";
    for chunk in [
        "You said: ",
        "hello there.\n\nThis is ",
        "the default reply.\n\nSend ",
        "another prompt.",
    ] {
        app.handle_chat_event(ChatEvent {
                chat_id: Some(app.chat_id().to_string()),
                seq: None,
                kind: "stream_delta".to_string(),
                raw: json!({"message_id": "assistant-1", "ops": [{"op": "append_content", "text": chunk}]}),
            });
        app.apply_stream_commit_tick();
        rendered.extend(pending_plain_lines(&mut app));
    }
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({"message_id": "assistant-1"}),
    });
    rendered.extend(pending_plain_lines(&mut app));
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"index": 1, "message": {
            "message_id": "assistant-1",
            "role": "assistant",
            "content": answer,
            "stream_finished": true,
        }}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "runtime_updated".to_string(),
        raw: json!({"state": "idle"}),
    });
    rendered.extend(pending_plain_lines(&mut app));

    let joined = rendered.join("\n");
    assert_eq!(joined.matches("hello there.").count(), 1, "{joined}");
    assert_eq!(joined.matches("› hello there").count(), 1, "{joined}");
    assert_eq!(joined.matches("another prompt.").count(), 1, "{joined}");
    let trimmed = rendered.iter().map(|line| line.trim()).collect::<Vec<_>>();
    let answer_start = trimmed
        .iter()
        .position(|line| *line == "• You said: hello there.")
        .expect("answer start");
    assert_eq!(
        &trimmed[answer_start..answer_start + 5],
        [
            "• You said: hello there.",
            "",
            "This is the default reply.",
            "",
            "Send another prompt.",
        ],
        "{rendered:?}"
    );
    assert!(!app.resize_reflow_is_due());
    assert!(
        app.visible_transcript().is_empty(),
        "{:?}",
        app.visible_transcript()
    );
}

#[test]
fn native_tool_turn_moves_finished_card_to_history_once() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    let mut rendered = Vec::new();
    let event = |app: &mut App, kind: &str, raw: Value| {
        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: kind.to_string(),
            raw,
        });
    };
    event(
        &mut app,
        "stream_started",
        json!({"message_id": "assistant-1"}),
    );
    event(
        &mut app,
        "stream_delta",
        json!({"message_id": "assistant-1", "ops": [{"op": "append_content", "text": "Let me look first.\n"}]}),
    );
    app.apply_stream_commit_tick();
    rendered.extend(pending_plain_lines(&mut app));
    let tool_calls = json!([{"id": "call-1", "type": "function", "function": {"name": "shell", "arguments": "{\"command\":\"ls -la\"}"}}]);
    event(
        &mut app,
        "stream_delta",
        json!({"message_id": "assistant-1", "ops": [{"op": "set_tool_calls", "tool_calls": tool_calls}]}),
    );
    event(
        &mut app,
        "stream_finished",
        json!({"message_id": "assistant-1", "finish_reason": "tool_calls"}),
    );
    rendered.extend(pending_plain_lines(&mut app));
    event(
        &mut app,
        "message_added",
        json!({"index": 1, "message": {
            "message_id": "assistant-1", "role": "assistant", "content": "Let me look first.\n",
            "tool_calls": tool_calls, "stream_finished": true,
        }}),
    );
    rendered.extend(pending_plain_lines(&mut app));
    assert!(
        app.visible_transcript()
            .iter()
            .any(|item| matches!(item, TranscriptItem::Tool(card) if card.status.is_active())),
        "{:?}",
        app.visible_transcript()
    );
    event(
        &mut app,
        "message_added",
        json!({"index": 2, "message": {
            "message_id": "tool-1", "role": "tool", "tool_call_id": "call-1",
            "content": "total 8\ndrwxr-xr-x  src\n",
        }}),
    );
    rendered.extend(pending_plain_lines(&mut app));
    event(
        &mut app,
        "stream_started",
        json!({"message_id": "assistant-2"}),
    );
    event(
        &mut app,
        "stream_delta",
        json!({"message_id": "assistant-2", "ops": [{"op": "append_content", "text": "The listing shows a small project.\n"}]}),
    );
    event(
        &mut app,
        "stream_finished",
        json!({"message_id": "assistant-2"}),
    );
    event(
        &mut app,
        "message_added",
        json!({"index": 3, "message": {
            "message_id": "assistant-2", "role": "assistant", "content": "The listing shows a small project.\n", "stream_finished": true,
        }}),
    );
    event(&mut app, "runtime_updated", json!({"state": "idle"}));
    rendered.extend(pending_plain_lines(&mut app));

    let joined = rendered.join("\n");
    assert!(
        app.visible_transcript().is_empty(),
        "{:?}\n{joined}",
        app.visible_transcript()
    );
    assert_eq!(joined.matches("Let me look first.").count(), 1, "{joined}");
    assert_eq!(joined.matches("ls -la").count(), 1, "{joined}");
    assert_eq!(joined.matches("The listing shows").count(), 1, "{joined}");
    assert!(joined.contains("succeeded"), "{joined}");
}

#[test]
fn aborted_stream_persisted_after_queued_dispatch_is_not_rendered_twice() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    let event = |app: &mut App, kind: &str, raw: Value| {
        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: kind.to_string(),
            raw,
        });
    };
    app.composer.set_text("slow please");
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        AppAction::SendMessage { .. }
    ));
    let mut rendered = pending_plain_lines(&mut app);
    event(&mut app, "stream_started", json!({"message_id": "a1"}));
    event(
        &mut app,
        "stream_delta",
        json!({"message_id": "a1", "ops": [{"op": "append_content", "text": "Line one.\nLine two.\nLine three.\n"}]}),
    );
    for _ in 0..4 {
        app.apply_stream_commit_tick();
        rendered.extend(pending_plain_lines(&mut app));
    }
    assert!(
        rendered.iter().any(|line| line.contains("Line one.")),
        "{rendered:?}"
    );

    app.composer.set_text("queued follow-up");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert_eq!(app.input_queue().len(), 1);
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::Abort);
    let dispatched = app.handle_command_finished(
        CommandContextTag::Abort {
            origin: app.command_origin(),
        },
        Ok(()),
    );
    assert!(
        matches!(dispatched, AppAction::SendMessage { .. }),
        "{dispatched:?}"
    );
    rendered.extend(pending_plain_lines(&mut app));
    event(
        &mut app,
        "stream_finished",
        json!({"message_id": "a1", "finish_reason": "abort"}),
    );
    event(
        &mut app,
        "message_added",
        json!({"index": 2, "message": {
            "message_id": "a1", "role": "assistant", "content": "Line one.\nLine two.\nLine three.\n", "stream_finished": true,
        }}),
    );
    rendered.extend(pending_plain_lines(&mut app));

    let joined = rendered.join("\n");
    assert_eq!(joined.matches("Line one.").count(), 1, "{joined}");
    assert_eq!(joined.matches("Line three.").count(), 1, "{joined}");
    assert_eq!(joined.matches("› queued follow-up").count(), 1, "{joined}");
}

#[test]
fn native_reasoning_is_archived_before_the_answer_it_precedes() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    let event = |app: &mut App, kind: &str, raw: Value| {
        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: kind.to_string(),
            raw,
        });
    };
    event(&mut app, "stream_started", json!({"message_id": "a1"}));
    event(
        &mut app,
        "stream_delta",
        json!({"message_id": "a1", "ops": [{"op": "append_reasoning", "text": "weigh the options\n"}]}),
    );
    event(
        &mut app,
        "stream_delta",
        json!({"message_id": "a1", "ops": [{"op": "append_content", "text": "Final answer.\n"}]}),
    );
    let mut rendered = Vec::new();
    for _ in 0..3 {
        app.apply_stream_commit_tick();
        rendered.extend(pending_plain_lines(&mut app));
    }
    event(&mut app, "stream_finished", json!({"message_id": "a1"}));
    event(
        &mut app,
        "message_added",
        json!({"index": 1, "message": {
            "message_id": "a1", "role": "assistant", "content": "Final answer.\n",
            "reasoning": "weigh the options\n", "stream_finished": true,
        }}),
    );
    rendered.extend(pending_plain_lines(&mut app));

    let joined = rendered.join("\n");
    let thinking = joined.find("Thinking").expect(&joined);
    let answer = joined.find("Final answer.").expect(&joined);
    assert!(thinking < answer, "{joined}");
    assert_eq!(joined.matches("Thinking").count(), 1, "{joined}");
    assert_eq!(joined.matches("Final answer.").count(), 1, "{joined}");
    assert!(
        app.visible_transcript().is_empty(),
        "{:?}",
        app.visible_transcript()
    );
}

#[test]
fn reasoning_stream_finished_snapshot_reconciles_to_one_reasoning_cell() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_started".to_string(),
        raw: json!({"message_id": "a1"}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"message_id": "a1", "ops": [{"op": "append_reasoning", "text": "think"}]}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({"message_id": "a1"}),
    });
    app.handle_chat_event(snapshot_event(
        &app,
        vec![json!({
            "message_id": "a1",
            "role": "assistant",
            "reasoning": "think",
            "stream_finished": true,
        })],
    ));

    assert_eq!(reasoning_cell_count(&app), 1);
    assert_eq!(reasoning_text(&app), "think");
}

#[test]
fn in_progress_plan_snapshot_replay_preserves_history_and_live_stream_once() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    let messages = vec![json!({
        "message_id": "p1",
        "role": "plan",
        "content": "- one\n- two\n- three\n- four\n- five\n",
        "stream_finished": false,
        "extra": {"plan": {"mode": "agent", "version": 1}}
    })];

    app.handle_chat_event(snapshot_event(&app, messages.clone()));
    let mut committed = String::new();
    for _ in 0..12 {
        app.apply_stream_commit_tick();
        committed.push_str(&pending_plain_text(&mut app));
    }
    assert!(committed.contains("- five"), "{committed}");
    assert_eq!(committed.matches("- one").count(), 1, "{committed}");
    let visible_before = app.visible_transcript().len();
    let stream_text_before = plan_stream_text(&app);

    app.handle_chat_event(snapshot_event(&app, messages));

    assert_eq!(app.history_pending_count(), 0);
    assert_eq!(app.visible_transcript().len(), visible_before);
    assert_eq!(plan_stream_text(&app), stream_text_before);
    assert!(
        app.visible_transcript()
            .iter()
            .filter(|item| matches!(item, TranscriptItem::PlanStream(_)))
            .count()
            <= 1
    );
}

#[test]
fn ordinary_event_snapshot_replay_does_not_duplicate_events() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    let messages = vec![json!({
        "message_id": "e1",
        "role": "event",
        "content": "Process exited with code 0",
        "extra": {"event": {"subkind": "process_completed", "source": "exec.registry", "payload": {"exit_code": 0}}}
    })];

    app.handle_chat_event(snapshot_event(&app, messages.clone()));
    let events_before = app.events_pane().events().len();
    let pending_before = app.history_pending_count();

    app.handle_chat_event(snapshot_event(&app, messages));

    assert_eq!(app.events_pane().events().len(), events_before);
    assert_eq!(app.history_pending_count(), pending_before);
}

#[test]
fn streaming_resize_rewraps_live_tail() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_started".to_string(),
        raw: json!({"message_id": "a1"}),
    });
    app.update_stream_width_for_terminal(80);
    let source = "intro\nalpha beta gamma delta epsilon zeta eta theta";
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"message_id": "a1", "ops": [{"op": "append_content", "text": source}]}),
    });
    let wide_tail = stream_tail_plain_lines(&app);

    app.update_stream_width_for_terminal(16);
    let narrow_tail = stream_tail_plain_lines(&app);

    assert_eq!(
        wide_tail,
        vec!["alpha beta gamma delta epsilon zeta eta theta"]
    );
    assert!(narrow_tail.len() > wide_tail.len());
    assert_eq!(assistant_text(&app), source);
    app.apply_stream_commit_tick();
    assert_eq!(app.active_stream_committed(), "intro\n");
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({"message_id": "a1"}),
    });
    assert_eq!(assistant_text(&app), format!("{source}\n"));
}

#[test]
fn native_scrollback_completed_tool_moves_to_history_once() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "shell", "arguments": "{\"cmd\":\"echo 1\"}"}}]}]}),
        });
    assert_eq!(tool_cards(&app).len(), 1);

    app.complete_tool("call-1", "tool", "done".to_string(), ToolStatus::Succeeded);

    assert_eq!(tool_cards(&app).len(), 1);
    assert_eq!(tool_cards(&app)[0].status, ToolStatus::Succeeded);
    assert_eq!(app.selected_tool_index(), Some(0));
    assert_eq!(app.history_pending_count(), 0);

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });

    assert_eq!(tool_cards(&app).len(), 0);
    assert_eq!(app.selected_tool_index(), None);
    assert_eq!(app.history_pending_count(), 1);
    let inserted_before = app.history_inserted_cell_count();
    let insertions = app.pending_history_insertions(80);
    assert_eq!(
        insertions
            .iter()
            .map(|insertion| insertion.cell_ids.len())
            .sum::<usize>(),
        1
    );
    let rendered_after_insert = app.history_render_count();
    app.begin_frame_render();
    assert_eq!(app.visible_transcript().len(), 0);
    assert!(app.pending_history_insertions(80).is_empty());
    assert_eq!(app.history_inserted_cell_count() - inserted_before, 1);
    assert_eq!(app.history_render_count(), rendered_after_insert);
}

#[test]
fn native_snapshot_keeps_rendered_content_and_skips_identical_snapshot() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "snapshot".to_string(),
        raw: json!({"runtime": {"state": "idle"}, "messages": [
            {"message_id": "a1", "role": "assistant", "content": "stale", "stream_finished": true}
        ]}),
    });
    let stale = app.pending_history_insertions(80);
    assert_eq!(
        stale
            .iter()
            .map(|insertion| insertion.cell_ids.len())
            .sum::<usize>(),
        1
    );

    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"runtime": {"state": "idle"}, "messages": [
                {"message_id": "a1", "role": "assistant", "content": "corrected", "stream_finished": true}
            ]}),
        });
    assert!(app.pending_history_insertions(80).is_empty());
    assert!(!app.resize_reflow_is_due());

    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"runtime": {"state": "idle"}, "messages": [
                {"message_id": "a1", "role": "assistant", "content": "corrected", "stream_finished": true}
            ]}),
        });
    assert!(app.pending_history_insertions(80).is_empty());
    assert_eq!(app.history_inserted_cell_count(), 2);
}

#[test]
fn app_config_enables_vim_theme_and_custom_binding() {
    let mut app = App::new(project());
    app.test_apply_tui_config_content(
        r#"
vim_mode = true

[bindings]
send = "ctrl-s"

[theme]
name = "plain"
"#,
    );
    assert!(app.vim_enabled());
    assert_eq!(app.vim_mode(), VimMode::Normal);
    assert_eq!(app.theme().name(), "plain");
    app.composer.set_text("hello");
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(matches!(
        action,
        AppAction::SendMessage { prompt, .. } if prompt == "hello"
    ));
}

#[test]
fn app_config_applies_notification_settings() {
    let mut app = App::new(project());
    app.test_apply_tui_config_content(
        r#"
[notifications]
enabled = true
bell = false
"#,
    );

    assert_eq!(app.notification_status_label(), "OSC9");
}

#[test]
fn app_config_reports_invalid_multibyte_theme_color() {
    let mut app = App::new(project());
    app.test_apply_tui_config_content(
        r##"
[theme]
accent = "#ééé"
"##,
    );

    assert_eq!(app.theme().name(), "dark");
    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Notice(text) if text.contains("invalid theme accent color"))
    }));
}

#[test]
fn app_notification_gate_disables_pending_notifications() {
    let mut app = App::new(project());
    app.test_set_notifications_config(NotificationConfig::new(false, true, Duration::ZERO));

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });

    assert!(app.test_take_pending_notifications().is_empty());
    assert_eq!(app.notification_status_label(), "off");
}

#[test]
fn stream_finished_and_approval_queue_notifications_when_unfocused() {
    let mut app = App::new(project());
    app.test_set_terminal_focus(false);
    app.test_set_notifications_config(NotificationConfig::new(true, true, Duration::ZERO));

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));

    let pending = app.test_take_pending_notifications();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0], b"\x1b]9;Refact: response ready\x07\x07");
    assert_eq!(pending[1], b"\x1b]9;Refact: approval needed\x07\x07");
}

#[test]
fn app_notifications_debounce_duplicate_turn_complete() {
    let mut app = App::new(project());
    app.test_set_terminal_focus(false);
    app.test_set_notifications_config(NotificationConfig::new(true, true, Duration::from_secs(60)));

    for _ in 0..2 {
        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_finished".to_string(),
            raw: json!({}),
        });
    }

    assert_eq!(app.test_take_pending_notifications().len(), 1);
}

#[test]
fn notifications_suppress_when_focused() {
    let mut app = App::new(project());
    app.test_set_terminal_focus(true);
    app.test_set_notifications_config(NotificationConfig::new(true, true, Duration::ZERO));

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));

    assert!(app.test_take_pending_notifications().is_empty());
}

#[test]
fn app_dispatches_through_active_registry_binding() {
    let mut app = App::new(project());
    let keymap = KeymapRegistry::from_toml_str(
        r#"
[bindings]
new-chat = "ctrl-x"
"#,
    )
    .unwrap();
    app.test_set_keymap(keymap);
    let old_chat_id = app.chat_id().to_string();
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
        AppAction::SubscribeCurrent
    );
    assert_ne!(app.chat_id(), old_chat_id);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        AppAction::None
    );
}

#[test]
fn recent_session_next_key_switches_to_next_chat() {
    let mut app = App::new(project());
    let current = app.chat_id().to_string();
    app.test_set_recent_sessions(vec![
        PickerItem {
            id: current.clone(),
            title: "Current".to_string(),
            description: "now".to_string(),
        },
        PickerItem {
            id: "chat-next".to_string(),
            title: "Next chat".to_string(),
            description: "recent".to_string(),
        },
    ]);

    let action = app.handle_key(key(KeyCode::F(7)));

    assert_eq!(action, AppAction::SubscribeCurrent);
    assert_eq!(app.chat_id(), "chat-next");
    assert_eq!(app.session_title(), Some("Next chat"));
    assert!(app
        .session_tabs()
        .iter()
        .any(|tab| { tab.id == "chat-next" && tab.is_current }));
}

#[test]
fn recent_session_previous_key_wraps_and_switches() {
    let mut app = App::new(project());
    let current = app.chat_id().to_string();
    app.test_set_recent_sessions(vec![
        PickerItem {
            id: current,
            title: "Current".to_string(),
            description: "now".to_string(),
        },
        PickerItem {
            id: "chat-last".to_string(),
            title: "Last chat".to_string(),
            description: "recent".to_string(),
        },
    ]);

    let action = app.handle_key(key(KeyCode::F(6)));

    assert_eq!(action, AppAction::SubscribeCurrent);
    assert_eq!(app.chat_id(), "chat-last");
}

#[test]
fn recent_session_key_with_empty_cache_requests_refresh() {
    let mut app = App::new(project());

    assert_eq!(
        app.handle_key(key(KeyCode::F(7))),
        AppAction::RefreshRecentSessions
    );
    assert_eq!(app.session_tabs().len(), 1);
}

#[test]
fn app_vim_mode_basic_motions_and_delete_line() {
    let mut app = App::new(project());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(!app.vim_enabled());
    app.test_execute_command_name("vim");
    assert!(app.vim_enabled());
    assert_eq!(app.vim_mode(), VimMode::Normal);
    app.composer.set_text("alpha beta");
    app.handle_key(key(KeyCode::Char('b')));
    assert_eq!(app.composer.cursor_char_offset(), 6);
    app.handle_key(key(KeyCode::Char('b')));
    assert_eq!(app.composer.cursor_char_offset(), 0);
    app.composer.set_text("alpha beta\ngamma");
    app.handle_key(key(KeyCode::Char('b')));
    app.handle_key(key(KeyCode::Char('d')));
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.composer(), "alpha beta\n");
    app.handle_key(key(KeyCode::Char('i')));
    assert_eq!(app.vim_mode(), VimMode::Insert);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.vim_mode(), VimMode::Normal);
}

#[test]
fn app_submits_message_and_streams_answer() {
    let mut app = App::new(project());
    app.composer.set_text("hello");
    let action = app.handle_key(key(KeyCode::Enter));
    assert!(matches!(
        action,
        AppAction::SendMessage { prompt, .. } if prompt == "hello"
    ));
    assert_eq!(app.session_state(), SessionState::Generating);
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "append_content", "text": "hi"}]}),
    });
    assert_eq!(assistant_text(&app), "hi");
    assert!(!app.stream_has_committable_lines());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    assert_eq!(assistant_text(&app), "hi\n");
    assert_eq!(app.session_state(), SessionState::Idle);
}

#[test]
fn runtime_states_map_to_distinct_footer_states() {
    let mut app = App::new(project());
    let states = [
        ("idle", SessionState::Idle, "idle"),
        ("generating", SessionState::Generating, "generating"),
        (
            "executing_tools",
            SessionState::ExecutingTools,
            "running tools",
        ),
        ("paused", SessionState::Paused, "approval pending"),
        ("waiting_ide", SessionState::WaitingIde, "waiting for IDE…"),
        (
            "waiting_user_input",
            SessionState::WaitingUserInput,
            "waiting for input",
        ),
        ("completed", SessionState::Completed, "completed"),
        ("error", SessionState::Error, "error"),
    ];

    for (runtime_state, expected_state, expected_footer_label) in states {
        app.handle_chat_event(runtime_updated_event(&app, runtime_state));

        assert_eq!(app.session_state(), expected_state);
        let footer = crate::ui::footer::FooterData::from_app(&app);
        assert!(crate::ui::footer::footer_text(&footer).contains(expected_footer_label));
    }
}

#[test]
fn waiting_ide_footer_is_blocked_and_abort_restores_idle() {
    let mut app = App::new(project());
    app.handle_chat_event(runtime_updated_event(&app, "waiting_ide"));

    let footer = crate::ui::footer::footer_text(&crate::ui::footer::FooterData::from_app(&app));
    assert!(footer.contains("waiting for IDE…"));
    assert!(footer.contains("Esc to abort"));
    assert!(!footer.contains("● idle"));
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::Abort);
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
    assert!(!app.abort_in_flight);
}

#[test]
fn stream_finished_preserves_completed_runtime_state() {
    let mut app = App::new(project());
    app.handle_chat_event(runtime_updated_event(&app, "completed"));
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });

    assert_eq!(app.session_state(), SessionState::Completed);
}

#[test]
fn app_newline_keys_insert_and_enter_submits() {
    let mut app = App::new(project());
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::empty()));
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()));
    assert_eq!(app.composer(), "a\nb\nc");
    let action = app.handle_key(key(KeyCode::Enter));
    assert!(matches!(
        action,
        AppAction::SendMessage { prompt, .. } if prompt == "a\nb\nc"
    ));
}

#[test]
fn app_space_key_inserts_space_in_composer() {
    let mut app = App::new(project());

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()));
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::empty()));

    assert_eq!(app.composer(), "a b");
}

#[test]
fn composer_submit_preserves_multiline_whitespace_and_history() {
    let mut app = App::new(project());
    let prompt = "  indented\nkeep trailing line\n";
    app.composer.set_text(prompt);

    let action = app.handle_key(key(KeyCode::Enter));

    assert!(matches!(
        action,
        AppAction::SendMessage { prompt: sent, .. } if sent == prompt
    ));
    assert_eq!(
        app.composer_state().history_entries(),
        &[prompt.to_string()]
    );
}

#[test]
fn ask_questions_tool_result_opens_form_and_submits_canonical_reply() {
    let mut app = App::new(project());
    app.handle_chat_event(ask_questions_tool_event(
        &app,
        "call-ask",
        json!([
            {"id": "confirm", "type": "yes_no", "text": "Proceed?"}
        ]),
    ));
    assert!(app.ask_questions_form().is_none());
    app.handle_chat_event(waiting_user_input_event(&app));

    assert!(app.ask_questions_form().is_some());
    assert_eq!(app.session_state(), SessionState::WaitingUserInput);
    app.handle_key(key(KeyCode::Char('n')));
    let action = app.handle_key(key(KeyCode::Enter));

    assert!(app.ask_questions_form().is_none());
    assert!(matches!(
        action,
        AppAction::SendMessage { prompt, .. }
            if prompt == "[QA:call-ask]\n> [confirm] Proceed?\nNo"
    ));
    app.handle_chat_event(waiting_user_input_event(&app));
    assert!(app.ask_questions_form().is_none());
}

#[test]
fn ask_questions_does_not_submit_unseen_yes_no_defaults() {
    let mut app = App::new(project());
    app.handle_chat_event(ask_questions_tool_event(
        &app,
        "call-ask",
        json!([
            {"id":"first","type":"yes_no","text":"First?"},
            {"id":"second","type":"yes_no","text":"Second?"},
            {"id":"third","type":"yes_no","text":"Third?"}
        ]),
    ));
    app.handle_chat_event(waiting_user_input_event(&app));

    assert_eq!(app.handle_key(key(KeyCode::Right)), AppAction::None);
    assert_eq!(app.handle_key(key(KeyCode::Right)), AppAction::None);
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);

    let form = app
        .ask_questions_form()
        .expect("incomplete form remains open");
    assert_eq!(form.current_index(), 0);
    assert_eq!(
        form.submission_error().as_deref(),
        Some("Answer outstanding questions: 1, 2, 3")
    );
    assert!(!form.format_answers().contains("\nYes"));
}

#[test]
fn ask_questions_cancel_stays_canceled_and_manual_reply_is_canonical() {
    let mut app = App::new(project());
    let tool_message = json!({
        "role": "tool",
        "tool_call_id": "call-ask",
        "tool_failed": false,
        "content": json!({
            "type": "ask_questions",
            "tool_call_id": "call-ask",
            "questions": [
                {"id": "notes", "type": "free_text", "text": "Notes?"}
            ]
        }).to_string()
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": tool_message.clone()}),
    });
    app.handle_chat_event(waiting_user_input_event(&app));

    assert!(app.ask_questions_form().is_some());
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::None);
    assert!(app.ask_questions_form().is_none());
    assert_eq!(app.session_state(), SessionState::WaitingUserInput);

    app.handle_chat_event(waiting_user_input_event(&app));
    assert!(app.ask_questions_form().is_none());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"thread": {}, "runtime": {"state": "waiting_user_input"}, "messages": [tool_message]}),
        });
    assert!(app.ask_questions_form().is_none());

    app.composer.set_text("manual answer");
    let action = app.handle_key(key(KeyCode::Enter));

    assert!(matches!(
        action,
        AppAction::SendMessage { prompt, .. }
            if prompt == "[QA:call-ask]\n> [notes] Notes?\n(no answer)\n\n> [__additional__] Additional comments\nmanual answer"
    ));
}

#[test]
fn queued_input_waits_for_idle_runtime_after_stream_finished() {
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

    let action = app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });

    assert_eq!(action, AppAction::None);
    assert_eq!(app.input_queue().len(), 2);

    let action = app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "runtime_updated".to_string(),
        raw: json!({"state": "executing_tools"}),
    });

    assert_eq!(action, AppAction::None);
    assert_eq!(app.input_queue().len(), 2);
    assert_eq!(app.session_state(), SessionState::ExecutingTools);

    let action = app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "runtime_updated".to_string(),
        raw: json!({"state": "idle"}),
    });

    assert!(matches!(
        action,
        AppAction::SendMessage { prompt, .. } if prompt == "second"
    ));
    assert_eq!(app.input_queue().len(), 1);
    assert_eq!(app.input_queue().items()[0].text, "third");
    assert_eq!(app.session_state(), SessionState::Generating);
}

#[test]
fn project_switch_handoff_prevents_cross_project_queue_dispatch() {
    let project_a = project();
    let mut app = App::new(project_a.clone());
    let chat_a = app.chat_id().to_string();
    app.input_queue
        .enqueue("first for A".to_string(), Value::Null);
    app.input_queue
        .enqueue("second for A".to_string(), Value::Null);
    app.composer.set_text("draft for A");

    app.set_project(project_b());

    assert!(app.input_queue().is_empty());
    assert!(app.composer().is_empty());
    assert_eq!(
        app.handle_chat_event(runtime_updated_event(&app, "idle")),
        AppAction::None
    );
    app.abort_in_flight = true;
    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::Abort {
                origin: app.command_origin(),
            },
            Ok(()),
        ),
        AppAction::None
    );

    app.set_project(project_a);

    assert_eq!(app.chat_id(), chat_a);
    assert_eq!(app.composer(), "draft for A");
    assert_eq!(app.input_queue().len(), 2);
    assert!(matches!(
        app.handle_chat_event(runtime_updated_event(&app, "idle")),
        AppAction::SendMessage { prompt, .. } if prompt == "first for A"
    ));
    assert_eq!(app.input_queue().items()[0].text, "second for A");
}

#[test]
fn chat_switch_handoff_restores_each_chat_draft_and_queue() {
    let mut app = App::new(project());
    let chat_a = app.chat_id().to_string();
    app.input_queue
        .enqueue("queued for A".to_string(), Value::Null);
    app.composer.set_text("draft for A");

    app.resume_chat("chat-b".to_string(), "Chat B".to_string(), None);

    assert!(app.input_queue().is_empty());
    assert!(app.composer().is_empty());
    app.input_queue
        .enqueue("queued for B".to_string(), Value::Null);
    app.composer.set_text("draft for B");

    app.resume_chat(chat_a, "Chat A".to_string(), None);

    assert_eq!(app.composer(), "draft for A");
    assert_eq!(app.input_queue().items()[0].text, "queued for A");

    app.resume_chat("chat-b".to_string(), "Chat B".to_string(), None);

    assert_eq!(app.composer(), "draft for B");
    assert_eq!(app.input_queue().items()[0].text, "queued for B");
}
