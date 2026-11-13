pub use refact_tools::tools::tool_task_check_agents::*;


#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use chrono::DateTime;
#[cfg(test)]
use chrono::Duration;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_tasks::types::AbVariantInfo;
#[cfg(test)]
use refact_tasks::types::TeamMember;
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_runtime_api::SessionState;


#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use refact_chat_api::TaskMeta as ThreadTaskMeta;
    use refact_tasks::types::{BoardCard, TaskBoard, TaskMeta as StoredTaskMeta, TaskStatus};
    use crate::tools::tool_task_wait_for_agents::ToolTaskWaitForAgents;
    use crate::tools::tools_description::Tool;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn status(
        id: &str,
        priority: &str,
        column: &str,
        session_state: Option<SessionState>,
        minutes_ago: i64,
    ) -> AgentStatus {
        let ts = now() - Duration::minutes(minutes_ago);
        AgentStatus {
            card_id: id.to_string(),
            variant_key: None,
            card_title: format!("{} title", id),
            agent_chat_id: format!("agent-{}", id),
            column: column.to_string(),
            priority: priority.to_string(),
            session_state,
            last_status_update: Some(format!("{}: last: cat", ts.to_rfc3339())),
            last_activity_at: Some(ts),
            final_report: if column == "done" {
                Some("done".to_string())
            } else {
                None
            },
            last_tool_name: Some("cat".to_string()),
            change_seq: ts.timestamp() as u64,
            room_members: vec![],
        }
    }

    fn query(format: AgentReportFormat) -> AgentStatusQuery {
        let mut query = AgentStatusQuery::default_for_format(format);
        query.status_filter = None;
        query.group_by = AgentGroupBy::None;
        query
    }

    fn card_lines(output: &str) -> Vec<&str> {
        output.lines().filter(|line| line.contains("T-")).collect()
    }

    fn args(items: &[(&str, Value)]) -> HashMap<String, Value> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn board_card(id: &str, column: &str, agent_chat_id: Option<String>) -> BoardCard {
        BoardCard {
            id: id.to_string(),
            title: format!("{} title", id),
            column: column.to_string(),
            priority: "P1".to_string(),
            depends_on: vec![],
            instructions: "do work".to_string(),
            assignee: agent_chat_id.as_ref().map(|_| "agent-1".to_string()),
            agent_chat_id,
            retry_count: 0,
            status_updates: vec![],
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: now().to_rfc3339(),
            started_at: Some(now().to_rfc3339()),
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

    fn orphan_doing_card(id: &str) -> BoardCard {
        let mut card = board_card(id, "doing", None);
        card.assignee = Some("agent-1".to_string());
        card
    }

    fn ab_doing_card(id: &str) -> BoardCard {
        let mut card = board_card(id, "doing", None);
        card.assignee = Some("ab".to_string());
        card.ab_variants = Some(refact_tasks::types::AbVariants {
            a: AbVariantInfo {
                agent_id: "agent-a".to_string(),
                chat_id: "agent-chat-a".to_string(),
                worktree: "/tmp/agent-a".to_string(),
                worktree_name: Some("worktree-a".to_string()),
                branch: Some("branch-a".to_string()),
                model: Some("model-a".to_string()),
                finish: None,
            },
            b: AbVariantInfo {
                agent_id: "agent-b".to_string(),
                chat_id: "agent-chat-b".to_string(),
                worktree: "/tmp/agent-b".to_string(),
                worktree_name: Some("worktree-b".to_string()),
                branch: Some("branch-b".to_string()),
                model: Some("model-b".to_string()),
                finish: None,
            },
            winner: None,
        });
        card
    }

    async fn insert_live_session(
        gcx: Arc<crate::global_context::GlobalContext>,
        chat_id: &str,
        state: SessionState,
    ) {
        let mut session = refact_chat_api::ChatSession::new(chat_id.to_string());
        session.set_runtime_state(state, None);
        gcx.chat_sessions
            .write()
            .await
            .insert(chat_id.to_string(), Arc::new(AMutex::new(session)));
    }

    fn task_meta() -> StoredTaskMeta {
        let now = Utc::now().to_rfc3339();
        StoredTaskMeta {
            schema_version: 1,
            id: "task-1".to_string(),
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

    async fn write_task_board(
        root: &std::path::Path,
        board: TaskBoard,
    ) -> Arc<crate::global_context::GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let task_dir = root.join(".refact").join("tasks").join("task-1");
        tokio::fs::create_dir_all(&task_dir).await.unwrap();
        tokio::fs::write(
            task_dir.join("meta.yaml"),
            serde_yaml::to_string(&task_meta()).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(
            task_dir.join("board.yaml"),
            serde_yaml::to_string(&board).unwrap(),
        )
        .await
        .unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.to_path_buf()];
        gcx
    }

    async fn write_empty_task(root: &std::path::Path) -> Arc<crate::global_context::GlobalContext> {
        write_task_board(root, TaskBoard::default()).await
    }

    async fn planner_ccx(
        gcx: Arc<crate::global_context::GlobalContext>,
        role: &str,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
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

    fn tool_output_text(result: (bool, Vec<ContextEnum>)) -> String {
        match result.1.into_iter().next().unwrap() {
            ContextEnum::ChatMessage(message) => match message.content {
                ChatContent::SimpleText(text) => text,
                _ => panic!("expected text output"),
            },
            _ => panic!("expected chat message"),
        }
    }

    #[test]
    fn compact_format_renders_one_line_per_agent() {
        let statuses = vec![
            status("T-1", "P0", "doing", Some(SessionState::Generating), 3),
            status("T-2", "P1", "doing", Some(SessionState::ExecutingTools), 4),
        ];
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();

        let lines = card_lines(&output);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("T-1"));
        assert!(lines[1].contains("T-2"));
    }

    #[test]
    fn active_generation_loop_is_not_stuck_only_because_activity_is_old() {
        let statuses = vec![
            status("T-1", "P0", "doing", Some(SessionState::Generating), 30),
            status("T-2", "P0", "doing", Some(SessionState::ExecutingTools), 30),
        ];
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();

        assert!(output.starts_with("⚠️  Alerts: 0 stuck, 0 failed, 0 needing approval"));
        assert!(!output.contains("STUCK"));
        assert!(output.contains("generating"));
        assert!(output.contains("exec tools"));
    }

    #[test]
    fn doing_agent_is_stuck_when_generation_loop_is_off() {
        let statuses = vec![status("T-1", "P0", "doing", Some(SessionState::Idle), 5)];
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();

        assert!(output.starts_with("⚠️  Alerts: 1 stuck, 0 failed, 0 needing approval"));
        assert!(output.contains("STUCK"));
    }

    #[test]
    fn idle_agent_is_running_during_grace_period() {
        let statuses = vec![status("T-1", "P0", "doing", Some(SessionState::Idle), 2)];
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();
        assert!(output.starts_with("⚠️  Alerts: 0 stuck, 0 failed, 0 needing approval"));
        assert!(!output.contains("STUCK"));
        assert_eq!(classify_agent_status(&statuses[0], now()), AgentStateKind::Running);
    }

    #[test]
    fn stale_idle_agent_is_stuck_after_grace_period() {
        let statuses = vec![status("T-1", "P0", "doing", Some(SessionState::Idle), 5)];
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();
        assert!(output.starts_with("⚠️  Alerts: 1 stuck, 0 failed, 0 needing approval"));
        assert!(output.contains("STUCK"));
    }

    #[test]
    fn starting_agent_is_running() {
        let statuses = vec![status("T-1", "P0", "doing", Some(SessionState::Starting), 1)];
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();
        assert!(output.starts_with("⚠️  Alerts: 0 stuck, 0 failed, 0 needing approval"));
        assert!(!output.contains("STUCK"));
        assert!(output.contains("starting"));
    }

    #[test]
    fn fresh_doing_card_without_session_is_running_during_grace_period() {
        let statuses = vec![status("T-1", "P0", "doing", None, 1)];
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();
        assert!(output.starts_with("⚠️  Alerts: 0 stuck, 0 failed, 0 needing approval"));
        assert!(!output.contains("STUCK"));
    }

    #[test]
    fn stale_doing_card_without_session_is_stuck_after_grace_period() {
        let statuses = vec![status("T-1", "P0", "doing", None, 5)];
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();
        assert!(output.starts_with("⚠️  Alerts: 1 stuck, 0 failed, 0 needing approval"));
        assert!(output.contains("STUCK"));
    }

    #[test]
    fn waitable_agent_status_at_returns_true_for_starting_agent() {
        let status = status("T-1", "P0", "doing", Some(SessionState::Starting), 1);
        assert!(waitable_agent_status_at(&status, now()));
    }

    #[test]
    fn waitable_agent_status_at_returns_true_for_fresh_none_agent() {
        let status = status("T-1", "P0", "doing", None, 1);
        assert!(waitable_agent_status_at(&status, now()));
    }

    #[test]
    fn waitable_agent_status_at_returns_false_for_stale_none_agent() {
        let status = status("T-1", "P0", "doing", None, 5);
        assert!(!waitable_agent_status_at(&status, now()));
    }

    #[test]
    fn waitable_agent_status_at_returns_true_for_fresh_idle_agent() {
        let status = status("T-1", "P0", "doing", Some(SessionState::Idle), 1);
        assert!(waitable_agent_status_at(&status, now()));
    }

    #[test]
    fn waitable_agent_status_at_returns_false_for_stale_idle_agent() {
        let status = status("T-1", "P0", "doing", Some(SessionState::Idle), 5);
        assert!(!waitable_agent_status_at(&status, now()));
    }

    #[test]
    fn check_agents_surfaces_doing_card_without_agent_chat_id_as_stuck() {
        let card = orphan_doing_card("T-404");
        assert!(should_report_card(&card));

        let status = agent_status_from_card(&card, 0, None, None);
        assert_eq!(status.agent_chat_id, "none");
        assert_eq!(classify_agent_status(&status, now()), AgentStateKind::Stuck);

        let output =
            format_agent_statuses_at(&[status], &query(AgentReportFormat::Compact), now()).unwrap();

        assert!(output.starts_with("⚠️  Alerts: 1 stuck"));
        assert!(output.contains("T-404"));
        assert!(output.contains("STUCK"));
    }

    #[tokio::test]
    async fn get_agent_statuses_surfaces_doing_card_without_agent_chat_id_as_stuck() {
        let temp = tempfile::tempdir().unwrap();
        let board = TaskBoard {
            cards: vec![orphan_doing_card("T-404")],
            ..Default::default()
        };
        let gcx = write_task_board(temp.path(), board).await;
        let chat_facade = refact_runtime_api::app_state_from_gcx(gcx.clone()).await.chat.facade.clone();

        let statuses = get_agent_statuses_for_ctx(&gcx.chat_ctx(gcx.clone()), chat_facade, "task-1")
            .await
            .unwrap();

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].card_id, "T-404");
        assert_eq!(statuses[0].agent_chat_id, "none");
        assert_eq!(
            classify_agent_status(&statuses[0], now()),
            AgentStateKind::Stuck
        );
    }

    #[tokio::test]
    async fn check_agents_tool_output_surfaces_doing_card_without_agent_chat_id() {
        let temp = tempfile::tempdir().unwrap();
        let board = TaskBoard {
            cards: vec![orphan_doing_card("T-404")],
            ..Default::default()
        };
        let gcx = write_task_board(temp.path(), board).await;
        let ccx = planner_ccx(gcx, "planner").await;

        let output = tool_output_text(
            ToolTaskCheckAgents::new()
                .tool_execute(ccx, &"call".to_string(), &HashMap::new())
                .await
                .unwrap(),
        );

        assert!(output.starts_with("⚠️  Alerts: 1 stuck"));
        assert!(output.contains("T-404"));
        assert!(output.contains("STUCK"));
        assert!(output.contains("needs attention"));
    }

    #[tokio::test]
    async fn check_agents_surfaces_active_ab_variants() {
        let temp = tempfile::tempdir().unwrap();
        let board = TaskBoard {
            cards: vec![ab_doing_card("T-AB")],
            ..Default::default()
        };
        let gcx = write_task_board(temp.path(), board).await;
        insert_live_session(gcx.clone(), "agent-chat-a", SessionState::Generating).await;
        insert_live_session(gcx.clone(), "agent-chat-b", SessionState::Paused).await;
        let chat_facade = refact_runtime_api::app_state_from_gcx(gcx.clone()).await.chat.facade.clone();

        let statuses = get_agent_statuses_for_ctx(&gcx.chat_ctx(gcx.clone()), chat_facade, "task-1")
            .await
            .unwrap();
        let output =
            format_agent_statuses_at(&statuses, &query(AgentReportFormat::Compact), now()).unwrap();

        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].status_id(), "T-AB/a");
        assert_eq!(statuses[1].status_id(), "T-AB/b");
        assert!(output.contains("T-AB/a"));
        assert!(output.contains("T-AB/b"));
        assert!(output.contains("generating"));
        assert!(output.contains("paused"));
        assert!(!output.contains("STUCK"));
    }

    #[tokio::test]
    async fn wait_agents_records_ab_variant_ids() {
        use refact_chat_api::ChatSession;
        use std::sync::atomic::Ordering;

        let temp = tempfile::tempdir().unwrap();
        let board = TaskBoard {
            cards: vec![ab_doing_card("T-AB")],
            ..Default::default()
        };
        let gcx = write_task_board(temp.path(), board).await;
        insert_live_session(gcx.clone(), "agent-chat-a", SessionState::Generating).await;
        insert_live_session(gcx.clone(), "agent-chat-b", SessionState::ExecutingTools).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let app = {
            let ccx_lock = ccx.lock().await;
            ccx_lock.app.clone()
        };
        let session_arc = Arc::new(AMutex::new(ChatSession::new("planner-chat".to_string())));
        app.chat
            .sessions
            .write()
            .await
            .insert("planner-chat".to_string(), session_arc.clone());

        let result = ToolTaskWaitForAgents::new()
            .tool_execute(ccx.clone(), &"call".to_string(), &HashMap::new())
            .await
            .unwrap();

        let output = tool_output_text(result);
        assert!(output.contains("T-AB/a"));
        assert!(output.contains("T-AB/b"));
        let session = session_arc.lock().await;
        assert_eq!(session.waiting_for_card_ids, vec!["T-AB/a", "T-AB/b"]);
        drop(session);
        let aborted = {
            let ccx_lock = ccx.lock().await;
            ccx_lock.abort_flag.load(Ordering::SeqCst)
        };
        assert!(aborted);
    }

    #[tokio::test]
    async fn wait_agents_invalid_wait_does_not_record_wait_state_or_abort() {
        use refact_chat_api::ChatSession;
        use std::sync::atomic::Ordering;

        let temp = tempfile::tempdir().unwrap();
        let board = TaskBoard {
            cards: vec![orphan_doing_card("T-1")],
            ..Default::default()
        };
        let gcx = write_task_board(temp.path(), board).await;
        let ccx = planner_ccx(gcx, "planner").await;
        let app = {
            let ccx_lock = ccx.lock().await;
            ccx_lock.app.clone()
        };
        let session_arc = Arc::new(tokio::sync::Mutex::new(ChatSession::new(
            "planner-chat".to_string(),
        )));
        app.chat
            .sessions
            .write()
            .await
            .insert("planner-chat".to_string(), session_arc.clone());

        let result = ToolTaskWaitForAgents::new()
            .tool_execute(
                ccx.clone(),
                &"call".to_string(),
                &args(&[("card_ids", json!(["T-1"]))]),
            )
            .await
            .unwrap();

        let ContextEnum::ChatMessage(message) = &result.1[0] else {
            panic!("expected chat message")
        };
        assert_eq!(message.tool_failed, Some(true));
        let ChatContent::SimpleText(text) = &message.content else {
            panic!("expected text")
        };
        assert!(text.contains("planner wait mode was not entered"));
        assert!(text.contains("T-1"));
        assert!(text.contains("STUCK"));

        let session = session_arc.lock().await;
        assert!(session.waiting_for_card_ids.is_empty());
        assert!(session.wake_up_at.is_none());
        drop(session);

        let aborted = {
            let ccx_lock = ccx.lock().await;
            ccx_lock.abort_flag.load(Ordering::SeqCst)
        };
        assert!(!aborted);
    }

    #[test]
    fn sticky_alerts_always_render_when_attention_agents_exist() {
        let statuses = vec![
            status("T-1", "P0", "doing", None, 18),
            status("T-2", "P1", "failed", Some(SessionState::Error), 2),
        ];
        let mut query = query(AgentReportFormat::Compact);
        query.card_ids = Some(HashSet::from(["missing".to_string()]));
        let output = format_agent_statuses_at(&statuses, &query, now()).unwrap();

        assert!(output.starts_with("⚠️  Alerts: 1 stuck, 1 failed, 0 needing approval"));
        assert!(card_lines(&output).is_empty());
    }

    #[test]
    fn filters_by_status_priority_and_card_ids() {
        let statuses = vec![
            status("T-1", "P0", "doing", Some(SessionState::Generating), 2),
            status("T-2", "P1", "done", Some(SessionState::Completed), 4),
            status("T-3", "P1", "failed", Some(SessionState::Error), 6),
        ];
        let mut query = query(AgentReportFormat::Compact);
        query.status_filter = Some(HashSet::from([
            AgentStateKind::Done,
            AgentStateKind::Failed,
        ]));
        query.priority_filter = Some(HashSet::from(["P1".to_string()]));
        query.card_ids = Some(HashSet::from(["T-2".to_string()]));
        let output = format_agent_statuses_at(&statuses, &query, now()).unwrap();

        let lines = card_lines(&output);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("T-2"));
    }

    #[test]
    fn sort_by_priority_orders_p0_before_p1_before_p2() {
        let statuses = vec![
            status("T-2", "P2", "doing", Some(SessionState::Generating), 2),
            status("T-0", "P0", "doing", Some(SessionState::Generating), 2),
            status("T-1", "P1", "doing", Some(SessionState::Generating), 2),
        ];
        let mut query = query(AgentReportFormat::Compact);
        query.sort = AgentSort::Priority;
        let output = format_agent_statuses_at(&statuses, &query, now()).unwrap();
        let lines = card_lines(&output);

        assert!(lines[0].contains("T-0"));
        assert!(lines[1].contains("T-1"));
        assert!(lines[2].contains("T-2"));
    }

    #[test]
    fn pagination_limit_and_offset_work() {
        let statuses = vec![
            status("T-1", "P1", "doing", Some(SessionState::Generating), 2),
            status("T-2", "P1", "doing", Some(SessionState::Generating), 2),
            status("T-3", "P1", "doing", Some(SessionState::Generating), 2),
        ];
        let mut query = query(AgentReportFormat::Compact);
        query.sort = AgentSort::CardId;
        query.limit = 2;
        query.offset = 1;
        let output = format_agent_statuses_at(&statuses, &query, now()).unwrap();
        let lines = card_lines(&output);

        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("T-2"));
        assert!(lines[1].contains("T-3"));
        assert!(output.contains("showing 2 of 3; no more pages"));
    }

    #[test]
    fn delta_mode_returns_only_changed_since_seq() {
        let mut old = status("T-1", "P1", "doing", Some(SessionState::Generating), 30);
        old.change_seq = 10;
        let mut changed = status("T-2", "P1", "doing", Some(SessionState::Generating), 2);
        changed.change_seq = 20;
        let mut query = query(AgentReportFormat::Delta);
        query.since_seq = Some(15);
        let output = format_agent_statuses_at(&[old, changed], &query, now()).unwrap();
        let lines = card_lines(&output);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("T-2"));
        assert!(!output.contains("T-1"));
    }

    #[tokio::test]
    async fn check_agents_rejects_mismatched_task_id_override() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx, "planner").await;

        let err = ToolTaskCheckAgents::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("task_id", json!("task-2"))]),
            )
            .await
            .unwrap_err();

        assert_eq!(
            err,
            "task_id override is not allowed from this planner chat"
        );
    }

    #[tokio::test]
    async fn wait_for_agents_rejects_mismatched_task_id_override() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx, "planner").await;

        let err = ToolTaskWaitForAgents::new()
            .tool_execute(
                ccx,
                &"call".to_string(),
                &args(&[("task_id", json!("task-2"))]),
            )
            .await
            .unwrap_err();

        assert_eq!(
            err,
            "task_id override is not allowed from this planner chat"
        );
    }

    #[tokio::test]
    async fn check_agents_non_planner_role_rejected() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = planner_ccx(gcx, "agents").await;

        let err = ToolTaskCheckAgents::new()
            .tool_execute(ccx, &"call".to_string(), &HashMap::new())
            .await
            .unwrap_err();

        assert!(err.contains("task planner"));
    }

    #[tokio::test]
    async fn check_agents_allows_same_task_id() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = write_empty_task(temp.path()).await;
        let ccx = planner_ccx(gcx, "planner").await;

        let output = tool_output_text(
            ToolTaskCheckAgents::new()
                .tool_execute(
                    ccx,
                    &"call".to_string(),
                    &args(&[("task_id", json!("task-1"))]),
                )
                .await
                .unwrap(),
        );

        assert!(output.contains("No agents have been spawned yet for this task"));
    }

    #[test]
    fn detail_format_shows_room_members_for_room_cards() {
        let mut status = status("T-1", "P0", "doing", Some(SessionState::Generating), 3);
        status.room_members = vec![
            TeamMember {
                role: "coder".to_string(),
                agent_chat_id: Some("agent-T-1-coder".to_string()),
                member_status: Some(refact_tasks::types::TeamStatus::Running),
                ..Default::default()
            },
            TeamMember {
                role: "reviewer".to_string(),
                agent_chat_id: Some("agent-T-1-reviewer".to_string()),
                member_status: Some(refact_tasks::types::TeamStatus::Running),
                ..Default::default()
            },
        ];
        let output =
            format_agent_statuses_at(&[status], &query(AgentReportFormat::Detail), now()).unwrap();

        assert!(output.contains("**Room members:**"));
        assert!(output.contains("coder \u{2014} `agent-T-1-coder` (running)"));
        assert!(output.contains("reviewer \u{2014} `agent-T-1-reviewer` (running)"));
    }

    #[test]
    fn detail_format_hides_room_members_for_single_agent_cards() {
        let status = status("T-1", "P0", "doing", Some(SessionState::Generating), 3);
        let output =
            format_agent_statuses_at(&[status], &query(AgentReportFormat::Detail), now()).unwrap();

        assert!(!output.contains("**Room members:**"));
    }

    #[test]
    fn compact_format_does_not_show_room_members() {
        let mut status = status("T-1", "P0", "doing", Some(SessionState::Generating), 3);
        status.room_members = vec![
            TeamMember {
                role: "coder".to_string(),
                ..Default::default()
            },
            TeamMember {
                role: "reviewer".to_string(),
                ..Default::default()
            },
        ];
        let output =
            format_agent_statuses_at(&[status], &query(AgentReportFormat::Compact), now()).unwrap();

        assert!(!output.contains("**Room members:**"));
    }
}
