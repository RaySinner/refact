#[cfg(test)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use crate::buddy::autonomous_workflows::{autonomous_workflow_meta, REFACT_SELF_CRITIC_WORKFLOW_ID};
#[cfg(test)]
use crate::buddy::jobs::autonomous_chats::{execute_autonomous_spec, AutonomousBuddyChatSpec};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
#[cfg(test)]
use crate::app_state::AppState;

pub use refact_runtime_api::buddy_jobs::refact_self_critic::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::buddy::autonomous_workflows::{
        AUTONOMOUS_BUDDY_WORKFLOWS, REFACT_COMPILE_SNIFFER_WORKFLOW_ID,
    };
    use crate::buddy::conversation_ledger::workflow_id_to_mapping;
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
    async fn refact_self_critic_runs_on_24h_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("refact-agent/engine")).unwrap();
        std::fs::write(dir.path().join("refact-agent/engine/Cargo.toml"), "").unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let ctx = test_context(dir.path());
        let job = RefactSelfCriticJob;

        assert_eq!(job.cooldown_seconds(), 24 * 60 * 60);
        assert!(job.should_run(gcx, &ctx).await);

        let spec = build_self_critic_spec(&ctx);
        assert_eq!(spec.workflow_id, REFACT_SELF_CRITIC_WORKFLOW_ID);
        assert_eq!(spec.project_root, dir.path().to_string_lossy().to_string());
        assert!(spec.evidence.contains("prompt_fingerprint="));
        assert!(!spec.evidence.contains("date="));
    }

    #[test]
    fn refact_engine_workspace_requires_engine_cargo_manifest() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_refact_engine_workspace(dir.path()));

        std::fs::create_dir_all(dir.path().join("refact-agent/engine")).unwrap();
        std::fs::write(dir.path().join("refact-agent/engine/Cargo.toml"), "").unwrap();

        assert!(is_refact_engine_workspace(dir.path()));
    }

    #[test]
    fn both_workflows_in_autonomous_workflows_metadata() {
        let ids = AUTONOMOUS_BUDDY_WORKFLOWS
            .iter()
            .map(|meta| meta.id)
            .collect::<Vec<_>>();

        assert!(ids.contains(&REFACT_SELF_CRITIC_WORKFLOW_ID));
        assert!(ids.contains(&REFACT_COMPILE_SNIFFER_WORKFLOW_ID));

        let self_critic = workflow_id_to_mapping(REFACT_SELF_CRITIC_WORKFLOW_ID);
        assert_eq!(self_critic.kind, "system");
        assert_eq!(self_critic.badge, Some("Self-Critic"));

        let compile_sniffer = workflow_id_to_mapping(REFACT_COMPILE_SNIFFER_WORKFLOW_ID);
        assert_eq!(compile_sniffer.kind, "system");
        assert_eq!(compile_sniffer.badge, Some("Compile Sniffer"));
    }

    #[test]
    fn prompt_tree_fingerprint_is_content_based() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("a.yaml"), "alpha").unwrap();
        std::fs::write(dir.path().join("nested/b.yaml"), "beta").unwrap();

        let first = hash_prompt_tree_at(dir.path());
        let second = hash_prompt_tree_at(dir.path());
        assert_eq!(first, second);

        std::fs::write(dir.path().join("nested/b.yaml"), "beta changed").unwrap();
        let changed = hash_prompt_tree_at(dir.path());
        assert_ne!(first, changed);
    }
}
