use chrono::Utc;
use tracing::debug;

use crate::app_state::AppState;
use crate::call_validation::ChatContent;
use crate::chat::types::ThreadParams;

#[cfg(test)]
use super::settings::BuddySettings;
use super::types::{BuddyChatPhraseBank, BuddyPersonalityProfile, BuddyRuntimeEvent};
use super::voice_service::{voice_service, ChatReactionSpeechIntent, VoiceCtx};
#[cfg(test)]
use super::voice_service::VoiceService;

pub use refact_buddy_core::chat_reactions::*;

const CHAT_REACTION_PERMITS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

pub struct AcceptedUserMessage {
    pub chat_id: String,
    pub thread: ThreadParams,
    pub content: ChatContent,
}

pub struct ChatActivityCompletion {
    pub chat_id: String,
    pub thread: ThreadParams,
}

fn record_queued_reaction(
    svc: &mut crate::buddy::actor::BuddyService,
    chat_id: &str,
    signal_type: &str,
    event: BuddyRuntimeEvent,
    reservation: ChatReactionLimiterReservation,
) -> bool {
    if !svc.gate_chat_reaction_event(&event) {
        svc.chat_reaction_limiter.rollback(reservation);
        svc.chat_reaction_debug
            .record_not_queued(chat_id, signal_type);
        return false;
    }
    let stored_event = svc.enqueue_runtime_event_with_stored(event);
    if let Some(stored_event) = stored_event.as_ref() {
        svc.chat_reaction_limiter
            .record_success(chat_id, stored_event, Utc::now());
        svc.chat_reaction_debug
            .record_emitted(chat_id, signal_type, Some(stored_event));
        true
    } else {
        svc.chat_reaction_limiter.rollback(reservation);
        svc.chat_reaction_debug
            .record_not_queued(chat_id, signal_type);
        false
    }
}

pub fn should_observe_thread(thread: &ThreadParams) -> bool {
    if let Some(meta) = thread.buddy_meta.as_ref() {
        if meta.is_buddy_chat {
            return false;
        }
    }
    if thread.task_meta.is_some() {
        return false;
    }
    let mode = thread.mode.as_str();
    if matches!(mode, "buddy" | "setup" | "task_agent" | "task_planner") {
        return false;
    }
    if mode.starts_with("setup_") {
        return false;
    }
    if thread.parent_id.is_some() && thread.link_type.as_deref() != Some("branch") {
        return false;
    }
    true
}
#[derive(Clone)]
struct ChatReactionVoiceInputs {
    persona: BuddyPersonalityProfile,
    identity_name: String,
    pulse_one_liner: String,
}

pub fn chat_reaction_candidate(
    thread: &ThreadParams,
    raw_text: &str,
    settings: Option<&BuddySettings>,
) -> Result<ChatReactionCandidate, ChatReactionSkipReason> {
    if !should_observe_thread(thread) {
        return Err(ChatReactionSkipReason::ThreadFiltered);
    }
    let analysis_text =
        prepare_analysis_text(raw_text).ok_or(ChatReactionSkipReason::TextTooShort)?;
    let settings = settings.ok_or(ChatReactionSkipReason::BuddyUnavailable)?;
    if !settings_allow_chat_reactions(settings) {
        return Err(ChatReactionSkipReason::SettingsDisabled);
    }
    let kind = classify_chat_reaction_kind(&analysis_text, settings);
    Ok(ChatReactionCandidate {
        analysis_text,
        kind,
    })
}

async fn render_chat_reaction_text(
    app: &AppState,
    kind: &ChatReactionKind,
    analysis_text: &str,
    voice_inputs: &ChatReactionVoiceInputs,
    phrase_bank: Option<BuddyChatPhraseBank>,
    #[cfg(test)] voice: &VoiceService,
) -> String {
    if let Some(bank) = phrase_bank.as_ref() {
        if super::actor::chat_phrase_bank_is_fresh(bank, Utc::now()) {
            if let Some(line) = phrase_bank_line(bank, kind.clone(), analysis_text) {
                return line;
            }
        }
    }
    let fallback = fallback_chat_reaction_text(kind.clone(), analysis_text);
    let intent = match kind {
        ChatReactionKind::Humor => ChatReactionSpeechIntent::Humor,
        ChatReactionKind::Insight => ChatReactionSpeechIntent::Insight,
        ChatReactionKind::BugCandidate => ChatReactionSpeechIntent::BugCandidate,
        ChatReactionKind::Ambient => ChatReactionSpeechIntent::Ambient,
    };
    let ctx = VoiceCtx {
        persona: &voice_inputs.persona,
        identity_name: voice_inputs.identity_name.as_str(),
        pulse_one_liner: voice_inputs.pulse_one_liner.clone(),
        workflow_id: Some("chat_reaction"),
        workflow_summary: Some(analysis_text),
    };
    #[cfg(test)]
    let rendered = voice.render_chat_reaction(app.clone(), ctx, intent).await;
    #[cfg(not(test))]
    let rendered = voice_service()
        .await
        .render_chat_reaction(app.clone(), ctx, intent)
        .await;
    sanitize_chat_reaction_speech_text(&rendered, analysis_text).unwrap_or(fallback)
}

pub async fn maybe_enqueue_chat_reaction(app: AppState, accepted: AcceptedUserMessage) {
    let raw_text = accepted.content.content_text_only();

    let plan = {
        let mut svc_guard = app.buddy.buddy.lock().await;
        let settings = svc_guard.as_ref().map(|svc| &svc.settings);
        let candidate = match chat_reaction_candidate(&accepted.thread, &raw_text, settings) {
            Ok(candidate) => candidate,
            Err(reason) => {
                if let Some(svc) = svc_guard.as_mut() {
                    svc.chat_reaction_debug
                        .record_skipped(&accepted.chat_id, reason);
                }
                debug!(
                    target: "buddy.chat_reactions",
                    chat_id = %accepted.chat_id,
                    reason = %reason.as_str(),
                    message_chars = raw_text.chars().count(),
                    "buddy chat reaction skipped"
                );
                return;
            }
        };
        let Some(svc) = svc_guard.as_mut() else {
            return;
        };
        let now = chrono::Utc::now();
        let reservation = match svc.chat_reaction_limiter.try_allow_kind(
            &accepted.chat_id,
            candidate.kind.clone(),
            now,
        ) {
            Ok(reservation) => reservation,
            Err(reason) => {
                svc.chat_reaction_debug
                    .record_skipped(&accepted.chat_id, reason);
                debug!(
                    target: "buddy.chat_reactions",
                    chat_id = %accepted.chat_id,
                    reason = %reason.as_str(),
                    reaction_kind = ?candidate.kind,
                    analysis_hash = %message_hash(&candidate.analysis_text),
                    "buddy chat reaction skipped"
                );
                return;
            }
        };
        let voice_inputs = ChatReactionVoiceInputs {
            persona: svc.state.personality.clone(),
            identity_name: svc.state.identity.name.clone(),
            pulse_one_liner: format!(
                "{} pending ops, {} recent stuck task alerts",
                svc.pulse.memory.pending_ops,
                svc.pulse.tasks.recent_stuck_alert_count_1h()
            ),
        };
        let phrase_bank = svc.state.chat_phrase_bank.clone();
        (candidate, reservation, voice_inputs, phrase_bank)
    };

    #[cfg(test)]
    let voice = voice_service().await;
    let app2 = app.clone();
    let chat_id = accepted.chat_id.clone();
    tokio::spawn(async move {
        let (candidate, reservation, voice_inputs, phrase_bank) = plan;
        let permit = match CHAT_REACTION_PERMITS.try_acquire() {
            Ok(permit) => permit,
            Err(_) => {
                let analysis_hash = message_hash(&candidate.analysis_text);
                let mut svc_guard = app2.buddy.buddy.lock().await;
                if let Some(svc) = svc_guard.as_mut() {
                    svc.chat_reaction_limiter.rollback(reservation);
                    svc.chat_reaction_debug
                        .record_skipped(&chat_id, ChatReactionSkipReason::RateLimited);
                }
                debug!(
                    target: "buddy.chat_reactions",
                    chat_id = %chat_id,
                    reason = %ChatReactionSkipReason::RateLimited.as_str(),
                    reaction_kind = ?candidate.kind,
                    analysis_hash = %analysis_hash,
                    "buddy chat reaction skipped"
                );
                return;
            }
        };
        let speech = render_chat_reaction_text(
            &app2,
            &candidate.kind,
            &candidate.analysis_text,
            &voice_inputs,
            phrase_bank,
            #[cfg(test)]
            &voice,
        )
        .await;
        let event = build_reaction_event(
            &chat_id,
            &candidate.analysis_text,
            &ChatReaction {
                kind: candidate.kind.clone(),
                text: speech,
            },
        );
        let event_id = event.id.clone();
        let analysis_hash = message_hash(&candidate.analysis_text);
        let mut svc_guard = app2.buddy.buddy.lock().await;
        if let Some(svc) = svc_guard.as_mut() {
            if settings_allow_chat_reactions(&svc.settings) {
                let signal_type = event.signal_type.clone();
                if record_queued_reaction(svc, &chat_id, signal_type.as_str(), event, reservation) {
                    debug!(
                        target: "buddy.chat_reactions",
                        chat_id = %chat_id,
                        reaction_kind = ?candidate.kind,
                        analysis_hash = %analysis_hash,
                        event_id = %event_id,
                        "buddy chat reaction emitted"
                    );
                } else {
                    debug!(
                        target: "buddy.chat_reactions",
                        chat_id = %chat_id,
                        reaction_kind = ?candidate.kind,
                        analysis_hash = %analysis_hash,
                        event_id = %event_id,
                        "buddy chat reaction not queued"
                    );
                }
            } else {
                svc.chat_reaction_limiter.rollback(reservation);
                svc.chat_reaction_debug
                    .record_skipped(&chat_id, ChatReactionSkipReason::SettingsDisabled);
                debug!(
                    target: "buddy.chat_reactions",
                    chat_id = %chat_id,
                    reason = %ChatReactionSkipReason::SettingsDisabled.as_str(),
                    reaction_kind = ?candidate.kind,
                    analysis_hash = %analysis_hash,
                    "buddy chat reaction skipped"
                );
            }
        } else {
            debug!(
                target: "buddy.chat_reactions",
                chat_id = %chat_id,
                reason = %ChatReactionSkipReason::BuddyUnavailable.as_str(),
                reaction_kind = ?candidate.kind,
                analysis_hash = %analysis_hash,
                "buddy chat reaction skipped"
            );
        }
        drop(permit);
    });
}

pub async fn maybe_enqueue_chat_activity_reaction(app: AppState, activity: ChatActivityCompletion) {
    let title = activity.thread.title.trim();
    let analysis_text = if title.is_empty() {
        CHAT_ACTIVITY_ANALYSIS_TEXT.to_string()
    } else {
        format!("chat '{}' completed activity safely", title)
    };

    let plan = {
        let mut svc_guard = app.buddy.buddy.lock().await;
        if !should_observe_thread(&activity.thread) {
            if let Some(svc) = svc_guard.as_mut() {
                svc.chat_reaction_debug
                    .record_skipped(&activity.chat_id, ChatReactionSkipReason::ThreadFiltered);
            }
            debug!(
                target: "buddy.chat_reactions",
                chat_id = %activity.chat_id,
                reason = %ChatReactionSkipReason::ThreadFiltered.as_str(),
                "buddy chat activity reaction skipped"
            );
            return;
        }
        let Some(svc) = svc_guard.as_mut() else {
            debug!(
                target: "buddy.chat_reactions",
                chat_id = %activity.chat_id,
                reason = %ChatReactionSkipReason::BuddyUnavailable.as_str(),
                "buddy chat activity reaction skipped"
            );
            return;
        };
        if !settings_allow_chat_reactions(&svc.settings) {
            svc.chat_reaction_debug
                .record_skipped(&activity.chat_id, ChatReactionSkipReason::SettingsDisabled);
            debug!(
                target: "buddy.chat_reactions",
                chat_id = %activity.chat_id,
                reason = %ChatReactionSkipReason::SettingsDisabled.as_str(),
                "buddy chat activity reaction skipped"
            );
            return;
        }
        let now = Utc::now();
        if svc
            .chat_reaction_limiter
            .has_recent_non_bug_success(&activity.chat_id, now)
        {
            svc.chat_reaction_debug
                .record_skipped(&activity.chat_id, ChatReactionSkipReason::RateLimited);
            debug!(
                target: "buddy.chat_reactions",
                chat_id = %activity.chat_id,
                reason = %ChatReactionSkipReason::RateLimited.as_str(),
                reaction_kind = ?ChatReactionKind::Ambient,
                analysis_hash = %message_hash(&analysis_text),
                "buddy chat activity reaction skipped"
            );
            return;
        }
        let reservation = match svc.chat_reaction_limiter.try_allow_kind(
            &activity.chat_id,
            ChatReactionKind::Ambient,
            now,
        ) {
            Ok(reservation) => reservation,
            Err(reason) => {
                svc.chat_reaction_debug
                    .record_skipped(&activity.chat_id, reason);
                debug!(
                    target: "buddy.chat_reactions",
                    chat_id = %activity.chat_id,
                    reason = %reason.as_str(),
                    reaction_kind = ?ChatReactionKind::Ambient,
                    analysis_hash = %message_hash(&analysis_text),
                    "buddy chat activity reaction skipped"
                );
                return;
            }
        };
        let phrase_bank = svc.state.chat_phrase_bank.clone();
        (reservation, analysis_text, phrase_bank)
    };

    let (reservation, analysis_text, phrase_bank) = plan;
    let permit = match CHAT_REACTION_PERMITS.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            let mut svc_guard = app.buddy.buddy.lock().await;
            if let Some(svc) = svc_guard.as_mut() {
                svc.chat_reaction_limiter.rollback(reservation);
                svc.chat_reaction_debug
                    .record_skipped(&activity.chat_id, ChatReactionSkipReason::RateLimited);
            }
            debug!(
                target: "buddy.chat_reactions",
                chat_id = %activity.chat_id,
                reason = %ChatReactionSkipReason::RateLimited.as_str(),
                reaction_kind = ?ChatReactionKind::Ambient,
                analysis_hash = %message_hash(&analysis_text),
                "buddy chat activity reaction skipped"
            );
            return;
        }
    };
    let speech = phrase_bank
        .as_ref()
        .filter(|bank| super::actor::chat_phrase_bank_is_fresh(bank, Utc::now()))
        .and_then(|bank| phrase_bank_line(bank, ChatReactionKind::Ambient, &analysis_text))
        .unwrap_or_else(|| fallback_chat_reaction_text(ChatReactionKind::Ambient, &analysis_text));
    let event = build_reaction_event(
        &activity.chat_id,
        &analysis_text,
        &ChatReaction {
            kind: ChatReactionKind::Ambient,
            text: speech,
        },
    );
    let event_id = event.id.clone();
    let analysis_hash = message_hash(&analysis_text);
    let mut svc_guard = app.buddy.buddy.lock().await;
    if let Some(svc) = svc_guard.as_mut() {
        if settings_allow_chat_reactions(&svc.settings) {
            let signal_type = event.signal_type.clone();
            if record_queued_reaction(
                svc,
                &activity.chat_id,
                signal_type.as_str(),
                event,
                reservation,
            ) {
                debug!(
                    target: "buddy.chat_reactions",
                    chat_id = %activity.chat_id,
                    reaction_kind = ?ChatReactionKind::Ambient,
                    analysis_hash = %analysis_hash,
                    event_id = %event_id,
                    "buddy chat activity reaction emitted"
                );
            } else {
                debug!(
                    target: "buddy.chat_reactions",
                    chat_id = %activity.chat_id,
                    reaction_kind = ?ChatReactionKind::Ambient,
                    analysis_hash = %analysis_hash,
                    event_id = %event_id,
                    "buddy chat activity reaction not queued"
                );
            }
        } else {
            svc.chat_reaction_limiter.rollback(reservation);
            svc.chat_reaction_debug
                .record_skipped(&activity.chat_id, ChatReactionSkipReason::SettingsDisabled);
            debug!(
                target: "buddy.chat_reactions",
                chat_id = %activity.chat_id,
                reason = %ChatReactionSkipReason::SettingsDisabled.as_str(),
                reaction_kind = ?ChatReactionKind::Ambient,
                analysis_hash = %analysis_hash,
                "buddy chat activity reaction skipped"
            );
        }
    }
    drop(permit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_reaction_candidate_reports_skip_reasons() {
        let settings = BuddySettings::default();
        let thread = ThreadParams::default();
        let normal_text = "please design a new component state flow now";

        let mut filtered = ThreadParams::default();
        filtered.mode = "task_agent".to_string();
        assert_eq!(
            chat_reaction_candidate(&filtered, normal_text, Some(&settings)),
            Err(ChatReactionSkipReason::ThreadFiltered)
        );

        assert_eq!(
            chat_reaction_candidate(&thread, "too short", Some(&settings)),
            Err(ChatReactionSkipReason::TextTooShort)
        );

        assert_eq!(
            chat_reaction_candidate(&thread, normal_text, None),
            Err(ChatReactionSkipReason::BuddyUnavailable)
        );

        let mut disabled = BuddySettings::default();
        disabled.chat_reactions_enabled = false;
        assert_eq!(
            chat_reaction_candidate(&thread, normal_text, Some(&disabled)),
            Err(ChatReactionSkipReason::SettingsDisabled)
        );

        let mut disabled = BuddySettings::default();
        disabled.enabled = false;
        assert_eq!(
            chat_reaction_candidate(&thread, normal_text, Some(&disabled)),
            Err(ChatReactionSkipReason::SettingsDisabled)
        );

        let mut disabled = BuddySettings::default();
        disabled.quiet_mode = true;
        assert_eq!(
            chat_reaction_candidate(&thread, normal_text, Some(&disabled)),
            Err(ChatReactionSkipReason::SettingsDisabled)
        );

        let mut no_kind = BuddySettings::default();
        no_kind.chat_reactions_enabled = false;
        assert_eq!(
            chat_reaction_candidate(
                &thread,
                "please write a friendly greeting for this tiny helper",
                Some(&no_kind),
            ),
            Err(ChatReactionSkipReason::SettingsDisabled)
        );
    }
}
