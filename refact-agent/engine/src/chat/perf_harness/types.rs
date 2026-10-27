use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessMode {
    Quick,
    Soak,
    FullSoak,
}

impl HarnessMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Soak => "soak",
            Self::FullSoak => "full_soak",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcurrentChatWorkload {
    pub id: String,
    pub seed: u64,
    pub chat_count: u8,
    pub history_mib: u8,
    pub tool_descriptors: u16,
    pub same_index_directory_contention: bool,
    pub rapid_same_chat_checkpoints: u64,
}

impl ConcurrentChatWorkload {
    pub fn ci_fixture() -> Self {
        Self::new(1, 1, 10)
    }

    pub fn fixed_matrix() -> Vec<Self> {
        [1, 4, 8, 16, 32]
            .into_iter()
            .flat_map(|chat_count| {
                [1, 10, 50].into_iter().flat_map(move |history_mib| {
                    [10, 50, 200].into_iter().map(move |tool_descriptors| {
                        Self::new(chat_count, history_mib, tool_descriptors)
                    })
                })
            })
            .collect()
    }

    pub(crate) fn new(chat_count: u8, history_mib: u8, tool_descriptors: u16) -> Self {
        let seed = (chat_count as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((history_mib as u64).wrapping_mul(10_007))
            .wrapping_add(tool_descriptors as u64);
        Self {
            id: format!("chats-{chat_count}-history-{history_mib}m-tools-{tool_descriptors}"),
            seed,
            chat_count,
            history_mib,
            tool_descriptors,
            same_index_directory_contention: true,
            rapid_same_chat_checkpoints: RAPID_CHECKPOINTS_PER_CHAT,
        }
    }

    pub fn logical_history_bytes(&self) -> u64 {
        u64::from(self.history_mib) * 1024 * 1024
    }

    pub fn materialized_history_bytes(&self, mode: HarnessMode) -> usize {
        match mode {
            HarnessMode::Quick => usize::try_from(self.logical_history_bytes())
                .unwrap_or(usize::MAX)
                .min(QUICK_HISTORY_BYTES_CAP),
            HarnessMode::Soak | HarnessMode::FullSoak => {
                usize::try_from(self.logical_history_bytes()).unwrap_or(usize::MAX)
            }
        }
    }

    pub fn fixture_signature(&self, mode: HarnessMode) -> String {
        format!(
            "{:016x}",
            stable_hash(&[
                self.seed,
                u64::from(self.chat_count),
                u64::from(self.history_mib),
                u64::from(self.tool_descriptors),
                u64::try_from(self.materialized_history_bytes(mode)).unwrap_or(u64::MAX),
                match mode {
                    HarnessMode::Quick => 1,
                    HarnessMode::Soak => 2,
                    HarnessMode::FullSoak => 3,
                },
            ])
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkOptions {
    pub mode: HarnessMode,
    pub warmup_samples: usize,
    pub measured_samples: usize,
}

impl BenchmarkOptions {
    pub const fn quick() -> Self {
        Self {
            mode: HarnessMode::Quick,
            warmup_samples: 1,
            measured_samples: 3,
        }
    }

    pub const fn soak() -> Self {
        Self {
            mode: HarnessMode::Soak,
            warmup_samples: 1,
            measured_samples: 5,
        }
    }

    pub const fn full_soak() -> Self {
        Self {
            mode: HarnessMode::FullSoak,
            warmup_samples: 1,
            measured_samples: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FullSoakWorkload {
    pub id: String,
    pub chat_count: u8,
    pub active_chats: u8,
    pub background_chats: u8,
    pub history_bytes_per_chat: usize,
}

impl FullSoakWorkload {
    pub fn ci_fixture() -> Self {
        Self::new(1)
    }

    pub fn fixed_matrix() -> Vec<Self> {
        [1, 4, 8, 16, 32].into_iter().map(Self::new).collect()
    }

    pub(crate) fn new(chat_count: u8) -> Self {
        let active_chats = ((chat_count + 1) / 2).max(1);
        Self {
            id: format!("full-soak-{chat_count}-chats"),
            chat_count,
            active_chats,
            background_chats: chat_count.saturating_sub(active_chats),
            history_bytes_per_chat: 4 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FullSoakSubsystemFlags {
    pub chat_sessions: bool,
    pub queue_processors: bool,
    pub trajectory_writer: bool,
    pub trajectory_index_coordinator: bool,
    pub trajectory_watcher: bool,
    pub codegraph: bool,
    pub vecdb_local_backend: bool,
    pub buddy: bool,
    pub agent_monitor: bool,
    pub goal_monitor: bool,
    pub scheduler: bool,
    pub exec_registry: bool,
    pub session_cleanup: bool,
    pub exec_registry_entries: u64,
    pub vecdb_disclosure: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FullSoakRolloutSwitches {
    pub trajectory_writer_enabled: bool,
    pub trajectory_index_coordinator_enabled: bool,
    pub trajectory_watcher_self_write_enabled: bool,
    pub tool_catalog_snapshots_enabled: bool,
    pub vecdb_path_coalescing_enabled: bool,
}

impl FullSoakRolloutSwitches {
    pub(crate) fn as_all_disabled(&self) -> bool {
        !self.trajectory_writer_enabled
            && !self.trajectory_index_coordinator_enabled
            && !self.trajectory_watcher_self_write_enabled
            && !self.tool_catalog_snapshots_enabled
            && !self.vecdb_path_coalescing_enabled
    }

    pub(crate) fn as_all_enabled(&self) -> bool {
        self.trajectory_writer_enabled
            && self.trajectory_index_coordinator_enabled
            && self.trajectory_watcher_self_write_enabled
            && self.tool_catalog_snapshots_enabled
            && self.vecdb_path_coalescing_enabled
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FullSoakCounters {
    pub queue_processors_started: u64,
    pub queue_processors_exited: u64,
    pub queue_notify_wakes: u64,
    pub queue_empty_locks: u64,
    pub queue_lock_contention_events: u64,
    pub tool_execution_wait_events: u64,
    pub tool_execution_wait_us: u64,
    pub stream_deltas: u64,
    pub tool_calls: u64,
    pub sse_events: u64,
    pub trajectory_events: u64,
    pub trajectory_files: u64,
    pub index_writes: u64,
    pub index_bytes_written: u64,
    pub watcher_suppressions: u64,
    pub watcher_replays: u64,
    pub vecdb_enqueues: u64,
    pub vecdb_coalesced_paths: u64,
    pub vecdb_enqueue_requests: u64,
    pub vecdb_pending_unique_paths: u64,
    pub vecdb_processed_paths: u64,
    pub catalog_builds: u64,
    pub catalog_pool_builds: u64,
    pub monitor_scans: u64,
    pub cleanup_scans: u64,
    pub exec_registry_entries: u64,
    pub errors: u64,
    pub ordering_errors: u64,
    pub restore_errors: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FullSoakProcessMetrics {
    pub sample_count: usize,
    pub cpu_time_delta_us: Option<u64>,
    pub rss_baseline_bytes: Option<u64>,
    pub rss_peak_bytes: Option<u64>,
    pub rss_delta_bytes: Option<i64>,
    pub read_bytes_delta: Option<u64>,
    pub write_bytes_delta: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FullSoakVecdbMetrics {
    pub enqueue_requests: u64,
    pub pending_unique_paths: u64,
    pub processed_paths: u64,
    pub amplification_ratio: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FullSoakToolCallStages {
    pub session_extraction_history_clone_latency: LatencySummary,
    pub catalog_pool_acquire_latency: LatencySummary,
    pub alias_resolution_latency: LatencySummary,
    pub confirmation_latency: LatencySummary,
    pub prehooks_latency: LatencySummary,
    pub execution_wait_latency: LatencySummary,
    pub execution_lookup_latency: LatencySummary,
    pub execution_runtime_latency: LatencySummary,
    pub posthooks_latency: LatencySummary,
    pub result_postprocess_privacy_latency: LatencySummary,
    pub session_merge_events_latency: LatencySummary,
    pub checkpoint_scheduling_latency: LatencySummary,
    pub accounted_latency: LatencySummary,
    pub unattributed_latency: LatencySummary,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FullSoakVariantBenchmarkReport {
    pub variant: String,
    pub rollout_switches: FullSoakRolloutSwitches,
    pub subsystems: FullSoakSubsystemFlags,
    pub counters: FullSoakCounters,
    pub queue_wait_latency: LatencySummary,
    pub first_delta_latency: LatencySummary,
    pub checkpoint_return_latency: LatencySummary,
    pub required_flush_latency: LatencySummary,
    pub tool_call_end_to_end_latency: LatencySummary,
    pub tool_call_stages: FullSoakToolCallStages,
    pub sse_serialize_latency: LatencySummary,
    pub sse_emit_latency: LatencySummary,
    pub process_samples: Vec<FullSoakProcessMetrics>,
    pub vecdb_deferred_queue: FullSoakVecdbMetrics,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FullSoakWorkloadBenchmarkReport {
    pub workload: FullSoakWorkload,
    pub variants: Vec<FullSoakVariantBenchmarkReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FullSoakBenchmarkReport {
    pub comparison_label: String,
    pub workloads: Vec<FullSoakWorkloadBenchmarkReport>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BenchmarkCounters {
    pub save_calls: u64,
    pub rapid_checkpoint_saves: u64,
    pub required_commits: u64,
    pub trajectory_files: u64,
    pub measured_files_written: u64,
    pub measured_bytes_written: u64,
    pub index_rebuilds: u64,
    pub catalog_builds: u64,
    pub catalog_tool_descriptors: u64,
    pub catalog_policy_entries: u64,
    pub errors: u64,
}

impl BenchmarkCounters {
    pub(crate) fn operations(&self) -> u64 {
        self.save_calls
            .saturating_add(self.index_rebuilds)
            .saturating_add(self.catalog_builds)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DiagnosticCounters {
    pub trajectory_snapshot: u64,
    pub trajectory_serialize: u64,
    pub trajectory_atomic_write: u64,
    pub trajectory_commit: u64,
    pub trajectory_index_lock_wait: u64,
    pub trajectory_index_read: u64,
    pub trajectory_index_write: u64,
    pub trajectory_index_rebuild: u64,
    pub tool_catalog_build: u64,
}

impl DiagnosticCounters {
    pub(crate) fn count(events: &[PerfEvent], component: PerfComponent) -> u64 {
        events
            .iter()
            .filter(|event| event.component == component.as_str())
            .count() as u64
    }

    pub(crate) fn from_events(events: &[PerfEvent]) -> Self {
        Self {
            trajectory_snapshot: Self::count(events, PerfComponent::TrajectorySnapshot),
            trajectory_serialize: Self::count(events, PerfComponent::TrajectorySerialize),
            trajectory_atomic_write: Self::count(events, PerfComponent::TrajectoryAtomicWrite),
            trajectory_commit: Self::count(events, PerfComponent::TrajectoryCommit),
            trajectory_index_lock_wait: Self::count(events, PerfComponent::TrajectoryIndexLockWait),
            trajectory_index_read: Self::count(events, PerfComponent::TrajectoryIndexRead),
            trajectory_index_write: Self::count(events, PerfComponent::TrajectoryIndexWrite),
            trajectory_index_rebuild: Self::count(events, PerfComponent::TrajectoryIndexRebuild),
            tool_catalog_build: Self::count(events, PerfComponent::ToolCatalogBuild),
        }
    }

    pub(crate) fn add_assign(&mut self, other: &Self) {
        self.trajectory_snapshot += other.trajectory_snapshot;
        self.trajectory_serialize += other.trajectory_serialize;
        self.trajectory_atomic_write += other.trajectory_atomic_write;
        self.trajectory_commit += other.trajectory_commit;
        self.trajectory_index_lock_wait += other.trajectory_index_lock_wait;
        self.trajectory_index_read += other.trajectory_index_read;
        self.trajectory_index_write += other.trajectory_index_write;
        self.trajectory_index_rebuild += other.trajectory_index_rebuild;
        self.tool_catalog_build += other.tool_catalog_build;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LatencySummary {
    pub sample_count: usize,
    pub min_us: u64,
    pub max_us: u64,
    pub mean_us: f64,
    pub variance_us: f64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

impl LatencySummary {
    pub(crate) fn from_samples(samples: &[u64]) -> Result<Self, String> {
        if samples.is_empty() {
            return Err("cannot calculate latency summary without samples".to_string());
        }
        let min_us = *samples.iter().min().expect("non-empty samples");
        let max_us = *samples.iter().max().expect("non-empty samples");
        let mean_us =
            samples.iter().map(|sample| *sample as f64).sum::<f64>() / samples.len() as f64;
        let variance_us = samples
            .iter()
            .map(|sample| {
                let delta = *sample as f64 - mean_us;
                delta * delta
            })
            .sum::<f64>()
            / samples.len() as f64;
        Ok(Self {
            sample_count: samples.len(),
            min_us,
            max_us,
            mean_us,
            variance_us,
            p50_us: percentile_us(samples, 50),
            p95_us: percentile_us(samples, 95),
            p99_us: percentile_us(samples, 99),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FanoutBenchmarkWorkload {
    pub history_message_count: usize,
    pub history_message_bytes: usize,
    pub delta_count: usize,
    pub active_subscriber_count: usize,
    pub lagging_subscriber_count: usize,
    pub event_channel_capacity: usize,
    pub snapshot_runs: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FanoutDeltaMetrics {
    pub deltas_per_second: f64,
    pub operations_per_delta: f64,
    pub bytes_per_delta: f64,
    pub coalesce_window_ms: u64,
    pub baseline_event_count: usize,
    pub coalesced_event_count: usize,
    pub baseline_serialize_cpu_us: u64,
    pub coalesced_serialize_cpu_us: u64,
    pub serialization_cpu_reduction_percent: f64,
    pub projected_first_delta_latency_us: u64,
    pub emit_lock_wait_latency: LatencySummary,
    pub serialize_latency: LatencySummary,
    pub broadcast_latency: LatencySummary,
    pub first_delta_latency: LatencySummary,
    pub serialization_and_broadcast_percent_of_emit_wall_time: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FanoutSnapshotMetrics {
    pub snapshot_count: usize,
    pub clone_latency: LatencySummary,
    pub clone_bytes: LatencySummary,
    pub serialize_latency: LatencySummary,
    pub serialized_bytes: LatencySummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FanoutSubscriberMetrics {
    pub subscriber_count: usize,
    pub active_subscriber_count: usize,
    pub active_received_delta_count: usize,
    pub active_lag_recoveries: u64,
    pub lag_recoveries: u64,
    pub lagged_events: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FanoutBenchmarkReport {
    pub schema: &'static str,
    pub workload: FanoutBenchmarkWorkload,
    pub delta: FanoutDeltaMetrics,
    pub snapshot: FanoutSnapshotMetrics,
    pub subscribers: FanoutSubscriberMetrics,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MachineMetrics {
    pub rss_bytes: Option<u64>,
    pub cpu_percent: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VariantBenchmarkReport {
    pub variant: String,
    pub workload_signature: String,
    pub logical_history_bytes: u64,
    pub materialized_history_bytes: usize,
    pub counters: BenchmarkCounters,
    pub diagnostics: DiagnosticCounters,
    pub snapshot_latency: LatencySummary,
    pub serialize_latency: LatencySummary,
    pub atomic_write_latency: LatencySummary,
    pub commit_latency: LatencySummary,
    pub index_wait_latency: LatencySummary,
    pub index_write_latency: LatencySummary,
    pub catalog_acquisition_latency: LatencySummary,
    pub checkpoint_return_latency: LatencySummary,
    pub background_flush_latency: LatencySummary,
    pub total_operation_latency: LatencySummary,
    pub throughput_operations_per_sec: f64,
    pub machine: MachineMetrics,
}

pub const TOOL_POOL_CHAT_COUNT: u8 = 8;
pub const TOOL_POOL_DESCRIPTOR_COUNT: u16 = 50;
pub(crate) const TOOL_POOL_SAME_NAME_PARALLEL_CALLS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolPoolWorkload {
    pub id: String,
    pub seed: u64,
    pub chat_count: u8,
    pub tool_descriptors: u16,
    pub tool_calls_per_chat: u16,
    pub same_name_parallel_calls: u8,
}

impl ToolPoolWorkload {
    pub fn fixed() -> Self {
        Self {
            id: "turn-tool-pool-8-chats-50-tools".to_string(),
            seed: 0x7100_0000,
            chat_count: TOOL_POOL_CHAT_COUNT,
            tool_descriptors: TOOL_POOL_DESCRIPTOR_COUNT,
            tool_calls_per_chat: TOOL_POOL_DESCRIPTOR_COUNT,
            same_name_parallel_calls: TOOL_POOL_SAME_NAME_PARALLEL_CALLS as u8,
        }
    }

    pub fn fixture_signature(&self) -> String {
        format!(
            "{:016x}",
            stable_hash(&[
                self.seed,
                u64::from(self.chat_count),
                u64::from(self.tool_descriptors),
                u64::from(self.tool_calls_per_chat),
                u64::from(self.same_name_parallel_calls),
            ])
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ToolPoolCounters {
    pub immutable_catalog_builds: u64,
    pub mutable_vector_builds: u64,
    pub parallel_vector_expansions: u64,
    pub confirmation_preflight_starts: u64,
    pub confirmation_preflight_tool_checks: u64,
    pub execution_lookups: u64,
    pub tool_calls: u64,
    pub tool_runtime_calls: u64,
    pub errors: u64,
}

impl ToolPoolCounters {
    pub fn catalog_preflight_operations(&self) -> u64 {
        self.immutable_catalog_builds
            .saturating_add(self.mutable_vector_builds)
            .saturating_add(self.parallel_vector_expansions)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolPoolVariantBenchmarkReport {
    pub variant: String,
    pub workload_signature: String,
    pub counters: ToolPoolCounters,
    pub catalog_acquisition_latency: LatencySummary,
    pub schema_alias_preparation_latency: LatencySummary,
    pub warm_schema_alias_preparation_latency: Option<LatencySummary>,
    pub confirmation_preflight_latency: LatencySummary,
    pub execution_lookup_latency: LatencySummary,
    pub tool_start_overhead_latency: LatencySummary,
    pub tool_runtime_latency: LatencySummary,
    pub machine: MachineMetrics,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolPoolComparison {
    pub legacy_catalog_preflight_operations: u64,
    pub pooled_catalog_preflight_operations: u64,
    pub catalog_preflight_operation_reduction_percent: f64,
    pub tool_start_p95_us: u64,
    pub warm_schema_alias_p95_us: u64,
    pub remaining_stage: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolPoolWorkloadBenchmarkReport {
    pub workload: ToolPoolWorkload,
    pub variants: Vec<ToolPoolVariantBenchmarkReport>,
    pub comparison: ToolPoolComparison,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkloadBenchmarkReport {
    pub workload: ConcurrentChatWorkload,
    pub variants: Vec<VariantBenchmarkReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConcurrentChatBenchmarkReport {
    pub schema: &'static str,
    pub mode: String,
    pub warmup_samples: usize,
    pub measured_samples: usize,
    pub workloads: Vec<WorkloadBenchmarkReport>,
    pub tool_pool_workload: ToolPoolWorkloadBenchmarkReport,
}

pub(crate) struct Sample {
    pub(crate) counters: BenchmarkCounters,
    pub(crate) diagnostics: DiagnosticCounters,
    pub(crate) snapshot_elapsed_us: u64,
    pub(crate) serialize_elapsed_us: u64,
    pub(crate) atomic_write_elapsed_us: u64,
    pub(crate) commit_elapsed_us: u64,
    pub(crate) index_wait_elapsed_us: u64,
    pub(crate) index_write_elapsed_us: u64,
    pub(crate) catalog_elapsed_us: u64,
    pub(crate) checkpoint_return_elapsed_us: u64,
    pub(crate) background_flush_elapsed_us: u64,
    pub(crate) total_elapsed_us: u64,
}

pub(crate) struct ToolPoolSample {
    pub(crate) counters: ToolPoolCounters,
    pub(crate) catalog_acquisition_elapsed_us: Vec<u64>,
    pub(crate) schema_alias_preparation_elapsed_us: Vec<u64>,
    pub(crate) warm_schema_alias_preparation_elapsed_us: Vec<u64>,
    pub(crate) confirmation_preflight_elapsed_us: Vec<u64>,
    pub(crate) execution_lookup_elapsed_us: Vec<u64>,
    pub(crate) tool_runtime_elapsed_us: Vec<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoEnrichmentQueryMode {
    Repeated,
    Distinct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoEnrichmentVecdbMode {
    Warm,
    Cold,
    Empty,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AutoEnrichmentWorkload {
    pub chat_count: usize,
    pub root_count: usize,
    pub knowledge_file_count: usize,
    pub query_mode: AutoEnrichmentQueryMode,
    pub vecdb_mode: AutoEnrichmentVecdbMode,
    pub history_message_count: usize,
    pub privacy_exclusion_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AutoEnrichmentStageReport {
    pub stage: String,
    pub latency: LatencySummary,
    pub fraction_of_accounted_wall_percent: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AutoEnrichmentRepeatedWork {
    pub attempts: u64,
    pub scoped_searches: u64,
    pub fallback_files_read: u64,
    pub embedding_retries: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_coalesced: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AutoEnrichmentWorkloadReport {
    pub workload: AutoEnrichmentWorkload,
    pub end_to_end_latency: LatencySummary,
    pub stages: Vec<AutoEnrichmentStageReport>,
    pub dominant_stages: Vec<String>,
    pub repeated_work: AutoEnrichmentRepeatedWork,
    pub inserted_contexts: u64,
    pub injected_file_count: u64,
    pub injected_char_count: u64,
    pub injected_estimated_tokens: u64,
    pub privacy_exclusion_violations: u64,
    pub max_concurrent_search: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AutoEnrichmentBenchmarkReport {
    pub schema: &'static str,
    pub workloads: Vec<AutoEnrichmentWorkloadReport>,
}
