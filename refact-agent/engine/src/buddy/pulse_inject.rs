use chrono::Utc;

use crate::app_state::AppState;
use crate::buddy::actor::redact_sensitive;
use crate::call_validation::ChatMessage;

pub use refact_buddy_core::pulse_inject::*;

async fn read_lessons(app: &AppState) -> Vec<PulseLesson> {
    let idx_arc = app.gcx.knowledge_index.clone();
    let cards = {
        let idx = idx_arc.lock().await;
        idx.lessons_for_pulse(5)
    };
    cards
        .into_iter()
        .map(|card| {
            let updated = card
                .updated
                .clone()
                .or_else(|| card.created_at.clone())
                .or_else(|| card.created.clone())
                .unwrap_or_default();
            let preview = card
                .description
                .clone()
                .or_else(|| card.summary.clone())
                .unwrap_or_default();
            PulseLesson {
                title: redact_sensitive(&card.title),
                preview: redact_sensitive(&preview),
                tags: card.tags.iter().map(|tag| redact_sensitive(tag)).collect(),
                updated: redact_sensitive(&updated),
            }
        })
        .collect()
}

async fn build_friction(gcx: AppState) -> PulseFriction {
    let buddy_arc = gcx.buddy.buddy.clone();
    let (diagnostics, pulse_diagnostic_types, stuck_tasks) = {
        let lock = buddy_arc.lock().await;
        let Some(service) = lock.as_ref() else {
            return PulseFriction::default();
        };
        (
            service
                .recent_diagnostics
                .iter()
                .map(|diag| (diag.error_type.clone(), diag.collected_at.clone()))
                .collect::<Vec<_>>(),
            service.pulse.diagnostics.top_error_types.clone(),
            service.pulse.tasks.recent_stuck_alert_count_1h(),
        )
    };
    pulse_friction_from_diagnostics(
        &diagnostics,
        &pulse_diagnostic_types,
        stuck_tasks,
        Utc::now(),
    )
}

async fn build_activity_section(gcx: AppState) -> PulseActivitySection {
    let user_activity = gcx.buddy.user_activity.clone();
    let actions = user_activity.lock().await.last_hours(24);
    build_activity_section_from_actions(&actions)
}

pub async fn build_buddy_pulse_payload(gcx: AppState) -> Option<BuddyPulsePayload> {
    let buddy_arc = gcx.buddy.buddy.clone();
    let project_root: std::path::PathBuf = {
        let lock = buddy_arc.lock().await;
        let service = lock.as_ref()?;
        service.project_root.clone()
    };

    let preferences = read_preferences(&project_root).await;
    let lessons = read_lessons(&gcx).await;
    let friction = build_friction(gcx.clone()).await;
    let recent_reports = build_recent_reports(&project_root).await;
    let user_activity = build_activity_section(gcx).await;

    let payload = BuddyPulsePayload {
        preferences,
        lessons,
        friction,
        recent_reports,
        user_activity,
        generated_at: redact_sensitive(&Utc::now().to_rfc3339()),
    };

    if payload_is_empty(&payload) {
        None
    } else {
        Some(payload)
    }
}

pub async fn build_buddy_pulse_message(gcx: AppState) -> Option<ChatMessage> {
    let payload = build_buddy_pulse_payload(gcx).await?;
    Some(pulse_message_from_payload(&payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
                ts: Utc::now() - chrono::Duration::minutes(10),
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
