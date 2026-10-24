use super::*;
use super::tests_support::*;
use serde_json::json;

    #[test]
    fn test_add_message_generates_id_if_empty() {
        let mut session = make_session();
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        session.add_message(msg);
        assert!(!session.messages[0].message_id.is_empty());
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn test_add_message_preserves_existing_id() {
        let mut session = make_session();
        let msg = ChatMessage {
            message_id: "custom-id".into(),
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        session.add_message(msg);
        assert_eq!(session.messages[0].message_id, "custom-id");
    }

    #[test]
    fn test_add_message_stamps_created_at_ms() {
        let mut session = make_session();
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        session.add_message(msg);
        let created_at_ms = session
            .messages[0]
            .extra
            .get("created_at_ms")
            .and_then(|v| v.as_u64());
        assert!(created_at_ms.is_some_and(|ms| ms > 0));
    }

    #[test]
    fn test_add_message_preserves_existing_created_at_ms() {
        let mut session = make_session();
        let mut msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        msg.extra.insert("created_at_ms".into(), json!(42_000));
        session.add_message(msg);
        assert_eq!(
            session
                .messages[0]
                .extra
                .get("created_at_ms")
                .and_then(|v| v.as_u64()),
            Some(42_000)
        );
    }

    #[test]
    fn test_insert_message_stamps_created_at_ms() {
        let mut session = make_session();
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::SimpleText("hi".into()),
            ..Default::default()
        };
        session.insert_message(0, msg);
        assert!(session
            .messages[0]
            .extra
            .get("created_at_ms")
            .and_then(|v| v.as_u64())
            .is_some_and(|ms| ms > 0));
    }

    #[test]
    fn test_update_message_returns_index() {
        let mut session = make_session();
        let msg = ChatMessage {
            message_id: "m1".into(),
            role: "user".into(),
            content: ChatContent::SimpleText("original".into()),
            ..Default::default()
        };
        session.messages.push(msg);
        let updated = ChatMessage {
            message_id: "m1".into(),
            role: "user".into(),
            content: ChatContent::SimpleText("updated".into()),
            ..Default::default()
        };
        let idx = session.update_message("m1", updated);
        assert_eq!(idx, Some(0));
        match &session.messages[0].content {
            ChatContent::SimpleText(s) => assert_eq!(s, "updated"),
            _ => panic!("Expected SimpleText"),
        }
    }

    #[test]
    fn test_update_message_unknown_id_returns_none() {
        let mut session = make_session();
        let msg = ChatMessage::default();
        assert!(session.update_message("unknown", msg).is_none());
    }

    #[test]
    fn test_remove_message_returns_index() {
        let mut session = make_session();
        session.messages.push(ChatMessage {
            message_id: "m1".into(),
            ..Default::default()
        });
        session.messages.push(ChatMessage {
            message_id: "m2".into(),
            ..Default::default()
        });
        let idx = session.remove_message("m1");
        assert_eq!(idx, Some(0));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].message_id, "m2");
    }

    #[test]
    fn test_remove_message_unknown_id_returns_none() {
        let mut session = make_session();
        assert!(session.remove_message("unknown").is_none());
    }

    #[test]
    fn test_truncate_messages() {
        let mut session = make_session();
        for i in 0..5 {
            session.messages.push(ChatMessage {
                message_id: format!("m{}", i),
                ..Default::default()
            });
        }
        session.truncate_messages(2);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[1].message_id, "m1");
    }

    #[test]
    fn test_truncate_beyond_length_is_noop() {
        let mut session = make_session();
        session.messages.push(ChatMessage::default());
        let version_before = session.trajectory_version;
        session.truncate_messages(10);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.trajectory_version, version_before);
    }

    #[test]
    fn ide_tool_result_emits_event_not_user_message() {
        let mut session = make_session();
        session.runtime.state = SessionState::WaitingIde;
        let mut rx = session.subscribe();

        session.record_ide_tool_result(
            "call_ide_1".to_string(),
            "The user accepted the changes.".to_string(),
            false,
        );

        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, "tool");
        assert_eq!(session.messages[0].tool_call_id, "call_ide_1");
        assert_eq!(session.messages[0].tool_failed, Some(false));
        assert_eq!(
            session.messages[1].role,
            crate::chat::internal_roles::EVENT_ROLE
        );
        assert!(!session.messages.iter().any(|message| {
            message.role == "user"
                && message
                    .content
                    .content_text_only()
                    .contains("accepted the changes")
        }));
        assert_eq!(
            session.messages[1].extra["event"]["subkind"],
            json!("ide_callback")
        );
        assert_eq!(
            session.messages[1].extra["event"]["source"],
            json!("ide.bridge")
        );
        assert_eq!(
            session.messages[1].extra["event"]["payload"],
            json!({
                "tool_call_id": "call_ide_1",
                "ok": true,
                "summary": "The user accepted the changes."
            })
        );
        assert_eq!(
            session.messages[1].content.content_text_only(),
            "The user accepted the changes."
        );
        assert_eq!(session.runtime.state, SessionState::WaitingIde);

        let mut event_roles = Vec::new();
        while let Ok(json) = rx.try_recv() {
            let envelope: EventEnvelope = serde_json::from_str(&json).unwrap();
            if let ChatEvent::MessageAdded { message, .. } = envelope.event {
                event_roles.push(message.role);
            }
        }
        assert_eq!(
            event_roles,
            vec!["tool", crate::chat::internal_roles::EVENT_ROLE]
        );
    }

    #[test]
    fn test_skill_deactivation_cleanup_compacts_messages() {
        let mut session = make_session();

        // Add 2 pre-skill messages
        session.add_message(make_user_message("pre-skill message 1"));
        session.add_message(make_user_message("pre-skill message 2"));
        let anchor = session.messages.len(); // = 2

        // Add skill-run messages that should be removed
        session.add_message(make_user_message("skill run message A"));
        session.add_message(make_user_message("skill run message B"));
        session.add_message(make_user_message("skill run message C"));
        assert_eq!(session.messages.len(), 5);

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: anchor,
            skill_name: "my-skill".to_string(),
            report: "Did useful things.".to_string(),
            activation_tool_call_id: None,
        });

        session.perform_skill_deactivation_cleanup();

        // 2 pre-skill messages + 1 report = 3 total
        assert_eq!(session.messages.len(), 3, "Expected 2 pre-skill + 1 report");
        let last = session.messages.last().unwrap();
        assert_eq!(last.role, "plain_text");
        if let crate::call_validation::ChatContent::SimpleText(ref text) = last.content {
            assert!(
                text.contains("## Skill Report: my-skill"),
                "Report header missing: {}",
                text
            );
            assert!(
                text.contains("Skill 'my-skill' executed successfully"),
                "Report preface missing: {}",
                text
            );
            assert!(
                text.contains("Did useful things."),
                "Report body missing: {}",
                text
            );
        } else {
            panic!("Expected SimpleText content in report message");
        }
        // pending is consumed
        assert!(session.pending_skill_deactivation.is_none());
    }

    #[test]
    fn test_skill_deactivation_cleanup_noop_when_no_pending() {
        let mut session = make_session();
        session.add_message(make_user_message("msg1"));
        session.add_message(make_user_message("msg2"));

        session.perform_skill_deactivation_cleanup();
        // Nothing changed
        assert_eq!(session.messages.len(), 2);
    }

    #[test]
    fn test_skill_deactivation_keeps_activation_tool_message() {
        let mut session = make_session();

        session.add_message(make_user_message("pre-skill"));
        let anchor = session.messages.len();

        let tool_message = ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText("Skill activated".to_string()),
            tool_call_id: "call_activate_skill".to_string(),
            tool_failed: Some(false),
            ..Default::default()
        };
        session.add_message(tool_message);

        session.add_message(ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "context_file".to_string(),
            content: ChatContent::SimpleText("Skill body".to_string()),
            ..Default::default()
        });
        session.add_message(make_user_message("skill run"));

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: anchor,
            skill_name: "tool-skill".to_string(),
            report: "Wrapped up".to_string(),
            activation_tool_call_id: Some("call_activate_skill".to_string()),
        });

        session.perform_skill_deactivation_cleanup();

        assert_eq!(
            session.messages.len(),
            3,
            "Expected pre-skill + tool + report"
        );
        assert_eq!(
            session.messages[1].role, "tool",
            "Activation tool message must remain"
        );
        assert_eq!(session.messages[1].tool_call_id, "call_activate_skill");
        assert_eq!(session.messages.last().unwrap().role, "plain_text");
    }

    #[test]
    fn test_skill_deactivation_skips_exact_activation_tool_call_id() {
        let mut session = make_session();

        session.add_message(make_user_message("pre-skill"));
        let anchor = session.messages.len();

        let unrelated_tool = ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText("Unrelated tool".to_string()),
            tool_call_id: "call_other_tool".to_string(),
            tool_failed: Some(false),
            ..Default::default()
        };
        session.add_message(unrelated_tool);

        let activation_tool = ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "tool".to_string(),
            content: ChatContent::SimpleText("Skill activated".to_string()),
            tool_call_id: "call_activate_skill".to_string(),
            tool_failed: Some(false),
            ..Default::default()
        };
        session.add_message(activation_tool);

        session.add_message(ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: "cd_instruction".to_string(),
            content: ChatContent::SimpleText("Skill body".to_string()),
            ..Default::default()
        });
        session.add_message(make_user_message("skill run"));

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: anchor,
            skill_name: "tool-skill".to_string(),
            report: "Wrapped up".to_string(),
            activation_tool_call_id: Some("call_activate_skill".to_string()),
        });

        session.perform_skill_deactivation_cleanup();

        assert_eq!(
            session.messages.len(),
            3,
            "Expected pre-skill + activation tool + report"
        );
        assert_eq!(session.messages[1].tool_call_id, "call_activate_skill");
    }

    #[test]
    fn test_skill_deactivation_without_anchor_still_records_report() {
        let mut session = make_session();
        session.add_message(make_user_message("pre-skill"));

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: session.messages.len(),
            skill_name: "no-anchor".to_string(),
            report: "All done".to_string(),
            activation_tool_call_id: None,
        });

        session.perform_skill_deactivation_cleanup();

        assert_eq!(session.messages.len(), 2, "Expected pre-skill + report");
        let last = session.messages.last().unwrap();
        assert_eq!(last.role, "plain_text");
        if let crate::call_validation::ChatContent::SimpleText(ref text) = last.content {
            assert!(text.contains("## Skill Report: no-anchor"));
            assert!(text.contains("All done"));
        }
    }

    #[test]
    fn test_skill_deactivation_cleanup_rejects_out_of_range_index() {
        let mut session = make_session();
        session.add_message(make_user_message("only message"));
        assert_eq!(session.messages.len(), 1);

        session.pending_skill_deactivation = Some(crate::chat::types::PendingSkillDeactivation {
            start_index: 99, // beyond messages.len()
            skill_name: "bad-skill".to_string(),
            report: "report".to_string(),
            activation_tool_call_id: None,
        });

        session.perform_skill_deactivation_cleanup();

        // No truncation, no report added — skipped with warning
        assert_eq!(
            session.messages.len(),
            1,
            "Messages must not be modified on bad index"
        );
        assert!(
            session.pending_skill_deactivation.is_none(),
            "pending must be consumed even on skip"
        );
    }

    #[test]
    fn test_new_with_trajectory_clears_active_skill() {
        let thread = ThreadParams {
            id: "t1".into(),
            active_skill: Some("leftover-skill".to_string()),
            ..Default::default()
        };
        let session = ChatSession::new_with_trajectory(
            "t1".into(),
            vec![],
            thread,
            "2024-01-01T00:00:00Z".into(),
            None,
            Vec::new(),
            None,
        );
        assert!(
            session.thread.active_skill.is_none(),
            "active_skill must be cleared on restore: compaction anchor is lost after restart"
        );
    }
