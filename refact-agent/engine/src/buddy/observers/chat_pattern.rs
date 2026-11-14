pub use refact_runtime_api::buddy_observers::chat_pattern::*;

#[cfg(test)]
pub fn run_chat_pattern_observer_sync(
    messages: &[crate::call_validation::ChatMessage],
    chat_id: &str,
) -> Vec<crate::buddy::types::BuddyFact> {
    detect_chat_pattern_facts(messages, chat_id, chrono::Utc::now())
}
