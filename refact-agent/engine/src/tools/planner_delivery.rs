pub use refact_tools::tools::planner_delivery::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use refact_core::chat_types::{DeliveryOutcome, PushMode};
#[cfg(test)]
use refact_runtime_api::SessionState;
#[cfg(test)]
use refact_tasks::types::BoardCard;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn push_mode_defaults_to_append_when_absent() {
        assert_eq!(parse_push_mode(&args(&[])).unwrap(), PushMode::Append);
    }

    #[test]
    fn push_mode_null_is_treated_as_absent() {
        let args = args(&[("push", Value::Null)]);
        assert_eq!(parse_push_mode(&args).unwrap(), PushMode::Append);
    }

    #[test]
    fn push_mode_parses_all_variants_explicitly() {
        for (raw, expected) in [
            ("preempt", PushMode::Preempt),
            ("append", PushMode::Append),
            ("when_idle", PushMode::WhenIdle),
        ] {
            let args = args(&[("push", json!(raw))]);
            assert_eq!(parse_push_mode(&args).unwrap(), expected);
            assert_eq!(push_mode_label(expected), raw);
        }
    }

    fn test_card(column: &str, agent_chat_id: Option<&str>) -> BoardCard {
        BoardCard {
            id: "T-1".to_string(),
            title: "Card".to_string(),
            column: column.to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: Some("agent-1".to_string()),
            agent_chat_id: agent_chat_id.map(str::to_string),
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: Utc::now().to_rfc3339(),
            started_at: None,
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

    #[test]
    fn stale_card_reason_reports_missing_column_and_rebound_chat() {
        let mut card = test_card("doing", Some("chat-1"));

        assert!(stale_card_reason(None, "T-1", "chat-1", true)
            .unwrap()
            .contains("no longer exists"));
        assert!(stale_card_reason(Some(&card), "T-1", "chat-1", true).is_none());

        card.column = "done".to_string();
        assert!(stale_card_reason(Some(&card), "T-1", "chat-1", true)
            .unwrap()
            .contains("now in column 'done'"));
        assert!(
            stale_card_reason(Some(&card), "T-1", "chat-1", false).is_none(),
            "column must be ignored when require_doing is false"
        );

        card.column = "doing".to_string();
        card.agent_chat_id = Some("chat-2".to_string());
        assert!(stale_card_reason(Some(&card), "T-1", "chat-1", true)
            .unwrap()
            .contains("agent_chat_id changed from 'chat-1' to 'chat-2'"));
    }

    #[test]
    fn duplicate_outcome_is_never_reported_as_reaching_the_agent() {
        let duplicate = DeliveryReport::Delivered {
            outcome: DeliveryOutcome::Duplicate,
            prior_state: Some(SessionState::Idle),
        };
        assert!(!duplicate.reached_agent());
        assert!(duplicate.short_status().starts_with("not delivered"));
        assert!(duplicate.describe("Steer").contains("NOT delivered"));

        for outcome in [DeliveryOutcome::Queued, DeliveryOutcome::Delivered] {
            let report = DeliveryReport::Delivered {
                outcome,
                prior_state: None,
            };
            assert!(report.reached_agent());
            assert!(!report.short_status().starts_with("not delivered"));
        }
    }

    #[test]
    fn non_delivered_reports_never_claim_success() {
        let reports = [
            DeliveryReport::NoLiveSession,
            DeliveryReport::Skipped {
                reason: "card T-1 is now in column 'done'".to_string(),
            },
            DeliveryReport::Failed {
                error: "queue unavailable".to_string(),
            },
        ];
        for report in reports {
            assert!(!report.reached_agent());
            assert!(report.short_status().starts_with("not delivered"));
            assert!(report.describe("Pause").contains("NOT delivered"));
        }
    }
}
