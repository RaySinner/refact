pub use refact_runtime_api::buddy_humor::*;

#[cfg(test)]
mod tests {
    use super::*;

    use crate::buddy::types::BuddyFactKind;

    #[tokio::test]
    async fn humor_generator_routes_through_voice_service() {
        let (service, renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
            vec![Some("tiny joke".to_string())],
        );
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;

        let lines = DefaultHumorGenerator
            .generate(BuddyFactKind::TaskStuck, "tasks:1 stuck:1".to_string(), app)
            .await;

        assert_eq!(lines, vec!["tiny joke".to_string()]);
        assert_eq!(renderer.intent_kinds(), vec!["speech:humor".to_string()]);
    }
}
