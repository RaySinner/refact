pub use refact_files::documents_state::{
    mark_files_cache_dirty, FILES_CACHE_DEBOUNCE_SECONDS,
};

pub use refact_files::path_utils::{
    any_glob_matches_path, canonical_path, canonicalize_normalized_path, deserialize_path,
    preprocess_path_for_normalization, serialize_path, shortify_paths_from_indexed,
    CommandSimplifiedDirExt,
};

pub use refact_runtime_api::files_correction::{
    check_if_its_inside_a_workspace_or_config,
    check_if_its_inside_a_workspace_or_config_for_ctx,
    check_if_its_inside_a_workspace_worktree_or_config, correct_to_nearest_dir_path,
    correct_to_nearest_filename, files_cache_rebuild_as_needed, get_active_project_path,
    get_active_project_path_for_services, get_active_workspace_folder, get_project_dirs,
    get_raw_project_dirs, get_unscoped_project_dirs, get_unscoped_project_dirs_for_ctx,
    paths_from_anywhere, resolve_codegraph_queue_path, shortify_paths,
};

pub(crate) use refact_runtime_api::files_correction_impl::{
    check_if_its_inside_a_workspace_or_config_impl, correct_to_nearest_dir_path_impl,
    correct_to_nearest_filename_impl, get_active_project_path_for_services_impl,
    get_active_project_path_impl, get_project_dirs_impl, get_unscoped_project_dirs_impl,
    paths_from_anywhere_impl, shortify_paths_impl,
};

pub use refact_worktrees::path_mappings::{
    dedupe_paths, map_registered_worktree_path, normalize_path_for_unscoped_paths,
    normalize_path_for_unscoped_root_selection, project_dirs_for_unscoped_paths,
    registered_alias_paths, registered_worktree_path_mappings, registered_worktree_roots,
    RegisteredWorktreePathMapping, WorktreePathMappings,
};

#[cfg(test)]
mod tests {
    use super::{
        canonicalize_normalized_path, correct_to_nearest_filename, get_active_project_path,
        get_active_workspace_folder, get_unscoped_project_dirs, paths_from_anywhere,
        project_dirs_for_unscoped_paths,
    };
    use refact_worktrees::types::{WorktreeMeta, WorktreeRegistry, WorktreeRegistryRecord};
    #[cfg(all(
        not(all(target_arch = "aarch64", target_os = "linux")),
        not(debug_assertions)
    ))]
    use crate::fuzzy_search::fuzzy_search;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn normalized_path(path: &Path) -> PathBuf {
        canonicalize_normalized_path(path.to_path_buf())
    }

    fn assert_single_normalized_candidate(candidates: Vec<String>, expected: &Path) {
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            normalized_path(Path::new(&candidates[0])),
            normalized_path(expected)
        );
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
        let registry = WorktreeRegistry {
            schema_version: 1,
            source_workspace_root: source.clone(),
            project_hash: hash,
            records: vec![WorktreeRegistryRecord {
                meta: WorktreeMeta {
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

    #[test]
    fn unscoped_project_dirs_ignore_registered_cache_worktrees_when_source_exists() {
        let temp = tempfile::tempdir().unwrap();
        let cache_dir = temp.path().join("cache");
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let worktree = worktree_root(&cache_dir, &source, "wt");
        fs::create_dir_all(&worktree).unwrap();
        write_worktree_registry(&cache_dir, &source, &worktree);

        let project_dirs = project_dirs_for_unscoped_paths(
            &cache_dir,
            &[worktree.clone(), source.clone(), worktree.join("nested")],
        );

        assert_eq!(project_dirs, vec![normalized_path(&source)]);
    }

    #[tokio::test]
    async fn unscoped_project_dirs_map_registered_worktree_to_source_when_source_absent() {
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

        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![worktree.clone()];
        *gcx.documents_state.workspace_files.lock().unwrap() = vec![worktree_file.clone()];
        *gcx.documents_state.cache_dirty.lock().await = 1.0;

        let project_dirs = get_unscoped_project_dirs(gcx.clone()).await;
        assert_eq!(project_dirs, vec![normalized_path(&source)]);

        let visible_paths = paths_from_anywhere(gcx.clone()).await;
        assert_eq!(visible_paths, vec![normalized_path(&source_file)]);

        let candidates =
            correct_to_nearest_filename(gcx.clone(), &"src/lib.rs".to_string(), false, 10).await;
        assert_single_normalized_candidate(candidates, &source_file);
    }

    #[tokio::test]
    async fn unscoped_path_cache_ignores_detached_worktree_files() {
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

        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![worktree.clone(), source.clone()];
        *gcx.documents_state.workspace_files.lock().unwrap() =
            vec![worktree_file.clone(), source_file.clone()];
        *gcx.documents_state.cache_dirty.lock().await = 1.0;

        let project_dirs = get_unscoped_project_dirs(gcx.clone()).await;
        assert_eq!(project_dirs, vec![normalized_path(&source)]);

        let visible_paths = paths_from_anywhere(gcx.clone()).await;
        assert_eq!(visible_paths, vec![normalized_path(&source_file)]);

        let candidates =
            correct_to_nearest_filename(gcx, &"src/lib.rs".to_string(), false, 10).await;
        assert_single_normalized_candidate(candidates, &source_file);
    }

    #[tokio::test]
    async fn active_project_path_maps_registered_worktree_active_file_to_source_root() {
        let source_a_temp = tempfile::tempdir().unwrap();
        let source_a = source_a_temp.path().join("source-a");
        let source_b_temp = tempfile::tempdir().unwrap();
        let source_b = source_b_temp.path().join("source-b");
        fs::create_dir_all(source_a.join("src")).unwrap();
        fs::create_dir_all(source_b.join("src")).unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        let worktree_b = worktree_root(&gcx.cache_dir, &source_b, "wt");
        fs::create_dir_all(worktree_b.join("src")).unwrap();
        let active_source_file = source_b.join("src").join("lib.rs");
        let active_worktree_file = worktree_b.join("src").join("lib.rs");
        fs::write(&active_source_file, "fn source_b() {}\n").unwrap();
        fs::write(&active_worktree_file, "fn worktree_b() {}\n").unwrap();
        write_worktree_registry(&gcx.cache_dir, &source_b, &worktree_b);

        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![source_a.clone(), worktree_b.clone()];
        *gcx.documents_state.active_file_path.lock().await = Some(active_worktree_file);

        let active_project = get_active_project_path(gcx.clone()).await;
        assert_eq!(active_project, Some(normalized_path(&source_b)));

        let active_workspace = get_active_workspace_folder(gcx).await;
        assert_eq!(active_workspace, Some(normalized_path(&source_b)));
    }

    #[tokio::test]
    async fn active_project_path_maps_worktree_only_active_file_to_source_root() {
        let source_a_temp = tempfile::tempdir().unwrap();
        let source_a = source_a_temp.path().join("source-a");
        let source_b_temp = tempfile::tempdir().unwrap();
        let source_b = source_b_temp.path().join("source-b");
        fs::create_dir_all(&source_a).unwrap();
        fs::create_dir_all(&source_b).unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        let worktree_b = worktree_root(&gcx.cache_dir, &source_b, "wt");
        let active_worktree_file = worktree_b.join("src").join("new.rs");
        fs::create_dir_all(active_worktree_file.parent().unwrap()).unwrap();
        fs::write(&active_worktree_file, "fn new_only_in_worktree() {}\n").unwrap();
        write_worktree_registry(&gcx.cache_dir, &source_b, &worktree_b);

        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![source_a.clone(), worktree_b.clone()];
        *gcx.documents_state.active_file_path.lock().await = Some(active_worktree_file);

        let active_project = get_active_project_path(gcx).await;
        assert_eq!(active_project, Some(normalized_path(&source_b)));
    }

    #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
    #[cfg(not(debug_assertions))]
    #[test]
    fn test_fuzzy_search_speed() {
        // Arrange
        let workspace_paths = vec![
            PathBuf::from("home").join("user").join("repo1"),
            PathBuf::from("home").join("user").join("repo2"),
            PathBuf::from("home").join("user").join("repo3"),
            PathBuf::from("home").join("user").join("repo4"),
        ];

        let mut paths = Vec::new();
        for i in 0..100000 {
            let path = workspace_paths[i % workspace_paths.len()]
                .join(format!("dir{}", i % 1000))
                .join(format!("dir{}", i / 1000))
                .join(format!("file{}.ext", i));
            paths.push(path);
        }
        let start_time = std::time::Instant::now();
        let paths_str = paths
            .iter()
            .map(|x| x.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        let correction_candidate = PathBuf::from("file100000")
            .join("dir1000")
            .join("file100000.ext")
            .to_string_lossy()
            .to_string();

        // Act
        let results = fuzzy_search(&correction_candidate, paths_str, 10, &['/', '\\']);

        // Assert
        let time_spent = start_time.elapsed();
        println!("fuzzy_search took {} ms", time_spent.as_millis());
        assert_eq!(results.len(), 10, "The result should contain 10 paths");
        println!("{:?}", results);
    }
}
