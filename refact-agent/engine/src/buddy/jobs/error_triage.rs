pub use refact_runtime_api::buddy_jobs::error_triage::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::app_state_from_gcx;
    use crate::buddy::diagnostics::{DiagnosticContext, DiagnosticSeverity};
    use crate::buddy::scheduler::{BuddyJob, BuddyJobContext};
    use crate::buddy::types::BuddyJobState;

    fn diagnostics(error_type: &str, count: usize) -> Vec<DiagnosticContext> {
        (0..count)
            .map(|idx| DiagnosticContext {
                error_type: error_type.to_string(),
                error_message: format!("boom {idx}"),
                source_file: None,
                tool_name: Some("chat".to_string()),
                chat_id: None,
                model_id: None,
                collected_at: chrono::Utc::now().to_rfc3339(),
                severity: DiagnosticSeverity::High,
                occurrences: None,
            })
            .collect()
    }

    fn context_with(
        recent_diagnostics: Vec<DiagnosticContext>,
        last_result: Option<String>,
    ) -> BuddyJobContext {
        BuddyJobContext {
            identity_name: "Pixel".to_string(),
            personality: Default::default(),
            onboarding: Default::default(),
            recent_diagnostics,
            project_root: std::path::PathBuf::new(),
            job_state: BuddyJobState {
                last_result,
                ..Default::default()
            },
            workflow_summaries: vec![],
            total_workflow_runs: 0,
            suggestion_state: vec![],
            pet: Default::default(),
            active_quest: None,
            settings: Default::default(),
            pulse: Default::default(),
            facts: vec![],
            recent_activities: vec![],
        }
    }

    #[test]
    fn triage_signature_buckets_counts() {
        assert_eq!(triage_signature("llm_error", 3), "triage:llm_error:2");
        assert_eq!(triage_signature("llm_error", 5), "triage:llm_error:4");
        assert_eq!(triage_signature("llm_error", 7), "triage:llm_error:4");
        assert_eq!(triage_signature("llm_error", 9), "triage:llm_error:8");
    }

    #[tokio::test]
    async fn repeated_cluster_signature_is_suppressed() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = app_state_from_gcx(gcx).await;
        let job = ErrorTriageJob;

        let first = job
            .execute(app.clone(), context_with(diagnostics("llm_error", 5), None))
            .await;
        assert!(first.suggestion.is_some());
        let signature = first.last_result.clone().unwrap();

        let suppressed = job
            .execute(
                app.clone(),
                context_with(diagnostics("llm_error", 6), Some(signature.clone())),
            )
            .await;
        assert!(suppressed.suggestion.is_none());
        assert!(suppressed.last_result.is_none());

        let escalated = job
            .execute(
                app,
                context_with(diagnostics("llm_error", 16), Some(signature)),
            )
            .await;
        assert!(escalated.suggestion.is_some());
        assert_eq!(
            escalated.last_result.as_deref(),
            Some("triage:llm_error:16")
        );
    }
}
