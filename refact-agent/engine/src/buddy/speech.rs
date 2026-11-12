use chrono::Utc;

use crate::app_state::AppState;

use super::types::{
    BuddyActivity, BuddyPersonalityProfile, BuddyPulse, BuddyQuest, BuddyRuntimeEvent,
    BuddySpeechItem,
};
use super::voice_service::{voice_service, SpeechIntent, VoiceCtx, VoiceIntent};
use refact_buddy_core::actor::{make_runtime_event, redact_sensitive, BuddyMutation};

const BUDDY_SPEECH_MAX_CHARS: usize = 280;
const BUDDY_TITLE_MAX_CHARS: usize = 120;
const BUDDY_DESCRIPTION_MAX_CHARS: usize = 500;
const BUDDY_STATIC_SPEECH_FALLBACK: &str = "Tiny gremlin update: something needs attention.";
const BUDDY_STATIC_TITLE_FALLBACK: &str = "Buddy update";
const BUDDY_STATIC_DESCRIPTION_FALLBACK: &str = "Buddy has an update ready.";

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
