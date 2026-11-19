use crate::app_state::AppState;

use crate::buddy_scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
use refact_buddy_core::types::{BuddyControl, BuddySpeechItem};
use crate::buddy_voice_service::{SpeechIntent, VoiceCtx, voice_service};

fn greeting_fallback_text(ctx: &BuddyJobContext) -> String {
    if !ctx.onboarding.greeted {
        format!("Hi! I'm {}! 🎉 I'll help you with errors, setup, and staying productive. Want a quick tour?", ctx.identity_name)
    } else {
        format!(
            "Welcome back! 👋 I'm {} — ready to help.",
            ctx.identity_name
        )
    }
}

fn greeting_controls(ctx: &BuddyJobContext) -> Vec<BuddyControl> {
    if !ctx.onboarding.greeted {
        vec![
            BuddyControl {
                id: "tour".to_string(),
                label: "Take a Tour".to_string(),
                action: "open_buddy".to_string(),
                action_param: None,
                style: "primary".to_string(),
            },
            BuddyControl {
                id: "skip".to_string(),
                label: "Skip".to_string(),
                action: "dismiss".to_string(),
                action_param: None,
                style: "secondary".to_string(),
            },
        ]
    } else {
        vec![]
    }
}

fn greeting_workflow_summary(
    ctx: &BuddyJobContext,
    pulse_one_liner: &str,
    fallback: &str,
) -> String {
    ctx.workflow_summaries
        .last()
        .map(|summary| {
            let status = summary
                .last_outcome
                .as_deref()
                .or(summary.failure_summary.as_deref())
                .unwrap_or("unknown");
            format!(
                "recent workflow: {} {}; {}",
                summary.workflow_id, status, pulse_one_liner
            )
        })
        .unwrap_or_else(|| fallback.to_string())
}

pub struct GreetingJob;

#[async_trait::async_trait]
impl BuddyJob for GreetingJob {
    fn id(&self) -> &str {
        "greeting"
    }
    fn cooldown_seconds(&self) -> u64 {
        86400
    }
    fn priority(&self) -> u32 {
        0
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        !ctx.onboarding.greeted || ctx.job_state.last_run.is_none()
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let fallback_text = greeting_fallback_text(&ctx);
        let controls = greeting_controls(&ctx);
        let mut speech = match crate::buddy_actor_runtime::buddy_snapshot(gcx.clone()).await {
            Some(snapshot) => {
                let pulse_one_liner = format!(
                    "{} pending ops, {} recent stuck task alerts",
                    snapshot.pulse.memory.pending_ops,
                    snapshot.pulse.tasks.recent_stuck_alert_count_1h()
                );
                let workflow_summary =
                    greeting_workflow_summary(&ctx, &pulse_one_liner, &fallback_text);
                let voice_ctx = VoiceCtx {
                    persona: &snapshot.state.personality,
                    identity_name: snapshot.state.identity.name.as_str(),
                    pulse_one_liner,
                    workflow_id: Some("greeting"),
                    workflow_summary: Some(&workflow_summary),
                };
                voice_service()
                    .await
                    .render_speech(gcx, voice_ctx, SpeechIntent::Greeting)
                    .await
            }
            None => BuddySpeechItem {
                id: format!("greeting-{}", chrono::Utc::now().timestamp()),
                text: fallback_text,
                mood: "happy".to_string(),
                scope: "global".to_string(),
                persistent: false,
                ttl_seconds: if ctx.onboarding.greeted { 8 } else { 15 },
                dedupe_key: Some("greeting".to_string()),
                created_at: chrono::Utc::now().to_rfc3339(),
                controls: vec![],
                chat_id: None,
                speech_intent: None,
            },
        };
        speech.ttl_seconds = if ctx.onboarding.greeted { 8 } else { 15 };
        speech.dedupe_key = Some("greeting".to_string());
        speech.controls = controls;
        BuddyJobResult {
            speech: Some(speech),
            speech_intent: Some(SpeechIntent::Greeting),
            mark_greeted: true,
            ..Default::default()
        }
    }
}
