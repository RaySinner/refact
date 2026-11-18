// The generation loop now lives in `refact_tools::chat_generation`; the child
// modules below are one-line re-export shims so `crate::chat::generation::<child>`
// keeps resolving, and `support` stays local because it is the prelude the
// engine's own test modules reach through `use super::*`.
//
// The tests stay in the engine: they build a real `AppState` from `make_test_gcx`,
// which only the engine can construct.
pub use refact_tools::chat_generation::{
    ensure_session_preamble, maybe_save_trajectory_with_intent,
    prepare_session_preamble_and_knowledge, run_llm_generation, save_rag_results_to_session,
    start_generation, GenerationResult,
};
#[allow(unused_imports)]
pub(crate) use self::support::{batch_stream_delta_ops, normalize_stop_reason, NormalizedStopReason};

pub(crate) mod preamble;
pub(crate) mod recovery;
pub(crate) mod retry;
pub(crate) mod run_loop;
pub(crate) mod stream;
pub(crate) mod support;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_more;
