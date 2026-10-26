use super::*;

pub fn percentile_us(samples: &[u64], percentile: u8) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let percentile = usize::from(percentile.min(100));
    let index = ((sorted.len() - 1) * percentile + 99) / 100;
    sorted[index]
}

pub(crate) fn aggregate_tool_pool_counters(samples: &[ToolPoolSample]) -> ToolPoolCounters {
    samples
        .iter()
        .fold(ToolPoolCounters::default(), |mut total, sample| {
            total.immutable_catalog_builds += sample.counters.immutable_catalog_builds;
            total.mutable_vector_builds += sample.counters.mutable_vector_builds;
            total.parallel_vector_expansions += sample.counters.parallel_vector_expansions;
            total.confirmation_preflight_starts += sample.counters.confirmation_preflight_starts;
            total.confirmation_preflight_tool_checks +=
                sample.counters.confirmation_preflight_tool_checks;
            total.execution_lookups += sample.counters.execution_lookups;
            total.tool_calls += sample.counters.tool_calls;
            total.tool_runtime_calls += sample.counters.tool_runtime_calls;
            total.errors += sample.counters.errors;
            total
        })
}

pub(crate) fn tool_pool_elapsed(
    samples: &[ToolPoolSample],
    elapsed: impl Fn(&ToolPoolSample) -> &Vec<u64>,
) -> Vec<u64> {
    samples
        .iter()
        .flat_map(|sample| elapsed(sample).iter().copied())
        .collect()
}

pub(crate) fn event_count(events: &[PerfEvent], component: PerfComponent) -> u64 {
    events
        .iter()
        .filter(|event| event.component == component.as_str())
        .count() as u64
}

pub(crate) fn event_item_count(events: &[PerfEvent], component: PerfComponent) -> u64 {
    events
        .iter()
        .filter(|event| event.component == component.as_str())
        .filter_map(|event| event.item_count)
        .sum()
}

pub(crate) fn event_elapsed(events: &[PerfEvent], component: PerfComponent) -> Vec<u64> {
    events
        .iter()
        .filter(|event| event.component == component.as_str())
        .map(|event| event.elapsed_us)
        .collect()
}

pub(crate) fn event_elapsed_sum(events: &[PerfEvent], component: PerfComponent) -> u64 {
    event_elapsed(events, component).into_iter().sum()
}

pub(crate) fn full_soak_tool_call_stage_sample(
    events: &[PerfEvent],
) -> FullSoakToolCallStageSample {
    let confirmation_us = event_elapsed_sum(events, PerfComponent::ToolConfirmationPreflight);
    FullSoakToolCallStageSample {
        session_extraction_history_clone_us: event_elapsed_sum(
            events,
            PerfComponent::ToolSessionExtraction,
        ),
        catalog_pool_acquire_us: event_elapsed_sum(events, PerfComponent::ToolCatalogPoolAcquire),
        alias_resolution_us: event_elapsed_sum(events, PerfComponent::ToolAliasResolution),
        confirmation_us,
        prehooks_us: event_elapsed_sum(events, PerfComponent::ToolPreHook),
        execution_wait_us: event_elapsed_sum(events, PerfComponent::ToolExecutionWait),
        execution_lookup_us: event_elapsed_sum(events, PerfComponent::ToolExecutionLookup),
        execution_runtime_us: event_elapsed_sum(events, PerfComponent::ToolRuntime),
        posthooks_us: event_elapsed_sum(events, PerfComponent::ToolPostHook),
        result_postprocess_privacy_us: event_elapsed_sum(
            events,
            PerfComponent::ToolResultPostprocess,
        ),
        session_merge_events_us: event_elapsed_sum(events, PerfComponent::ToolSessionMergeEvents),
        checkpoint_scheduling_us: event_elapsed_sum(
            events,
            PerfComponent::ToolCheckpointScheduling,
        ),
    }
}

#[cfg(test)]
pub(crate) fn full_soak_stage_accounting_is_bounded(
    end_to_end_us: u64,
    stages: &FullSoakToolCallStageSample,
) -> bool {
    stages.accounted_us() <= end_to_end_us.saturating_add(stages.execution_wait_us)
}

pub(crate) fn tool_pool_errors(events: &[PerfEvent]) -> u64 {
    events
        .iter()
        .filter(|event| event.outcome == "failure")
        .filter(|event| {
            matches!(
                event.component,
                "tool.catalog_build"
                    | "tool.mutable_vector_build"
                    | "tool.pool_parallel_expansion"
                    | "tool.confirmation_preflight"
                    | "tool.execution_lookup"
                    | "tool.runtime"
            )
        })
        .count() as u64
}

pub(crate) fn aggregate_counters(samples: &[Sample]) -> BenchmarkCounters {
    samples
        .iter()
        .fold(BenchmarkCounters::default(), |mut total, sample| {
            total.save_calls += sample.counters.save_calls;
            total.rapid_checkpoint_saves += sample.counters.rapid_checkpoint_saves;
            total.required_commits += sample.counters.required_commits;
            total.trajectory_files += sample.counters.trajectory_files;
            total.measured_files_written += sample.counters.measured_files_written;
            total.measured_bytes_written += sample.counters.measured_bytes_written;
            total.index_rebuilds += sample.counters.index_rebuilds;
            total.catalog_builds += sample.counters.catalog_builds;
            total.catalog_tool_descriptors += sample.counters.catalog_tool_descriptors;
            total.catalog_policy_entries += sample.counters.catalog_policy_entries;
            total.errors += sample.counters.errors;
            total
        })
}

pub(crate) fn aggregate_diagnostics(samples: &[Sample]) -> DiagnosticCounters {
    samples
        .iter()
        .fold(DiagnosticCounters::default(), |mut total, sample| {
            total.add_assign(&sample.diagnostics);
            total
        })
}

pub(crate) fn latency_for(
    samples: &[Sample],
    elapsed: impl Fn(&Sample) -> u64,
) -> Result<LatencySummary, String> {
    LatencySummary::from_samples(&samples.iter().map(elapsed).collect::<Vec<_>>())
}

pub(crate) fn elapsed_for(events: &[PerfEvent], component: PerfComponent) -> u64 {
    events
        .iter()
        .filter(|event| event.component == component.as_str())
        .map(|event| event.elapsed_us)
        .sum::<u64>()
        .max(1)
}

pub(crate) fn terminal_operation_failures(events: &[PerfEvent]) -> u64 {
    events
        .iter()
        .filter(|event| event.outcome == "failure")
        .filter(|event| {
            matches!(
                event.component,
                "trajectory.serialize"
                    | "trajectory.atomic_write"
                    | "trajectory.commit"
                    | "trajectory.index_rebuild"
                    | "tool.catalog_build"
            )
        })
        .count() as u64
}

pub(crate) fn deterministic_payload(seed: u64, bytes: usize) -> Vec<u8> {
    let mut state = seed;
    let mut payload = Vec::with_capacity(bytes);
    for _ in 0..bytes {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        payload.push(b'a' + ((state >> 32) % 26) as u8);
    }
    payload
}

pub(crate) fn stable_hash(values: &[u64]) -> u64 {
    values.iter().fold(0xcbf2_9ce4_8422_2325, |hash, value| {
        hash.wrapping_mul(0x100_0000_01b3) ^ value
    })
}

pub(crate) fn elapsed_us(started: Instant) -> u64 {
    started
        .elapsed()
        .as_micros()
        .try_into()
        .unwrap_or(u64::MAX)
        .max(1)
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessResourceSnapshot {
    cpu_time_us: Option<u64>,
    rss_bytes: Option<u64>,
    read_bytes: Option<u64>,
    write_bytes: Option<u64>,
}

pub(crate) struct FullSoakProcessSampler {
    stop: Arc<AtomicBool>,
    snapshots: Arc<StdMutex<Vec<ProcessResourceSnapshot>>>,
    task: std::thread::JoinHandle<()>,
}

impl FullSoakProcessSampler {
    pub(crate) fn start() -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let snapshots = Arc::new(StdMutex::new(vec![sample_process_resources()]));
        let task_stop = stop.clone();
        let task_snapshots = snapshots.clone();
        let task = std::thread::Builder::new()
            .name("full-soak-process-sampler".to_string())
            .spawn(move || {
                while !task_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                    if let Ok(mut snapshots) = task_snapshots.lock() {
                        snapshots.push(sample_process_resources());
                    }
                }
            })
            .map_err(|error| format!("failed to start full soak process sampler: {error}"))?;
        Ok(Self {
            stop,
            snapshots,
            task,
        })
    }

    pub(crate) fn finish(self) -> Result<FullSoakProcessMetrics, String> {
        self.stop.store(true, Ordering::Relaxed);
        self.task
            .join()
            .map_err(|_| "full soak process sampler panicked".to_string())?;
        let mut snapshots = self
            .snapshots
            .lock()
            .map_err(|_| "full soak process sampler lock poisoned".to_string())?
            .clone();
        snapshots.push(sample_process_resources());
        Ok(process_metrics_from_snapshots(&snapshots))
    }
}

pub(crate) fn process_metrics_from_snapshots(
    snapshots: &[ProcessResourceSnapshot],
) -> FullSoakProcessMetrics {
    let first = snapshots.first();
    let last = snapshots.last();
    let baseline_rss = first.and_then(|sample| sample.rss_bytes);
    let peak_rss = snapshots.iter().filter_map(|sample| sample.rss_bytes).max();
    FullSoakProcessMetrics {
        sample_count: snapshots.len(),
        cpu_time_delta_us: option_delta(
            first.and_then(|sample| sample.cpu_time_us),
            last.and_then(|sample| sample.cpu_time_us),
        ),
        rss_baseline_bytes: baseline_rss,
        rss_peak_bytes: peak_rss,
        rss_delta_bytes: baseline_rss
            .zip(peak_rss)
            .map(|(first, peak)| peak as i64 - first as i64),
        read_bytes_delta: option_delta(
            first.and_then(|sample| sample.read_bytes),
            last.and_then(|sample| sample.read_bytes),
        ),
        write_bytes_delta: option_delta(
            first.and_then(|sample| sample.write_bytes),
            last.and_then(|sample| sample.write_bytes),
        ),
    }
}

pub(crate) fn option_delta(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    before
        .zip(after)
        .map(|(before, after)| after.saturating_sub(before))
}

pub(crate) fn sample_process_resources() -> ProcessResourceSnapshot {
    let pid = Pid::from_u32(std::process::id());
    let pids = [pid];
    let mut system = System::new();
    let refresh_kind = ProcessRefreshKind::nothing()
        .with_memory()
        .with_disk_usage()
        .without_tasks();
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, refresh_kind);
    match system.process(pid) {
        Some(process) => {
            let usage = process.disk_usage();
            ProcessResourceSnapshot {
                cpu_time_us: read_process_cpu_time_us(),
                rss_bytes: Some(process.memory()),
                read_bytes: Some(usage.total_read_bytes),
                write_bytes: Some(usage.total_written_bytes),
            }
        }
        None => ProcessResourceSnapshot {
            cpu_time_us: None,
            rss_bytes: None,
            read_bytes: None,
            write_bytes: None,
        },
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn read_process_cpu_time_us() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let close = stat.rfind(')')?;
    let fields = stat
        .get(close + 2..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    let user_ticks = fields.get(11)?.parse::<u64>().ok()?;
    let system_ticks = fields.get(12)?.parse::<u64>().ok()?;
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks_per_second <= 0 {
        return None;
    }
    user_ticks
        .saturating_add(system_ticks)
        .checked_mul(1_000_000)?
        .checked_div(ticks_per_second as u64)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn read_process_cpu_time_us() -> Option<u64> {
    None
}

pub(crate) fn sample_machine_metrics() -> MachineMetrics {
    let snapshot = sample_process_resources();
    MachineMetrics {
        rss_bytes: snapshot.rss_bytes,
        cpu_percent: None,
    }
}
