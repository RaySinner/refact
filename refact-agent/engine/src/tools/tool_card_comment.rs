pub use refact_tools::tools::tool_card_comment::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_core::chat_types::{ChatContent, ContextEnum};
#[cfg(test)]
use refact_runtime_api::task_storage as storage;
#[cfg(test)]
use refact_tasks::types::CardComment;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use crate::http::routers::v1::tasks::{CreateCardCommentRequest, handle_create_card_comment};
    use refact_tasks::comments_cap::COMMENT_CAP;
    use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus};
    use crate::tools::tools_description::Tool;
    use axum::extract::{Path, State};
    use axum::response::Json;
    use uuid::Uuid;

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
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

    fn test_card(id: &str, comments: Vec<CardComment>) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: format!("Card {}", id),
            column: "doing".to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: String::new(),
            assignee: Some("agent-1".to_string()),
            agent_chat_id: Some(format!("agent-chat-{}", id)),
            retry_count: 0,
            status_updates: vec![],
            comments,
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: Utc::now().to_rfc3339(),
            started_at: Some(Utc::now().to_rfc3339()),
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

    fn comment(id: &str, timestamp: &str, body: &str) -> CardComment {
        CardComment {
            id: id.to_string(),
            author_role: "planner".to_string(),
            author_id: None,
            timestamp: timestamp.to_string(),
            body: body.to_string(),
            reply_to: None,
        }
    }

    fn task_meta() -> TaskMeta {
        let now = Utc::now().to_rfc3339();
        TaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
            name: "Task".to_string(),
            status: TaskStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            cards_total: 1,
            cards_done: 0,
            cards_failed: 0,
            agents_active: 1,
            base_branch: None,
            base_commit: None,
            default_agent_model: None,
            is_name_generated: false,
            last_agents_summary_at: None,
            planner_session_state: None,
        }
    }

    async fn write_task(
        root: &std::path::Path,
        cards: Vec<BoardCard>,
    ) -> Arc<crate::global_context::GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        storage::save_task_meta(gcx.clone(), "task-1", &task_meta())
            .await
            .unwrap();
        storage::save_board(
            gcx.clone(),
            "task-1",
            &TaskBoard {
                cards,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        gcx
    }

    async fn task_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
        role: &str,
        card_id: Option<&str>,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                format!("{}-chat", role),
                None,
                "model".to_string(),
                Some(ThreadTaskMeta {
                    task_id: "task-1".to_string(),
                    role: role.to_string(),
                    agent_id: (role == "agents").then(|| "agent-1".to_string()),
                    card_id: card_id.map(str::to_string),
                    planner_chat_id: Some("planner-chat".to_string()),
                }),
                None,
            )
            .await,
        ))
    }

    async fn create_http_comment(
        gcx: Arc<crate::global_context::GlobalContext>,
        card_id: &str,
        body: &str,
        reply_to: Option<String>,
    ) -> Result<TaskBoard, (hyper::StatusCode, String)> {
        Ok(handle_create_card_comment(
            State(refact_runtime_api::app_state_from_gcx(gcx).await),
            Path(("task-1".to_string(), card_id.to_string())),
            Json(CreateCardCommentRequest {
                body: body.to_string(),
                author_role: "user".to_string(),
                author_id: Some("user-1".to_string()),
                reply_to,
            }),
        )
        .await?
        .0)
    }

    #[tokio::test]
    async fn card_comment_add_records_planner_agent_and_reply_comments() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-41", vec![])]).await;
        let planner = task_ccx(gcx.clone(), "planner", None).await;
        let agent = task_ccx(gcx.clone(), "agents", Some("T-41")).await;

        ToolCardCommentAdd::new()
            .tool_execute(
                planner,
                &"call-1".to_string(),
                &args(&[
                    ("card_id", json!("T-41")),
                    ("body", json!("Please use sticky alerts.")),
                ]),
            )
            .await
            .unwrap();
        let first_id = storage::load_board(gcx.clone(), "task-1")
            .await
            .unwrap()
            .get_card("T-41")
            .unwrap()
            .comments[0]
            .id
            .clone();

        ToolCardCommentAdd::new()
            .tool_execute(
                agent,
                &"call-2".to_string(),
                &args(&[
                    ("card_id", json!("T-41")),
                    ("body", json!("Got it.")),
                    ("reply_to", json!(first_id.clone())),
                ]),
            )
            .await
            .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let card = board.get_card("T-41").unwrap();
        assert_eq!(card.comments.len(), 2);
        assert_eq!(card.comments[0].author_role, "planner");
        assert!(card.comments[0].author_id.is_none());
        assert_eq!(card.comments[0].body, "Please use sticky alerts.");
        assert_eq!(card.comments[1].author_role, "agents");
        assert_eq!(card.comments[1].author_id.as_deref(), Some("agent-1"));
        assert_eq!(
            card.comments[1].reply_to.as_deref(),
            Some(first_id.as_str())
        );
        assert!(card.last_heartbeat_at.is_some());
    }

    #[tokio::test]
    async fn task_comment_http_created_comment_is_replyable_from_tool() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-41", vec![])]).await;
        let board = create_http_comment(gcx.clone(), "T-41", "HTTP seed.", None)
            .await
            .unwrap();
        let http_id = board.get_card("T-41").unwrap().comments[0].id.clone();
        assert!(Uuid::parse_str(&http_id).is_ok());

        ToolCardCommentAdd::new()
            .tool_execute(
                task_ccx(gcx.clone(), "agents", Some("T-41")).await,
                &"call-tool-reply".to_string(),
                &args(&[
                    ("card_id", json!("T-41")),
                    ("body", json!("Tool reply.")),
                    ("reply_to", json!(http_id.clone())),
                ]),
            )
            .await
            .unwrap();

        let board = storage::load_board(gcx, "task-1").await.unwrap();
        let comments = &board.get_card("T-41").unwrap().comments;
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[1].author_role, "agents");
        assert_eq!(comments[1].reply_to.as_deref(), Some(http_id.as_str()));
    }

    #[tokio::test]
    async fn task_comment_tool_created_comment_is_replyable_from_http() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-41", vec![])]).await;
        ToolCardCommentAdd::new()
            .tool_execute(
                task_ccx(gcx.clone(), "planner", None).await,
                &"call-tool-seed".to_string(),
                &args(&[("card_id", json!("T-41")), ("body", json!("Tool seed."))]),
            )
            .await
            .unwrap();
        let tool_id = storage::load_board(gcx.clone(), "task-1")
            .await
            .unwrap()
            .get_card("T-41")
            .unwrap()
            .comments[0]
            .id
            .clone();
        assert!(Uuid::parse_str(&tool_id).is_ok());

        let board = create_http_comment(gcx, "T-41", "HTTP reply.", Some(tool_id.clone()))
            .await
            .unwrap();
        let comments = &board.get_card("T-41").unwrap().comments;
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[1].author_role, "user");
        assert_eq!(comments[1].reply_to.as_deref(), Some(tool_id.as_str()));
    }

    #[tokio::test]
    async fn task_comment_http_rejects_invalid_reply_target() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-41", vec![])]).await;
        let err = create_http_comment(
            gcx,
            "T-41",
            "Bad reply.",
            Some("not-a-real-comment".to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(err.0, hyper::StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "reply_to references unknown comment");
    }

    #[tokio::test]
    async fn task_comment_tool_rejects_invalid_reply_target_by_existence_not_shape() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_task(temp.path(), vec![test_card("T-41", vec![])]).await;
        let missing_uuid = Uuid::new_v4().to_string();

        let err = ToolCardCommentAdd::new()
            .tool_execute(
                task_ccx(gcx, "planner", None).await,
                &"call-bad-reply".to_string(),
                &args(&[
                    ("card_id", json!("T-41")),
                    ("body", json!("Bad reply.")),
                    ("reply_to", json!(missing_uuid)),
                ]),
            )
            .await
            .unwrap_err();

        assert_eq!(err, "reply_to references unknown comment");
    }

    #[tokio::test]
    async fn card_comment_list_filters_since_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let comments = vec![
            comment("11111111", "2026-05-22T10:00:00Z", "old"),
            comment("22222222", "2026-05-22T10:05:00Z", "new"),
        ];
        let gcx = write_task(temp.path(), vec![test_card("T-41", comments)]).await;
        let planner = task_ccx(gcx, "planner", None).await;

        let output = output_text(
            ToolCardCommentList::new()
                .tool_execute(
                    planner,
                    &"call".to_string(),
                    &args(&[
                        ("card_id", json!("T-41")),
                        ("since", json!("2026-05-22T10:01:00Z")),
                    ]),
                )
                .await
                .unwrap(),
        );

        assert!(output.contains("# Comments on T-41 (1)"));
        assert!(!output.contains("old"));
        assert!(output.contains("new"));
        assert!(output.contains("[id=22222222]"));
    }

    #[tokio::test]
    async fn card_comment_add_enforces_comment_cap() {
        let temp = tempfile::tempdir().unwrap();
        let comments = (0..COMMENT_CAP)
            .map(|idx| comment(&format!("{:08x}", idx), "2026-05-22T10:00:00Z", "existing"))
            .collect();
        let gcx = write_task(temp.path(), vec![test_card("T-41", comments)]).await;
        let planner = task_ccx(gcx.clone(), "planner", None).await;

        let err = ToolCardCommentAdd::new()
            .tool_execute(
                planner,
                &"call".to_string(),
                &args(&[("card_id", json!("T-41")), ("body", json!("overflow"))]),
            )
            .await
            .unwrap_err();

        assert!(err.contains("maximum 500 comments"));
        let board = storage::load_board(gcx, "task-1").await.unwrap();
        assert_eq!(board.get_card("T-41").unwrap().comments.len(), COMMENT_CAP);
    }

    #[test]
    fn format_comments_uses_limit_and_reply_metadata() {
        let comments = vec![
            comment("11111111", "2026-05-22T10:00:00Z", "first"),
            CardComment {
                reply_to: Some("11111111".to_string()),
                ..comment("22222222", "2026-05-22T10:05:00Z", "second")
            },
        ];

        let output = format_comments("T-41", &comments, 1, None);

        assert!(output.contains("# Comments on T-41 (1)"));
        assert!(!output.contains("first"));
        assert!(output.contains("second"));
        assert!(output.contains("[id=22222222, reply_to=11111111]"));
    }
}
