use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;

use crate::app_state::AppState;
use crate::buddy::observers::{BuddyObserver, ObserverContext};
use crate::buddy::settings::BuddySettings;
use crate::buddy::types::{BuddyFact, BuddyJobState};
use crate::file_filter::KNOWLEDGE_FOLDER_NAME;

pub use refact_buddy_core::observers::memory_garden::*;

pub struct MemoryGardenObserver;

const MEMORY_GARDEN_DAILY_COUNTER_JOB_ID: &str = "memory_garden";

async fn memory_garden_job_state(gcx: AppState) -> BuddyJobState {
    let buddy_arc = gcx.buddy.buddy.clone();
    let buddy = buddy_arc.lock().await;
    buddy
        .as_ref()
        .and_then(|svc| {
            svc.state
                .job_cooldowns
                .get(MEMORY_GARDEN_DAILY_COUNTER_JOB_ID)
                .cloned()
        })
        .unwrap_or_default()
}

async fn persist_memory_garden_daily_counter(gcx: AppState, counter: &MemoryGardenDailyCounter) {
    let Ok(last_result) = serde_json::to_string(counter) else {
        return;
    };
    let buddy_arc = gcx.buddy.buddy.clone();
    let mut buddy = buddy_arc.lock().await;
    if let Some(svc) = buddy.as_mut() {
        let state = svc
            .state
            .job_cooldowns
            .entry(MEMORY_GARDEN_DAILY_COUNTER_JOB_ID.to_string())
            .or_default();
        state.last_result = Some(last_result);
        svc.dirty = true;
    }
}

async fn knowledge_dirs(gcx: AppState) -> Vec<PathBuf> {
    let project_dirs = crate::files_correction::get_project_dirs(gcx.gcx.clone()).await;
    let mut dirs: Vec<PathBuf> = project_dirs
        .iter()
        .map(|d| d.join(KNOWLEDGE_FOLDER_NAME))
        .filter(|d| d.exists())
        .collect();
    let global_dir = gcx.paths.config_dir.join("knowledge");
    if global_dir.exists() {
        dirs.push(global_dir);
    }
    dirs
}

async fn detect_memory_garden(gcx: AppState, now: DateTime<Utc>) -> Vec<BuddyFact> {
    let dirs = knowledge_dirs(gcx).await;
    let entries = scan_knowledge_dirs_from_paths(dirs.clone()).await;
    let references = scan_knowledge_references(&dirs);
    memory_garden_facts_from_entries_with_references(&entries, &references, now)
}

#[async_trait::async_trait]
impl BuddyObserver for MemoryGardenObserver {
    fn id(&self) -> &'static str {
        "memory_garden"
    }

    fn cadence_seconds(&self) -> u64 {
        600
    }

    fn requires_setting(&self, settings: &BuddySettings) -> bool {
        settings.observers.memory_garden
            && settings.housekeeping_enabled
            && settings.proactive_enabled
    }

    async fn observe(&self, gcx: AppState, ctx: &ObserverContext) -> Vec<BuddyFact> {
        let facts = detect_memory_garden(gcx.clone(), ctx.now).await;
        let job_state = memory_garden_job_state(gcx.clone()).await;
        let (facts, counter) = transition_memory_garden_facts(facts, &job_state, ctx.now);
        if let Some(counter) = counter {
            persist_memory_garden_daily_counter(gcx, &counter).await;
        }
        facts
    }
}

#[cfg(test)]
pub(crate) async fn scan_knowledge_dir_count_for_test(dir: PathBuf) -> usize {
    scan_knowledge_dirs_from_paths(vec![dir]).await.len()
}
