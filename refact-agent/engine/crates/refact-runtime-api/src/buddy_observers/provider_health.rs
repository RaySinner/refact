use crate::app_state::AppState;
use crate::buddy_observers::{BuddyObserver, ObserverContext};
use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::types::BuddyFact;

pub use refact_buddy_core::observers::provider_health::*;

pub struct ProviderHealthObserver;

#[async_trait::async_trait]
impl BuddyObserver for ProviderHealthObserver {
    fn id(&self) -> &'static str {
        "provider_health"
    }

    fn cadence_seconds(&self) -> u64 {
        300
    }

    fn requires_setting(&self, settings: &BuddySettings) -> bool {
        settings.observers.provider_health && settings.proactive_enabled
    }

    async fn observe(&self, gcx: AppState, _ctx: &ObserverContext) -> Vec<BuddyFact> {
        let caps_state = gcx.model.caps.read().await;
        let caps = match &caps_state.caps {
            Some(c) => c.clone(),
            None => return vec![],
        };
        let chat_models: Vec<String> = caps.chat_models.keys().cloned().collect();
        let completion_models: Vec<String> = caps.completion_models.keys().cloned().collect();
        detect_provider_health_facts(&caps.defaults, &chat_models, &completion_models, chrono::Utc::now())
    }
}
