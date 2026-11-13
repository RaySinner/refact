pub use refact_tools::tools::tool_task_board::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use refact_tasks::types::{BoardCard, TaskBoard};
#[cfg(test)]
use crate::tools::tools_description::Tool;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_card_schema_includes_target_files() {
        let schema = ToolTaskBoardUpdateCard::new()
            .tool_description()
            .input_schema;
        assert!(schema["properties"].get("target_files").is_some());
        assert_eq!(
            schema["properties"]["target_files"]["type"],
            serde_json::json!("string")
        );
    }

    fn card(
        id: &str,
        title: &str,
        column: &str,
        priority: &str,
        depends_on: Vec<&str>,
    ) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: title.to_string(),
            column: column.to_string(),
            priority: priority.to_string(),
            depends_on: depends_on.into_iter().map(String::from).collect(),
            instructions: format!("Implement {} with enough details for a short brief.", title),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: "2026-05-16T00:00:00Z".to_string(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            target_files: vec![],
            scope_guard_mode: Default::default(),
            team_members: vec![],
        }
    }

    fn sample_board() -> TaskBoard {
        let mut doing = card("T-22", "auto-nudge", "doing", "P0", vec![]);
        doing.started_at = Some(Utc::now().to_rfc3339());
        let mut done = card("T-21", "done prerequisite", "done", "P1", vec![]);
        done.completed_at = Some(Utc::now().to_rfc3339());
        TaskBoard {
            rev: 7,
            cards: vec![
                doing,
                done,
                card("T-23", "Task Documents", "planned", "P0", vec![]),
                card("T-24", "Memory search", "planned", "P1", vec!["T-23"]),
                card("T-28", "Scoped injection", "planned", "P2", vec!["T-24"]),
                card("T-29", "Filtered card", "planned", "P2", vec!["T-21"]),
                card("T-30", "Failed card", "failed", "P1", vec![]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn ready_cards_renders_enriched_table() {
        let rendered = render_ready_cards(&sample_board());

        assert!(rendered.contains("# Ready Cards (2)"));
        assert!(rendered.contains("| Card | Title | Priority | Depends On | Brief |"));
        assert!(
            rendered.contains("| T-23 | Task Documents | P0 | (none) | Implement Task Documents")
        );
        assert!(rendered.contains("| T-29 | Filtered card | P2 | T-21 | Implement Filtered card"));
        assert!(rendered.contains("# Blocked (2)"));
        assert!(rendered.contains("| T-24 | Memory search | T-23 |"));
        assert!(rendered.contains("# In Progress (1)"));
        assert!(rendered.contains("T-22"));
        assert!(rendered.contains("# Completed\n1 cards (use board_get to list)"));
        assert!(rendered.contains("# Failed\nT-30"));
    }

    #[test]
    fn tree_mode_shows_dependency_structure() {
        let board = sample_board();
        let cards = board.cards.iter().collect::<Vec<_>>();
        let rendered = render_board_tree(&cards, BoardVerbosity::Brief);

        assert!(rendered.contains("T-23 (planned, P0) Task Documents"));
        assert!(rendered.contains("└─ T-24 (planned, P1) Memory search"));
        assert!(rendered.contains("└─ T-28 (planned, P2) Scoped injection"));
    }

    #[test]
    fn mermaid_mode_produces_valid_syntax() {
        let board = sample_board();
        let cards = board.cards.iter().collect::<Vec<_>>();
        let rendered = render_board_mermaid(&cards);

        assert!(rendered.starts_with("flowchart TD\n"));
        assert!(rendered.contains("card_T_23[\"T-23 (planned) Task Documents\"]:::planned"));
        assert!(rendered.contains("card_T_23 --> card_T_24"));
        assert!(rendered.contains("classDef planned"));
        assert!(rendered.contains("classDef doing"));
    }

    #[test]
    fn board_get_card_verbosity_filters_work() {
        let mut card = card("T-1", "Card", "done", "P0", vec![]);
        card.status_updates.push(refact_tasks::types::StatusUpdate {
            timestamp: "2026-05-16T00:00:00Z".to_string(),
            message: "updated".to_string(),
        });
        card.final_report = Some("final report".to_string());

        let brief = render_card_details(&card, BoardVerbosity::Brief).unwrap();
        assert!(brief.contains("id: T-1"));
        assert!(!brief.contains("status_updates"));
        assert!(!brief.contains("final_report"));

        let full = render_card_details(&card, BoardVerbosity::Full).unwrap();
        assert!(full.contains("status_updates"));
        assert!(full.contains("final_report"));

        let updates = render_card_details(&card, BoardVerbosity::UpdatesOnly).unwrap();
        assert!(updates.contains("status_updates"));
        assert!(updates.contains("updated"));
        assert!(!updates.contains("final_report"));
        assert!(!updates.contains("instructions"));
    }

    #[test]
    fn column_and_priority_filters_work() {
        let board = sample_board();
        let args = HashMap::from([(
            "filter".to_string(),
            serde_json::json!({"column": ["planned"], "priority": ["P2"]}),
        )]);
        let filter = parse_filter(&args);
        let cards = board
            .cards
            .iter()
            .filter(|card| card_matches_filter(card, &filter))
            .collect::<Vec<_>>();
        let rendered = render_board_summary(&board, &cards, BoardVerbosity::Minimal).unwrap();

        assert!(rendered.contains("id: T-28"));
        assert!(rendered.contains("id: T-29"));
        assert!(!rendered.contains("id: T-23"));
        assert!(!rendered.contains("id: T-22"));
        assert!(!rendered.contains("title:"));
    }

    #[test]
    fn board_get_schema_includes_new_params() {
        let schema = ToolTaskBoardGet::new().tool_description().input_schema;
        assert!(schema["properties"].get("filter").is_some());
        assert!(schema["properties"].get("mode").is_some());
        assert!(schema["properties"].get("verbosity").is_some());
    }
}
