pub use refact_tools::tools::review_agents::config::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use serde::Deserialize;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::SubchatParameters;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_caps_core::model_resolve::resolve_chat_model;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::{try_load_caps_quickly_if_not_present, GlobalContext};
#[cfg(test)]
#[allow(unused_imports)]
use crate::llm::params::CacheControl;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::ExplicitSubchatSpec;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_types::ReviewDepth;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_settings_defaults_match_the_documented_arguments() {
        let settings = ReviewSettings::default();

        assert_eq!(settings.default_depth, "normal");
        assert_eq!(settings.parallel_depth, 4);
        assert_eq!(settings.variants, 1);
        assert_eq!(settings.idle_timeout_secs, 360);
        assert_eq!(settings.model_slot, ModelSlot::Chat);
    }

    #[test]
    fn review_settings_variants_pick_distinct_slots_in_order() {
        let settings = ReviewSettings::default();

        assert_eq!(settings.slots_for_variants(1), vec![ModelSlot::Chat]);
        assert_eq!(
            settings.slots_for_variants(2),
            vec![ModelSlot::Chat, ModelSlot::Thinking]
        );
        assert_eq!(
            settings.slots_for_variants(3),
            vec![ModelSlot::Chat, ModelSlot::Thinking, ModelSlot::Chat2]
        );
    }

    #[test]
    fn review_settings_parse_from_yaml_with_stage_overrides() {
        let yaml = r#"
default_depth: deep
parallel_depth: 8
variants: 2
idle_timeout_secs: 120
model_slot: thinking
stages:
  execution:
    enabled: false
  mechanical:
    model_slot: light
"#;
        let settings: ReviewSettings = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(settings.default_depth, "deep");
        assert_eq!(settings.parallel_depth, 8);
        assert_eq!(settings.idle_timeout_secs, 120);
        assert_eq!(settings.model_slot, ModelSlot::Thinking);
        assert_eq!(settings.stage_override("execution").enabled, Some(false));
        assert_eq!(
            settings.stage_override("mechanical").model_slot,
            Some(ModelSlot::Light)
        );
        assert!(settings.stage_override("diff").enabled.is_none());
    }

    #[test]
    fn review_stage_spec_is_always_autonomous() {
        let base = SubchatParameters {
            subchat_model_type: refact_core::chat_types::ChatModelType::Default,
            subchat_model: String::new(),
            subchat_n_ctx: 100000,
            subchat_max_new_tokens: 8000,
            subchat_temperature: None,
            subchat_tokens_for_rag: 5000,
            subchat_reasoning_effort: None,
            subchat_cache_control: CacheControl::Off,
        };

        let spec = stage_subchat_spec(&base, &ReviewSettings::default(), "m".to_string());

        assert!(spec.autonomous_no_confirm);
        assert_eq!(spec.params.subchat_tokens_for_rag, 0);
        assert_eq!(spec.model, "m");
    }
}
