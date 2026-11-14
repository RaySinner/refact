pub use refact_tools::tools::tool_task_overview::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::time::SystemTime;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::{DateTime, Duration, Utc};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use serde_yaml::Value as YamlValue;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::fs;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::history_limit::{compute_context_budget, ContextBudgetReport, ContextPressure};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
#[allow(unused_imports)]
use refact_tasks::types::{BoardCard, TaskBoard};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_check_agents::{get_agent_statuses_for_ctx, AgentStatus};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::{ChatSessionFacade, SessionState};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::global_context::GlobalContext;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{TaskMeta, TaskStatus};
    use crate::tools::tools_description::Tool;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn task_meta() -> TaskMeta {
        TaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Empty Task".to_string(),
            status: TaskStatus::Active,
            created_at: "2026-05-22T00:00:00Z".to_string(),
            updated_at: "2026-05-22T00:00:00Z".to_string(),
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

    async fn write_empty_task(root: &Path) -> Arc<GlobalContext> {
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

    async fn planner_ccx(
        gcx: Arc<GlobalContext>,
        role: &str,
        messages: Vec<ChatMessage>,
        n_ctx: usize,
    ) -> Arc<AMutex<AtCommandsContext>> {
        let app = refact_runtime_api::app_state_from_gcx(gcx).await;
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app,
                n_ctx,
                20,
                false,
                messages,
                "planner-chat".to_string(),
                None,
                "model".to_string(),
                Some(ThreadTaskMeta {
                    task_id: "task-1".to_string(),
                    role: role.to_string(),
                    agent_id: None,
                    card_id: None,
                    planner_chat_id: None,
                }),
                None,
            )
            .await,
        ))
    }

    fn output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    #[test]
    fn tool_task_overview_description_correct() {
        let desc = ToolTaskOverview::new().tool_description();

        assert_eq!(desc.name, "task_overview");
        assert_eq!(desc.display_name, "Task Overview");
        assert_eq!(desc.input_schema["required"], json!([]));
        assert!(desc.input_schema["properties"].get("task_id").is_some());
        assert!(desc.description.contains("Planner-only"));
    }

    fn status(id: &str, session_state: Option<SessionState>, minutes_ago: i64) -> AgentStatus {
        let ts = fixed_now() - Duration::minutes(minutes_ago);
        AgentStatus {
            card_id: id.to_string(),
            variant_key: None,
            card_title: format!("{} title", id),
            agent_chat_id: format!("agent-{}", id),
            column: "doing".to_string(),
            priority: "P0".to_string(),
            session_state,
            last_status_update: None,
            last_activity_at: Some(ts),
            final_report: None,
            last_tool_name: None,
            change_seq: ts.timestamp() as u64,
            room_members: Vec::new(),
        }
    }

    #[test]
    fn task_overview_counts_old_active_generation_as_running() {
        let statuses = vec![
            status("T-1", Some(SessionState::Generating), 30),
            status("T-2", Some(SessionState::ExecutingTools), 30),
        ];
        let output = render_agent_health(&statuses, fixed_now());

        assert!(output.contains("🔄 2 running"));
        assert!(output.contains("🔴 0 stuck"));
    }

    #[test]
    fn task_overview_counts_off_generation_loop_as_stuck() {
        let statuses = vec![status("T-1", Some(SessionState::Idle), 2)];
        let output = render_agent_health(&statuses, fixed_now());

        assert!(output.contains("🔄 0 running"));
        assert!(output.contains("🔴 1 stuck"));
    }

    #[tokio::test]
    async fn tool_task_overview_empty_task_renders_gracefully() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_empty_task(temp.path()).await;
        let ccx = planner_ccx(gcx, "planner", vec![], 200_000).await;
        let output = output_text(
            ToolTaskOverview::new()
                .tool_execute(ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );

        assert!(output.contains("# Task Overview: Empty Task"));
        assert!(output.contains("## Board"));
        assert!(output.contains("## Agent Health"));
        assert!(output.contains("## Memory Pulse"));
        assert!(output.contains("## Documents"));
        assert!(output.contains("## Recent Activity (last 1h)"));
        assert!(output.contains("## Context Pressure"));
        assert!(output.contains("_No cards yet._"));
        assert!(output.contains("No task documents found"));
        assert!(output.contains("No activity in the last hour"));
    }

    #[tokio::test]
    async fn tool_task_overview_rejects_non_planner() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx, "agents", vec![], 200_000).await;
        let err = ToolTaskOverview::new()
            .tool_execute(ccx, &"call".to_string(), &HashMap::new())
            .await
            .unwrap_err();

        assert!(err.contains("can only be called by the task planner"));
    }

    #[test]
    fn tool_task_overview_pressure_indicator_correct() {
        let message = ChatMessage::new("user".to_string(), "x".repeat(280));
        let budget = compute_context_budget(&[message], 100);
        let output = render_context_pressure(&budget);

        assert!(output.contains("Planner chat: 80% of 100 tokens used (Medium)"));
    }

    #[test]
    fn tool_task_overview_render_all_sections_for_empty_inputs() {
        let board = TaskBoard::default();
        let budget = compute_context_budget(&[], 100);
        let output = render_task_overview(
            "Empty Task",
            &board,
            &[],
            &MemoryPulse::default(),
            &[],
            &[],
            &budget,
            fixed_now(),
        );

        assert!(output.contains("## Board"));
        assert!(output.contains("## Agent Health"));
        assert!(output.contains("## Memory Pulse"));
        assert!(output.contains("## Documents"));
        assert!(output.contains("## Recent Activity (last 1h)"));
        assert!(output.contains("## Context Pressure"));
        assert!(output.chars().count() <= MAX_REPORT_CHARS);
    }
}
