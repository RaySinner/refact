mod config;
mod diagnostics;
mod privacy;
mod runner;
mod session;
mod state;
#[cfg(test)]
mod tests;

pub(crate) const MAX_CONTEXT_LIMIT_COMPACT_ATTEMPTS: usize = 1;
pub(crate) const MAX_EMPTY_CHOICE_RETRIES: usize = 2;
pub(crate) const EMPTY_CHOICE_ERROR_PREFIX: &str = "subchat produced no visible content";
pub(crate) const PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS: usize = 2_000;
pub(crate) const PARENT_COMPACTION_DIAGNOSTIC_REDACTION_LOOKAHEAD_CHARS: usize = 512;
pub(crate) const PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED: &str = "\n...[truncated]";
pub(crate) const PARTIAL_OUTPUT_STREAM_ERROR: &str =
    "Stream interrupted after partial output and all retry attempts failed.";
pub(crate) const GUARDED_REPORT_INSTRUCTION: &str = "Guarded file contents may be used to complete the task, but do not quote or reproduce them verbatim in the final report.";

pub use refact_chat_api::{
    ExplicitSubchatSpec, SubchatConfig, SubchatParameters, SubchatProgress, SubchatResult,
    ToolsPolicy, TraceParent, WrapUpConfig,
};

pub use refact_runtime_api::subchat_runtime::{resolve_subchat_model, resolve_subchat_params};
pub(crate) use refact_runtime_api::subchat_runtime::{
    llm_endpoint_unusable_reason, normalize_subchat_params_for_model,
    resolve_subchat_model_for_tool,
};
#[cfg(test)]
pub(crate) use refact_runtime_api::subchat_runtime::parse_subchat_cache_control;

pub use config::{
    resolve_subchat_config,
    resolve_subchat_config_with_parent,
    resolve_subchat_config_with_explicit_params,
    run_subchat_once_with_explicit_params,
};
pub(crate) use config::*;
pub use runner::{
    run_subchat,
    run_subchat_once,
    run_subchat_once_with_abort,
    run_subchat_once_with_parent,
};
pub(crate) use diagnostics::*;
pub(crate) use privacy::*;
pub(crate) use runner::*;
pub(crate) use session::*;
pub(crate) use state::*;

