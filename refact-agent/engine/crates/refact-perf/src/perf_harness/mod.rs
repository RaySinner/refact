use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::perf_diagnostics::{PerfComponent, PerfEvent};

pub const CONCURRENT_CHAT_BENCHMARK_SCHEMA: &str = "refact.concurrent_chat_benchmark.v1";
pub const FANOUT_BENCHMARK_SCHEMA: &str = "refact.chat_fanout_benchmark.v1";
pub const AUTO_ENRICHMENT_BENCHMARK_SCHEMA: &str = "refact.auto_enrichment_benchmark.v1";
const QUICK_HISTORY_BYTES_CAP: usize = 8 * 1024;
const RAPID_CHECKPOINTS_PER_CHAT: u64 = 4;

pub mod metrics;
pub mod types;
pub mod validation;

#[allow(hidden_glob_reexports)]
pub use metrics::*;
#[allow(hidden_glob_reexports)]
pub use types::*;
#[allow(hidden_glob_reexports)]
pub use validation::*;
