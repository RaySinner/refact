use std::sync::Arc;

pub mod chat_pattern;
pub mod customization_drift;
pub mod diagnostic_cluster;
pub mod git_pressure;
pub mod mcp_auth;
pub mod memory_garden;
pub mod privacy_degraded;
pub mod provider_health;
pub mod task_health;
pub mod trajectory_clutter;
pub mod worktree_hygiene;

use crate::buddy::settings::BuddySettings;
use crate::buddy::types::BuddyFact;
use crate::app_state::AppState;

pub use refact_runtime_api::buddy_observers::{BuddyObserver, Ephemeral, ObserverContext};

pub fn build_observer_registry() -> Vec<Arc<dyn BuddyObserver>> {
    vec![
        Arc::new(task_health::TaskHealthObserver),
        Arc::new(trajectory_clutter::TrajectoryClutterObserver),
        Arc::new(git_pressure::GitPressureObserver),
        Arc::new(worktree_hygiene::WorktreeHygieneObserver),
        Arc::new(diagnostic_cluster::DiagnosticClusterObserver),
        Arc::new(provider_health::ProviderHealthObserver),
        Arc::new(mcp_auth::McpAuthObserver),
        Arc::new(chat_pattern::ChatPatternObserver),
        Arc::new(customization_drift::CustomizationDriftObserver),
        Arc::new(memory_garden::MemoryGardenObserver),
    ]
}
