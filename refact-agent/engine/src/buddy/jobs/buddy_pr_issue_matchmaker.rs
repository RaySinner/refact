#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
#[cfg(test)]
use chrono::{DateTime, Utc};
#[cfg(test)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::buddy::autonomous_workflows::{
    autonomous_workflow_meta, BUDDY_PR_ISSUE_MATCHMAKER_WORKFLOW_ID,
};
#[cfg(test)]
use crate::buddy::jobs::autonomous_chats::{execute_autonomous_spec, AutonomousBuddyChatSpec};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};

pub use refact_runtime_api::buddy_jobs::buddy_pr_issue_matchmaker::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::settings::BuddySettings;
    use crate::buddy::types::{BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DIFF_CHECKS: AtomicUsize = AtomicUsize::new(0);

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
        std::fs::write(dir.path().join("changed.txt"), "original\n").unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("changed.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        drop(index);
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        drop(tree);
        (dir, repo)
    }

    async fn counted_git_diff_shortstat_changed_lines(project_root: &Path) -> u32 {
        DIFF_CHECKS.fetch_add(1, Ordering::SeqCst);
        git_diff_shortstat_changed_lines(project_root).await
    }

    async fn diff_changed_lines_counted(ctx: &BuddyJobContext) -> u32 {
        if let Some(cache) = fresh_cache(ctx) {
            return cache.changed_lines;
        }
        counted_git_diff_shortstat_changed_lines(&ctx.project_root).await
    }

    #[tokio::test]
    async fn buddy_pr_issue_matchmaker_caches_diff_check_within_4h() {
        let (dir, _repo) = init_temp_git_repo();
        let root = dir.path();
        std::fs::write(
            root.join("changed.txt"),
            (0..12)
                .map(|idx| format!("line {idx}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let fresh_cache = serialize_cache(&PrIssueMatchmakerCache {
            checked_at: Utc::now(),
            changed_lines: 12,
        });
        let cached_ctx = test_context(root, Some(fresh_cache));
        DIFF_CHECKS.store(0, Ordering::SeqCst);

        assert_eq!(diff_changed_lines_counted(&cached_ctx).await, 12);
        assert_eq!(DIFF_CHECKS.load(Ordering::SeqCst), 0);

        let stale_cache = serialize_cache(&PrIssueMatchmakerCache {
            checked_at: Utc::now() - chrono::Duration::hours(5),
            changed_lines: 0,
        });
        let stale_ctx = test_context(root, Some(stale_cache));
        assert!(diff_changed_lines_counted(&stale_ctx).await >= 10);
        assert_eq!(DIFF_CHECKS.load(Ordering::SeqCst), 1);
        let job = BuddyPrIssueMatchmakerJob;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let small_cache = serialize_cache(&PrIssueMatchmakerCache {
            checked_at: Utc::now(),
            changed_lines: 0,
        });
        assert!(
            !job.should_run(gcx.clone(), &test_context(root, Some(small_cache)))
                .await
        );
        assert!(job.should_run(gcx, &test_context(root, None)).await);
        let pending_cache = checked_diff_cache(root.to_path_buf()).await;
        store_pending_diff_cache(root, pending_cache.clone());
        assert_eq!(take_pending_diff_cache(root), Some(pending_cache));
        assert_eq!(take_pending_diff_cache(root), None);
        assert_eq!(
            parse_shortstat_changed_lines(" 1 file changed, 7 insertions(+), 3 deletions(-)"),
            10
        );
    }
}
