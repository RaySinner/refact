use std::path::Path;

use tokio::fs;
use tracing::warn;

pub use refact_buddy_core::state::*;
use refact_buddy_core::types::BuddyState;

pub async fn load_state(project_root: &Path) -> BuddyState {
    let path = project_root.join(".refact/buddy/state.json");
    match fs::read_to_string(&path).await {
        Ok(content) => match serde_json::from_str::<BuddyState>(&content) {
            Ok(mut state) => {
                sync_state(&mut state);
                state
            }
            Err(e) => {
                warn!("Failed to parse buddy state: {}, using defaults", e);
                default_buddy_state()
            }
        },
        Err(_) => default_buddy_state(),
    }
}

pub async fn save_state(project_root: &Path, state: &BuddyState) -> Result<(), String> {
    let path = project_root.join(".refact/buddy/state.json");
    super::storage::atomic_write_json(&path, state).await
}
