pub use refact_runtime_api::buddy_jobs::chat_phrase_bank::*;

#[cfg(test)]
use chrono::{Local, Timelike, Utc};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext};
#[cfg(test)]
use crate::buddy::settings::BuddySettings;
#[cfg(test)]
use crate::buddy::types::BuddyFactKind;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use chrono::TimeZone;
    use crate::buddy::types::{
        BuddyActivity, BuddyFact, BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse,
        BuddyWorkflowSummary,
    };

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
    async fn phrase_bank_runs_once_per_day_at_digest_hour() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let hour = Local::now().hour() as u8;
        let mut ctx = test_context(dir.path());
        ctx.settings.daily_digest_hour = Some(hour);
        let job = ChatPhraseBankJob;

        assert!(job.should_run(gcx.clone(), &ctx).await);

        ctx.settings.daily_digest_hour = Some(hour.saturating_sub(1));
        assert!(job.should_run(gcx.clone(), &ctx).await);

        ctx.job_state.last_result = Some(date_key(&Local::now()));
        assert!(!job.should_run(gcx, &ctx).await);
    }

    #[test]
    fn phrase_bank_dedupes_on_local_date_key() {
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
    async fn phrase_bank_prompt_uses_recent_signals() {
        let response = Some(
            [
                "humor: Tiny task goblin is juggling stuck work with oven mitts.",
                "humor: MCP crumbs formed a conga line near the logs.",
                "humor: Git gremlins stacked diffs into a tiny wobbly tower.",
                "insight: Pin the stuck task first; the breadcrumb parade needs a marshal.",
                "insight: Memory cleanup has snack-sized knots worth untangling.",
                "insight: Worktree clutter wants one tidy sweep before more sparkle.",
                "bug: Error crumbs are circling; bring the detective spoon.",
                "bug: Frontend hiccups look bug-shaped, with suspicious tap shoes.",
                "bug: The failing integration left muddy pawprints in the log hallway.",
                "ambient: Pixel is orbiting quietly with a tiny clipboard and snacks.",
                "ambient: Companion confetti on standby while the project hums.",
                "ambient: I am watching the trail without nibbling the evidence.",
            ]
            .join("\n"),
        );
        let (service, renderer) =
            crate::buddy::voice_service::test_voice_service_with_responses(vec![response]);
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let mut ctx = test_context(dir.path());
        ctx.pulse.tasks.total = 7;
        ctx.pulse.tasks.recent_stuck_alerts_1h = 2;
        ctx.pulse.mcp.failing = 1;
        ctx.recent_activities.push(BuddyActivity {
            icon: "•".to_string(),
            title: "Reviewed chat bubble behavior".to_string(),
            description: "Kept repeated errors on Buddy Home".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            activity_type: "review".to_string(),
            chat_id: None,
            failure_category: None,
            failure_summary: None,
        });

        let bank = build_chat_phrase_bank(gcx, &ctx, Utc::now())
            .await
            .expect("bank generated");

        assert_eq!(bank.lines.len(), 12);
        assert!(bank.lines.iter().any(|line| line.kind == "humor"));
        let prompts = renderer.prompts();
        let (system, user) = prompts.first().expect("prompt captured");
        assert!(system.contains("reusable daily Buddy chat phrase bank"));
        assert!(system.contains("Do not use generic canned phrases"));
        assert!(user.contains("tasks=7 recent_stuck_alerts_1h=2"));
        assert!(user.contains("Reviewed chat bubble behavior"));
        assert!(user.contains("Build 12 reusable one-liners"));
    }

    #[tokio::test]
    async fn phrase_bank_evidence_is_redacted_before_prompt_and_storage() {
        let response = Some(
            [
                "humor: Tiny task goblin is juggling stuck work with oven mitts.",
                "humor: MCP crumbs formed a conga line near the logs.",
                "humor: Git gremlins stacked diffs into a tiny wobbly tower.",
                "insight: Pin the stuck task first; the breadcrumb parade needs a marshal.",
                "insight: Memory cleanup has snack-sized knots worth untangling.",
                "insight: Worktree clutter wants one tidy sweep before more sparkle.",
                "bug: Error crumbs are circling; bring the detective spoon.",
                "bug: Frontend hiccups look bug-shaped, with suspicious tap shoes.",
                "bug: The failing integration left muddy pawprints in the log hallway.",
                "ambient: Pixel is orbiting quietly with a tiny clipboard and snacks.",
                "ambient: Companion confetti on standby while the project hums.",
                "ambient: I am watching the trail without nibbling the evidence.",
            ]
            .join("\n"),
        );
        let (service, renderer) =
            crate::buddy::voice_service::test_voice_service_with_responses(vec![response]);
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let mut ctx = test_context(dir.path());
        ctx.pulse
            .diagnostics
            .top_error_types
            .push("Bearer sk-pulsesecret123456".to_string());
        ctx.recent_activities.push(BuddyActivity {
            icon: "•".to_string(),
            title: "Auth trail sk-activitysecret123456".to_string(),
            description: "token=activity-secret and /home/svakhreev/private/file.rs".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            activity_type: "review api_key=activity-kind-value".to_string(),
            chat_id: None,
            failure_category: None,
            failure_summary: None,
        });
        ctx.facts.push(BuddyFact {
            kind: BuddyFactKind::McpAuthExpired,
            key: "auth".to_string(),
            source: "test",
            payload: serde_json::json!({
                "token": "fact-secret-value",
                "nested": {
                    "Authorization": "Bearer nestedsecret",
                    "message": "password=fact-password and sk-factsecret123456",
                },
            }),
            seen_at: Utc::now(),
            confidence: 0.9,
        });
        ctx.workflow_summaries.push(BuddyWorkflowSummary {
            workflow_id: "workflow sk-workflowsecret123456".to_string(),
            last_run: None,
            run_count: 1,
            last_outcome: Some("Bearer workflow-secret and token=workflow-secret".to_string()),
            failure_category: None,
            failure_summary: None,
            ..Default::default()
        });

        let bank = build_chat_phrase_bank(gcx, &ctx, Utc::now())
            .await
            .expect("bank generated");
        let prompts = renderer.prompts();
        let (_, user) = prompts.first().expect("prompt captured");

        for text in [&bank.evidence_summary, user] {
            assert!(
                text.contains("[REDACTED"),
                "redaction marker missing: {text}"
            );
            for secret in [
                "sk-pulsesecret123456",
                "sk-activitysecret123456",
                "activity-secret",
                "/home/svakhreev/private/file.rs",
                "activity-kind-value",
                "fact-secret-value",
                "nestedsecret",
                "fact-password",
                "sk-factsecret123456",
                "sk-workflowsecret123456",
                "workflow-secret",
            ] {
                assert!(
                    !text.contains(secret),
                    "secret leaked into phrase-bank evidence: {secret}"
                );
            }
        }
    }

    #[tokio::test]
    async fn phrase_bank_failed_generation_records_daily_backoff() {
        let (service, _renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
            vec![Some("humor: only one usable kind".to_string())],
        );
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let mut ctx = test_context(dir.path());
        let job = ChatPhraseBankJob;

        let result = job.execute(gcx, ctx.clone()).await;

        assert!(result.chat_phrase_bank.is_none());
        let last_result = result.last_result.expect("failed run records backoff");
        assert!(last_result.starts_with(FAILED_RESULT_PREFIX));
        ctx.job_state.last_result = Some(last_result);
        assert!(!should_run_at(&ctx, Local::now()));
    }

    #[tokio::test]
    async fn phrase_bank_rejects_underfilled_generated_bank() {
        let response = Some(
            [
                "humor: Tiny task goblin is juggling stuck work with oven mitts.",
                "insight: Pin the stuck task first; the breadcrumb parade needs a marshal.",
                "bug: Error crumbs are circling; bring the detective spoon.",
                "ambient: Pixel is orbiting quietly with a tiny clipboard and snacks.",
            ]
            .join("\n"),
        );
        let (service, _renderer) =
            crate::buddy::voice_service::test_voice_service_with_responses(vec![response]);
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let ctx = test_context(dir.path());

        let bank = build_chat_phrase_bank(gcx, &ctx, Utc::now()).await;

        assert!(bank.is_none(), "daily bank must have 3 lines per kind");
    }

    #[tokio::test]
    async fn phrase_bank_caps_overfilled_generated_bank_to_expected_lines() {
        let response = Some(
            [
                "humor: Tiny task goblin is juggling stuck work with oven mitts.",
                "humor: MCP crumbs formed a conga line near the logs.",
                "humor: Git gremlins stacked diffs into a tiny wobbly tower.",
                "humor: Extra humor line should not be stored.",
                "insight: Pin the stuck task first; the breadcrumb parade needs a marshal.",
                "insight: Memory cleanup has snack-sized knots worth untangling.",
                "insight: Worktree clutter wants one tidy sweep before more sparkle.",
                "insight: Extra insight line should not be stored.",
                "bug: Error crumbs are circling; bring the detective spoon.",
                "bug: Frontend hiccups look bug-shaped, with suspicious tap shoes.",
                "bug: The failing integration left muddy pawprints in the log hallway.",
                "bug: Extra bug line should not be stored.",
                "ambient: Pixel is orbiting quietly with a tiny clipboard and snacks.",
                "ambient: Companion confetti on standby while the project hums.",
                "ambient: I am watching the trail without nibbling the evidence.",
                "ambient: Extra ambient line should not be stored.",
            ]
            .join("\n"),
        );
        let (service, _renderer) =
            crate::buddy::voice_service::test_voice_service_with_responses(vec![response]);
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let ctx = test_context(dir.path());

        let bank = build_chat_phrase_bank(gcx, &ctx, Utc::now())
            .await
            .expect("overfilled bank still has enough usable lines");

        assert_eq!(bank.lines.len(), PHRASE_KINDS.len() * PHRASES_PER_KIND);
        for kind in PHRASE_KINDS {
            assert_eq!(
                bank.lines.iter().filter(|line| line.kind == kind).count(),
                3
            );
        }
        assert!(!bank.lines.iter().any(|line| line.text.contains("Extra")));
    }
}
