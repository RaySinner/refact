pub use refact_runtime_api::codegraph_highlev::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use refact_codegraph::{CodeGraphService, Counts, QueuedPath};

    use crate::global_context::GlobalContext;
    use crate::privacy::{FilePrivacySettings, PrivacySettings};

    fn set_privacy(gcx: &Arc<GlobalContext>, blocked: Vec<String>) {
        let loaded_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: Vec::new(),
                blocked,
            },
            loaded_ts,
        });
    }

    #[test]
    fn wal_truncate_is_rate_limited_to_the_minimum_interval() {
        let now = Instant::now();
        assert!(should_truncate_wal(None, now));
        assert!(!should_truncate_wal(Some(now), now));
        assert!(!should_truncate_wal(
            Some(now),
            now + Duration::from_secs(WAL_TRUNCATE_MIN_INTERVAL_SECS - 1)
        ));
        assert!(should_truncate_wal(
            Some(now),
            now + Duration::from_secs(WAL_TRUNCATE_MIN_INTERVAL_SECS)
        ));
    }

    #[tokio::test]
    async fn codegraph_db_path_uses_cache_dir_for_project() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let project = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![project.path().to_path_buf()];

        let path = codegraph_db_path(gcx.clone()).await;

        assert!(path.starts_with(gcx.cache_dir.join("codegraph")));
        assert!(!path.starts_with(project.path().join(".refact")));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(CODEGRAPH_DB_FILE)
        );
    }

    #[test]
    fn progress_latch_reports_only_bucket_changes() {
        let mut reported = 0;

        assert!(should_report_unprocessed(250, &mut reported));
        assert_eq!(reported, 250);
        assert!(!should_report_unprocessed(225, &mut reported));
        assert!(should_report_unprocessed(199, &mut reported));
        assert_eq!(reported, 199);
        assert!(!should_report_unprocessed(0, &mut reported));

        reported = 0;
        assert!(should_report_unprocessed(40, &mut reported));
    }

    #[test]
    fn connect_cadence_uses_batch_or_elapsed_thresholds() {
        assert!(!should_connect_usages(
            CONNECT_EVERY_BATCHES - 1,
            Duration::from_secs(CONNECT_EVERY_SECS - 1)
        ));
        assert!(should_connect_usages(
            CONNECT_EVERY_BATCHES,
            Duration::from_secs(0)
        ));
        assert!(should_connect_usages(
            CONNECT_EVERY_BATCHES + 1,
            Duration::from_secs(0)
        ));
        assert!(should_connect_usages(
            0,
            Duration::from_secs(CONNECT_EVERY_SECS)
        ));
        assert!(should_connect_usages(
            0,
            Duration::from_secs(CONNECT_EVERY_SECS + 1)
        ));
    }

    #[test]
    fn completion_message_includes_counts() {
        let counts = Counts {
            nodes: 11,
            edges: 22,
            files: 3,
            fts_docs: 3,
        };

        assert_eq!(
            completion_message(&counts, 0),
            "codegraph: index complete — 11 nodes, 22 edges, 3 files"
        );
        assert_eq!(
            completion_message(&counts, 4),
            "codegraph: index complete — 11 nodes, 22 edges, 3 files, 4 files failed to parse (symbols may be missing)"
        );
    }

    #[test]
    fn stale_stored_paths_only_removes_paths_absent_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let kept = temp.path().join("src").join("kept.rs");
        let jsonl_only = temp.path().join("jsonl_only.rs");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "fn kept() {}\n").unwrap();
        std::fs::write(&jsonl_only, "fn jsonl_only() {}\n").unwrap();
        let kept = kept.to_string_lossy().to_string();
        let missing = temp
            .path()
            .join("src")
            .join("gone.rs")
            .to_string_lossy()
            .to_string();
        let jsonl_only = jsonl_only.to_string_lossy().to_string();

        let stale = stale_stored_paths(vec![kept, missing.clone(), jsonl_only]);

        assert_eq!(stale, vec![missing]);
    }

    #[tokio::test]
    async fn reconcile_deleted_paths_preserves_live_jsonl_only_entries() {
        let service = Arc::new(CodeGraphService::open_in_memory().unwrap());
        let temp = tempfile::tempdir().unwrap();
        let kept = temp.path().join("src").join("kept.rs");
        let jsonl_only = temp.path().join("jsonl_only.rs");
        let gone = temp.path().join("src").join("gone.rs");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "fn kept() {}\n").unwrap();
        std::fs::write(&jsonl_only, "fn jsonl_only() {}\n").unwrap();
        let kept = kept.to_string_lossy().to_string();
        let jsonl_only = jsonl_only.to_string_lossy().to_string();
        let gone = gone.to_string_lossy().to_string();

        service
            .index_file(&kept, "fn kept() {}\n", "rust")
            .await
            .unwrap();
        service
            .index_file(&jsonl_only, "fn jsonl_only() {}\n", "rust")
            .await
            .unwrap();
        service
            .index_file(&gone, "fn gone() {}\n", "rust")
            .await
            .unwrap();

        let removed = reconcile_deleted_paths(service.clone()).await.unwrap();

        assert_eq!(removed, 1);
        let mut paths = service.all_paths().await.unwrap();
        paths.sort();
        assert_eq!(paths, vec![jsonl_only, kept]);
    }

    #[tokio::test]
    async fn shutdown_set_during_drained_batch_still_indexes_paths() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_privacy(&gcx, Vec::new());
        gcx.shutdown_flag.store(true, Ordering::Relaxed);
        let service = Arc::new(CodeGraphService::open_in_memory().unwrap());
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.rs");
        let second = temp.path().join("second.rs");
        std::fs::write(&first, "fn first() {}\n").unwrap();
        std::fs::write(&second, "fn second() {}\n").unwrap();
        let first = first.to_string_lossy().to_string();
        let second = second.to_string_lossy().to_string();

        process_index_batch(
            gcx,
            service.clone(),
            vec![
                QueuedPath::new(first.clone(), first.clone()),
                QueuedPath::new(second.clone(), second.clone()),
            ],
        )
        .await;

        let mut paths = service.all_paths().await.unwrap();
        paths.sort();
        assert_eq!(paths, vec![first, second]);
    }

    #[tokio::test]
    async fn read_error_for_existing_path_keeps_existing_index() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let service = Arc::new(CodeGraphService::open_in_memory().unwrap());
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("blocked.rs");
        std::fs::write(&file, "fn newer() {}\n").unwrap();
        let file = file.to_string_lossy().to_string();
        service
            .index_file(&file, "fn indexed() {}\n", "rust")
            .await
            .unwrap();
        set_privacy(&gcx, vec![file.clone()]);

        prepare_index_entry_or_remove_path(
            gcx,
            service.clone(),
            QueuedPath::new(file.clone(), file.clone()),
        )
        .await
        .unwrap();

        assert_eq!(service.all_paths().await.unwrap(), vec![file.clone()]);
        assert_eq!(
            service.all_files_with_text().await.unwrap(),
            vec![(file, "fn indexed() {}\n".to_string())]
        );
    }

    #[tokio::test]
    async fn missing_read_path_preserves_live_store_path_index() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_privacy(&gcx, Vec::new());
        let service = Arc::new(CodeGraphService::open_in_memory().unwrap());
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("source").join("src").join("lib.rs");
        let read_path = temp.path().join("worktree").join("src").join("lib.rs");
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(read_path.parent().unwrap()).unwrap();
        std::fs::write(&store_path, "fn source_live() {}\n").unwrap();
        let store_path = store_path.to_string_lossy().to_string();
        let read_path = read_path.to_string_lossy().to_string();
        service
            .index_file(&store_path, "fn indexed_old() {}\n", "rust")
            .await
            .unwrap();

        prepare_index_entry_or_remove_path(
            gcx,
            service.clone(),
            QueuedPath::new(store_path.clone(), read_path),
        )
        .await
        .unwrap();

        assert_eq!(service.all_paths().await.unwrap(), vec![store_path.clone()]);
        assert_eq!(
            service.all_files_with_text().await.unwrap(),
            vec![(store_path, "fn indexed_old() {}\n".to_string())]
        );
    }

    #[tokio::test]
    async fn worktree_read_path_indexes_under_store_path_key() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_privacy(&gcx, Vec::new());
        let service = Arc::new(CodeGraphService::open_in_memory().unwrap());
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("source").join("src").join("lib.rs");
        let read_path = temp.path().join("worktree").join("src").join("lib.rs");
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(read_path.parent().unwrap()).unwrap();
        std::fs::write(&store_path, "fn source() {}\n").unwrap();
        std::fs::write(&read_path, "fn worktree() {}\n").unwrap();
        let store_path = store_path.to_string_lossy().to_string();
        let read_path = read_path.to_string_lossy().to_string();

        let entry = prepare_index_entry_or_remove_path(
            gcx,
            service.clone(),
            QueuedPath::new(store_path.clone(), read_path),
        )
        .await
        .unwrap()
        .unwrap();
        service.index_files_batch(&[entry]).await.unwrap();

        assert_eq!(
            service.all_files_with_text().await.unwrap(),
            vec![(store_path, "fn worktree() {}\n".to_string())]
        );
    }

    #[tokio::test]
    async fn missing_store_path_removes_existing_index() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        set_privacy(&gcx, Vec::new());
        let service = Arc::new(CodeGraphService::open_in_memory().unwrap());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("src").join("gone.rs");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "fn gone() {}\n").unwrap();
        let path = path.to_string_lossy().to_string();
        service
            .index_file(&path, "fn indexed() {}\n", "rust")
            .await
            .unwrap();
        std::fs::remove_file(&path).unwrap();

        prepare_index_entry_or_remove_path(
            gcx,
            service.clone(),
            QueuedPath::new(path.clone(), path.clone()),
        )
        .await
        .unwrap();

        assert!(service.all_paths().await.unwrap().is_empty());
        assert!(service.all_files_with_text().await.unwrap().is_empty());
    }

    #[test]
    fn stale_store_sweep_removes_only_old_unopened_stores() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codegraph");
        let active = root.join("active-hash");
        let old = root.join("old-hash");
        let fresh = root.join("fresh-hash");
        let no_db = root.join("no-db-hash");
        for dir in [&active, &old, &fresh, &no_db] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for dir in [&active, &old, &fresh] {
            std::fs::write(dir.join(CODEGRAPH_DB_FILE), b"db").unwrap();
        }
        std::fs::write(no_db.join("stray.txt"), b"x").unwrap();
        let now = std::time::SystemTime::now();
        let ancient = now - STALE_STORE_MAX_AGE - Duration::from_secs(60 * 60);
        for dir in [&active, &old, &no_db] {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                filetime::set_file_mtime(
                    entry.path(),
                    filetime::FileTime::from_system_time(ancient),
                )
                .unwrap();
            }
        }

        let removed = sweep_stale_codegraph_stores(&active.join(CODEGRAPH_DB_FILE), now);

        assert_eq!(removed, vec![old.clone()]);
        assert!(active.exists());
        assert!(fresh.exists());
        assert!(no_db.exists());
        assert!(!old.exists());
    }

    #[test]
    fn stale_store_sweep_skips_a_directory_whose_lease_is_held() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codegraph");
        let active = root.join("active-hash");
        let held = root.join("held-hash");
        let unheld = root.join("unheld-hash");
        for dir in [&active, &held, &unheld] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(CODEGRAPH_DB_FILE), b"db").unwrap();
        }
        let lease =
            refact_codegraph::store::try_acquire_shared_lease(&held.join(CODEGRAPH_DB_FILE))
                .unwrap()
                .expect("the shared lease must be grantable");
        let now = std::time::SystemTime::now();
        let ancient = now - STALE_STORE_MAX_AGE - Duration::from_secs(60 * 60);
        for dir in [&active, &held, &unheld] {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                filetime::set_file_mtime(
                    entry.path(),
                    filetime::FileTime::from_system_time(ancient),
                )
                .unwrap();
            }
        }

        let removed = sweep_stale_codegraph_stores(&active.join(CODEGRAPH_DB_FILE), now);

        assert_eq!(removed, vec![unheld.clone()]);
        assert!(
            held.exists(),
            "a store another engine still holds must never be swept"
        );
        assert!(!unheld.exists());
        drop(lease);
    }
}
