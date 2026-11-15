#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use chrono::{Datelike, Local, Timelike, Utc, Weekday};
#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::buddy::autonomous_workflows::{autonomous_workflow_meta, BUDDY_FRIDAY_RETRO_WORKFLOW_ID};
#[cfg(test)]
use crate::buddy::jobs::autonomous_chats::{execute_autonomous_spec, AutonomousBuddyChatSpec};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};

pub use refact_runtime_api::buddy_jobs::buddy_friday_retro::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use chrono::TimeZone;
    use crate::buddy::settings::BuddySettings;
    use crate::buddy::types::{BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse};

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
    async fn buddy_friday_retro_should_run_on_friday_at_or_after_digest_hour() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let now = Local::now();
        let mut ctx = test_context(dir.path());
        ctx.settings.daily_digest_hour = Some(now.hour() as u8);
        let expected = now.weekday() == Weekday::Fri;

        assert_eq!(
            BuddyFridayRetroJob.should_run(gcx.clone(), &ctx).await,
            expected
        );

        ctx.settings.daily_digest_hour = Some(now.hour().saturating_sub(1) as u8);
        assert_eq!(
            BuddyFridayRetroJob.should_run(gcx.clone(), &ctx).await,
            expected
        );

        ctx.settings.daily_digest_hour = Some(now.hour().saturating_add(1).min(23) as u8);
        assert_eq!(
            BuddyFridayRetroJob.should_run(gcx, &ctx).await,
            expected && now.hour() == 23
        );
        assert_eq!(BuddyFridayRetroJob.cooldown_seconds(), COOLDOWN_SECONDS);
    }

    #[test]
    fn buddy_friday_retro_runs_later_on_friday_after_digest_hour() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_context(dir.path());
        ctx.settings.daily_digest_hour = Some(18);
        let friday_late = Local
            .with_ymd_and_hms(2026, 6, 5, 23, 0, 0)
            .single()
            .unwrap();

        assert!(should_run_at(&ctx, friday_late));
    }

    #[test]
    fn buddy_friday_retro_dedupes_on_local_week_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_context(dir.path());
        ctx.settings.daily_digest_hour = Some(18);
        let friday_late = Local
            .with_ymd_and_hms(2026, 6, 5, 23, 0, 0)
            .single()
            .unwrap();
        ctx.job_state.last_result = Some(week_key_at(&friday_late));

        assert!(!should_run_at(&ctx, friday_late));
    }
}
