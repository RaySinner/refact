use super::*;

fn message(role: &str) -> ChatMessage {
    ChatMessage {
        message_id: format!("{role}-id"),
        role: role.to_string(),
        content: ChatContent::SimpleText(role.to_string()),
        ..Default::default()
    }
}

#[test]
fn fixed_creation_instant_is_flattened_and_immutable_for_all_roles() {
    for role in ["user", "assistant", "event"] {
        let mut message = message(role);
        stamp_created_at_ms_at(&mut message, 1_234_567);
        stamp_created_at_ms_at(&mut message, 9_999_999);

        assert_eq!(created_at_ms(&message), Some(1_234_567));
        assert_eq!(
            serde_json::to_value(&message).unwrap()["created_at_ms"],
            json!(1_234_567)
        );
    }
}

#[test]
fn assistant_start_and_finish_keep_the_start_instant() {
    let mut session = ChatSession::new("assistant-timestamp".to_string());
    session.start_stream_at(10_000).unwrap();
    assert_eq!(
        created_at_ms(session.draft_message.as_ref().unwrap()),
        Some(10_000)
    );

    session.emit_stream_delta(vec![DeltaOp::AppendContent {
        text: "answer".to_string(),
    }]);
    session.finish_stream(None);

    assert_eq!(session.messages.len(), 1);
    assert_eq!(created_at_ms(&session.messages[0]), Some(10_000));
}

#[test]
fn update_preserves_or_accepts_creation_instant_without_backfill() {
    let mut session = ChatSession::new("update-timestamp".to_string());
    let mut original = message("user");
    stamp_created_at_ms_at(&mut original, 20_000);
    session.add_message(original);

    session.update_message("user-id", message("user"));
    assert_eq!(created_at_ms(&session.messages[0]), Some(20_000));

    let mut replacement = message("user");
    stamp_created_at_ms_at(&mut replacement, 30_000);
    session.update_message("user-id", replacement);
    assert_eq!(created_at_ms(&session.messages[0]), Some(30_000));

    session.messages.push(message("assistant"));
    session.update_message("assistant-id", message("assistant"));
    assert!(!session.messages[1].extra.contains_key("created_at_ms"));
}
