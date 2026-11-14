use chrono::{DateTime, Utc};
use serde_json::json;

use crate::types::{BuddyFact, BuddyFactKind};
use refact_worktrees::types::WorktreeInventory;

pub fn detect_worktree_hygiene_facts(
    inventory: WorktreeInventory,
    now: DateTime<Utc>,
) -> Vec<BuddyFact> {
    if inventory.summary.total == 0 {
        return Vec::new();
    }
    let candidate_ids = inventory.cleanup_candidates.clone();
    vec![BuddyFact {
        kind: BuddyFactKind::WorktreeHygiene,
        key: format!("worktree_hygiene:{}", inventory.project_hash),
        source: "worktree_hygiene",
        payload: json!({
            "project_hash": inventory.project_hash,
            "summary": inventory.summary,
            "cleanup_candidates": candidate_ids,
            "worktrees": inventory.worktrees,
        }),
        seen_at: now,
        confidence: 0.95,
    }]
}
