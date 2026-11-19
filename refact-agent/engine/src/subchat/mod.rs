mod config;
mod diagnostics;
mod privacy;
mod runner;
mod session;
mod state;
#[cfg(test)]
mod tests;

pub(crate) use refact_runtime_api::subchat_core::{
    is_aborted, subchat_retries_allowed, EMPTY_CHOICE_ERROR_PREFIX,
    MAX_CONTEXT_LIMIT_COMPACT_ATTEMPTS, MAX_EMPTY_CHOICE_RETRIES,
};
#[cfg(test)]
pub(crate) use refact_runtime_api::subchat_core::{
    GUARDED_REPORT_INSTRUCTION, PARENT_COMPACTION_DIAGNOSTIC_MAX_CHARS,
    PARENT_COMPACTION_DIAGNOSTIC_REDACTION_LOOKAHEAD_CHARS, PARENT_COMPACTION_DIAGNOSTIC_TRUNCATED,
    PARTIAL_OUTPUT_STREAM_ERROR,
};

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
