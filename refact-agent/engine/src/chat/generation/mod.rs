mod preamble;
mod recovery;
mod retry;
mod run_loop;
mod stream;
mod support;

pub use preamble::{
    ensure_session_preamble, prepare_session_preamble_and_knowledge, save_rag_results_to_session,
};
pub use run_loop::start_generation;
pub use stream::{GenerationResult, run_llm_generation};
pub(in crate::chat) use preamble::maybe_save_trajectory_with_intent;
#[allow(unused_imports)]
pub(crate) use recovery::is_high_pressure_length_stop;
#[allow(unused_imports)]
pub(crate) use support::{NormalizedStopReason, batch_stream_delta_ops, normalize_stop_reason};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_more;
