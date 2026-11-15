pub use refact_runtime_api::buddy_observers::git_pressure::*;
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    use chrono::Utc;

    use crate::buddy::memory_lifecycle::{MemoryOpType, MemorySource};
    use crate::knowledge_graph::kg_structs::KnowledgeFrontmatter;

    fn init_repo() -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        (dir, repo)
    }

    fn signature() -> git2::Signature<'static> {
        git2::Signature::now("test", "test@example.com").unwrap()
    }

    fn commit_paths(repo: &git2::Repository, paths: &[&str], message: &str) -> git2::Oid {
        let mut index = repo.index().unwrap();
        for path in paths {
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = signature();
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap()
    }

    fn commit_file(
        repo: &git2::Repository,
        root: &Path,
        path: &str,
        content: &str,
        message: &str,
    ) -> git2::Oid {
        let file_path = root.join(path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&file_path, content).unwrap();
        commit_paths(repo, &[path], message)
    }

    fn rename_and_commit(
        repo: &git2::Repository,
        root: &Path,
        old_path: &str,
        new_path: &str,
        message: &str,
    ) -> git2::Oid {
        fs::rename(root.join(old_path), root.join(new_path)).unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new(old_path)).unwrap();
        index.add_path(Path::new(new_path)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = signature();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
            .unwrap()
    }

    fn write_memory(root: &Path, name: &str, frontmatter: KnowledgeFrontmatter, body: &str) {
        let dir = root.join(crate::file_filter::KNOWLEDGE_FOLDER_NAME);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, format!("{}\n\n{}", frontmatter.to_yaml(), body)).unwrap();
    }

    #[tokio::test]
    async fn rename_detection_enqueues_high_confidence_repair_links_candidate() {
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "old.rs", "one\ntwo\n", "initial");
        rename_and_commit(
            &repo,
            dir.path(),
            "old.rs",
            "new.rs",
            "rename old to new because layout",
        );
        write_memory(
            dir.path(),
            "old.md",
            KnowledgeFrontmatter {
                id: Some("old-memory".to_string()),
                title: Some("Old memory".to_string()),
                status: Some("proposed".to_string()),
                source_tool: Some("buddy_memory_lifecycle:git".to_string()),
                filenames: vec!["old.rs".to_string()],
                ..Default::default()
            },
            "Remember old.rs",
        );
        fs::remove_file(dir.path().join(".refact/buddy/memory_ops.jsonl")).ok();

        let ops = detect_and_enqueue_git_memory_ops(dir.path(), Utc::now()).await;

        let repair = ops
            .iter()
            .find(|op| op.op_type == MemoryOpType::RepairLinks)
            .expect("repair op");
        assert_eq!(repair.source, MemorySource::Git);
        assert!(!repair.requires_approval);
        assert!(repair.confidence >= 0.85);
        assert_eq!(
            repair.payload.filenames.as_deref(),
            Some(&vec!["new.rs".to_string()][..])
        );
    }

    #[tokio::test]
    async fn repeated_cochange_threshold_enqueues_one_pattern_candidate() {
        let (dir, repo) = init_repo();
        for idx in 0..4 {
            fs::write(dir.path().join("a.rs"), format!("a {idx}\n")).unwrap();
            fs::write(dir.path().join("b.rs"), format!("b {idx}\n")).unwrap();
            commit_paths(&repo, &["a.rs", "b.rs"], "fix pair because bug");
        }

        let ops = detect_and_enqueue_git_memory_ops(dir.path(), Utc::now()).await;
        let patterns = ops
            .iter()
            .filter(|op| {
                op.op_type == MemoryOpType::CreateMemory
                    && op
                        .payload
                        .canonical
                        .as_ref()
                        .map(|payload| payload.kind.as_str() == "pattern")
                        .unwrap_or(false)
            })
            .collect::<Vec<_>>();

        assert_eq!(patterns.len(), 1);
        let payload = patterns[0].payload.canonical.as_ref().unwrap();
        assert_eq!(
            payload.filenames,
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
    }
}
