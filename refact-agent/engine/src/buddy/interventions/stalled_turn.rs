pub use refact_runtime_api::buddy_interventions::stalled_turn::*;

#[cfg(test)]
use chrono::{DateTime, Duration, Utc};
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use uuid::Uuid;
#[cfg(test)]
use refact_buddy_core::settings::BuddySettings;
#[cfg(test)]
use refact_buddy_core::stalled_turn::{
    classify_stalled_turn, StalledTurnSignal, StalledTurnVerdict,
};
#[cfg(test)]
use refact_chat_api::types::{GoalStatus, PushMode, SessionState};
#[cfg(test)]
use crate::chat::internal_roles::{event, EventSubkind};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_validation::{ChatContent, ChatMessage};
    use crate::chat::types::{ChatSession, GoalSnapshot};
    use refact_buddy_core::stalled_turn::NotStalledReason;

    fn visible(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            role: role.to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    fn session_with_goal() -> ChatSession {
        let mut session = ChatSession::new("chat-1".to_string());
        session.goal = Some(GoalSnapshot {
            active: true,
            status: GoalStatus::Active,
            ..Default::default()
        });
        session
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn budget() -> StalledTurnBudget {
        StalledTurnBudget {
            min_interval_secs: 300,
            max_per_hour: 6,
        }
    }

    #[test]
    fn tool_free_turn_with_active_goal_classifies_as_stalled() {
        let mut session = session_with_goal();
        session
            .messages
            .push(visible("assistant", "I think that is everything for now."));
        let ctx = collect_turn_context(&session, 0);
        assert!(matches!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::Stalled(_)
        ));
    }

    #[test]
    fn turn_with_tool_calls_is_never_a_stall() {
        let mut session = session_with_goal();
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("working".to_string()),
            tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                id: "call-1".to_string(),
                function: crate::call_validation::ChatToolFunction {
                    name: "search_pattern".to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                index: None,
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        });
        let ctx = collect_turn_context(&session, 1);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::HasToolCalls)
        );
    }

    #[test]
    fn reasoning_only_turn_is_empty_message() {
        let mut session = session_with_goal();
        session.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("   ".to_string()),
            reasoning_content: Some("I pondered deeply".to_string()),
            ..Default::default()
        });
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::EmptyMessage)
        );
    }

    #[test]
    fn completed_goal_is_not_active() {
        let mut session = session_with_goal();
        session.goal.as_mut().unwrap().status = GoalStatus::Completed;
        session.messages.push(visible("assistant", "done"));
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::NoActiveObjective)
        );
    }

    #[test]
    fn tool_forbidden_mode_is_not_a_stall() {
        let mut session = session_with_goal();
        session.thread.mode = "chat".to_string();
        session
            .messages
            .push(visible("assistant", "Just a plain answer."));
        let ctx = collect_turn_context(&session, 0);
        assert_eq!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::NotStalled(NotStalledReason::ToolsForbiddenMode)
        );
    }

    #[test]
    fn intervention_is_off_when_the_setting_is_off() {
        let mut settings = BuddySettings::default();
        assert!(intervention_enabled(&settings));
        settings.stalled_turn_intervention = false;
        assert!(!intervention_enabled(&settings));
    }

    #[test]
    fn budget_defaults_are_five_minutes_and_six_per_hour() {
        let from_settings = StalledTurnBudget::from_settings(&BuddySettings::default());
        assert_eq!(from_settings.min_interval_secs, 300);
        assert_eq!(from_settings.max_per_hour, 6);
    }

    #[test]
    fn first_nudge_is_always_allowed() {
        assert!(stalled_turn_allowed(&[], now(), budget()));
    }

    #[test]
    fn second_nudge_inside_the_interval_is_suppressed() {
        let history = vec![now() - Duration::seconds(120)];
        assert!(!stalled_turn_allowed(&history, now(), budget()));

        let later = now() + Duration::seconds(181);
        assert!(stalled_turn_allowed(&history, later, budget()));
    }

    #[test]
    fn hourly_cap_stops_a_chatty_buddy() {
        let history: Vec<DateTime<Utc>> = (0..6)
            .map(|index| now() - Duration::seconds(400 + index * 400))
            .collect();
        assert!(!stalled_turn_allowed(&history, now(), budget()));

        // Entries older than the rolling hour no longer count.
        let stale: Vec<DateTime<Utc>> = (0..6)
            .map(|index| now() - Duration::hours(2) - Duration::seconds(index * 400))
            .collect();
        assert!(stalled_turn_allowed(&stale, now(), budget()));
    }

    #[test]
    fn zero_budget_means_buddy_never_speaks() {
        let zero = StalledTurnBudget {
            min_interval_secs: 300,
            max_per_hour: 0,
        };
        assert!(!stalled_turn_allowed(&[], now(), zero));
    }

    #[test]
    fn user_reply_after_the_stalled_turn_suppresses_the_nudge() {
        let messages = vec![
            visible("user", "please fix the tests"),
            visible("assistant", "All good, I think we are done."),
        ];
        assert!(!user_spoke_after_last_assistant(&messages));

        let mut with_user = messages.clone();
        with_user.push(visible("user", "no, keep going"));
        assert!(user_spoke_after_last_assistant(&with_user));
    }

    #[test]
    fn missing_assistant_message_counts_as_moved_on() {
        assert!(user_spoke_after_last_assistant(&[visible("user", "hi")]));
        assert!(user_spoke_after_last_assistant(&[]));
    }

    #[test]
    fn agent_must_be_between_turns_to_be_nudged() {
        let mut session = session_with_goal();
        session.set_runtime_state(SessionState::Idle, None);
        assert!(agent_ready_for_nudge(&session));

        session.set_runtime_state(SessionState::Generating, None);
        assert!(!agent_ready_for_nudge(&session));

        session.set_runtime_state(SessionState::Idle, None);
        session.turn_depth = 1;
        assert!(!agent_ready_for_nudge(&session));

        session.turn_depth = 0;
        session.draft_message = Some(visible("assistant", "half an answer"));
        assert!(!agent_ready_for_nudge(&session));

        session.draft_message = None;
        session.closed = true;
        assert!(!agent_ready_for_nudge(&session));
    }

    #[test]
    fn plain_reply_becomes_a_nudge() {
        assert_eq!(
            parse_buddy_reply("  Call `update_textdoc` and finish the card.  "),
            Some(BuddyReply::Nudge(
                "Call `update_textdoc` and finish the card.".to_string()
            ))
        );
    }

    #[test]
    fn escalate_marker_is_stripped_from_the_question() {
        assert_eq!(
            parse_buddy_reply("ESCALATE_USER: Which API key should the agent use?"),
            Some(BuddyReply::EscalateUser(
                "Which API key should the agent use?".to_string()
            ))
        );
        assert_eq!(
            parse_buddy_reply("ESCALATE_USER:   "),
            None,
            "an empty escalation is not a question"
        );
    }

    #[test]
    fn empty_reply_is_dropped_and_long_replies_are_capped() {
        assert_eq!(parse_buddy_reply("   \n "), None);

        let long = "x".repeat(MAX_REPLY_CHARS + 200);
        match parse_buddy_reply(&long) {
            Some(BuddyReply::Nudge(text)) => assert_eq!(text.chars().count(), MAX_REPLY_CHARS),
            other => panic!("expected a capped nudge, got {other:?}"),
        }
    }

    #[test]
    fn prompt_carries_the_turn_the_plan_the_goal_and_the_card() {
        let prompt = StalledTurnPrompt {
            chat_id: "chat-1".to_string(),
            mode: "task_agent".to_string(),
            reason_context: "card_doing".to_string(),
            snippet: "I think we are done here, right?".to_string(),
            plan: Some("1. Implement\n2. Test".to_string()),
            goal: Some("Ship the sandbox layer".to_string()),
            task_id: Some("task-1".to_string()),
            card_id: Some("S-3".to_string()),
            card_title: Some("Buddy intervention".to_string()),
            card_target_files: vec!["src/buddy/interventions/stalled_turn.rs".to_string()],
        };

        let text = build_buddy_user_prompt(&prompt);
        assert!(text.contains("chat-1"), "{text}");
        assert!(text.contains("task_agent"), "{text}");
        assert!(text.contains("doing column"), "{text}");
        assert!(text.contains("I think we are done here, right?"), "{text}");
        assert!(text.contains("1. Implement"), "{text}");
        assert!(text.contains("Ship the sandbox layer"), "{text}");
        assert!(text.contains("S-3 - Buddy intervention"), "{text}");
        assert!(text.contains("src/buddy/interventions/stalled_turn.rs"), "{text}");
        assert!(text.contains("Write the nudge now."), "{text}");
    }

    #[test]
    fn prompt_skips_absent_sections() {
        let prompt = StalledTurnPrompt {
            chat_id: "chat-1".to_string(),
            reason_context: "goal_active".to_string(),
            snippet: "nothing to do".to_string(),
            ..Default::default()
        };
        let text = build_buddy_user_prompt(&prompt);
        assert!(!text.contains("Current plan"), "{text}");
        assert!(!text.contains("Current goal"), "{text}");
        assert!(!text.contains("Card:"), "{text}");
        assert!(text.contains("active goal"), "{text}");
    }

    #[test]
    fn nudge_is_a_runner_notice_that_wakes_the_agent() {
        let delivery = stalled_turn_delivery(
            "chat-1",
            4,
            &BuddyReply::Nudge("Go finish the card.".to_string()),
        );

        assert_eq!(delivery.id, "stalled-turn:chat-1:4");
        assert_eq!(delivery.push, PushMode::Append);
        assert_eq!(delivery.source, STALLED_TURN_SOURCE);
        assert!(delivery.wake, "a stalled agent must actually resume");
        assert_eq!(delivery.messages.len(), 1);
        let message = &delivery.messages[0];
        assert_eq!(message.role, "event", "the user must not look like the author");
        assert_eq!(message.content.content_text_only(), "Go finish the card.");
        assert_eq!(message.extra["event"]["subkind"], json!("system_notice"));
        assert_eq!(message.extra["event"]["source"], json!(STALLED_TURN_SOURCE));
        assert_eq!(
            message.extra["event"]["payload"]["kind"],
            json!("stalled_turn_nudge")
        );
        assert_eq!(
            message.extra["event"]["payload"]["turn_index"],
            json!(4)
        );
    }

    #[test]
    fn escalation_goes_to_the_human_without_starting_a_turn() {
        let delivery = stalled_turn_delivery(
            "chat-1",
            4,
            &BuddyReply::EscalateUser("Which API key should I use?".to_string()),
        );

        assert_eq!(delivery.id, "stalled-turn-escalation:chat-1:4");
        assert_eq!(delivery.push, PushMode::WhenIdle);
        assert!(!delivery.wake, "the agent must not be restarted by an escalation");
        assert_eq!(delivery.messages[0].role, "event");
        assert_eq!(
            delivery.messages[0].extra["event"]["payload"]["kind"],
            json!("stalled_turn_escalation")
        );
    }

    #[test]
    fn consecutive_stalls_in_one_chat_get_distinct_delivery_ids() {
        let reply = BuddyReply::Nudge("keep going".to_string());
        assert_ne!(
            stalled_turn_delivery("chat-1", 3, &reply).id,
            stalled_turn_delivery("chat-1", 4, &reply).id
        );
        assert_ne!(
            stalled_turn_delivery("chat-1", 3, &reply).id,
            stalled_turn_delivery("chat-2", 3, &reply).id
        );
    }

    #[test]
    fn raw_buddy_reply_becomes_the_right_kind_of_delivery() {
        let nudge = intervention_delivery("chat-1", 2, "Stop idling and call `update_textdoc`.")
            .expect("a plain reply is a nudge");
        assert_eq!(nudge.push, PushMode::Append);
        assert!(nudge.wake);
        assert_eq!(
            nudge.messages[0].content.content_text_only(),
            "Stop idling and call `update_textdoc`."
        );

        let escalation =
            intervention_delivery("chat-1", 2, "ESCALATE_USER: Which API key is it?")
                .expect("an escalation marker is a question");
        assert_eq!(escalation.push, PushMode::WhenIdle);
        assert!(!escalation.wake);
        assert_eq!(
            escalation.messages[0].content.content_text_only(),
            "Which API key is it?"
        );

        assert!(intervention_delivery("chat-1", 2, "   ").is_none());
    }

    #[test]
    fn prompt_context_reads_the_session_not_the_fact_payload() {
        let mut session = session_with_goal();
        session.goal.as_mut().unwrap().content = "Finish the card".to_string();
        session.thread.mode = "task_agent".to_string();
        session.thread.task_meta = Some(refact_chat_api::TaskMeta {
            task_id: "task-1".to_string(),
            role: "agents".to_string(),
            agent_id: Some("agent-1".to_string()),
            card_id: Some("S-3".to_string()),
            planner_chat_id: None,
        });
        let signal = StalledTurnSignal {
            chat_id: "chat-1".to_string(),
            reason_context: "card_doing",
        };

        let prompt = collect_prompt_context(&session, &signal, "all done");

        assert_eq!(prompt.chat_id, "chat-1");
        assert_eq!(prompt.mode, "task_agent");
        assert_eq!(prompt.snippet, "all done");
        assert_eq!(prompt.goal.as_deref(), Some("Finish the card"));
        assert_eq!(prompt.task_id.as_deref(), Some("task-1"));
        assert_eq!(prompt.card_id.as_deref(), Some("S-3"));
        assert_eq!(prompt.card_title, None, "title comes from the board, not the thread");
    }

    fn assistant_with_tool_call(name: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText(String::new()),
            tool_calls: Some(vec![crate::call_validation::ChatToolCall {
                id: "call-1".to_string(),
                function: crate::call_validation::ChatToolFunction {
                    name: name.to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                index: None,
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]),
            ..Default::default()
        }
    }

    fn stalled_nudge(text: &str) -> ChatMessage {
        event(
            EventSubkind::SystemNotice,
            STALLED_TURN_SOURCE,
            json!({"kind": "stalled_turn_nudge"}),
            text,
        )
    }

    #[test]
    fn plan_without_checkboxes_is_treated_as_unfinished() {
        assert!(plan_has_unfinished_items("1. Implement\n2. Test"));
        assert!(plan_has_unfinished_items("- implement the thing"));
    }

    #[test]
    fn empty_plan_has_nothing_pending() {
        assert!(!plan_has_unfinished_items(""));
        assert!(!plan_has_unfinished_items("   \n  "));
    }

    #[test]
    fn fully_checked_plan_has_nothing_pending() {
        assert!(!plan_has_unfinished_items(
            "- [x] wire the hook\n- [X] write tests"
        ));
    }

    #[test]
    fn partly_checked_plan_is_unfinished() {
        assert!(plan_has_unfinished_items(
            "- [x] wire the hook\n- [ ] write tests"
        ));
    }

    #[test]
    fn mixed_bullet_markers_all_count_as_checkboxes() {
        assert!(!plan_has_unfinished_items(
            "* [x] one\n+ [x] two\n1. [x] three"
        ));
        assert!(plan_has_unfinished_items(
            "* [x] one\n+ [ ] two\n1. [x] three"
        ));
    }

    #[test]
    fn no_progress_is_false_without_a_previous_nudge() {
        let messages = vec![
            visible("user", "go"),
            visible("assistant", "I think we are done."),
        ];
        assert!(!no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn tool_call_after_the_nudge_is_progress() {
        let messages = vec![
            stalled_nudge("keep going"),
            assistant_with_tool_call("update_textdoc"),
        ];
        assert!(!no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn plain_answer_after_the_nudge_is_no_progress() {
        let messages = vec![
            stalled_nudge("keep going"),
            visible("assistant", "I think we are done."),
        ];
        assert!(no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn user_message_after_the_nudge_is_no_progress() {
        let messages = vec![
            stalled_nudge("keep going"),
            visible("user", "actually, stop"),
        ];
        assert!(no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn tool_call_before_the_nudge_does_not_count_as_progress() {
        let messages = vec![
            assistant_with_tool_call("search_pattern"),
            stalled_nudge("keep going"),
            visible("assistant", "done"),
        ];
        assert!(no_progress_since_last_nudge(&messages));
    }

    #[test]
    fn trailing_event_message_does_not_hide_the_assistant_reply() {
        let mut session = session_with_goal();
        session.messages.push(visible("assistant", "готово"));
        session.messages.push(event(
            EventSubkind::ProcessCompleted,
            "process",
            json!({}),
            "done",
        ));

        let ctx = collect_turn_context(&session, 0);
        assert!(ctx.has_content);
        assert_eq!(last_assistant_text(&session), "готово");
        assert!(matches!(
            classify_stalled_turn(&ctx),
            StalledTurnVerdict::Stalled(_)
        ));
    }

    #[test]
    fn finishing_tool_call_in_the_turn_means_the_agent_is_done() {
        let messages = vec![
            visible("user", "finish the card"),
            assistant_with_tool_call(AGENT_FINISH_TOOL),
            visible("assistant", "All done."),
        ];
        assert!(agent_already_finished(&messages));
    }

    #[test]
    fn ordinary_tool_calls_do_not_mean_the_agent_is_done() {
        let messages = vec![
            visible("user", "keep going"),
            assistant_with_tool_call("update_textdoc"),
            visible("assistant", "Still working."),
        ];
        assert!(!agent_already_finished(&messages));
        assert!(!agent_already_finished(&[]));
    }

    #[test]
    fn only_a_doing_card_keeps_the_intervention_alive() {
        assert!(card_work_is_active(Some(CARD_DOING_COLUMN)));
        assert!(!card_work_is_active(Some("done")));
        assert!(!card_work_is_active(Some("failed")));
        assert!(
            card_work_is_active(None),
            "an unreadable board must not silence the nudge"
        );
    }

    #[test]
    fn silent_tail_nudge_leaves_nothing_for_the_hook_to_do() {
        assert!(!stalled_turn_hook_applies(true, true));
        assert!(stalled_turn_hook_applies(false, true));
        assert!(!stalled_turn_hook_applies(false, false));
    }

    #[tokio::test]
    async fn released_slot_does_not_block_the_next_nudge() {
        let chat_id = format!("chat-release-{}", Uuid::new_v4());
        let at = Utc::now();

        assert!(reserve_stalled_turn_slot(&chat_id, at, budget()).await);
        assert!(
            !reserve_stalled_turn_slot(&chat_id, at, budget()).await,
            "the reserved slot must hold the cooldown"
        );

        release_stalled_turn_slot(&chat_id, at).await;
        assert!(
            reserve_stalled_turn_slot(&chat_id, at + Duration::seconds(1), budget()).await,
            "a failed intervention must not burn the slot"
        );
    }
}
