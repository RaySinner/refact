use super::*;

pub struct BuddyMemoryGardenerJob;
pub struct BuddyKnowledgeConflictResolverJob;
pub struct BuddyBehaviorLearnerJob;
pub struct BuddyUserHabitCoachJob;
pub struct BuddyModelCostOptimizerJob;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MemoryRouting {
    MemoryGardener,
    KnowledgeConflictResolver,
}
pub(super) const MEMORY_COOLDOWN_SECS: u64 = 6 * 60 * 60;
pub(super) const MAX_FACT_EVIDENCE_ITEMS: usize = 12;
pub(super) fn fact_kind_name(kind: BuddyFactKind) -> &'static str {
    match kind {
        BuddyFactKind::TaskStuck => "task_stuck",
        BuddyFactKind::TaskAbandoned => "task_abandoned",
        BuddyFactKind::TaskClusterDuplicate => "task_cluster_duplicate",
        BuddyFactKind::TrajectoryClutter => "trajectory_clutter",
        BuddyFactKind::ChatRetryStreak => "chat_retry_streak",
        BuddyFactKind::MemoryOrphan => "memory_orphan",
        BuddyFactKind::MemoryStaleConflict => "memory_stale_conflict",
        BuddyFactKind::MemoryRecurringLesson => "memory_recurring_lesson",
        BuddyFactKind::ModePromptOverlap => "mode_prompt_overlap",
        BuddyFactKind::SkillTriggerWeak => "skill_trigger_weak",
        BuddyFactKind::AgentsMdGapDetected => "agents_md_gap_detected",
        BuddyFactKind::DefaultModelMissing => "default_model_missing",
        BuddyFactKind::BrokenModelReference => "broken_model_reference",
        BuddyFactKind::McpAuthExpired => "mcp_auth_expired",
        BuddyFactKind::IntegrationFailing => "integration_failing",
        BuddyFactKind::DiagnosticCluster => "diagnostic_cluster",
        BuddyFactKind::FrontendErrorBurst => "frontend_error_burst",
        BuddyFactKind::GitDiffWidening => "git_diff_widening",
        BuddyFactKind::UncommittedPressure => "uncommitted_pressure",
        BuddyFactKind::WorktreeHygiene => "worktree_hygiene",
        BuddyFactKind::AgentTurnStalled => "agent_turn_stalled",
    }
}

pub(super) fn push_payload_string(lines: &mut Vec<String>, label: &str, value: &serde_json::Value) {
    if let Some(text) = value.as_str() {
        let text = redact_and_cap_text(text, 240);
        if !text.is_empty() {
            lines.push(format!("{}: {}", label, text));
        }
    }
}

pub(super) fn payload_string_array(
    value: &serde_json::Value,
    key: &str,
    max: usize,
) -> Vec<String> {
    let mut items: Vec<String> = value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(|item| redact_and_cap_text(item, 120))
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default();
    items.sort();
    items.dedup();
    items.truncate(max);
    items
}

pub(super) fn memory_fact_to_evidence_line(fact: &BuddyFact) -> String {
    let mut parts = vec![format!(
        "{} key={} source={} confidence={:.2}",
        fact_kind_name(fact.kind),
        redact_and_cap_text(&fact.key, 180),
        fact.source,
        fact.confidence
    )];
    if let Some(count) = fact.payload.get("count").and_then(|v| v.as_u64()) {
        parts.push(format!("count={}", count));
    }
    let memory_ids = payload_string_array(&fact.payload, "memory_ids", 8);
    if !memory_ids.is_empty() {
        parts.push(format!("memory_ids={}", memory_ids.join(", ")));
    }
    let doc_ids = payload_string_array(&fact.payload, "doc_ids", 8);
    if !doc_ids.is_empty() {
        parts.push(format!("doc_ids={}", doc_ids.join(", ")));
    }
    let tags = payload_string_array(&fact.payload, "tags", 8);
    if !tags.is_empty() {
        parts.push(format!("tags={}", tags.join(", ")));
    }
    if let Some(title) = fact.payload.get("title").and_then(|v| v.as_str()) {
        parts.push(format!("title={}", redact_and_cap_text(title, 160)));
    }
    if let Some(summary) = fact
        .payload
        .get("conflict_summary")
        .or_else(|| fact.payload.get("summary"))
        .and_then(|v| v.as_str())
    {
        parts.push(format!("summary={}", redact_and_cap_text(summary, 240)));
    }
    if let Some(tag_hash) = fact.payload.get("tag_hash").and_then(|v| v.as_str()) {
        parts.push(format!("tag_hash={}", redact_and_cap_text(tag_hash, 120)));
    }
    parts.join("; ")
}

pub(super) fn stable_facts<'a>(facts: impl Iterator<Item = &'a BuddyFact>) -> Vec<&'a BuddyFact> {
    let mut facts = facts.collect::<Vec<_>>();
    facts.sort_by(|a, b| {
        fact_kind_name(a.kind)
            .cmp(fact_kind_name(b.kind))
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.source.cmp(b.source))
            .then_with(|| a.seen_at.cmp(&b.seen_at))
    });
    facts
}

pub(super) fn memory_route_for_fact_kind(kind: BuddyFactKind) -> Option<MemoryRouting> {
    match kind {
        BuddyFactKind::MemoryOrphan | BuddyFactKind::MemoryRecurringLesson => {
            Some(MemoryRouting::MemoryGardener)
        }
        BuddyFactKind::MemoryStaleConflict => Some(MemoryRouting::KnowledgeConflictResolver),
        _ => None,
    }
}

pub(super) fn memory_gardener_evidence(ctx: &BuddyJobContext) -> Option<AutonomousEvidence> {
    let mut lines = Vec::new();
    if ctx.pulse.memory.total > 0 || ctx.pulse.memory.orphan > 0 {
        lines.push(format!(
            "memory_pulse total={} orphan={}",
            ctx.pulse.memory.total, ctx.pulse.memory.orphan
        ));
    }
    for fact in stable_facts(ctx.facts.iter().filter(|fact| {
        matches!(
            memory_route_for_fact_kind(fact.kind),
            Some(MemoryRouting::MemoryGardener)
        )
    }))
    .into_iter()
    .take(MAX_FACT_EVIDENCE_ITEMS)
    {
        lines.push(memory_fact_to_evidence_line(fact));
    }
    if ctx.pulse.memory.orphan == 0
        && !lines
            .iter()
            .any(|line| line.contains("memory_orphan") || line.contains("memory_recurring_lesson"))
    {
        return None;
    }
    Some(AutonomousEvidence {
        prompt: "Review memory garden signals and recommend safe cleanup, consolidation, or follow-up. Use only the metadata in evidence; do not assume full memory contents.".to_string(),
        evidence: lines.join("\n"),
    })
}

pub(super) fn stale_conflict_fact_actionable(fact: &BuddyFact) -> bool {
    if fact.kind != BuddyFactKind::MemoryStaleConflict {
        return false;
    }
    !payload_string_array(&fact.payload, "doc_ids", 1).is_empty()
        || !payload_string_array(&fact.payload, "memory_ids", 1).is_empty()
        || fact
            .payload
            .get("conflict_summary")
            .or_else(|| fact.payload.get("summary"))
            .and_then(|v| v.as_str())
            .map(|text| !text.trim().is_empty())
            .unwrap_or(false)
}

pub(super) fn knowledge_conflict_evidence(ctx: &BuddyJobContext) -> Option<AutonomousEvidence> {
    let facts = stable_facts(ctx.facts.iter().filter(|fact| {
        matches!(
            memory_route_for_fact_kind(fact.kind),
            Some(MemoryRouting::KnowledgeConflictResolver)
        ) && stale_conflict_fact_actionable(fact)
    }))
    .into_iter()
    .take(MAX_FACT_EVIDENCE_ITEMS)
    .collect::<Vec<_>>();
    if facts.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    if ctx.pulse.memory.stale_conflicts > 0 {
        lines.push(format!(
            "memory_pulse stale_conflicts={}",
            ctx.pulse.memory.stale_conflicts
        ));
    }
    for fact in facts {
        lines.push(memory_fact_to_evidence_line(fact));
    }
    Some(AutonomousEvidence {
        prompt: "Review stale/conflicting knowledge signals and propose a safe resolution plan. Use only doc ids and conflict summaries from evidence; never infer full document bodies.".to_string(),
        evidence: lines.join("\n"),
    })
}
#[async_trait::async_trait]
impl BuddyJob for BuddyMemoryGardenerJob {
    fn id(&self) -> &str {
        MEMORY_GARDENER_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        MEMORY_COOLDOWN_SECS
    }

    fn priority(&self) -> u32 {
        20
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        let Some(evidence) = memory_gardener_evidence(ctx) else {
            return false;
        };
        !same_signal_or_failed_backoff(
            ctx,
            &build_spec(self.id(), evidence).signal_hash,
            self.cooldown_seconds(),
        )
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let Some(evidence) = memory_gardener_evidence(&ctx) else {
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

#[async_trait::async_trait]
impl BuddyJob for BuddyKnowledgeConflictResolverJob {
    fn id(&self) -> &str {
        KNOWLEDGE_CONFLICT_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        MEMORY_COOLDOWN_SECS
    }

    fn priority(&self) -> u32 {
        21
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        let Some(evidence) = knowledge_conflict_evidence(ctx) else {
            return false;
        };
        !same_signal_or_failed_backoff(
            ctx,
            &build_spec(self.id(), evidence).signal_hash,
            self.cooldown_seconds(),
        )
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let Some(evidence) = knowledge_conflict_evidence(&ctx) else {
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
