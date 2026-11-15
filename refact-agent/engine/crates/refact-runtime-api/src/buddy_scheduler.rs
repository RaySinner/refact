//! Buddy job scheduler core.
//!
//! `BuddyJobContext`, `BuddyJobResult`, the `BuddyJob` trait and the
//! `BuddyScheduler` tick loop only need `AppState` plus `refact-buddy-core`
//! types, so they live here; the engine buddy module re-exports them and
//! supplies the concrete job list.

use std::sync::Arc;

use tokio::sync::Mutex as AMutex;

use refact_buddy_core::actor::{make_runtime_event, BuddyService};
use refact_buddy_core::autonomous_workflows::is_autonomous_workflow_id;
use refact_buddy_core::diagnostics::DiagnosticContext;
use refact_buddy_core::settings::BuddySettings;
use refact_buddy_core::speech_policy::{intent_key, pick_speech_intent};
use refact_buddy_core::state::mark_greeted;
use refact_buddy_core::types::{
    BuddyActivity, BuddyBubblePolicy, BuddyChatPhraseBank, BuddyFact, BuddyJobState, BuddyOnboarding,
    BuddyOpportunity, BuddyPersonalityProfile, BuddyPetState, BuddyPulse, BuddyQuest,
    BuddyRuntimeEvent, BuddySpeechItem, BuddySuggestion, BuddyWorkflowSummary,
};
use refact_buddy_core::voice_service::{SpeechIntent, SpeechIntentWireToken};
use refact_buddy_core::workflows::WorkflowFailureReport;

use crate::app_state::AppState;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone)]
pub struct BuddyJobContext {
    pub identity_name: String,
    pub personality: BuddyPersonalityProfile,
    pub onboarding: BuddyOnboarding,
    pub recent_diagnostics: Vec<DiagnosticContext>,
    pub project_root: std::path::PathBuf,
    pub job_state: BuddyJobState,
    pub workflow_summaries: Vec<BuddyWorkflowSummary>,
    pub total_workflow_runs: u64,
    pub suggestion_state: Vec<BuddySuggestion>,
    pub pet: BuddyPetState,
    pub active_quest: Option<BuddyQuest>,
    pub settings: BuddySettings,
    pub pulse: BuddyPulse,
    pub facts: Vec<BuddyFact>,
    pub recent_activities: Vec<BuddyActivity>,
}

pub struct BuddyJobResult {
    pub speech_intent: Option<SpeechIntent>,
    pub speech: Option<BuddySpeechItem>,
    pub suggestion: Option<BuddySuggestion>,
    pub activity: Option<BuddyActivity>,
    pub runtime_event: Option<BuddyRuntimeEvent>,
    pub chat_phrase_bank: Option<BuddyChatPhraseBank>,
    pub workflow_failure: Option<WorkflowFailureReport>,
    pub opportunities: Vec<(BuddyOpportunity, u64)>,
    pub last_result: Option<String>,
    pub xp: u64,
    pub mark_greeted: bool,
}

impl Default for BuddyJobResult {
    fn default() -> Self {
        Self {
            speech_intent: None,
            speech: None,
            suggestion: None,
            activity: None,
            runtime_event: None,
            chat_phrase_bank: None,
            workflow_failure: None,
            opportunities: Vec::new(),
            last_result: None,
            xp: 0,
            mark_greeted: false,
        }
    }
}

impl BuddyJobResult {
    fn has_visible_output(&self) -> bool {
        self.speech.is_some()
            || self.suggestion.is_some()
            || self.activity.is_some()
            || self.runtime_event.is_some()
            || self.chat_phrase_bank.is_some()
            || self.workflow_failure.is_some()
            || !self.opportunities.is_empty()
            || self.xp > 0
    }
}

pub fn next_last_result(existing: Option<&str>, result: Option<&str>) -> Option<String> {
    result.or(existing).map(ToString::to_string)
}

fn should_record_job_result(result: &BuddyJobResult, records_empty_result: bool) -> bool {
    records_empty_result || result.has_visible_output() || result.last_result.is_some()
}

pub fn speech_runtime_event(
    job_id: &str,
    intent: SpeechIntent,
    speech: &BuddySpeechItem,
    title: String,
    description: Option<String>,
) -> BuddyRuntimeEvent {
    let intent_key = intent_key(intent);
    let dedupe_key = speech
        .dedupe_key
        .as_deref()
        .map(|key| format!("speech_runtime:{job_id}:{key}"))
        .unwrap_or_else(|| format!("speech_runtime:{job_id}:{intent_key}"));
    let mut event = make_runtime_event(
        &format!("speech_{intent_key}"),
        if title.trim().is_empty() {
            &speech.text
        } else {
            &title
        },
        job_id,
        &dedupe_key,
        "completed",
        Some(speech_runtime_priority(intent)),
    );
    event.description = description.filter(|text| !text.trim().is_empty());
    event.ttl_ms = Some(speech.ttl_seconds.saturating_mul(1000).max(1000));
    event.speech_text = Some(speech.text.clone());
    event.scene = Some(speech_runtime_scene(intent).to_string());
    event.duration_hint = Some(speech.ttl_seconds.min(u64::from(u32::MAX)) as u32);
    event.persistent = speech.persistent;
    event.controls = speech.controls.clone();
    event.chat_id = speech.chat_id.clone();
    event.bubble_policy = Some(speech_runtime_bubble_policy(intent));
    event
}

fn speech_runtime_bubble_policy(intent: SpeechIntent) -> BuddyBubblePolicy {
    match intent {
        SpeechIntent::Humor
        | SpeechIntent::Insight
        | SpeechIntent::MemoryPulseCommentary
        | SpeechIntent::ChatReaction
        | SpeechIntent::AgentInterjection => BuddyBubblePolicy::Ambient,
        SpeechIntent::Greeting
        | SpeechIntent::Tour
        | SpeechIntent::Milestone
        | SpeechIntent::Win
        | SpeechIntent::QuestAccept
        | SpeechIntent::QuestComplete
        | SpeechIntent::Suggestion
        | SpeechIntent::ErrorAlert => BuddyBubblePolicy::Durable,
    }
}

fn speech_runtime_priority(intent: SpeechIntent) -> &'static str {
    match intent {
        SpeechIntent::ErrorAlert => "high",
        SpeechIntent::Win
        | SpeechIntent::Milestone
        | SpeechIntent::QuestAccept
        | SpeechIntent::QuestComplete => "normal",
        SpeechIntent::Insight | SpeechIntent::MemoryPulseCommentary => "normal",
        SpeechIntent::Greeting | SpeechIntent::Tour | SpeechIntent::Suggestion => "low",
        SpeechIntent::Humor | SpeechIntent::ChatReaction => "low",
        SpeechIntent::AgentInterjection => "normal",
    }
}

fn speech_runtime_scene(intent: SpeechIntent) -> &'static str {
    match intent {
        SpeechIntent::ErrorAlert => "alert",
        SpeechIntent::Win | SpeechIntent::Milestone | SpeechIntent::QuestComplete => "celebrate",
        SpeechIntent::Insight | SpeechIntent::MemoryPulseCommentary | SpeechIntent::Suggestion => {
            "insight"
        }
        SpeechIntent::Greeting | SpeechIntent::Tour | SpeechIntent::QuestAccept => "welcome",
        SpeechIntent::Humor | SpeechIntent::ChatReaction => "playful",
        SpeechIntent::AgentInterjection => "insight",
    }
}

pub fn result_after_suggestion_policy(
    result: BuddyJobResult,
    settings: &BuddySettings,
    suggestion_state: &[BuddySuggestion],
) -> BuddyJobResult {
    if suggestions_allowed(settings, suggestion_state) {
        return result;
    }
    BuddyJobResult {
        suggestion: None,
        ..result
    }
}

#[async_trait::async_trait]
pub trait BuddyJob: Send + Sync {
    fn id(&self) -> &str;
    fn cooldown_seconds(&self) -> u64;
    fn priority(&self) -> u32;
    fn produces_suggestion(&self) -> bool {
        false
    }
    fn runs_when_suggestions_blocked(&self) -> bool {
        false
    }
    fn records_empty_result(&self) -> bool {
        true
    }
    fn is_autonomous(&self) -> bool {
        is_autonomous_workflow_id(self.id())
    }
    async fn should_run(&self, gcx: AppState, ctx: &BuddyJobContext) -> bool;
    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult;
}

pub const MAX_UNREAD_SUGGESTIONS: usize = 3;
pub const MAX_AUTONOMOUS_JOBS_PER_TICK: usize = 2;

pub fn suggestions_allowed(
    settings: &BuddySettings,
    suggestion_state: &[BuddySuggestion],
) -> bool {
    let unread = suggestion_state
        .iter()
        .filter(|suggestion| !suggestion.dismissed)
        .count();
    settings.proactive_enabled && unread < MAX_UNREAD_SUGGESTIONS
}

pub struct BuddyScheduler {
    pub jobs: Vec<Box<dyn BuddyJob>>,
}

impl BuddyScheduler {
    pub fn new(mut jobs: Vec<Box<dyn BuddyJob>>) -> Self {
        jobs.sort_by_key(|j| j.priority());
        Self { jobs }
    }

    pub async fn tick(
        &self,
        gcx: AppState,
        buddy_arc: Arc<AMutex<Option<BuddyService>>>,
        project_root: &std::path::Path,
    ) {
        let ctx_opt = {
            let buddy = buddy_arc.lock().await;
            buddy.as_ref().map(|svc| {
                (
                    svc.state.clone(),
                    svc.recent_diagnostics.clone(),
                    svc.settings.clone(),
                    svc.pulse.clone(),
                    svc.fact_store.iter().cloned().collect::<Vec<_>>(),
                )
            })
        };
        let (state, diags, settings, pulse, facts) = match ctx_opt {
            Some(x) => x,
            None => return,
        };
        let mut ready_results: Vec<(&str, BuddyJobResult, bool)> = vec![];
        let mut autonomous_jobs_run = 0usize;
        for job in &self.jobs {
            let is_autonomous = job.is_autonomous();
            if is_autonomous && autonomous_jobs_run >= MAX_AUTONOMOUS_JOBS_PER_TICK {
                continue;
            }
            if job.produces_suggestion()
                && !job.runs_when_suggestions_blocked()
                && !suggestions_allowed(&settings, &state.suggestion_state)
            {
                continue;
            }
            let job_state = state
                .job_cooldowns
                .get(job.id())
                .cloned()
                .unwrap_or_default();
            if job_state.dismissed {
                continue;
            }
            let elapsed = job_state
                .last_run
                .as_deref()
                .and_then(|r| chrono::DateTime::parse_from_rfc3339(r).ok())
                .map(|t| {
                    chrono::Utc::now()
                        .signed_duration_since(t)
                        .num_seconds()
                        .max(0) as u64
                })
                .unwrap_or(u64::MAX);
            if elapsed < job.cooldown_seconds() {
                continue;
            }
            let total_workflow_runs = state.workflow_summaries.iter().map(|w| w.run_count).sum();
            let ctx = BuddyJobContext {
                identity_name: state.identity.name.clone(),
                personality: state.personality.clone(),
                onboarding: state.onboarding.clone(),
                recent_diagnostics: diags.clone(),
                project_root: project_root.to_path_buf(),
                job_state: job_state.clone(),
                workflow_summaries: state.workflow_summaries.clone(),
                total_workflow_runs,
                suggestion_state: state.suggestion_state.clone(),
                pet: state.pet.clone(),
                active_quest: state.active_quest.clone(),
                settings: settings.clone(),
                pulse: pulse.clone(),
                facts: facts.clone(),
                recent_activities: state.recent_activities.clone(),
            };
            if !job.should_run(gcx.clone(), &ctx).await {
                continue;
            }
            if is_autonomous {
                autonomous_jobs_run += 1;
            }
            let result = job.execute(gcx.clone(), ctx).await;
            let result = if job.produces_suggestion() {
                result_after_suggestion_policy(result, &settings, &state.suggestion_state)
            } else {
                result
            };
            let records_empty_result = job.records_empty_result();
            ready_results.push((job.id(), result, records_empty_result));
        }

        let now = chrono::Utc::now();
        let winner = {
            let buddy = buddy_arc.lock().await;
            let Some(svc) = buddy.as_ref() else {
                return;
            };
            let speech_result_indexes = ready_results
                .iter()
                .enumerate()
                .filter_map(|(idx, (_, result, _))| {
                    result
                        .speech_intent
                        .zip(result.speech.as_ref())
                        .map(|_| idx)
                })
                .collect::<Vec<_>>();
            let candidates = speech_result_indexes
                .iter()
                .filter_map(|idx| {
                    let (_, result, _) = &ready_results[*idx];
                    result.speech_intent.zip(result.speech.clone())
                })
                .collect::<Vec<_>>();
            let candidate_winner =
                pick_speech_intent(&candidates, &svc.state.speech_rotation, now);
            candidate_winner.and_then(|winner_idx| speech_result_indexes.get(winner_idx).copied())
        };

        for (idx, (job_id, mut result, records_empty_result)) in
            ready_results.into_iter().enumerate()
        {
            if result.speech.is_some() && result.speech_intent.is_some() && Some(idx) != winner {
                result.speech = None;
                result.speech_intent = None;
            }
            if should_record_job_result(&result, records_empty_result) {
                let mut workflow_failure_write = None;
                let mut buddy = buddy_arc.lock().await;
                if let Some(svc) = buddy.as_mut() {
                    let mut js = svc
                        .state
                        .job_cooldowns
                        .entry(job_id.to_string())
                        .or_default()
                        .clone();
                    js.last_run = Some(chrono::Utc::now().to_rfc3339());
                    js.run_count += 1;
                    js.last_result =
                        next_last_result(js.last_result.as_deref(), result.last_result.as_deref());
                    svc.state.job_cooldowns.insert(job_id.to_string(), js);
                    svc.dirty = true;
                    if let Some(suggestion) = result.suggestion {
                        svc.maybe_add_suggestion(suggestion);
                    }
                    if let Some(activity) = result.activity {
                        svc.add_activity(activity);
                    }
                    if let Some(mut speech) = result.speech {
                        if let Some(intent) = result.speech_intent {
                            speech.speech_intent = Some(intent.wire_token().to_string());
                        }
                        let displayed = svc.update_speech(speech);
                        if displayed && result.mark_greeted && mark_greeted(&mut svc.state) {
                            svc.dirty = true;
                        }
                        svc.dirty = true;
                    }
                    if let Some(event) = result.runtime_event {
                        svc.enqueue_runtime_event(event);
                    }
                    if let Some(phrase_bank) = result.chat_phrase_bank {
                        svc.state.chat_phrase_bank = Some(phrase_bank);
                        svc.dirty = true;
                    }
                    if let Some(report) = result.workflow_failure {
                        workflow_failure_write = svc.record_workflow_failure_report(report);
                    }
                    for (opportunity, cooldown_secs) in result.opportunities {
                        svc.surface_opportunity_with_cooldown(opportunity, cooldown_secs);
                    }
                    if result.xp > 0 {
                        svc.grant_xp(result.xp);
                    }
                }
                drop(buddy);
                if let Some((path, report)) = workflow_failure_write {
                    BuddyService::append_workflow_failure_transcript(&path, &report).await;
                }
            }
        }
    }
}
