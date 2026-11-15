use chrono::{DateTime, Utc};

use crate::buddy_memory_lifecycle::{
    detect_git_memory_ops, load_memory_doc_snapshots_from_knowledge_dirs, MemoryLifecycleOp,
    MemoryOpType,
};
use crate::buddy_observers::{BuddyObserver, ObserverContext};
use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::types::BuddyFact;
use crate::app_state::AppState;
use refact_git::operations::{mine_git_history, GitHistoryOptions, GitHistoryReport};

pub struct GitPressureObserver;

pub(crate) const MAX_GIT_MEMORY_HISTORY_COMMITS: usize = 120;
pub(crate) const MAX_GIT_MEMORY_OPS_PER_SCAN: usize = 40;

pub use refact_buddy_core::observers::git_pressure::*;

fn empty_git_history_report() -> GitHistoryReport {
    GitHistoryReport {
        commits: Vec::new(),
        cochanges: Vec::new(),
        hotspots: Vec::new(),
        commit_cap_hit: false,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub async fn detect_and_enqueue_git_memory_ops(
    project_root: &std::path::Path,
    now: DateTime<Utc>,
) -> Vec<MemoryLifecycleOp> {
    let root = project_root.to_path_buf();
    let mined = tokio::task::spawn_blocking(move || {
        let report = match mine_git_history(
            &root,
            GitHistoryOptions {
                max_commits: MAX_GIT_MEMORY_HISTORY_COMMITS,
                cochange_threshold: 3,
                max_hotspots: 20,
                max_cochange_pairs: 50,
                ..GitHistoryOptions::default()
            },
        ) {
            Ok(report) => report,
            Err(err) => {
                tracing::debug!("git_pressure: git memory mining skipped: {}", err);
                return (root, empty_git_history_report());
            }
        };
        (root, report)
    })
    .await
    .unwrap_or_else(|_| (project_root.to_path_buf(), empty_git_history_report()));
    let (root, report) = mined;
    if report.commits.is_empty() {
        return Vec::new();
    }
    let knowledge_dirs = vec![root.join(refact_files::file_filter::KNOWLEDGE_FOLDER_NAME)]
        .into_iter()
        .filter(|dir| dir.exists())
        .collect::<Vec<_>>();
    let docs = load_memory_doc_snapshots_from_knowledge_dirs(&knowledge_dirs).await;
    let mut ops = tokio::task::spawn_blocking(move || detect_git_memory_ops(&report, &docs, now))
        .await
        .unwrap_or_default();
    ops.truncate(MAX_GIT_MEMORY_OPS_PER_SCAN);
    sort_git_memory_ops_for_enqueue(&mut ops);
    let mut enqueued = Vec::new();
    for op in ops.clone() {
        let op_id = op.op_id.clone();
        match refact_buddy_core::storage::enqueue_memory_op(project_root, op).await {
            Ok(updated) => {
                if let Some(saved) = updated.ops.iter().find(|saved| saved.op_id == op_id) {
                    enqueued.push(saved.clone());
                }
            }
            Err(err) => tracing::warn!("buddy: failed to enqueue git memory op: {}", err),
        }
    }
    enqueued
}

fn sort_git_memory_ops_for_enqueue(ops: &mut [MemoryLifecycleOp]) {
    ops.sort_by(|a, b| {
        git_memory_enqueue_priority(a)
            .cmp(&git_memory_enqueue_priority(b))
            .then_with(|| a.idempotency_key.cmp(&b.idempotency_key))
            .then_with(|| a.op_id.cmp(&b.op_id))
    });
}

fn git_memory_enqueue_priority(op: &MemoryLifecycleOp) -> u8 {
    match op.op_type {
        MemoryOpType::RepairLinks => 0,
        MemoryOpType::MarkStale => 1,
        MemoryOpType::MarkReviewNeeded => 2,
        MemoryOpType::CreateMemory => 3,
        _ => 4,
    }
}

#[async_trait::async_trait]
impl BuddyObserver for GitPressureObserver {
    fn id(&self) -> &'static str {
        "git_pressure"
    }

    fn cadence_seconds(&self) -> u64 {
        300
    }

    fn requires_setting(&self, settings: &BuddySettings) -> bool {
        settings.observers.git_pressure
    }

    async fn observe(&self, gcx: AppState, ctx: &ObserverContext) -> Vec<BuddyFact> {
        let root = ctx.project_root.clone();
        let now = ctx.now;
        let facts_root = root.clone();
        let facts =
            tokio::task::spawn_blocking(move || detect_git_pressure_facts(&facts_root, now))
                .await
                .unwrap_or_default();
        let git_ops = detect_and_enqueue_git_memory_ops(&root, now).await;
        if !git_ops.is_empty() {
            let buddy_arc = gcx.buddy.buddy.clone();
            let updated_memory_ops = refact_buddy_core::storage::load_memory_ops(&root).await;
            let mut buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_mut() {
                svc.memory_ops = updated_memory_ops;
            }
        }
        facts
    }
}

