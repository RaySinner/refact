pub use refact_buddy_core::diagnostics::{
    collect_diagnostics_from_error, diagnostic_id, diagnostic_signature, diagnostic_source_bucket,
    unavailable_model_id, DiagnosticContext, DiagnosticSeverity,
};
pub(crate) use refact_buddy_core::diagnostics::classify_error;

pub use refact_runtime_api::buddy_diagnostics::{
    collect_diagnostics, collect_diagnostics_with_model,
};
