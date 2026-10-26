use std::path::Path;

pub use refact_buddy_core::storage::*;

use super::memory_lifecycle::{apply_memory_lifecycle_op_status, MemoryOpsState};
use super::settings::AutonomyLevel;
use crate::app_state::AppState;

pub async fn drain_memory_ops(
    project_root: &Path,
    gcx: AppState,
    autonomy: AutonomyLevel,
    max_applies: usize,
) -> Result<(MemoryOpsState, usize), String> {
    drain_memory_ops_with(project_root, autonomy, max_applies, |working| {
        let gcx = gcx.clone();
        async move { apply_memory_lifecycle_op_status(gcx, &working).await }
    })
    .await
}

pub async fn apply_memory_batch(
    project_root: &Path,
    gcx: AppState,
    batch_key: &str,
    max_applies: usize,
) -> Result<(MemoryOpsState, usize, usize, usize), String> {
    apply_memory_batch_with(project_root, batch_key, max_applies, |working| {
        let gcx = gcx.clone();
        async move { apply_memory_lifecycle_op_status(gcx, &working).await }
    })
    .await
}

#[cfg_attr(not(test), allow(dead_code))]
pub async fn apply_artifact_decisions(
    project_root: &Path,
    gcx: AppState,
    decisions: &[(String, bool)],
) -> Result<(MemoryOpsState, usize, usize), String> {
    apply_artifact_decisions_with(project_root, decisions, |working| {
        let gcx = gcx.clone();
        async move { apply_memory_lifecycle_op_status(gcx, &working).await }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::buddy::memory_lifecycle::{
        MemoryCreatePayload, MemoryLifecycleOp, MemoryLifecyclePayload, MemoryOpStatus,
        MemoryOpType, MEMORY_OP_EXACT_DUPLICATE_REASON,
    };

    fn test_op(op_id: &str, evidence: &str, status: MemoryOpStatus) -> MemoryLifecycleOp {
        let mut op = MemoryLifecycleOp::pending(
            op_id,
            MemorySource::MemoryGarden,
            MemoryOpType::CreateMemory,
            vec![".refact/knowledge/item.md".to_string()],
            evidence,
            0.91,
            Utc::now().to_rfc3339(),
        );
        op.status = status;
        op
    }

    async fn test_gcx_with_workspace(dir: &Path) -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *AppState::from_gcx(gcx.clone())
                .await
                .workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.to_path_buf()];
        }
        AppState::from_gcx(gcx).await
    }

    async fn write_test_memory(root: &Path, name: &str) -> String {
        let dir = root.join(crate::file_filter::KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(name);
        tokio::fs::write(
            &path,
            "---\ntitle: Test\ntags:\n- memory\n---\n\nExisting body\n",
        )
        .await
        .unwrap();
        path.to_string_lossy().to_string()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_approval_required_queue_apply_auto_applies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let gcx = test_gcx_with_workspace(root).await;
        let item_path = root.join(".refact").join("knowledge").join("item.md");
        tokio::fs::create_dir_all(item_path.parent().unwrap())
            .await
            .unwrap();
        let frontmatter = crate::knowledge_graph::kg_structs::KnowledgeFrontmatter {
            id: Some("item".to_string()),
            title: Some("item".to_string()),
            status: Some("active".to_string()),
            tags: vec!["memory".to_string()],
            kind: Some("domain".to_string()),
            ..Default::default()
        };
        tokio::fs::write(
            &item_path,
            format!("{}\n\nItem body", frontmatter.to_yaml()),
        )
        .await
        .unwrap();

        let op = test_op("op-archive", "archive", MemoryOpStatus::Pending);
        let mut op = MemoryLifecycleOp {
            op_type: MemoryOpType::Archive,
            requires_approval: true,
            target_paths: vec![item_path.to_string_lossy().to_string()],
            ..op
        };
        op.status = MemoryOpStatus::Pending;

        enqueue_memory_op(root, op.clone()).await.unwrap();
        let (state, changed) = drain_memory_ops(root, gcx, AutonomyLevel::Suggest, 100)
            .await
            .unwrap();

        assert_eq!(state.ops.len(), 1);
        assert_eq!(state.ops[0].status, MemoryOpStatus::Applied);
        assert_eq!(state.ops[0].error, None);
        assert_eq!(changed, 1);
        assert_eq!(state.pending_count, 0);
        assert_eq!(state.failed_count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drain_applies_approved_mark_review_needed_op() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let target = write_test_memory(root, "review.md").await;
        let gcx = test_gcx_with_workspace(root).await;
        let mut op = test_op("op-review", "review", MemoryOpStatus::Approved);
        op.op_type = MemoryOpType::MarkReviewNeeded;
        op.requires_approval = false;
        op.target_paths = vec![target];
        op.payload = MemoryLifecyclePayload {
            review_after: Some("2026-05-02".to_string()),
            ..Default::default()
        };

        enqueue_memory_op(root, op).await.unwrap();
        let (state, changed) = drain_memory_ops(root, gcx, AutonomyLevel::Suggest, 100)
            .await
            .unwrap();

        assert_eq!(changed, 1);
        assert_eq!(state.ops.len(), 1);
        assert!(matches!(
            state.ops[0].status,
            MemoryOpStatus::Applied | MemoryOpStatus::Skipped
        ));
        assert_eq!(state.pending_count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drain_picks_exact_duplicate_for_suggest_and_safe_auto() {
        let suggest_dir = tempfile::tempdir().unwrap();
        let suggest_root = suggest_dir.path();
        let suggest_gcx = test_gcx_with_workspace(suggest_root).await;
        let mut op = test_op(
            "op-auto-merge",
            &format!(
                "{}: canonical=a, superseded=b",
                MEMORY_OP_EXACT_DUPLICATE_REASON
            ),
            MemoryOpStatus::Pending,
        );
        op.op_type = MemoryOpType::MergeArchive;
        op.requires_approval = true;
        op.confidence = 0.94;
        op.payload = MemoryLifecyclePayload {
            canonical: Some(MemoryCreatePayload {
                content: "Canonical merged body with plenty of alphabetic text.".to_string(),
                ..Default::default()
            }),
            superseded_paths: vec!["knowledge/missing.md".to_string()],
            ..Default::default()
        };
        enqueue_memory_op(suggest_root, op.clone()).await.unwrap();

        let (suggest_state, suggest_changed) =
            drain_memory_ops(suggest_root, suggest_gcx, AutonomyLevel::Suggest, 100)
                .await
                .unwrap();

        assert_eq!(suggest_changed, 1);
        assert_ne!(suggest_state.ops[0].status, MemoryOpStatus::Pending);

        let safe_dir = tempfile::tempdir().unwrap();
        let safe_root = safe_dir.path();
        let safe_gcx = test_gcx_with_workspace(safe_root).await;
        enqueue_memory_op(safe_root, op).await.unwrap();

        let (safe_state, safe_changed) =
            drain_memory_ops(safe_root, safe_gcx, AutonomyLevel::SafeAuto, 100)
                .await
                .unwrap();

        assert_eq!(safe_changed, 1);
        assert_ne!(safe_state.ops[0].status, MemoryOpStatus::Pending);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apply_memory_batch_applies_only_matching_class() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let target = write_test_memory(root, "batchdoc.md").await;
        let gcx = test_gcx_with_workspace(root).await;

        let mut review_op = test_op("op-batch-review", "needs review", MemoryOpStatus::Pending);
        review_op.op_type = MemoryOpType::MarkReviewNeeded;
        review_op.requires_approval = true;
        review_op.target_paths = vec![target.clone()];
        review_op.payload = MemoryLifecyclePayload {
            review_after: Some("2026-05-02".to_string()),
            ..Default::default()
        };
        let mut archive_op = test_op("op-batch-archive", "stale doc", MemoryOpStatus::Pending);
        archive_op.op_type = MemoryOpType::ArchiveCandidate;
        archive_op.requires_approval = true;
        archive_op.target_paths = vec![target];

        enqueue_memory_op(root, review_op).await.unwrap();
        enqueue_memory_op(root, archive_op).await.unwrap();

        let (state, applied, failed, remaining) =
            apply_memory_batch(root, gcx, "review", 100).await.unwrap();

        assert_eq!(applied, 1);
        assert_eq!(failed, 0);
        assert_eq!(remaining, 0);
        let review = state
            .ops
            .iter()
            .find(|op| op.op_id == "op-batch-review")
            .unwrap();
        assert!(matches!(
            review.status,
            MemoryOpStatus::Applied | MemoryOpStatus::Skipped
        ));
        let archive = state
            .ops
            .iter()
            .find(|op| op.op_id == "op-batch-archive")
            .unwrap();
        assert_eq!(archive.status, MemoryOpStatus::Pending);

        let reloaded = load_memory_ops(root).await;
        assert_eq!(reloaded.pending_count, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_batch_and_decisions_apply_op_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let target = write_test_memory(root, "race.md").await;
        let gcx = test_gcx_with_workspace(root).await;
        let mut op = test_op("op-race", "needs review", MemoryOpStatus::Pending);
        op.op_type = MemoryOpType::MarkReviewNeeded;
        op.requires_approval = true;
        op.target_paths = vec![target];
        op.payload = MemoryLifecyclePayload {
            review_after: Some("2026-05-02".to_string()),
            ..Default::default()
        };
        enqueue_memory_op(root, op).await.unwrap();

        let root_a = root.to_path_buf();
        let gcx_a = gcx.clone();
        let batch = tokio::spawn(async move {
            apply_memory_batch(&root_a, gcx_a, "review", 100)
                .await
                .unwrap()
        });
        let root_b = root.to_path_buf();
        let gcx_b = gcx.clone();
        let decisions = tokio::spawn(async move {
            apply_artifact_decisions(&root_b, gcx_b, &[("op-race".to_string(), true)])
                .await
                .unwrap()
        });
        let (_, batch_applied, batch_failed, _) = batch.await.unwrap();
        let (_, decided, _) = decisions.await.unwrap();

        assert!(
            batch_applied + batch_failed + decided <= 1,
            "op must be applied by at most one processor: batch={}/{} decisions={}",
            batch_applied,
            batch_failed,
            decided
        );
        let state = load_memory_ops(root).await;
        assert_eq!(state.ops.len(), 1);
    }
}
