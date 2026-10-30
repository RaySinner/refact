use std::collections::{HashMap, HashSet};
use std::time::Instant;

use chrono::{DateTime, Utc};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::warn;
use uuid::Uuid;

use crate::drafts::{
    validate_draft_payload, DraftCreateError, DraftStore, DraftTarget, DraftValidationError,
};
use crate::events::DiagnosticBuddyEvent as BuddyEvent;
use crate::facts::FactStore;
use crate::memory_lifecycle_model::{memory_lifecycle_op_counts, MemoryOpsState};
#[cfg(feature = "test-util")]
use crate::opportunities::OpportunityDetector;
use crate::opportunities::OpportunityQueue;
use crate::policy::{evaluate_with_mutes, PolicyDecision};
use crate::runtime_queue::RuntimeQueue;
use crate::runtime_writer::RuntimeQueueWriteOp;
use crate::settings::BuddySettings;
use crate::snapshot::BuddySnapshot;
use crate::storage::RuntimeQueueRecord;
use crate::types::{
    BuddyActivity, BuddyChatPhraseBank, BuddyDraft, BuddyOpportunity, BuddyPulse, BuddyQuest,
    BuddyRuntimeEvent, BuddySpeechItem, BuddyState, BuddySuggestion, OpportunityStatus,
};
use crate::voice_service::{SpeechIntent, SpeechIntentWireToken};

const SUGGESTION_RATE_LIMIT_SECS: u64 = 300;
const SUGGESTION_EXPIRY_SECS: i64 = 300;
const QUEST_SUGGESTION_EXPIRY_SECS: i64 = 24 * 3600;

pub fn chat_phrase_bank_is_fresh(bank: &BuddyChatPhraseBank, now: DateTime<Utc>) -> bool {
    bank.day == now.date_naive().to_string()
}

pub fn validate_workflow_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Redact common credential shapes. Uses regex with case-insensitive matching
/// so **all** occurrences are scrubbed regardless of capitalization. Mirrors
/// the secret patterns used by the GUI's `reportBuddyFrontendError.ts` so the
/// backend can't leak something the frontend would have masked.
pub fn redact_sensitive(text: &str) -> String {
    refact_core::string_utils::redact_sensitive(text)
}

pub fn redact_diagnostic_metadata(value: &str) -> Option<String> {
    let redacted = redact_sensitive(value).trim().to_string();
    if redacted.is_empty() {
        None
    } else {
        Some(redacted)
    }
}

pub struct BuddyService {
    pub state: BuddyState,
    pub settings: BuddySettings,
    pub events_tx: broadcast::Sender<BuddyEvent>,
    pub project_root: std::path::PathBuf,
    pub last_suggestion_at: Option<Instant>,
    pub recent_diagnostics: Vec<crate::diagnostics::DiagnosticContext>,
    pub memory_ops: MemoryOpsState,
    pub last_issue_at: Option<Instant>,
    pub recent_issue_errors: Vec<(String, DateTime<Utc>)>,
    pub runtime_queue: RuntimeQueue,
    pub dismissed_runtime_keys: HashMap<String, DateTime<Utc>>,
    pub dirty: bool,
    pub active_speech: Option<BuddySpeechItem>,
    pub queue_writer: Option<mpsc::UnboundedSender<RuntimeQueueWriteOp>>,
    pub fact_store: FactStore,
    pub opportunity_queue: OpportunityQueue,
    pub opportunity_accept_claims: HashSet<String>,
    pub pulse: BuddyPulse,
    pub draft_store: DraftStore,
    pub drafts_dirty: bool,
    pub last_observer_tick: HashMap<&'static str, DateTime<Utc>>,
    pub background_tasks: Vec<JoinHandle<()>>,
    pub chat_reaction_limiter: crate::chat_reactions::ChatReactionLimiter,
    pub chat_reaction_debug: crate::chat_reactions::ChatReactionDebugState,
    pub auto_quiet_window: Option<(u8, u8)>,
    #[cfg(feature = "test-util")]
    pub force_next_runtime_enqueue_drop: bool,
}

impl BuddyService {
    pub fn new(
        project_root: std::path::PathBuf,
        mut state: BuddyState,
        settings: BuddySettings,
        recent_diagnostics: Vec<crate::diagnostics::DiagnosticContext>,
        runtime_queue: RuntimeQueue,
        events_tx: broadcast::Sender<BuddyEvent>,
        queue_writer: Option<mpsc::UnboundedSender<RuntimeQueueWriteOp>>,
    ) -> Self {
        let opportunity_queue = OpportunityQueue::from_state(
            state.opportunities.clone(),
            state.dismissed_history.clone(),
        );
        let dismissed_runtime_keys = runtime_queue
            .items
            .iter()
            .chain(runtime_queue.now_playing.iter())
            .filter(|event| event.dismissed)
            .filter_map(|event| event.dedupe_key.clone().map(|key| (key, Utc::now())))
            .collect();
        let opportunity_snapshot = opportunity_queue.snapshot();
        let dismissed_snapshot = opportunity_queue.dismissed_history_snapshot();
        let state_changed = state.opportunities.len() != opportunity_snapshot.len()
            || state.dismissed_history.len() != dismissed_snapshot.len();
        state.opportunities = opportunity_snapshot;
        state.dismissed_history = dismissed_snapshot;
        Self {
            project_root,
            state,
            settings,
            events_tx,
            last_suggestion_at: None,
            recent_diagnostics,
            memory_ops: MemoryOpsState::default(),
            last_issue_at: None,
            recent_issue_errors: Vec::new(),
            runtime_queue,
            dismissed_runtime_keys,
            dirty: state_changed,
            active_speech: None,
            queue_writer,
            fact_store: FactStore::new(),
            opportunity_queue,
            opportunity_accept_claims: HashSet::new(),
            pulse: BuddyPulse::default(),
            draft_store: DraftStore::new(),
            drafts_dirty: false,
            last_observer_tick: HashMap::new(),
            background_tasks: Vec::new(),
            chat_reaction_limiter: crate::chat_reactions::ChatReactionLimiter::new(),
            chat_reaction_debug: crate::chat_reactions::ChatReactionDebugState::new(),
            auto_quiet_window: None,
            #[cfg(feature = "test-util")]
            force_next_runtime_enqueue_drop: false,
        }
    }

    /// Push a record into the writer queue. The writer task applies them in
    /// strict order — see [`run_runtime_queue_writer`].
    fn persist_record(&self, record: RuntimeQueueRecord) {
        if let Some(tx) = &self.queue_writer {
            // send only fails if the receiver was dropped (shutdown). In that
            // case the data is no longer needed; nothing to do.
            let _ = tx.send(RuntimeQueueWriteOp::Append(record));
        }
    }

    fn persist_event(&self, event: BuddyRuntimeEvent) {
        self.persist_record(RuntimeQueueRecord::Event { event });
    }

    fn persist_removal(&self, id: String) {
        self.persist_record(RuntimeQueueRecord::Removed { id });
    }

    fn persist_now_playing(&self, slot: Option<BuddyRuntimeEvent>) {
        self.persist_record(RuntimeQueueRecord::NowPlaying { event: slot });
    }

    pub fn track_background_task(&mut self, handle: JoinHandle<()>) {
        self.background_tasks.retain(|task| !task.is_finished());
        self.background_tasks.push(handle);
    }

    pub fn take_background_tasks(&mut self) -> Vec<JoinHandle<()>> {
        std::mem::take(&mut self.background_tasks)
    }

    pub fn snapshot(&self) -> BuddySnapshot {
        let opportunities = self.opportunity_queue.snapshot();
        let mut state = self.state.clone();
        state.opportunities = opportunities.clone();
        let mut pulse = self.pulse.clone();
        self.apply_memory_ops_to_pulse(&mut pulse);
        BuddySnapshot {
            state,
            settings: self.settings.clone(),
            enabled: self.settings.enabled,
            storage: Some(crate::settings::storage_metadata(&self.project_root)),
            recent_diagnostics: self.recent_diagnostics.clone(),
            runtime_queue: self.runtime_queue.items.iter().cloned().collect(),
            now_playing: self.runtime_queue.now_playing.clone(),
            active_speech: self.active_speech.clone(),
            pulse,
            opportunities,
            active_drafts: self.draft_store.snapshot(),
            chat_reaction_debug: Some(self.chat_reaction_debug.snapshot()),
        }
    }

    pub fn expire_opportunities(&mut self) {
        let now = Utc::now();
        let expiring: Vec<String> = self
            .opportunity_queue
            .iter()
            .filter(|o| {
                o.expires_at <= now
                    && matches!(o.status, OpportunityStatus::New | OpportunityStatus::Shown)
            })
            .map(|o| o.id.clone())
            .collect();
        let changed = self.opportunity_queue.expire_old(now);
        if !changed {
            return;
        }
        for id in expiring {
            let _ = self.events_tx.send(BuddyEvent::OpportunityResolved {
                opportunity_id: id,
                status: OpportunityStatus::Expired,
            });
        }
        self.state.opportunities = self.opportunity_queue.snapshot();
        self.state.dismissed_history = self.opportunity_queue.dismissed_history_snapshot();
        self.dirty = true;
    }

    #[cfg(feature = "test-util")]
    pub fn add_opportunity(&mut self, opp: BuddyOpportunity) {
        self.add_opportunity_with_cooldown(
            opp,
            crate::opportunities::DEFAULT_COOLDOWN.num_seconds() as u64,
        );
    }

    pub fn add_opportunity_with_cooldown(&mut self, opp: BuddyOpportunity, cooldown_secs: u64) {
        self.opportunity_queue
            .push_with_cooldown(opp.clone(), cooldown_secs);
        self.state.opportunities = self.opportunity_queue.snapshot();
        self.state.dismissed_history = self.opportunity_queue.dismissed_history_snapshot();
        self.dirty = true;
        let _ = self
            .events_tx
            .send(BuddyEvent::OpportunityProduced { opportunity: opp });
    }

    pub fn surface_opportunity_with_cooldown(
        &mut self,
        mut opp: BuddyOpportunity,
        cooldown_secs: u64,
    ) -> bool {
        match evaluate_with_mutes(
            &opp,
            &self.settings,
            &self.opportunity_queue,
            &self.state.muted_rules,
        ) {
            PolicyDecision::Drop { reason } => {
                tracing::debug!("buddy: opportunity dropped by policy: {}", reason);
                false
            }
            PolicyDecision::Surface { humor_allowed } => {
                opp.humor_allowed = humor_allowed;
                let rule_key = crate::verdicts::rule_key_of(&opp.cooldown_key);
                let multiplier = crate::verdicts::cooldown_multiplier_for_rule(
                    &self.state.verdicts,
                    &rule_key,
                );
                self.add_opportunity_with_cooldown(opp, cooldown_secs.saturating_mul(multiplier));
                true
            }
        }
    }

    pub fn record_verdict(
        &mut self,
        cooldown_key: &str,
        kind: crate::types::BuddyOpportunityKind,
        action_kind: &str,
        outcome: crate::verdicts::VerdictOutcome,
    ) {
        let verdict = crate::verdicts::BuddyVerdict {
            rule_key: crate::verdicts::rule_key_of(cooldown_key),
            kind: serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            action_kind: action_kind.to_string(),
            verdict: outcome,
            at: Utc::now(),
        };
        crate::verdicts::record_verdict(&mut self.state.verdicts, verdict);
        self.dirty = true;
    }

    pub fn mute_rule_for_cooldown_key(&mut self, cooldown_key: &str) -> bool {
        let changed =
            crate::verdicts::mute_rule(&mut self.state.muted_rules, cooldown_key);
        if changed {
            self.dirty = true;
        }
        changed
    }

    pub fn unmute_rule(&mut self, rule_key: &str) -> bool {
        let changed =
            crate::verdicts::unmute_rule(&mut self.state.muted_rules, rule_key);
        if changed {
            self.dirty = true;
        }
        changed
    }

    pub fn set_memory_ops(&mut self, memory_ops: MemoryOpsState) {
        self.memory_ops = memory_ops;
        self.surface_memory_batch_opportunities();
    }

    fn surface_memory_batch_opportunities(&mut self) {
        let batches =
            crate::memory_lifecycle_model::memory_op_batches(&self.memory_ops.ops);
        let now = Utc::now();
        for batch in batches {
            if batch.count == 0 {
                continue;
            }
            let label = crate::memory_lifecycle_model::memory_op_batch_label(
                &batch.batch_key,
                batch.count,
            );
            let summary = if batch.preview.is_empty() {
                label
            } else {
                format!("{} — e.g. {}", label, batch.preview.join("; "))
            };
            let opp = BuddyOpportunity {
                id: Uuid::new_v4().to_string(),
                kind: crate::types::BuddyOpportunityKind::MemoryOpsBatch,
                summary,
                priority: crate::types::BuddyPriority::Normal,
                confidence: 0.9,
                fact_keys: vec![],
                cooldown_key: format!("memory_ops_batch:{}", batch.batch_key),
                cooldown_secs: 24 * 60 * 60,
                status: OpportunityStatus::New,
                proposed_actions: vec![
                    crate::types::BuddyAction::ApplyMemoryBatch {
                        batch_key: batch.batch_key.clone(),
                        count_hint: batch.count,
                    },
                    crate::types::BuddyAction::Dismiss,
                ],
                humor: None,
                humor_allowed: false,
                related: crate::types::BuddyOpportunityLinks::default(),
                created_at: now,
                expires_at: now + chrono::Duration::hours(24),
                resolved_at: None,
            };
            self.surface_opportunity_with_cooldown(opp, 24 * 60 * 60);
        }
    }

    pub fn accept_quest_from_suggestion(
        &mut self,
        suggestion_id: &str,
    ) -> Result<(BuddySuggestion, BuddyQuest), String> {
        let suggestion = self
            .state
            .suggestion_state
            .iter()
            .find(|suggestion| suggestion.id == suggestion_id)
            .cloned()
            .ok_or_else(|| format!("suggestion not found: {}", suggestion_id))?;
        let quest = suggestion
            .quest
            .clone()
            .ok_or_else(|| format!("suggestion is not a quest: {}", suggestion_id))?;
        self.dismiss_suggestion(suggestion_id);
        crate::state::activate_quest(&mut self.state, quest.clone());
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::StateUpdated {
            state: self.state.clone(),
        });
        Ok((suggestion, quest))
    }

    pub fn claim_opportunity_accept(&mut self, id: &str) -> bool {
        self.opportunity_accept_claims.insert(id.to_string())
    }

    pub fn clear_opportunity_accept_claim(&mut self, id: &str) {
        self.opportunity_accept_claims.remove(id);
    }

    pub fn is_opportunity_accept_claimed(&self, id: &str) -> bool {
        self.opportunity_accept_claims.contains(id)
    }

    pub fn resolve_opportunity(&mut self, id: &str, status: OpportunityStatus) -> bool {
        let changed = if matches!(status, OpportunityStatus::Dismissed) {
            self.opportunity_queue.dismiss(id)
        } else {
            self.opportunity_queue.mark_status(id, status)
        };
        if !changed {
            return false;
        }
        self.state.opportunities = self.opportunity_queue.snapshot();
        self.state.dismissed_history = self.opportunity_queue.dismissed_history_snapshot();
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::OpportunityResolved {
            opportunity_id: id.to_string(),
            status,
        });
        true
    }

    pub fn set_pulse(&mut self, pulse: BuddyPulse) {
        let mut pulse = pulse;
        self.apply_memory_ops_to_pulse(&mut pulse);
        self.pulse = pulse.clone();
        let _ = self.events_tx.send(BuddyEvent::PulseUpdated { pulse });
    }

    fn apply_memory_ops_to_pulse(&self, pulse: &mut BuddyPulse) {
        pulse.memory.pending_ops = self.memory_ops.pending_count + self.memory_ops.approved_count;
        pulse.memory.applied_ops = self.memory_ops.applied_count;
        pulse.memory.failed_ops = self.memory_ops.failed_count;
        let counts = memory_lifecycle_op_counts(&self.memory_ops.ops);
        pulse.memory.duplicate_candidates = counts.duplicate_candidates;
        pulse.memory.merge_candidates = counts.merge_candidates;
        pulse.memory.archive_candidates = counts.archive_candidates;
        pulse.memory.review_candidates = counts.review_candidates;
        pulse.memory.conflict_candidates = counts.conflict_candidates;
    }

    pub fn create_draft(
        &mut self,
        kind: crate::types::DraftKind,
        title: String,
        yaml_or_json: String,
        explanation: String,
    ) -> Result<BuddyDraft, DraftCreateError> {
        validate_draft_payload(&title, &yaml_or_json, &explanation)?;
        let draft = self
            .draft_store
            .create(kind, title, yaml_or_json, explanation);
        self.drafts_dirty = true;
        let _ = self.events_tx.send(BuddyEvent::DraftCreated {
            draft: draft.clone(),
        });
        Ok(draft)
    }

    pub fn delete_draft(&mut self, id: &str) -> Option<BuddyDraft> {
        let draft = self.draft_store.delete(id)?;
        self.drafts_dirty = true;
        let _ = self.events_tx.send(BuddyEvent::DraftRemoved {
            draft_id: id.to_string(),
        });
        Some(draft)
    }

    pub fn consume_draft(&mut self, id: &str) -> Option<BuddyDraft> {
        let draft = self.draft_store.consume(id)?;
        self.drafts_dirty = true;
        let _ = self.events_tx.send(BuddyEvent::DraftConsumed {
            draft_id: id.to_string(),
        });
        Some(draft)
    }

    pub fn expire_drafts(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let expired = self.draft_store.expire_old(now);
        if !expired.is_empty() {
            self.drafts_dirty = true;
        }
        for id in &expired {
            let _ = self.events_tx.send(BuddyEvent::DraftRemoved {
                draft_id: id.clone(),
            });
        }
        expired
    }

    pub fn consume_validated_draft(
        &mut self,
        id: &str,
        expected_kind: crate::types::DraftKind,
        target: DraftTarget<'_>,
    ) -> Result<BuddyDraft, DraftValidationError> {
        self.draft_store.get_validated(id, expected_kind, target)?;
        self.consume_draft(id).ok_or(DraftValidationError::NotFound)
    }

    #[cfg(feature = "test-util")]
    pub fn detect_and_surface(&mut self) {
        let candidates = OpportunityDetector::new().detect(
            &self.fact_store,
            &self.pulse,
            &self.opportunity_queue,
        );
        for (opp, cooldown_secs) in candidates {
            self.surface_opportunity_with_cooldown(opp, cooldown_secs);
        }
    }

    pub fn update_speech(&mut self, speech: BuddySpeechItem) -> bool {
        self.update_speech_gated(speech, false)
    }

    pub fn update_speech_user_initiated(&mut self, speech: BuddySpeechItem) -> bool {
        self.update_speech_gated(speech, true)
    }

    fn update_speech_gated(&mut self, speech: BuddySpeechItem, user_initiated: bool) -> bool {
        let intent = speech
            .speech_intent
            .as_deref()
            .and_then(crate::speech_policy::parse_intent_key);
        let now = Utc::now();
        let local_hour = chrono::Timelike::hour(&chrono::Local::now());
        let decision = if user_initiated {
            crate::speech_policy::gate_speech_user_initiated(
                &self.settings,
                intent,
                speech.chat_id.as_deref(),
            )
        } else {
            crate::speech_policy::gate_speech(
                &self.settings,
                &self.state.speech_rotation,
                intent,
                speech.chat_id.as_deref(),
                local_hour,
                self.auto_quiet_window,
                now,
            )
        };
        self.record_speech_decision(&speech, intent, decision);
        if !decision.allowed {
            tracing::debug!("buddy: speech dropped by arbiter: {}", decision.reason);
            return false;
        }
        if let Some(intent) = intent {
            crate::speech_policy::record_emission(&mut self.state.speech_rotation, intent, now);
            self.dirty = true;
        }
        if let Some(key) = &speech.dedupe_key {
            if let Some(existing) = &self.active_speech {
                if existing.dedupe_key.as_deref() == Some(key.as_str()) {
                    self.active_speech = Some(speech.clone());
                    let _ = self.events_tx.send(BuddyEvent::SpeechUpdated { speech });
                    return true;
                }
            }
        }
        self.active_speech = Some(speech.clone());
        let _ = self.events_tx.send(BuddyEvent::SpeechUpdated { speech });
        true
    }

    fn record_speech_decision(
        &mut self,
        speech: &BuddySpeechItem,
        intent: Option<SpeechIntent>,
        decision: crate::speech_policy::SpeechGateDecision,
    ) {
        let record = crate::types::SpeechDecisionRecord {
            at: Utc::now(),
            intent: intent.map(|i| crate::speech_policy::intent_key(i).to_string()),
            allowed: decision.allowed,
            reason: decision.reason.to_string(),
            preview: refact_core::string_utils::safe_truncate(&speech.text, 120).to_string(),
            source: speech
                .dedupe_key
                .clone()
                .unwrap_or_else(|| speech.id.clone()),
        };
        self.state.speech_decisions.push(record);
        let overflow = self.state.speech_decisions.len().saturating_sub(50);
        if overflow > 0 {
            self.state.speech_decisions.drain(..overflow);
        }
        self.dirty = true;
    }

    pub fn send_navigation(&self, page: crate::types::BuddyPage) {
        let _ = self.events_tx.send(BuddyEvent::NavigationRequest { page });
    }

    pub fn enqueue_runtime_event(&mut self, event: BuddyRuntimeEvent) {
        let _ = self.enqueue_runtime_event_with_stored(event);
    }

    fn runtime_event_intent(signal_type: &str) -> Option<SpeechIntent> {
        if let Some(token) = signal_type.strip_prefix("speech_") {
            return crate::speech_policy::parse_intent_key(token);
        }
        if signal_type == "chat_bug_candidate" {
            return Some(SpeechIntent::ErrorAlert);
        }
        None
    }

    fn gate_runtime_event_speech(&mut self, mut event: BuddyRuntimeEvent) -> BuddyRuntimeEvent {
        let Some(speech_text) = event.speech_text.clone() else {
            return event;
        };
        let intent = Self::runtime_event_intent(&event.signal_type);
        let local_hour = chrono::Timelike::hour(&chrono::Local::now());
        let decision = crate::speech_policy::gate_speech(
            &self.settings,
            &self.state.speech_rotation,
            intent,
            event.chat_id.as_deref(),
            local_hour,
            self.auto_quiet_window,
            Utc::now(),
        );
        if !decision.allowed && decision.reason != "intent_budget" {
            let preview_item = BuddySpeechItem {
                id: event.id.clone(),
                text: speech_text,
                mood: "neutral".to_string(),
                scope: "global".to_string(),
                persistent: false,
                ttl_seconds: 10,
                dedupe_key: event.dedupe_key.clone(),
                speech_intent: intent.map(|i| crate::speech_policy::intent_key(i).to_string()),
                created_at: event.created_at.clone(),
                controls: vec![],
                chat_id: event.chat_id.clone(),
            };
            self.record_speech_decision(&preview_item, intent, decision);
            event.speech_text = None;
        }
        event
    }

    pub fn gate_chat_reaction_event(&mut self, event: &BuddyRuntimeEvent) -> bool {
        let Some(speech_text) = event.speech_text.clone() else {
            return true;
        };
        let intent = Self::runtime_event_intent(&event.signal_type);
        let local_hour = chrono::Timelike::hour(&chrono::Local::now());
        let now = Utc::now();
        let decision = crate::speech_policy::gate_speech(
            &self.settings,
            &self.state.speech_rotation,
            intent,
            event.chat_id.as_deref(),
            local_hour,
            self.auto_quiet_window,
            now,
        );
        let preview_item = BuddySpeechItem {
            id: event.id.clone(),
            text: speech_text,
            mood: "neutral".to_string(),
            scope: "global".to_string(),
            persistent: false,
            ttl_seconds: 10,
            dedupe_key: event.dedupe_key.clone(),
            speech_intent: intent.map(|i| crate::speech_policy::intent_key(i).to_string()),
            created_at: event.created_at.clone(),
            controls: vec![],
            chat_id: event.chat_id.clone(),
        };
        self.record_speech_decision(&preview_item, intent, decision);
        if !decision.allowed {
            tracing::debug!(
                "buddy: chat reaction speech dropped by arbiter: {}",
                decision.reason
            );
            return false;
        }
        if let Some(intent) = intent {
            crate::speech_policy::record_emission(&mut self.state.speech_rotation, intent, now);
            self.dirty = true;
        }
        true
    }

    pub fn enqueue_runtime_event_with_stored(
        &mut self,
        event: BuddyRuntimeEvent,
    ) -> Option<BuddyRuntimeEvent> {
        #[cfg(feature = "test-util")]
        if self.force_next_runtime_enqueue_drop {
            self.force_next_runtime_enqueue_drop = false;
            return None;
        }
        let event = self.gate_runtime_event_speech(event);
        let event = self.apply_runtime_dismissal_memory(event);
        let dedupe_key = event.dedupe_key.clone();
        let input_id = event.id.clone();
        let evicted = self.runtime_queue.enqueue(event);
        // Broadcast and persist the coalesced/stored event so SSE consumers and
        // dismiss_runtime_event_by_id agree on the id after dedupe.
        let to_persist = if let Some(key) = dedupe_key.as_deref() {
            self.runtime_queue
                .items
                .iter()
                .find(|e| e.dedupe_key.as_deref() == Some(key))
                .cloned()
                .or_else(|| {
                    self.runtime_queue
                        .now_playing
                        .as_ref()
                        .filter(|e| e.dedupe_key.as_deref() == Some(key))
                        .cloned()
                })
        } else {
            self.runtime_queue
                .items
                .iter()
                .find(|e| e.id == input_id)
                .cloned()
        };
        if let Some(ev) = to_persist.as_ref() {
            let _ = self
                .events_tx
                .send(BuddyEvent::RuntimeEvent { event: ev.clone() });
            self.persist_event(ev.clone());
        }
        if dedupe_key.is_some()
            && self
                .runtime_queue
                .now_playing
                .as_ref()
                .map(|np| np.dedupe_key == dedupe_key)
                .unwrap_or(false)
        {
            self.persist_now_playing(self.runtime_queue.now_playing.clone());
        }
        // Tombstone every evicted id so replay matches in-memory state.
        for id in evicted {
            self.persist_removal(id);
        }
        to_persist
    }

    pub fn expire_runtime_events_at(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let removed = self.runtime_queue.prune_expired_at(now);
        for id in &removed {
            self.persist_removal(id.clone());
        }
        removed
    }

    fn apply_runtime_dismissal_memory(
        &mut self,
        mut event: BuddyRuntimeEvent,
    ) -> BuddyRuntimeEvent {
        let now = Utc::now();
        let cutoff = now - chrono::Duration::hours(24);
        self.dismissed_runtime_keys
            .retain(|_, dismissed_at| *dismissed_at >= cutoff);
        if let Some(key) = event.dedupe_key.as_deref() {
            if self
                .dismissed_runtime_keys
                .get(key)
                .map(|dismissed_at| *dismissed_at >= cutoff)
                .unwrap_or(false)
            {
                event.dismissed = true;
            }
        }
        event
    }

    pub fn complete_runtime_event(&mut self, dedupe_key: &str, status: &str) {
        self.runtime_queue.complete(dedupe_key, status);
        if let Some(e) = self
            .runtime_queue
            .items
            .iter()
            .find(|e| e.dedupe_key.as_deref() == Some(dedupe_key))
            .cloned()
        {
            let _ = self
                .events_tx
                .send(BuddyEvent::RuntimeEvent { event: e.clone() });
            self.persist_event(e);
        }
        if self
            .runtime_queue
            .now_playing
            .as_ref()
            .and_then(|np| np.dedupe_key.as_deref())
            == Some(dedupe_key)
        {
            self.persist_now_playing(self.runtime_queue.now_playing.clone());
        }
    }

    /// Mark a runtime event as dismissed by its `id` (frontend-visible identifier).
    /// The event stays in the queue with `dismissed: true` so the dismissal
    /// persists across snapshot reloads. Emits a RuntimeEvent so all clients
    /// see the updated flag immediately.
    /// Returns true if a matching event was found and updated.
    pub fn dismiss_runtime_event_by_id(&mut self, id: &str) -> bool {
        let mut found = false;
        let mut updated_event: Option<BuddyRuntimeEvent> = None;
        if let Some(e) = self.runtime_queue.items.iter_mut().find(|e| e.id == id) {
            e.dismissed = true;
            updated_event = Some(e.clone());
            found = true;
        }
        if let Some(ref mut np) = self.runtime_queue.now_playing {
            if np.id == id {
                np.dismissed = true;
                updated_event = Some(np.clone());
                found = true;
            }
        }
        if let Some(event) = updated_event {
            if let Some(key) = event.dedupe_key.as_ref() {
                self.dismissed_runtime_keys.insert(key.clone(), Utc::now());
            }
            self.dirty = true;
            let _ = self.events_tx.send(BuddyEvent::RuntimeEvent {
                event: event.clone(),
            });
            self.persist_event(event);
            // The dismiss path may also have flipped the `dismissed` flag on
            // now_playing; record the slot's current state so replay sees it.
            if self
                .runtime_queue
                .now_playing
                .as_ref()
                .map(|np| np.id == id)
                .unwrap_or(false)
            {
                self.persist_now_playing(self.runtime_queue.now_playing.clone());
            }
        }
        found
    }

    pub fn add_activity(&mut self, activity: BuddyActivity) {
        crate::state::add_activity(&mut self.state, activity.clone());
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::ActivityAdded { activity });
    }

    pub fn refresh_active_quest(&mut self) {
        let progressed = crate::state::refresh_active_quest_progress(&mut self.state);
        let completed = self
            .state
            .active_quest
            .as_ref()
            .map(|quest| quest.status == "active" && quest.progress >= quest.goal)
            .unwrap_or(false);

        if !completed {
            if progressed {
                self.dirty = true;
                let _ = self.events_tx.send(BuddyEvent::StateUpdated {
                    state: self.state.clone(),
                });
            }
            return;
        }

        let Some(quest) = crate::state::complete_active_quest(&mut self.state) else {
            return;
        };

        let reward = quest.reward_xp;
        let title = quest.title.clone();
        let icon = quest.icon.clone();
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::StateUpdated {
            state: self.state.clone(),
        });

        self.add_activity(BuddyActivity {
            icon,
            title: format!("Quest complete: {title}"),
            description: format!(
                "{} wrapped up '{title}' and earned a growth boost.",
                self.state.identity.name
            ),
            timestamp: Utc::now().to_rfc3339(),
            activity_type: "quest_completed".to_string(),
            chat_id: None,
            failure_category: None,
            failure_summary: None,
        });
        self.update_speech_user_initiated(BuddySpeechItem {
            id: format!("quest-complete-{}", quest.id),
            text: format!("Quest complete: {title}! Tiny victory dance?"),
            mood: "happy".to_string(),
            scope: "global".to_string(),
            persistent: false,
            ttl_seconds: 12,
            dedupe_key: Some(format!("quest_complete_{}", quest.quest_type)),
            speech_intent: Some(SpeechIntent::QuestComplete.wire_token().to_string()),
            created_at: Utc::now().to_rfc3339(),
            controls: vec![],
            chat_id: None,
        });
        self.enqueue_runtime_event(BuddyRuntimeEvent {
            speech_text: Some(format!("Quest complete: {title}")),
            scene: Some("celebrate".to_string()),
            duration_hint: Some(10),
            persistent: false,
            controls: vec![],
            chat_id: None,
            failure_category: None,
            failure_summary: None,
            ..make_runtime_event(
                "task_completed",
                &format!("Quest complete: {title}"),
                "buddy_quest",
                &format!("quest_complete_{}", quest.quest_type),
                "completed",
                Some("high"),
            )
        });
        if reward > 0 {
            self.grant_xp(reward);
        }
    }

    pub fn dismiss_quest(&mut self) {
        crate::state::clear_active_quest(&mut self.state);
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::StateUpdated {
            state: self.state.clone(),
        });
    }

    pub fn grant_xp(&mut self, amount: u64) {
        crate::state::grant_xp(&mut self.state, amount);
        self.refresh_active_quest();
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::StateUpdated {
            state: self.state.clone(),
        });
    }

    pub fn apply_pet_tick(&mut self, elapsed_seconds: u64) {
        if !self.settings.enabled {
            return;
        }
        if !crate::state::apply_pet_tick(&mut self.state, elapsed_seconds) {
            return;
        }
        self.refresh_active_quest();
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::StateUpdated {
            state: self.state.clone(),
        });
    }

    pub fn add_suggestion(&mut self, suggestion: BuddySuggestion) {
        if self.state.suggestion_state.len() >= 50 {
            if let Some(pos) = self.state.suggestion_state.iter().position(|s| s.dismissed) {
                self.state.suggestion_state.remove(pos);
            }
        }
        self.state.suggestion_state.push(suggestion.clone());
        self.last_suggestion_at = Some(Instant::now());
        self.dirty = true;
        let _ = self
            .events_tx
            .send(BuddyEvent::SuggestionAdded { suggestion });
    }

    pub fn maybe_add_suggestion(&mut self, suggestion: BuddySuggestion) -> bool {
        if let Some(last) = self.last_suggestion_at {
            if last.elapsed().as_secs() < SUGGESTION_RATE_LIMIT_SECS {
                return false;
            }
        }
        let dupe = self.state.suggestion_state.iter().any(|s| {
            !s.dismissed
                && s.suggestion_type == suggestion.suggestion_type
                && s.title == suggestion.title
        });
        if dupe {
            return false;
        }
        self.add_suggestion(suggestion);
        true
    }

    pub fn dismiss_suggestion(&mut self, id: &str) {
        if let Some(s) = self.state.suggestion_state.iter_mut().find(|s| s.id == id) {
            s.dismissed = true;
        }
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::SuggestionDismissed {
            suggestion_id: id.to_string(),
        });
    }

    pub fn workflow_failed(&mut self, workflow_id: &str, activity: crate::types::BuddyActivity) {
        self.add_activity(activity);
        let now = Utc::now().to_rfc3339();
        if let Some(ws) = self
            .state
            .workflow_summaries
            .iter_mut()
            .find(|w| w.workflow_id == workflow_id)
        {
            ws.last_run = Some(now);
            ws.run_count += 1;
            ws.last_outcome = Some("failed".to_string());
        } else {
            self.state
                .workflow_summaries
                .push(crate::types::BuddyWorkflowSummary {
                    workflow_id: workflow_id.to_string(),
                    last_run: Some(now),
                    run_count: 1,
                    failure_category: None,
                    failure_summary: None,
                    last_outcome: Some("failed".to_string()),
                    ..Default::default()
                });
        }
        self.refresh_active_quest();
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::StateUpdated {
            state: self.state.clone(),
        });
    }

    pub fn record_workflow_telemetry(
        &mut self,
        workflow_id: &str,
        llm_calls: u64,
        tokens_in: u64,
        tokens_out: u64,
        produced_output: bool,
    ) {
        if llm_calls == 0 && tokens_in == 0 && tokens_out == 0 && !produced_output {
            return;
        }
        let summary = if let Some(idx) = self
            .state
            .workflow_summaries
            .iter()
            .position(|w| w.workflow_id == workflow_id)
        {
            &mut self.state.workflow_summaries[idx]
        } else {
            self.state
                .workflow_summaries
                .push(crate::types::BuddyWorkflowSummary {
                    workflow_id: workflow_id.to_string(),
                    ..Default::default()
                });
            self.state.workflow_summaries.last_mut().unwrap()
        };
        summary.llm_calls = summary.llm_calls.saturating_add(llm_calls);
        summary.tokens_in = summary.tokens_in.saturating_add(tokens_in);
        summary.tokens_out = summary.tokens_out.saturating_add(tokens_out);
        if produced_output {
            summary.outputs = summary.outputs.saturating_add(1);
            summary.last_output_at = Some(Utc::now().to_rfc3339());
        }
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.state
            .llm_spend
            .record(&today, llm_calls, tokens_in, tokens_out);
        self.dirty = true;
    }

    pub fn llm_budget_exhausted(&self) -> bool {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.state
            .llm_spend
            .is_over_budget(&today, self.settings.daily_llm_token_budget)
    }

    /// Charge an agent interjection to the speech budget, and remember it in the
    /// decision log so a dropped nudge is visible in the GUI rather than silent.
    ///
    /// This is deliberately *not* the same path as `gate_chat_reaction_event`:
    /// that one lets an `intent_budget` drop through for runtime events. An
    /// interjection is stricter on purpose — see
    /// [`crate::chat_interjection::gate_interjection`].
    pub fn record_interjection_emission(&mut self, chat_id: &str, text: &str) {
        let now = Utc::now();
        crate::speech_policy::record_emission(
            &mut self.state.speech_rotation,
            SpeechIntent::AgentInterjection,
            now,
        );
        self.record_speech_decision(
            &BuddySpeechItem {
                id: format!("buddy-interjection-{}", Uuid::new_v4()),
                text: refact_core::string_utils::safe_truncate(text, 120).to_string(),
                mood: "neutral".to_string(),
                scope: "global".to_string(),
                persistent: false,
                ttl_seconds: 10,
                dedupe_key: None,
                speech_intent: Some(
                    crate::speech_policy::intent_key(SpeechIntent::AgentInterjection)
                        .to_string(),
                ),
                created_at: now.to_rfc3339(),
                controls: vec![],
                chat_id: Some(chat_id.to_string()),
            },
            Some(SpeechIntent::AgentInterjection),
            crate::speech_policy::SpeechGateDecision {
                allowed: true,
                reason: "interjection_delivered",
            },
        );
    }

    pub fn record_workflow_failure_report(
        &mut self,
        report: crate::workflows::WorkflowFailureReport,
    ) -> Option<(std::path::PathBuf, crate::workflows::WorkflowFailureReport)> {
        if !validate_workflow_id(&report.workflow_id) {
            tracing::warn!(
                "buddy: refusing to record failure report with invalid workflow_id '{}'",
                report.workflow_id
            );
            return None;
        }
        let title = format!(
            "{} failed: {}",
            crate::workflows::workflow_label(&report.workflow_id),
            report.category.title()
        );
        let summary = redact_sensitive(&report.summary);
        let detail = redact_sensitive(&report.detail);
        let description = if summary.trim().is_empty() {
            detail.clone()
        } else {
            summary.clone()
        };
        let activity = BuddyActivity {
            icon: "⚠️".to_string(),
            title: title.clone(),
            description: refact_core::string_utils::safe_truncate(&description, 240).to_string(),
            timestamp: Utc::now().to_rfc3339(),
            activity_type: report.workflow_id.clone(),
            chat_id: report.chat_id.clone(),
            failure_category: Some(report.category.as_str().to_string()),
            failure_summary: Some(summary.clone()),
        };
        self.add_activity(activity);

        let now = Utc::now().to_rfc3339();
        let outcome = format!("failed:{}", report.category.as_str());
        if let Some(ws) = self
            .state
            .workflow_summaries
            .iter_mut()
            .find(|w| w.workflow_id == report.workflow_id)
        {
            ws.last_run = Some(now.clone());
            ws.run_count += 1;
            ws.failure_category = Some(report.category.as_str().to_string());
            ws.failure_summary = Some(summary.clone());
            ws.last_outcome = Some(outcome.clone());
        } else {
            self.state
                .workflow_summaries
                .push(crate::types::BuddyWorkflowSummary {
                    workflow_id: report.workflow_id.clone(),
                    last_run: Some(now),
                    run_count: 1,
                    failure_category: Some(report.category.as_str().to_string()),
                    failure_summary: Some(summary.clone()),
                    last_outcome: Some(outcome),
                    ..Default::default()
                });
        }
        self.refresh_active_quest();
        self.dirty = true;
        let _ = self.events_tx.send(BuddyEvent::StateUpdated {
            state: self.state.clone(),
        });

        let mut event = make_runtime_event(
            &format!("{}_failed", report.workflow_id),
            &title,
            "buddy",
            &format!(
                "workflow_failure:{}:{}",
                report.workflow_id,
                report.category.as_str()
            ),
            "failed",
            Some(report.category.priority()),
        );
        event.description = Some(refact_core::string_utils::safe_truncate(&detail, 600).to_string());
        event.failure_category = Some(report.category.as_str().to_string());
        event.failure_summary = Some(summary.clone());
        event.speech_text = Some(if summary.trim().is_empty() {
            format!(
                "{}: {}",
                report.category.title(),
                refact_core::string_utils::safe_truncate(&detail, 160)
            )
        } else {
            summary.clone()
        });
        event.scene = Some("alert".to_string());
        event.duration_hint = Some(12);
        event.persistent = true;
        event.bubble_policy = Some(crate::types::BuddyBubblePolicy::Durable);
        event.chat_id = report.chat_id.clone();
        self.enqueue_runtime_event(event);

        self.workflow_failure_transcript_write(&report)
    }

    pub fn add_diagnostic(&mut self, mut ctx: crate::diagnostics::DiagnosticContext) {
        ctx.error_message = redact_sensitive(&ctx.error_message);
        ctx.source_file = ctx
            .source_file
            .as_deref()
            .and_then(redact_diagnostic_metadata);
        ctx.tool_name = ctx
            .tool_name
            .as_deref()
            .and_then(redact_diagnostic_metadata);
        let signature = crate::diagnostics::diagnostic_signature(&ctx);
        let duplicate = self
            .recent_diagnostics
            .iter()
            .rev()
            .any(|existing| crate::diagnostics::diagnostic_signature(existing) == signature);
        if duplicate {
            self.enqueue_diagnostic_runtime_event(&ctx);
            return;
        }
        self.recent_diagnostics.push(ctx.clone());
        if self.recent_diagnostics.len() > 100 {
            self.recent_diagnostics.remove(0);
        }
        let project_root = self.project_root.clone();
        let ctx_for_disk = ctx.clone();
        let handle = tokio::spawn(async move {
            if let Err(err) = crate::storage::append_diagnostic(&project_root, &ctx_for_disk).await
            {
                warn!("buddy: failed to persist diagnostic history: {}", err);
            }
        });
        self.track_background_task(handle);
        let _ = self.events_tx.send(BuddyEvent::DiagnosticAdded {
            diagnostic: ctx.clone(),
        });

        // Surface every diagnostic as a runtime event so it lands in the
        // "Recent Errors" panel and is persisted to runtime_queue.jsonl.
        // This catches frontend errors (POST /v1/buddy/diagnostics/collect)
        // and backend report_error paths (chrome / mcp tools) which would
        // otherwise be invisible to the panel.
        self.enqueue_diagnostic_runtime_event(&ctx);
    }

    fn enqueue_diagnostic_runtime_event(&mut self, ctx: &crate::diagnostics::DiagnosticContext) {
        use crate::diagnostics::DiagnosticSeverity;
        let priority = match ctx.severity {
            DiagnosticSeverity::Critical => "critical",
            DiagnosticSeverity::High => "high",
            DiagnosticSeverity::Medium => "normal",
            DiagnosticSeverity::Low => "low",
        };
        let redacted = redact_sensitive(&ctx.error_message);
        let truncated: String = redacted.chars().take(80).collect();
        let title = if ctx.error_type.is_empty() {
            truncated.clone()
        } else {
            format!("{}: {}", ctx.error_type, truncated)
        };
        let dedupe_key = format!("diag:{}", crate::diagnostics::diagnostic_signature(ctx));
        let source = ctx
            .source_file
            .as_deref()
            .or(ctx.tool_name.as_deref())
            .unwrap_or("buddy");
        let mut ev = make_runtime_event(
            "error",
            &title,
            source,
            &dedupe_key,
            "failed",
            Some(priority),
        );
        ev.description = Some(redacted);
        ev.chat_id = ctx.chat_id.clone();
        self.enqueue_runtime_event(ev);
    }

    pub fn diagnostic_by_collected_at(
        &self,
        collected_at: &str,
    ) -> Option<crate::diagnostics::DiagnosticContext> {
        self.recent_diagnostics
            .iter()
            .find(|diag| diag.collected_at == collected_at)
            .cloned()
    }

    pub fn diagnostic_by_id(&self, id: &str) -> Option<crate::diagnostics::DiagnosticContext> {
        self.recent_diagnostics
            .iter()
            .find(|diag| crate::diagnostics::diagnostic_id(diag) == id)
            .cloned()
    }

    pub fn record_issue_created(&mut self, error_message: String) {
        self.last_issue_at = Some(Instant::now());
        let now = chrono::Utc::now();
        self.recent_issue_errors.push((error_message, now));
        self.recent_issue_errors
            .retain(|(_, ts)| now.signed_duration_since(*ts).num_seconds() < 86400);
        if self.recent_issue_errors.len() > 200 {
            let excess = self.recent_issue_errors.len() - 200;
            self.recent_issue_errors.drain(0..excess);
        }
    }

    pub async fn append_workflow_transcript(
        &self,
        project_root: &std::path::Path,
        workflow_id: &str,
        output_summary: &str,
        success: bool,
    ) {
        Self::append_workflow_transcript_to_path(
            project_root,
            workflow_id,
            output_summary,
            success,
        )
        .await;
    }

    pub async fn append_workflow_transcript_to_path(
        project_root: &std::path::Path,
        workflow_id: &str,
        output_summary: &str,
        success: bool,
    ) {
        if !validate_workflow_id(workflow_id) {
            warn!("buddy: rejecting invalid workflow_id: {:?}", workflow_id);
            return;
        }
        let path = project_root.join(format!(
            ".refact/buddy/chats/workflows/{}.json",
            workflow_id
        ));
        crate::workflows::append_workflow_entry(&path, output_summary, success).await;
    }

    pub async fn append_workflow_failure_transcript(
        path: &std::path::Path,
        report: &crate::workflows::WorkflowFailureReport,
    ) {
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                warn!(
                    "buddy: failed to create workflow transcript dir {:?}: {}",
                    parent, err
                );
                return;
            }
        }
        crate::workflows::append_workflow_entry_with_failure(
            path,
            &report.detail,
            false,
            Some((&report.category, report.summary.as_str())),
        )
        .await;
    }

    fn workflow_failure_transcript_write(
        &self,
        report: &crate::workflows::WorkflowFailureReport,
    ) -> Option<(std::path::PathBuf, crate::workflows::WorkflowFailureReport)> {
        if !validate_workflow_id(&report.workflow_id) {
            warn!(
                "buddy: rejecting invalid workflow_id: {:?}",
                report.workflow_id
            );
            return None;
        }
        let path = self.project_root.join(format!(
            ".refact/buddy/chats/workflows/{}.json",
            report.workflow_id
        ));
        Some((path, report.clone()))
    }

    pub fn report_error(
        &mut self,
        error_type: &str,
        error_msg: &str,
        source: Option<&str>,
        chat_id: Option<&str>,
    ) {
        self.report_error_with_model(error_type, error_msg, source, chat_id, None);
    }

    pub fn report_error_with_model(
        &mut self,
        error_type: &str,
        error_msg: &str,
        source: Option<&str>,
        chat_id: Option<&str>,
        model_id: Option<&str>,
    ) {
        let severity = crate::diagnostics::classify_diagnostic_severity(error_msg);
        let ctx = crate::diagnostics::DiagnosticContext {
            error_type: error_type.to_string(),
            error_message: error_msg.to_string(),
            source_file: source.map(|s| s.to_string()),
            tool_name: None,
            chat_id: chat_id.map(|s| s.to_string()),
            model_id: model_id.map(|s| s.to_string()),
            collected_at: Utc::now().to_rfc3339(),
            severity,
            occurrences: None,
        };
        self.add_diagnostic(ctx);
        let redacted = redact_sensitive(error_msg);
        let truncated: String = redacted.chars().take(80).collect();
        self.add_activity(BuddyActivity {
            icon: "⚠️".to_string(),
            title: format!("{}: {}", error_type, truncated),
            description: redacted,
            timestamp: Utc::now().to_rfc3339(),
            activity_type: "error".to_string(),
            chat_id: chat_id.map(|s| s.to_string()),
            failure_category: None,
            failure_summary: None,
        });
        self.dirty = true;
    }

    pub fn expire_suggestions(&mut self) {
        let now = chrono::Utc::now();
        let mut changed = false;
        for s in self.state.suggestion_state.iter_mut() {
            if s.dismissed {
                continue;
            }
            let expiry_secs = if s.quest.is_some() {
                QUEST_SUGGESTION_EXPIRY_SECS
            } else {
                SUGGESTION_EXPIRY_SECS
            };
            if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&s.created_at) {
                let age = now.signed_duration_since(created).num_seconds();
                if age > expiry_secs {
                    s.dismissed = true;
                    changed = true;
                }
            }
        }
        let before = self.state.suggestion_state.len();
        self.state.suggestion_state.retain(|s| {
            if !s.dismissed {
                return true;
            }
            let retention_secs = if s.quest.is_some() {
                QUEST_SUGGESTION_EXPIRY_SECS + 3600
            } else {
                3600
            };
            if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&s.created_at) {
                now.signed_duration_since(created).num_seconds() < retention_secs
            } else {
                false
            }
        });
        if changed || self.state.suggestion_state.len() != before {
            self.dirty = true;
            let _ = self.events_tx.send(BuddyEvent::StateUpdated {
                state: self.state.clone(),
            });
        }
    }
}

pub fn make_runtime_event(
    signal_type: &str,
    title: &str,
    source: &str,
    dedupe_key: &str,
    status: &str,
    priority: Option<&str>,
) -> BuddyRuntimeEvent {
    BuddyRuntimeEvent {
        id: Uuid::new_v4().to_string(),
        signal_type: signal_type.to_string(),
        title: title.to_string(),
        description: None,
        source: source.to_string(),
        status: status.to_string(),
        failure_category: None,
        failure_summary: None,
        progress: None,
        dedupe_key: Some(dedupe_key.to_string()),
        priority: priority.unwrap_or("normal").to_string(),
        created_at: Utc::now().to_rfc3339(),
        ttl_ms: None,
        bubble_policy: None,
        speech_text: None,
        scene: None,
        duration_hint: None,
        persistent: false,
        controls: Vec::new(),
        chat_id: None,
        dismissed: false,
    }
}

pub fn same_day_log_filter(line: &str, collected_at: &str) -> bool {
    let Some(prefix) = line.get(0..6) else {
        return false;
    };
    let Some(target) = chrono::DateTime::parse_from_rfc3339(collected_at)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
    else {
        return true;
    };
    let Ok(time) = chrono::NaiveTime::parse_from_str(prefix, "%H%M%S") else {
        return false;
    };
    let candidate = target.date_naive().and_time(time).and_utc();
    let diff = target.signed_duration_since(candidate).num_seconds();
    diff >= 0 && diff <= 24 * 3600
}

pub struct BuddyMutation {
    pub runtime_event: Option<BuddyRuntimeEvent>,
    pub xp: u64,
    pub activity: Option<crate::types::BuddyActivity>,
    pub mood: Option<String>,
}

impl Default for BuddyMutation {
    fn default() -> Self {
        Self {
            runtime_event: None,
            xp: 0,
            activity: None,
            mood: None,
        }
    }
}
