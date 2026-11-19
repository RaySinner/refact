use chrono::{DateTime, Local, Timelike, Utc};

use crate::app_state::AppState;
use refact_buddy_core::actor::redact_sensitive;
use crate::buddy_scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::types::{BuddyChatPhrase, BuddyChatPhraseBank, BuddyFactKind};
use crate::buddy_voice_service::{voice_service, VoiceCtx};

pub struct ChatPhraseBankJob;

pub const FAILED_RESULT_PREFIX: &str = "failed:";
const JOB_ID: &str = "chat_phrase_bank";
const COOLDOWN_SECONDS: u64 = 0;
const PRIORITY: u32 = 4;
const MAX_EVIDENCE_LINES: usize = 18;
const MAX_JSON_ARRAY_ITEMS: usize = 12;
const MAX_JSON_OBJECT_FIELDS: usize = 24;
const MAX_JSON_DEPTH: usize = 6;
const MAX_JSON_STRING_CHARS: usize = 120;
pub const PHRASE_KINDS: [&str; 4] = ["humor", "insight", "bug", "ambient"];
pub const PHRASES_PER_KIND: usize = 3;

fn refresh_hour(settings: &BuddySettings) -> u32 {
    settings.daily_digest_hour.unwrap_or(18).min(23) as u32
}

pub fn date_key(now: &DateTime<Local>) -> String {
    now.date_naive().to_string()
}

pub fn should_run_at(ctx: &BuddyJobContext, now: DateTime<Local>) -> bool {
    let today = date_key(&now);
    let failed_today = format!("{FAILED_RESULT_PREFIX}{today}");
    if ctx
        .job_state
        .last_result
        .as_deref()
        .is_some_and(|result| result == today || result == failed_today.as_str())
    {
        return false;
    }
    now.hour() >= refresh_hour(&ctx.settings)
}

fn kind_token(kind: BuddyFactKind) -> &'static str {
    match kind {
        BuddyFactKind::TaskStuck => "task-stuck",
        BuddyFactKind::TaskAbandoned => "task-abandoned",
        BuddyFactKind::TaskClusterDuplicate => "task-duplicates",
        BuddyFactKind::TrajectoryClutter => "trajectory-clutter",
        BuddyFactKind::ChatRetryStreak => "chat-retries",
        BuddyFactKind::MemoryOrphan => "memory-orphan",
        BuddyFactKind::MemoryStaleConflict => "memory-conflict",
        BuddyFactKind::MemoryRecurringLesson => "memory-lesson",
        BuddyFactKind::ModePromptOverlap => "prompt-overlap",
        BuddyFactKind::SkillTriggerWeak => "skill-trigger",
        BuddyFactKind::AgentsMdGapDetected => "agents-md-gap",
        BuddyFactKind::DefaultModelMissing => "missing-model",
        BuddyFactKind::BrokenModelReference => "broken-model",
        BuddyFactKind::McpAuthExpired => "mcp-auth",
        BuddyFactKind::IntegrationFailing => "integration-failing",
        BuddyFactKind::DiagnosticCluster => "diagnostic-cluster",
        BuddyFactKind::FrontendErrorBurst => "frontend-errors",
        BuddyFactKind::GitDiffWidening => "git-diff-widening",
        BuddyFactKind::UncommittedPressure => "uncommitted-pressure",
        BuddyFactKind::WorktreeHygiene => "worktree-hygiene",
        BuddyFactKind::AgentTurnStalled => "agent-turn-stalled",
    }
}

fn cap_text(value: &str, max_chars: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn safe_evidence_text(value: &str, max_chars: usize) -> String {
    cap_text(&redact_sensitive(value), max_chars)
}

fn sensitive_json_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "authorization",
        "cookie",
        "password",
        "secret",
        "token",
    ]
    .into_iter()
    .any(|needle| key.contains(needle))
}

fn redact_evidence_json(value: &serde_json::Value) -> serde_json::Value {
    redact_evidence_json_at_depth(value, MAX_JSON_DEPTH)
}

fn redact_evidence_json_at_depth(
    value: &serde_json::Value,
    remaining_depth: usize,
) -> serde_json::Value {
    if remaining_depth == 0 {
        return serde_json::Value::String("[TRUNCATED]".to_string());
    }
    match value {
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .take(MAX_JSON_ARRAY_ITEMS)
                .map(|value| redact_evidence_json_at_depth(value, remaining_depth - 1))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .take(MAX_JSON_OBJECT_FIELDS)
                .map(|(key, value)| {
                    let value = if sensitive_json_key(key) {
                        serde_json::Value::String("[REDACTED]".to_string())
                    } else {
                        redact_evidence_json_at_depth(value, remaining_depth - 1)
                    };
                    (redact_sensitive(key), value)
                })
                .collect(),
        ),
        serde_json::Value::String(text) => {
            serde_json::Value::String(safe_evidence_text(text, MAX_JSON_STRING_CHARS))
        }
        other => other.clone(),
    }
}

fn safe_evidence_json(value: &serde_json::Value, max_chars: usize) -> String {
    safe_evidence_text(&redact_evidence_json(value).to_string(), max_chars)
}

fn phrase_bank_pulse_summary(ctx: &BuddyJobContext) -> String {
    let top_errors = safe_evidence_text(&ctx.pulse.diagnostics.top_error_types.join("|"), 160);
    format!(
        "tasks={} recent_stuck_alerts_1h={} memory={} pending={} mcp={} failing={} diagnostics={} top_errors={} git_files={} diff_lines={} worktrees={}",
        ctx.pulse.tasks.total,
        ctx.pulse.tasks.recent_stuck_alert_count_1h(),
        ctx.pulse.memory.total,
        ctx.pulse.memory.pending_ops,
        ctx.pulse.mcp.total,
        ctx.pulse.mcp.failing,
        ctx.pulse.diagnostics.last_hour,
        top_errors,
        ctx.pulse.git.uncommitted_files,
        ctx.pulse.git.diff_lines_4h,
        ctx.pulse.worktrees.total,
    )
}

fn phrase_bank_evidence(ctx: &BuddyJobContext) -> String {
    let mut lines = vec![format!("pulse: {}", phrase_bank_pulse_summary(ctx))];

    for activity in ctx.recent_activities.iter().rev().take(6) {
        lines.push(format!(
            "activity:{}:{}:{}",
            safe_evidence_text(&activity.activity_type, 32),
            safe_evidence_text(&activity.title, 80),
            safe_evidence_text(&activity.description, 120),
        ));
    }

    for fact in ctx.facts.iter().rev().take(8) {
        lines.push(format!(
            "fact:{} confidence={:.2} payload={}",
            kind_token(fact.kind),
            fact.confidence,
            safe_evidence_json(&fact.payload, 160),
        ));
    }

    for workflow in ctx.workflow_summaries.iter().rev().take(3) {
        let outcome = safe_evidence_text(workflow.last_outcome.as_deref().unwrap_or("none"), 120);
        lines.push(format!(
            "workflow:{} runs={} outcome={}",
            safe_evidence_text(&workflow.workflow_id, 48),
            workflow.run_count,
            outcome,
        ));
    }

    lines.truncate(MAX_EVIDENCE_LINES);
    lines.join("\n")
}

fn usable_phrase_count(lines: &[BuddyChatPhrase]) -> bool {
    lines.len() >= PHRASE_KINDS.len() * PHRASES_PER_KIND
        && PHRASE_KINDS
            .into_iter()
            .all(|kind| lines.iter().filter(|line| line.kind == kind).count() >= PHRASES_PER_KIND)
}

fn normalize_phrase_lines(lines: Vec<BuddyChatPhrase>) -> Vec<BuddyChatPhrase> {
    let mut normalized = Vec::with_capacity(PHRASE_KINDS.len() * PHRASES_PER_KIND);
    for kind in PHRASE_KINDS {
        normalized.extend(
            lines
                .iter()
                .filter(|line| line.kind == kind)
                .take(PHRASES_PER_KIND)
                .cloned(),
        );
    }
    normalized
}

pub async fn build_chat_phrase_bank(
    gcx: AppState,
    ctx: &BuddyJobContext,
    now: DateTime<Utc>,
) -> Option<BuddyChatPhraseBank> {
    let evidence = phrase_bank_evidence(ctx);
    let voice = voice_service().await;
    let voice_ctx = VoiceCtx {
        persona: &ctx.personality,
        identity_name: ctx.identity_name.as_str(),
        pulse_one_liner: phrase_bank_pulse_summary(ctx),
        workflow_id: Some(JOB_ID),
        workflow_summary: Some(evidence.as_str()),
    };
    let lines = voice.render_chat_phrase_bank(gcx, voice_ctx).await;
    if !usable_phrase_count(&lines) {
        return None;
    }
    let lines = normalize_phrase_lines(lines);
    Some(BuddyChatPhraseBank {
        day: now.date_naive().to_string(),
        generated_at: now.to_rfc3339(),
        evidence_summary: evidence,
        lines,
    })
}

#[async_trait::async_trait]
impl BuddyJob for ChatPhraseBankJob {
    fn id(&self) -> &str {
        JOB_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        COOLDOWN_SECONDS
    }

    fn priority(&self) -> u32 {
        PRIORITY
    }

    fn records_empty_result(&self) -> bool {
        false
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        should_run_at(ctx, Local::now())
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let now = Utc::now();
        let key = date_key(&Local::now());
        match build_chat_phrase_bank(gcx, &ctx, now).await {
            Some(chat_phrase_bank) => BuddyJobResult {
                chat_phrase_bank: Some(chat_phrase_bank),
                last_result: Some(key),
                ..Default::default()
            },
            None => BuddyJobResult {
                last_result: Some(format!("{FAILED_RESULT_PREFIX}{key}")),
                ..Default::default()
            },
        }
    }
}
