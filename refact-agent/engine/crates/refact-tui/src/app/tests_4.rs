use super::*;
use super::tests_util::*;

#[test]
fn project_picker_filters_and_selects() {
    let mut picker = surfaces::ProjectPickerState::new(vec![
        ProjectEntry {
            id: "a".to_string(),
            slug: "alpha".to_string(),
            root: PathBuf::from("/tmp/alpha"),
            pinned: Some(false),
            last_active_ms: Some(0),
            settings: Value::Null,
        },
        ProjectEntry {
            id: "b".to_string(),
            slug: "beta".to_string(),
            root: PathBuf::from("/tmp/beta"),
            pinned: Some(false),
            last_active_ms: Some(0),
            settings: Value::Null,
        },
    ]);
    picker.filter = "bet".to_string();
    assert_eq!(picker.filtered_projects()[0].slug, "beta");
}

#[test]
fn synthetic_pause_approval_yields_decision_and_clears_modal() {
    let mut app = App::new(project());
    let chat_id = app.chat_id().to_string();
    app.handle_chat_event(ChatEvent {
            chat_id: Some(chat_id),
            seq: None,
            kind: "pause_required".to_string(),
            raw: json!({"reasons": [{"type": "confirmation", "tool_name": "shell", "command": "echo hi", "rule": "*", "tool_call_id": "call-1"}]}),
        });
    assert!(app.approval_modal().is_some());
    let action = app.handle_key(key(KeyCode::Char('y')));
    assert!(app.approval_modal().is_none());
    assert_tool_decision_action(
        &action,
        vec![ToolDecision {
            tool_call_id: "call-1".to_string(),
            accepted: true,
        }],
        None,
    );
}

#[test]
fn denied_approval_marks_matching_tool_denied() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{
            "id": "call-denied",
            "function": {"name": "shell", "arguments": "{}"}
        }]}]}),
    });
    app.handle_chat_event(pause_event(&app, "call-denied", "shell"));
    assert_eq!(tool_cards(&app)[0].status, ToolStatus::AwaitingApproval);

    assert!(matches!(
        app.handle_key(key(KeyCode::Char('n'))),
        AppAction::SendToolDecisions { .. }
    ));

    let card = tool_cards(&app)[0];
    assert_eq!(card.status, ToolStatus::Denied);
    assert!(rendered_item_plain_text(&TranscriptItem::Tool(card.clone())).contains("⊘ denied"));
}

#[test]
fn cleared_approval_marks_matching_tool_cancelled() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{
            "id": "call-cancelled",
            "function": {"name": "shell", "arguments": "{}"}
        }]}]}),
    });
    app.handle_chat_event(pause_event(&app, "call-cancelled", "shell"));

    app.clear_approvals();

    assert_eq!(tool_cards(&app)[0].status, ToolStatus::Cancelled);
}

#[test]
fn tool_decision_failure_restores_approval_queue() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));
    let action = app.handle_key(key(KeyCode::Char('y')));
    let AppAction::SendToolDecisions { rollback, .. } = action else {
        panic!("expected tool decision action");
    };
    assert!(app.approval_modal().is_none());

    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: "decision-1".to_string(),
            rollback,
        },
        Err("decision rejected".to_string()),
    );

    assert!(matches!(
        app.approval_modal(),
        Some(modal) if modal.tool_call_ids() == ["call-1"]
    ));
    assert_eq!(app.approval_pending_clear_count(), 0);
}

#[test]
fn stale_tool_decision_failure_is_inert_after_chat_switch() {
    let mut app = App::new(project());
    let origin = app.command_origin();
    app.handle_chat_event(pause_event(&app, "call-old", "shell"));
    let AppAction::SendToolDecisions {
        client_request_id,
        rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('y')))
    else {
        panic!("expected tool decision action");
    };

    app.resume_chat("chat-next".to_string(), "Next chat".to_string(), None);
    app.handle_chat_event(tool_call_delta_event(&app, "call-current"));

    let status = tool_cards(&app)[0].status;
    let notice_count = app.visible_transcript().len();

    assert_eq!(
        app.handle_command_finished(
            CommandContextTag::ToolDecisions {
                origin,
                client_request_id,
                rollback,
            },
            Err("decision rejected".to_string()),
        ),
        AppAction::None
    );

    assert!(app.approval_modal().is_none());
    assert_eq!(tool_cards(&app)[0].status, status);
    assert_eq!(app.visible_transcript().len(), notice_count);
}

#[test]
fn current_origin_tool_decision_failure_restores_approval_queue() {
    let mut app = App::new(project());
    let origin = app.command_origin();
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));
    let AppAction::SendToolDecisions {
        client_request_id,
        rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('y')))
    else {
        panic!("expected tool decision action");
    };

    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin,
            client_request_id,
            rollback,
        },
        Err("decision rejected".to_string()),
    );

    assert!(matches!(
        app.approval_modal(),
        Some(modal) if modal.tool_call_ids() == ["call-1"]
    ));
}

#[test]
fn native_scrollback_tool_decision_failure_restores_pending_history() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.handle_chat_event(tool_call_delta_event(&app, "call-1"));
    app.complete_tool("call-1", "tool", "done".to_string(), ToolStatus::Succeeded);
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));

    let AppAction::SendToolDecisions { rollback, .. } = app.handle_key(key(KeyCode::Char('y')))
    else {
        panic!("expected tool decision action");
    };
    assert_eq!(
        app.history_tool_status_for_test("call-1"),
        Some(ToolStatus::ApprovedOnce)
    );

    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: "decision-1".to_string(),
            rollback,
        },
        Err("decision rejected".to_string()),
    );

    assert_eq!(
        app.history_tool_status_for_test("call-1"),
        Some(ToolStatus::AwaitingApproval)
    );
    assert!(matches!(
        app.approval_modal(),
        Some(modal) if modal.tool_call_ids() == ["call-1"]
    ));
    assert!(!app.resize_reflow.has_pending_reflow());
}

#[test]
fn native_scrollback_tool_decision_failure_reflows_drained_history() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.handle_chat_event(tool_call_delta_event(&app, "call-1"));
    app.complete_tool("call-1", "tool", "done".to_string(), ToolStatus::Succeeded);
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));

    let AppAction::SendToolDecisions { rollback, .. } = app.handle_key(key(KeyCode::Char('y')))
    else {
        panic!("expected tool decision action");
    };
    app.pending_history_insertions(80);
    assert_eq!(app.history_pending_count(), 0);
    app.resize_reflow.clear_pending_reflow();

    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: "decision-1".to_string(),
            rollback,
        },
        Err("decision rejected".to_string()),
    );

    assert_eq!(
        app.history_tool_status_for_test("call-1"),
        Some(ToolStatus::AwaitingApproval)
    );
    assert!(app.resize_reflow.has_pending_reflow());
}

#[test]
fn native_scrollback_out_of_order_failure_restores_only_its_scope() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);
    app.pending_history_insertions(80);
    for tool_call_id in ["call-a", "call-b"] {
        app.handle_chat_event(tool_call_delta_event(&app, tool_call_id));
        app.complete_tool(
            tool_call_id,
            "tool",
            "done".to_string(),
            ToolStatus::Succeeded,
        );
    }
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    app.handle_chat_event(pause_event(&app, "call-a", "shell"));
    app.handle_chat_event(pause_event(&app, "call-b", "cat"));

    let AppAction::SendToolDecisions {
        client_request_id: first_id,
        rollback: first_rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('y')))
    else {
        panic!("expected first tool decision action");
    };
    let AppAction::SendToolDecisions {
        client_request_id: second_id,
        rollback: second_rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('n')))
    else {
        panic!("expected second tool decision action");
    };

    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: second_id,
            rollback: second_rollback,
        },
        Ok(()),
    );
    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: first_id,
            rollback: first_rollback,
        },
        Err("first decision rejected".to_string()),
    );

    assert_eq!(
        app.history_tool_status_for_test("call-a"),
        Some(ToolStatus::AwaitingApproval)
    );
    assert_eq!(
        app.history_tool_status_for_test("call-b"),
        Some(ToolStatus::Denied)
    );
    assert!(matches!(
        app.approval_modal(),
        Some(modal) if modal.tool_call_ids() == ["call-a"]
    ));
}

#[test]
fn tool_decision_failure_restores_only_its_own_approval_after_newer_success() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-a", "shell"));
    app.handle_chat_event(pause_event(&app, "call-b", "cat"));

    let AppAction::SendToolDecisions {
        client_request_id: first_id,
        rollback: first_rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('y')))
    else {
        panic!("expected first tool decision action");
    };
    let AppAction::SendToolDecisions {
        client_request_id: second_id,
        rollback: second_rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('n')))
    else {
        panic!("expected second tool decision action");
    };

    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: second_id,
            rollback: second_rollback,
        },
        Ok(()),
    );
    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: first_id,
            rollback: first_rollback,
        },
        Err("first decision rejected".to_string()),
    );

    assert!(matches!(
        app.approval_modal(),
        Some(modal) if modal.tool_call_ids() == ["call-a"]
    ));
    assert_eq!(app.approval_pending_clear_count(), 1);
}

#[test]
fn newer_tool_decision_failure_preserves_older_success() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-a", "shell"));
    app.handle_chat_event(pause_event(&app, "call-b", "cat"));

    let AppAction::SendToolDecisions {
        client_request_id: first_id,
        rollback: first_rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('y')))
    else {
        panic!("expected first tool decision action");
    };
    let AppAction::SendToolDecisions {
        client_request_id: second_id,
        rollback: second_rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('n')))
    else {
        panic!("expected second tool decision action");
    };

    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: second_id,
            rollback: second_rollback,
        },
        Err("second decision rejected".to_string()),
    );
    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id: first_id,
            rollback: first_rollback,
        },
        Ok(()),
    );

    assert!(matches!(
        app.approval_modal(),
        Some(modal) if modal.tool_call_ids() == ["call-b"]
    ));
    assert_eq!(app.approval_pending_clear_count(), 1);
}

#[test]
fn stale_tool_decision_failure_preserves_newer_approval() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-a", "shell"));
    let AppAction::SendToolDecisions {
        client_request_id,
        rollback,
        ..
    } = app.handle_key(key(KeyCode::Char('y')))
    else {
        panic!("expected tool decision action");
    };
    app.handle_chat_event(pause_event(&app, "call-b", "cat"));

    app.handle_command_finished(
        CommandContextTag::ToolDecisions {
            origin: app.command_origin(),
            client_request_id,
            rollback,
        },
        Err("first decision rejected".to_string()),
    );

    assert!(matches!(
        app.approval_modal(),
        Some(modal) if modal.tool_call_ids() == ["call-a"] && modal.pending_after() == 1
    ));
    let action = app.handle_key(key(KeyCode::Char('n')));
    assert!(matches!(
        action,
        AppAction::SendToolDecisions { decisions, .. }
            if decisions == vec![ToolDecision {
                tool_call_id: "call-a".to_string(),
                accepted: false,
            }]
    ));
    assert!(matches!(
        app.approval_modal(),
        Some(modal) if modal.tool_call_ids() == ["call-b"]
    ));
}

#[test]
fn multi_tool_pause_yields_per_tool_decisions() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "pause_required".to_string(),
            raw: json!({"reasons": [
                {"type": "confirmation", "tool_name": "shell", "command": "echo one", "rule": "*", "tool_call_id": "call-1"},
                {"type": "confirmation", "tool_name": "cat", "command": "cat Cargo.toml", "rule": "*", "tool_call_id": "call-2"}
            ]}),
        });

    assert_tool_decision_action(
        &app.handle_key(key(KeyCode::Char('n'))),
        vec![
            ToolDecision {
                tool_call_id: "call-1".to_string(),
                accepted: false,
            },
            ToolDecision {
                tool_call_id: "call-2".to_string(),
                accepted: false,
            },
        ],
        None,
    );
}

#[test]
fn approval_detail_toggle_uses_escape_as_back_not_deny() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));

    assert!(!app.approval_modal().unwrap().details_open());
    assert_eq!(app.handle_key(key(KeyCode::Char('v'))), AppAction::None);
    assert!(app.approval_modal().unwrap().details_open());
    assert_eq!(app.handle_key(key(KeyCode::Down)), AppAction::None);
    assert_eq!(app.approval_modal().unwrap().detail_scroll(), 1);
    assert_eq!(app.handle_key(key(KeyCode::Esc)), AppAction::None);
    let modal = app.approval_modal().unwrap();
    assert!(!modal.details_open());
    assert_eq!(modal.detail_scroll(), 0);
}

#[test]
fn two_approvals_before_first_resolved_preserve_fifo() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));
    app.handle_chat_event(pause_event(&app, "call-2", "cat"));

    let first = app.approval_modal().unwrap();
    assert_eq!(first.reasons()[0].tool_call_id, "call-1");
    assert_eq!(first.pending_after(), 1);

    let first_action = app.handle_key(key(KeyCode::Char('y')));
    assert_tool_decision_action(
        &first_action,
        vec![ToolDecision {
            tool_call_id: "call-1".to_string(),
            accepted: true,
        }],
        None,
    );
    assert_eq!(app.approval_pending_clear_count(), 1);
    let second = app.approval_modal().unwrap();
    assert_eq!(second.reasons()[0].tool_call_id, "call-2");
    assert_eq!(second.pending_after(), 0);

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "pause_cleared".to_string(),
        raw: json!({}),
    });
    assert!(app.approval_modal().is_some());
    assert_eq!(app.approval_pending_clear_count(), 0);

    let second_action = app.handle_key(key(KeyCode::Char('n')));
    assert_tool_decision_action(
        &second_action,
        vec![ToolDecision {
            tool_call_id: "call-2".to_string(),
            accepted: false,
        }],
        None,
    );
    assert!(app.approval_modal().is_none());
}

#[test]
fn decided_approval_snapshot_replay_skips_decided_scope_and_keeps_pending() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-a", "shell"));
    app.handle_chat_event(pause_event(&app, "call-b", "cat"));

    let first_action = app.handle_key(key(KeyCode::Char('y')));
    assert_tool_decision_action(
        &first_action,
        vec![ToolDecision {
            tool_call_id: "call-a".to_string(),
            accepted: true,
        }],
        None,
    );
    assert_eq!(app.approval_pending_clear_count(), 1);
    assert_eq!(
        app.approval_modal().unwrap().reasons()[0].tool_call_id,
        "call-b"
    );

    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"runtime": {"state": "paused", "pause_reasons": [pause_reason_value("call-a", "shell")]}, "messages": []}),
        });
    assert_eq!(app.approval_pending_clear_count(), 1);
    assert_eq!(
        app.approval_modal().unwrap().reasons()[0].tool_call_id,
        "call-b"
    );

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "pause_cleared".to_string(),
        raw: json!({}),
    });
    assert_eq!(app.approval_pending_clear_count(), 0);
    assert_eq!(
        app.approval_modal().unwrap().reasons()[0].tool_call_id,
        "call-b"
    );

    let second_action = app.handle_key(key(KeyCode::Char('n')));
    assert_tool_decision_action(
        &second_action,
        vec![ToolDecision {
            tool_call_id: "call-b".to_string(),
            accepted: false,
        }],
        None,
    );
    assert!(app.approval_modal().is_none());
}

#[test]
fn snapshot_brand_new_pause_preserves_existing_pending_order() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-a", "shell"));

    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "snapshot".to_string(),
            raw: json!({"runtime": {"state": "paused", "pause_reasons": [pause_reason_value("call-c", "cat")]}, "messages": []}),
        });

    let first = app.approval_modal().unwrap();
    assert_eq!(first.reasons()[0].tool_call_id, "call-a");
    assert_eq!(first.pending_after(), 1);

    let first_action = app.handle_key(key(KeyCode::Char('y')));
    assert_tool_decision_action(
        &first_action,
        vec![ToolDecision {
            tool_call_id: "call-a".to_string(),
            accepted: true,
        }],
        None,
    );
    assert_eq!(
        app.approval_modal().unwrap().reasons()[0].tool_call_id,
        "call-c"
    );
}

#[test]
fn pause_cleared_with_empty_approval_queue_is_noop() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "pause_cleared".to_string(),
        raw: json!({}),
    });

    assert!(app.approval_modal().is_none());
    assert_eq!(app.approval_pending_clear_count(), 0);
}

#[test]
fn pause_cleared_with_tool_call_id_clears_matching_scope_only() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-a", "shell"));
    app.handle_chat_event(pause_event(&app, "call-b", "cat"));

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "pause_cleared".to_string(),
        raw: json!({"tool_call_id": "call-b"}),
    });

    assert_eq!(
        app.approval_modal().unwrap().reasons()[0].tool_call_id,
        "call-a"
    );
    assert_eq!(app.approval_modal().unwrap().pending_after(), 0);
}

#[test]
fn markdown_collector_commits_complete_lines_to_transcript() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_started".to_string(),
        raw: json!({}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "append_content", "text": "# Title\npartial"}]}),
    });
    assert_eq!(assistant_text(&app), "# Title\npartial");
    assert!(app.stream_has_committable_lines());

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "append_content", "text": " tail"}]}),
    });
    assert_eq!(assistant_text(&app), "# Title\npartial tail");
    assert!(app.stream_has_committable_lines());

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_finished".to_string(),
        raw: json!({}),
    });
    assert_eq!(assistant_text(&app), "# Title\npartial tail\n");
}

#[test]
fn unfinished_plan_message_streams_then_finalizes_to_plan_cell() {
    let mut app = App::new(project());

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "message_id": "p1",
            "role": "plan",
            "content": "## Plan\n",
            "stream_finished": false,
            "extra": {"plan": {"mode": "agent", "version": 1}}
        }}),
    });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_updated".to_string(),
        raw: json!({"message_id": "p1", "message": {
            "message_id": "p1",
            "role": "plan",
            "content": "## Plan\n- one\n",
            "stream_finished": false,
            "extra": {"plan": {"mode": "agent", "version": 1}}
        }}),
    });

    let streamed = plan_stream_text(&app);
    assert!(streamed.contains("Proposed Plan"));
    assert!(streamed.contains("## Plan"));
    assert!(streamed.contains("- one"));

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_updated".to_string(),
        raw: json!({"message_id": "p1", "message": {
            "message_id": "p1",
            "role": "plan",
            "content": "## Plan\n- one\n",
            "stream_finished": true,
            "extra": {"plan": {"mode": "agent", "version": 1}}
        }}),
    });

    assert!(plan_stream_text(&app).is_empty());
    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Plan(data) if data.content.contains("- one"))
    }));
}

#[test]
fn native_plan_stream_commits_non_final_plan_cells() {
    let mut app = App::new(project());
    app.set_native_scrollback(true);

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {
            "message_id": "p1",
            "role": "plan",
            "content": "- one\n- two\n- three\n- four\n- five\n- six\n- seven\n- eight\n",
            "stream_finished": false,
            "extra": {"plan": {"mode": "agent", "version": 1}}
        }}),
    });

    app.apply_stream_commit_tick();
    assert!(app.history_pending_count() > 0);
    let pending_text = app
        .pending_history_insertions(80)
        .into_iter()
        .flat_map(|insertion| insertion.lines)
        .map(|line| line_to_plain_string(&line.line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pending_text.contains("Proposed Plan"));

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_updated".to_string(),
        raw: json!({"message_id": "p1", "message": {
            "message_id": "p1",
            "role": "plan",
            "content": "- one\n- two\n- three\n- four\n- five\n- six\n- seven\n- eight\n",
            "stream_finished": true,
            "extra": {"plan": {"mode": "agent", "version": 1}}
        }}),
    });

    assert!(plan_stream_text(&app).is_empty());
    assert!(app.resize_reflow_is_due());
    app.resize_reflow_insertions(80, 100);
    app.pending_history_insertions(80);
    assert_eq!(app.history_pending_count(), 0);
    assert!(app.visible_transcript().iter().any(|item| {
        matches!(item, TranscriptItem::Plan(data) if data.content.contains("- eight"))
    }));
}

#[test]
fn authoritative_mutation_rebuilds_active_plan_stream_once_and_continues() {
    for native_scrollback in [false, true] {
        let mut app = App::new(project());
        app.set_native_scrollback(native_scrollback);
        if native_scrollback {
            app.pending_history_insertions(80);
        }
        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"message": {
                "message_id": "p1",
                "role": "plan",
                "content": "## Plan\n- base\n",
                "stream_finished": false,
                "extra": {"plan": {"mode": "agent", "version": 1}},
            }}),
        });
        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"message": {
                "message_id": "d1",
                "role": "event",
                "content": "- first delta\n",
                "stream_finished": false,
                "extra": {"event": {"subkind": "plan_delta", "payload": {"seq": 1}}},
            }}),
        });
        let mut committed_all = String::new();
        if native_scrollback {
            app.apply_stream_commit_tick();
            committed_all.push_str(&pending_plain_text(&mut app));
        }

        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"index": 0, "message": {
                "message_id": "u1",
                "role": "user",
                "content": "inserted",
            }}),
        });

        if native_scrollback {
            committed_all.push_str(&pending_plain_text(&mut app));
        } else {
            let rebuilt = plan_stream_text(&app);
            for content in ["## Plan", "- base", "- first delta"] {
                assert_eq!(rebuilt.matches(content).count(), 1, "{content}: {rebuilt}");
            }
            assert_eq!(
                app.visible_transcript()
                    .iter()
                    .filter(|item| matches!(item, TranscriptItem::PlanStream(_)))
                    .count(),
                1
            );
        }
        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_updated".to_string(),
            raw: json!({"message_id": "d1", "message": {
                "message_id": "d1",
                "role": "event",
                "content": "- first delta\n- continued\n",
                "stream_finished": false,
                "extra": {"event": {"subkind": "plan_delta", "payload": {"seq": 1}}},
            }}),
        });
        if native_scrollback {
            for _ in 0..8 {
                app.apply_stream_commit_tick();
                committed_all.push_str(&pending_plain_text(&mut app));
            }
            committed_all.push_str(&plan_stream_text(&app));
            for content in ["## Plan", "- base", "- first delta", "- continued"] {
                assert_eq!(
                    committed_all.matches(content).count(),
                    1,
                    "{content}: {committed_all}"
                );
            }
        } else {
            let continued = plan_stream_text(&app);
            for content in ["- base", "- first delta", "- continued"] {
                assert_eq!(
                    continued.matches(content).count(),
                    1,
                    "{content}: {continued}"
                );
            }
        }

        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_updated".to_string(),
            raw: json!({"message_id": "p1", "message": {
                "message_id": "p1",
                "role": "plan",
                "content": "## Plan\n- base\n",
                "stream_finished": true,
                "extra": {"plan": {"mode": "agent", "version": 1}},
            }}),
        });
        app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_updated".to_string(),
            raw: json!({"message_id": "d1", "message": {
                "message_id": "d1",
                "role": "event",
                "content": "- first delta\n- continued\n",
                "stream_finished": true,
                "extra": {"event": {"subkind": "plan_delta", "payload": {"seq": 1}}},
            }}),
        });

        assert!(plan_stream_text(&app).is_empty());
        let plans = app
            .visible_transcript()
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::Plan(data) => Some(data),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(plans.len(), 1);
        for content in ["- base", "- first delta", "- continued"] {
            assert_eq!(
                plans[0].content.matches(content).count(),
                1,
                "{content}: {}",
                plans[0].content
            );
        }
    }
}

#[test]
fn set_tool_calls_updates_existing_card_by_id() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "shell", "arguments": "{\"cmd\":\"echo 1\"}"}}]}]}),
        });
    assert_eq!(tool_cards(&app).len(), 1);
    app.complete_tool("call-1", "tool", "done".to_string(), ToolStatus::Succeeded);
    if let [card] = tool_cards(&app).as_slice() {
        assert_eq!(card.status, ToolStatus::Succeeded);
        assert_eq!(card.result, "done");
    } else {
        panic!("expected one card");
    }
    app.handle_key(key(KeyCode::Enter));

    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "shell", "arguments": "{\"cmd\":\"echo 2\"}"}}]}]}),
        });
    let cards = tool_cards(&app);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].status, ToolStatus::Succeeded);
    assert_eq!(cards[0].result, "done");
    assert!(cards[0].expanded);
    assert!(cards[0].args_preview.contains("echo 2"));
}

#[test]
fn working_indicator_ticks_and_tracks_current_tool_detail() {
    let mut app = App::new(project());
    app.apply_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "runtime_updated".to_string(),
        raw: json!({"state": "generating"}),
    });
    assert_eq!(app.working_tick(), 0);
    app.apply_stream_commit_tick();
    assert_eq!(app.working_tick(), 1);

    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "shell", "arguments": "{\"cmd\":\"echo 1\"}"}}]}]}),
        });
    assert!(app
        .working_detail()
        .is_some_and(|detail| detail.contains("shell") && detail.contains("echo 1")));

    app.apply_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "runtime_updated".to_string(),
        raw: json!({"state": "idle"}),
    });
    assert_eq!(app.working_tick(), 0);
    assert_eq!(app.working_elapsed_ms(), 0);
    assert_eq!(app.working_detail(), None);
}

#[test]
fn reasoning_toggle_survives_append_updates() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "append_reasoning", "text": "first"}]}),
    });
    assert!(matches!(
        app.visible_transcript().last(),
        Some(TranscriptItem::Reasoning(_, true))
    ));
    app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT));
    assert!(matches!(
        app.visible_transcript().last(),
        Some(TranscriptItem::Reasoning(_, false))
    ));
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "append_reasoning", "text": " second"}]}),
    });
    assert!(matches!(
        app.visible_transcript().last(),
        Some(TranscriptItem::Reasoning(text, false)) if text == "first second"
    ));
}

#[test]
fn model_text_escape_sanitization_reaches_live_transcript_and_render_cells() {
    let mut app = App::new(project());
    let injected = injected_model_text();
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [
            {"op": "append_reasoning", "text": injected},
            {"op": "append_content", "text": injected}
        ]}),
    });

    let assistant = assistant_text(&app);
    let reasoning = reasoning_text(&app);
    assert_escape_inert(&assistant);
    assert_escape_inert(&reasoning);
    assert_model_text_survives(&assistant);
    assert_model_text_survives(&reasoning);
    for item in app.visible_transcript() {
        if matches!(
            item,
            TranscriptItem::Assistant(_) | TranscriptItem::Reasoning(_, _)
        ) {
            assert_rendered_item_escape_inert(item);
        }
    }
}

#[test]
fn inbound_user_message_escape_text_stays_inert_in_rendered_cells() {
    let mut app = App::new(project());
    let injected = injected_model_text();
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "message_added".to_string(),
        raw: json!({"message": {"role": "user", "message_id": "u1", "content": injected}}),
    });

    let user = app
        .transcript_state()
        .messages()
        .iter()
        .find(|message| message.role == TranscriptRole::User)
        .unwrap();
    assert_escape_inert(&user.content);
    assert_model_text_survives(&user.content);
    for item in app.visible_transcript() {
        if matches!(item, TranscriptItem::User(_)) {
            assert_rendered_item_escape_inert(item);
        }
    }
}

#[test]
fn tool_expand_survives_result_update() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "shell", "arguments": "{\"cmd\":\"echo hi\"}"}}]}]}),
        });
    assert!(app.toggle_selected_tool());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "message_added".to_string(),
            raw: json!({"message": {"role": "tool", "tool_call_id": "call-1", "content": "done", "tool_failed": false}}),
        });
    let cards = tool_cards(&app);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].status, ToolStatus::Succeeded);
    assert_eq!(cards[0].result, "done");
    assert!(cards[0].expanded);
}

#[test]
fn subchat_update_attaches_bounded_progress_to_parent_tool() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "tool_subagent", "arguments": "{}"}}]}]}),
        });
    let attached = (0..20)
        .map(|idx| format!("src/file_{idx}.rs"))
        .collect::<Vec<_>>();
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "subchat_update".to_string(),
        raw: json!({
            "tool_call_id": "call-1",
            "subchat_id": "1/2: search({\"query\":\"needle\"})",
            "attached_files": attached,
            "depth": 9
        }),
    });

    let cards = tool_cards(&app);
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0].subchat_log,
        vec!["1/2: search({\"query\":\"needle\"})"]
    );
    assert_eq!(cards[0].attached_files.len(), MAX_SUBCHAT_ATTACHED_FILES);
    assert_eq!(cards[0].subchat_depth, MAX_SUBCHAT_DEPTH);
    assert!(cards[0].subchat_active);
    assert!(cards[0].subchat_truncated);
}

#[test]
fn subchat_update_file_notice_does_not_replace_latest_progress_and_clear_resets() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "tool_subagent", "arguments": "{}"}}]}]}),
        });
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "subchat_update".to_string(),
        raw: json!({"tool_call_id": "call-1", "subchat_id": "thinking", "attached_files": []}),
    });
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "subchat_update".to_string(),
            raw: json!({"tool_call_id": "call-1", "subchat_id": "/tool:files", "attached_files": ["README.md"]}),
        });
    assert_eq!(tool_cards(&app)[0].subchat_log, vec!["thinking"]);
    assert_eq!(tool_cards(&app)[0].attached_files, vec!["README.md"]);

    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "subchat_update".to_string(),
        raw: json!({"tool_call_id": "call-1", "subchat_id": "", "attached_files": []}),
    });
    assert!(tool_cards(&app)[0].subchat_log.is_empty());
    assert!(tool_cards(&app)[0].attached_files.is_empty());
    assert!(!tool_cards(&app)[0].subchat_active);
}

#[test]
fn subagents_command_lists_live_activity_and_empty_state() {
    let _surface_lock = goal_dock::test_surface_lock();
    let mut app = App::new(project());
    app.execute_command_name("subagents");
    assert!(matches!(
        app.visible_transcript().last(),
        Some(TranscriptItem::Info(lines)) if lines.iter().any(|line| line == "No active subagents")
    ));

    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "tool_subagent", "arguments": "{}"}}]}]}),
        });
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "subchat_update".to_string(),
            raw: json!({"tool_call_id": "call-1", "subchat_id": "collecting context", "attached_files": ["src/lib.rs"]}),
        });
    app.execute_command_name("multi-agents");
    assert!(matches!(
        app.visible_transcript().last(),
        Some(TranscriptItem::Info(lines))
            if lines.join("\n").contains("tool_subagent [call-1]")
                && lines.join("\n").contains("collecting context")
    ));
}

#[test]
fn subagents_command_hides_internal_tool_prefixes() {
    let _surface_lock = goal_dock::test_surface_lock();
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "stream_delta".to_string(),
            raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [{"id": "call-1", "function": {"name": "t_process_start", "arguments": "{}"}}]}]}),
        });
    app.handle_chat_event(ChatEvent {
            chat_id: Some(app.chat_id().to_string()),
            seq: None,
            kind: "subchat_update".to_string(),
            raw: json!({"tool_call_id": "call-1", "subchat_id": "collecting context", "attached_files": []}),
        });
    app.execute_command_name("subagents");

    let rendered = match app.visible_transcript().last() {
        Some(TranscriptItem::Info(lines)) => lines.join("\n"),
        _ => panic!("expected subagents card"),
    };
    assert!(rendered.contains("process_start [call-1]"));
    assert!(!rendered.contains("t_process_start"));
}

#[test]
fn approval_decision_appends_record_cell() {
    let mut app = App::new(project());
    app.handle_chat_event(pause_event(&app, "call-1", "shell"));
    let action = app.handle_key(key(KeyCode::Char('y')));
    assert!(matches!(action, AppAction::SendToolDecisions { .. }));
    assert!(matches!(
        app.visible_transcript().last(),
        Some(TranscriptItem::Approval(_, Some(ToolStatus::ApprovedOnce)))
    ));
}

#[test]
fn approve_for_chat_sets_patch_scope() {
    let mut app = App::new(project());
    app.test_set_approval(ApprovalModalState::new(vec![PauseReason {
        reason_type: "confirmation".to_string(),
        tool_name: "update_textdoc".to_string(),
        command: "{}".to_string(),
        rule: "default".to_string(),
        tool_call_id: "call-1".to_string(),
        integr_config_path: None,
        args: None,
        diff: None,
    }]));
    let action = app.handle_key(key(KeyCode::Char('a')));
    assert_tool_decision_action(
        &action,
        vec![ToolDecision {
            tool_call_id: "call-1".to_string(),
            accepted: true,
        }],
        Some(json!({"auto_approve_editing_tools": true})),
    );
}

#[test]
fn picker_selection_applies_params_to_next_message() {
    let mut app = App::new(project());
    app.open_model_picker(json!({"chat_models": {"m1": {"name": "Model One"}}}));
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    app.composer.set_text("hello");
    let action = app.handle_key(key(KeyCode::Enter));
    match action {
        AppAction::SendMessage { params, .. } => {
            assert_eq!(params["model"], "m1");
        }
        other => panic!("unexpected action: {other:?}"),
    }
}

#[test]
fn modal_picker_space_still_toggles_multi_select() {
    let mut app = App::new(project());

    app.test_open_permissions_picker();
    assert_eq!(app.modal_picker().unwrap().selected_count(), 0);
    assert_eq!(app.handle_key(key(KeyCode::Char(' '))), AppAction::None);

    let picker = app.modal_picker().unwrap();
    assert_eq!(picker.selected_count(), 1);
    assert!(picker.is_selected("editing_tools"));
}

#[test]
fn tab_cycles_tool_selection_and_enter_toggles_selected_tool() {
    let mut app = App::new(project());
    app.handle_chat_event(ChatEvent {
        chat_id: Some(app.chat_id().to_string()),
        seq: None,
        kind: "stream_delta".to_string(),
        raw: json!({"ops": [{"op": "set_tool_calls", "tool_calls": [
            {"id": "call-1", "function": {"name": "shell", "arguments": "{}"}},
            {"id": "call-2", "function": {"name": "cat", "arguments": "{}"}}
        ]}]}),
    });

    assert_eq!(app.selected_tool_index(), Some(2));
    assert_eq!(app.handle_key(key(KeyCode::Tab)), AppAction::None);
    assert_eq!(app.selected_tool_index(), Some(1));
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);
    assert!(tool_cards(&app)[0].expanded);
}

#[test]
fn caps_update_context_window_for_selected_and_default_models() {
    let mut app = App::new(project());
    app.apply_caps(&json!({
        "defaults": {"chat_default_model": "openai/gpt-demo"},
        "chat_models": {
            "openai/gpt-demo": {"n_ctx": 128_000},
            "openai/gpt-small": {"n_ctx": 32_000}
        }
    }));

    assert_eq!(app.context_window_tokens(), Some(128_000));

    app.open_model_picker(
        json!({"chat_models": {"openai/gpt-small": {"name": "Small", "n_ctx": 32_000}}}),
    );
    assert_eq!(app.handle_key(key(KeyCode::Enter)), AppAction::None);

    assert_eq!(app.context_window_tokens(), Some(32_000));
}

#[test]
fn subscription_status_tracks_waking_offline_and_retry_hints() {
    let mut app = App::new(project());

    app.begin_subscription_attempt();
    assert_eq!(app.subscription_status(), SubscriptionStatus::Waking);
    assert!(app.daemon_online());

    app.record_chat_disconnected(
        "request failed with status 503: worker starting",
        true,
        false,
    );
    assert_eq!(app.subscription_status(), SubscriptionStatus::Waking);
    assert_eq!(app.retry_hint(), Some("worker waking; retrying"));
    assert!(app.daemon_online());

    app.record_chat_disconnected("connection refused", true, false);
    assert_eq!(app.subscription_status(), SubscriptionStatus::Waking);
    assert!(app.daemon_online());

    app.record_chat_resubscribe("request failed with status 429: retry-after: 2s");
    assert_eq!(app.subscription_status(), SubscriptionStatus::Waking);
    assert_eq!(app.retry_hint(), Some("rate limited; retry after 2s"));
}

#[test]
fn exhausted_reconnect_is_visible_and_retry_is_bound() {
    let mut app = App::new(project());

    app.record_subscription_exhausted();

    assert_eq!(app.subscription_status(), SubscriptionStatus::Offline);
    assert!(!app.daemon_online());
    assert_eq!(app.session_state(), SessionState::Error);
    assert_eq!(
        app.retry_hint(),
        Some("SSE reconnect retries exhausted; retry the subscription")
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )),
        AppAction::SubscribeCurrent
    );
}

#[test]
fn seq_gap_requests_resubscribe_before_gap_delta_applies() {
    let mut app = App::new(project());
    let mut tracker = ChatSeqTracker::new();
    let snapshot = chat_event(
        &app,
        0,
        "snapshot",
        json!({"thread": {"model": "", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": []}),
    );
    assert_eq!(
        apply_tracked(&mut app, &mut tracker, snapshot),
        ChatSeqDecision::Apply
    );
    let started = chat_event(&app, 1, "stream_started", json!({}));
    assert_eq!(
        apply_tracked(&mut app, &mut tracker, started),
        ChatSeqDecision::Apply
    );
    let kept = chat_event(
        &app,
        2,
        "stream_delta",
        json!({"ops": [{"op": "append_content", "text": "kept\n"}]}),
    );
    assert_eq!(
        apply_tracked(&mut app, &mut tracker, kept),
        ChatSeqDecision::Apply
    );
    let dropped = chat_event(
        &app,
        4,
        "stream_delta",
        json!({"ops": [{"op": "append_content", "text": "dropped"}]}),
    );
    let decision = apply_tracked(&mut app, &mut tracker, dropped);
    assert!(matches!(decision, ChatSeqDecision::Resubscribe(_)));
    assert_eq!(assistant_text(&app), "kept\n");
}

#[test]
fn duplicate_seq_does_not_duplicate_transcript_content() {
    let mut app = App::new(project());
    let mut tracker = ChatSeqTracker::new();
    let snapshot = chat_event(
        &app,
        0,
        "snapshot",
        json!({"thread": {"model": "", "mode": "agent"}, "runtime": {"state": "idle"}, "messages": []}),
    );
    assert_eq!(
        apply_tracked(&mut app, &mut tracker, snapshot),
        ChatSeqDecision::Apply
    );
    let started = chat_event(&app, 1, "stream_started", json!({}));
    assert_eq!(
        apply_tracked(&mut app, &mut tracker, started),
        ChatSeqDecision::Apply
    );
    let delta = json!({"ops": [{"op": "append_content", "text": "once\n"}]});
    let first = chat_event(&app, 2, "stream_delta", delta.clone());
    assert_eq!(
        apply_tracked(&mut app, &mut tracker, first),
        ChatSeqDecision::Apply
    );
    let duplicate = chat_event(&app, 2, "stream_delta", delta);
    let decision = apply_tracked(&mut app, &mut tracker, duplicate);
    assert_eq!(decision, ChatSeqDecision::Suppress);
    assert_eq!(assistant_text(&app), "once\n");
}

#[tokio::test]
async fn stale_subscription_generation_is_ignored() {
    let mut app = App::new(project());
    let mut subscriptions = SubscriptionManager::new();
    let join = tokio::spawn(async { futures::future::pending::<()>().await });
    subscriptions.current = Some(SubscriptionTask {
        generation: 2,
        project_id: "p2".to_string(),
        chat_id: app.chat_id().to_string(),
        reconnect_attempts: 0,
        join,
    });
    let stale = chat_event(
        &app,
        1,
        "stream_delta",
        json!({"ops": [{"op": "append_content", "text": "stale"}]}),
    );
    assert!(subscriptions.apply_chat_event(&mut app, 1, stale).is_none());
    assert!(matches!(
        app.visible_transcript(),
        [TranscriptItem::Notice(_)]
    ));
    let fresh = chat_event(
        &app,
        1,
        "stream_delta",
        json!({"ops": [{"op": "append_content", "text": "fresh\n"}]}),
    );
    assert!(subscriptions.apply_chat_event(&mut app, 2, fresh).is_some());
    assert_eq!(assistant_text(&app), "fresh\n");
}
#[tokio::test]
async fn subscription_switch_aborts_prior_task() {
    let mut subscriptions = SubscriptionManager::new();
    subscriptions.generation = 1;
    let join = tokio::spawn(async { futures::future::pending::<()>().await });
    let abort = join.abort_handle();
    subscriptions.current = Some(SubscriptionTask {
        generation: 1,
        project_id: "p1".to_string(),
        chat_id: "c1".to_string(),
        reconnect_attempts: 0,
        join,
    });
    let client = DaemonClient::new("http://127.0.0.1:1", None).unwrap();
    let (tx, _rx) = mpsc::channel(1);
    subscriptions.subscribe(client, "p2".to_string(), "c2".to_string(), tx);
    for _ in 0..10 {
        if abort.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(abort.is_finished());
    assert!(subscriptions.is_current(2));
    subscriptions.abort_current();
}
