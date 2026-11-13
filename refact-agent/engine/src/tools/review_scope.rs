pub use refact_tools::tools::review_scope::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{HashMap, HashSet};
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::process::Command;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_types::{ReviewDiffSummary, ReviewScopeSummary, ScopeMode};
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,5 @@ fn thing() {\n context\n+added\n+added\n@@ -80,0 +90,1 @@\n+one more\ndiff --git a/src/other.rs b/src/other.rs\n--- /dev/null\n+++ b/src/other.rs\n@@ -0,0 +1,4 @@\n+new file\n";

    fn run_git(root: &Path, args: &[&str]) -> String {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_repo(root: &Path) {
        run_git(root, &["init"]);
        run_git(root, &["checkout", "-b", "main"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test User"]);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        run_git(root, &["add", "base.txt"]);
        run_git(root, &["commit", "-m", "base"]);
    }

    #[test]
    fn review_scope_hunk_membership_uses_new_side_ranges() {
        let hunks = DiffHunks::parse(PATCH);

        assert_eq!(hunks.hunk_count(), 3);
        assert!(hunks.contains("src/lib.rs", 10, 10));
        assert!(hunks.contains("src/lib.rs", 14, 20));
        assert!(!hunks.contains("src/lib.rs", 15, 20));
        assert!(hunks.contains("src/lib.rs", 90, 90));
        assert!(hunks.contains("/abs/repo/src/other.rs", 1, 4));
        assert!(!hunks.contains("src/untouched.rs", 1, 400));
        assert!(hunks.touches_file("src/lib.rs"));
        assert!(!hunks.touches_file("src/untouched.rs"));
    }

    #[tokio::test]
    async fn review_scope_attribution_falls_back_to_blame_for_moved_lines() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        let base = run_git(temp.path(), &["rev-parse", "HEAD"]);
        std::fs::write(temp.path().join("moved.rs"), "alpha\nbeta\ngamma\n").unwrap();
        run_git(temp.path(), &["add", "moved.rs"]);
        run_git(temp.path(), &["commit", "-m", "add moved"]);

        let scope = ReviewScope {
            mode: ScopeMode::Strict,
            requested: vec![],
            files: vec![],
            dropped_files: vec![],
            changed_files: vec![],
            focus: None,
            plan: None,
            base: Some(base),
            head: None,
            diff_patch: None,
            patch_total_bytes: 0,
            hunks: DiffHunks::parse("--- a/moved.rs\n+++ b/moved.rs\n@@ -0,0 +1,1 @@\n+alpha\n"),
            repo_root: Some(temp.path().to_path_buf()),
            expansion: None,
        };
        let attribution = DiffAttribution::new(&scope);

        assert!(attribution.introduced("moved.rs", 1, 1).await);
        assert!(attribution.introduced("moved.rs", 3, 3).await);
        assert!(!attribution.introduced("base.txt", 1, 1).await);
    }

    #[tokio::test]
    async fn review_scope_strict_keeps_requested_files_and_flags_outsiders() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let requested = vec![temp.path().join("base.txt")];

        let scope = build_review_scope(
            gcx,
            ScopeRequest {
                requested: requested.clone(),
                mode: ScopeMode::Strict,
                base: None,
                focus: Some("  safety  ".to_string()),
                plan: None,
                max_files: 10,
            },
        )
        .await;

        assert_eq!(scope.requested, requested);
        assert!(scope.in_scope(&temp.path().join("base.txt").to_string_lossy()));
        assert!(!scope.in_scope("src/elsewhere.rs"));
        assert_eq!(scope.focus.as_deref(), Some("safety"));
        assert!(scope.base.is_some());
    }

    #[tokio::test]
    async fn review_scope_broad_accepts_any_file_and_records_changed_set() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        run_git(temp.path(), &["checkout", "-b", "feature"]);
        std::fs::write(temp.path().join("changed.rs"), "changed\n").unwrap();
        run_git(temp.path(), &["add", "changed.rs"]);
        run_git(temp.path(), &["commit", "-m", "change"]);
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];

        let scope = build_review_scope(
            gcx,
            ScopeRequest {
                requested: vec![],
                mode: ScopeMode::Broad,
                base: Some("main".to_string()),
                focus: None,
                plan: None,
                max_files: 10,
            },
        )
        .await;

        assert_eq!(
            scope.changed_files,
            vec![refact_files::path_utils::canonicalize_normalized_path(
                temp.path().join("changed.rs")
            )]
        );
        assert!(scope.in_scope("anything/at/all.rs"));
        assert!(scope
            .diff_patch
            .as_deref()
            .is_some_and(|patch| patch.contains("changed.rs")));
        assert!(!scope.hunks.is_empty());
    }

    #[tokio::test]
    async fn review_scope_records_every_file_it_dropped_to_the_max_files_cap() {
        let temp = tempfile::tempdir().unwrap();
        init_repo(temp.path());
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let requested: Vec<PathBuf> = (0..5)
            .map(|index| temp.path().join(format!("file{index}.rs")))
            .collect();

        let scope = build_review_scope(
            gcx,
            ScopeRequest {
                requested: requested.clone(),
                mode: ScopeMode::Strict,
                base: None,
                focus: None,
                plan: None,
                max_files: 3,
            },
        )
        .await;

        assert_eq!(scope.files.len(), 3);
        assert_eq!(scope.dropped_files, requested[3..].to_vec());
        assert_eq!(scope.summary().dropped_files.len(), 2);
        assert!(scope
            .expansion
            .as_deref()
            .is_some_and(|note| note.contains("2 not reviewed")));
    }

    #[test]
    fn review_scope_generated_paths_are_excluded_from_review() {
        assert!(is_generated_path("Cargo.lock"));
        assert!(is_generated_path("gui/package-lock.json"));
        assert!(is_generated_path("src/__snapshots__/App.test.tsx.snap"));
        assert!(is_generated_path("web/node_modules/left-pad/index.js"));
        assert!(!is_generated_path("src/lib.rs"));
        assert!(!is_generated_path("gui/package.json"));
    }

    #[tokio::test]
    async fn review_scope_without_git_still_reviews_requested_files() {
        let temp = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];

        let scope = build_review_scope(
            gcx,
            ScopeRequest {
                requested: vec![temp.path().join("solo.rs")],
                mode: ScopeMode::Strict,
                base: None,
                focus: None,
                plan: None,
                max_files: 10,
            },
        )
        .await;

        assert_eq!(scope.base, None);
        assert!(scope.changed_files.is_empty());
        assert_eq!(scope.files.len(), 1);
        assert!(scope.hunks.is_empty());
    }
}
