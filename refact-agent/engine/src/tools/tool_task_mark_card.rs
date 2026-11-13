pub use refact_tools::tools::tool_task_mark_card::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use refact_core::chat_types::{ChatContent, ContextEnum};
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
use crate::tools::tools_description::Tool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use crate::global_context::GlobalContext;
    use refact_tasks::types::{
        BoardCard, ScopeGuardMode, TaskBoard, TaskMeta as StoredTaskMeta, TaskStatus,
    };
    use serde_json::json;
    use std::path::Path;

    fn done_card(card_id: &str) -> BoardCard {
        let now = Utc::now().to_rfc3339();
        BoardCard {
            id: card_id.to_string(),
            title: "Test card".to_string(),
            column: "done".to_string(),
            priority: "P1".to_string(),
            depends_on: Vec::new(),
            instructions: "Do something".to_string(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: Vec::new(),
            comments: vec![],
            final_report: Some("original report".to_string()),
            final_report_structured: None,
            verifier_report: None,
            created_at: now.clone(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: Some(now),
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: Vec::new(),
            scope_guard_mode: ScopeGuardMode::Off,
        }
    }

    async fn write_done_task(root: &Path) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        let now = Utc::now().to_rfc3339();
        let meta = StoredTaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 1,
            cards_failed: 0,
            agents_active: 0,
            base_branch: Some("main".to_string()),
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        storage::save_task_meta(gcx.clone(), "task-1", &meta)
            .await
            .unwrap();
        storage::save_board(
            gcx.clone(),
            "task-1",
            &TaskBoard {
                cards: vec![done_card("T-1")],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        gcx
    }

    async fn make_ccx(
        gcx: Arc<GlobalContext>,
        task_meta: Option<ThreadTaskMeta>,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                "test-chat".to_string(),
                None,
                "model".to_string(),
                task_meta,
                None,
            )
            .await,
        ))
    }

    fn thread_task_meta() -> ThreadTaskMeta {
        ThreadTaskMeta {
            task_id: "task-1".to_string(),
            role: "planner".to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: Some("planner-task-1-1".to_string()),
        }
    }

    #[tokio::test]
    async fn mark_done_on_already_done_card_updates_report() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_done_task(temp.path()).await;
        let ccx = make_ccx(gcx.clone(), Some(thread_task_meta())).await;
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("report".to_string(), json!("updated report")),
            ("task_id".to_string(), json!("task-1")),
        ]);

        let result = ToolTaskMarkCardDone::new()
            .tool_execute(ccx, &"call-1".to_string(), &args)
            .await
            .expect("mark_done on already-done card should succeed");

        let message = result.1.first().expect("should return a message");
        if let ContextEnum::ChatMessage(msg) = message {
            if let ChatContent::SimpleText(text) = &msg.content {
                assert!(
                    text.contains("already done"),
                    "response should indicate card was already done: {}",
                    text
                );
            }
        }

        let board = storage::load_board(gcx.clone(), "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "done");
        assert_eq!(card.final_report.as_deref(), Some("updated report"));
        assert!(
            card
                .status_updates
                .iter()
                .any(|s| s.message == "Final report updated by planner"),
            "status_updates should include 'Final report updated by planner'"
        );
    }

    #[tokio::test]
    async fn mark_done_on_active_card_marks_done() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = temp.path().join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let now = Utc::now().to_rfc3339();
        let meta = StoredTaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 0,
            base_branch: Some("main".to_string()),
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        };
        storage::save_task_meta(gcx.clone(), "task-1", &meta)
            .await
            .unwrap();
        let mut card = done_card("T-1");
        card.column = "doing".to_string();
        card.completed_at = None;
        storage::save_board(
            gcx.clone(),
            "task-1",
            &TaskBoard {
                cards: vec![card],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let ccx = make_ccx(gcx.clone(), Some(thread_task_meta())).await;
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("report".to_string(), json!("fresh report")),
            ("task_id".to_string(), json!("task-1")),
        ]);

        let result = ToolTaskMarkCardDone::new()
            .tool_execute(ccx, &"call-2".to_string(), &args)
            .await
            .expect("mark_done on active card should succeed");

        let message = result.1.first().expect("should return a message");
        if let ContextEnum::ChatMessage(msg) = message {
            if let ChatContent::SimpleText(text) = &msg.content {
                assert!(
                    text.contains("marked as done"),
                    "response should indicate card was marked done: {}",
                    text
                );
            }
        }

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-1").unwrap();
        assert_eq!(card.column, "done");
        assert_eq!(card.final_report.as_deref(), Some("fresh report"));
        assert!(card.completed_at.is_some());
    }
}
