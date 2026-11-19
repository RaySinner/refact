pub use refact_runtime_api::buddy_jobs::tour::*;

#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::buddy::scheduler::BuddyJob;
#[cfg(test)]
use crate::buddy::types::BuddySpeechItem;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::scheduler::BuddyJobContext;
    use crate::buddy::settings::{BuddySettings, QuietHoursMode};
    use crate::buddy::types::{BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse};

    fn test_context() -> BuddyJobContext {
        BuddyJobContext {
            identity_name: "Pixel".to_string(),
            personality: Default::default(),
            onboarding: BuddyOnboarding {
                greeted: true,
                tour_completed: false,
                ..Default::default()
            },
            recent_diagnostics: vec![],
            project_root: std::path::PathBuf::from("/tmp/project"),
            job_state: BuddyJobState::default(),
            workflow_summaries: vec![],
            total_workflow_runs: 0,
            suggestion_state: vec![],
            pet: BuddyPetState::default(),
            active_quest: None,
            settings: BuddySettings::default(),
            pulse: BuddyPulse::default(),
            facts: vec![],
            recent_activities: vec![],
        }
    }

    async fn make_gcx_with_quiet_buddy() -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let mut settings = BuddySettings::default();
        settings.quiet_hours_mode = QuietHoursMode::Fixed;
        let hour = chrono::Timelike::hour(&chrono::Local::now());
        settings.quiet_hours_start = hour as u8;
        settings.quiet_hours_end = ((hour + 1) % 24) as u8;
        let mut state = crate::buddy::state::default_buddy_state();
        state.onboarding.greeted = true;
        let service = crate::buddy::actor::BuddyService::new(
            std::env::temp_dir().join(format!("buddy-tour-drop-test-{}", uuid::Uuid::new_v4())),
            state,
            settings,
            Vec::new(),
            crate::buddy::runtime_queue::RuntimeQueue::new(),
            tx,
            None,
        );
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let buddy_arc = app.buddy.buddy.clone();
        *buddy_arc.lock().await = Some(service);
        app
    }

    #[test]
    fn tour_speech_preserves_fallback_fields_after_voice_render() {
        let fallback = tour_fallback_speech("fallback".to_string());
        let voice = BuddySpeechItem {
            id: "voice-id".to_string(),
            text: "voice text".to_string(),
            mood: "curious".to_string(),
            scope: "chat".to_string(),
            persistent: true,
            ttl_seconds: 999,
            dedupe_key: Some("voice".to_string()),
            created_at: "voice-created".to_string(),
            controls: vec![crate::buddy::types::BuddyControl {
                id: "voice-control".to_string(),
                label: "Voice".to_string(),
                action: "voice".to_string(),
                action_param: None,
                style: "primary".to_string(),
            }],
            chat_id: Some("chat".to_string()),
            speech_intent: None,
        };

        let normalized = normalize_tour_speech(fallback.clone(), voice);

        assert_eq!(normalized.text, "voice text");
        assert_eq!(normalized.mood, "curious");
        assert_eq!(normalized.id, fallback.id);
        assert_eq!(normalized.scope, fallback.scope);
        assert_eq!(normalized.persistent, fallback.persistent);
        assert_eq!(normalized.ttl_seconds, fallback.ttl_seconds);
        assert_eq!(normalized.dedupe_key, fallback.dedupe_key);
        assert_eq!(normalized.created_at, fallback.created_at);
        assert_eq!(normalized.controls.len(), fallback.controls.len());
        assert_eq!(normalized.chat_id, fallback.chat_id);
    }

    #[test]
    fn tour_speech_preserves_fallback_text_and_mood_when_voice_text_empty() {
        let fallback = tour_fallback_speech("fallback".to_string());
        let voice = BuddySpeechItem {
            id: "voice-id".to_string(),
            text: "   ".to_string(),
            mood: "curious".to_string(),
            scope: "chat".to_string(),
            persistent: true,
            ttl_seconds: 999,
            dedupe_key: Some("voice".to_string()),
            created_at: "voice-created".to_string(),
            controls: vec![],
            chat_id: Some("chat".to_string()),
            speech_intent: None,
        };

        let normalized = normalize_tour_speech(fallback.clone(), voice);

        assert_eq!(normalized.text, "fallback");
        assert_eq!(normalized.mood, fallback.mood);
        assert_eq!(normalized.id, fallback.id);
    }

    #[tokio::test]
    async fn dropped_tour_speech_does_not_return_recordable_completion() {
        let gcx = make_gcx_with_quiet_buddy().await;
        let job = TourJob;

        let result = job.execute(gcx.clone(), test_context()).await;

        assert!(result.last_result.is_none());
        assert!(result.speech.is_none());
        assert!(result.speech_intent.is_none());
        assert!(!job.records_empty_result());
        let buddy = gcx.buddy.buddy.lock().await;
        let service = buddy.as_ref().unwrap();
        assert!(service.active_speech.is_none());
        assert!(service.state.job_cooldowns.get("tour").is_none());
    }
}
