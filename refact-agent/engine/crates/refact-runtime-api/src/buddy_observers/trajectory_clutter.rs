use crate::app_state::AppState;
use crate::buddy_observers::{BuddyObserver, ObserverContext};
use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::types::BuddyFact;

pub use refact_buddy_core::observers::trajectory_clutter::*;

pub struct TrajectoryClutterObserver;

#[async_trait::async_trait]
impl BuddyObserver for TrajectoryClutterObserver {
    fn id(&self) -> &'static str {
        "trajectory_clutter"
    }

    fn cadence_seconds(&self) -> u64 {
        300
    }

    fn requires_setting(&self, settings: &BuddySettings) -> bool {
        settings.observers.trajectory_clutter
    }

    async fn observe(&self, gcx: AppState, ctx: &ObserverContext) -> Vec<BuddyFact> {
        let traj_dir = ctx.project_root.join(".refact").join("trajectories");
        if !traj_dir.exists() {
            return vec![];
        }
        let hash = path_hash(&ctx.project_root);
        let (total, untitled, oldest) = scan_trajectories_dir(&traj_dir).await;
        let _ = gcx;
        detect_trajectory_clutter_facts(&hash, total, untitled, oldest, ctx.now)
    }
}
