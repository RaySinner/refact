//! Seam over the engine's buddy speech renderers.
//!
//! Rendering buddy speech needs the engine's subchat/LLM machinery
//! (`buddy::voice_service`), so the engine installs the implementation through the
//! `ContextFactory` and crate-side HTTP routers reach it from here.

use std::sync::Arc;

use async_trait::async_trait;
use refact_buddy_core::actor::BuddyMutation;
use refact_buddy_core::types::{
    BuddyPersonalityProfile, BuddyPulse, BuddyQuest, BuddySpeechItem,
};
use refact_buddy_core::voice_service::SpeechIntent;

use crate::app_state::AppState;

pub struct CompletedQuestVoice {
    pub mutation: BuddyMutation,
    pub speech: BuddySpeechItem,
}

#[async_trait]
pub trait BuddySpeechRuntime: Send + Sync {
    async fn render_buddy_speech(
        &self,
        app: AppState,
        persona: BuddyPersonalityProfile,
        identity_name: String,
        pulse: BuddyPulse,
        workflow_id: Option<String>,
        workflow_summary: String,
        intent: SpeechIntent,
        fallback_text: String,
    ) -> BuddySpeechItem;

    async fn complete_quest_with_voice(
        &self,
        app: AppState,
        quest: BuddyQuest,
        persona: BuddyPersonalityProfile,
        identity_name: String,
        pulse: BuddyPulse,
    ) -> CompletedQuestVoice;
}

fn runtime(app: &AppState) -> Arc<dyn BuddySpeechRuntime> {
    app.gcx.context_factory.get().buddy_speech()
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
    let runtime = runtime(&app);
    runtime
        .render_buddy_speech(
            app,
            persona,
            identity_name,
            pulse,
            workflow_id,
            workflow_summary,
            intent,
            fallback_text,
        )
        .await
}

pub async fn complete_quest_with_voice(
    app: AppState,
    quest: BuddyQuest,
    persona: BuddyPersonalityProfile,
    identity_name: String,
    pulse: BuddyPulse,
) -> CompletedQuestVoice {
    let runtime = runtime(&app);
    runtime
        .complete_quest_with_voice(app, quest, persona, identity_name, pulse)
        .await
}
