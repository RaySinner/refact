use super::*;


fn registration_modes(
    plan: &[WatchRegistration],
    root: &Path,
) -> Vec<(PathBuf, WatchRegistrationMode)> {
    let mut entries = plan
        .iter()
        .map(|registration| {
            (
                registration
                    .path
                    .strip_prefix(root)
                    .unwrap_or(registration.path.as_path())
                    .to_path_buf(),
                registration.mode,
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

fn write_file(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn normalized(path: &Path) -> PathBuf {
    crate::files_correction::canonical_path(path.to_string_lossy().to_string())
}

fn worktree_root(cache_dir: &Path, source: &Path, id: &str) -> PathBuf {
    let source = normalized(source);
    cache_dir
        .join("worktrees")
        .join(refact_worktrees::service::project_hash_for_path(&source))
        .join(id)
}

fn write_worktree_registry(cache_dir: &Path, source: &Path, worktree: &Path) {
    let source = normalized(source);
    let worktree = normalized(worktree);
    let hash = refact_worktrees::service::project_hash_for_path(&source);
    let registry_dir = cache_dir.join("worktrees").join(&hash);
    std::fs::create_dir_all(&registry_dir).unwrap();
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
                repo_root: source,
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
    std::fs::write(
        registry_dir.join("index.json"),
        serde_json::to_string_pretty(&registry).unwrap(),
    )
    .unwrap();
}

async fn scan_workspace(root: &Path) -> Vec<PathBuf> {
    let mut indexing_everywhere = IndexingEverywhere::default();
    let (files, _) = retrieve_files_in_workspace_folders(
        vec![root.to_path_buf()],
        &mut indexing_everywhere,
        false,
        false,
        &[],
    )
    .await;
    files
}

async fn scan_workspace_with_global_config_root(
    root: &Path,
    global_config_root: &Path,
) -> Vec<PathBuf> {
    let mut indexing_everywhere = IndexingEverywhere::default();
    let roots = vec![global_config_root.to_path_buf()];
    let (files, _) = retrieve_files_in_workspace_folders(
        vec![root.to_path_buf()],
        &mut indexing_everywhere,
        false,
        false,
        &roots,
    )
    .await;
    files
}

async fn cache_dirty_value(gcx: &Arc<GlobalContext>) -> f64 {
    let dirty = { gcx.documents_state.cache_dirty.clone() };
    let value = *dirty.lock().await;
    value
}

fn allow_all_privacy(gcx: &Arc<GlobalContext>) {
    let loaded_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 60;
    *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
        privacy_rules: crate::privacy::FilePrivacySettings {
            only_send_to_servers_I_control: Vec::new(),
            blocked: Vec::new(),
        },
        loaded_ts,
    });
}

fn set_blocked_privacy(gcx: &Arc<GlobalContext>, blocked: Vec<String>) {
    *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
        privacy_rules: crate::privacy::FilePrivacySettings {
            only_send_to_servers_I_control: Vec::new(),
            blocked,
        },
        loaded_ts: u64::MAX / 2,
    });
}

fn path_counts(paths: &[PathBuf]) -> HashMap<PathBuf, usize> {
    let mut counts = HashMap::new();
    for path in paths {
        *counts.entry(path.clone()).or_insert(0) += 1;
    }
    counts
}

#[test]
fn ls_files_limited_stops_after_small_entry_budget_and_reports_truncation() {
    let temp = tempfile::tempdir().unwrap();
    for i in 0..50 {
        write_file(&temp.path().join(format!("file_{:03}.txt", i)), "x");
    }
    let indexing_everywhere = IndexingEverywhere::default();
    let result = ls_files_limited(
        &indexing_everywhere,
        &temp.path().to_path_buf(),
        true,
        5,
        None,
    )
    .unwrap();
    assert!(result.truncated, "expected truncated result");
    assert!(!result.aborted, "did not expect abort");
    assert!(
        result.files.len() <= 5,
        "collected {} files, expected <= budget",
        result.files.len()
    );
    assert!(!result.files.is_empty(), "expected some files collected");
}

#[test]
fn ls_files_limited_observes_preset_abort_flag() {
    let temp = tempfile::tempdir().unwrap();
    for i in 0..10 {
        write_file(&temp.path().join(format!("file_{:03}.txt", i)), "x");
    }
    let indexing_everywhere = IndexingEverywhere::default();
    let abort = AtomicBool::new(true);
    let result = ls_files_limited(
        &indexing_everywhere,
        &temp.path().to_path_buf(),
        true,
        usize::MAX,
        Some(&abort),
    )
    .unwrap();
    assert!(result.aborted, "expected aborted result");
    assert!(
        result.files.is_empty(),
        "expected no files when aborted up-front"
    );
}

#[test]
fn ls_files_limited_unlimited_budget_matches_small_directory_listing() {
    let temp = tempfile::tempdir().unwrap();
    write_file(&temp.path().join("a.txt"), "a");
    write_file(&temp.path().join("sub").join("b.txt"), "b");
    write_file(&temp.path().join("sub").join("c.txt"), "c");
    let indexing_everywhere = IndexingEverywhere::default();

    let unlimited = ls_files(&indexing_everywhere, &temp.path().to_path_buf(), true).unwrap();
    let limited = ls_files_limited(
        &indexing_everywhere,
        &temp.path().to_path_buf(),
        true,
        usize::MAX,
        None,
    )
    .unwrap();

    assert!(!limited.truncated, "small dir should not be truncated");
    assert!(!limited.aborted, "small dir should not be aborted");
    assert_eq!(
        path_counts(&limited.files),
        path_counts(&unlimited),
        "limited (unlimited budget) should match ls_files output"
    );
}

#[test]
fn ls_files_limited_errors_on_non_directory_like_ls_files() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("not_a_dir.txt");
    write_file(&file, "x");
    let indexing_everywhere = IndexingEverywhere::default();
    let err = ls_files_limited(&indexing_everywhere, &file, true, 10, None).unwrap_err();
    assert!(
        err.contains("is not a directory"),
        "unexpected error: {err}"
    );
}

#[test]
fn tiering_is_permutation_nothing_dropped() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(".git")).unwrap();
    std::fs::create_dir_all(temp.path().join("competitors").join("one").join(".git")).unwrap();
    std::fs::create_dir_all(temp.path().join("vendor").join("two").join(".git")).unwrap();
    let primary_a = temp.path().join("src").join("a.rs");
    let nested_a = temp
        .path()
        .join("competitors")
        .join("one")
        .join("src")
        .join("a.rs");
    let primary_b = temp.path().join("src").join("b.rs");
    let nested_b = temp
        .path()
        .join("vendor")
        .join("two")
        .join("src")
        .join("b.rs");
    write_file(&primary_a, "fn a() {}\n");
    write_file(&nested_a, "fn nested_a() {}\n");
    write_file(&primary_b, "fn b() {}\n");
    write_file(&nested_b, "fn nested_b() {}\n");
    let files = vec![
        normalized(&primary_a),
        normalized(&nested_a),
        normalized(&primary_b),
        normalized(&nested_b),
    ];

    let ordered =
        order_workspace_files_for_initial_enqueue(files.clone(), &[normalized(temp.path())]);

    assert_eq!(path_counts(&ordered), path_counts(&files));
    assert_eq!(
        ordered,
        vec![
            normalized(&primary_a),
            normalized(&primary_b),
            normalized(&nested_a),
            normalized(&nested_b),
        ]
    );
}

#[test]
fn nested_git_detection() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(".git")).unwrap();
    std::fs::create_dir_all(temp.path().join("nested_a").join(".git")).unwrap();
    std::fs::create_dir_all(temp.path().join("nested_b").join(".git")).unwrap();
    let primary = temp.path().join("src").join("main.rs");
    let nested_a = temp.path().join("nested_a").join("lib.rs");
    let nested_b = temp.path().join("nested_b").join("lib.rs");
    write_file(&primary, "fn main() {}\n");
    write_file(&nested_a, "fn nested_a() {}\n");
    write_file(&nested_b, "fn nested_b() {}\n");
    let files = vec![
        normalized(&nested_a),
        normalized(&primary),
        normalized(&nested_b),
    ];

    let ordered =
        order_workspace_files_for_initial_enqueue(files.clone(), &[normalized(temp.path())]);

    assert_eq!(
        ordered,
        vec![
            normalized(&primary),
            normalized(&nested_a),
            normalized(&nested_b),
        ]
    );

    let no_git = tempfile::tempdir().unwrap();
    let plain = no_git.path().join("plain.rs");
    let nested = no_git.path().join("nested").join("lib.rs");
    std::fs::create_dir_all(no_git.path().join("nested").join(".git")).unwrap();
    write_file(&plain, "fn plain() {}\n");
    write_file(&nested, "fn nested() {}\n");
    let files = vec![normalized(&nested), normalized(&plain)];

    let ordered =
        order_workspace_files_for_initial_enqueue(files.clone(), &[normalized(no_git.path())]);

    assert_eq!(ordered, files);
}

#[tokio::test]
async fn workspace_scan_excludes_refact_import_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let regular = temp.path().join("src").join("main.rs");
    let manifest = temp
        .path()
        .join(".refact")
        .join("imports")
        .join("competitors.json");
    write_file(&regular, "fn main() {}\n");
    write_file(&manifest, "{\"ok\":true}");

    let files = scan_workspace(temp.path()).await;

    assert!(files.contains(&normalized(&regular)));
    assert!(!files.contains(&normalized(&manifest)));
}

#[tokio::test]
async fn workspace_scan_excludes_refact_import_staging_content() {
    let temp = tempfile::tempdir().unwrap();
    let regular = temp.path().join("src").join("lib.rs");
    let staged = temp
        .path()
        .join(".refact")
        .join("imports")
        .join("staging")
        .join("skill")
        .join("SKILL.md");
    write_file(&regular, "pub fn ok() {}\n");
    write_file(&staged, "staged skill content\n");

    let files = scan_workspace(temp.path()).await;

    assert!(files.contains(&normalized(&regular)));
    assert!(!files.contains(&normalized(&staged)));
}

#[test]
fn watch_plan_excludes_blocklisted_dirs_and_keeps_refact_visible() {
    let temp = tempfile::tempdir().unwrap();
    let root = normalized(temp.path());
    write_file(
        &root.join("src").join("a").join("b").join("lib.rs"),
        "pub fn ok() {}
",
    );
    write_file(
        &root.join("node_modules").join("pkg").join("index.js"),
        "console.log('x');
",
    );
    write_file(
        &root.join("target").join("debug").join("app"),
        "binary
",
    );
    write_file(
        &root.join(".git").join("HEAD"),
        "ref: refs/heads/main
",
    );
    write_file(
        &root.join(".venv").join("bin").join("python"),
        "#!/usr/bin/env python
",
    );
    write_file(
        &root.join("dist").join("bundle.js"),
        "console.log('dist');
",
    );
    write_file(
        &root
            .join(".refact")
            .join("skills")
            .join("example")
            .join("SKILL.md"),
        "# skill
",
    );

    let indexing_everywhere = IndexingEverywhere::default();
    let plan = build_watch_plan(&[root.clone()], &indexing_everywhere);
    let entries = registration_modes(&plan, &root);

    assert!(entries.contains(&(PathBuf::from(""), WatchRegistrationMode::NonRecursive)));
    assert!(entries.contains(&(PathBuf::from("src"), WatchRegistrationMode::Recursive)));
    assert!(entries.contains(&(PathBuf::from(".refact"), WatchRegistrationMode::Recursive)));
    assert!(!entries
        .iter()
        .any(|(path, _)| path == Path::new("node_modules")));
    assert!(!entries.iter().any(|(path, _)| path == Path::new("target")));
    assert!(!entries.iter().any(|(path, _)| path == Path::new(".git")));
    assert!(!entries.iter().any(|(path, _)| path == Path::new(".venv")));
    assert!(!entries.iter().any(|(path, _)| path == Path::new("dist")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_created_directory_under_non_recursive_root_is_watched_end_to_end() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let root = normalized(temp.path());
    write_file(
        &root.join("src").join("lib.rs"),
        "pub fn before() {}
",
    );
    write_file(
        &root.join("node_modules").join("pkg").join("index.js"),
        "console.log('x');
",
    );
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.clone()];
    *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![root.clone()];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    *gcx.codegraph.lock().await = Some(service.clone());

    let indexing_everywhere = IndexingEverywhere::default();
    assert_eq!(
        classify_watch_subtree(&root, &root, &indexing_everywhere),
        WatchSubtreeClass::Mixed,
        "root must be non-recursive so that only the dynamic hook can cover new dirs"
    );

    watcher_init(gcx.clone()).await;

    let created_dir = root.join("runtime");
    std::fs::create_dir_all(&created_dir).unwrap();
    let runtime_file = created_dir.join("new.rs");
    let expected = normalized(&runtime_file).to_string_lossy().to_string();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while !seen && Instant::now() < deadline {
        write_file(
            &runtime_file,
            "pub fn runtime() {}
",
        );
        tokio::time::sleep(FILE_EVENT_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;
        seen = service.drain_batch(10).contains(&expected);
    }
    assert!(
        seen,
        "file inside a runtime-created directory never produced an event: the dynamic \
         watch registration did not happen, or the notify thread deadlocked"
    );

    gcx.shutdown_flag.store(true, Ordering::Relaxed);
    gcx.documents_state.file_event_debounce_notify.notify_one();
}

#[tokio::test]
async fn workspace_scan_keeps_refact_skills() {
    let temp = tempfile::tempdir().unwrap();
    let skill = temp
        .path()
        .join(".refact")
        .join("skills")
        .join("example")
        .join("SKILL.md");
    write_file(&skill, "# Example skill\nUse this skill.\n");

    let files = scan_workspace(temp.path()).await;

    assert!(files.contains(&normalized(&skill)));
}

#[tokio::test]
async fn workspace_scan_excludes_refact_generated_index_json() {
    let temp = tempfile::tempdir().unwrap();
    let regular = temp.path().join("docs").join("index.json");
    let generated = temp
        .path()
        .join(".refact")
        .join("trajectories")
        .join("index.json");
    write_file(&regular, "{\"user\":true}\n");
    write_file(&generated, "{\"schema_version\":1}\n");

    let files = scan_workspace(temp.path()).await;

    assert!(files.contains(&normalized(&regular)));
    assert!(!files.contains(&normalized(&generated)));
}

#[test]
fn refact_runtime_state_denies_engine_written_top_level_entries_only() {
    let refact = Path::new("/repo/.refact");
    for denied in [
        "stats/00000268.jsonl",
        "shell_audit.jsonl",
        "shell_audit.jsonl.tmp",
        "scheduled_tasks.json",
        "background_agents/records.json",
        "background_agents/results/agent.json",
        "vecdb/data.lance",
        "review_scratch/abc/src/lib.rs",
        "visual_baselines/home.png",
        "trajectories/index.json",
        "tasks/t1/trajectories/chat.json",
    ] {
        assert!(
            path_is_refact_runtime_state(&refact.join(denied)),
            "{denied} must be treated as runtime state"
        );
    }
    for kept in [
        "knowledge/stats/note.md",
        "skills/stats/SKILL.md",
        "modes/agent.yaml",
        "integrations.d/github.yaml",
        "project_information.yaml",
        "commands/ship.md",
        "statistics/report.md",
    ] {
        assert!(
            !path_is_refact_runtime_state(&refact.join(kept)),
            "{kept} must stay visible"
        );
    }
    assert!(!path_is_refact_runtime_state(Path::new(
        "/repo/src/stats/mod.rs"
    )));
}

#[tokio::test]
async fn workspace_scan_excludes_engine_runtime_logs_under_refact() {
    let temp = tempfile::tempdir().unwrap();
    let refact = temp.path().join(".refact");
    let stats = refact.join("stats").join("00000001.jsonl");
    let audit = refact.join("shell_audit.jsonl");
    let records = refact.join("background_agents").join("records.json");
    let knowledge = refact.join("knowledge").join("stats").join("note.md");
    write_file(&stats, "{\"tokens\":1}\n");
    write_file(&audit, "{\"cmd\":\"ls\"}\n");
    write_file(&records, "{\"records\":[]}\n");
    write_file(&knowledge, "# stats note\n");

    let files = scan_workspace(temp.path()).await;

    assert!(!files.contains(&normalized(&stats)));
    assert!(!files.contains(&normalized(&audit)));
    assert!(!files.contains(&normalized(&records)));
    assert!(files.contains(&normalized(&knowledge)));
}

#[tokio::test]
async fn workspace_scan_keeps_repo_local_config_refact_index_json() {
    let temp = tempfile::tempdir().unwrap();
    let config_index = temp
        .path()
        .join(".config")
        .join("refact")
        .join("trajectories")
        .join("index.json");
    write_file(&config_index, "{\"user\":true}\n");

    let files = scan_workspace(temp.path()).await;

    assert!(files.contains(&normalized(&config_index)));
}

#[tokio::test]
async fn workspace_scan_excludes_configured_global_config_index_json() {
    let temp = tempfile::tempdir().unwrap();
    let config_root = temp.path().join("global_config").join("refact");
    let generated = config_root.join("trajectories").join("index.json");
    let regular = config_root.join("knowledge").join("index.json");
    write_file(&generated, "{\"schema_version\":1}\n");
    write_file(&regular, "{\"user\":true}\n");

    let files = scan_workspace_with_global_config_root(&config_root, &config_root).await;

    assert!(!files.contains(&normalized(&generated)));
    assert!(files.contains(&normalized(&regular)));
}

#[tokio::test]
async fn workspace_scan_excludes_refact_codegraph_db_files() {
    let temp = tempfile::tempdir().unwrap();
    let regular = temp.path().join("src").join("lib.rs");
    let db_wal = temp
        .path()
        .join(".refact")
        .join("codegraph")
        .join("codegraph.sqlite-wal");
    write_file(&regular, "pub fn ok() {}\n");
    write_file(&db_wal, "sqlite wal content\n");

    let files = scan_workspace(temp.path()).await;

    assert!(files.contains(&normalized(&regular)));
    assert!(!files.contains(&normalized(&db_wal)));
    assert!(is_valid_file(&normalized(&db_wal), false, false).is_err());
    assert!(!path_triggers_registry_reload(&db_wal));
}

#[tokio::test]
async fn on_did_change_maps_registered_worktree_file_to_source_cache_path() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn old() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn old() {}\n");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);

    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![worktree.clone()];
    *gcx.documents_state.workspace_files.lock().unwrap() = vec![normalized(&worktree_file)];
    *gcx.documents_state.cache_dirty.lock().await = 1.0;
    crate::files_correction::files_cache_rebuild_as_needed(gcx.clone()).await;

    on_did_change(
        gcx.clone(),
        &worktree_file,
        &"fn changed() {}\n".to_string(),
    )
    .await;

    let workspace_files = gcx.documents_state.workspace_files.lock().unwrap().clone();
    assert!(workspace_files.contains(&normalized(&source_file)));
    assert!(!workspace_files.contains(&normalized(&worktree_file)));
}

#[tokio::test]
async fn open_worktree_buffer_is_read_through_mapped_source_path() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn disk() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree_disk() {}\n");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);

    let unsaved_text = "fn unsaved_from_ide() {}\n".to_string();
    on_did_open(
        gcx.clone(),
        &worktree_file,
        &unsaved_text,
        &"rust".to_string(),
    )
    .await;

    let read_text = get_file_text_from_memory_or_disk(gcx.clone(), &source_file)
        .await
        .unwrap();
    assert_eq!(read_text, unsaved_text);

    let memory_doc_map = gcx.documents_state.memory_document_map.lock().await;
    assert!(memory_doc_map.contains_key(&normalized(&source_file)));
    assert!(!memory_doc_map.contains_key(&normalized(&worktree_file)));
}

#[tokio::test]
async fn worktree_read_rejects_blocked_mapped_source_alias() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("blocked_source");
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn source() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree() {}\n");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);
    set_blocked_privacy(&gcx, vec!["*/blocked_source/*".to_string()]);

    let error = get_file_text_from_memory_or_disk(gcx, &worktree_file)
        .await
        .unwrap_err();

    assert!(error.contains("privacy"), "unexpected error: {error}");
}

#[test]
fn read_context_allows_controlled_path_but_rejects_blocked_alias() {
    let temp = tempfile::tempdir().unwrap();
    let source = normalized(&temp.path().join("source"));
    let worktree = normalized(&temp.path().join("worktree"));
    let source_file = source.join("src").join("lib.rs");
    let worktree_file = worktree.join("src").join("lib.rs");
    let mappings = Arc::new(vec![
        crate::files_correction::RegisteredWorktreePathMapping {
            root: worktree,
            source_root: source,
        },
    ]);
    let controlled_settings = Arc::new(PrivacySettings {
        privacy_rules: crate::privacy::FilePrivacySettings {
            only_send_to_servers_I_control: vec![source_file.to_string_lossy().to_string()],
            blocked: Vec::new(),
        },
        loaded_ts: 0,
    });
    let controlled_context = FileReadContext {
        runtime: Arc::new(EngineFilesRuntime),
        privacy_settings: controlled_settings,
        worktree_mappings: mappings.clone(),
    };

    assert!(check_file_privacy_for_read_preparation_with_context(
        &controlled_context,
        &source_file
    )
    .is_ok());
    assert!(check_file_privacy_with_context(&controlled_context, &source_file).is_err());

    let blocked_alias_context = FileReadContext {
        runtime: Arc::new(EngineFilesRuntime),
        privacy_settings: Arc::new(PrivacySettings {
            privacy_rules: crate::privacy::FilePrivacySettings {
                only_send_to_servers_I_control: vec![source_file.to_string_lossy().to_string()],
                blocked: vec![worktree_file.to_string_lossy().to_string()],
            },
            loaded_ts: 0,
        }),
        worktree_mappings: mappings,
    };
    assert!(check_file_privacy_for_read_preparation_with_context(
        &blocked_alias_context,
        &source_file,
    )
    .is_err());
}

#[test]
fn strictest_zone_uses_registered_worktree_alias() {
    let temp = tempfile::tempdir().unwrap();
    let source = normalized(&temp.path().join("source"));
    let worktree = normalized(&temp.path().join("worktree"));
    let source_file = source.join("src").join("lib.rs");
    let worktree_file = worktree.join("src").join("lib.rs");
    let policy = refact_privacy::PrivacyPolicy {
        blocked: Vec::new(),
        zones: vec![
            refact_privacy::Zone {
                name: "secrets".to_string(),
                patterns: vec![worktree_file.to_string_lossy().to_string()],
                send_to: Vec::new(),
                on_shell_read: refact_privacy::ShellBehavior::Deny,
            },
            refact_privacy::Zone {
                name: "normal".to_string(),
                patterns: vec!["*".to_string()],
                send_to: vec!["*".to_string()],
                on_shell_read: refact_privacy::ShellBehavior::Withhold,
            },
        ],
        subagents: refact_privacy::SubagentPolicy::default(),
        ..Default::default()
    }
    .compile()
    .unwrap();
    let mappings = vec![crate::files_correction::RegisteredWorktreePathMapping {
        root: worktree,
        source_root: source.clone(),
    }];

    assert_eq!(policy.zone_for_path(&source_file).name, "normal");
    assert_eq!(policy.zone_for_path(&worktree_file).name, "secrets");
    assert_eq!(
        strictest_zone_for_path(&policy, &source_file, &[source], &mappings).name,
        "secrets"
    );
}

#[tokio::test]
async fn source_read_rejects_blocked_worktree_alias_buffer() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn source() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree() {}\n");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);
    allow_all_privacy(&gcx);
    on_did_open(
        gcx.clone(),
        &worktree_file,
        &"fn unsaved() {}\n".to_string(),
        &"rust".to_string(),
    )
    .await;
    set_blocked_privacy(&gcx, vec!["*/worktrees/*/wt/*".to_string()]);

    let error = get_file_text_from_memory_or_disk(gcx, &source_file)
        .await
        .unwrap_err();

    assert!(error.contains("privacy"), "unexpected error: {error}");
}

#[tokio::test]
async fn bounded_disk_read_accepts_limit_and_rejects_limit_plus_one() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let temp = tempfile::tempdir().unwrap();
    let exact = temp.path().join("exact.txt");
    let oversized = temp.path().join("oversized.txt");
    std::fs::write(&exact, b"1234").unwrap();
    std::fs::write(&oversized, b"12345").unwrap();
    let read_context = prepare_file_read_context(gcx.clone()).await;

    let exact_text = get_file_text_from_memory_or_disk_with_context(
        gcx.clone(),
        &exact,
        &read_context,
        Some(4),
    )
    .await
    .unwrap();
    let oversized_error =
        get_file_text_from_memory_or_disk_with_context(gcx, &oversized, &read_context, Some(4))
            .await
            .unwrap_err();

    assert_eq!(exact_text, "1234");
    assert!(oversized_error.contains("exceeds 4 byte"));
}

#[tokio::test]
async fn raw_worktree_invalidation_removes_mapped_source_buffer() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn disk() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree_disk() {}\n");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);

    let unsaved_text = "fn unsaved_from_ide() {}\n".to_string();
    on_did_open(
        gcx.clone(),
        &worktree_file,
        &unsaved_text,
        &"rust".to_string(),
    )
    .await;
    assert_eq!(
        get_file_text_from_memory_or_disk(gcx.clone(), &source_file)
            .await
            .unwrap(),
        unsaved_text
    );

    assert!(remove_memory_document_for_path(gcx.clone(), &worktree_file).await);
    assert_eq!(
        get_file_text_from_memory_or_disk(gcx.clone(), &source_file)
            .await
            .unwrap(),
        "fn disk() {}\n"
    );
}

#[tokio::test]
async fn raw_worktree_watcher_event_uses_source_blocklist_after_mapping() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let source_file = source.join("src").join("blocked.rs");
    write_file(&source_file, "fn blocked() {}\n");
    write_file(
        &source.join(".refact").join("indexing.yaml"),
        "blocklist:\n  - 'src/blocked.rs'\n",
    );

    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("blocked.rs");
    write_file(&worktree_file, "fn blocked_worktree() {}\n");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![worktree.clone()];
    *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![source.clone()];

    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(worktree_file.clone());
    file_watcher_event(event, Arc::downgrade(&gcx)).await;

    let workspace_files = gcx.documents_state.workspace_files.lock().unwrap().clone();
    assert!(!workspace_files.contains(&normalized(&source_file)));
    assert!(!workspace_files.contains(&normalized(&worktree_file)));
}

#[tokio::test]
async fn on_did_delete_registered_worktree_file_removes_source_keyed_codegraph_row() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    std::fs::create_dir_all(source.join("src")).unwrap();
    // Canonicalize while the dir exists so raw fixture paths match canonical
    // index keys on Windows runners with an 8.3 short TMP (RUNNER~1).
    let source = normalized(&source);
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn source() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree() {}\n");
    let worktree = normalized(&worktree);
    let worktree_file = worktree.join("src").join("lib.rs");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    let source_key = normalized(&source_file).to_string_lossy().to_string();
    service
        .index_file(&source_key, "fn source() {}\n", "rust")
        .await
        .unwrap();
    *gcx.codegraph.lock().await = Some(service.clone());

    std::fs::remove_file(&worktree_file).unwrap();
    on_did_delete(gcx.clone(), &worktree_file).await;

    assert_eq!(service.counts().await.unwrap().files, 0);
    assert!(source_file.exists());
}

#[tokio::test]
async fn on_did_open_ignores_refact_import_internal_paths() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp
        .path()
        .join(".refact")
        .join("imports")
        .join("staging")
        .join("x.md");
    let text = "staged content".to_string();
    let language_id = "markdown".to_string();

    on_did_open(gcx.clone(), &path, &text, &language_id).await;

    let (memory_doc_map, active_fp) = {
        (
            gcx.documents_state.memory_document_map.clone(),
            gcx.documents_state.active_file_path.clone(),
        )
    };
    let has_doc = memory_doc_map.lock().await.contains_key(&path);
    let active_file_path = active_fp.lock().await.clone();
    assert!(!has_doc);
    assert!(active_file_path.is_none());
    assert_eq!(cache_dirty_value(&gcx).await, 0.0);
}

#[tokio::test]
async fn on_did_change_ignores_refact_import_internal_paths() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp
        .path()
        .join(".refact")
        .join("imports")
        .join("staging")
        .join("x.md");
    let text = "changed staged content".to_string();

    on_did_change(gcx.clone(), &path, &text).await;

    let (memory_doc_map2, active_fp2, workspace_files_len) = {
        let wf_len = gcx.documents_state.workspace_files.lock().unwrap().len();
        (
            gcx.documents_state.memory_document_map.clone(),
            gcx.documents_state.active_file_path.clone(),
            wf_len,
        )
    };
    let has_doc = memory_doc_map2.lock().await.contains_key(&path);
    let active_file_path = active_fp2.lock().await.clone();
    assert!(!has_doc);
    assert!(active_file_path.is_none());
    assert_eq!(workspace_files_len, 0);
    assert_eq!(cache_dirty_value(&gcx).await, 0.0);
}

#[tokio::test]
async fn watcher_ignores_refact_codegraph_db_changes() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp
        .path()
        .join(".refact")
        .join("codegraph")
        .join("codegraph.sqlite-wal");
    write_file(&path, "sqlite wal content\n");

    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(path.clone());
    file_watcher_event(event, Arc::downgrade(&gcx)).await;

    let workspace_files_len = gcx.documents_state.workspace_files.lock().unwrap().len();
    assert_eq!(workspace_files_len, 0);
    assert_eq!(cache_dirty_value(&gcx).await, 0.0);
}

#[tokio::test]
async fn on_did_delete_ignores_refact_import_internal_paths() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp
        .path()
        .join(".refact")
        .join("imports")
        .join("competitors.json");
    let mut doc = Document::new(&path);
    doc.update_text(&"{}".to_string());
    {
        gcx.documents_state
            .memory_document_map
            .lock()
            .await
            .insert(path.clone(), Arc::new(ARwLock::new(doc)));
    }

    on_did_delete(gcx.clone(), &path).await;

    let mdm = gcx.documents_state.memory_document_map.clone();
    let has_doc = mdm.lock().await.contains_key(&path);
    assert!(has_doc);
    assert_eq!(cache_dirty_value(&gcx).await, 0.0);
}

#[tokio::test]
async fn on_did_open_keeps_refact_skills_paths() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp
        .path()
        .join(".refact")
        .join("skills")
        .join("example")
        .join("SKILL.md");
    let text = "# Example skill".to_string();
    let language_id = "markdown".to_string();

    on_did_open(gcx.clone(), &path, &text, &language_id).await;

    let (mdm3, afp3) = {
        (
            gcx.documents_state.memory_document_map.clone(),
            gcx.documents_state.active_file_path.clone(),
        )
    };
    let has_doc = mdm3.lock().await.contains_key(&path);
    let active_file_path = afp3.lock().await.clone();
    assert!(has_doc);
    assert_eq!(active_file_path, Some(path));
    assert!(cache_dirty_value(&gcx).await > 0.0);
}

fn write_head(repo_path: &Path, content: &str) {
    let git_dir = repo_path.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(git_dir.join("HEAD"), content).unwrap();
}

fn git_signature() -> git2::Signature<'static> {
    git2::Signature::now("test", "test@test.com").unwrap()
}

fn git_commit_all(repo: &git2::Repository, message: &str) -> git2::Oid {
    let sig = git_signature();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.update_all(["*"].iter(), None).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .and_then(|oid| repo.find_commit(oid).ok());
    match parent {
        Some(parent) => repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
            .unwrap(),
        None => repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
            .unwrap(),
    }
}

fn git_checkout_branch(repo: &git2::Repository, branch: &str) {
    repo.set_head(&format!("refs/heads/{branch}")).unwrap();
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_head(Some(&mut checkout)).unwrap();
}

#[test]
fn is_git_head_path_detects_head() {
    assert!(is_git_head_path(Path::new("/project/.git/HEAD")));
    assert!(!is_git_head_path(Path::new("/project/.git/config")));
    assert!(!is_git_head_path(Path::new("/project/src/HEAD")));
    assert!(!is_git_head_path(Path::new("/project/.git")));
}

#[test]
fn read_git_head_returns_trimmed_content() {
    let temp = tempfile::tempdir().unwrap();
    write_head(temp.path(), "ref: refs/heads/main\n");
    let head = read_git_head(temp.path());
    assert_eq!(head, Some("ref: refs/heads/main".to_string()));
}

#[test]
fn read_git_head_returns_none_for_missing_repo() {
    let temp = tempfile::tempdir().unwrap();
    let head = read_git_head(temp.path());
    assert!(head.is_none());
}

#[tokio::test]
async fn debounced_duplicate_events_single_enqueue() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("src").join("lib.rs");
    write_file(&file, "pub fn debounce() -> &'static str { \"first\" }\n");
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![normalized(temp.path())];
    *gcx.documents_state.workspace_files.lock().unwrap() = vec![normalized(&file)];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    *gcx.codegraph.lock().await = Some(service.clone());

    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(file.clone());
    file_watcher_event(event, Arc::downgrade(&gcx)).await;
    write_file(&file, "pub fn debounce() -> &'static str { \"second\" }\n");
    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(file.clone());
    file_watcher_event(event, Arc::downgrade(&gcx)).await;

    assert_eq!(service.queue_len(), 0);
    tokio::time::sleep(FILE_EVENT_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    assert_eq!(service.queue_len(), 1);
    let batch = service.drain_batch(10);
    let indexed_path = normalized(&file).to_string_lossy().to_string();
    assert_eq!(batch, vec![indexed_path.clone()]);
    let text = get_file_text_from_memory_or_disk(gcx.clone(), &PathBuf::from(&indexed_path))
        .await
        .unwrap();
    service
        .index_file(&indexed_path, &text, "rust")
        .await
        .unwrap();
    let mut files = service.all_files_with_text().await.unwrap();
    files.sort();
    assert_eq!(
        files,
        vec![(
            normalized(&file).to_string_lossy().to_string(),
            "pub fn debounce() -> &'static str { \"second\" }\n".to_string(),
        ),]
    );
}

#[tokio::test]
async fn debounced_worktree_edit_flushes_source_store_with_worktree_read_path() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    std::fs::create_dir_all(source.join("src")).unwrap();
    // Canonicalize while the dir exists so raw fixture paths match canonical
    // index keys on Windows runners with an 8.3 short TMP (RUNNER~1).
    let source = normalized(&source);
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn source() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree() {}\n");
    let worktree = normalized(&worktree);
    let worktree_file = worktree.join("src").join("lib.rs");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![worktree.clone()];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    *gcx.codegraph.lock().await = Some(service.clone());

    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(worktree_file.clone());
    file_watcher_event(event, Arc::downgrade(&gcx)).await;
    tokio::time::sleep(FILE_EVENT_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    let queued = service.drain_batch_entries(10);
    assert_eq!(
        queued,
        vec![QueuedPath::new(
            normalized(&source_file).to_string_lossy().to_string(),
            normalized(&worktree_file).to_string_lossy().to_string(),
        )]
    );
    crate::codegraph::cg_highlev::process_index_batch(gcx.clone(), service.clone(), queued)
        .await;
    assert_eq!(
        service.all_files_with_text().await.unwrap(),
        vec![(
            normalized(&source_file).to_string_lossy().to_string(),
            "fn worktree() {}\n".to_string(),
        )]
    );
}

#[tokio::test]
async fn debounced_worktree_remove_flushes_source_store_remove() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    std::fs::create_dir_all(source.join("src")).unwrap();
    // Canonicalize while the dir exists so raw fixture paths match canonical
    // index keys on Windows runners with an 8.3 short TMP (RUNNER~1).
    let source = normalized(&source);
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn source() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree() {}\n");
    let worktree = normalized(&worktree);
    let worktree_file = worktree.join("src").join("lib.rs");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![worktree.clone()];
    *gcx.documents_state.workspace_files.lock().unwrap() = vec![normalized(&source_file)];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    let source_key = normalized(&source_file).to_string_lossy().to_string();
    service
        .index_file(&source_key, "fn indexed() {}\n", "rust")
        .await
        .unwrap();
    *gcx.codegraph.lock().await = Some(service.clone());

    std::fs::remove_file(&worktree_file).unwrap();
    let event = notify::Event::new(notify::EventKind::Remove(notify::event::RemoveKind::File))
        .add_path(worktree_file.clone());
    file_watcher_event(event, Arc::downgrade(&gcx)).await;
    tokio::time::sleep(FILE_EVENT_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    assert_eq!(service.counts().await.unwrap().files, 0);
    assert_eq!(
        service.drain_batch_entries(10),
        vec![QueuedPath::new(
            source_key,
            normalized(&worktree_file).to_string_lossy().to_string(),
        )]
    );
    let workspace_files = gcx.documents_state.workspace_files.lock().unwrap().clone();
    assert!(!workspace_files.contains(&normalized(&source_file)));
    assert!(source_file.exists());
}

#[test]
fn debounced_file_event_preserves_worktree_read_path_under_source_key() {
    let temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = temp.path().join("source");
    let cache_dir = temp.path().join("cache");
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn source() {}\n");
    let worktree = worktree_root(&cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree() {}\n");
    write_worktree_registry(&cache_dir, &source, &worktree);
    let mappings = crate::files_correction::registered_worktree_path_mappings(&cache_dir);
    let queue_path = resolve_codegraph_queue_path(&worktree_file, &mappings);
    let mut debounce = HashMap::new();

    debounce.insert(
        PathBuf::from(&queue_path.store_path),
        DebouncedFileEvent {
            read_path: PathBuf::from(&queue_path.read_path),
            at: Instant::now(),
            kind: DebouncedFileEventKind::Upsert,
        },
    );

    let event = debounce
        .remove(&normalized(&source_file))
        .expect("debounce key should be canonical source path");
    assert_eq!(event.read_path, normalized(&worktree_file));
}

#[tokio::test]
async fn worktree_remove_event_preserves_remove_kind_when_source_exists() {
    let source_temp = tempfile::Builder::new()
        .prefix("refact-src-")
        .tempdir()
        .unwrap();
    let source = source_temp.path().join("source");
    std::fs::create_dir_all(source.join("src")).unwrap();
    // Canonicalize while the dir exists so raw fixture paths match canonical
    // index keys on Windows runners with an 8.3 short TMP (RUNNER~1).
    let source = normalized(&source);
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let source_file = source.join("src").join("lib.rs");
    write_file(&source_file, "fn source() {}\n");
    let worktree = worktree_root(&gcx.cache_dir, &source, "wt");
    let worktree_file = worktree.join("src").join("lib.rs");
    write_file(&worktree_file, "fn worktree() {}\n");
    let worktree = normalized(&worktree);
    let worktree_file = worktree.join("src").join("lib.rs");
    write_worktree_registry(&gcx.cache_dir, &source, &worktree);
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![worktree.clone()];

    let event = notify::Event::new(notify::EventKind::Remove(notify::event::RemoveKind::File))
        .add_path(worktree_file.clone());
    file_watcher_event(event, Arc::downgrade(&gcx)).await;

    let event = gcx
        .documents_state
        .file_event_debounce
        .lock()
        .unwrap()
        .get(&normalized(&source_file))
        .cloned()
        .expect("remove event should be keyed by canonical source path");
    assert_eq!(event.kind, DebouncedFileEventKind::Remove);
    assert_eq!(event.read_path, normalized(&worktree_file));
    gcx.shutdown_flag.store(true, Ordering::Relaxed);
    gcx.documents_state.file_event_debounce_notify.notify_one();
}
#[tokio::test]
async fn debounced_file_event_worker_is_bounded_and_flushes_pending_on_shutdown() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    allow_all_privacy(&gcx);
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("src").join("first.rs");
    let second = temp.path().join("src").join("second.rs");
    write_file(&first, "pub fn first() {}\n");
    write_file(&second, "pub fn second() {}\n");
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![normalized(temp.path())];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    *gcx.codegraph.lock().await = Some(service.clone());

    let now = Instant::now();
    schedule_debounced_file_event(
        gcx.clone(),
        QueuedPath::new(
            normalized(&first).to_string_lossy().to_string(),
            normalized(&first).to_string_lossy().to_string(),
        ),
        DebouncedFileEventKind::Upsert,
        now,
    );
    schedule_debounced_file_event(
        gcx.clone(),
        QueuedPath::new(
            normalized(&second).to_string_lossy().to_string(),
            normalized(&second).to_string_lossy().to_string(),
        ),
        DebouncedFileEventKind::Upsert,
        now,
    );
    {
        let task = gcx.documents_state.file_event_debounce_task.lock().unwrap();
        assert!(task.is_some());
    }
    assert_eq!(service.queue_len(), 0);

    gcx.shutdown_flag.store(true, Ordering::Relaxed);
    gcx.documents_state.file_event_debounce_notify.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if service.queue_len() == 2
                && gcx
                    .documents_state
                    .file_event_debounce
                    .lock()
                    .unwrap()
                    .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let mut queued = service.drain_batch(10);
    queued.sort();
    assert_eq!(
        queued,
        vec![
            normalized(&first).to_string_lossy().to_string(),
            normalized(&second).to_string_lossy().to_string(),
        ]
    );
    assert!(gcx
        .documents_state
        .file_event_debounce_task
        .lock()
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn branch_switch_enqueues_only_changed_files() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let changed = temp.path().join("src").join("changed.rs");
    let unchanged = temp.path().join("src").join("unchanged.rs");
    write_file(&changed, "pub fn changed() -> &'static str { \"main\" }\n");
    write_file(&unchanged, "pub fn unchanged() {}\n");
    let main_oid = git_commit_all(&repo, "main");
    {
        let main_commit = repo.find_commit(main_oid).unwrap();
        repo.branch("dev", &main_commit, false).unwrap();
    }
    git_checkout_branch(&repo, "dev");
    write_file(&changed, "pub fn changed() -> &'static str { \"dev\" }\n");
    git_commit_all(&repo, "dev");
    git_checkout_branch(&repo, "main");

    let canonical_repo = normalized(temp.path());
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![canonical_repo.clone()];
    *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![canonical_repo.clone()];
    *gcx.documents_state.workspace_files.lock().unwrap() =
        vec![normalized(&changed), normalized(&unchanged)];
    {
        let mut heads = gcx.documents_state.git_branch_heads.lock().unwrap();
        heads.insert(canonical_repo.clone(), "ref: refs/heads/main".to_string());
    }
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    *gcx.codegraph.lock().await = Some(service.clone());

    git_checkout_branch(&repo, "dev");
    let head_path = temp.path().join(".git").join("HEAD");
    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(head_path);
    on_git_head_change(Arc::downgrade(&gcx), event).await;
    tokio::time::sleep(BRANCH_HEAD_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    assert_eq!(
        service.drain_batch(10),
        vec![normalized(&changed).to_string_lossy().to_string()]
    );
}

#[tokio::test]
async fn branch_switch_enqueues_deleted_files_and_removes_old_index() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let deleted = temp.path().join("src").join("deleted.rs");
    let kept = temp.path().join("src").join("kept.rs");
    write_file(&deleted, "pub fn deleted() {}\n");
    write_file(&kept, "pub fn kept() {}\n");
    let main_oid = git_commit_all(&repo, "main");
    {
        let main_commit = repo.find_commit(main_oid).unwrap();
        repo.branch("dev", &main_commit, false).unwrap();
    }
    git_checkout_branch(&repo, "dev");
    std::fs::remove_file(&deleted).unwrap();
    git_commit_all(&repo, "dev deletes file");
    git_checkout_branch(&repo, "main");

    let canonical_repo = normalized(temp.path());
    let deleted_path = normalized(&deleted).to_string_lossy().to_string();
    let kept_path = normalized(&kept);
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![canonical_repo.clone()];
    *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![canonical_repo.clone()];
    *gcx.documents_state.workspace_files.lock().unwrap() =
        vec![PathBuf::from(&deleted_path), kept_path.clone()];
    {
        let mut heads = gcx.documents_state.git_branch_heads.lock().unwrap();
        heads.insert(canonical_repo, "ref: refs/heads/main".to_string());
    }
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    service
        .index_file(&deleted_path, "pub fn deleted() {}\n", "rust")
        .await
        .unwrap();
    *gcx.codegraph.lock().await = Some(service.clone());

    git_checkout_branch(&repo, "dev");
    let head_path = temp.path().join(".git").join("HEAD");
    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(head_path);
    on_git_head_change(Arc::downgrade(&gcx), event).await;
    tokio::time::sleep(BRANCH_HEAD_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    assert_eq!(service.counts().await.unwrap().files, 0);
    assert_eq!(service.drain_batch(10), vec![deleted_path.clone()]);
    let workspace_files = gcx.documents_state.workspace_files.lock().unwrap().clone();
    assert!(!workspace_files.contains(&PathBuf::from(&deleted_path)));
    assert!(workspace_files.contains(&kept_path));
}

#[tokio::test]
async fn branch_switch_diff_failure_falls_back_to_full_workspace_reindex() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("src").join("fallback.rs");
    write_file(&file, "pub fn fallback() {}\n");
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![normalized(temp.path())];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    *gcx.codegraph.lock().await = Some(service.clone());

    enqueue_branch_head_changes(
        gcx.clone(),
        vec![BranchHeadChange {
            repo_path: temp.path().join("not-a-repo"),
            old_head: Some("missing-old".to_string()),
            new_head: Some("missing-new".to_string()),
        }],
    )
    .await;

    let indexed_path = normalized(&file).to_string_lossy().to_string();
    assert_eq!(service.drain_batch(10), vec![indexed_path.clone()]);
    assert!(gcx
        .documents_state
        .workspace_files
        .lock()
        .unwrap()
        .contains(&PathBuf::from(indexed_path)));
}

#[tokio::test]
async fn close_write_event_routes_to_file_change() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("src").join("close_write.rs");
    write_file(&file, "pub fn close_write() {}\n");
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![normalized(temp.path())];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    *gcx.codegraph.lock().await = Some(service.clone());

    let event = notify::Event::new(notify::EventKind::Access(AccessKind::Close(
        AccessMode::Write,
    )))
    .add_path(file.clone());
    file_watcher_event(event, Arc::downgrade(&gcx)).await;
    tokio::time::sleep(FILE_EVENT_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    assert_eq!(
        service.drain_batch(10),
        vec![normalized(&file).to_string_lossy().to_string()]
    );
}

#[tokio::test]
async fn rename_remove_create_removes_old_and_enqueues_new() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    // Canonical root first: raw event paths must equal canonical index keys
    // on Windows runners with an 8.3 short TMP.
    let root = normalized(temp.path());
    std::fs::create_dir_all(root.join("src")).unwrap();
    let old_file = root.join("src").join("old.rs");
    let new_file = root.join("src").join("new.rs");
    write_file(&old_file, "pub fn old_name() {}\n");
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.clone()];
    *gcx.documents_state.workspace_files.lock().unwrap() = vec![normalized(&old_file)];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    service
        .index_file(
            &normalized(&old_file).to_string_lossy(),
            "pub fn old_name() {}\n",
            "rust",
        )
        .await
        .unwrap();
    assert_eq!(service.counts().await.unwrap().files, 1);
    *gcx.codegraph.lock().await = Some(service.clone());

    std::fs::remove_file(&old_file).unwrap();
    let remove_event =
        notify::Event::new(notify::EventKind::Remove(notify::event::RemoveKind::File))
            .add_path(old_file.clone());
    file_watcher_event(remove_event, Arc::downgrade(&gcx)).await;
    write_file(&new_file, "pub fn new_name() {}\n");
    let create_event =
        notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
            .add_path(new_file.clone());
    file_watcher_event(create_event, Arc::downgrade(&gcx)).await;
    tokio::time::sleep(FILE_EVENT_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    let workspace_files = gcx.documents_state.workspace_files.lock().unwrap().clone();
    assert!(!workspace_files.contains(&normalized(&old_file)));
    assert!(workspace_files.contains(&normalized(&new_file)));
    assert_eq!(service.counts().await.unwrap().files, 0);
    assert_eq!(
        service.drain_batch(10),
        vec![
            normalized(&old_file).to_string_lossy().to_string(),
            normalized(&new_file).to_string_lossy().to_string()
        ]
    );
}

#[tokio::test]
async fn branch_head_change_triggers_reindex() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let file = temp.path().join("src").join("lib.rs");
    write_file(&file, "pub fn branch_test() -> &'static str { \"main\" }\n");
    let main_oid = git_commit_all(&repo, "main");
    {
        let main_commit = repo.find_commit(main_oid).unwrap();
        repo.branch("dev", &main_commit, false).unwrap();
    }
    git_checkout_branch(&repo, "dev");
    write_file(&file, "pub fn branch_test() -> &'static str { \"dev\" }\n");
    git_commit_all(&repo, "dev");
    git_checkout_branch(&repo, "main");
    let canonical_repo = normalized(temp.path());
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![canonical_repo.clone()];
    *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![canonical_repo.clone()];
    {
        let mut heads = gcx.documents_state.git_branch_heads.lock().unwrap();
        heads.insert(canonical_repo.clone(), "ref: refs/heads/main".to_string());
    }

    git_checkout_branch(&repo, "dev");
    let head_path = temp.path().join(".git").join("HEAD");
    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(head_path);

    on_git_head_change(Arc::downgrade(&gcx), event).await;
    tokio::time::sleep(BRANCH_HEAD_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    let heads = gcx.documents_state.git_branch_heads.lock().unwrap();
    assert_eq!(
        heads.get(&canonical_repo),
        Some(&"ref: refs/heads/dev".to_string())
    );
    let ts = gcx
        .documents_state
        .branch_reindex_last_ts
        .load(Ordering::Relaxed);
    assert!(
        ts > 0,
        "reindex timestamp should be set after branch change"
    );
}

#[tokio::test]
async fn no_reindex_when_head_unchanged() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let canonical_repo = normalized(temp.path());

    write_head(temp.path(), "ref: refs/heads/main\n");
    {
        let mut heads = gcx.documents_state.git_branch_heads.lock().unwrap();
        heads.insert(canonical_repo.clone(), "ref: refs/heads/main".to_string());
    }

    let head_path = temp.path().join(".git").join("HEAD");
    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(head_path);

    on_git_head_change(Arc::downgrade(&gcx), event).await;
    tokio::time::sleep(BRANCH_HEAD_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    let ts = gcx
        .documents_state
        .branch_reindex_last_ts
        .load(Ordering::Relaxed);
    assert_eq!(
        ts, 0,
        "no reindex should occur when HEAD content is unchanged"
    );
}

#[tokio::test]
async fn debounce_rapid_head_changes_flushes_newest_head() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let temp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let file = temp.path().join("src").join("lib.rs");
    write_file(&file, "pub fn branch_test() -> &'static str { \"main\" }\n");
    let main_oid = git_commit_all(&repo, "main");
    {
        let main_commit = repo.find_commit(main_oid).unwrap();
        repo.branch("dev", &main_commit, false).unwrap();
        repo.branch("feature", &main_commit, false).unwrap();
    }
    git_checkout_branch(&repo, "dev");
    write_file(&file, "pub fn branch_test() -> &'static str { \"dev\" }\n");
    git_commit_all(&repo, "dev");
    git_checkout_branch(&repo, "feature");
    write_file(
        &file,
        "pub fn branch_test() -> &'static str { \"feature\" }\n",
    );
    git_commit_all(&repo, "feature");
    git_checkout_branch(&repo, "main");
    let canonical_repo = normalized(temp.path());

    {
        let mut heads = gcx.documents_state.git_branch_heads.lock().unwrap();
        heads.insert(canonical_repo.clone(), "ref: refs/heads/main".to_string());
    }
    *gcx.documents_state.workspace_folders.lock().unwrap() = vec![canonical_repo.clone()];
    *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![canonical_repo.clone()];
    *gcx.documents_state.workspace_files.lock().unwrap() = vec![normalized(&file)];
    let service = Arc::new(crate::codegraph::CodeGraphService::open_in_memory().unwrap());
    *gcx.codegraph.lock().await = Some(service.clone());

    git_checkout_branch(&repo, "dev");
    let head_path = temp.path().join(".git").join("HEAD");
    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(head_path);

    on_git_head_change(Arc::downgrade(&gcx), event).await;
    git_checkout_branch(&repo, "feature");
    let head_path = temp.path().join(".git").join("HEAD");
    let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(head_path);
    on_git_head_change(Arc::downgrade(&gcx), event).await;
    tokio::time::sleep(BRANCH_HEAD_DEBOUNCE_WINDOW + Duration::from_millis(80)).await;

    let heads = gcx.documents_state.git_branch_heads.lock().unwrap();
    assert_eq!(
        heads.get(&canonical_repo),
        Some(&"ref: refs/heads/feature".to_string()),
        "debounce should keep latest head from rapid changes"
    );
    assert_eq!(
        service.drain_batch(10),
        vec![normalized(&file).to_string_lossy().to_string()],
        "debounce should enqueue the final branch diff once"
    );
}

#[test]
fn registry_reload_ignores_refact_import_paths() {
    assert!(path_is_refact_import_internal(Path::new(
        "/repo/.refact/imports/competitors.json"
    )));
    assert!(!path_is_refact_import_internal(Path::new(
        "/repo/.refact/skills/example/SKILL.md"
    )));
    assert!(!path_triggers_registry_reload(Path::new(
        "/repo/.refact/imports/staging/source/.refact/subagents/agent.yaml"
    )));
    assert!(!path_triggers_registry_reload(Path::new(
        "/repo/.refact/codegraph/codegraph.sqlite-wal"
    )));
    assert!(path_triggers_registry_reload(Path::new(
        "/repo/.refact/modes/agent.yaml"
    )));
    assert!(path_triggers_registry_reload(Path::new(
        "/repo/.refact/subagents/agent.yaml"
    )));
    assert!(path_triggers_registry_reload(Path::new(
        "/repo/.refact/toolbox_commands/command.yaml"
    )));
    assert!(path_triggers_registry_reload(Path::new(
        "/repo/.refact/code_lens/lens.yaml"
    )));
}
