//! Trajectory-index listing counters.
//!
//! Moved out of chat::trajectory_index so perf_telemetry can read them
//! without a dependency cycle.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum TrajectoryIndexListingCaller {
    ChatApi,
    Sidebar,
    ChatPathLookup,
    StaleEntryCleanup,
    AllTrajectoriesMetadata,
    TaskTrajectoryApi,
    TaskPlannerIdAllocation,
    TaskAgentMonitor,
}

impl TrajectoryIndexListingCaller {
    const ALL: [Self; 8] = [
        Self::ChatApi,
        Self::Sidebar,
        Self::ChatPathLookup,
        Self::StaleEntryCleanup,
        Self::AllTrajectoriesMetadata,
        Self::TaskTrajectoryApi,
        Self::TaskPlannerIdAllocation,
        Self::TaskAgentMonitor,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatApi => "chat_api",
            Self::Sidebar => "sidebar",
            Self::ChatPathLookup => "chat_path_lookup",
            Self::StaleEntryCleanup => "stale_entry_cleanup",
            Self::AllTrajectoriesMetadata => "all_trajectories_metadata",
            Self::TaskTrajectoryApi => "task_trajectory_api",
            Self::TaskPlannerIdAllocation => "task_planner_id_allocation",
            Self::TaskAgentMonitor => "task_agent_monitor",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrajectoryIndexListingCounters {
    pub calls_by_caller: Vec<(&'static str, u64)>,
    pub cache_hits: u64,
    pub index_reads: u64,
    pub directory_scans: u64,
    pub generation_checks: u64,
    pub directory_evictions: u64,
    pub journal_appends: u64,
    pub checkpoint_writes: u64,
}

pub struct TrajectoryIndexListingCounterState {
    calls_by_caller: [AtomicU64; TrajectoryIndexListingCaller::ALL.len()],
    cache_hits: AtomicU64,
    index_reads: AtomicU64,
    directory_scans: AtomicU64,
    generation_checks: AtomicU64,
    directory_evictions: AtomicU64,
    journal_appends: AtomicU64,
    checkpoint_writes: AtomicU64,
}

impl TrajectoryIndexListingCounterState {
    fn new() -> Self {
        Self {
            calls_by_caller: std::array::from_fn(|_| AtomicU64::new(0)),
            cache_hits: AtomicU64::new(0),
            index_reads: AtomicU64::new(0),
            directory_scans: AtomicU64::new(0),
            generation_checks: AtomicU64::new(0),
            directory_evictions: AtomicU64::new(0),
            journal_appends: AtomicU64::new(0),
            checkpoint_writes: AtomicU64::new(0),
        }
    }

    fn record_listing_caller(&self, caller: TrajectoryIndexListingCaller) {
        self.calls_by_caller[caller as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn record_listing_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    fn record_index_read(&self) {
        self.index_reads.fetch_add(1, Ordering::Relaxed);
    }

    fn record_directory_scan(&self) {
        self.directory_scans.fetch_add(1, Ordering::Relaxed);
    }

    fn record_generation_check(&self) {
        self.generation_checks.fetch_add(1, Ordering::Relaxed);
    }

    fn record_directory_eviction(&self) {
        self.directory_evictions.fetch_add(1, Ordering::Relaxed);
    }

    fn record_journal_append(&self) {
        self.journal_appends.fetch_add(1, Ordering::Relaxed);
    }

    fn record_checkpoint_write(&self) {
        self.checkpoint_writes.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> TrajectoryIndexListingCounters {
        TrajectoryIndexListingCounters {
            calls_by_caller: TrajectoryIndexListingCaller::ALL
                .iter()
                .map(|caller| {
                    (
                        caller.as_str(),
                        self.calls_by_caller[*caller as usize].load(Ordering::Relaxed),
                    )
                })
                .collect(),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            index_reads: self.index_reads.load(Ordering::Relaxed),
            directory_scans: self.directory_scans.load(Ordering::Relaxed),
            generation_checks: self.generation_checks.load(Ordering::Relaxed),
            directory_evictions: self.directory_evictions.load(Ordering::Relaxed),
            journal_appends: self.journal_appends.load(Ordering::Relaxed),
            checkpoint_writes: self.checkpoint_writes.load(Ordering::Relaxed),
        }
    }

    fn reset(&self) {
        for counter in &self.calls_by_caller {
            counter.store(0, Ordering::Relaxed);
        }
        self.cache_hits.store(0, Ordering::Relaxed);
        self.index_reads.store(0, Ordering::Relaxed);
        self.directory_scans.store(0, Ordering::Relaxed);
        self.generation_checks.store(0, Ordering::Relaxed);
        self.directory_evictions.store(0, Ordering::Relaxed);
        self.journal_appends.store(0, Ordering::Relaxed);
        self.checkpoint_writes.store(0, Ordering::Relaxed);
    }
}

pub static TRAJECTORY_INDEX_LISTING_COUNTERS: std::sync::OnceLock<TrajectoryIndexListingCounterState> =
    std::sync::OnceLock::new();

#[derive(Clone)]
pub enum TrajectoryIndexListingCounterScope {
    Global,
    Local(Arc<TrajectoryIndexListingCounterState>),
}

pub static GLOBAL_TRAJECTORY_INDEX_LISTING_COUNTER_SCOPE: TrajectoryIndexListingCounterScope =
    TrajectoryIndexListingCounterScope::Global;

pub fn listing_counters() -> &'static TrajectoryIndexListingCounterState {
    TRAJECTORY_INDEX_LISTING_COUNTERS.get_or_init(TrajectoryIndexListingCounterState::new)
}

impl TrajectoryIndexListingCounterScope {
    fn state(&self) -> &TrajectoryIndexListingCounterState {
        match self {
            Self::Global => listing_counters(),
            Self::Local(counters) => counters,
        }
    }

    fn record_listing_caller(&self, caller: TrajectoryIndexListingCaller) {
        self.state().record_listing_caller(caller);
    }

    fn record_listing_cache_hit(&self) {
        self.state().record_listing_cache_hit();
    }

    fn record_index_read(&self) {
        self.state().record_index_read();
    }

    fn record_directory_scan(&self) {
        self.state().record_directory_scan();
    }

    fn record_generation_check(&self) {
        self.state().record_generation_check();
    }

    fn record_directory_eviction(&self) {
        self.state().record_directory_eviction();
    }

    fn record_journal_append(&self) {
        self.state().record_journal_append();
    }

    fn record_checkpoint_write(&self) {
        self.state().record_checkpoint_write();
    }

    fn local() -> Self {
        Self::Local(Arc::new(TrajectoryIndexListingCounterState::new()))
    }

    fn snapshot(&self) -> TrajectoryIndexListingCounters {
        self.state().snapshot()
    }

    fn reset(&self) {
        self.state().reset();
    }

    fn is_global(&self) -> bool {
        matches!(self, Self::Global)
    }
}

pub fn trajectory_index_listing_counters() -> TrajectoryIndexListingCounters {
    listing_counters().snapshot()
}
