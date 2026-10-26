use super::*;

#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn percentile_math_uses_nearest_rank_at_tail_percentiles() {
    let samples = [10, 20, 30, 40, 50];
    assert_eq!(percentile_us(&samples, 0), 10);
    assert_eq!(percentile_us(&samples, 50), 30);
    assert_eq!(percentile_us(&samples, 95), 50);
    assert_eq!(percentile_us(&samples, 99), 50);
    assert_eq!(percentile_us(&[], 50), 0);
}

#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn full_soak_process_metrics_use_baseline_delta_and_peak() {
    let metrics = process_metrics_from_snapshots(&[
        ProcessResourceSnapshot {
            cpu_time_us: Some(100),
            rss_bytes: Some(1_000),
            read_bytes: Some(50),
            write_bytes: Some(75),
        },
        ProcessResourceSnapshot {
            cpu_time_us: Some(125),
            rss_bytes: Some(900),
            read_bytes: Some(60),
            write_bytes: Some(80),
        },
        ProcessResourceSnapshot {
            cpu_time_us: Some(160),
            rss_bytes: Some(1_250),
            read_bytes: Some(90),
            write_bytes: Some(110),
        },
    ]);

    assert_eq!(metrics.sample_count, 3);
    assert_eq!(metrics.cpu_time_delta_us, Some(60));
    assert_eq!(metrics.rss_baseline_bytes, Some(1_000));
    assert_eq!(metrics.rss_peak_bytes, Some(1_250));
    assert_eq!(metrics.rss_delta_bytes, Some(250));
    assert_eq!(metrics.read_bytes_delta, Some(40));
    assert_eq!(metrics.write_bytes_delta, Some(35));
}

#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn full_soak_variant_order_alternates() {
    assert_eq!(full_soak_variant_order(0), [false, true]);
    assert_eq!(full_soak_variant_order(1), [true, false]);
    assert_eq!(full_soak_variant_order(2), [false, true]);
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn auto_enrichment_fixture_measures_real_fanout_lock_contention_and_privacy() {
    let report = run_auto_enrichment_ci_fixture().expect("auto enrichment fixture runs");

    assert_eq!(report.workload.chat_count, 10);
    assert_eq!(report.privacy_exclusion_violations, 0);
    assert_eq!(report.repeated_work.attempts, 10);
    assert!(report.repeated_work.scoped_searches <= 1);
    assert!(report.repeated_work.cache_misses <= 1);
    assert!(
        report.repeated_work.cache_hits + report.repeated_work.cache_coalesced >= 9,
        "{:#?}",
        report.repeated_work
    );
    assert!(report.inserted_contexts > 0);
    assert!(report.injected_file_count > 0);
    assert!(report.injected_char_count > 0);
    assert!(report.injected_estimated_tokens > 0);
    assert!(report.max_concurrent_search >= 1);
    assert!(report.max_concurrent_search <= crate::memories::MAX_CONCURRENT_ENRICHMENT_SEARCHES);
    assert!(report
        .stages
        .iter()
        .any(|stage| stage.stage == "enrichment.vecdb_lock_wait"));
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn auto_enrichment_fallback_measurement_stays_bounded_and_schema_validates() {
    let report = benchmark_runtime_builder()
        .enable_all()
        .build()
        .expect("runtime starts")
        .block_on(run_auto_enrichment_workload(&AutoEnrichmentWorkload {
            chat_count: 10,
            root_count: 2,
            knowledge_file_count: 10,
            query_mode: AutoEnrichmentQueryMode::Distinct,
            vecdb_mode: AutoEnrichmentVecdbMode::Unavailable,
            history_message_count: 4,
            privacy_exclusion_count: 1,
        }))
        .expect("fallback fixture runs");
    assert!(report.repeated_work.fallback_files_read <= 100);
    assert_eq!(report.privacy_exclusion_violations, 0);

    let json = render_auto_enrichment_json(&AutoEnrichmentBenchmarkReport {
        schema: AUTO_ENRICHMENT_BENCHMARK_SCHEMA,
        workloads: vec![
            report.clone(),
            AutoEnrichmentWorkloadReport {
                workload: AutoEnrichmentWorkload {
                    chat_count: 50,
                    ..report.workload.clone()
                },
                ..report.clone()
            },
            AutoEnrichmentWorkloadReport {
                workload: AutoEnrichmentWorkload {
                    chat_count: 100,
                    ..report.workload.clone()
                },
                ..report
            },
        ],
    })
    .expect("report serializes");
    validate_auto_enrichment_report_json(&json).expect("report schema validates");
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn auto_enrichment_ten_thousand_file_fallback_reads_no_corpus_files() {
    let report = benchmark_runtime_builder()
        .enable_all()
        .build()
        .expect("runtime starts")
        .block_on(run_auto_enrichment_workload(&AutoEnrichmentWorkload {
            chat_count: 1,
            root_count: 1,
            knowledge_file_count: 10_000,
            query_mode: AutoEnrichmentQueryMode::Repeated,
            vecdb_mode: AutoEnrichmentVecdbMode::Unavailable,
            history_message_count: 4,
            privacy_exclusion_count: 1,
        }))
        .expect("fallback fixture runs");

    assert_eq!(report.repeated_work.fallback_files_read, 0);
    assert_eq!(report.privacy_exclusion_violations, 0);
    assert!(report.inserted_contexts > 0);
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn fanout_fixture_measures_high_rate_deltas_large_history_and_lag_recovery() {
    let report = run_fanout_benchmark().expect("fanout fixture should run");

    assert_eq!(report.schema, FANOUT_BENCHMARK_SCHEMA);
    assert_eq!(
        report.workload.history_message_count,
        FANOUT_HISTORY_MESSAGE_COUNT
    );
    assert_eq!(
        report.workload.history_message_bytes,
        FANOUT_HISTORY_MESSAGE_BYTES
    );
    assert_eq!(report.workload.delta_count, FANOUT_DELTA_COUNT);
    assert_eq!(
        report.subscribers.active_received_delta_count,
        report.workload.delta_count * report.workload.active_subscriber_count
    );
    assert_eq!(report.subscribers.active_lag_recoveries, 0);
    assert_eq!(report.subscribers.lag_recoveries, 1);
    assert!(report.subscribers.lagged_events > 0);
    assert_eq!(
        report.delta.serialize_latency.sample_count,
        FANOUT_DELTA_COUNT
    );
    assert_eq!(
        report.delta.broadcast_latency.sample_count,
        FANOUT_DELTA_COUNT
    );
    assert_eq!(report.delta.first_delta_latency.sample_count, 1);
    assert_eq!(report.delta.baseline_event_count, FANOUT_DELTA_COUNT);
    assert!(report.delta.coalesced_event_count < report.delta.baseline_event_count);
    assert!(report.delta.serialization_cpu_reduction_percent >= 50.0);
    assert!(report.delta.projected_first_delta_latency_us <= 50_000);
    assert_eq!(report.snapshot.snapshot_count, FANOUT_SNAPSHOT_RUNS + 1);
    assert!(report.snapshot.clone_bytes.p95_us > 1_000_000);
    assert!(report.snapshot.serialized_bytes.p95_us > 1_000_000);
}

#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn fixed_seed_workloads_are_repeatable() {
    let first = ConcurrentChatWorkload::fixed_matrix();
    let second = ConcurrentChatWorkload::fixed_matrix();
    assert_eq!(first, second);
    assert_eq!(
        first[0].fixture_signature(HarnessMode::Quick),
        second[0].fixture_signature(HarnessMode::Quick)
    );
}

#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn fixed_tool_pool_workload_has_the_required_concurrency_and_catalog_size() {
    let workload = ToolPoolWorkload::fixed();

    assert_eq!(workload.chat_count, TOOL_POOL_CHAT_COUNT);
    assert_eq!(workload.tool_descriptors, TOOL_POOL_DESCRIPTOR_COUNT);
    assert_eq!(workload.tool_calls_per_chat, TOOL_POOL_DESCRIPTOR_COUNT);
    assert_eq!(
        workload.same_name_parallel_calls as usize,
        TOOL_POOL_SAME_NAME_PARALLEL_CALLS
    );
    assert_eq!(
        deterministic_tool_calls(&workload, 0).len(),
        TOOL_POOL_DESCRIPTOR_COUNT as usize
    );
}

#[serial_test::serial]
#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn tool_pool_fixture_compares_real_legacy_and_pooled_operations() {
    let report = run_tool_pool_ci_fixture().expect("tool pool fixture should run");
    let legacy = &report.variants[0];
    let pooled = &report.variants[1];
    let expected_calls =
        u64::from(report.workload.chat_count) * u64::from(report.workload.tool_calls_per_chat);
    let maximum_expansions = u64::from(report.workload.chat_count)
        * (u64::from(report.workload.same_name_parallel_calls) - 1);

    assert_eq!(legacy.variant, "legacy");
    assert_eq!(pooled.variant, "pooled");
    assert_eq!(legacy.counters.tool_calls, expected_calls);
    assert_eq!(pooled.counters.tool_calls, expected_calls);
    assert_eq!(
        legacy.counters.confirmation_preflight_tool_checks,
        expected_calls
    );
    assert_eq!(
        pooled.counters.confirmation_preflight_tool_checks,
        expected_calls
    );
    assert_eq!(
        pooled.counters.mutable_vector_builds,
        u64::from(report.workload.chat_count)
    );
    assert!(pooled.counters.parallel_vector_expansions <= maximum_expansions);
    assert!(
        report
            .comparison
            .catalog_preflight_operation_reduction_percent
            >= 80.0,
        "{:#?}",
        report.comparison
    );
    assert!(pooled.tool_start_overhead_latency.p95_us < 100_000);
    match report.comparison.remaining_stage.as_deref() {
        None => assert!(report.comparison.warm_schema_alias_p95_us < 1_000),
        Some("warm_schema_alias_preparation") => {
            assert!(report.comparison.warm_schema_alias_p95_us >= 1_000)
        }
        Some(stage) => panic!("unexpected remaining stage: {stage}"),
    }
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn fixed_fixture_repeats_structural_counters_without_requiring_identical_wall_clock() {
    let first = run_ci_fixture().expect("first fixture run succeeds");
    let second = run_ci_fixture().expect("second fixture run succeeds");
    let first_variant = &first.variants[0];
    let second_variant = &second.variants[0];
    assert_eq!(first.variants.len(), 2);
    assert_eq!(second.variants.len(), 2);
    assert_eq!(first.workload, second.workload);
    assert_eq!(first_variant.counters, second_variant.counters);
    assert_eq!(
        first_variant.diagnostics.trajectory_snapshot,
        second_variant.diagnostics.trajectory_snapshot
    );
    assert_eq!(
        first_variant.diagnostics.trajectory_serialize,
        second_variant.diagnostics.trajectory_serialize
    );
    assert_eq!(
        first_variant.diagnostics.trajectory_atomic_write,
        second_variant.diagnostics.trajectory_atomic_write
    );
    assert_eq!(
        first_variant.diagnostics.trajectory_commit,
        second_variant.diagnostics.trajectory_commit
    );
    assert_eq!(
        first_variant.diagnostics.trajectory_index_rebuild,
        second_variant.diagnostics.trajectory_index_rebuild
    );
    assert_eq!(
        first_variant.diagnostics.tool_catalog_build,
        second_variant.diagnostics.tool_catalog_build
    );
    assert!(first_variant.diagnostics.trajectory_index_lock_wait > 0);
    assert!(first_variant.diagnostics.trajectory_index_read > 0);
    assert!(first_variant.diagnostics.trajectory_index_write > 0);
    assert!(second_variant.diagnostics.trajectory_index_lock_wait > 0);
    assert!(second_variant.diagnostics.trajectory_index_read > 0);
    assert!(second_variant.diagnostics.trajectory_index_write > 0);
    assert_ne!(first_variant.total_operation_latency.sample_count, 0);
    assert_ne!(second_variant.total_operation_latency.sample_count, 0);
}

#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn fixed_workload_matrix_covers_every_required_dimension() {
    assert!(workload_matrix_is_complete(
        &ConcurrentChatWorkload::fixed_matrix()
    ));
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn ci_fixture_measures_real_saves_and_diagnostics() {
    let report = run_ci_fixture().expect("CI fixture should run");
    assert_eq!(report.variants.len(), 2);
    let variant = &report.variants[0];
    assert_eq!(variant.variant, "legacy");
    assert!(variant.counters.measured_files_written >= variant.counters.trajectory_files);
    assert!(variant.counters.measured_bytes_written > 0);
    assert!(variant.counters.required_commits > 0);
    assert!(variant.diagnostics.trajectory_atomic_write >= variant.counters.required_commits);
    assert!(variant.diagnostics.trajectory_index_write > 0);
    assert_ci_invariants(&variant.counters, &variant.diagnostics).expect("fixture invariants");
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn repeated_real_saves_scale_observed_save_and_diagnostic_counts() {
    let mut workload = ConcurrentChatWorkload::ci_fixture();
    workload.rapid_same_chat_checkpoints = 7;
    let report = benchmark_runtime_builder()
        .enable_all()
        .build()
        .expect("runtime starts")
        .block_on(run_workload(
            &workload,
            &BenchmarkOptions {
                mode: HarnessMode::Quick,
                warmup_samples: 0,
                measured_samples: 1,
            },
        ))
        .expect("real workload runs");
    let variant = &report.variants[0];
    assert_eq!(variant.counters.save_calls, 9);
    assert!(variant.diagnostics.trajectory_atomic_write >= variant.counters.required_commits);
    assert!(variant.counters.measured_bytes_written > 0);
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn extra_real_operation_changes_measured_counters() {
    let report = run_ci_fixture().expect("CI fixture should run");
    let variant = &report.variants[0];
    let mut diagnostics = variant.diagnostics.clone();
    diagnostics.tool_catalog_build += 1;
    assert!(assert_ci_invariants(&variant.counters, &diagnostics).is_err());
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn report_compares_legacy_and_coalesced_writer_variants() {
    let tool_pool_workload = run_tool_pool_ci_fixture().expect("tool pool fixture should run");
    let report = ConcurrentChatBenchmarkReport {
        schema: CONCURRENT_CHAT_BENCHMARK_SCHEMA,
        mode: HarnessMode::Quick.as_str().to_string(),
        warmup_samples: 0,
        measured_samples: 1,
        workloads: vec![run_ci_fixture().expect("CI fixture should run")],
        tool_pool_workload,
    };
    let json = render_json(&report).expect("report serializes");
    validate_report_json(&json).expect("report schema validates");
    assert!(json.contains("\"legacy\""));
    assert!(json.contains("\"coalesced\""));
    assert!(json.contains("\"pooled\""));
}

#[test]
#[serial_test::serial]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn full_soak_ci_fixture_starts_required_subsystems() {
    let report = run_full_soak_ci_fixture().expect("full soak CI fixture should run");

    assert_eq!(report.workload.chat_count, 1);
    assert_eq!(report.variants.len(), 2);
    assert_eq!(report.variants[0].variant, "legacy");
    assert_eq!(report.variants[1].variant, "optimized");
    assert!(report.variants[0].rollout_switches.as_all_disabled());
    assert!(!report.variants[0].subsystems.trajectory_writer);
    assert!(report.variants[1].rollout_switches.as_all_enabled());
    assert!(report.variants[1].subsystems.trajectory_writer);
    assert_full_soak_invariants(&report.variants[0].counters, &report.variants[0].subsystems)
        .expect("legacy full soak invariants");
    assert_full_soak_invariants(&report.variants[1].counters, &report.variants[1].subsystems)
        .expect("optimized full soak invariants");
    assert!(report.variants[0]
        .subsystems
        .vecdb_disclosure
        .contains("local recording VecDB backend"));
    for variant in &report.variants {
        assert_eq!(
            variant.tool_call_end_to_end_latency.sample_count,
            variant.tool_call_stages.accounted_latency.sample_count
        );
        assert_eq!(
            variant.tool_call_stages.unattributed_latency.sample_count,
            1
        );
        let process = variant
            .process_samples
            .first()
            .expect("full soak process sample");
        assert!(process.sample_count >= 2);
        assert!(process.cpu_time_delta_us.is_some());
        assert!(process.rss_baseline_bytes.is_some());
        assert!(process.rss_peak_bytes.is_some());
        assert!(process.read_bytes_delta.is_some());
        assert!(process.write_bytes_delta.is_some());
        assert!(
            variant.counters.vecdb_enqueue_requests > variant.counters.vecdb_pending_unique_paths
        );
        assert!(variant.counters.vecdb_processed_paths > 0);
        assert_eq!(
            variant.vecdb_deferred_queue.enqueue_requests,
            variant.counters.vecdb_enqueue_requests
        );
        assert!(variant.vecdb_deferred_queue.amplification_ratio >= 1.0);
    }
}

#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn full_soak_stage_accounting_bounds_nested_wall_clock_stages() {
    let stages = FullSoakToolCallStageSample {
        session_extraction_history_clone_us: 3,
        catalog_pool_acquire_us: 4,
        alias_resolution_us: 2,
        confirmation_us: 5,
        prehooks_us: 6,
        execution_wait_us: 30,
        session_merge_events_us: 3,
        checkpoint_scheduling_us: 2,
        ..Default::default()
    };

    assert!(full_soak_stage_accounting_is_bounded(25, &stages));
    assert!(!full_soak_stage_accounting_is_bounded(10, &stages));
}

#[test]
#[cfg_attr(
    not(feature = "perf-tests"),
    ignore = "performance benchmark; run with --features perf-tests"
)]
fn full_soak_report_schema_declares_matrix_and_disclosures() {
    let latency = LatencySummary::from_samples(&[1]).unwrap();
    let variant = FullSoakVariantBenchmarkReport {
        variant: "legacy".to_string(),
        rollout_switches: FullSoakRolloutSwitches {
            trajectory_writer_enabled: false,
            trajectory_index_coordinator_enabled: false,
            trajectory_watcher_self_write_enabled: false,
            tool_catalog_snapshots_enabled: false,
            vecdb_path_coalescing_enabled: false,
        },
        subsystems: FullSoakSubsystemFlags {
            chat_sessions: true,
            queue_processors: true,
            trajectory_writer: false,
            trajectory_index_coordinator: false,
            trajectory_watcher: true,
            codegraph: true,
            vecdb_local_backend: true,
            buddy: true,
            agent_monitor: true,
            goal_monitor: true,
            scheduler: true,
            exec_registry: true,
            session_cleanup: true,
            exec_registry_entries: 1,
            vecdb_disclosure: "local recording VecDB backend".to_string(),
        },
        counters: FullSoakCounters::default(),
        queue_wait_latency: latency.clone(),
        first_delta_latency: latency.clone(),
        checkpoint_return_latency: latency.clone(),
        required_flush_latency: latency.clone(),
        tool_call_end_to_end_latency: latency.clone(),
        tool_call_stages: FullSoakToolCallStages {
            session_extraction_history_clone_latency: latency.clone(),
            catalog_pool_acquire_latency: latency.clone(),
            alias_resolution_latency: latency.clone(),
            confirmation_latency: latency.clone(),
            prehooks_latency: latency.clone(),
            execution_wait_latency: latency.clone(),
            execution_lookup_latency: latency.clone(),
            execution_runtime_latency: latency.clone(),
            posthooks_latency: latency.clone(),
            result_postprocess_privacy_latency: latency.clone(),
            session_merge_events_latency: latency.clone(),
            checkpoint_scheduling_latency: latency.clone(),
            accounted_latency: latency.clone(),
            unattributed_latency: latency.clone(),
        },
        sse_serialize_latency: latency.clone(),
        sse_emit_latency: latency,
        process_samples: vec![FullSoakProcessMetrics {
            sample_count: 2,
            ..Default::default()
        }],
        vecdb_deferred_queue: FullSoakVecdbMetrics::default(),
    };
    let optimized = FullSoakVariantBenchmarkReport {
        variant: "optimized".to_string(),
        rollout_switches: FullSoakRolloutSwitches {
            trajectory_writer_enabled: true,
            trajectory_index_coordinator_enabled: true,
            trajectory_watcher_self_write_enabled: true,
            tool_catalog_snapshots_enabled: true,
            vecdb_path_coalescing_enabled: true,
        },
        subsystems: variant.subsystems.clone(),
        counters: variant.counters.clone(),
        queue_wait_latency: variant.queue_wait_latency.clone(),
        first_delta_latency: variant.first_delta_latency.clone(),
        checkpoint_return_latency: variant.checkpoint_return_latency.clone(),
        required_flush_latency: variant.required_flush_latency.clone(),
        tool_call_end_to_end_latency: variant.tool_call_end_to_end_latency.clone(),
        tool_call_stages: variant.tool_call_stages.clone(),
        sse_serialize_latency: variant.sse_serialize_latency.clone(),
        sse_emit_latency: variant.sse_emit_latency.clone(),
        process_samples: variant.process_samples.clone(),
        vecdb_deferred_queue: variant.vecdb_deferred_queue.clone(),
    };
    let report = FullSoakBenchmarkReport {
        comparison_label:
            "synthetic same-version legacy rollout comparison; not a historical Wave 0 baseline"
                .to_string(),
        workloads: FullSoakWorkload::fixed_matrix()
            .into_iter()
            .map(|workload| FullSoakWorkloadBenchmarkReport {
                workload,
                variants: vec![variant.clone(), optimized.clone()],
            })
            .collect(),
    };

    let json = render_full_soak_json(&report).expect("full soak report serializes");
    validate_full_soak_report_json(&json).expect("full soak schema validates");
}
