use std::sync::Arc;

use refact_core::chat_types::canonical_mode_id;

use crate::global_context::GlobalContext;

/// Validate and canonicalize a mode ID with strict registry existence check.
/// Returns 422-compatible error if mode is invalid or doesn't exist in registry.
pub async fn validate_mode_for_request(
    gcx: Arc<GlobalContext>,
    mode: &str,
) -> Result<String, String> {
    let canonical = canonical_mode_id(mode)?;

    let mode_config = crate::customization_registry_gcx::get_mode_config(gcx, &canonical, None).await;

    if mode_config.is_none() {
        return Err(format!("Mode '{}' does not exist in registry", canonical));
    }

    Ok(canonical)
}
