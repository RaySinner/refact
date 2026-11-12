pub use refact_core::call_validation::*;
pub use refact_core::chat_types::ChatModelType;
pub use refact_chat_api::{ChatMeta, SubchatParameters};
pub use refact_core::diff_chunk::DiffChunk;

/// Validate and canonicalize a mode ID with strict registry existence check.
/// Returns 422-compatible error if mode is invalid or doesn't exist in registry.
pub async fn validate_mode_for_request(
    gcx: std::sync::Arc<crate::global_context::GlobalContext>,
    mode: &str,
) -> Result<String, String> {
    let canonical = canonical_mode_id(mode)?;

    let mode_config =
        crate::yaml_configs::customization_registry::get_mode_config(gcx, &canonical, None).await;

    if mode_config.is_none() {
        return Err(format!("Mode '{}' does not exist in registry", canonical));
    }

    Ok(canonical)
}
