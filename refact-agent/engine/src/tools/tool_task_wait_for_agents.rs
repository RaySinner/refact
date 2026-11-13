pub use refact_tools::tools::tool_task_wait_for_agents::*;


#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use refact_chat_api::ChatSession;
#[cfg(test)]
use crate::tools::tool_task_check_agents::AgentStatus;
#[cfg(test)]
use crate::tools::tool_task_check_agents::parse_agent_status_query;
#[cfg(test)]
use crate::tools::tool_task_check_agents::waitable_agent_statuses;


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;

    fn make_status(card_id: &str, column: &str) -> AgentStatus {
        AgentStatus {
            card_id: card_id.to_string(),
            variant_key: None,
            card_title: format!("{} title", card_id),
            agent_chat_id: format!("agent-{}", card_id),
            column: column.to_string(),
            priority: "P1".to_string(),
            session_state: Some(refact_runtime_api::SessionState::Generating),
            last_status_update: None,
            last_activity_at: None,
            final_report: (column == "done").then(|| "done".to_string()),
            last_tool_name: None,
            change_seq: 0,
            room_members: Vec::new(),
        }
    }

    fn offline_status(card_id: &str) -> AgentStatus {
        let mut status = make_status(card_id, "doing");
        status.session_state = None;
        status
    }

    #[test]
    fn wait_agents_rejects_wake_up_secs_below_30() {
        let mut args = HashMap::new();
        args.insert("wake_up_after_secs".to_string(), json!(29));
        let err = parse_wake_up_after_secs(&args).unwrap_err();
        assert!(err.contains("at least 30"), "got: {}", err);
    }

    #[test]
    fn wait_agents_rejects_wake_up_secs_above_1800() {
        let mut args = HashMap::new();
        args.insert("wake_up_after_secs".to_string(), json!(1801));
        let err = parse_wake_up_after_secs(&args).unwrap_err();
        assert!(err.contains("at most 1800"), "got: {}", err);
    }

    #[tokio::test]
    async fn wait_agents_records_wake_up_at_on_session_when_argument_present() {
        use crate::app_state::AppState;

        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;

        let chat_id = "planner-wake-test".to_string();
        let session = ChatSession::new(chat_id.clone());
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        app.chat
            .sessions
            .write()
            .await
            .insert(chat_id.clone(), session_arc.clone());

        let secs = 120u64;
        let before = Utc::now();
        let args: HashMap<String, Value> = [("wake_up_after_secs".to_string(), json!(secs))]
            .into_iter()
            .collect();

        let wake_up_secs = parse_wake_up_after_secs(&args).unwrap();
        assert_eq!(wake_up_secs, Some(secs));

        let wake_at = Utc::now() + chrono::Duration::seconds(secs as i64);
        {
            let sessions = app.chat.sessions.read().await;
            if let Some(sa) = sessions.get(&chat_id) {
                let mut s = sa.lock().await;
                s.wake_up_at = Some(wake_at);
                s.mark_persisted_runtime_changed();
            }
        }

        let session = session_arc.lock().await;
        let stored = session.wake_up_at.unwrap();
        let expected = before + chrono::Duration::seconds(secs as i64);
        let diff = (stored - expected).num_seconds().abs();
        assert!(diff <= 2, "wake_up_at set within ±2s, diff={}", diff);
        assert!(session.trajectory_dirty);
    }

    #[test]
    fn wait_agents_does_not_set_wake_up_at_when_argument_absent() {
        let args: HashMap<String, Value> = HashMap::new();
        let result = parse_wake_up_after_secs(&args).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn wait_agents_replaces_timed_wait_with_indefinite_wait() {
        let mut session = ChatSession::new("planner-timed-to-indefinite".to_string());
        replace_session_wake_up_at(&mut session, Some(120));
        assert!(session.wake_up_at.is_some());

        replace_session_wake_up_at(&mut session, None);

        assert_eq!(session.wake_up_at, None);
    }

    #[test]
    fn wait_agents_replaces_indefinite_wait_with_timed_wait() {
        let mut session = ChatSession::new("planner-indefinite-to-timed".to_string());
        replace_session_wake_up_at(&mut session, None);
        assert_eq!(session.wake_up_at, None);

        let before = Utc::now();
        replace_session_wake_up_at(&mut session, Some(120));

        let stored = session
            .wake_up_at
            .expect("timed wait should set wake_up_at");
        let expected = before + chrono::Duration::seconds(120);
        assert!((stored - expected).num_seconds().abs() <= 1);
    }

    #[test]
    fn wait_agents_populates_waiting_for_card_ids_from_filter() {
        let statuses = vec![
            make_status("T-1", "doing"),
            make_status("T-2", "doing"),
            make_status("T-3", "doing"),
        ];
        let args = HashMap::from([("card_ids".to_string(), json!(["T-1", "T-2"]))]);
        let query = parse_agent_status_query(&args).unwrap();
        let waitable = waitable_agent_statuses(&statuses, &query, false);
        let ids = resolve_waiting_card_ids(&waitable);
        assert_eq!(ids, vec!["T-1", "T-2"]);
    }

    #[test]
    fn wait_agents_populates_waiting_for_card_ids_from_all_active_when_no_filter() {
        let statuses = vec![
            make_status("T-1", "doing"),
            make_status("T-2", "doing"),
            make_status("T-3", "done"),
        ];
        let query = parse_agent_status_query(&HashMap::new()).unwrap();
        let waitable = waitable_agent_statuses(&statuses, &query, false);
        let ids = resolve_waiting_card_ids(&waitable);
        assert_eq!(ids, vec!["T-1", "T-2"]);
    }

    #[test]
    fn wait_agents_does_not_wait_for_offline_doing_cards() {
        let statuses = vec![offline_status("T-1")];
        let query = parse_agent_status_query(&HashMap::new()).unwrap();
        let waitable = waitable_agent_statuses(&statuses, &query, false);
        let ids = resolve_waiting_card_ids(&waitable);
        assert!(ids.is_empty());
    }

    #[test]
    fn wait_agents_does_not_wait_when_card_filter_matches_no_live_agent() {
        let statuses = vec![make_status("T-1", "doing")];
        let args = HashMap::from([("card_ids".to_string(), json!(["T-404"]))]);
        let query = parse_agent_status_query(&args).unwrap();
        let waitable = waitable_agent_statuses(&statuses, &query, false);
        let ids = resolve_waiting_card_ids(&waitable);
        assert!(ids.is_empty());
    }

    #[test]
    fn wait_agents_default_query_can_wait_for_paused_live_agent() {
        let mut status = make_status("T-1", "doing");
        status.session_state = Some(refact_runtime_api::SessionState::Paused);
        let statuses = vec![status];
        let query = parse_agent_status_query(&HashMap::new()).unwrap();
        let waitable = waitable_agent_statuses(&statuses, &query, false);
        let ids = resolve_waiting_card_ids(&waitable);
        assert_eq!(ids, vec!["T-1"]);
    }

    #[test]
    fn wait_agents_waits_for_starting_agents() {
        let mut status = make_status("T-1", "doing");
        status.session_state = Some(refact_runtime_api::SessionState::Starting);
        let statuses = vec![status];
        let query = parse_agent_status_query(&HashMap::new()).unwrap();
        let waitable = waitable_agent_statuses(&statuses, &query, false);
        let ids = resolve_waiting_card_ids(&waitable);
        assert_eq!(ids, vec!["T-1"]);
    }

    #[test]
    fn wait_agents_caps_waiting_for_card_ids_at_50() {
        let statuses: Vec<AgentStatus> = (1..=60)
            .map(|i| make_status(&format!("T-{}", i), "doing"))
            .collect();
        let waitable: Vec<&AgentStatus> = statuses.iter().collect();
        let ids = resolve_waiting_card_ids(&waitable);
        assert_eq!(ids.len(), 50);
    }

    #[test]
    fn wait_agents_natural_sorts_card_ids() {
        let statuses = vec![
            make_status("T-10", "doing"),
            make_status("T-2", "doing"),
            make_status("T-1", "doing"),
        ];
        let waitable: Vec<&AgentStatus> = statuses.iter().collect();
        let ids = resolve_waiting_card_ids(&waitable);
        assert_eq!(ids, vec!["T-1", "T-2", "T-10"]);
    }

    #[test]
    fn wait_agents_uses_variant_status_ids() {
        let mut variant = make_status("T-1", "doing");
        variant.variant_key = Some("a".to_string());
        let statuses = vec![variant];
        let waitable: Vec<&AgentStatus> = statuses.iter().collect();

        let ids = resolve_waiting_card_ids(&waitable);

        assert_eq!(ids, vec!["T-1/a"]);
    }
}
