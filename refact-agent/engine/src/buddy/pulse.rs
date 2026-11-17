pub use refact_runtime_api::buddy_pulse::*;

#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use crate::buddy::facts::FactStore;
#[cfg(test)]
use crate::buddy::types::{BuddyFactKind, CompetitorImportPulse, CustomizationPulse};
#[cfg(test)]
use crate::ext::competitor_import::types::ImportStatus;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::types::BuddyFact;
    use crate::buddy::memory_lifecycle::{
        MemoryLifecycleOp, MemoryOpStatus, MemoryOpType, MemorySource,
    };
    use crate::buddy::storage::enqueue_memory_op;
    use crate::ext::competitor_import::manifest::write_last_report;
    use crate::ext::competitor_import::types::{
        Competitor, ImportCandidateSummary, ImportKind, ImportOutcome, ImportScope,
        ImportSourceRoot, ImportSummary,
    };

    fn candidate(
        competitor: Competitor,
        kind: ImportKind,
        scope: ImportScope,
        name: &str,
    ) -> ImportCandidateSummary {
        ImportCandidateSummary {
            competitor,
            kind,
            scope,
            source_root: PathBuf::from("/source"),
            source_path: PathBuf::from(format!("/source/{name}.md")),
            dest_name: name.to_string(),
            destination_path: PathBuf::from(format!("commands/{name}.md")),
            metadata: serde_json::Value::Null,
        }
    }

    fn memory_fact(key: &str, payload: serde_json::Value) -> BuddyFact {
        BuddyFact {
            kind: BuddyFactKind::MemoryOrphan,
            key: key.to_string(),
            source: "test",
            payload,
            seen_at: Utc::now(),
            confidence: 0.9,
        }
    }

    async fn write_summary_report(scope_root: &Path, mut summary: ImportSummary) {
        summary.mark_completed();
        write_last_report(scope_root, &summary).await.unwrap();
    }

    fn init_repo_with_files(root: &Path, files: &[(&str, &str)]) -> git2::Repository {
        let repo = git2::Repository::init(root).unwrap();
        for (path, content) in files {
            let file_path = root.join(path);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&file_path, content).unwrap();
        }
        let mut index = repo.index().unwrap();
        for (path, _) in files {
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        drop(index);
        {
            let tree = repo.find_tree(tree_id).unwrap();
            let signature = git2::Signature::now("Buddy", "buddy@example.com").unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    #[tokio::test]
    async fn memory_pulse_includes_lifecycle_op_counts() {
        let temp = tempfile::tempdir().unwrap();
        let mut op = MemoryLifecycleOp::pending(
            "op-merge",
            MemorySource::MemoryGarden,
            MemoryOpType::MergeArchive,
            vec![".refact/knowledge/old.md".to_string()],
            "exact content_hash duplicate",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        op.status = MemoryOpStatus::Pending;
        enqueue_memory_op(temp.path(), op).await.unwrap();

        let pulse = build_memory_pulse(temp.path(), &FactStore::new()).await;

        assert_eq!(pulse.pending_ops, 1);
        assert_eq!(pulse.merge_candidates, 1);
        assert_eq!(pulse.duplicate_candidates, 1);
    }

    #[tokio::test]
    async fn memory_pulse_orphan_count_dedupes_memory_ids() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = FactStore::new();
        store.ingest(memory_fact(
            "orphan-a",
            serde_json::json!({ "memory_ids": ["memory-1", "memory-2"] }),
        ));
        store.ingest(memory_fact(
            "orphan-b",
            serde_json::json!({ "memory_ids": ["memory-1", "memory-2"] }),
        ));

        let pulse = build_memory_pulse(temp.path(), &store).await;

        assert_eq!(pulse.orphan, 2);
    }

    #[tokio::test]
    async fn memory_pulse_total_counts_markdown_files_only() {
        let temp = tempfile::tempdir().unwrap();
        let knowledge_dir = temp.path().join(".refact").join("knowledge");
        tokio::fs::create_dir_all(knowledge_dir.join("nested"))
            .await
            .unwrap();
        tokio::fs::write(knowledge_dir.join("one.md"), "# One")
            .await
            .unwrap();
        tokio::fs::write(knowledge_dir.join("nested").join("two.md"), "# Two")
            .await
            .unwrap();
        tokio::fs::write(knowledge_dir.join("notes.txt"), "ignore")
            .await
            .unwrap();

        let pulse = build_memory_pulse(temp.path(), &FactStore::new()).await;

        assert_eq!(pulse.total, 2);
    }

    #[tokio::test]
    async fn competitor_import_pulse_merges_global_and_project_reports() {
        let temp = tempfile::tempdir().unwrap();
        let refact_config = temp.path().join("config").join("refact");
        let workspace = temp.path().join("workspace");
        let mut global = ImportSummary::from_scopes(vec![ImportScope::Global]);
        global.discovered_sources.push(ImportSourceRoot {
            competitor: Competitor::ClaudeCode,
            scope: ImportScope::Global,
            path: temp.path().join(".claude"),
        });
        let global_candidate = candidate(
            Competitor::ClaudeCode,
            ImportKind::Command,
            ImportScope::Global,
            "global",
        );
        global.candidates.push(global_candidate.clone());
        global.add_outcome(ImportOutcome {
            candidate: global_candidate,
            status: ImportStatus::Created,
            message: "created".to_string(),
        });
        write_summary_report(&refact_config, global).await;

        let project_scope = ImportScope::Project {
            root: workspace.clone(),
        };
        let mut project = ImportSummary::from_scopes(vec![project_scope.clone()]);
        project.discovered_sources.push(ImportSourceRoot {
            competitor: Competitor::OpenCode,
            scope: project_scope.clone(),
            path: workspace.join(".opencode"),
        });
        let project_candidate = candidate(
            Competitor::OpenCode,
            ImportKind::Command,
            project_scope.clone(),
            "project",
        );
        project.candidates.push(project_candidate.clone());
        project.add_outcome(ImportOutcome {
            candidate: project_candidate,
            status: ImportStatus::Conflict,
            message: "conflict".to_string(),
        });
        project.add_issue(crate::ext::competitor_import::types::ImportIssue {
            competitor: Some(Competitor::OpenCode),
            kind: Some(ImportKind::Command),
            scope: Some(project_scope),
            path: Some(PathBuf::from("commands/project.md")),
            status: ImportStatus::Error,
            message: "error".to_string(),
        });
        write_summary_report(&workspace.join(".refact"), project).await;

        let pulse = build_competitor_import_pulse(&refact_config, &[workspace]).await;

        assert!(pulse.last_run_at.is_some());
        assert_eq!(pulse.discovered_candidates, 2);
        assert_eq!(pulse.created, 1);
        assert_eq!(pulse.conflicts, 1);
        assert_eq!(pulse.errors, 1);
        assert_eq!(pulse.attention_items, 2);
        assert!(pulse.has_attention_items);
        assert_eq!(pulse.sources_seen.get("claude_code"), Some(&1));
        assert_eq!(pulse.sources_seen.get("opencode"), Some(&1));
    }

    #[tokio::test]
    async fn competitor_import_pulse_sources_seen_uses_actual_activity() {
        let temp = tempfile::tempdir().unwrap();
        let refact_config = temp.path().join("config").join("refact");
        let workspace = temp.path().join("workspace");
        let command_path = workspace.join(".claude").join("commands").join("only.md");
        tokio::fs::create_dir_all(command_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&command_path, "Only Claude is present.")
            .await
            .unwrap();

        crate::ext::competitor_import::run_project_import_with_paths(&[workspace.clone()]).await;
        let pulse = build_competitor_import_pulse(&refact_config, &[workspace]).await;

        assert_eq!(pulse.sources_seen.get("claude_code"), Some(&1));
        assert_eq!(pulse.sources_seen.len(), 1);
        assert!(!pulse.sources_seen.contains_key("opencode"));
        assert!(!pulse.sources_seen.contains_key("kilo_code"));
        assert!(!pulse.sources_seen.contains_key("continue_dev"));
    }

    #[tokio::test]
    async fn competitor_import_pulse_includes_stale_count() {
        let temp = tempfile::tempdir().unwrap();
        let refact_config = temp.path().join("config").join("refact");
        let workspace = temp.path().join("workspace");
        let project_scope = ImportScope::Project {
            root: workspace.clone(),
        };
        let stale_candidate = candidate(
            Competitor::OpenCode,
            ImportKind::Command,
            project_scope.clone(),
            "stale",
        );
        let mut summary = ImportSummary::from_scopes(vec![project_scope]);
        summary.add_outcome(ImportOutcome {
            candidate: stale_candidate,
            status: ImportStatus::Stale,
            message: "source no longer exists; generated destination preserved".to_string(),
        });
        write_summary_report(&workspace.join(".refact"), summary).await;

        let pulse = build_competitor_import_pulse(&refact_config, &[workspace]).await;

        assert_eq!(pulse.stale, 1);
        assert_eq!(pulse.attention_items, 0);
        assert!(!pulse.has_attention_items);
        assert_eq!(pulse.sources_seen.get("opencode"), Some(&1));
    }

    #[tokio::test]
    async fn missing_competitor_import_manifests_return_default_pulse() {
        let temp = tempfile::tempdir().unwrap();
        let pulse = build_competitor_import_pulse(
            &temp.path().join("config").join("refact"),
            &[temp.path().join("workspace")],
        )
        .await;

        assert!(pulse.last_run_at.is_none());
        assert_eq!(pulse.discovered_candidates, 0);
        assert_eq!(pulse.attention_items, 0);
        assert!(!pulse.has_attention_items);
        assert!(pulse.sources_seen.is_empty());
    }

    #[test]
    fn customization_pulse_deserializes_without_competitor_import_field() {
        let json = r#"{"modes":1,"skills":2,"commands":3,"subagents":4,"hooks":5}"#;

        let pulse: CustomizationPulse = serde_json::from_str(json).unwrap();

        assert_eq!(pulse.modes, 1);
        assert_eq!(pulse.competitor_import, CompetitorImportPulse::default());
    }

    #[test]
    fn git_pulse_discovers_repo_from_subdirectory() {
        let temp = tempfile::tempdir().unwrap();
        let _repo = init_repo_with_files(temp.path(), &[("README.md", "initial\n")]);
        std::fs::write(temp.path().join("README.md"), "changed\n").unwrap();
        let subdir = temp.path().join("workspace").join("nested");
        std::fs::create_dir_all(&subdir).unwrap();

        let pulse = build_git_pulse(&subdir);

        assert!(pulse.branches > 0);
        assert_eq!(pulse.uncommitted_files, 1);
    }

    #[test]
    fn git_pulse_untracked_directory_scan_is_not_recursive() {
        let temp = tempfile::tempdir().unwrap();
        let _repo = init_repo_with_files(temp.path(), &[("README.md", "initial\n")]);
        let nested = temp.path().join("untracked").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        for idx in 0..30 {
            std::fs::write(nested.join(format!("file_{idx}.rs")), "fn main() {}\n").unwrap();
        }

        let pulse = build_git_pulse(temp.path());

        assert_eq!(pulse.uncommitted_files, 1);
    }

    #[test]
    fn git_pulse_counts_tracked_modified_file() {
        let temp = tempfile::tempdir().unwrap();
        let _repo = init_repo_with_files(
            temp.path(),
            &[("src/lib.rs", "pub fn value() -> u8 { 1 }\n")],
        );
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn value() -> u8 { 2 }\n",
        )
        .unwrap();

        let pulse = build_git_pulse(temp.path());

        assert_eq!(pulse.uncommitted_files, 1);
    }
}
