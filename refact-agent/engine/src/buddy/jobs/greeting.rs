pub use refact_runtime_api::buddy_jobs::greeting::*;

#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::buddy::scheduler::BuddyJob;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::scheduler::BuddyJobContext;
    use crate::buddy::settings::BuddySettings;
    use crate::buddy::types::{
        BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse, BuddySuggestion,
    };

    fn test_context() -> BuddyJobContext {
        BuddyJobContext {
            identity_name: "Pixel".to_string(),
            personality: Default::default(),
            onboarding: BuddyOnboarding::default(),
            recent_diagnostics: vec![],
            project_root: std::path::PathBuf::from("/tmp/project"),
            job_state: BuddyJobState::default(),
            workflow_summaries: vec![],
            total_workflow_runs: 0,
            suggestion_state: Vec::<BuddySuggestion>::new(),
            pet: BuddyPetState::default(),
            active_quest: None,
            settings: BuddySettings::default(),
            pulse: BuddyPulse::default(),
            facts: vec![],
            recent_activities: vec![],
        }
    }

    async fn make_gcx_with_buddy() -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let mut state = crate::buddy::state::default_buddy_state();
        state.identity.name = "Pixel".to_string();
        let service = crate::buddy::actor::BuddyService::new(
            std::env::temp_dir().join(format!(
                "buddy-greeting-voice-test-{}",
                uuid::Uuid::new_v4()
            )),
            state,
            BuddySettings::default(),
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

    #[tokio::test]
    async fn greeting_uses_voice_service_when_available() {
        let (service, renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
            vec![Some("voice hello".to_string())],
        );
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let gcx = make_gcx_with_buddy().await;
        let job = GreetingJob;

        let result = job.execute(gcx, test_context()).await;

        let speech = result.speech.unwrap();
        assert_eq!(speech.text, "voice hello");
        assert_eq!(speech.dedupe_key.as_deref(), Some("greeting"));
        assert!(result.mark_greeted);
        assert_eq!(renderer.intent_kinds(), vec!["speech:greeting".to_string()]);
    }
}
