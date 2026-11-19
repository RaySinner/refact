use crate::app_state::AppState;

use crate::buddy_scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
use refact_buddy_core::types::BuddySpeechItem;
use crate::buddy_voice_service::{SpeechIntent, SpeechIntentWireToken, VoiceCtx, voice_service};

pub struct TourJob;

pub fn tour_fallback_speech(text: String) -> BuddySpeechItem {
    BuddySpeechItem {
        id: format!("tour-{}", chrono::Utc::now().timestamp()),
        text,
        mood: "excited".to_string(),
        scope: "global".to_string(),
        persistent: false,
        ttl_seconds: 15,
        dedupe_key: Some("tour".to_string()),
        created_at: chrono::Utc::now().to_rfc3339(),
        controls: vec![],
        chat_id: None,
        speech_intent: None,
    }
}

pub fn normalize_tour_speech(
    mut fallback: BuddySpeechItem,
    voice_speech: BuddySpeechItem,
) -> BuddySpeechItem {
    if !voice_speech.text.trim().is_empty() {
        fallback.text = voice_speech.text;
        if !voice_speech.mood.trim().is_empty() {
            fallback.mood = voice_speech.mood;
        }
    }
    fallback
}

async fn emit_tour_speech(gcx: &AppState, mut speech: BuddySpeechItem) -> bool {
    speech.speech_intent = Some(SpeechIntent::Tour.wire_token().to_string());
    let buddy_arc = gcx.buddy.buddy.clone();
    let mut buddy = buddy_arc.lock().await;
    let Some(svc) = buddy.as_mut() else {
        return false;
    };
    svc.update_speech(speech)
}

#[async_trait::async_trait]
impl BuddyJob for TourJob {
    fn id(&self) -> &str {
        "tour"
    }
    fn cooldown_seconds(&self) -> u64 {
        86400
    }
    fn priority(&self) -> u32 {
        1
    }

    fn records_empty_result(&self) -> bool {
        false
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        ctx.onboarding.greeted && !ctx.onboarding.tour_completed && ctx.job_state.last_run.is_none()
    }

    async fn execute(&self, gcx: AppState, _ctx: BuddyJobContext) -> BuddyJobResult {
        let fallback_text = "This is me on your dashboard — I track everything happening in your project. Ask me about setup, skills, or MCP anytime!".to_string();
        let mut speech = tour_fallback_speech(fallback_text.clone());
        if let Some(snapshot) = crate::buddy_actor_runtime::buddy_snapshot(gcx.clone()).await {
            let pulse_one_liner = format!(
                "{} pending ops, {} recent stuck task alerts",
                snapshot.pulse.memory.pending_ops,
                snapshot.pulse.tasks.recent_stuck_alert_count_1h()
            );
            let voice_ctx = VoiceCtx {
                persona: &snapshot.state.personality,
                identity_name: snapshot.state.identity.name.as_str(),
                pulse_one_liner,
                workflow_id: None,
                workflow_summary: Some(&fallback_text),
            };
            let voice_speech = voice_service()
                .await
                .render_speech(gcx.clone(), voice_ctx, SpeechIntent::Tour)
                .await;
            speech = normalize_tour_speech(speech, voice_speech);
        }
        if !emit_tour_speech(&gcx, speech).await {
            return BuddyJobResult::default();
        }
        BuddyJobResult {
            last_result: Some("completed".to_string()),
            ..Default::default()
        }
    }
}
