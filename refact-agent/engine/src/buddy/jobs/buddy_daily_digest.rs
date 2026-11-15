#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use chrono::{DateTime, Local, Timelike, Utc};
#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::buddy::autonomous_workflows::{autonomous_workflow_meta, BUDDY_DAILY_DIGEST_WORKFLOW_ID};
#[cfg(test)]
use crate::buddy::jobs::autonomous_chats::{execute_autonomous_spec, AutonomousBuddyChatSpec};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
#[cfg(test)]
use crate::buddy::settings::BuddySettings;

pub use refact_runtime_api::buddy_jobs::buddy_daily_digest::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use chrono::TimeZone;
    use crate::buddy::autonomous_workflows::{
        BUDDY_FRIDAY_RETRO_WORKFLOW_ID, BUDDY_IDLE_SUGGESTER_WORKFLOW_ID,
        BUDDY_PR_ISSUE_MATCHMAKER_WORKFLOW_ID,
    };
    use crate::buddy::conversation_ledger::workflow_id_to_mapping;
    use crate::buddy::types::{BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse};

    fn defaults_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates")
            .join("refact-yaml-configs")
            .join("src")
            .join("defaults")
    }

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
    async fn buddy_daily_digest_should_run_at_or_after_configured_hour() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let hour = Local::now().hour() as u8;
        let mut ctx = test_context(dir.path());
        let job = BuddyDailyDigestJob;

        ctx.settings.daily_digest_hour = Some(hour);
        assert!(job.should_run(gcx.clone(), &ctx).await);

        ctx.settings.daily_digest_hour = Some(hour.saturating_sub(1));
        assert!(job.should_run(gcx.clone(), &ctx).await);

        if hour < 23 {
            ctx.settings.daily_digest_hour = Some(hour + 1);
            assert!(!job.should_run(gcx, &ctx).await);
        }
        assert_eq!(job.cooldown_seconds(), COOLDOWN_SECONDS);
        assert_eq!(BuddySettings::default().daily_digest_hour, Some(18));
    }

    #[test]
    fn buddy_daily_digest_runs_later_same_day_after_digest_hour() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_context(dir.path());
        ctx.settings.daily_digest_hour = Some(18);
        let late_same_day = Local
            .with_ymd_and_hms(2026, 6, 5, 23, 0, 0)
            .single()
            .unwrap();

        assert!(should_run_at(&ctx, late_same_day));
    }

    #[test]
    fn buddy_daily_digest_dedupes_on_local_date_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = test_context(dir.path());
        ctx.settings.daily_digest_hour = Some(18);
        let now = Local
            .with_ymd_and_hms(2026, 6, 5, 23, 0, 0)
            .single()
            .unwrap();
        ctx.job_state.last_result = Some(date_key(&now));

        assert!(!should_run_at(&ctx, now));
    }

    #[tokio::test]
    async fn all_4_workflow_yamls_loadable() {
        let defaults_dir = defaults_dir();
        let registry =
            crate::yaml_configs::customization_registry::load_registry_from_dir(&defaults_dir)
                .await;
        let ids = [
            BUDDY_DAILY_DIGEST_WORKFLOW_ID,
            BUDDY_FRIDAY_RETRO_WORKFLOW_ID,
            BUDDY_IDLE_SUGGESTER_WORKFLOW_ID,
            BUDDY_PR_ISSUE_MATCHMAKER_WORKFLOW_ID,
        ];

        for id in ids {
            assert!(registry.subagents.contains_key(id), "missing {id}");
            let mapping = workflow_id_to_mapping(id);
            assert_eq!(mapping.kind, "system");
            assert!(mapping.badge.is_some());
        }

        let errors = registry
            .errors
            .iter()
            .filter(|error| ids.iter().any(|id| error.file_path.contains(id)))
            .map(|error| format!("{}: {}", error.file_path, error.error))
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "{errors:?}");
    }
}
