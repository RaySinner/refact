pub use refact_agents_core::monitor::{monitor_once, run_background_agent_monitor};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use chrono::{TimeDelta, Utc};

    use crate::agents::types::{BgAgentKind, BgAgentStatus, CreateAgentRequest};

    #[tokio::test]
    async fn monitor_marks_old_running_record_with_progress() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let (record, _, _) = app
            .agents
            .create(CreateAgentRequest {
                parent_chat_id: "parent-monitor".to_string(),
                parent_root_chat_id: None,
                parent_tool_call_id: None,
                kind: BgAgentKind::Subagent,
                config_name: "subagent".to_string(),
                title: "title".to_string(),
                prompt: "prompt".to_string(),
                target_files: vec![],
                model: "model".to_string(),
                model_type: None,
                goal_summary: None,
                plan_present: false,
                worktree_id: None,
                worktree_branch: None,
            })
            .await
            .unwrap();
        let running = app
            .agents
            .mark_running(&record.agent_id, "missing-child".to_string())
            .await
            .unwrap();
        app.agents
            .set_last_update_at_for_test(&record.agent_id, Utc::now() - TimeDelta::minutes(25))
            .await
            .unwrap();
        let _ = running;

        monitor_once(app.clone()).await.unwrap();

        let updated = app
            .agents
            .get("parent-monitor", &record.agent_id)
            .await
            .unwrap();
        assert_eq!(updated.status, BgAgentStatus::Running);
        assert!(updated
            .progress
            .as_deref()
            .unwrap_or("")
            .contains("20+ minutes"));
    }

    #[tokio::test]
    async fn monitor_retries_cooled_deferred_completion() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let workspace = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let session = Arc::new(tokio::sync::Mutex::new(
            crate::chat::types::ChatSession::new("parent-monitor-deferred".to_string()),
        ));
        session
            .lock()
            .await
            .queue_processor_running
            .store(true, Ordering::SeqCst);
        app.chat
            .sessions
            .write()
            .await
            .insert("parent-monitor-deferred".to_string(), session.clone());
        let (record, _, _) = app
            .agents
            .create(CreateAgentRequest {
                parent_chat_id: "parent-monitor-deferred".to_string(),
                parent_root_chat_id: None,
                parent_tool_call_id: None,

                kind: BgAgentKind::Subagent,
                config_name: "subagent".to_string(),
                title: "title".to_string(),
                prompt: "prompt".to_string(),
                target_files: vec![],
                model: "model".to_string(),
                model_type: None,
                goal_summary: None,
                plan_present: false,
                worktree_id: None,
                worktree_branch: None,
            })
            .await
            .unwrap();
        app.agents
            .mark_completed(
                &record.agent_id,
                crate::agents::types::AgentCompletion {
                    result_summary: "done".to_string(),
                    edited_files: vec![],
                    diff_summary: None,
                    conflict_summary: None,
                    child_chat_id: Some("child-monitor-deferred".to_string()),
                },
            )
            .await
            .unwrap();
        app.agents
            .set_completion_message_id(&record.agent_id, "deferred".to_string())
            .await
            .unwrap();
        app.agents
            .set_deferred_at_for_test(&record.agent_id, Utc::now() - TimeDelta::seconds(11))
            .await
            .unwrap();

        monitor_once(app.clone()).await.unwrap();

        assert_eq!(session.lock().await.delivery_wake_sources.len(), 1);
        let updated = app
            .agents
            .get("parent-monitor-deferred", &record.agent_id)
            .await
            .unwrap();
        assert_ne!(updated.completion_message_id.as_deref(), Some("deferred"));
        assert!(updated.completion_pushed_at.is_some());
        assert!(updated.deferred_at.is_none());
    }
}
