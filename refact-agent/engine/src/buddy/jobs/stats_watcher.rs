pub use refact_runtime_api::buddy_jobs::stats_watcher::*;

#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::diagnostics::{DiagnosticContext, DiagnosticSeverity};
    use crate::buddy::settings::BuddySettings;
    use crate::buddy::scheduler::result_after_suggestion_policy;
    use crate::buddy::types::{
        BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse, BuddySuggestion,
    };

    fn test_context_with_state(
        runs: u64,
        diagnostics_count: usize,
        job_state: BuddyJobState,
    ) -> BuddyJobContext {
        let collected_at = chrono::Utc::now().to_rfc3339();
        let recent_diagnostics = (0..diagnostics_count)
            .map(|idx| DiagnosticContext {
                error_type: "timeout".to_string(),
                error_message: format!("timeout {idx}"),
                source_file: None,
                tool_name: None,
                chat_id: None,
                collected_at: collected_at.clone(),
                severity: DiagnosticSeverity::High,
                occurrences: None,
                model_id: None,
            })
            .collect();
        BuddyJobContext {
            identity_name: "Pixel".to_string(),
            personality: Default::default(),
            onboarding: BuddyOnboarding::default(),
            recent_diagnostics,
            project_root: std::path::PathBuf::from("/tmp/project"),
            job_state,
            workflow_summaries: vec![],
            total_workflow_runs: runs,
            suggestion_state: vec![],
            pet: BuddyPetState::default(),
            active_quest: None,
            settings: BuddySettings::default(),
            pulse: BuddyPulse::default(),
            facts: vec![],
            recent_activities: vec![],
        }
    }

    fn test_context(runs: u64, diagnostics_count: usize) -> BuddyJobContext {
        test_context_with_state(runs, diagnostics_count, BuddyJobState::default())
    }

    fn active_suggestion(idx: usize) -> BuddySuggestion {
        BuddySuggestion {
            id: format!("suggestion-{idx}"),
            suggestion_type: "test".to_string(),
            title: format!("Suggestion {idx}"),
            description: "Test".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            dismissed: false,
            controls: vec![],
            quest: None,
        }
    }

    #[test]
    fn stats_watcher_declares_suggestion_output() {
        let job = StatsWatcherJob;

        assert!(job.produces_suggestion());
    }

    #[tokio::test]
    async fn stats_watcher_error_burst_returns_suggestion() {
        let job = StatsWatcherJob;
        let ctx = test_context(1, 5);
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        let result = job.execute(gcx, ctx).await;

        assert!(result.suggestion.is_some());
        assert_eq!(result.last_result.as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn stats_watcher_should_run_only_for_visible_suggestion_policy() {
        let job = StatsWatcherJob;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let quiet_ctx = test_context(12, 0);
        let milestone_ctx = test_context_with_state(
            12,
            0,
            BuddyJobState {
                last_result: Some("9".to_string()),
                ..Default::default()
            },
        );
        let updated_ctx = test_context_with_state(
            12,
            0,
            BuddyJobState {
                last_result: Some("12".to_string()),
                ..Default::default()
            },
        );

        assert!(job.should_run(gcx.clone(), &quiet_ctx).await);
        assert!(job.should_run(gcx.clone(), &milestone_ctx).await);
        assert!(!job.should_run(gcx.clone(), &updated_ctx).await);
        assert!(!job.should_run(gcx.clone(), &test_context(0, 0)).await);

        let result = job.execute(gcx, updated_ctx).await;
        assert!(result.suggestion.is_none());
        assert!(result.speech.is_none());
        assert!(result.activity.is_none());
        assert_eq!(result.last_result.as_deref(), Some("12"));
    }

    #[tokio::test]
    async fn stats_watcher_suppresses_suggestion_when_proactive_is_disabled_but_keeps_milestones() {
        let job = StatsWatcherJob;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let mut error_ctx = test_context(1, 5);
        error_ctx.settings.proactive_enabled = false;
        let mut milestone_ctx = test_context_with_state(
            10,
            0,
            BuddyJobState {
                last_result: Some("9".to_string()),
                ..Default::default()
            },
        );
        milestone_ctx.settings.proactive_enabled = false;

        assert!(job.should_run(gcx.clone(), &error_ctx).await);
        assert!(job.should_run(gcx.clone(), &milestone_ctx).await);

        let error_result = job.execute(gcx.clone(), error_ctx.clone()).await;
        assert!(error_result.suggestion.is_some());
        let error_result = result_after_suggestion_policy(
            error_result,
            &error_ctx.settings,
            &error_ctx.suggestion_state,
        );
        assert!(error_result.suggestion.is_none());
        assert_eq!(error_result.last_result.as_deref(), Some("1"));

        let milestone_result = job.execute(gcx, milestone_ctx).await;
        assert!(milestone_result.suggestion.is_none());
        assert!(milestone_result.speech.is_some());
        assert!(milestone_result.activity.is_some());
        assert_eq!(milestone_result.last_result.as_deref(), Some("10"));
    }

    #[tokio::test]
    async fn stats_watcher_suppresses_suggestion_when_unread_cap_is_full() {
        let job = StatsWatcherJob;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let mut ctx = test_context(1, 5);
        ctx.suggestion_state = (0..crate::buddy::scheduler::MAX_UNREAD_SUGGESTIONS)
            .map(active_suggestion)
            .collect();

        assert!(job.should_run(gcx.clone(), &ctx).await);
        let result = job.execute(gcx, ctx.clone()).await;
        assert!(result.suggestion.is_some());
        let result = result_after_suggestion_policy(result, &ctx.settings, &ctx.suggestion_state);

        assert!(result.suggestion.is_none());
        assert_eq!(result.last_result.as_deref(), Some("1"));
    }
}
