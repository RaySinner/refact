#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(test)]
use std::io::Read;
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
use crate::buddy::autonomous_workflows::{autonomous_workflow_meta, BUDDY_SKILL_AUTHOR_WORKFLOW_ID};
#[cfg(test)]
use crate::buddy::jobs::autonomous_chats::{
    execute_autonomous_spec, same_signal, AutonomousBuddyChatSpec,
};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
#[cfg(test)]
use crate::ext::competitor_import::markdown::sanitize_skill_id;
#[cfg(test)]
use crate::app_state::AppState;

pub use refact_runtime_api::buddy_jobs::buddy_skill_author::*;

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

    fn write_trajectory(path: &Path, text: &str) {
        std::fs::write(
            path,
            serde_json::json!({
                "messages": [{"role": "user", "content": text}]
            })
            .to_string(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn buddy_skill_author_finds_5_plus_similar_trajectories() {
        let dir = tempfile::tempdir().unwrap();
        let trajectories = dir.path().join(".refact/trajectories");
        std::fs::create_dir_all(&trajectories).unwrap();
        for idx in 0..5 {
            write_trajectory(
                &trajectories.join(format!("chat_{idx}.json")),
                "Please review the auth flow for missing tests and edge cases in the service layer.",
            );
        }
        write_trajectory(
            &trajectories.join("other.json"),
            "A different one-off request",
        );
        let scan = scan_skill_author(dir.path());
        let spec = build_skill_author_spec(&test_context(dir.path(), None), &scan).unwrap();
        let ctx = test_context(dir.path(), Some(serialize_scan(&spec.signal_hash, &scan)));
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        assert!(BuddySkillAuthorJob.should_run(gcx, &ctx).await);
        let candidate = scan.candidate.unwrap();
        assert_eq!(candidate.count, 5);
        assert_eq!(
            candidate.skill_id,
            "please-review-the-auth-flow-for-missing-tests-and-edge-cases-in-the-service-layer"
        );
    }
}
