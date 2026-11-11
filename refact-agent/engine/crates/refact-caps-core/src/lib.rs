pub mod caps_state;
pub mod code_assistant_caps;
pub mod identity;
pub mod model_records;
pub mod model_resolve;
pub mod provider_names;
pub mod provider_config;
pub mod providers;

pub use model_resolve::{
    apply_registry_caps_to_chat_model, resolve_chat_model, resolve_model,
    resolve_model_caps_for_provider_model_with_bases,
};
