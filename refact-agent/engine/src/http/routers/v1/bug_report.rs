pub use refact_http::routers::v1::bug_report::*;

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[tokio::test]
    async fn errors_scan_newest_first_errors_before_warns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.log");
        tokio::fs::write(
            &path,
            concat!(
                "141901.700 INFO src/chat/trajectories.rs:1 hello\n",
                "141901.710 WARN src/chat/trajectories.rs:2 older warn\n",
                "141901.720 ERROR src/chat/trajectories.rs:3 older error\n",
                "141901.730 WARN src/chat/trajectories.rs:4 newest warn\n",
                "141901.740 ERROR src/chat/trajectories.rs:5 newest error\n",
            ),
        )
        .await
        .unwrap();
        let source = ResolvedLogSource {
            source: "engine",
            path,
            exists: true,
        };

        let errors = scan_recent_errors(&[source], 3).await;

        assert_eq!(errors[0].level, "error");
        assert_eq!(errors[0].message, "newest error");
        assert_eq!(errors[0].count, 1);
        assert_eq!(
            errors[0].location,
            Some("src/chat/trajectories.rs:5".to_string())
        );
        assert_eq!(errors[1].level, "error");
        assert_eq!(errors[1].message, "older error");
        assert_eq!(errors[2].level, "warn");
        assert_eq!(errors[2].message, "newest warn");
    }

    #[tokio::test]
    async fn parse_engine_format_extracts_level_location_and_ignores_info_message_error() {
        let parsed = parse_log_line(
            "141901.740 WARN src/chat/trajectories.rs:1498 skipping unsupported task",
        )
        .unwrap();
        assert_eq!(parsed.level, "warn");
        assert_eq!(
            parsed.location,
            Some("src/chat/trajectories.rs:1498".to_string())
        );

        let parsed_info = parse_log_line(
            "141902.001 INFO src/vecdb/vdb_highlev.rs:51 message contains ERROR token",
        )
        .unwrap();
        assert_eq!(parsed_info.level, "info");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.log");
        tokio::fs::write(
            &path,
            "141902.001 INFO src/vecdb/vdb_highlev.rs:51 message contains ERROR token\n",
        )
        .await
        .unwrap();
        let source = ResolvedLogSource {
            source: "engine",
            path,
            exists: true,
        };

        assert!(scan_recent_errors(&[source], 10).await.is_empty());
    }

    #[test]
    fn parse_daemon_format_extracts_level_and_location() {
        let parsed = parse_log_line(
            "2026-07-14T06:48:51.702625Z  WARN refact_lsp::daemon::cron_clock: tick failed",
        )
        .unwrap();

        assert_eq!(parsed.level, "warn");
        assert_eq!(
            parsed.location,
            Some("refact_lsp::daemon::cron_clock".to_string())
        );
    }

    #[tokio::test]
    async fn continuation_lines_attach_to_preceding_error_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.log");
        tokio::fs::write(
            &path,
            concat!(
                "141901.740 ERROR src/chat/trajectories.rs:1498 failed request\n",
                "--- HTTP build_info starts ---\n",
                "    /tmp/refact/path\n",
                "stack trace line\n",
                "fourth line is ignored\n",
                "141901.750 INFO src/chat/trajectories.rs:1499 recovered\n",
            ),
        )
        .await
        .unwrap();
        let source = ResolvedLogSource {
            source: "engine",
            path,
            exists: true,
        };

        let errors = scan_recent_errors(&[source], 10).await;

        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].message,
            "failed request\n--- HTTP build_info starts ---\n    /tmp/refact/path\nstack trace line"
        );
    }

    #[tokio::test]
    async fn dedupes_repeated_warns_and_counts_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.log");
        tokio::fs::write(
            &path,
            concat!(
                "141901.740 WARN src/tasks.rs:10 task cron_123 skipped\n",
                "141901.741 WARN src/tasks.rs:10 task cron_456 skipped\n",
                "141901.742 WARN src/tasks.rs:10 task cron_789 skipped\n",
            ),
        )
        .await
        .unwrap();
        let source = ResolvedLogSource {
            source: "engine",
            path,
            exists: true,
        };

        let errors = scan_recent_errors(&[source], 10).await;

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].level, "warn");
        assert_eq!(errors[0].message, "task cron_789 skipped");
        assert_eq!(errors[0].count, 3);
        assert_eq!(errors[0].location, Some("src/tasks.rs:10".to_string()));
    }

    #[tokio::test]
    async fn fallback_token_scan_detects_plain_error_without_location() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.log");
        tokio::fs::write(&path, "something ERROR happened\n")
            .await
            .unwrap();
        let source = ResolvedLogSource {
            source: "engine",
            path,
            exists: true,
        };

        let errors = scan_recent_errors(&[source], 10).await;

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].level, "error");
        assert_eq!(errors[0].message, "something ERROR happened");
        assert_eq!(errors[0].location, None);
    }

    #[tokio::test]
    async fn bundle_contains_redacted_engine_log_and_system_info() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        let dest_dir = dir.path().join("bundles");
        tokio::fs::create_dir_all(&cache_dir).await.unwrap();
        let engine_path = cache_dir.join("engine.log");
        tokio::fs::write(&engine_path, "api_key=sk-abc123defgh456ijklmn0000\n")
            .await
            .unwrap();
        let engine = ResolvedLogSource {
            source: "engine",
            path: engine_path,
            exists: true,
        };
        let daemon = ResolvedLogSource {
            source: "daemon",
            path: cache_dir.join("missing-daemon.log"),
            exists: false,
        };
        let context = build_context_response_from_parts(
            8001,
            cache_dir.clone(),
            dir.path().join("config"),
            Vec::new(),
            engine.clone(),
            daemon.clone(),
        );

        let response = build_bug_report_bundle(BugReportBundleInput {
            dest_dir,
            context,
            engine_log: engine,
            daemon_log: daemon,
            redact: true,
            webui_lines: None,
            ide_lines: None,
        })
        .await
        .unwrap();

        assert!(Path::new(&response.path).exists());
        let file = std::fs::File::open(&response.path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut engine_log = String::new();
        archive
            .by_name("engine.log")
            .unwrap()
            .read_to_string(&mut engine_log)
            .unwrap();
        assert!(!engine_log.contains("sk-abc123defgh456ijklmn0000"));
        assert!(engine_log.contains("[REDACTED]"));
        archive.by_name("system_info.json").unwrap();
    }

    #[test]
    fn tail_clamp_bounds() {
        assert_eq!(clamp_tail_lines(Some(0)), 1);
        assert_eq!(clamp_tail_lines(Some(99_999)), 2000);
        assert_eq!(clamp_tail_lines(None), 200);
    }

    #[test]
    fn dest_dir_validation() {
        let cache_dir = Path::new("/tmp/refact-cache");
        assert_eq!(
            expand_dest_dir(None, cache_dir).unwrap(),
            cache_dir.join("bug-reports")
        );
        #[cfg(unix)]
        {
            assert_eq!(
                expand_dest_dir(Some("/tmp/bundles".to_string()), cache_dir).unwrap(),
                PathBuf::from("/tmp/bundles")
            );
        }
        assert!(expand_dest_dir(Some("relative/dir".to_string()), cache_dir).is_err());
        assert!(expand_dest_dir(Some("/tmp/ok/../escape".to_string()), cache_dir).is_err());
    }
}
