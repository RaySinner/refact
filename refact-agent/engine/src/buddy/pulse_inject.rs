pub use refact_runtime_api::buddy_pulse_inject::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::app_state::AppState;
    use crate::buddy::actor::BuddyService;
    use crate::buddy::runtime_queue::RuntimeQueue;
    use crate::buddy::settings::BuddySettings;
    use crate::call_validation::ChatContent;
    use refact_buddy_core::user_action::UserAction;
    use tokio::sync::broadcast;

    async fn make_gcx_with_buddy(project_root: &Path) -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (tx, _) = broadcast::channel(16);
        let service = BuddyService::new(
            project_root.to_path_buf(),
            crate::buddy::state::default_buddy_state(),
            BuddySettings::default(),
            Vec::new(),
            RuntimeQueue::new(),
            tx,
            None,
        );
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let buddy_arc = app.buddy.buddy.clone();
        *buddy_arc.lock().await = Some(service);
        app
    }

    #[tokio::test]
    async fn pulse_returns_none_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = make_gcx_with_buddy(dir.path()).await;
        let payload = build_buddy_pulse_payload(gcx).await;
        assert!(payload.is_none());
    }

    #[tokio::test]
    async fn pulse_excludes_low_confidence_prefs() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join(".refact/buddy/user_profile.md");
        tokio::fs::create_dir_all(profile.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &profile,
            "# User Profile\n\n## low\n- statement: \"Ignore me\"\n- confidence: 0.49\n- last_updated: 2026-05-14T09:00:00Z\n\n## high\n- statement: \"Keep me\"\n- confidence: 0.90\n- last_updated: 2026-05-14T10:00:00Z\n",
        )
        .await
        .unwrap();
        let gcx = make_gcx_with_buddy(dir.path()).await;
        let payload = build_buddy_pulse_payload(gcx).await.unwrap();
        assert_eq!(payload.preferences.len(), 1);
        assert_eq!(payload.preferences[0].statement, "Keep me");
    }

    #[tokio::test]
    async fn pulse_includes_user_activity_section_with_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = make_gcx_with_buddy(dir.path()).await;
        let user_activity = gcx.buddy.user_activity.clone();
        {
            let mut ring = user_activity.lock().await;
            ring.push(UserAction::FileOpened {
                path: "src/main.rs".to_string(),
                ts: chrono::Utc::now() - chrono::Duration::minutes(10),
            });
        }
        let payload = build_buddy_pulse_payload(gcx).await.unwrap();
        assert_eq!(payload.user_activity.grouped[0].action_type, "file_opened");
        assert!(payload
            .user_activity
            .time_of_day_pattern
            .starts_with("mostly active"));
    }

    #[tokio::test]
    async fn pulse_message_uses_correct_marker() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join(".refact/buddy/user_profile.md");
        tokio::fs::create_dir_all(profile.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &profile,
            "# User Profile\n\n## direct\n- statement: \"Prefers direct fixes\"\n- confidence: 0.90\n- last_updated: 2026-05-14T10:00:00Z\n",
        )
        .await
        .unwrap();
        let gcx = make_gcx_with_buddy(dir.path()).await;
        let message = build_buddy_pulse_message(gcx).await.unwrap();
        assert_eq!(message.role, "context_file");
        assert_eq!(message.tool_call_id, BUDDY_PULSE_MARKER);
        match message.content {
            ChatContent::ContextFiles(files) => {
                assert_eq!(files[0].file_name, "buddy_project_memory_pulse.md");
            }
            _ => panic!("expected context files"),
        }
    }

    #[tokio::test]
    async fn pulse_payload_serializes_in_extra_map() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join(".refact/buddy/user_profile.md");
        tokio::fs::create_dir_all(profile.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(
            &profile,
            "# User Profile\n\n## concise\n- statement: \"Likes concise summaries\"\n- confidence: 0.80\n- last_updated: 2026-05-14T10:00:00Z\n",
        )
        .await
        .unwrap();
        let gcx = make_gcx_with_buddy(dir.path()).await;
        let message = build_buddy_pulse_message(gcx).await.unwrap();
        let payload_value = message.extra.get("buddy_pulse_payload").unwrap();
        assert_eq!(
            payload_value["preferences"][0]["statement"],
            "Likes concise summaries"
        );
        assert!(serde_json::to_value(&message)
            .unwrap()
            .get("buddy_pulse_payload")
            .is_some());
    }
}
