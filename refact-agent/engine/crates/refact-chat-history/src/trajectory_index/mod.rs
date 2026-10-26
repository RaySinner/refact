use std::collections::{HashMap, HashSet};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex as AMutex;
use uuid::Uuid;

use crate::trajectory_metrics::{
    calculate_line_changes_from_messages, calculate_task_progress_from_messages,
    calculate_token_totals_from_messages, trajectory_meta_title,
};
use crate::trajectory_model::{
    trajectory_list_data_is_displayable_chat, TrajectoryData, TrajectoryListCandidate,
    TrajectoryListData, TrajectoryMeta,
};
use refact_chat_api::{TrajectorySourceIdentity, WorktreeMeta};
use refact_core::file_lock;
use refact_core::trajectory_settings::{self, TrajectoryRuntimeSettings};
use refact_files::path_utils::canonicalize_normalized_path;
use refact_perf::perf_diagnostics::{self, PerfComponent, PerfOutcome};

pub const TRAJECTORY_INDEX_SCHEMA_VERSION: u32 = 1;
pub const TRAJECTORY_INDEX_FILE: &str = "index.json";
pub const TRAJECTORY_INDEX_JOURNAL_FILE: &str = "index.journal.jsonl";
pub const TRAJECTORY_INDEX_JOURNAL_SCHEMA_VERSION: u32 = 1;
pub const TRAJECTORY_INDEX_COORDINATOR_ENV: &str = "REFACT_TRAJECTORY_INDEX_COORDINATOR";
pub const TRAJECTORY_INDEX_LOCK_ORDER: &str =
    "release_global_and_session_locks_before_trajectory_index_io";
pub const TRAJECTORY_INDEX_TMP_SWEEP_MAX_AGE: Duration = Duration::from_secs(10 * 60);
pub use refact_perf::listing_counters::{
    trajectory_index_listing_counters, TrajectoryIndexListingCaller, TrajectoryIndexListingCounters,
};
use refact_perf::listing_counters::{
    listing_counters, TrajectoryIndexListingCounterScope, TrajectoryIndexListingCounterState,
    GLOBAL_TRAJECTORY_INDEX_LISTING_COUNTER_SCOPE,
};

mod coordinator;
mod entries;
mod generation;
mod journal;
mod listing;
mod rebuild;
mod rollout;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_journal;

use coordinator::*;
use entries::*;
use generation::*;
use journal::*;
use listing::*;
use rebuild::*;
use rollout::*;

pub use coordinator::{trajectory_index_coordinator_background_task, TrajectoryIndexCoordinator};
pub use entries::{
    entry_from_trajectory_value, index_entry_file_name_is_valid, read_trajectory_index,
    source_from_hint_or_value, sweep_trajectory_index_tmp_files,
    sweep_trajectory_index_tmp_files_in_roots, trajectory_file_path_for_entry,
    trajectory_index_entry_is_fresh, trajectory_index_journal_path, trajectory_index_path,
    write_trajectory_index_atomic, TrajectoryIndex, TrajectoryIndexEntry,
    TrajectoryIndexSkippedFile, TrajectoryIndexSource,
};
pub use listing::{
    list_candidate_from_entry, list_trajectory_entries_with_rollout,
    list_trajectory_entries_with_rollout_for, meta_from_entry, remove_trajectory_index_entries,
    remove_trajectory_index_entries_with_rollout, remove_trajectory_index_entry,
    remove_trajectory_index_entry_with_rollout, upsert_trajectory_index_entry,
    upsert_trajectory_index_entry_from_owned_value,
    upsert_trajectory_index_entry_from_owned_value_with_rollout,
    upsert_trajectory_index_entry_from_value, upsert_trajectory_index_entry_with_rollout,
};
pub use rebuild::rebuild_trajectory_index_from_disk;
pub use listing::list_trajectory_entries_from_index_or_rebuild;
pub use rollout::{
    trajectory_index_coordinator_rollout_enabled, trajectory_index_coordinator_rollout_enabled_for,
};
