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
    use std::sync::Arc;
    use refact_chat_api::TaskMeta;
    use refact_runtime_api::AtCommandsContext;
    use refact_tasks::events::{TaskEvent, TaskEventEnvelope};
    use tokio::sync::broadcast::error::TryRecvError;
    use tokio::sync::Mutex as AMutex;

    async fn make_board_context() -> (
        tempfile::TempDir,
        Arc<crate::global_context::GlobalContext>,
        Arc<AMutex<AtCommandsContext>>,
        String,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let meta = refact_runtime_api::task_storage::create_task(gcx.clone(), "Atomic board")
            .await
            .unwrap();
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx.clone()).await,
                4096,
                20,
                false,
                vec![],
                "planner-chat".to_string(),
                None,
                "model".to_string(),
                Some(TaskMeta {
                    task_id: meta.id.clone(),
                    role: "planner".to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: None,
                }),
                None,
            )
            .await,
        ));
        (temp, gcx, ccx, meta.id)
    }

    async fn run_board_tool<T: Tool>(
        tool: &mut T,
        ccx: &Arc<AMutex<AtCommandsContext>>,
        args: HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        let tool_call_id = "board-test".to_string();
        tool.tool_execute(ccx.clone(), &tool_call_id, &args)
            .await
            .map(|_| ())
    }

    fn collect_board_events(
        rx: &mut tokio::sync::broadcast::Receiver<TaskEventEnvelope>,
    ) -> Vec<u64> {
        let mut events = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(envelope) => {
                    if let TaskEvent::BoardChanged { rev, .. } = envelope.event {
                        events.push(rev);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(skipped)) => {
                    panic!("task event receiver lagged, skipped {skipped} events")
                }
            }
        }
        events
    }

    async fn load_board(
        ccx: &Arc<AMutex<AtCommandsContext>>,
        task_id: &str,
    ) -> TaskBoard {
        let ctx = ccx.lock().await.app.chat_ctx();
        refact_runtime_api::task_storage::load_board_for_ctx(&ctx, task_id)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn board_mutations_publish_one_monotonic_revision_each() {
        let (_temp, gcx, ccx, task_id) = make_board_context().await;
        let mut rx = gcx.task_events_tx.as_ref().unwrap().subscribe();

        let mut create = ToolTaskBoardCreateCard::new();
        run_board_tool(
            &mut create,
            &ccx,
            HashMap::from([
                ("card_id".to_string(), serde_json::json!("T-1")),
                ("title".to_string(), serde_json::json!("First card")),
            ]),
        )
        .await
        .unwrap();

        let mut update = ToolTaskBoardUpdateCard::new();
        run_board_tool(
            &mut update,
            &ccx,
            HashMap::from([
                ("card_id".to_string(), serde_json::json!("T-1")),
                ("title".to_string(), serde_json::json!("Renamed card")),
            ]),
        )
        .await
        .unwrap();

        let mut move_card = ToolTaskBoardMoveCard::new();
        run_board_tool(
            &mut move_card,
            &ccx,
            HashMap::from([
                ("card_id".to_string(), serde_json::json!("T-1")),
                ("column".to_string(), serde_json::json!("doing")),
            ]),
        )
        .await
        .unwrap();

        let mut delete = ToolTaskBoardDeleteCard::new();
        run_board_tool(
            &mut delete,
            &ccx,
            HashMap::from([("card_id".to_string(), serde_json::json!("T-1"))]),
        )
        .await
        .unwrap();

        let board = load_board(&ccx, &task_id).await;
        assert_eq!(board.rev, 4);
        assert!(board.cards.is_empty());

        let events = collect_board_events(&mut rx);
        assert_eq!(events.len(), 4);
        for (index, rev) in events.iter().enumerate() {
            assert_eq!(*rev, index as u64 + 1);
        }
    }

    #[tokio::test]
    async fn failed_board_mutations_keep_revision_and_emit_nothing() {
        let (_temp, gcx, ccx, task_id) = make_board_context().await;
        let mut rx = gcx.task_events_tx.as_ref().unwrap().subscribe();

        let mut create = ToolTaskBoardCreateCard::new();
        run_board_tool(
            &mut create,
            &ccx,
            HashMap::from([
                ("card_id".to_string(), serde_json::json!("T-1")),
                ("title".to_string(), serde_json::json!("First card")),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(collect_board_events(&mut rx).len(), 1);

        let duplicate = run_board_tool(
            &mut create,
            &ccx,
            HashMap::from([
                ("card_id".to_string(), serde_json::json!("T-1")),
                ("title".to_string(), serde_json::json!("Duplicate card")),
            ]),
        )
        .await
        .unwrap_err();
        assert!(duplicate.contains("already exists"), "{duplicate}");

        let mut update = ToolTaskBoardUpdateCard::new();
        let missing = run_board_tool(
            &mut update,
            &ccx,
            HashMap::from([("card_id".to_string(), serde_json::json!("T-404"))]),
        )
        .await
        .unwrap_err();
        assert!(missing.contains("not found"), "{missing}");

        let invalid_roster = run_board_tool(
            &mut update,
            &ccx,
            HashMap::from([
                ("card_id".to_string(), serde_json::json!("T-1")),
                (
                    "team_members".to_string(),
                    serde_json::json!([
                        {"role": "coder", "agent_chat_id": "agent-same"},
                        {"role": "reviewer", "agent_chat_id": "agent-same"},
                    ]),
                ),
            ]),
        )
        .await
        .unwrap_err();
        assert!(
            invalid_roster.contains("duplicate team member"),
            "{invalid_roster}"
        );

        let mut move_card = ToolTaskBoardMoveCard::new();
        let missing = run_board_tool(
            &mut move_card,
            &ccx,
            HashMap::from([
                ("card_id".to_string(), serde_json::json!("T-404")),
                ("column".to_string(), serde_json::json!("doing")),
            ]),
        )
        .await
        .unwrap_err();
        assert!(missing.contains("not found"), "{missing}");

        let mut delete = ToolTaskBoardDeleteCard::new();
        let missing = run_board_tool(
            &mut delete,
            &ccx,
            HashMap::from([("card_id".to_string(), serde_json::json!("T-404"))]),
        )
        .await
        .unwrap_err();
        assert!(missing.contains("not found"), "{missing}");

        let board = load_board(&ccx, &task_id).await;
        assert_eq!(board.rev, 1);
        assert_eq!(board.cards.len(), 1);
        assert!(collect_board_events(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn concurrent_board_mutations_keep_persisted_board_and_revisions_consistent() {
        const CARD_COUNT: usize = 8;

        let (_temp, gcx, ccx, task_id) = make_board_context().await;
        let mut rx = gcx.task_events_tx.as_ref().unwrap().subscribe();

        let mut handles = Vec::new();
        for index in 0..CARD_COUNT {
            let ccx = ccx.clone();
            handles.push(tokio::spawn(async move {
                let mut create = ToolTaskBoardCreateCard::new();
                run_board_tool(
                    &mut create,
                    &ccx,
                    HashMap::from([
                        (
                            "card_id".to_string(),
                            serde_json::json!(format!("T-{}", index + 1)),
                        ),
                        (
                            "title".to_string(),
                            serde_json::json!(format!("Card {}", index + 1)),
                        ),
                    ]),
                )
                .await
            }));
        }
        for handle in handles {
            let _ = handle.await.unwrap();
        }

        let board = load_board(&ccx, &task_id).await;
        assert_eq!(board.rev, CARD_COUNT as u64);
        assert_eq!(board.cards.len(), CARD_COUNT);

        let events = collect_board_events(&mut rx);
        assert_eq!(events.len(), CARD_COUNT);
        for (index, rev) in events.iter().enumerate() {
            assert_eq!(*rev, index as u64 + 1);
        }
    }

    #[tokio::test]
    async fn large_card_payload_is_not_serialized_into_board_changed_event() {
        let (_temp, gcx, ccx, task_id) = make_board_context().await;
        let mut rx = gcx.task_events_tx.as_ref().unwrap().subscribe();
        let instructions = "x".repeat(5 * 1024 * 1024);

        let mut create = ToolTaskBoardCreateCard::new();
        run_board_tool(
            &mut create,
            &ccx,
            HashMap::from([
                ("card_id".to_string(), serde_json::json!("T-1")),
                ("title".to_string(), serde_json::json!("Large card")),
                ("instructions".to_string(), serde_json::json!(instructions)),
            ]),
        )
        .await
        .unwrap();

        let board = load_board(&ccx, &task_id).await;
        assert_eq!(board.cards[0].instructions.len(), 5 * 1024 * 1024);
        let envelope = rx.try_recv().unwrap();
        let json = serde_json::to_string(&envelope).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(matches!(envelope.event, TaskEvent::BoardChanged { .. }));
        assert_eq!(value["type"], "board_changed");
        assert_eq!(value["task_id"], task_id.as_str());
        assert_eq!(value["rev"], 1);
        assert!(value.get("board").is_none());
        assert!(value.get("cards").is_none());
        assert!(json.len() < 1024);
    }

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
