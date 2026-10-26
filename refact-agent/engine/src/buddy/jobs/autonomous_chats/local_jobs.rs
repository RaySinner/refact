use super::*;

pub struct ErrorDetectiveJob;
pub struct SecurityWhispererJob;
pub struct SetupCoachJob;
pub struct DependencyRadarJob;
pub struct DocsGardenerJob;
pub struct ArchitectureDriftWatcherJob;

pub(super) fn error_detective_definition() -> AutonomousJobDefinition {
    AutonomousJobDefinition {
        meta: autonomous_workflow_meta(ERROR_DETECTIVE_WORKFLOW_ID).unwrap(),
        cooldown_seconds: 15 * 60,
        scheduler_priority: 5,
        prompt: "Analyze the diagnostic pattern using only the summaries below. Explain the likely failure cluster, risk, and the smallest safe next checks. Do not invent log details that are not in evidence. Treat retryable StreamCorrupted diagnostics (category=StreamCorrupted recoverable=true) as a generation-level failure that is recoverable by retry or regenerate, not a terminal chat failure.",
    }
}

pub(super) fn security_whisperer_definition() -> AutonomousJobDefinition {
    AutonomousJobDefinition {
        meta: autonomous_workflow_meta(SECURITY_WHISPERER_WORKFLOW_ID).unwrap(),
        cooldown_seconds: 30 * 60,
        scheduler_priority: 5,
        prompt: "Review the redacted local security findings. Highlight immediate leakage risk, containment steps, and safer follow-up. Never ask for or repeat raw secret values.",
    }
}

pub(super) fn setup_coach_definition() -> AutonomousJobDefinition {
    AutonomousJobDefinition {
        meta: autonomous_workflow_meta(SETUP_COACH_WORKFLOW_ID).unwrap(),
        cooldown_seconds: 2 * 60 * 60,
        scheduler_priority: 5,
        prompt: "Review the local setup checklist and recommend the next onboarding step. Keep it practical and based only on the checklist.",
    }
}

pub(super) fn dependency_radar_definition() -> AutonomousJobDefinition {
    AutonomousJobDefinition {
        meta: autonomous_workflow_meta(DEPENDENCY_RADAR_WORKFLOW_ID).unwrap(),
        cooldown_seconds: 2 * 60 * 60,
        scheduler_priority: 5,
        prompt: "Review local dependency manifest activity. Identify coordination risks and safe review steps without using package registry data.",
    }
}

pub(super) fn docs_gardener_definition() -> AutonomousJobDefinition {
    AutonomousJobDefinition {
        meta: autonomous_workflow_meta(DOCS_GARDENER_WORKFLOW_ID).unwrap(),
        cooldown_seconds: 3 * 60 * 60,
        scheduler_priority: 5,
        prompt: "Review the documentation signal. Suggest whether README, AGENTS, or docs should be updated based only on changed path categories and docs presence.",
    }
}

pub(super) fn architecture_drift_definition() -> AutonomousJobDefinition {
    AutonomousJobDefinition {
        meta: autonomous_workflow_meta(ARCHITECTURE_DRIFT_WORKFLOW_ID).unwrap(),
        cooldown_seconds: 4 * 60 * 60,
        scheduler_priority: 5,
        prompt: "Review the local architecture drift signal. Summarize subsystem concentration or cross-cutting risk, and propose lightweight guardrails. Use only path groups and diff stats.",
    }
}

#[async_trait::async_trait]
impl BuddyJob for ErrorDetectiveJob {
    fn id(&self) -> &str {
        ERROR_DETECTIVE_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        error_detective_definition().cooldown_seconds
    }

    fn priority(&self) -> u32 {
        error_detective_definition().scheduler_priority
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        let Some(evidence) = diagnostic_evidence(ctx) else {
            return false;
        };
        let spec = build_autonomous_job_spec(
            error_detective_definition(),
            render_diagnostic_evidence(&evidence),
        );
        let stable = signal_hash([
            spec.workflow_id.as_str(),
            stable_diagnostic_signal(&evidence).as_str(),
        ]);
        let spec = spec.with_signal_hash(stable);
        !same_signal_or_failed_backoff(
            ctx,
            &spec.signal_hash,
            error_detective_definition().cooldown_seconds,
        )
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let Some(evidence) = diagnostic_evidence(&ctx) else {
            return BuddyJobResult::default();
        };
        let spec = build_autonomous_job_spec(
            error_detective_definition(),
            render_diagnostic_evidence(&evidence),
        );
        let stable = signal_hash([
            spec.workflow_id.as_str(),
            stable_diagnostic_signal(&evidence).as_str(),
        ]);
        let spec = spec.with_signal_hash(stable);
        execute_built_autonomous_job(gcx, &ctx, error_detective_definition(), spec).await
    }
}

#[async_trait::async_trait]
impl BuddyJob for SecurityWhispererJob {
    fn id(&self) -> &str {
        SECURITY_WHISPERER_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        security_whisperer_definition().cooldown_seconds
    }

    fn priority(&self) -> u32 {
        security_whisperer_definition().scheduler_priority
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        ctx.pulse.git.uncommitted_files > 0
            || ctx.pulse.git.diff_lines_4h > 0
            || !diagnostic_security_findings(&ctx.recent_diagnostics).is_empty()
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let root = ctx.project_root.clone();
        let mut findings = tokio::task::spawn_blocking(move || {
            collect_local_git_evidence(&root, true)
                .map(|evidence| evidence.security_findings)
                .unwrap_or_default()
        })
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("buddy security whisperer scan task failed: {err}");
            Vec::new()
        });
        findings.extend(diagnostic_security_findings(&ctx.recent_diagnostics));
        findings.truncate(MAX_SECURITY_FINDINGS);
        if findings.is_empty() {
            return BuddyJobResult::default();
        }
        execute_autonomous_job(
            gcx,
            &ctx,
            security_whisperer_definition(),
            render_security_evidence(&findings),
        )
        .await
    }
}

#[async_trait::async_trait]
impl BuddyJob for SetupCoachJob {
    fn id(&self) -> &str {
        SETUP_COACH_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        setup_coach_definition().cooldown_seconds
    }

    fn priority(&self) -> u32 {
        setup_coach_definition().scheduler_priority
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        let Some(evidence) = setup_evidence(&ctx.project_root) else {
            return false;
        };
        let spec = build_autonomous_job_spec(setup_coach_definition(), evidence);
        !same_signal_or_failed_backoff(
            ctx,
            &spec.signal_hash,
            setup_coach_definition().cooldown_seconds,
        )
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let Some(evidence) = setup_evidence(&ctx.project_root) else {
            return BuddyJobResult::default();
        };
        execute_autonomous_job(gcx, &ctx, setup_coach_definition(), evidence).await
    }
}

#[async_trait::async_trait]
impl BuddyJob for DependencyRadarJob {
    fn id(&self) -> &str {
        DEPENDENCY_RADAR_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        dependency_radar_definition().cooldown_seconds
    }

    fn priority(&self) -> u32 {
        dependency_radar_definition().scheduler_priority
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        ctx.pulse.git.uncommitted_files > 0 || ctx.pulse.git.diff_lines_4h > 0
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let root = ctx.project_root.clone();
        let evidence = tokio::task::spawn_blocking(move || {
            let git = collect_local_git_evidence(&root, false);
            dependency_manifest_evidence(git.as_ref(), &root)
        })
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("buddy dependency radar scan task failed: {err}");
            DependencyManifestEvidence::default()
        });
        if !dependency_manifest_trigger(&evidence) {
            return BuddyJobResult::default();
        }
        execute_autonomous_job(
            gcx,
            &ctx,
            dependency_radar_definition(),
            render_dependency_evidence(&evidence),
        )
        .await
    }
}

#[async_trait::async_trait]
impl BuddyJob for DocsGardenerJob {
    fn id(&self) -> &str {
        DOCS_GARDENER_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        docs_gardener_definition().cooldown_seconds
    }

    fn priority(&self) -> u32 {
        docs_gardener_definition().scheduler_priority
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        ctx.pulse.git.uncommitted_files > 0 || ctx.pulse.git.diff_lines_4h > 0
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let root = ctx.project_root.clone();
        let evidence = match tokio::task::spawn_blocking(move || {
            let git = collect_local_git_evidence(&root, false);
            docs_evidence(git.as_ref(), &root)
        })
        .await
        {
            Ok(evidence) => evidence,
            Err(err) => {
                tracing::warn!("buddy docs gardener scan task failed: {err}");
                return BuddyJobResult::default();
            }
        };
        if !docs_trigger(&evidence) {
            return BuddyJobResult::default();
        }
        execute_autonomous_job(
            gcx,
            &ctx,
            docs_gardener_definition(),
            render_docs_evidence(&evidence),
        )
        .await
    }
}

#[async_trait::async_trait]
impl BuddyJob for ArchitectureDriftWatcherJob {
    fn id(&self) -> &str {
        ARCHITECTURE_DRIFT_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        architecture_drift_definition().cooldown_seconds
    }

    fn priority(&self) -> u32 {
        architecture_drift_definition().scheduler_priority
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        ctx.pulse.git.uncommitted_files > 0 || ctx.pulse.git.diff_lines_4h > 0
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let root = ctx.project_root.clone();
        let evidence = tokio::task::spawn_blocking(move || {
            collect_local_git_evidence(&root, false).map(|git| architecture_evidence(&git))
        })
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("buddy architecture drift scan task failed: {err}");
            None
        });
        let Some(evidence) = evidence else {
            return BuddyJobResult::default();
        };
        if !architecture_trigger(&evidence) {
            return BuddyJobResult::default();
        }
        execute_autonomous_job(
            gcx,
            &ctx,
            architecture_drift_definition(),
            render_architecture_evidence(&evidence),
        )
        .await
    }
}
#[cfg(test)]
pub(super) async fn execute_dependency_radar_with_evidence(
    gcx: AppState,
    ctx: &BuddyJobContext,
    evidence: DependencyManifestEvidence,
) -> BuddyJobResult {
    if !dependency_manifest_trigger(&evidence) {
        return BuddyJobResult::default();
    }
    execute_built_autonomous_job(
        gcx,
        ctx,
        dependency_radar_definition(),
        build_autonomous_job_spec(
            dependency_radar_definition(),
            render_dependency_evidence(&evidence),
        ),
    )
    .await
}
