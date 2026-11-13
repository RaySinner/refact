pub use refact_tools::tools::tool_task_verify_card::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use crate::tools::tools_description::Tool;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use crate::global_context::GlobalContext;
    use refact_runtime_api::task_storage as storage;
    use refact_tasks::types::{TaskBoard, TaskMeta as StoredTaskMeta, TaskStatus};
    use std::path::Path;

    fn task_meta() -> StoredTaskMeta {
        let now = chrono::Utc::now().to_rfc3339();
        StoredTaskMeta {
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
        }
    }

    async fn write_task(root: &Path) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        storage::save_task_meta(gcx.clone(), "task-1", &task_meta())
            .await
            .unwrap();
        storage::save_board(gcx.clone(), "task-1", &TaskBoard::default())
            .await
            .unwrap();
        gcx
    }

    fn thread_task_meta(role: &str) -> ThreadTaskMeta {
        ThreadTaskMeta {
            task_id: "task-1".to_string(),
            role: role.to_string(),
            agent_id: (role == "agents").then(|| "agent-1".to_string()),
            card_id: (role == "agents").then(|| "T-1".to_string()),
            planner_chat_id: Some("planner-task-1-1".to_string()),
        }
    }

    async fn task_ccx(
        gcx: Arc<GlobalContext>,
        task_meta: Option<ThreadTaskMeta>,
        chat_id: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                chat_id.to_string(),
                None,
                "model".to_string(),
                task_meta,
                None,
            )
            .await,
        ))
    }

    #[tokio::test]
    async fn task_verify_card_rejects_mismatched_task_id() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = task_ccx(gcx, Some(thread_task_meta("planner")), "planner-task-1-1").await;
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("task_id".to_string(), json!("task-2")),
        ]);

        let err = ToolTaskVerifyCard::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(err.contains("does not match bound task_id"));
    }

    #[tokio::test]
    async fn task_verify_card_uses_bound_task_when_no_args() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path()).await;
        let ccx = task_ccx(gcx, Some(thread_task_meta("planner")), "planner-task-1-1").await;
        let args = HashMap::from([("card_id".to_string(), json!("missing-card"))]);

        let err = ToolTaskVerifyCard::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert_eq!(err, "Card missing-card not found");
    }

    #[tokio::test]
    async fn task_verify_card_planner_without_metadata_rejected() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = task_ccx(gcx, None, "unbound-chat").await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let err = ToolTaskVerifyCard::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(err.contains("task planner"));
    }

    async fn write_task_with_card(root: &Path, worktree: &Path) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        storage::save_task_meta(gcx.clone(), "task-1", &task_meta())
            .await
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let card = refact_tasks::types::BoardCard {
            id: "T-1".to_string(),
            title: "Test card".to_string(),
            column: "done".to_string(),
            priority: "P1".to_string(),
            depends_on: Vec::new(),
            instructions: String::new(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: Vec::new(),
            comments: vec![],
            final_report: Some("done".to_string()),
            final_report_structured: None,
            verifier_report: None,
            created_at: now.clone(),
            started_at: None,
            last_heartbeat_at: None,
            completed_at: Some(now),
            agent_branch: None,
            agent_worktree: Some(worktree.to_string_lossy().to_string()),
            agent_worktree_name: None,
            base_branch: Some("main".to_string()),
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: Vec::new(),
            scope_guard_mode: refact_tasks::types::ScopeGuardMode::Off,
        };
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
        gcx
    }

    fn init_git_repo(root: &Path) {
        for args in [
            vec!["init"],
            vec!["checkout", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test User"],
            vec!["add", "file.txt"],
            vec!["commit", "-m", "initial"],
        ] {
            if args[0] == "add" {
                std::fs::write(root.join("file.txt"), "hello\n").unwrap();
            }
            let output = std::process::Command::new("git")
                .current_dir(root)
                .args(&args)
                .output()
                .unwrap_or_else(|e| panic!("git {:?} failed: {e}", args));
            assert!(output.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&output.stderr));
        }
    }

    #[tokio::test]
    async fn task_verify_card_with_custom_command_includes_it_in_results() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        init_git_repo(&worktree);

        let gcx = write_task_with_card(temp.path(), &worktree).await;
        let ccx = task_ccx(gcx.clone(), Some(thread_task_meta("planner")), "planner-task-1-1").await;
        let args = HashMap::from([
            ("card_id".to_string(), json!("T-1")),
            ("command".to_string(), json!("echo hello")),
        ]);

        let result = ToolTaskVerifyCard::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await;

        match result {
            Ok(_) => {
                let board = storage::load_board(gcx.clone(), "task-1").await.unwrap();
                let card = board.get_card("T-1").unwrap();
                let report = card
                    .verifier_report
                    .as_ref()
                    .expect("verifier_report should be set after verification");
                assert!(
                    report.command_results.iter().any(|r| r.command == "echo hello"),
                    "custom command 'echo hello' should appear in command_results, got: {:?}",
                    report.command_results.iter().map(|r| &r.command).collect::<Vec<_>>()
                );
            }
            Err(e) => {
                panic!("task_verify_card with custom command should not fail with: {e}");
            }
        }
    }

    #[tokio::test]
    async fn task_verify_card_non_planner_role_rejected() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = task_ccx(gcx, Some(thread_task_meta("agents")), "agent-chat").await;
        let args = HashMap::from([("card_id".to_string(), json!("T-1"))]);

        let err = ToolTaskVerifyCard::new()
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();

        assert!(err.contains("task planner"));
    }
}
