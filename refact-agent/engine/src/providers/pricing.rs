pub use refact_runtime_api::providers_pricing_runtime::*;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use refact_providers::traits::ModelPricing;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::caps::model_caps::{model_caps_from_models_dev_catalog, ModelCapabilities};
    use crate::caps::models_dev::load_models_dev_snapshot_catalog;
    use crate::caps::CodeAssistantCaps;
    use crate::providers::traits::CustomModelConfig;

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn caps_with_model_caps(
        model_caps: HashMap<String, ModelCapabilities>,
    ) -> Arc<CodeAssistantCaps> {
        Arc::new(CodeAssistantCaps {
            model_caps: Arc::new(model_caps),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn test_models_dev_custom_pricing_overrides_caps_pricing() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let catalog = load_models_dev_snapshot_catalog().unwrap();
        let model_caps = model_caps_from_models_dev_catalog(&catalog).unwrap();
        let mut provider = crate::providers::create_provider("openai").unwrap();
        provider.add_custom_model(
            "gpt-4o".to_string(),
            CustomModelConfig {
                pricing: Some(ModelPricing {
                    prompt: 101.0,
                    generated: 202.0,
                    cache_read: Some(303.0),
                    cache_creation: Some(404.0),
                    context_over_200k: None,
                }),
                ..Default::default()
            },
        );
        let providers = { gcx.providers.clone() };
        providers.write().await.add(provider);
        let caps_state = gcx.caps_state.clone();
        {
            let mut caps_state = caps_state.write().await;
            caps_state.caps = Some(caps_with_model_caps(model_caps));
            caps_state.last_attempted_ts = now_secs();
        }

        let pricing = lookup_model_pricing(&gcx, "openai/gpt-4o").await.unwrap();

        assert_eq!(pricing.prompt, 101.0);
        assert_eq!(pricing.generated, 202.0);
        assert_eq!(pricing.cache_read, Some(303.0));
        assert_eq!(pricing.cache_creation, Some(404.0));
    }

    #[tokio::test]
    async fn test_models_dev_lookup_pricing_falls_back_to_caps() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let catalog = load_models_dev_snapshot_catalog().unwrap();
        let model_caps = model_caps_from_models_dev_catalog(&catalog).unwrap();
        let caps_state = gcx.caps_state.clone();
        {
            let mut caps_state = caps_state.write().await;
            caps_state.caps = Some(caps_with_model_caps(model_caps));
            caps_state.last_attempted_ts = now_secs();
        }

        let pricing = lookup_model_pricing(&gcx, "openai/gpt-4o").await.unwrap();

        assert!(pricing.is_valid());
        assert!(pricing.prompt > 0.0);
        assert!(pricing.generated > 0.0);
    }
}
