use std::sync::OnceLock;
use std::time::Instant;

use refact_ext::skills::SkillIndex;
use refact_ext::slash_commands::SlashCommand;

pub struct SlashCacheEntry {
    pub commands: Vec<SlashCommand>,
    pub skills: Vec<SkillIndex>,
    pub loaded_at: Instant,
    pub generation: u64,
}

fn slash_cache() -> &'static tokio::sync::RwLock<Option<SlashCacheEntry>> {
    static SLASH_CACHE: OnceLock<tokio::sync::RwLock<Option<SlashCacheEntry>>> = OnceLock::new();
    SLASH_CACHE.get_or_init(|| tokio::sync::RwLock::new(None))
}

pub async fn invalidate_slash_cache() {
    *slash_cache().write().await = None;
}

pub async fn cached_slash_commands() -> Option<(Vec<SlashCommand>, Vec<SkillIndex>, Instant, u64)> {
    let read = slash_cache().read().await;
    read.as_ref().map(|entry| {
        (
            entry.commands.clone(),
            entry.skills.clone(),
            entry.loaded_at,
            entry.generation,
        )
    })
}

pub async fn store_slash_cache(
    commands: Vec<SlashCommand>,
    skills: Vec<SkillIndex>,
    loaded_at: Instant,
    generation: u64,
) {
    *slash_cache().write().await = Some(SlashCacheEntry {
        commands,
        skills,
        loaded_at,
        generation,
    });
}
