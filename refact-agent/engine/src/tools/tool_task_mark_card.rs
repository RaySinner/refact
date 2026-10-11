use std::collections::HashMap;
use std::sync::Arc;
use serde_json::Value;
use tokio::sync::Mutex as AMutex;
use async_trait::async_trait;
use chrono::Utc;

use crate::tools::tools_description::{
    Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params,
};
use crate::call_validation::{ChatMessage, ChatContent, ContextEnum};
use crate::at_commands::at_commands::AtCommandsContext;
use crate::tasks::storage;
use crate::tasks::types::StatusUpdate;

async fn get_task_id(
    ccx: &Arc<AMutex<AtCommandsContext>>,
    args: &HashMap<String, Value>,
) -> Result<String, String> {
    if let Some(id) = args.get("task_id").and_then(|v| v.as_str()) {
        return Ok(id.to_string());
    }
    let ccx_lock = ccx.lock().await;
    ccx_lock
        .task_meta
        .as_ref()
        .map(|m| m.task_id.clone())
        .ok_or_else(|| "Missing 'task_id' (and chat is not bound to a task)".to_string())
}

pub struct ToolTaskMarkCardDone;
pub struct ToolTaskMarkCardFailed;

impl ToolTaskMarkCardDone {
    pub fn new() -> Self {
        Self
    }
}

impl ToolTaskMarkCardFailed {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ToolTaskMarkCardDone {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "mark_done".to_string(),
            display_name: "Task Mark Card Done".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Manually mark a card as done. Use this if an agent completed work but forgot to call agent_finish(), or to finalize a card after reviewing the agent's work.".to_string(),
            input_schema: json_schema_from_params(&[("card_id", "string", "Card ID to mark as done"), ("report", "string", "Summary/report for the completed card")], &["card_id", "report"]),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let task_id = get_task_id(&ccx, args).await?;
        let card_id = args
            .get("card_id")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'card_id'")?;
        let report = args
            .get("report")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'report'")?;

        let gcx = ccx.lock().await.app.gcx.clone();

        let card_title = {
            let card_id_owned = card_id.to_string();
            let report_owned = report.to_string();

            let (board, already_done) =
                storage::update_board_atomic(gcx.clone(), &task_id, move |board| {
                    let card = board
                        .get_card_mut(&card_id_owned)
                        .ok_or(format!("Card {} not found", card_id_owned))?;

                    if card.column == "done" {
                        card.final_report = Some(report_owned.clone());
                        card.status_updates.push(StatusUpdate {
                            timestamp: Utc::now().to_rfc3339(),
                            message: "Final report updated by planner".to_string(),
                        });
                        return Ok(true);
                    }

                    card.final_report = Some(report_owned.clone());
                    card.final_report_structured = None;
                    card.column = "done".to_string();
                    card.completed_at = Some(Utc::now().to_rfc3339());
                    card.status_updates.push(StatusUpdate {
                        timestamp: Utc::now().to_rfc3339(),
                        message: "Manually marked as done by planner".to_string(),
                    });
                    Ok(false)
                })
                .await?;

            storage::update_task_stats(gcx.clone(), &task_id).await?;
            board
                .get_card(card_id)
                .map(|c| c.title.clone())
                .unwrap_or_default()
        };

        let result = if already_done {
            format!(
                "✅ **Card already done — final report updated:** {}\n\n**Report:**\n{}",
                card_title, report
            )
        } else {
            format!(
                "✅ **Card marked as done:** {}\n\n**Report:**\n{}",
                card_title, report
            )
        };

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[async_trait]
impl Tool for ToolTaskMarkCardFailed {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "mark_failed".to_string(),
            display_name: "Task Mark Card Failed".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Manually mark a card as failed. Use this to resolve stuck agents, mark cards that cannot be completed, or when an agent errored without calling agent_finish().".to_string(),
            input_schema: json_schema_from_params(&[("card_id", "string", "Card ID to mark as failed"), ("reason", "string", "Reason for failure")], &["card_id", "reason"]),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let task_id = get_task_id(&ccx, args).await?;
        let card_id = args
            .get("card_id")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'card_id'")?;
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'reason'")?;

        let gcx = ccx.lock().await.app.gcx.clone();

        let card_title = {
            let card_id_owned = card_id.to_string();
            let reason_owned = reason.to_string();

            let (board, _) = storage::update_board_atomic(gcx.clone(), &task_id, move |board| {
                let card = board
                    .get_card_mut(&card_id_owned)
                    .ok_or(format!("Card {} not found", card_id_owned))?;

                if card.column == "failed" {
                    return Err(format!("Card {} is already failed", card_id_owned));
                }

                card.final_report = Some(format!("FAILED: {}", reason_owned));
                card.final_report_structured = None;
                card.column = "failed".to_string();
                card.completed_at = Some(Utc::now().to_rfc3339());
                card.status_updates.push(StatusUpdate {
                    timestamp: Utc::now().to_rfc3339(),
                    message: format!("Manually marked as failed: {}", reason_owned),
                });
                Ok(())
            })
            .await?;

            storage::update_task_stats(gcx.clone(), &task_id).await?;
            board
                .get_card(card_id)
                .map(|c| c.title.clone())
                .unwrap_or_default()
        };

        let result = format!(
            "❌ **Card marked as failed:** {}\n\n**Reason:**\n{}",
            card_title, reason
        );

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(result),
                tool_calls: None,
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::chat::types::TaskMeta as ThreadTaskMeta;
    use crate::global_context::GlobalContext;
    use crate::tasks::types::{
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
            AtCommandsContext::new_from_app(
                AppState::from_gcx(gcx).await,
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
