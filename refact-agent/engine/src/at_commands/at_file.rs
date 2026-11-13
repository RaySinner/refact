pub use refact_runtime_api::at_commands_impl::at_file::{AtFile, AtParamFilePath};
pub use refact_runtime_api::at_command_executor::{
    file_repair_candidates, return_one_candidate_or_a_good_error,
};

#[cfg(test)]
mod tests {
    use refact_files::path_utils::canonicalize_normalized_path;
    use refact_runtime_api::at_command_executor::return_one_candidate_or_a_good_error;
    use refact_runtime_api::GlobalContextCtx;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn normalized_path(path: &Path) -> PathBuf {
        canonicalize_normalized_path(path.to_path_buf())
    }

    fn worktree_root(cache_dir: &Path, source: &Path, id: &str) -> PathBuf {
        let source = normalized_path(source);
        cache_dir
            .join("worktrees")
            .join(refact_worktrees::service::project_hash_for_path(&source))
            .join(id)
    }

    fn write_worktree_registry(cache_dir: &Path, source: &Path, worktree: &Path) {
        let source = normalized_path(source);
        let worktree = normalized_path(worktree);
        let hash = refact_worktrees::service::project_hash_for_path(&source);
        let registry_dir = cache_dir.join("worktrees").join(&hash);
        fs::create_dir_all(&registry_dir).unwrap();
        let registry = refact_worktrees::types::WorktreeRegistry {
            schema_version: 1,
            source_workspace_root: source.clone(),
            project_hash: hash,
            records: vec![refact_worktrees::types::WorktreeRegistryRecord {
                meta: refact_worktrees::types::WorktreeMeta {
                    id: "wt".to_string(),
                    kind: "chat".to_string(),
                    root: worktree,
                    source_workspace_root: source.clone(),
                    repo_root: source.clone(),
                    branch: Some("refact/chat/test".to_string()),
                    base_branch: Some("main".to_string()),
                    base_commit: None,
                    task_id: None,
                    card_id: None,
                    agent_id: None,
                    enforce: true,
                },
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                last_seen_at: None,
                references: Vec::new(),
                last_known_status: None,
            }],
        };
        fs::write(
            registry_dir.join("index.json"),
            serde_json::to_string_pretty(&registry).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn relative_candidates_ignore_detached_cache_worktree_when_source_exists() {
        let source_temp = tempfile::tempdir().unwrap();
        let source = source_temp.path().join("source");
        fs::create_dir_all(source.join("src")).unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
        fs::create_dir_all(worktree.join("src")).unwrap();
        let source_file = source.join("src").join("lib.rs");
        let worktree_file = worktree.join("src").join("lib.rs");
        fs::write(&source_file, "fn source() {}\n").unwrap();
        fs::write(&worktree_file, "fn worktree() {}\n").unwrap();
        write_worktree_registry(&gcx.cache_dir, &source, &worktree);

        let candidates = vec![
            worktree_file.to_string_lossy().to_string(),
            source_file.to_string_lossy().to_string(),
        ];
        let project_paths = vec![worktree, source];

        let tool = gcx.tool_ctx(gcx.clone());
        let resolved = return_one_candidate_or_a_good_error(
            &tool,
            &"src/lib.rs".to_string(),
            &candidates,
            &project_paths,
            false,
        )
        .await
        .unwrap();

        assert_eq!(resolved, source_file.to_string_lossy().to_string());
    }
}
