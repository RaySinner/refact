pub use refact_runtime_api::buddy_voice_service::*;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Barrier;
#[cfg(test)]
use refact_buddy_core::types::BuddyPersonalityProfile;

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(archetype_id: &str) -> BuddyPersonalityProfile {
        let mut profile = BuddyPersonalityProfile {
            archetype_id: archetype_id.to_string(),
            archetype_label: archetype_id.to_string(),
            vibe: "bright".to_string(),
            summary: "A helpful test buddy.".to_string(),
            prompt: "Helpful and concise".to_string(),
            traits: Default::default(),
        };
        profile.traits.playfulness = 77;
        profile
    }

    fn voice_ctx<'a>(persona: &'a BuddyPersonalityProfile) -> VoiceCtx<'a> {
        VoiceCtx {
            persona,
            identity_name: "Pixel",
            pulse_one_liner: "Tests are running".to_string(),
            workflow_id: Some("test_workflow"),
            workflow_summary: Some("checking voice service"),
        }
    }

    #[tokio::test]
    async fn voice_returns_fallback_when_renderer_returns_none() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer = TestVoiceRenderer::new(vec![None]);
        let service = VoiceService::new_with_renderer(renderer.clone());
        let persona = persona("helper_sprite");
        let ctx = voice_ctx(&persona);
        let expected = service.fallback_for(VoiceIntent::ChatTitle.as_str(), &ctx);

        let title = service.render_chat_title(gcx, ctx).await;

        assert_eq!(title, expected);
        assert_eq!(renderer.calls(), 1);
    }

    #[tokio::test]
    async fn voice_cache_hits_count_as_recent_lines() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer = TestVoiceRenderer::new(vec![Some("cached sparkle".to_string())]);
        let service = VoiceService::new_with_renderer(renderer.clone());
        let persona = persona("helper_sprite");

        let first = service
            .render_chat_title(gcx.clone(), voice_ctx(&persona))
            .await;
        let second = service.render_chat_title(gcx, voice_ctx(&persona)).await;

        assert_eq!(first, "cached sparkle");
        assert_ne!(second, "cached sparkle");
        assert_eq!(renderer.calls(), 1);
    }

    #[tokio::test]
    async fn voice_avoids_saying_same_cached_line_twice() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer = TestVoiceRenderer::new(vec![Some("cached sparkle".to_string())]);
        let service = VoiceService::new_with_renderer(renderer.clone());
        let persona = persona("helper_sprite");

        let first = service
            .render_speech(gcx.clone(), voice_ctx(&persona), SpeechIntent::Greeting)
            .await;
        let second = service
            .render_speech(gcx, voice_ctx(&persona), SpeechIntent::Greeting)
            .await;

        assert_eq!(first.text, "cached sparkle");
        assert_ne!(first.text, second.text);
        assert!(fallback_phrases(SpeechIntent::Greeting.as_str()).len() >= 2);
        assert_eq!(renderer.calls(), 1);
    }

    #[tokio::test]
    async fn voice_caps_output_at_max_chars() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer = TestVoiceRenderer::new(vec![Some("a".repeat(VOICE_MAX_CHARS + 40))]);
        let service = VoiceService::new_with_renderer(renderer);
        let persona = persona("helper_sprite");

        let title = service.render_chat_title(gcx, voice_ctx(&persona)).await;

        assert_eq!(title.chars().count(), VOICE_MAX_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn voice_truncates_at_word_boundary_with_ellipsis() {
        let line = normalize_voice_line_with_limit(
            "sparkly gremlin words should not be sliced through the middle",
            24,
        );

        assert_eq!(line, "sparkly gremlin words…");
        assert!(line.chars().count() <= 24);
    }

    #[tokio::test]
    async fn voice_strips_newlines() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer = TestVoiceRenderer::new(vec![Some("hello\nbuddy\r\nnow".to_string())]);
        let service = VoiceService::new_with_renderer(renderer);
        let persona = persona("helper_sprite");

        let title = service.render_chat_title(gcx, voice_ctx(&persona)).await;

        assert_eq!(title, "hello buddy now");
        assert!(!title.contains('\n'));
        assert!(!title.contains('\r'));
    }

    #[test]
    fn voice_returns_distinct_fallbacks_per_persona_archetype() {
        let renderer = TestVoiceRenderer::new(vec![]);
        let service = VoiceService::new_with_renderer(renderer);
        let first_persona = persona("helper_sprite");
        let second_persona = persona("quiet_guardian");
        let first = service.fallback_for("speech:insight", &voice_ctx(&first_persona));
        let second = service.fallback_for("speech:insight", &voice_ctx(&second_persona));

        assert_ne!(first, second);
    }

    #[test]
    fn chat_phrase_bank_line_rejects_raw_secrets() {
        assert!(parse_chat_phrase_bank_line("humor: token=secret sauce parade").is_none());
        assert!(
            parse_chat_phrase_bank_line("bug: Bearer sk-secret123456 left footprints").is_none()
        );
        assert!(
            parse_chat_phrase_bank_line("insight: sk-secret123456 needs a lint broom").is_none()
        );
    }

    #[tokio::test]
    async fn chat_reaction_voice_prompt_guides_humor_without_raw_echo() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer = TestVoiceRenderer::new(vec![Some("tiny gremlin line".to_string())]);
        let service = VoiceService::new_with_renderer(renderer.clone());
        let persona = persona("helper_sprite");
        let ctx = VoiceCtx {
            persona: &persona,
            identity_name: "Pixel",
            pulse_one_liner: "Tests are running".to_string(),
            workflow_id: Some("chat_reaction"),
            workflow_summary: Some("please iterate with token=[REDACTED] and compare options"),
        };

        let line = service
            .render_chat_reaction(gcx, ctx, ChatReactionSpeechIntent::Humor)
            .await;

        assert_eq!(line, "tiny gremlin line");
        let prompts = renderer.prompts();
        let (system, user) = prompts.first().expect("prompt captured");
        assert!(system.contains("Never echo private user text"));
        assert!(system.contains("broad interaction patterns"));
        assert!(system.contains("playfulness=77"));
        assert!(user.contains("chaotic cute Pixel gremlin joke"));
        assert!(user.contains("not the exact text"));
        assert!(!user.contains("token=secret"));
    }

    #[tokio::test]
    async fn chat_reaction_voice_prompt_guides_interaction_insight() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer = TestVoiceRenderer::new(vec![Some("iteration checkpoint".to_string())]);
        let service = VoiceService::new_with_renderer(renderer.clone());
        let persona = persona("helper_sprite");
        let ctx = VoiceCtx {
            persona: &persona,
            identity_name: "Pixel",
            pulse_one_liner: "Tests are running".to_string(),
            workflow_id: Some("chat_reaction"),
            workflow_summary: Some("please tweak and simplify the plan"),
        };

        let line = service
            .render_chat_reaction(gcx, ctx, ChatReactionSpeechIntent::Insight)
            .await;

        assert_eq!(line, "iteration checkpoint");
        let prompts = renderer.prompts();
        let (_, user) = prompts.first().expect("prompt captured");
        assert!(user.contains("interaction-aware comment"));
        assert!(user.contains("asking, iterating, tweaking"));
        assert!(user.contains("do not quote the message"));
    }

    #[tokio::test]
    async fn chat_reaction_voice_prompt_guides_ambient_check_in() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer = TestVoiceRenderer::new(vec![Some("soft companion blip".to_string())]);
        let service = VoiceService::new_with_renderer(renderer.clone());
        let persona = persona("helper_sprite");
        let ctx = VoiceCtx {
            persona: &persona,
            identity_name: "Pixel",
            pulse_one_liner: "Tests are running".to_string(),
            workflow_id: Some("chat_reaction"),
            workflow_summary: Some("please write a hello world example for me"),
        };

        let line = service
            .render_chat_reaction(gcx, ctx, ChatReactionSpeechIntent::Ambient)
            .await;

        assert_eq!(line, "soft companion blip");
        let prompts = renderer.prompts();
        let (_, user) = prompts.first().expect("prompt captured");
        assert!(user.contains("friendly low-noise Pixel gremlin check-in"));
        assert!(user.contains("being present"));
        assert!(user.contains("do not quote the message"));
    }

    #[tokio::test]
    async fn chat_reaction_voice_uses_chat_reaction_limit() {
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;
        let renderer =
            TestVoiceRenderer::new(vec![Some("x".repeat(CHAT_REACTION_VOICE_MAX_CHARS + 40))]);
        let service = VoiceService::new_with_renderer(renderer);
        let persona = persona("helper_sprite");
        let ctx = VoiceCtx {
            persona: &persona,
            identity_name: "Pixel",
            pulse_one_liner: "Tests are running".to_string(),
            workflow_id: Some("chat_reaction"),
            workflow_summary: Some("please write a hello world example for me"),
        };

        let line = service
            .render_chat_reaction(gcx, ctx, ChatReactionSpeechIntent::Ambient)
            .await;

        assert_eq!(line.chars().count(), CHAT_REACTION_VOICE_MAX_CHARS);
        assert!(line.ends_with('…'));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_voice_service_overrides_are_isolated_across_concurrent_tasks() {
        let barrier = Arc::new(Barrier::new(100));
        let mut tasks = Vec::with_capacity(100);

        for _ in 0..100 {
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                let (service, _) = test_voice_service_with_responses(vec![None]);
                let _guard = install_test_voice_service(service.clone()).await;
                barrier.wait().await;
                assert!(Arc::ptr_eq(&voice_service().await, &service));
            }));
        }

        for task in tasks {
            task.await.unwrap();
        }
    }

    #[test]
    fn chat_reaction_voice_request_uses_chat_reaction_token_budget() {
        let persona = persona("helper_sprite");
        let ctx = VoiceCtx {
            persona: &persona,
            identity_name: "Pixel",
            pulse_one_liner: "Tests are running".to_string(),
            workflow_id: Some("chat_reaction"),
            workflow_summary: Some("please write a hello world example for me"),
        };
        let request =
            VoiceRenderRequest::from_ctx(ChatReactionSpeechIntent::Ambient.as_str(), &ctx);

        assert_eq!(request.max_new_tokens(), CHAT_REACTION_VOICE_MAX_CHARS);
    }
}
