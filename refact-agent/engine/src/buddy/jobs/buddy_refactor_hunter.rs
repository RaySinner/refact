#[cfg(test)]
use chrono::{DateTime, Datelike, Local};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use crate::buddy::autonomous_workflows::{autonomous_workflow_meta, BUDDY_REFACTOR_HUNTER_WORKFLOW_ID};
#[cfg(test)]
use crate::buddy::jobs::autonomous_chats::{execute_autonomous_spec, AutonomousBuddyChatSpec};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
#[cfg(test)]
use crate::app_state::AppState;

pub use refact_runtime_api::buddy_jobs::buddy_refactor_hunter::*;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use crate::buddy::settings::BuddySettings;
    use crate::buddy::types::{BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse};
    use std::path::Path;

    fn test_context(project_root: &Path) -> BuddyJobContext {
        BuddyJobContext {
            identity_name: "Pixel".to_string(),
            personality: Default::default(),
            onboarding: BuddyOnboarding::default(),
            recent_diagnostics: vec![],
            project_root: project_root.to_path_buf(),
            job_state: BuddyJobState::default(),
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

    #[tokio::test]
    async fn buddy_refactor_hunter_uses_calendar_week_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let ctx = test_context(dir.path());
        let job = BuddyRefactorHunterJob;

        assert_eq!(job.cooldown_seconds(), COOLDOWN_SECONDS);
        assert!(job.should_run(gcx, &ctx).await);
        assert_eq!(
            build_refactor_hunter_spec(&ctx).workflow_id,
            BUDDY_REFACTOR_HUNTER_WORKFLOW_ID
        );
    }

    #[test]
    fn buddy_refactor_hunter_dedupes_on_local_week_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_context(dir.path());
        let now = Local
            .with_ymd_and_hms(2026, 6, 5, 12, 0, 0)
            .single()
            .unwrap();
        let key = week_key_at(&now);
        ctx.job_state.last_result = Some(key.clone());

        assert_eq!(key, week_key_at(&now));
        assert!(week_key_recorded(&ctx, &key));
    }
}
