use super::*;

pub(super) const MODEL_COST_RECENT_EVENT_LIMIT: usize = 500;
pub(super) fn is_autonomous_buddy_workflow_identifier(identifier: &str) -> bool {
    let normalized = identifier.to_ascii_lowercase();
    is_autonomous_workflow_id(&normalized)
        || normalized
            .strip_prefix("buddy-")
            .and_then(|rest| rest.split_once('-').map(|(workflow_id, _)| workflow_id))
            .map(is_autonomous_workflow_id)
            .unwrap_or(false)
}

pub(super) fn is_buddy_autonomous_report_event(event: &LlmCallEvent) -> bool {
    let identifiers = [
        event.chat_id.as_str(),
        event.root_chat_id.as_deref().unwrap_or_default(),
        event.task_id.as_deref().unwrap_or_default(),
        event.task_role.as_deref().unwrap_or_default(),
        event.agent_id.as_deref().unwrap_or_default(),
        event.card_id.as_deref().unwrap_or_default(),
    ];
    identifiers
        .iter()
        .any(|identifier| is_autonomous_buddy_workflow_identifier(identifier))
}

pub(super) fn model_cost_input_events(events: &[LlmCallEvent]) -> Vec<LlmCallEvent> {
    let mut filtered = events
        .iter()
        .filter(|event| !is_buddy_autonomous_report_event(event))
        .cloned()
        .collect::<Vec<_>>();
    filtered.sort_by(|a, b| {
        a.ts_start
            .cmp(&b.ts_start)
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.chat_id.cmp(&b.chat_id))
    });
    if filtered.len() > MODEL_COST_RECENT_EVENT_LIMIT {
        filtered.drain(0..filtered.len() - MODEL_COST_RECENT_EVENT_LIMIT);
    }
    filtered
}

pub(super) fn model_cost_evidence_from_events(
    events: &[LlmCallEvent],
) -> Option<AutonomousEvidence> {
    let events = model_cost_input_events(events);
    if events.len() < 5 {
        return None;
    }
    let summary = crate::stats::reader::aggregate_summary(&events, None, None);
    let totals = &summary.totals;
    let high_failure = totals.failed_calls >= 3
        && totals.failed_calls as f64 / totals.total_calls.max(1) as f64 >= 0.25;
    let high_latency = totals.avg_duration_ms >= 20_000;
    let high_tokens = totals.total_tokens >= 250_000;
    let high_cost = totals.total_cost_usd >= 1.0;
    let model_issue = summary.by_model.iter().any(|model| {
        (model.failed_calls >= 3
            && model.failed_calls as f64 / model.total_calls.max(1) as f64 >= 0.25)
            || model.avg_duration_ms >= 20_000
            || model.total_tokens >= 150_000
            || model.total_cost_usd >= 0.75
    });
    if !(high_failure || high_latency || high_tokens || high_cost || model_issue) {
        return None;
    }

    let mut lines = vec![format!(
        "recent_window max_events={} calls_bucket={} successful_bucket={} failed_bucket={} failure_rate_bucket={} total_tokens_bucket={} prompt_tokens_bucket={} completion_tokens_bucket={} cache_read_tokens_bucket={} cache_creation_tokens_bucket={} total_cost_usd_bucket={} avg_duration_bucket={} conversations_bucket={} messages_sent_bucket={}",
        MODEL_COST_RECENT_EVENT_LIMIT,
        bucket_count(totals.total_calls),
        bucket_count(totals.successful_calls),
        bucket_count(totals.failed_calls),
        bucket_rate(totals.failed_calls, totals.total_calls),
        bucket_tokens(totals.total_tokens),
        bucket_tokens(totals.total_prompt_tokens),
        bucket_tokens(totals.total_completion_tokens),
        bucket_tokens(totals.total_cache_read_tokens),
        bucket_tokens(totals.total_cache_creation_tokens),
        bucket_cost(totals.total_cost_usd),
        bucket_duration_ms(totals.avg_duration_ms),
        bucket_count(totals.total_conversations),
        bucket_count(totals.total_messages_sent)
    )];
    for model in summary.by_model.iter().take(8) {
        lines.push(format!(
            "model id={} provider={} model={} calls_bucket={} failed_bucket={} failure_rate_bucket={} tokens_bucket={} cost_usd_bucket={} avg_duration_bucket={}",
            redact_and_cap_text(&model.model_id, 120),
            redact_and_cap_text(&model.provider, 80),
            redact_and_cap_text(&model.model, 100),
            bucket_count(model.total_calls),
            bucket_count(model.failed_calls),
            bucket_rate(model.failed_calls, model.total_calls),
            bucket_tokens(model.total_tokens),
            bucket_cost(model.total_cost_usd),
            bucket_duration_ms(model.avg_duration_ms)
        ));
    }
    for provider in summary.by_provider.iter().take(6) {
        lines.push(format!(
            "provider name={} calls_bucket={} failed_bucket={} failure_rate_bucket={} tokens_bucket={} cost_usd_bucket={} total_duration_bucket={}",
            redact_and_cap_text(&provider.provider, 80),
            bucket_count(provider.total_calls),
            bucket_count(provider.failed_calls),
            bucket_rate(provider.failed_calls, provider.total_calls),
            bucket_tokens(provider.total_tokens),
            bucket_cost(provider.total_cost_usd),
            bucket_duration_ms(provider.total_duration_ms)
        ));
    }
    Some(AutonomousEvidence {
        prompt: "Analyze aggregate LLM usage, reliability, latency, token, and spend signals. Recommend model or configuration optimizations using aggregate stats only; do not infer message content.".to_string(),
        evidence: lines.join("\n"),
    })
}

pub(super) async fn model_cost_evidence(gcx: AppState) -> Option<AutonomousEvidence> {
    let stats_dirs = crate::stats::get_stats_dirs_for_read(gcx.gcx.clone()).await;
    let events = tokio::task::spawn_blocking(move || {
        crate::stats::reader::read_recent_stats_events_from_dirs(
            &stats_dirs,
            MODEL_COST_RECENT_EVENT_LIMIT,
        )
    })
    .await
    .unwrap_or_else(|err| {
        tracing::warn!("buddy model cost stats scan task failed: {err}");
        Vec::new()
    });
    model_cost_evidence_from_events(&events)
}
#[async_trait::async_trait]
impl BuddyJob for BuddyModelCostOptimizerJob {
    fn id(&self) -> &str {
        MODEL_COST_OPTIMIZER_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        MEMORY_COOLDOWN_SECS
    }

    fn priority(&self) -> u32 {
        24
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        ctx.total_workflow_runs >= 5 || ctx.pulse.providers.quota_warnings > 0
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let Some(evidence) = model_cost_evidence(gcx.clone()).await else {
            return BuddyJobResult::default();
        };
        execute_autonomous_spec(
            gcx,
            &ctx,
            build_spec(self.id(), evidence),
            self.cooldown_seconds(),
        )
        .await
    }
}
