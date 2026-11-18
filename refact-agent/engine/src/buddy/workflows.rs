pub use refact_buddy_core::workflows::*;
pub use refact_runtime_api::buddy_workflows::*;

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_for_workflow_side_effects() {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    async fn wait_for_path(path: &std::path::Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if tokio::fs::try_exists(path).await.unwrap_or(false) {
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn workflow_wrapper_rejects_invalid_id_before_event_or_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        *app.buddy.buddy.lock().await = Some(crate::buddy::actor::BuddyService::new(
            dir.path().to_path_buf(),
            crate::buddy::state::default_buddy_state(),
            crate::buddy::settings::BuddySettings::default(),
            Vec::new(),
            crate::buddy::runtime_queue::RuntimeQueue::new(),
            tx,
            None,
        ));

        let result = buddy_wrap_workflow(
            app.clone(),
            "../bad",
            "⚙️",
            1,
            |_| "ok".to_string(),
            || async { Ok::<_, String>(()) },
        )
        .await;

        assert_eq!(result.unwrap_err(), "invalid workflow_id: ../bad");
        let lock = app.buddy.buddy.lock().await;
        let svc = lock.as_ref().unwrap();
        assert!(svc.runtime_queue.items.is_empty());
        assert!(svc.state.recent_activities.is_empty());
        assert!(svc.state.workflow_summaries.is_empty());
        assert!(rx.try_recv().is_err());
        assert!(
            !tokio::fs::try_exists(dir.path().join(".refact/buddy/chats/workflows/bad.json"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn workflow_wrapper_failure_fallback_transcript_uses_report_not_raw_error() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }
        *app.buddy.buddy.lock().await = None;
        let raw_error = "OpenAI 404: model secret-model-not-found token=rawsecret not found";

        let result = buddy_wrap_workflow(
            app.clone(),
            "commit_msg",
            "⚙️",
            1,
            |_| "ok".to_string(),
            || async move { Err::<(), _>(raw_error.to_string()) },
        )
        .await;

        assert_eq!(result.unwrap_err(), raw_error);
        wait_for_workflow_side_effects().await;
        let path = dir
            .path()
            .join(".refact/buddy/chats/workflows/commit_msg.json");
        wait_for_path(&path).await;
        let value: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(path).await.unwrap()).unwrap();
        let entry = &value["entries"][0];
        let output = entry["output_summary"].as_str().unwrap();
        let summary = entry["failure_summary"].as_str().unwrap();
        assert_eq!(entry["success"], false);
        assert_eq!(entry["failure_category"], "model_unavailable");
        assert!(summary.contains("Model unavailable"));
        assert!(output.contains("[REDACTED"));
        assert!(!output.contains("rawsecret"));
        assert!(!summary.contains("rawsecret"));
    }

    #[tokio::test]
    async fn workflow_wrapper_failure_has_single_durable_failure_runtime_surface() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        {
            *app.workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        *app.buddy.buddy.lock().await = Some(crate::buddy::actor::BuddyService::new(
            dir.path().to_path_buf(),
            crate::buddy::state::default_buddy_state(),
            crate::buddy::settings::BuddySettings::default(),
            Vec::new(),
            crate::buddy::runtime_queue::RuntimeQueue::new(),
            tx,
            None,
        ));

        let result = buddy_wrap_workflow(
            app.clone(),
            "commit_msg",
            "⚙️",
            1,
            |_| "ok".to_string(),
            || async {
                Err::<(), _>("OpenAI 404: model refact/gpt-4.1-nano not found".to_string())
            },
        )
        .await;

        assert!(result.is_err());
        wait_for_workflow_side_effects().await;
        let lock = app.buddy.buddy.lock().await;
        let svc = lock.as_ref().unwrap();
        let workflow_events = svc
            .runtime_queue
            .items
            .iter()
            .filter(|event| event.dedupe_key.as_deref() == Some("workflow_commit_msg"))
            .collect::<Vec<_>>();
        assert_eq!(workflow_events.len(), 1);
        assert_eq!(workflow_events[0].status, "completed");
        assert!(workflow_events[0].failure_category.is_none());
        let failure_events = svc
            .runtime_queue
            .items
            .iter()
            .filter(|event| {
                event
                    .dedupe_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with("workflow_failure:commit_msg:"))
            })
            .collect::<Vec<_>>();
        assert_eq!(failure_events.len(), 1);
        assert_eq!(failure_events[0].status, "failed");
        assert_eq!(
            failure_events[0].failure_category.as_deref(),
            Some("model_unavailable")
        );
        assert!(failure_events[0].persistent);
    }
}
