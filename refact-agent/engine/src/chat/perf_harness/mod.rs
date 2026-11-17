use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use async_trait::async_trait;
use refact_core::memory_plane::MemoryPlaneRoots;
use refact_core::vecdb_types::{
    EmbeddingModelConfig, SearchResult, VecDbStatus, VecdbRecord, VecdbSearch, VecdbSearchScope,
};
use serde::Serialize;
use tokio::runtime::Builder;
use tokio::sync::Mutex as AMutex;

use crate::app_state::{AppState, AppToolRegistry, FixtureToolFactory, TOOL_CATALOG_SNAPSHOTS_ENV};
use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{ChatContent, ChatMessage, ChatToolCall, ChatToolFunction, ContextEnum};
use crate::chat::tools::{
    process_tool_calls_once, resolve_tool_call_aliases_with_catalog, ToolStepOutcome,
};
use crate::chat::perf_diagnostics::{
    self, MemoryPerfSink, PerfClock, PerfComponent, PerfEvent, PerfOutcome, PerfRecorder,
};
use crate::chat::prepare::build_canonical_openai_tools;
use crate::chat::types::{ChatEvent, ChatSession, DeltaOp, EventEnvelope};
use crate::chat::generation::batch_stream_delta_ops;
use crate::knowledge::enrichment::enrich_messages_with_knowledge;
use crate::chat::trajectories::{
    find_trajectory_path, load_trajectory_for_chat, persist_trajectory_snapshot_with_intent,
    trajectory_snapshot_from_session, TrajectoryWriterRolloutGuard,
};
use crate::chat::trajectory_index::{
    rebuild_trajectory_index_from_disk, upsert_trajectory_index_entry_from_owned_value,
};
use crate::files_correction::canonicalize_normalized_path;
use crate::global_context::SharedGlobalContext;
use crate::tools::tools_description::{
    MatchConfirmDeny, MatchConfirmDenyResult, Tool, ToolDesc, ToolSource, ToolSourceType,
};

pub use refact_perf::perf_harness::{
    AUTO_ENRICHMENT_BENCHMARK_SCHEMA, CONCURRENT_CHAT_BENCHMARK_SCHEMA, FANOUT_BENCHMARK_SCHEMA,
};
pub const TURN_MEMORY_FLEET_CHAT_COUNTS: [usize; 2] = [10, 100];
const FANOUT_HISTORY_MESSAGE_COUNT: usize = 256;
const FANOUT_HISTORY_MESSAGE_BYTES: usize = 4 * 1024;
const FANOUT_DELTA_COUNT: usize = 512;
const FANOUT_ACTIVE_SUBSCRIBER_COUNT: usize = 3;
const FANOUT_EVENT_CHANNEL_CAPACITY: usize = 128;
const FANOUT_SNAPSHOT_RUNS: usize = 8;

fn benchmark_runtime_builder() -> Builder {
    #[cfg(test)]
    {
        Builder::new_current_thread()
    }
    #[cfg(not(test))]
    {
        Builder::new_multi_thread()
    }
}

pub(crate) mod fixtures;
pub(crate) mod metrics;
pub(crate) mod runners;
pub(crate) mod turn_memory;
pub(crate) mod types;
pub(crate) mod validation;

#[cfg(test)]
mod tests;

#[allow(hidden_glob_reexports)]
pub use fixtures::*;
#[allow(hidden_glob_reexports)]
pub use metrics::*;
#[allow(hidden_glob_reexports)]
pub use runners::*;
#[allow(hidden_glob_reexports)]
pub use turn_memory::*;
#[allow(hidden_glob_reexports)]
pub use types::*;
#[allow(hidden_glob_reexports)]
pub use validation::*;
