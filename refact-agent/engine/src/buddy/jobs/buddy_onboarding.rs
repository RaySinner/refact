#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::time::SystemTime;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
use crate::buddy::autonomous_workflows::{autonomous_workflow_meta, BUDDY_ONBOARDING_WORKFLOW_ID};
#[cfg(test)]
use crate::buddy::jobs::autonomous_chats::{
    execute_autonomous_spec, same_signal, AutonomousBuddyChatSpec,
};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
#[cfg(test)]
use crate::app_state::AppState;

pub use refact_runtime_api::buddy_jobs::buddy_onboarding::*;

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

    #[tokio::test]
    async fn buddy_onboarding_detects_new_top_level_files_after_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("AGENTS.md");
        std::fs::write(&agents, "# Agents\n").unwrap();
        filetime::set_file_mtime(&agents, filetime::FileTime::from_unix_time(100, 0)).unwrap();
        let newer = dir.path().join("new-service");
        std::fs::create_dir_all(&newer).unwrap();
        filetime::set_file_mtime(&newer, filetime::FileTime::from_unix_time(200, 0)).unwrap();
        let ignored = dir.path().join("target");
        std::fs::create_dir_all(&ignored).unwrap();
        filetime::set_file_mtime(&ignored, filetime::FileTime::from_unix_time(300, 0)).unwrap();
        let scan = scan_onboarding(dir.path());
        let spec = build_onboarding_spec(&test_context(dir.path(), None), &scan);
        let ctx = test_context(dir.path(), Some(serialize_scan(&spec.signal_hash, &scan)));
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        assert!(BuddyOnboardingJob.should_run(gcx, &ctx).await);
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].path, "new-service");
        assert_eq!(scan.candidates[0].kind, "dir");
    }
}
