pub use refact_tools::tools::tool_task_batch::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{HashMap, HashSet};

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;

#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;

#[cfg(test)]
#[allow(unused_imports)]
use serde::Serialize;

#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};

#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;

#[cfg(test)]
#[allow(unused_imports)]
use tokio::task::JoinSet;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;

#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};

#[cfg(test)]
#[allow(unused_imports)]
use crate::app_state::AppState;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage as storage;

#[cfg(test)]
#[allow(unused_imports)]
use refact_tasks::types::{BoardCard, TaskBoard};

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_board::ToolTaskBoardCreateCard;

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_mark_card::{ToolTaskMarkCardDone, ToolTaskMarkCardFailed};

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_merge_agent::ToolTaskMergeAgent;

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_spawn_agent::ToolTaskSpawnAgent;

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::global_context::GlobalContext;
    use refact_tasks::types::{TaskMeta as StoredTaskMeta, TaskStatus};
    use chrono::Utc;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;

    fn card(id: &str, column: &str, deps: Vec<&str>) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: format!("Card {}", id),
            column: column.to_string(),
            priority: "P1".to_string(),
            depends_on: deps.into_iter().map(str::to_string).collect(),
            instructions: String::new(),
            assignee: None,
            agent_chat_id: None,
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

    fn task_meta(task_id: &str) -> StoredTaskMeta {
        let now = Utc::now().to_rfc3339();
        StoredTaskMeta {
            schema_version: 1,
            id: task_id.to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 0,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        }
    }

    async fn setup_task(board: TaskBoard) -> (Arc<GlobalContext>, tempfile::TempDir) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let temp = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let task_dir = temp.path().join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        storage::save_task_meta(gcx.clone(), "task-1", &task_meta("task-1"))
            .await
            .unwrap();
        storage::save_board(gcx.clone(), "task-1", &board)
            .await
            .unwrap();
        (gcx, temp)
    }

    async fn planner_ccx(gcx: Arc<GlobalContext>) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                Vec::new(),
                "planner-chat".to_string(),
                None,
                "model".to_string(),
                Some(ThreadTaskMeta {
                    task_id: "task-1".to_string(),
                    role: "planner".to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: Some("planner-chat".to_string()),
                }),
                None,
            )
            .await,
        ))
    }

    fn output_value(contexts: Vec<ContextEnum>) -> Value {
        let text = contexts_text(&contexts);
        serde_json::from_str(&text).unwrap()
    }

    fn rendered_merge_result(message: &str) -> Result<(bool, Vec<ContextEnum>), String> {
        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(message.to_string()),
                ..Default::default()
            })],
        ))
    }

    #[tokio::test]
    async fn planner_context_rejects_task_id_mismatching_bound_task() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx).await;
        let args = HashMap::from([("task_id".to_string(), json!("task-2"))]);

        let error = match planner_context(&ccx, &args, "batch_tool").await {
            Ok(_) => panic!("mismatching task_id should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("task-2"));
        assert!(error.contains("task-1"));
        assert!(error.contains("does not match"));
    }

    #[tokio::test]
    async fn planner_context_accepts_task_id_matching_bound_task() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx).await;
        let args = HashMap::from([("task_id".to_string(), json!("task-1"))]);

        let (_, task_id) = planner_context(&ccx, &args, "batch_tool").await.unwrap();

        assert_eq!(task_id, "task-1");
    }

    #[tokio::test]
    async fn planner_context_rejects_non_string_task_id() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx).await;
        let args = HashMap::from([("task_id".to_string(), json!(123))]);

        let error = match planner_context(&ccx, &args, "batch_tool").await {
            Ok(_) => panic!("non-string task_id should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("must be a string"));
    }

    #[tokio::test]
    async fn planner_context_rejects_blank_task_id() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx).await;
        let args = HashMap::from([("task_id".to_string(), json!("  \n  "))]);

        let error = match planner_context(&ccx, &args, "batch_tool").await {
            Ok(_) => panic!("blank task_id should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("non-empty string"));
    }

    #[test]
    fn spawn_agents_batch_with_3_valid_cards() {
        let board = TaskBoard {
            cards: vec![
                card("T-1", "planned", vec![]),
                card("T-2", "planned", vec![]),
                card("T-3", "planned", vec![]),
            ],
            ..Default::default()
        };
        let items = vec![
            Ok(SpawnBatchItem {
                card_id: "T-1".to_string(),
                suggested_steps: None,
                files_to_open: vec![],
                base_branch: None,
                base_commit: None,
            }),
            Ok(SpawnBatchItem {
                card_id: "T-2".to_string(),
                suggested_steps: None,
                files_to_open: vec![],
                base_branch: None,
                base_commit: None,
            }),
            Ok(SpawnBatchItem {
                card_id: "T-3".to_string(),
                suggested_steps: None,
                files_to_open: vec![],
                base_branch: None,
                base_commit: None,
            }),
        ];

        let errors = validate_spawn_agent_cards(&board, &items);

        assert_eq!(errors, vec![None, None, None]);
    }

    #[test]
    fn spawn_batch_forwards_card_base_refs() {
        let item = parse_spawn_item(&json!({
            "card_id": "T-1",
            "base_branch": "release",
            "base_commit": "abc123"
        }))
        .unwrap();
        let args = spawn_args("task-1", &item);
        assert_eq!(args.get("base_branch"), Some(&json!("release")));
        assert_eq!(args.get("base_commit"), Some(&json!("abc123")));
    }

    #[test]
    fn merge_ready_in_order_stops_on_conflict() {
        let mut first = card("T-1", "done", vec![]);
        first.agent_branch = Some("agent/T-1".to_string());
        let mut second = card("T-2", "done", vec!["T-1"]);
        second.agent_branch = Some("agent/T-2".to_string());
        let mut third = card("T-3", "done", vec!["T-2"]);
        third.agent_branch = Some("agent/T-3".to_string());
        let board = TaskBoard {
            cards: vec![third, second, first],
            ..Default::default()
        };
        let candidates = dependency_ordered_done_agent_cards(&board, 10);
        let outcomes = vec![
            MergeBatchResult {
                card_id: candidates[0].clone(),
                merged: true,
                conflict: false,
                error: None,
            },
            MergeBatchResult {
                card_id: candidates[1].clone(),
                merged: false,
                conflict: true,
                error: None,
            },
            MergeBatchResult {
                card_id: candidates[2].clone(),
                merged: true,
                conflict: false,
                error: None,
            },
        ];
        let mut processed = Vec::new();
        for outcome in outcomes {
            let keep_going = should_continue_merges(&outcome, true);
            processed.push(outcome.card_id);
            if !keep_going {
                break;
            }
        }

        assert_eq!(candidates, vec!["T-1", "T-2", "T-3"]);
        assert_eq!(processed, vec!["T-1", "T-2"]);
    }

    #[test]
    fn merge_result_reports_rendered_auto_revert_as_not_merged() {
        let rendered = r#"# Agent Work Merged

The agent's work has been successfully merged back to the target branch.

## Post-merge regression detected

**Verification:** `cargo test` failed with exit code 1
**Auto-reverted:** true
**Merge commit:** abc123
**Revert commit:** def456"#;

        let result = merge_result_from_tool("T-1".to_string(), rendered_merge_result(rendered));

        assert!(!result.merged);
        assert!(!result.conflict);
        assert!(result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("automatically reverted")));
        assert!(!should_continue_merges(&result, false));
    }

    #[test]
    fn merge_result_reports_rendered_non_reverted_merge_as_merged() {
        let rendered = r#"# Agent Work Merged

The agent's work has been successfully merged back to the target branch.

**Post-merge check:** `cargo test` passed"#;

        let result = merge_result_from_tool("T-1".to_string(), rendered_merge_result(rendered));

        assert!(result.merged);
        assert!(!result.conflict);
        assert_eq!(result.error, None);
        assert!(should_continue_merges(&result, true));
    }

    #[test]
    fn merge_ready_in_order_stops_after_generic_non_merge() {
        let result = MergeBatchResult {
            card_id: "T-1".to_string(),
            merged: false,
            conflict: false,
            error: Some("merge failed".to_string()),
        };

        assert!(!should_continue_merges(&result, false));
    }

    #[test]
    fn merge_ready_in_order_conflict_setting_only_controls_conflicts() {
        let result = MergeBatchResult {
            card_id: "T-1".to_string(),
            merged: true,
            conflict: true,
            error: None,
        };

        assert!(!should_continue_merges(&result, true));
        assert!(should_continue_merges(&result, false));
    }

    #[test]
    fn merge_ready_in_order_exposes_post_merge_verification_schema() {
        let schema = ToolMergeReadyInOrder::new().tool_description().input_schema;
        let properties = schema["properties"].as_object().unwrap();

        assert_eq!(properties["auto_revert"]["type"], json!("boolean"));
        assert_eq!(properties["auto_revert"]["default"], json!(false));
        assert_eq!(
            properties["auto_revert_timeout_secs"]["type"],
            json!("integer")
        );
        assert_eq!(
            properties["auto_revert_timeout_secs"]["default"],
            json!(refact_chat_api::tools_facade::DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS)
        );
        assert!(!properties.contains_key("force"));
    }

    #[test]
    fn merge_ready_in_order_forwards_post_merge_verification_args() {
        let requested = HashMap::from([
            ("auto_revert".to_string(), json!(true)),
            ("auto_revert_timeout_secs".to_string(), json!(120)),
        ]);
        let auto_revert = auto_revert_arg(&requested).unwrap();
        let timeout = auto_revert_timeout_secs_arg(&requested).unwrap();

        let forwarded = merge_args("task-1", "T-1", "squash", auto_revert, timeout);

        assert_eq!(forwarded["auto_revert"], json!(true));
        assert_eq!(forwarded["auto_revert_timeout_secs"], json!(120));
        assert!(!forwarded.contains_key("force"));
    }

    #[test]
    fn merge_ready_in_order_post_merge_verification_args_match_merge_agent_semantics() {
        let empty = HashMap::new();
        assert!(!auto_revert_arg(&empty).unwrap());
        assert_eq!(
            auto_revert_timeout_secs_arg(&empty).unwrap(),
            refact_chat_api::tools_facade::DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS
        );

        let invalid_bool = HashMap::from([("auto_revert".to_string(), json!("sometimes"))]);
        assert!(auto_revert_arg(&invalid_bool)
            .unwrap_err()
            .contains("must be a boolean"));

        let invalid_timeout =
            HashMap::from([("auto_revert_timeout_secs".to_string(), json!("120"))]);
        assert!(auto_revert_timeout_secs_arg(&invalid_timeout)
            .unwrap_err()
            .contains("positive integer"));

        let zero_timeout = HashMap::from([("auto_revert_timeout_secs".to_string(), json!(0))]);
        assert!(auto_revert_timeout_secs_arg(&zero_timeout)
            .unwrap_err()
            .contains("greater than zero"));
    }

    #[tokio::test]
    async fn mark_done_batch_mixed_success_failure() {
        let board = TaskBoard {
            cards: vec![card("T-1", "planned", vec![])],
            ..Default::default()
        };
        let (gcx, _temp) = setup_task(board).await;
        let ccx = planner_ccx(gcx.clone()).await;
        let args = HashMap::from_iter([(
            "items".to_string(),
            json!([
                { "card_id": "T-1", "report": "done cleanly" },
                { "card_id": "missing", "report": "not found" }
            ]),
        )]);
        let mut tool = ToolMarkDoneBatch::new();

        let (_, contexts) = tool
            .tool_execute(ccx, &"tool-call".to_string(), &args)
            .await
            .unwrap();
        let output = output_value(contexts);
        let board = storage::load_board(gcx, "task-1").await.unwrap();

        assert_eq!(output[0]["success"], json!(true));
        assert_eq!(output[1]["success"], json!(false));
        assert!(output[1]["error"].as_str().unwrap().contains("not found"));
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "done");
        assert_eq!(card.final_report.as_deref(), Some("done cleanly"));
    }

    #[test]
    fn board_create_batch_detects_cycles() {
        let board = TaskBoard::default();
        let items = vec![
            Ok(BoardCreateBatchItem {
                card_id: "T-1".to_string(),
                title: "First".to_string(),
                priority: "P1".to_string(),
                instructions: String::new(),
                depends_on: vec!["T-2".to_string()],
                target_files: vec![],
            }),
            Ok(BoardCreateBatchItem {
                card_id: "T-2".to_string(),
                title: "Second".to_string(),
                priority: "P1".to_string(),
                instructions: String::new(),
                depends_on: vec!["T-1".to_string()],
                target_files: vec![],
            }),
        ];

        let errors = validate_board_create_batch(&board, &items);

        assert!(errors.iter().all(|error| error
            .as_deref()
            .is_some_and(|error| error.contains("cycle"))));
    }
}
