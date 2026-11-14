use crate::app_state::AppState;
use crate::buddy_observers::{BuddyObserver, ObserverContext};
use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::types::BuddyFact;

pub use refact_buddy_core::observers::chat_pattern::*;

pub struct ChatPatternObserver;

#[async_trait::async_trait]
impl BuddyObserver for ChatPatternObserver {
    fn id(&self) -> &'static str {
        "chat_pattern"
    }

    fn cadence_seconds(&self) -> u64 {
        60
    }

    fn requires_setting(&self, settings: &BuddySettings) -> bool {
        settings.observers.chat_pattern
            && settings.message_observation_enabled
            && settings.proactive_enabled
    }

    async fn observe(&self, gcx: AppState, ctx: &ObserverContext) -> Vec<BuddyFact> {
        let sessions_map = gcx.chat.sessions.clone();
        let sessions_read = sessions_map.read().await;
        let mut facts = vec![];
        for (chat_id, session_arc) in sessions_read.iter() {
            if let Ok(session) = session_arc.try_lock() {
                // Run detection on borrowed slice — messages never leave this scope
                facts.extend(detect_chat_pattern_facts(
                    &session.messages,
                    chat_id,
                    ctx.now,
                ));
            }
        }
        facts
    }
}
