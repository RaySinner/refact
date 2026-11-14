use crate::buddy::settings::BuddySettings;
use crate::buddy::types::BuddyFact;
use crate::app_state::AppState;

use super::{BuddyObserver, ObserverContext};

pub struct WorktreeHygieneObserver;

pub use refact_buddy_core::observers::worktree_hygiene::*;

#[async_trait::async_trait]
impl BuddyObserver for WorktreeHygieneObserver {
    fn id(&self) -> &'static str {
        "worktree_hygiene"
    }

    fn cadence_seconds(&self) -> u64 {
        300
    }

    fn requires_setting(&self, settings: &BuddySettings) -> bool {
        settings.observers.git_pressure
    }

    async fn observe(&self, gcx: AppState, ctx: &ObserverContext) -> Vec<BuddyFact> {
        let cache_dir = gcx.paths.cache_dir.clone();
        let Ok(service) = crate::worktrees::service::WorktreeService::new_async(
            cache_dir,
            ctx.project_root.clone(),
        )
        .await
        else {
            return Vec::new();
        };
        let Ok(inventory) = service.inspect_worktrees().await else {
            return Vec::new();
        };
        detect_worktree_hygiene_facts(inventory, ctx.now)
    }
}
