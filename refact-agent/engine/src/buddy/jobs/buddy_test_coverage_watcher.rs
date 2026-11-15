#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
use crate::buddy::autonomous_workflows::{
    autonomous_workflow_meta, BUDDY_TEST_COVERAGE_WATCHER_WORKFLOW_ID,
};
#[cfg(test)]
use crate::buddy::jobs::autonomous_chats::{
    execute_autonomous_spec, parse_last_autonomous_result, AutonomousBuddyChatSpec,
    AutonomousLastResult,
};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
#[cfg(test)]
use crate::app_state::AppState;

pub use refact_runtime_api::buddy_jobs::buddy_test_coverage_watcher::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::settings::BuddySettings;
    use crate::buddy::types::{BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse};
    use std::path::Path;

    fn test_context(project_root: &Path, last_result: Option<String>) -> BuddyJobContext {
        BuddyJobContext {
            identity_name: "Pixel".to_string(),
            personality: Default::default(),
            onboarding: BuddyOnboarding::default(),
            recent_diagnostics: vec![],
            project_root: project_root.to_path_buf(),
            job_state: BuddyJobState {
                last_result,
                ..Default::default()
            },
            workflow_summaries: vec![],
            total_workflow_runs: 0,
            suggestion_state: vec![],
            pet: BuddyPetState::default(),
            active_quest: None,
            settings: BuddySettings::default(),
            pulse: BuddyPulse::default(),
            facts: vec![],
            recent_activities: vec![],
        }
    }

    fn init_temp_git_repo() -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        {
            let mut index = repo.index().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        (dir, repo)
    }

    #[tokio::test]
    async fn buddy_test_coverage_watcher_detects_missing_tests() {
        let (dir, _repo) = init_temp_git_repo();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("feature.rs"), "pub fn feature() {}\n").unwrap();
        let scan = scan_test_coverage(dir.path());
        let spec = build_test_coverage_spec(&test_context(dir.path(), None), &scan);
        let ctx = test_context(dir.path(), Some(serialize_scan(&spec.signal_hash, &scan)));
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        assert!(BuddyTestCoverageWatcherJob.should_run(gcx, &ctx).await);
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].path, "src/feature.rs");
    }

    #[tokio::test]
    async fn buddy_test_coverage_watcher_keeps_success_cache_shape() {
        let (dir, _repo) = init_temp_git_repo();
        let scan = CoverageScanResult {
            candidates: vec![CoverageCandidate {
                path: "src/feature.rs".to_string(),
                status: "missing_cfg_test_or_tests_dir".to_string(),
            }],
        };
        let spec = build_test_coverage_spec(&test_context(dir.path(), None), &scan);
        let last = AutonomousLastResult {
            signal_hash: spec.signal_hash.clone(),
            chat_id: "chat-a".to_string(),
            completed_at: Utc::now().to_rfc3339(),
            status: None,
            failure_category: None,
            consecutive_failures: None,
        };
        let ctx = test_context(
            dir.path(),
            Some(serialize_scan_with_autonomous(
                &spec.signal_hash,
                &scan,
                &last,
            )),
        );
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        assert!(!BuddyTestCoverageWatcherJob.should_run(gcx, &ctx).await);
        let cache = cached_scan(&ctx).expect("coverage cache parses after success");
        assert_eq!(cache.signal_hash, spec.signal_hash);
        assert_eq!(
            cache.autonomous.as_ref().map(|a| a.chat_id.as_str()),
            Some("chat-a")
        );
    }

    #[tokio::test]
    async fn buddy_test_coverage_watcher_runs_when_fresh_cache_hash_changed_without_success() {
        let (dir, _repo) = init_temp_git_repo();
        let scan = CoverageScanResult {
            candidates: vec![CoverageCandidate {
                path: "src/feature.rs".to_string(),
                status: "missing_cfg_test_or_tests_dir".to_string(),
            }],
        };
        let ctx = test_context(dir.path(), Some(serialize_scan("old-hash", &scan)));
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        assert!(BuddyTestCoverageWatcherJob.should_run(gcx, &ctx).await);
    }

    #[tokio::test]
    async fn buddy_test_coverage_watcher_refreshes_stale_success_cache() {
        let (dir, _repo) = init_temp_git_repo();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("feature.rs"), "pub fn feature() {}\n").unwrap();
        let scan = scan_test_coverage(dir.path());
        let spec = build_test_coverage_spec(&test_context(dir.path(), None), &scan);
        let old_scanned_at = Utc::now().timestamp() - (COOLDOWN_SECONDS as i64 * 2);
        let last = AutonomousLastResult {
            signal_hash: spec.signal_hash.clone(),
            chat_id: "chat-a".to_string(),
            completed_at: Utc::now().to_rfc3339(),
            status: None,
            failure_category: None,
            consecutive_failures: None,
        };
        let stale_cache = CoverageScanCache {
            scanned_at: old_scanned_at,
            signal_hash: spec.signal_hash.clone(),
            scan: scan.clone(),
            autonomous: Some(CoverageAutonomousResult {
                chat_id: last.chat_id.clone(),
                completed_at: last.completed_at.clone(),
            }),
        };
        let ctx = test_context(
            dir.path(),
            Some(serde_json::to_string(&stale_cache).unwrap()),
        );
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        let result = BuddyTestCoverageWatcherJob.execute(gcx, ctx).await;
        let refreshed = serde_json::from_str::<CoverageScanCache>(
            result.last_result.as_deref().expect("refreshed cache"),
        )
        .unwrap();

        assert!(refreshed.scanned_at > old_scanned_at);
        assert_eq!(refreshed.signal_hash, spec.signal_hash);
        assert_eq!(
            refreshed.autonomous.as_ref().map(|a| a.chat_id.as_str()),
            Some("chat-a")
        );
    }

    #[test]
    fn buddy_test_coverage_watcher_skips_test_bench_and_example_files() {
        let (dir, _repo) = init_temp_git_repo();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/tests")).unwrap();
        std::fs::create_dir_all(dir.path().join("benches")).unwrap();
        std::fs::create_dir_all(dir.path().join("examples")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("tests/integration.rs"),
            "#[test] fn it_works() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/tests/helper.rs"),
            "pub fn helper() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("benches/bench.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("examples/demo.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/feature.rs"), "pub fn feature() {}\n").unwrap();

        let paths = modified_rust_files(dir.path());

        assert_eq!(paths, vec!["src/feature.rs".to_string()]);
    }

    #[test]
    fn source_has_test_coverage_recognizes_supported_rust_test_layouts() {
        assert!(source_has_test_coverage(
            "src/feature.rs",
            "pub fn feature() {}\n#[cfg(test)] mod tests {}\n",
            &[]
        ));
        assert!(source_has_test_coverage(
            "src/feature.rs",
            "pub fn feature() {}\n",
            &["tests/feature.rs".to_string()]
        ));
        assert!(source_has_test_coverage(
            "crates/demo/src/feature.rs",
            "pub fn feature() {}\n",
            &["crates/demo/tests/feature.rs".to_string()]
        ));
        assert!(source_has_test_coverage(
            "src/feature.rs",
            "pub fn feature() {}\n",
            &["src/tests.rs".to_string()]
        ));
        assert!(source_has_test_coverage(
            "src/feature.rs",
            "pub fn feature() {}\n",
            &["src/tests/helper.rs".to_string()]
        ));
        assert!(!source_has_test_coverage(
            "src/feature.rs",
            "pub fn feature() {}\n",
            &["tests/other.rs".to_string()]
        ));
    }

    #[test]
    fn buddy_test_coverage_watcher_skips_deleted_rust_files() {
        let (dir, repo) = init_temp_git_repo();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("old.rs"), "pub fn old() {}\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("src/old.rs")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = git2::Signature::now("test", "test@test.com").unwrap();
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "add old", &tree, &[&parent])
                .unwrap();
        }
        std::fs::remove_file(src.join("old.rs")).unwrap();

        let scan = scan_test_coverage(dir.path());

        assert!(scan.candidates.is_empty());
    }

    #[test]
    fn buddy_test_coverage_watcher_parses_renamed_and_copied_modified_paths() {
        assert_eq!(
            parse_git_status_line("RM src/old.rs -> src/new.rs").as_deref(),
            Some("src/new.rs")
        );
        assert_eq!(
            parse_git_status_line("CM src/original.rs -> src/copied.rs").as_deref(),
            Some("src/copied.rs")
        );
    }

    #[test]
    fn buddy_test_coverage_watcher_caps_after_filtering_covered_files() {
        let (dir, _repo) = init_temp_git_repo();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for idx in 0..MAX_CANDIDATES {
            std::fs::write(
                src.join(format!("covered_{idx}.rs")),
                "#[cfg(test)] mod tests { #[test] fn covered() {} }\n",
            )
            .unwrap();
        }
        std::fs::write(src.join("uncovered.rs"), "pub fn uncovered() {}\n").unwrap();

        let scan = scan_test_coverage(dir.path());

        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].path, "src/uncovered.rs");
    }

    #[test]
    fn buddy_test_coverage_watcher_caps_to_5_candidates() {
        let (dir, _repo) = init_temp_git_repo();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for idx in 0..7 {
            std::fs::write(src.join(format!("file_{idx}.rs")), "pub fn item() {}\n").unwrap();
        }

        let scan = scan_test_coverage(dir.path());

        assert_eq!(scan.candidates.len(), MAX_CANDIDATES);
        assert_eq!(scan.candidates[0].path, "src/file_0.rs");
        assert_eq!(scan.candidates[4].path, "src/file_4.rs");
    }
}
