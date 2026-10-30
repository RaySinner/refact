use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::app_state::AppState;
pub use refact_buddy_core::actor::*;
pub use refact_buddy_core::runtime_writer::{run_runtime_queue_writer, RuntimeQueueWriteOp};
use refact_buddy_core::user_action::UserAction;
use super::events::BuddyEvent;
use super::facts::FactStore;
use super::humor::{HumorPlan, HumorService};
use super::memory_lifecycle::detect_memory_lifecycle_ops_from_knowledge_dirs;
use super::observers::{build_observer_registry, BuddyObserver, ObserverContext};
use super::opportunities::{primary_fact_kind_for_opportunity, OpportunityDetector};
use super::policy::{evaluate_with_mutes, PolicyDecision};
use super::snapshot::BuddySnapshot;
use super::types::{
    BuddyActivity, BuddyFact, BuddyFactKind, BuddyOpportunity, BuddyPersonalityProfile, BuddyPulse,
    BuddyQuest, BuddyRuntimeEvent, BuddySpeechItem, BuddySuggestion,
};
use super::voice_service::{voice_service, SpeechIntent, VoiceCtx, VoiceIntent};

const PET_DECAY_INTERVAL_SECS: u64 = 15;
const OBSERVER_CONCURRENCY: usize = 4;
const MEMORY_OPS_ARCHIVE_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
const MEMORY_OPS_COMPACT_INTERVAL_SECS: u64 = 6 * 60 * 60;
const MEMORY_OPS_DRAIN_INTERVAL_SECS: u64 = 600;
const MEMORY_OPS_DRAIN_MAX_APPLIES: usize = 200;
const BUDDY_SPEECH_MAX_CHARS: usize = 280;
const BUDDY_TITLE_MAX_CHARS: usize = 120;
const BUDDY_DESCRIPTION_MAX_CHARS: usize = 500;
const BUDDY_STATIC_SPEECH_FALLBACK: &str = "Tiny gremlin update: something needs attention.";
const BUDDY_STATIC_TITLE_FALLBACK: &str = "Buddy update";
const BUDDY_STATIC_DESCRIPTION_FALLBACK: &str = "Buddy has an update ready.";

pub(crate) async fn observe_buddy_facts_parallel(
    due_observers: Vec<Arc<dyn BuddyObserver>>,
    app: AppState,
    project_root: std::path::PathBuf,
    now: DateTime<Utc>,
) -> Vec<(u64, Vec<BuddyFact>)> {
    use futures::stream::{FuturesUnordered, StreamExt};

    let mut pending = FuturesUnordered::new();
    let mut all_facts = Vec::new();
    for obs in due_observers {
        let app = app.clone();
        let project_root = project_root.clone();
        pending.push(async move {
            let ctx = ObserverContext { project_root, now };
            let refresh_ttl = obs.emission_refresh_ttl_seconds();
            let facts =
                tokio::time::timeout(tokio::time::Duration::from_secs(5), obs.observe(app, &ctx))
                    .await
                    .unwrap_or_default();
            (refresh_ttl, facts)
        });
        if pending.len() >= OBSERVER_CONCURRENCY {
            if let Some(group) = pending.next().await {
                all_facts.push(group);
            }
        }
    }
    while let Some(group) = pending.next().await {
        all_facts.push(group);
    }
    all_facts
}
fn parse_commit_activity_from_log(output: &str) -> Vec<UserAction> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let sha = parts.next()?.trim();
            let _subject_slug = parts.next()?;
            let message = parts.next()?.trim();
            if sha.is_empty() {
                return None;
            }
            Some(UserAction::CommitMade {
                sha: sha.to_string(),
                message_first_line: message.chars().take(80).collect(),
                files: 0,
                ts: Utc::now(),
            })
        })
        .collect()
}

async fn poll_commit_activity_once(app: AppState, project_root: PathBuf) {
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .arg("log")
            .arg("-10")
            .arg("--since=1h ago")
            .arg("--pretty=format:%H|%f|%s")
            .current_dir(project_root)
            .output()
    })
    .await;
    let Ok(Ok(output)) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let actions = parse_commit_activity_from_log(&text);
    if actions.is_empty() {
        return;
    }
    let user_activity = app.chat_ctx().buddy.user_activity.clone();
    if let Ok(mut ring) = user_activity.try_lock() {
        let mut existing = ring
            .snapshot()
            .iter()
            .filter_map(|action| match action {
                UserAction::CommitMade { sha, .. } => Some(sha.clone()),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for action in actions {
            if let UserAction::CommitMade { sha, .. } = &action {
                if existing.contains(sha) {
                    continue;
                }
                existing.insert(sha.clone());
            }
            ring.push(action);
        }
    };
}

async fn commit_activity_poller(
    app: AppState,
    project_root: PathBuf,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(300)) => {
                if shutdown_flag.load(Ordering::SeqCst) {
                    break;
                }
                poll_commit_activity_once(app.clone(), project_root.clone()).await;
            }
            _ = wait_for_shutdown(shutdown_flag.clone()) => {
                break;
            }
        }
    }
}

async fn wait_for_shutdown(shutdown_flag: Arc<std::sync::atomic::AtomicBool>) {
    while !shutdown_flag.load(Ordering::SeqCst) {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}
fn normalize_generated_buddy_text(raw: &str) -> String {
    raw.replace(['\r', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim()
        .to_string()
}

fn contains_redaction_marker(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("[redacted") || lower.contains("<redacted") || lower.contains("&lt;redacted")
}

fn cap_generated_buddy_text(text: &str, max_chars: usize) -> String {
    crate::llm::safe_truncate(text, max_chars)
        .trim()
        .to_string()
}

fn safe_buddy_candidate(text: &str, max_chars: usize) -> Option<String> {
    let normalized = normalize_generated_buddy_text(text);
    let redacted = redact_sensitive(&normalized);
    if normalized.is_empty() || redacted != normalized || contains_redaction_marker(&normalized) {
        None
    } else {
        Some(cap_generated_buddy_text(&normalized, max_chars))
    }
}

fn safe_generated_buddy_text(
    generated: &str,
    fallback: String,
    static_fallback: &'static str,
    max_chars: usize,
) -> String {
    safe_buddy_candidate(generated, max_chars)
        .or_else(|| safe_buddy_candidate(&fallback, max_chars))
        .unwrap_or_else(|| cap_generated_buddy_text(static_fallback, max_chars))
}

fn safe_generated_buddy_text_opt(
    generated: Option<String>,
    fallback: Option<String>,
    static_fallback: &'static str,
    max_chars: usize,
) -> Option<String> {
    generated
        .as_deref()
        .and_then(|text| safe_buddy_candidate(text, max_chars))
        .or_else(|| {
            fallback
                .as_deref()
                .and_then(|text| safe_buddy_candidate(text, max_chars))
        })
        .or_else(|| Some(cap_generated_buddy_text(static_fallback, max_chars)))
}
pub async fn render_buddy_speech(
    app: AppState,
    persona: BuddyPersonalityProfile,
    identity_name: String,
    pulse: BuddyPulse,
    workflow_id: Option<String>,
    workflow_summary: String,
    intent: SpeechIntent,
    fallback_text: String,
) -> BuddySpeechItem {
    let pulse_one_liner = format!(
        "{} pending ops, {} recent stuck task alerts",
        pulse.memory.pending_ops,
        pulse.tasks.recent_stuck_alert_count_1h()
    );
    let voice_ctx = VoiceCtx {
        persona: &persona,
        identity_name: identity_name.as_str(),
        pulse_one_liner,
        workflow_id: workflow_id.as_deref(),
        workflow_summary: Some(workflow_summary.as_str()),
    };
    let mut speech = voice_service()
        .await
        .render_speech(app, voice_ctx, intent)
        .await;
    speech.text = safe_generated_buddy_text(
        &speech.text,
        fallback_text,
        BUDDY_STATIC_SPEECH_FALLBACK,
        BUDDY_SPEECH_MAX_CHARS,
    );
    speech
}

pub async fn render_buddy_runtime_event(
    app: AppState,
    persona: BuddyPersonalityProfile,
    identity_name: String,
    pulse: BuddyPulse,
    workflow_id: Option<String>,
    workflow_summary: String,
    status: &str,
    fallback_title: String,
    fallback_description: Option<String>,
) -> (String, Option<String>) {
    let pulse_one_liner = format!(
        "{} pending ops, {} recent stuck task alerts",
        pulse.memory.pending_ops,
        pulse.tasks.recent_stuck_alert_count_1h()
    );
    let voice_ctx = VoiceCtx {
        persona: &persona,
        identity_name: identity_name.as_str(),
        pulse_one_liner,
        workflow_id: workflow_id.as_deref(),
        workflow_summary: Some(workflow_summary.as_str()),
    };
    let (title, description) = voice_service()
        .await
        .render_runtime_event(app, voice_ctx, status)
        .await;
    let title_is_safe = safe_buddy_candidate(&title, BUDDY_TITLE_MAX_CHARS).is_some();
    (
        safe_generated_buddy_text(
            &title,
            fallback_title,
            BUDDY_STATIC_TITLE_FALLBACK,
            BUDDY_TITLE_MAX_CHARS,
        ),
        safe_generated_buddy_text_opt(
            if title_is_safe { description } else { None },
            fallback_description,
            BUDDY_STATIC_DESCRIPTION_FALLBACK,
            BUDDY_DESCRIPTION_MAX_CHARS,
        ),
    )
}

pub async fn render_buddy_activity_title(
    app: AppState,
    persona: BuddyPersonalityProfile,
    identity_name: String,
    pulse: BuddyPulse,
    workflow_id: Option<String>,
    workflow_summary: String,
    intent: VoiceIntent,
    fallback_title: String,
) -> String {
    let pulse_one_liner = format!(
        "{} pending ops, {} recent stuck task alerts",
        pulse.memory.pending_ops,
        pulse.tasks.recent_stuck_alert_count_1h()
    );
    let voice_ctx = VoiceCtx {
        persona: &persona,
        identity_name: identity_name.as_str(),
        pulse_one_liner,
        workflow_id: workflow_id.as_deref(),
        workflow_summary: Some(workflow_summary.as_str()),
    };
    let title = voice_service()
        .await
        .render_activity_title(app, voice_ctx, intent)
        .await;
    safe_generated_buddy_text(
        &title,
        fallback_title,
        BUDDY_STATIC_TITLE_FALLBACK,
        BUDDY_TITLE_MAX_CHARS,
    )
}
pub async fn buddy_complete_event(app: AppState, dedupe_key: &str, status: &str) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.complete_runtime_event(dedupe_key, status);
    }
}

pub async fn buddy_enqueue_event(app: AppState, event: BuddyRuntimeEvent) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.enqueue_runtime_event(event);
    }
}

pub async fn resolve_buddy_chat_model(app: AppState) -> String {
    let ctx = app.chat_ctx();
    let caps_state = ctx.model.caps.read().await;
    let Some(caps) = caps_state.caps.as_ref() else {
        return String::new();
    };
    let buddy = caps.defaults.chat_buddy_model.trim();
    if !buddy.is_empty() && crate::caps::resolve_model(&caps.chat_models, buddy).is_ok() {
        buddy.to_string()
    } else {
        String::new()
    }
}

pub async fn buddy_snapshot(app: AppState) -> Option<BuddySnapshot> {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    lock.as_ref().map(|svc| svc.snapshot())
}

pub async fn buddy_record_workflow_telemetry(
    app: AppState,
    workflow_id: &str,
    llm_calls: u64,
    tokens_in: u64,
    tokens_out: u64,
    produced_output: bool,
) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.record_workflow_telemetry(
            workflow_id,
            llm_calls,
            tokens_in,
            tokens_out,
            produced_output,
        );
    }
}

pub async fn buddy_update_speech(app: AppState, speech: BuddySpeechItem) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.update_speech(speech);
    }
}

pub async fn buddy_llm_budget_exhausted(app: &AppState) -> bool {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    lock.as_ref()
        .map(|svc| svc.llm_budget_exhausted())
        .unwrap_or(false)
}

pub async fn buddy_update_speech_user(app: AppState, speech: BuddySpeechItem) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.update_speech_user_initiated(speech);
    }
}

pub struct CompletedQuestVoice {
    pub mutation: BuddyMutation,
    pub speech: BuddySpeechItem,
}

pub async fn complete_quest_with_voice(
    app: AppState,
    quest: BuddyQuest,
    persona: BuddyPersonalityProfile,
    identity_name: String,
    pulse: BuddyPulse,
) -> CompletedQuestVoice {
    let title = quest.title.clone();
    let fallback_speech = format!("Quest complete: {title}! Tiny victory dance?");
    let fallback_event = format!("Quest complete: {title}");
    let mut speech = render_buddy_speech(
        app.clone(),
        persona.clone(),
        identity_name.clone(),
        pulse.clone(),
        Some(quest.quest_type.clone()),
        fallback_speech.clone(),
        SpeechIntent::QuestComplete,
        fallback_speech,
    )
    .await;
    speech.id = format!("quest-complete-{}", quest.id);
    speech.ttl_seconds = 12;
    speech.dedupe_key = Some(format!("quest_complete_{}", quest.quest_type));
    speech.controls = vec![];

    let (event_title, event_speech) = render_buddy_runtime_event(
        app,
        persona,
        identity_name.clone(),
        pulse,
        Some(quest.quest_type.clone()),
        fallback_event.clone(),
        "completed",
        fallback_event.clone(),
        Some(fallback_event.clone()),
    )
    .await;

    CompletedQuestVoice {
        mutation: BuddyMutation {
            activity: Some(BuddyActivity {
                icon: quest.icon.clone(),
                title: event_title.clone(),
                description: format!(
                    "{} wrapped up '{title}' and earned a growth boost.",
                    identity_name
                ),
                timestamp: Utc::now().to_rfc3339(),
                activity_type: "quest_completed".to_string(),
                chat_id: None,
                failure_category: None,
                failure_summary: None,
            }),
            runtime_event: Some(BuddyRuntimeEvent {
                speech_text: event_speech,
                scene: Some("celebrate".to_string()),
                duration_hint: Some(10),
                persistent: false,
                controls: vec![],
                chat_id: None,
                ..make_runtime_event(
                    "task_completed",
                    &event_title,
                    "buddy_quest",
                    &format!("quest_complete_{}", quest.quest_type),
                    "completed",
                    Some("high"),
                )
            }),
            ..Default::default()
        },
        speech,
    }
}

pub async fn report_error_persisted(
    app: AppState,
    error_type: &str,
    error_msg: &str,
    source: Option<&str>,
    chat_id: Option<&str>,
) {
    report_error_persisted_with_model(app, error_type, error_msg, source, chat_id, None).await;
}

pub async fn report_error_persisted_with_model(
    app: AppState,
    error_type: &str,
    error_msg: &str,
    source: Option<&str>,
    chat_id: Option<&str>,
    model_id: Option<&str>,
) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    if let Some(svc) = lock.as_mut() {
        svc.report_error_with_model(error_type, error_msg, source, chat_id, model_id);
    }
}

pub async fn latest_project_root(app: AppState) -> Result<std::path::PathBuf, String> {
    let ctx = app.chat_ctx();
    crate::files_correction::get_unscoped_project_dirs_for_ctx(&ctx)
        .await
        .into_iter()
        .next()
        .ok_or_else(|| "no project root".to_string())
}

pub async fn load_diagnostics_for_service(
    project_root: &Path,
) -> Vec<super::diagnostics::DiagnosticContext> {
    match super::storage::load_recent_diagnostics(project_root, 100).await {
        Ok(diags) => diags,
        Err(err) => {
            warn!("buddy: failed to load diagnostic history: {}", err);
            Vec::new()
        }
    }
}

pub async fn resolve_diagnostic(
    app: AppState,
    diagnostic_index: Option<usize>,
    diagnostic_id: Option<&str>,
    collected_at: Option<&str>,
    fallback: Option<super::diagnostics::DiagnosticContext>,
) -> Result<super::diagnostics::DiagnosticContext, String> {
    let project_root = latest_project_root(app.clone()).await?;
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let lock = buddy_arc.lock().await;
    let svc = lock
        .as_ref()
        .ok_or_else(|| "buddy service not initialized".to_string())?;
    let by_id = diagnostic_id.and_then(|id| svc.diagnostic_by_id(id));
    let by_time = collected_at.and_then(|ts| svc.diagnostic_by_collected_at(ts));
    let recent = svc.recent_diagnostics.clone();
    drop(lock);

    if let Some(id) = diagnostic_id {
        if let Some(ctx) = by_id {
            return Ok(ctx);
        }
        let diags = super::storage::load_diagnostics(&project_root).await?;
        if let Some(ctx) = diags
            .into_iter()
            .find(|diag| super::diagnostics::diagnostic_id(diag) == id)
        {
            return Ok(ctx);
        }
        return Err("diagnostic id not found".to_string());
    }

    if let Some(ts) = collected_at {
        if let Some(ctx) = by_time {
            return Ok(ctx);
        }
        let diags = super::storage::load_diagnostics(&project_root).await?;
        if let Some(ctx) = diags.into_iter().find(|diag| diag.collected_at == ts) {
            return Ok(ctx);
        }
        return Err("diagnostic timestamp not found".to_string());
    }

    if let Some(idx) = diagnostic_index {
        let diags = if recent.is_empty() {
            super::storage::load_recent_diagnostics(&project_root, 100).await?
        } else {
            recent
        };
        return diags
            .get(idx)
            .cloned()
            .ok_or_else(|| "diagnostic index out of range".to_string());
    }

    fallback.ok_or_else(|| "provide diagnostic reference or error".to_string())
}
pub async fn buddy_apply(app: AppState, m: BuddyMutation) {
    let buddy_arc = app.chat_ctx().buddy.buddy.clone();
    let mut lock = buddy_arc.lock().await;
    let Some(svc) = lock.as_mut() else { return };
    if let Some(ev) = m.runtime_event {
        svc.enqueue_runtime_event(ev);
    }
    if m.xp > 0 {
        svc.grant_xp(m.xp);
    }
    if let Some(activity) = m.activity {
        svc.add_activity(activity);
    }
    if let Some(mood) = m.mood {
        svc.state.semantic.mood = mood;
        svc.dirty = true;
        let _ = svc.events_tx.send(BuddyEvent::StateUpdated {
            state: svc.state.clone(),
        });
    }
}

pub async fn buddy_background_task(app: AppState) {
    let ctx = app.chat_ctx();
    let project_root = loop {
        if ctx.runtime.shutdown_flag.load(Ordering::SeqCst) {
            return;
        }
        let dirs = crate::files_correction::get_unscoped_project_dirs_for_ctx(&ctx).await;
        if let Some(root) = dirs.into_iter().next() {
            break root;
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    };

    if let Err(e) = super::storage::bootstrap_buddy_storage(&project_root).await {
        warn!("buddy: failed to bootstrap storage: {}", e);
        return;
    }
    super::storage::gc_stale_memory_ops_tmp_files(&project_root).await;
    match super::storage::archive_memory_ops_if_oversized(
        &project_root,
        MEMORY_OPS_ARCHIVE_THRESHOLD_BYTES,
    )
    .await
    {
        Ok(true) => info!("buddy: archived oversized memory ops queue"),
        Ok(false) => {}
        Err(err) => warn!(
            "buddy: failed to archive oversized memory ops queue: {}",
            err
        ),
    }

    let state = super::state::load_state(&project_root).await;
    let settings = super::settings::load_settings(&project_root).await;
    let recent_diagnostics = load_diagnostics_for_service(&project_root).await;
    let mut memory_ops = super::storage::load_memory_ops_repairing(&project_root).await;
    let before_memory_ops = memory_ops.pending_count + memory_ops.approved_count;
    if before_memory_ops > 5000 {
        match super::storage::compact_memory_ops(&project_root).await {
            Ok(compacted) => {
                let after_memory_ops = compacted.pending_count + compacted.approved_count;
                info!(
                    "buddy: compacted large memory ops queue at startup: {} -> {} pending/approved ops",
                    before_memory_ops, after_memory_ops
                );
                memory_ops = compacted;
            }
            Err(err) => warn!(
                "buddy: failed to compact large memory ops queue at startup: {}",
                err
            ),
        }
    }
    let runtime_queue = super::storage::load_runtime_queue(&project_root).await;

    let events_tx = ctx.buddy.buddy_events_tx.clone();

    // Spawn the single-writer task that owns runtime_queue.jsonl. All queue
    // mutations forward to this channel, which preserves on-disk write order.
    let (queue_tx, queue_rx) = mpsc::unbounded_channel::<RuntimeQueueWriteOp>();
    let writer_root = project_root.clone();
    let writer_handle = tokio::spawn(async move {
        run_runtime_queue_writer(writer_root, queue_rx).await;
    });

    let mut service = BuddyService::new(
        project_root.clone(),
        state,
        settings,
        recent_diagnostics,
        runtime_queue,
        events_tx,
        Some(queue_tx),
    );
    service.set_memory_ops(memory_ops);
    service
        .draft_store
        .restore(super::storage::load_drafts(&project_root).await);
    let humor_service = Arc::new(tokio::sync::Mutex::new(HumorService::new()));
    let observers = build_observer_registry();

    let buddy_arc = ctx.buddy.buddy.clone();
    *buddy_arc.lock().await = Some(service);
    let initial_pulse =
        super::pulse::build_pulse(app.clone(), &project_root, &FactStore::new()).await;
    {
        let mut buddy = buddy_arc.lock().await;
        if let Some(svc) = buddy.as_mut() {
            svc.set_pulse(initial_pulse);
            if svc
                .state
                .chat_phrase_bank
                .as_ref()
                .is_some_and(|bank| !chat_phrase_bank_is_fresh(bank, Utc::now()))
            {
                svc.state.chat_phrase_bank = None;
                svc.dirty = true;
            }
        }
    }

    let agents_md = project_root.join("AGENTS.md");
    let setup_done = tokio::fs::try_exists(&agents_md).await.unwrap_or(false);
    if !setup_done {
        let mut guard = buddy_arc.lock().await;
        if let Some(svc) = guard.as_mut() {
            let already = svc
                .state
                .suggestion_state
                .iter()
                .any(|s| s.suggestion_type == "setup");
            if !already {
                let suggestion = BuddySuggestion {
                    id: "setup".to_string(),
                    suggestion_type: "setup".to_string(),
                    title: "Set up this project".to_string(),
                    description:
                        "Run setup to generate guidelines, integrations, and toolbox commands."
                            .to_string(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    dismissed: false,
                    controls: vec![],
                    quest: None,
                };
                svc.add_suggestion(suggestion);
            }
        }
    }

    info!("buddy: service started for {:?}", project_root);

    let scheduler = super::scheduler::BuddyScheduler::new();
    let shutdown_flag = ctx.runtime.shutdown_flag.clone();
    let commit_poller_handle = tokio::spawn(commit_activity_poller(
        app.clone(),
        project_root.clone(),
        shutdown_flag.clone(),
    ));
    let mut expiry_tick: u64 = 0;

    loop {
        if shutdown_flag.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        expiry_tick += 1;
        if expiry_tick % PET_DECAY_INTERVAL_SECS == 0 {
            let mut buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_mut() {
                svc.apply_pet_tick(PET_DECAY_INTERVAL_SECS);
            }
        }
        if expiry_tick % 60 == 0 {
            let mut buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_mut() {
                svc.expire_suggestions();
            }
        }
        // Pulse refresh + opportunity expiry every 60s
        if expiry_tick % 60 == 0 {
            let now = Utc::now();
            let fact_snap = {
                let buddy = buddy_arc.lock().await;
                buddy
                    .as_ref()
                    .map(|svc| svc.fact_store.iter().cloned().collect::<Vec<_>>())
            };
            if let Some(facts) = fact_snap {
                let mut tmp_store = FactStore::new();
                for f in facts {
                    tmp_store.ingest(f);
                }
                let knowledge_dirs =
                    crate::files_correction::get_unscoped_project_dirs_for_ctx(&ctx)
                        .await
                        .into_iter()
                        .map(|dir| dir.join(crate::file_filter::KNOWLEDGE_FOLDER_NAME))
                        .filter(|dir| dir.exists())
                        .collect::<Vec<_>>();
                let lifecycle_ops =
                    detect_memory_lifecycle_ops_from_knowledge_dirs(&knowledge_dirs, now).await;
                if !lifecycle_ops.is_empty() {
                    let mut memory_ops = super::storage::load_memory_ops(&project_root).await;
                    for op in lifecycle_ops {
                        match super::storage::enqueue_memory_op(&project_root, op).await {
                            Ok(updated) => memory_ops = updated,
                            Err(err) => {
                                warn!("buddy: failed to enqueue memory lifecycle op: {}", err)
                            }
                        }
                    }
                    let mut buddy = buddy_arc.lock().await;
                    if let Some(svc) = buddy.as_mut() {
                        svc.set_memory_ops(memory_ops);
                    }
                }
                let new_pulse =
                    super::pulse::build_pulse(app.clone(), &project_root, &tmp_store).await;
                let mut buddy = buddy_arc.lock().await;
                if let Some(svc) = buddy.as_mut() {
                    svc.set_pulse(new_pulse);
                    svc.expire_opportunities();
                    svc.opportunity_queue.refresh_cooldowns(now);
                    svc.expire_drafts(now);
                    svc.expire_runtime_events_at(now);
                }
            }
        }
        {
            let window = {
                let actions = {
                    let ring = ctx.buddy.user_activity.lock().await;
                    ring.snapshot()
                };
                super::speech_policy::auto_quiet_window_from_actions(&actions)
            };
            let mut buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_mut() {
                svc.auto_quiet_window = window;
            }
        }
        // Observer ticking — each observer respects its own cadence
        {
            let now = Utc::now();
            let due_observers = {
                let buddy = buddy_arc.lock().await;
                match buddy.as_ref() {
                    Some(svc) => {
                        let s = svc.settings.clone();
                        let lt = svc.last_observer_tick.clone();
                        let due: Vec<Arc<dyn BuddyObserver>> = observers
                            .iter()
                            .filter(|obs| {
                                if !obs.requires_setting(&s) {
                                    return false;
                                }
                                match lt.get(obs.id()) {
                                    Some(t) => {
                                        (now - *t).num_seconds() as u64 >= obs.cadence_seconds()
                                    }
                                    None => true,
                                }
                            })
                            .cloned()
                            .collect();
                        due
                    }
                    None => vec![],
                }
            };
            if !due_observers.is_empty() {
                // Run observers without holding buddy lock (prevents deadlock with
                // DiagnosticClusterObserver which also locks buddy).
                let all_facts = observe_buddy_facts_parallel(
                    due_observers.clone(),
                    app.clone(),
                    project_root.clone(),
                    now,
                )
                .await;
                // Phase 1: ingest facts, detect candidates, add non-humor opps — all under buddy lock.
                let (humor_tasks, pulse_for_humor, humor_arc) = {
                    let mut buddy = buddy_arc.lock().await;
                    if let Some(svc) = buddy.as_mut() {
                        for obs in &due_observers {
                            svc.last_observer_tick.insert(obs.id(), now);
                        }
                        for (refresh_ttl_secs, facts) in all_facts {
                            svc.fact_store.ingest_many_with_refresh_ttl(
                                facts,
                                chrono::Duration::seconds(refresh_ttl_secs as i64),
                            );
                        }
                        let candidates = OpportunityDetector::new().detect(
                            &svc.fact_store,
                            &svc.pulse,
                            &svc.opportunity_queue,
                        );
                        let mut humor_needed: Vec<(BuddyOpportunity, BuddyFactKind, u64)> = vec![];
                        for (opp, cooldown_secs) in candidates {
                            match evaluate_with_mutes(
                                &opp,
                                &svc.settings,
                                &svc.opportunity_queue,
                                &svc.state.muted_rules,
                            ) {
                                PolicyDecision::Drop { reason } => {
                                    tracing::debug!("buddy: opp dropped by policy: {}", reason);
                                }
                                PolicyDecision::Surface { humor_allowed } => {
                                    if humor_allowed {
                                        let kind = primary_fact_kind_for_opportunity(
                                            &opp,
                                            &svc.fact_store,
                                        );
                                        humor_needed.push((opp, kind, cooldown_secs));
                                    } else {
                                        svc.surface_opportunity_with_cooldown(opp, cooldown_secs);
                                    }
                                }
                            }
                        }
                        let pulse = svc.pulse.clone();
                        let humor_arc = humor_service.clone();
                        (humor_needed, pulse, humor_arc)
                    } else {
                        (
                            vec![],
                            BuddyPulse::default(),
                            humor_service.clone(),
                        )
                    }
                }; // buddy lock released — LLM humor calls happen outside the lock

                // Phase 1.5: Buddy may speak into a live agent chat. Runs after
                // the facts are ingested (they are the trigger) and outside the
                // buddy lock (it awaits boards and chat sessions).
                let interjection_plan = {
                    let buddy = buddy_arc.lock().await;
                    buddy.as_ref().map(|svc| {
                        let facts = svc.fact_store.iter().cloned().collect::<Vec<_>>();
                        (
                            super::chat_interjection::stuck_task_ids(&facts),
                            svc.settings.clone(),
                            svc.state.speech_rotation.clone(),
                            svc.auto_quiet_window,
                            svc.llm_budget_exhausted(),
                        )
                    })
                };
                if let Some((stuck, settings, rotation, auto_quiet_window, llm_exhausted)) =
                    interjection_plan.filter(|(stuck, ..)| !stuck.is_empty())
                {
                    let targets =
                        super::chat_interjection::collect_interjection_targets(&app, &stuck).await;
                    for target in targets {
                        let text =
                            super::chat_interjection::build_interjection_text(&target);
                        match super::chat_interjection::maybe_interject_with_agent(
                            app.clone(),
                            &target,
                            settings.clone(),
                            rotation.clone(),
                            auto_quiet_window,
                            llm_exhausted,
                        )
                        .await
                        {
                            Ok(_) => {
                                let mut buddy = buddy_arc.lock().await;
                                if let Some(svc) = buddy.as_mut() {
                                    svc.record_interjection_emission(&target.chat_id, &text);
                                }
                            }
                            Err(reason) => {
                                tracing::debug!(
                                    target: "buddy.chat_interjection",
                                    task_id = %target.task_id,
                                    card_id = %target.card_id,
                                    agent_chat_id = %target.chat_id,
                                    reason = reason.as_str(),
                                    "buddy interjection skipped"
                                );
                            }
                        }
                    }
                }

                // Phase 1.6: Buddy answers a task agent's question the planner has
                // left hanging. Deliberately independent of the interjection pass:
                // a question can be overdue while every agent is busy, and "the agent
                // is working" is not "the answer arrived". Off by default, so this
                // whole block is skipped unless the user asked for it.
                let backstop_plan = {
                    let buddy = buddy_arc.lock().await;
                    buddy.as_ref().and_then(|svc| {
                        svc.settings.planner_backstop_enabled.then(|| {
                            (
                                svc.settings.clone(),
                                svc.state.speech_rotation.clone(),
                                svc.auto_quiet_window,
                            )
                        })
                    })
                };
                if let Some((settings, rotation, auto_quiet_window)) = backstop_plan {
                    let task_ids: Vec<String> = crate::tasks::storage::list_tasks_for_ctx(&ctx)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|meta| {
                            !matches!(
                                meta.status,
                                crate::tasks::types::TaskStatus::Completed
                                    | crate::tasks::types::TaskStatus::Abandoned
                            )
                        })
                        .map(|meta| meta.id)
                        .collect();
                    let candidates =
                        super::planner_backstop_runner::collect_backstop_candidates(
                            &app, &task_ids,
                        )
                        .await;
                    for candidate in candidates {
                        let card_id = candidate.question.card_id.clone();
                        let outcome = super::planner_backstop_runner::maybe_answer_overdue_question(
                            &app,
                            settings.clone(),
                            rotation.clone(),
                            auto_quiet_window,
                            &candidate.task_id,
                            &candidate.question,
                        )
                        .await;
                        match &outcome {
                            super::planner_backstop_runner::BackstopOutcome::Answered { .. }
                            | super::planner_backstop_runner::BackstopOutcome::NotifiedHuman {
                                ..
                            } => {
                                // Charge the interjection budget: the agent paid a
                                // whole turn to read this, same as any other message
                                // Buddy puts in an agent's transcript.
                                let mut buddy = buddy_arc.lock().await;
                                if let Some(svc) = buddy.as_mut() {
                                    svc.record_interjection_emission(
                                        &format!("backstop:{card_id}"),
                                        "planner backstop",
                                    );
                                }
                            }
                            super::planner_backstop_runner::BackstopOutcome::Skipped {
                                question_id,
                                reason,
                            } => {
                                tracing::debug!(
                                    target: "buddy.planner_backstop",
                                    task_id = %candidate.task_id,
                                    card_id = %card_id,
                                    question_id = %question_id,
                                    reason = %reason,
                                    "buddy planner backstop skipped"
                                );
                            }
                        }
                    }
                }

                // Phase 2: attach humor outside the buddy lock.
                let mut ready: Vec<(BuddyOpportunity, u64)> = Vec::with_capacity(humor_tasks.len());
                for (mut opp, kind, cooldown_secs) in humor_tasks {
                    let plan = {
                        let mut humor = humor_arc.lock().await;
                        humor.plan_humor(kind, &pulse_for_humor)
                    };
                    match plan {
                        HumorPlan::Ready(line) => {
                            opp.humor = Some(line);
                        }
                        HumorPlan::Generate(reservation) => {
                            let lines = reservation.generate(app.clone()).await;
                            let line = {
                                let mut humor = humor_arc.lock().await;
                                humor.complete_humor(reservation, lines)
                            };
                            if let Some(line) = line {
                                opp.humor = Some(line);
                            }
                        }
                        HumorPlan::Skip => {}
                    }
                    ready.push((opp, cooldown_secs));
                }

                // Phase 3: re-acquire buddy lock to add humor-processed opps.
                if !ready.is_empty() {
                    let mut buddy = buddy_arc.lock().await;
                    if let Some(svc) = buddy.as_mut() {
                        for (opp, cooldown_secs) in ready {
                            svc.surface_opportunity_with_cooldown(opp, cooldown_secs);
                        }
                    }
                }
            }
        }
        if expiry_tick % 30 == 0 {
            scheduler
                .tick(app.clone(), buddy_arc.clone(), &project_root)
                .await;
        }
        let state_to_save = {
            let mut buddy = buddy_arc.lock().await;
            buddy.as_mut().and_then(|svc| {
                if svc.dirty {
                    svc.dirty = false;
                    Some(svc.state.clone())
                } else {
                    None
                }
            })
        };
        if let Some(s) = state_to_save {
            if let Err(e) = super::state::save_state(&project_root, &s).await {
                warn!("buddy: failed to save state: {}", e);
                if let Some(svc) = buddy_arc.lock().await.as_mut() {
                    svc.dirty = true;
                }
            }
        }
        let drafts_to_save = {
            let mut buddy = buddy_arc.lock().await;
            buddy.as_mut().and_then(|svc| {
                if svc.drafts_dirty {
                    svc.drafts_dirty = false;
                    Some(svc.draft_store.snapshot())
                } else {
                    None
                }
            })
        };
        if let Some(drafts) = drafts_to_save {
            if let Err(e) = super::storage::save_drafts(&project_root, &drafts).await {
                warn!("buddy: failed to save drafts: {}", e);
                if let Some(svc) = buddy_arc.lock().await.as_mut() {
                    svc.drafts_dirty = true;
                }
            }
        }

        // Periodic compaction: ask the writer task to rewrite the JSONL from
        // the in-memory queue every 5 minutes. The append-on-mutation log can
        // grow unbounded under churn (progress updates, repeated coalesces);
        // this collapses it back to one line per surviving event. Routing
        // through the same channel keeps writes strictly ordered.
        if expiry_tick % 300 == 0 {
            let buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_ref() {
                if let Some(tx) = &svc.queue_writer {
                    let _ = tx.send(RuntimeQueueWriteOp::Compact(svc.runtime_queue.clone()));
                }
            }
        }
        if expiry_tick % MEMORY_OPS_COMPACT_INTERVAL_SECS == 0 {
            match super::storage::compact_memory_ops(&project_root).await {
                Ok(memory_ops) => {
                    let mut buddy = buddy_arc.lock().await;
                    if let Some(svc) = buddy.as_mut() {
                        svc.set_memory_ops(memory_ops);
                    }
                }
                Err(err) => warn!("buddy: failed to compact memory ops queue: {}", err),
            }
        }
        if expiry_tick % MEMORY_OPS_DRAIN_INTERVAL_SECS == 0 {
            let drain_settings = {
                let buddy = buddy_arc.lock().await;
                buddy
                    .as_ref()
                    .map(|svc| (svc.settings.autonomy_level, svc.settings.enabled))
            };
            if let Some((autonomy, enabled)) = drain_settings {
                if enabled {
                    match super::storage::drain_memory_ops(
                        &project_root,
                        app.clone(),
                        autonomy,
                        MEMORY_OPS_DRAIN_MAX_APPLIES,
                    )
                    .await
                    {
                        Ok((memory_ops, changed)) => {
                            let mut buddy = buddy_arc.lock().await;
                            if let Some(svc) = buddy.as_mut() {
                                svc.set_memory_ops(memory_ops);
                            }
                            if changed > 0 {
                                info!("buddy: drained {} memory ops", changed);
                            }
                        }
                        Err(err) => warn!("buddy: failed to drain memory ops queue: {}", err),
                    }
                }
            }
        }
    }

    let state_opt = {
        let buddy = buddy_arc.lock().await;
        buddy.as_ref().map(|s| s.state.clone())
    };
    if let Some(s) = state_opt {
        let _ = super::state::save_state(&project_root, &s).await;
    }
    let drafts_opt = {
        let buddy = buddy_arc.lock().await;
        buddy
            .as_ref()
            .filter(|s| s.drafts_dirty)
            .map(|s| s.draft_store.snapshot())
    };
    if let Some(drafts) = drafts_opt {
        let _ = super::storage::save_drafts(&project_root, &drafts).await;
    }

    // Final compaction on shutdown so the JSONL on disk is canonical, then
    // drop the writer sender (replacing the service slot does that for us)
    // and wait for the writer task to drain.
    // Take the whole service out of the slot atomically FIRST so any concurrent add_diagnostic
    // observes None and cannot spawn a new tracked task or enqueue new runtime records. Send the
    // final compaction from the taken service so nothing can mutate Buddy state after it.
    // Dropping the taken service also drops the writer sender so the writer task can finish.
    let mut service = buddy_arc.lock().await.take();
    if let Some(svc) = service.as_ref() {
        if let Some(tx) = &svc.queue_writer {
            let _ = tx.send(RuntimeQueueWriteOp::Compact(svc.runtime_queue.clone()));
        }
    }
    let mut background_tasks = service
        .as_mut()
        .map(|svc| svc.take_background_tasks())
        .unwrap_or_default();
    let drain = async {
        for handle in background_tasks.iter_mut() {
            let _ = handle.await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), drain)
        .await
        .is_err()
    {
        warn!("buddy: background tasks did not drain within 10s, aborting remaining");
        for handle in &background_tasks {
            handle.abort();
        }
    }
    drop(service);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), writer_handle).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), commit_poller_handle).await;

    info!("buddy: background task stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_made_poller_pushes_new_shas() {
        let mut ring =
            crate::buddy::user_activity::UserActivityRing::new(PathBuf::from("/tmp/project"), 200);
        ring.push(UserAction::CommitMade {
            sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            message_first_line: "old".to_string(),
            files: 0,
            ts: Utc::now(),
        });
        let existing = ring
            .snapshot()
            .iter()
            .filter_map(|action| match action {
                UserAction::CommitMade { sha, .. } => Some(sha.clone()),
                _ => None,
            })
            .collect::<HashSet<_>>();

        for action in parse_commit_activity_from_log(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa|old|old\nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb|new|new commit",
        ) {
            if let UserAction::CommitMade { sha, .. } = &action {
                if existing.contains(sha) {
                    continue;
                }
            }
            ring.push(action);
        }

        let actions = ring.snapshot();
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, UserAction::CommitMade { .. }))
                .count(),
            2
        );
        assert!(actions.iter().any(|action| matches!(
            action,
            UserAction::CommitMade { sha, message_first_line, .. }
                if sha == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" && message_first_line == "new commit"
        )));
    }
}
