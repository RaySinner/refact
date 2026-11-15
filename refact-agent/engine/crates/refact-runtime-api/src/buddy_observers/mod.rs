//! Buddy observers whose only engine-shaped dependency is `AppState`.
//!
//! `BuddyObserver::observe` takes `AppState` (defined here) plus
//! `ObserverContext`/`BuddyFact`/`BuddySettings` (already in `refact-buddy-core`),
//! so the trait and every observer that needs nothing else can live in this
//! crate. Observers still bound to engine-only modules (`task_health` on
//! `chat::task_agent_monitor`, `mcp_auth` on `integrations::mcp`) remain in the
//! engine.

use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::types::BuddyFact;

use crate::app_state::AppState;

pub mod chat_pattern;
pub mod customization_drift;
pub mod diagnostic_cluster;
pub mod git_pressure;
pub mod memory_garden;
pub mod provider_health;
pub mod trajectory_clutter;
pub mod worktree_hygiene;

pub use refact_buddy_core::observers::{Ephemeral, ObserverContext};

#[async_trait::async_trait]
pub trait BuddyObserver: Send + Sync {
    fn id(&self) -> &'static str;
    fn cadence_seconds(&self) -> u64;
    fn emission_refresh_ttl_seconds(&self) -> u64 {
        0
    }
    fn requires_setting(&self, settings: &BuddySettings) -> bool;
    async fn observe(&self, gcx: AppState, ctx: &ObserverContext) -> Vec<BuddyFact>;
}
