use super::*;

pub fn render_json(report: &ConcurrentChatBenchmarkReport) -> Result<String, String> {
    serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize benchmark report: {error}"))
}

pub fn render_full_soak_json(report: &FullSoakBenchmarkReport) -> Result<String, String> {
    serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize full soak benchmark report: {error}"))
}

pub fn render_fanout_json(report: &FanoutBenchmarkReport) -> Result<String, String> {
    serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize fanout benchmark report: {error}"))
}

pub fn render_auto_enrichment_json(
    report: &AutoEnrichmentBenchmarkReport,
) -> Result<String, String> {
    serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize enrichment benchmark report: {error}"))
}

pub fn validate_auto_enrichment_report_json(json: &str) -> Result<(), String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("invalid enrichment benchmark JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "enrichment benchmark JSON must be an object".to_string())?;
    if object.get("schema").and_then(serde_json::Value::as_str)
        != Some(AUTO_ENRICHMENT_BENCHMARK_SCHEMA)
    {
        return Err("enrichment benchmark schema is missing or unsupported".to_string());
    }
    let workloads = object
        .get("workloads")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "enrichment benchmark workloads must be an array".to_string())?;
    let chat_counts = workloads
        .iter()
        .filter_map(|workload| workload.get("workload"))
        .filter_map(|workload| workload.get("chat_count"))
        .filter_map(serde_json::Value::as_u64)
        .collect::<BTreeSet<_>>();
    if !chat_counts.is_superset(&BTreeSet::from([10, 50, 100])) {
        return Err("enrichment benchmark must quantify 10/50/100 concurrent chats".to_string());
    }
    for workload in workloads {
        for key in [
            "workload",
            "end_to_end_latency",
            "stages",
            "dominant_stages",
            "repeated_work",
            "inserted_contexts",
            "injected_file_count",
            "injected_char_count",
            "injected_estimated_tokens",
            "privacy_exclusion_violations",
            "max_concurrent_search",
        ] {
            if workload.get(key).is_none() {
                return Err(format!("enrichment workload is missing {key}"));
            }
        }
        if workload
            .get("privacy_exclusion_violations")
            .and_then(serde_json::Value::as_u64)
            != Some(0)
        {
            return Err("enrichment benchmark has a privacy exclusion violation".to_string());
        }
    }
    Ok(())
}

pub fn validate_report_json(json: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("invalid benchmark JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "benchmark JSON must be an object".to_string())?;
    if object.get("schema").and_then(serde_json::Value::as_str)
        != Some(CONCURRENT_CHAT_BENCHMARK_SCHEMA)
    {
        return Err("benchmark JSON schema is missing or unsupported".to_string());
    }
    for key in [
        "mode",
        "warmup_samples",
        "measured_samples",
        "workloads",
        "tool_pool_workload",
    ] {
        if !object.contains_key(key) {
            return Err(format!("benchmark JSON is missing {key}"));
        }
    }
    let workloads = object
        .get("workloads")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "benchmark JSON workloads must be an array".to_string())?;
    if workloads.is_empty() {
        return Err("benchmark JSON workloads must not be empty".to_string());
    }
    for workload in workloads {
        let workload = workload
            .as_object()
            .ok_or_else(|| "benchmark workload must be an object".to_string())?;
        let variants = workload
            .get("variants")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "benchmark variants must be an array".to_string())?;
        if variants.len() != 2
            || variants[0]
                .get("variant")
                .and_then(serde_json::Value::as_str)
                != Some("legacy")
            || variants[1]
                .get("variant")
                .and_then(serde_json::Value::as_str)
                != Some("coalesced")
        {
            return Err(
                "benchmark workload must include legacy and coalesced variants".to_string(),
            );
        }
        for key in [
            "logical_history_bytes",
            "materialized_history_bytes",
            "counters",
            "diagnostics",
            "snapshot_latency",
            "serialize_latency",
            "atomic_write_latency",
            "commit_latency",
            "index_wait_latency",
            "index_write_latency",
            "catalog_acquisition_latency",
            "checkpoint_return_latency",
            "background_flush_latency",
            "total_operation_latency",
            "machine",
        ] {
            if variants.iter().any(|variant| variant.get(key).is_none()) {
                return Err(format!("benchmark variant is missing {key}"));
            }
        }
    }
    let tool_pool_workload = object
        .get("tool_pool_workload")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "benchmark JSON tool_pool_workload must be an object".to_string())?;
    let variants = tool_pool_workload
        .get("variants")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "benchmark JSON tool pool variants must be an array".to_string())?;
    if variants.len() != 2
        || variants[0]
            .get("variant")
            .and_then(serde_json::Value::as_str)
            != Some("legacy")
        || variants[1]
            .get("variant")
            .and_then(serde_json::Value::as_str)
            != Some("pooled")
    {
        return Err(
            "benchmark tool pool workload must include legacy and pooled variants".to_string(),
        );
    }
    for key in ["workload", "comparison"] {
        if !tool_pool_workload.contains_key(key) {
            return Err(format!("benchmark tool pool workload is missing {key}"));
        }
    }
    for key in [
        "counters",
        "catalog_acquisition_latency",
        "schema_alias_preparation_latency",
        "warm_schema_alias_preparation_latency",
        "confirmation_preflight_latency",
        "execution_lookup_latency",
        "tool_start_overhead_latency",
        "tool_runtime_latency",
        "machine",
    ] {
        if variants.iter().any(|variant| variant.get(key).is_none()) {
            return Err(format!("benchmark tool pool variant is missing {key}"));
        }
    }
    Ok(())
}

pub fn validate_full_soak_report_json(json: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("invalid full soak JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "full soak JSON must be an object".to_string())?;
    if object
        .get("comparison_label")
        .and_then(serde_json::Value::as_str)
        != Some(
            "synthetic same-version legacy rollout comparison; not a historical Wave 0 baseline",
        )
    {
        return Err("full soak comparison label is missing or inaccurate".to_string());
    }
    let workloads = object
        .get("workloads")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "full soak workloads must be an array".to_string())?;
    let chat_counts = workloads
        .iter()
        .filter_map(|workload| workload.get("workload"))
        .filter_map(|workload| workload.get("chat_count"))
        .filter_map(serde_json::Value::as_u64)
        .collect::<BTreeSet<_>>();
    if chat_counts != BTreeSet::from([1, 4, 8, 16, 32]) {
        return Err("full soak report must contain the 1/4/8/16/32 chat matrix".to_string());
    }
    for workload in workloads {
        let variants = workload
            .get("variants")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "full soak variants must be an array".to_string())?;
        if variants.len() != 2
            || variants[0]
                .get("variant")
                .and_then(serde_json::Value::as_str)
                != Some("legacy")
            || variants[1]
                .get("variant")
                .and_then(serde_json::Value::as_str)
                != Some("optimized")
        {
            return Err(
                "full soak workload must include legacy and optimized variants".to_string(),
            );
        }
        for variant in variants {
            for key in [
                "subsystems",
                "rollout_switches",
                "counters",
                "queue_wait_latency",
                "first_delta_latency",
                "checkpoint_return_latency",
                "required_flush_latency",
                "tool_call_end_to_end_latency",
                "tool_call_stages",
                "sse_serialize_latency",
                "sse_emit_latency",
                "process_samples",
                "vecdb_deferred_queue",
            ] {
                if variant.get(key).is_none() {
                    return Err(format!("full soak variant is missing {key}"));
                }
            }
            let rollout_switches = variant
                .get("rollout_switches")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| "full soak rollout switches must be an object".to_string())?;
            for key in [
                "trajectory_writer_enabled",
                "trajectory_index_coordinator_enabled",
                "trajectory_watcher_self_write_enabled",
                "tool_catalog_snapshots_enabled",
                "vecdb_path_coalescing_enabled",
            ] {
                if rollout_switches.get(key).is_none() {
                    return Err(format!("full soak rollout switch is missing {key}"));
                }
            }
            let subsystems = variant
                .get("subsystems")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| "full soak subsystems must be an object".to_string())?;
            for key in [
                "chat_sessions",
                "queue_processors",
                "trajectory_writer",
                "trajectory_index_coordinator",
                "trajectory_watcher",
                "codegraph",
                "vecdb_local_backend",
                "buddy",
                "agent_monitor",
                "goal_monitor",
                "scheduler",
                "exec_registry",
                "exec_registry_entries",
                "session_cleanup",
                "vecdb_disclosure",
            ] {
                if subsystems.get(key).is_none() {
                    return Err(format!("full soak subsystem disclosure is missing {key}"));
                }
            }
            let process_samples = variant
                .get("process_samples")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| "full soak process samples must be an array".to_string())?;
            if process_samples.is_empty()
                || process_samples.iter().any(|sample| {
                    sample
                        .get("sample_count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0)
                        < 2
                })
            {
                return Err(
                    "full soak process samples must include baseline and peak observations"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

pub fn assert_ci_invariants(
    counters: &BenchmarkCounters,
    diagnostics: &DiagnosticCounters,
) -> Result<(), String> {
    if counters.save_calls == 0
        || counters.trajectory_files == 0
        || counters.measured_files_written == 0
    {
        return Err("benchmark did not persist measured trajectory files".to_string());
    }
    if counters.measured_bytes_written == 0 {
        return Err("benchmark did not observe trajectory bytes written".to_string());
    }
    if counters.required_commits == 0 {
        return Err("benchmark did not await a required trajectory commit".to_string());
    }
    if counters.catalog_builds != diagnostics.tool_catalog_build {
        return Err(format!(
            "catalog counter {} does not match observed diagnostics {}",
            counters.catalog_builds, diagnostics.tool_catalog_build
        ));
    }
    if diagnostics.trajectory_atomic_write < counters.required_commits
        || diagnostics.trajectory_serialize < counters.required_commits
    {
        return Err("trajectory diagnostics do not cover required commits".to_string());
    }
    if diagnostics.trajectory_index_write == 0 || diagnostics.trajectory_index_read == 0 {
        return Err("index diagnostics did not observe real index activity".to_string());
    }
    if counters.errors != 0 {
        return Err(format!(
            "benchmark recorded {} operation errors",
            counters.errors
        ));
    }
    Ok(())
}

pub fn workload_matrix_is_complete(workloads: &[ConcurrentChatWorkload]) -> bool {
    let chat_counts = workloads
        .iter()
        .map(|workload| workload.chat_count)
        .collect::<BTreeSet<_>>();
    let history_mib = workloads
        .iter()
        .map(|workload| workload.history_mib)
        .collect::<BTreeSet<_>>();
    let descriptors = workloads
        .iter()
        .map(|workload| workload.tool_descriptors)
        .collect::<BTreeSet<_>>();
    chat_counts == BTreeSet::from([1, 4, 8, 16, 32])
        && history_mib == BTreeSet::from([1, 10, 50])
        && descriptors == BTreeSet::from([10, 50, 200])
        && workloads.len() == 45
        && workloads.iter().all(|workload| {
            workload.same_index_directory_contention
                && workload.rapid_same_chat_checkpoints == RAPID_CHECKPOINTS_PER_CHAT
        })
}

pub(crate) fn aggregate_full_soak_variant(
    variant: &str,
    samples: &[FullSoakSample],
) -> Result<FullSoakVariantBenchmarkReport, String> {
    let first = samples
        .first()
        .ok_or_else(|| "full soak produced no samples".to_string())?;
    let counters = samples
        .iter()
        .fold(FullSoakCounters::default(), |mut total, sample| {
            total.queue_processors_started += sample.counters.queue_processors_started;
            total.queue_processors_exited += sample.counters.queue_processors_exited;
            total.queue_notify_wakes += sample.counters.queue_notify_wakes;
            total.queue_empty_locks += sample.counters.queue_empty_locks;
            total.queue_lock_contention_events += sample.counters.queue_lock_contention_events;
            total.tool_execution_wait_events += sample.counters.tool_execution_wait_events;
            total.tool_execution_wait_us += sample.counters.tool_execution_wait_us;
            total.stream_deltas += sample.counters.stream_deltas;
            total.tool_calls += sample.counters.tool_calls;
            total.sse_events += sample.counters.sse_events;
            total.trajectory_events += sample.counters.trajectory_events;
            total.trajectory_files += sample.counters.trajectory_files;
            total.index_writes += sample.counters.index_writes;
            total.index_bytes_written += sample.counters.index_bytes_written;
            total.watcher_suppressions += sample.counters.watcher_suppressions;
            total.watcher_replays += sample.counters.watcher_replays;
            total.vecdb_enqueues += sample.counters.vecdb_enqueues;
            total.vecdb_coalesced_paths += sample.counters.vecdb_coalesced_paths;
            total.vecdb_enqueue_requests += sample.counters.vecdb_enqueue_requests;
            total.vecdb_pending_unique_paths += sample.counters.vecdb_pending_unique_paths;
            total.vecdb_processed_paths += sample.counters.vecdb_processed_paths;
            total.catalog_builds += sample.counters.catalog_builds;
            total.catalog_pool_builds += sample.counters.catalog_pool_builds;
            total.monitor_scans += sample.counters.monitor_scans;
            total.cleanup_scans += sample.counters.cleanup_scans;
            total.exec_registry_entries += sample.counters.exec_registry_entries;
            total.errors += sample.counters.errors;
            total.ordering_errors += sample.counters.ordering_errors;
            total.restore_errors += sample.counters.restore_errors;
            total
        });
    let flatten = |measure: fn(&FullSoakSample) -> &Vec<u64>| {
        samples
            .iter()
            .flat_map(|sample| measure(sample).iter().copied())
            .collect::<Vec<_>>()
    };
    let stage_samples = samples
        .iter()
        .flat_map(|sample| sample.tool_call_stage_samples.iter())
        .collect::<Vec<_>>();
    let stage_latency = |measure: fn(&FullSoakToolCallStageSample) -> u64| {
        LatencySummary::from_samples(
            &stage_samples
                .iter()
                .map(|sample| measure(sample))
                .collect::<Vec<_>>(),
        )
    };
    let tool_call_end_to_end_us = flatten(|sample| &sample.tool_call_end_to_end_us);
    let accounted_us = stage_samples
        .iter()
        .map(|sample| sample.accounted_us())
        .collect::<Vec<_>>();
    let unattributed_us = tool_call_end_to_end_us
        .iter()
        .zip(accounted_us.iter())
        .map(|(total, accounted)| total.saturating_sub(*accounted))
        .collect::<Vec<_>>();
    let vecdb_deferred_queue = FullSoakVecdbMetrics {
        enqueue_requests: counters.vecdb_enqueue_requests,
        pending_unique_paths: counters.vecdb_pending_unique_paths,
        processed_paths: counters.vecdb_processed_paths,
        amplification_ratio: if counters.vecdb_pending_unique_paths == 0 {
            0.0
        } else {
            counters.vecdb_processed_paths as f64 / counters.vecdb_pending_unique_paths as f64
        },
    };
    Ok(FullSoakVariantBenchmarkReport {
        variant: variant.to_string(),
        rollout_switches: first.rollout_switches.clone(),
        subsystems: first.subsystems.clone(),
        counters,
        queue_wait_latency: LatencySummary::from_samples(&flatten(|sample| &sample.queue_wait_us))?,
        first_delta_latency: LatencySummary::from_samples(&flatten(|sample| {
            &sample.first_delta_us
        }))?,
        checkpoint_return_latency: LatencySummary::from_samples(&flatten(|sample| {
            &sample.checkpoint_return_us
        }))?,
        required_flush_latency: LatencySummary::from_samples(&flatten(|sample| {
            &sample.required_flush_us
        }))?,
        tool_call_end_to_end_latency: LatencySummary::from_samples(&tool_call_end_to_end_us)?,
        tool_call_stages: FullSoakToolCallStages {
            session_extraction_history_clone_latency: stage_latency(|sample| {
                sample.session_extraction_history_clone_us
            })?,
            catalog_pool_acquire_latency: stage_latency(|sample| sample.catalog_pool_acquire_us)?,
            alias_resolution_latency: stage_latency(|sample| sample.alias_resolution_us)?,
            confirmation_latency: stage_latency(|sample| sample.confirmation_us)?,
            prehooks_latency: stage_latency(|sample| sample.prehooks_us)?,
            execution_wait_latency: stage_latency(|sample| sample.execution_wait_us)?,
            execution_lookup_latency: stage_latency(|sample| sample.execution_lookup_us)?,
            execution_runtime_latency: stage_latency(|sample| sample.execution_runtime_us)?,
            posthooks_latency: stage_latency(|sample| sample.posthooks_us)?,
            result_postprocess_privacy_latency: stage_latency(|sample| {
                sample.result_postprocess_privacy_us
            })?,
            session_merge_events_latency: stage_latency(|sample| sample.session_merge_events_us)?,
            checkpoint_scheduling_latency: stage_latency(|sample| sample.checkpoint_scheduling_us)?,
            accounted_latency: LatencySummary::from_samples(&accounted_us)?,
            unattributed_latency: LatencySummary::from_samples(&unattributed_us)?,
        },
        sse_serialize_latency: LatencySummary::from_samples(&flatten(|sample| {
            &sample.sse_serialize_us
        }))?,
        sse_emit_latency: LatencySummary::from_samples(&flatten(|sample| &sample.sse_emit_us))?,
        process_samples: samples
            .iter()
            .map(|sample| sample.process.clone())
            .collect(),
        vecdb_deferred_queue,
    })
}

pub(crate) fn assert_full_soak_invariants(
    counters: &FullSoakCounters,
    subsystems: &FullSoakSubsystemFlags,
) -> Result<(), String> {
    if !subsystems.chat_sessions
        || !subsystems.queue_processors
        || !subsystems.trajectory_watcher
        || !subsystems.codegraph
        || !subsystems.vecdb_local_backend
        || !subsystems.buddy
        || !subsystems.scheduler
        || !subsystems.exec_registry
        || subsystems.exec_registry_entries == 0
    {
        return Err("full soak fixture did not start every required subsystem".to_string());
    }
    if counters.stream_deltas == 0
        || counters.tool_calls == 0
        || counters.trajectory_files == 0
        || counters.sse_events == 0
        || counters.vecdb_enqueues == 0
        || counters.vecdb_enqueue_requests <= counters.vecdb_pending_unique_paths
        || counters.vecdb_processed_paths == 0
        || counters.exec_registry_entries == 0
        || counters.errors != 0
    {
        return Err(
            "full soak fixture did not preserve chat, tool, SSE, or restore invariants".to_string(),
        );
    }
    Ok(())
}

pub(crate) async fn assert_tool_result_order(
    session: &Arc<AMutex<crate::chat::types::ChatSession>>,
    workload: &ToolPoolWorkload,
    chat_index: u8,
) -> Result<(), String> {
    let session = session.lock().await;
    let results = session
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .collect::<Vec<_>>();
    if results.len() != workload.tool_calls_per_chat as usize {
        return Err(format!(
            "tool pool fixture chat {chat_index} returned {} results instead of {}",
            results.len(),
            workload.tool_calls_per_chat
        ));
    }
    for (call_index, result) in results.iter().enumerate() {
        let expected = format!(
            "pool-{}-{chat_index}-{call_index}",
            workload.fixture_signature()
        );
        if result.tool_call_id != expected || result.tool_failed == Some(true) {
            return Err(format!(
                "tool pool fixture chat {chat_index} lost result ordering"
            ));
        }
    }
    Ok(())
}

pub(crate) fn assert_tool_pool_invariants(
    counters: &ToolPoolCounters,
    workload: &ToolPoolWorkload,
) -> Result<(), String> {
    let expected_calls = u64::from(workload.chat_count) * u64::from(workload.tool_calls_per_chat);
    if counters.tool_calls != expected_calls
        || counters.tool_runtime_calls != expected_calls
        || counters.execution_lookups != expected_calls
        || counters.confirmation_preflight_starts != u64::from(workload.chat_count)
        || counters.confirmation_preflight_tool_checks != expected_calls
    {
        return Err(
            "tool pool fixture did not exercise every real preflight and execution path"
                .to_string(),
        );
    }
    if counters.immutable_catalog_builds == 0 || counters.mutable_vector_builds == 0 {
        return Err(
            "tool pool fixture did not acquire real catalog and mutable vectors".to_string(),
        );
    }
    if counters.errors != 0 {
        return Err(format!(
            "tool pool fixture recorded {} errors",
            counters.errors
        ));
    }
    Ok(())
}
