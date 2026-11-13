pub use refact_tools::tools::tool_glob::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use glob::Pattern;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicBool;

    fn permissive_privacy() -> Arc<refact_core::privacy_types::PrivacySettings> {
        Arc::new(refact_core::privacy_types::PrivacySettings {
            privacy_rules: refact_core::privacy_types::FilePrivacySettings {
                only_send_to_servers_I_control: Vec::new(),
                blocked: Vec::new(),
            },
            loaded_ts: u64::MAX / 2,
        })
    }

    fn pat(p: &str) -> Pattern {
        Pattern::new(p).unwrap()
    }

    #[test]
    fn matches_by_filename_at_any_depth() {
        let opts = glob_match_options();
        let p = pat("*.rs");
        assert!(path_matches_glob(&p, "/root/src/deep/lib.rs", &opts));
        assert!(path_matches_glob(&p, "main.rs", &opts));
        assert!(!path_matches_glob(&p, "/root/src/lib.txt", &opts));
    }

    #[test]
    fn star_does_not_cross_path_separator_but_doublestar_does() {
        let opts = glob_match_options();
        assert!(!pat("src/*.rs").matches_with("src/a/b.rs", opts));
        assert!(pat("src/*.rs").matches_with("src/b.rs", opts));
        assert!(pat("src/**/*.rs").matches_with("src/a/b/c.rs", opts));
    }

    #[test]
    fn content_only_nonmatch_is_ignored() {
        let temp = tempfile::Builder::new()
            .prefix("refact-glob-content-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("notes.txt");
        fs::write(&file, "this file mentions needle.rs inside its body").unwrap();

        let files = vec![file.to_string_lossy().to_string()];
        let outcome = run_glob(
            &pat("*.rs"),
            &files,
            100,
            permissive_privacy(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            outcome.matches.is_empty(),
            "path-only glob must not match a .txt file even if its contents mention .rs"
        );
    }

    #[test]
    fn respects_scope_containment() {
        let files = vec!["/scope/a/one.rs".to_string(), "/scope/a/two.rs".to_string()];
        let outcome = run_glob(
            &pat("**/*.rs"),
            &files,
            100,
            permissive_privacy(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            outcome.matches,
            vec!["/scope/a/one.rs".to_string(), "/scope/a/two.rs".to_string()]
        );
    }

    #[test]
    fn privacy_blocks_paths_from_output() {
        let privacy = Arc::new(refact_core::privacy_types::PrivacySettings {
            privacy_rules: refact_core::privacy_types::FilePrivacySettings {
                only_send_to_servers_I_control: Vec::new(),
                blocked: vec!["**/*.pem".to_string()],
            },
            loaded_ts: u64::MAX / 2,
        });
        let files = vec![
            "/scope/keep.rs".to_string(),
            "/scope/secret.pem".to_string(),
        ];
        let outcome =
            run_glob(&pat("**/*"), &files, 100, privacy, &AtomicBool::new(false)).unwrap();
        assert!(outcome.matches.contains(&"/scope/keep.rs".to_string()));
        assert!(
            !outcome.matches.iter().any(|p| p.ends_with(".pem")),
            "privacy-blocked path must be excluded from glob output"
        );
    }

    #[test]
    fn oversized_files_are_not_read() {
        let temp = tempfile::Builder::new()
            .prefix("refact-glob-oversized-")
            .tempdir()
            .unwrap();
        let file = temp.path().join("huge.rs");
        fs::write(&file, vec![b'a'; 8 * 1024 * 1024]).unwrap();
        let files = vec![file.to_string_lossy().to_string()];
        let outcome = run_glob(
            &pat("*.rs"),
            &files,
            100,
            permissive_privacy(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome.matches.len(), 1, "oversized file matched by path");
    }

    #[test]
    fn truncation_is_reported() {
        let files: Vec<String> = (0..10).map(|i| format!("/scope/file_{i:02}.rs")).collect();
        let outcome = run_glob(
            &pat("**/*.rs"),
            &files,
            3,
            permissive_privacy(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome.matches.len(), 3);
        assert_eq!(outcome.total_matched, 10);
        assert!(outcome.truncated);
        assert_eq!(
            outcome.matches,
            vec![
                "/scope/file_00.rs".to_string(),
                "/scope/file_01.rs".to_string(),
                "/scope/file_02.rs".to_string()
            ]
        );
    }

    #[test]
    fn respects_preset_abort_flag() {
        let files: Vec<String> = (0..2000).map(|i| format!("/s/f{i}.rs")).collect();
        let result = run_glob(
            &pat("**/*.rs"),
            &files,
            100,
            permissive_privacy(),
            &AtomicBool::new(true),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("aborted"));
    }

    #[test]
    fn parse_max_results_clamps_and_defaults() {
        let default = HashMap::new();
        assert_eq!(parse_max_results(&default).unwrap(), DEFAULT_MAX_RESULTS);
        let high = HashMap::from_iter([("max_results".to_string(), Value::from(100000))]);
        assert_eq!(parse_max_results(&high).unwrap(), CAP_MAX_RESULTS);
        let zero = HashMap::from_iter([("max_results".to_string(), Value::from(0))]);
        assert_eq!(parse_max_results(&zero).unwrap(), 1);
        let bad =
            HashMap::from_iter([("max_results".to_string(), Value::String("lots".to_string()))]);
        assert!(parse_max_results(&bad).is_err());
    }
}
