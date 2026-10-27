use super::*;

pub fn run_benchmark(options: BenchmarkOptions) -> Result<ConcurrentChatBenchmarkReport, String> {
    if options.mode == HarnessMode::FullSoak {
        return Err(
            "full soak produces a distinct report; use run_full_soak_benchmark".to_string(),
        );
    }
    if options.measured_samples == 0 {
        return Err("measured_samples must be greater than zero".to_string());
    }
    benchmark_runtime_builder()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start benchmark Tokio runtime: {error}"))?
        .block_on(run_benchmark_async(options))
}

async fn run_benchmark_async(
    options: BenchmarkOptions,
) -> Result<ConcurrentChatBenchmarkReport, String> {
    let workloads = ConcurrentChatWorkload::fixed_matrix();
    let mut reports = Vec::with_capacity(workloads.len());
    for workload in workloads {
        reports.push(run_workload(&workload, &options).await?);
    }
    let tool_pool_workload = run_tool_pool_workload(&ToolPoolWorkload::fixed(), &options).await?;
    Ok(ConcurrentChatBenchmarkReport {
        schema: CONCURRENT_CHAT_BENCHMARK_SCHEMA,
        mode: options.mode.as_str().to_string(),
        warmup_samples: options.warmup_samples,
        measured_samples: options.measured_samples,
        workloads: reports,
        tool_pool_workload,
    })
}

pub fn run_ci_fixture() -> Result<WorkloadBenchmarkReport, String> {
    benchmark_runtime_builder()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start benchmark Tokio runtime: {error}"))?
        .block_on(run_workload(
            &ConcurrentChatWorkload::ci_fixture(),
            &BenchmarkOptions {
                mode: HarnessMode::Quick,
                warmup_samples: 0,
                measured_samples: 1,
            },
        ))
}

pub fn run_tool_pool_ci_fixture() -> Result<ToolPoolWorkloadBenchmarkReport, String> {
    benchmark_runtime_builder()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start benchmark Tokio runtime: {error}"))?
        .block_on(run_tool_pool_workload(
            &ToolPoolWorkload::fixed(),
            &BenchmarkOptions {
                mode: HarnessMode::Quick,
                warmup_samples: 0,
                measured_samples: 1,
            },
        ))
}

pub fn run_full_soak_benchmark(
    options: BenchmarkOptions,
) -> Result<FullSoakBenchmarkReport, String> {
    if options.mode != HarnessMode::FullSoak {
        return Err("full soak benchmark requires HarnessMode::FullSoak".to_string());
    }
    if options.measured_samples == 0 {
        return Err("measured_samples must be greater than zero".to_string());
    }
    benchmark_runtime_builder()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start full soak Tokio runtime: {error}"))?
        .block_on(run_full_soak_benchmark_async(options))
}

async fn run_full_soak_benchmark_async(
    options: BenchmarkOptions,
) -> Result<FullSoakBenchmarkReport, String> {
    let workloads = FullSoakWorkload::fixed_matrix();
    let mut reports = Vec::with_capacity(workloads.len());
    for workload in workloads {
        reports.push(run_full_soak_workload(&workload, &options).await?);
    }
    Ok(FullSoakBenchmarkReport {
        comparison_label:
            "synthetic same-version legacy rollout comparison; not a historical Wave 0 baseline"
                .to_string(),
        workloads: reports,
    })
}

pub fn run_full_soak_ci_fixture() -> Result<FullSoakWorkloadBenchmarkReport, String> {
    benchmark_runtime_builder()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start full soak CI Tokio runtime: {error}"))?
        .block_on(run_full_soak_workload(
            &FullSoakWorkload::ci_fixture(),
            &BenchmarkOptions {
                mode: HarnessMode::FullSoak,
                warmup_samples: 0,
                measured_samples: 1,
            },
        ))
}

pub fn run_fanout_benchmark() -> Result<FanoutBenchmarkReport, String> {
    benchmark_runtime_builder()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start fanout Tokio runtime: {error}"))?
        .block_on(run_fanout_benchmark_async())
}

pub fn run_auto_enrichment_benchmark() -> Result<AutoEnrichmentBenchmarkReport, String> {
    benchmark_runtime_builder()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start enrichment Tokio runtime: {error}"))?
        .block_on(run_auto_enrichment_benchmark_async())
}

pub fn run_auto_enrichment_ci_fixture() -> Result<AutoEnrichmentWorkloadReport, String> {
    benchmark_runtime_builder()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start enrichment CI Tokio runtime: {error}"))?
        .block_on(run_auto_enrichment_workload(&AutoEnrichmentWorkload {
            chat_count: 10,
            root_count: 2,
            knowledge_file_count: 10,
            query_mode: AutoEnrichmentQueryMode::Repeated,
            vecdb_mode: AutoEnrichmentVecdbMode::Cold,
            history_message_count: 8,
            privacy_exclusion_count: 1,
        }))
}

async fn run_auto_enrichment_benchmark_async() -> Result<AutoEnrichmentBenchmarkReport, String> {
    let workloads = vec![
        AutoEnrichmentWorkload {
            chat_count: 1,
            root_count: 1,
            knowledge_file_count: 0,
            query_mode: AutoEnrichmentQueryMode::Repeated,
            vecdb_mode: AutoEnrichmentVecdbMode::Unavailable,
            history_message_count: 1,
            privacy_exclusion_count: 0,
        },
        AutoEnrichmentWorkload {
            chat_count: 10,
            root_count: 1,
            knowledge_file_count: 10,
            query_mode: AutoEnrichmentQueryMode::Repeated,
            vecdb_mode: AutoEnrichmentVecdbMode::Warm,
            history_message_count: 8,
            privacy_exclusion_count: 1,
        },
        AutoEnrichmentWorkload {
            chat_count: 10,
            root_count: 2,
            knowledge_file_count: 10,
            query_mode: AutoEnrichmentQueryMode::Distinct,
            vecdb_mode: AutoEnrichmentVecdbMode::Cold,
            history_message_count: 16,
            privacy_exclusion_count: 1,
        },
        AutoEnrichmentWorkload {
            chat_count: 50,
            root_count: 2,
            knowledge_file_count: 10,
            query_mode: AutoEnrichmentQueryMode::Repeated,
            vecdb_mode: AutoEnrichmentVecdbMode::Cold,
            history_message_count: 32,
            privacy_exclusion_count: 1,
        },
        AutoEnrichmentWorkload {
            chat_count: 50,
            root_count: 8,
            knowledge_file_count: 1_000,
            query_mode: AutoEnrichmentQueryMode::Distinct,
            vecdb_mode: AutoEnrichmentVecdbMode::Unavailable,
            history_message_count: 64,
            privacy_exclusion_count: 1,
        },
        AutoEnrichmentWorkload {
            chat_count: 100,
            root_count: 8,
            knowledge_file_count: 1_000,
            query_mode: AutoEnrichmentQueryMode::Repeated,
            vecdb_mode: AutoEnrichmentVecdbMode::Empty,
            history_message_count: 64,
            privacy_exclusion_count: 1,
        },
        AutoEnrichmentWorkload {
            chat_count: 10,
            root_count: 1,
            knowledge_file_count: 10_000,
            query_mode: AutoEnrichmentQueryMode::Repeated,
            vecdb_mode: AutoEnrichmentVecdbMode::Unavailable,
            history_message_count: 8,
            privacy_exclusion_count: 1,
        },
    ];
    let mut reports = Vec::with_capacity(workloads.len());
    for workload in workloads {
        reports.push(run_auto_enrichment_workload(&workload).await?);
    }
    Ok(AutoEnrichmentBenchmarkReport {
        schema: AUTO_ENRICHMENT_BENCHMARK_SCHEMA,
        workloads: reports,
    })
}

pub(crate) async fn run_auto_enrichment_workload(
    workload: &AutoEnrichmentWorkload,
) -> Result<AutoEnrichmentWorkloadReport, String> {
    let _diagnostic_lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK
        .lock()
        .map_err(|_| "enrichment performance recorder lock poisoned".to_string())?;
    let fixture = AutoEnrichmentFixture::new(workload).await?;
    let sink = Arc::new(MemoryPerfSink::new());
    let recorder = Arc::new(PerfRecorder::with_salt(
        Arc::new(BenchmarkClock::default()),
        sink.clone(),
        [47; 32],
    ));
    let _recorder_guard = perf_diagnostics::install_test_recorder(recorder);
    let tasks = (0..workload.chat_count)
        .map(|chat_index| {
            let gcx = fixture.gcx.clone();
            let mut session = ChatSession::new("auto-enrichment-current-chat".to_string());
            session.messages = auto_enrichment_history(workload, chat_index);
            async move {
                let snapshot_started = Instant::now();
                let mut messages = session.messages.clone();
                let snapshot_bytes = messages
                    .iter()
                    .map(|message| {
                        serde_json::to_vec(message)
                            .map(|encoded| encoded.len())
                            .unwrap_or(0)
                    })
                    .sum::<usize>();
                perf_diagnostics::record_enrichment(
                    PerfComponent::EnrichmentSessionSnapshot,
                    "auto-enrichment-current-chat",
                    PerfOutcome::Success,
                    elapsed_us(snapshot_started),
                    Some(snapshot_bytes as u64),
                    Some(messages.len() as u64),
                    Some((snapshot_bytes as u64).saturating_add(3) / 4),
                );
                let started = Instant::now();
                enrich_messages_with_knowledge(
                    gcx,
                    &mut messages,
                    Some("auto-enrichment-current-chat"),
                    true,
                )
                .await;
                (elapsed_us(started), messages)
            }
        })
        .collect::<Vec<_>>();
    let outcomes = futures::future::join_all(tasks).await;
    let end_to_end_samples = outcomes
        .iter()
        .map(|(elapsed, _)| *elapsed)
        .collect::<Vec<_>>();
    let inserted_contexts = outcomes
        .iter()
        .filter(|(_, messages)| {
            messages.iter().any(|message| {
                message.role == "context_file" && message.tool_call_id == "knowledge_enrichment"
            })
        })
        .count() as u64;
    let privacy_exclusion_violations = outcomes
        .iter()
        .flat_map(|(_, messages)| messages)
        .filter(|message| message.role == "context_file")
        .filter_map(|message| match &message.content {
            ChatContent::ContextFiles(files) => Some(files),
            _ => None,
        })
        .flatten()
        .filter(|file| fixture.excluded_paths.contains(Path::new(&file.file_name)))
        .count() as u64;
    if privacy_exclusion_violations != 0 {
        return Err("enrichment fixture injected a current-chat memory".to_string());
    }
    let events = sink.events();
    let stages = enrichment_stage_reports(&events)?;
    let dominant_stages = stages
        .iter()
        .filter(|stage| stage.fraction_of_accounted_wall_percent > 15.0)
        .map(|stage| {
            format!(
                "{} ({:.1}%)",
                stage.stage, stage.fraction_of_accounted_wall_percent
            )
        })
        .collect::<Vec<_>>();
    let repeated_work = AutoEnrichmentRepeatedWork {
        attempts: event_count(&events, PerfComponent::EnrichmentAttempt),
        scoped_searches: event_item_count(&events, PerfComponent::EnrichmentScopedSearch),
        fallback_files_read: event_item_count(&events, PerfComponent::EnrichmentFallback),
        embedding_retries: 0,
        cache_hits: events
            .iter()
            .filter(|event| event.component == PerfComponent::EnrichmentCacheHit.as_str())
            .count() as u64,
        cache_misses: events
            .iter()
            .filter(|event| event.component == PerfComponent::EnrichmentCacheMiss.as_str())
            .count() as u64,
        cache_coalesced: events
            .iter()
            .filter(|event| event.component == PerfComponent::EnrichmentCacheCoalesced.as_str())
            .count() as u64,
    };
    let injected_file_count = events
        .iter()
        .filter(|event| event.component == PerfComponent::EnrichmentInsertion.as_str())
        .filter(|event| event.outcome == PerfOutcome::Success.as_str())
        .filter_map(|event| event.item_count)
        .sum();
    let injected_char_count = events
        .iter()
        .filter(|event| event.component == PerfComponent::EnrichmentInsertion.as_str())
        .filter(|event| event.outcome == PerfOutcome::Success.as_str())
        .filter_map(|event| event.size_bytes)
        .sum();
    let injected_estimated_tokens = events
        .iter()
        .filter(|event| event.component == PerfComponent::EnrichmentInsertion.as_str())
        .filter(|event| event.outcome == PerfOutcome::Success.as_str())
        .filter_map(|event| event.estimated_tokens)
        .sum();
    Ok(AutoEnrichmentWorkloadReport {
        workload: workload.clone(),
        end_to_end_latency: LatencySummary::from_samples(&end_to_end_samples)?,
        stages,
        dominant_stages,
        repeated_work,
        inserted_contexts,
        injected_file_count,
        injected_char_count,
        injected_estimated_tokens,
        privacy_exclusion_violations,
        max_concurrent_search: fixture
            .vecdb
            .as_ref()
            .map(|backend| backend.max_concurrent_searches.load(Ordering::SeqCst))
            .unwrap_or(0),
    })
}

pub(crate) fn auto_enrichment_history(
    workload: &AutoEnrichmentWorkload,
    chat_index: usize,
) -> Vec<ChatMessage> {
    let mut messages = (0..workload.history_message_count.saturating_sub(1))
        .map(|index| ChatMessage {
            role: if index % 2 == 0 {
                "user".to_string()
            } else {
                "assistant".to_string()
            },
            content: ChatContent::SimpleText("history enrichment codegraph ".repeat(16)),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let suffix = match workload.query_mode {
        AutoEnrichmentQueryMode::Repeated => "shared".to_string(),
        AutoEnrichmentQueryMode::Distinct => format!("chat-{chat_index}"),
    };
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: ChatContent::SimpleText(format!("find enrichment codegraph {suffix}")),
        ..Default::default()
    });
    messages
}

pub(crate) fn enrichment_stage_reports(
    events: &[PerfEvent],
) -> Result<Vec<AutoEnrichmentStageReport>, String> {
    let mut samples_by_stage = BTreeMap::<String, Vec<u64>>::new();
    for event in events
        .iter()
        .filter(|event| event.component.starts_with("enrichment."))
    {
        samples_by_stage
            .entry(event.component.to_string())
            .or_default()
            .push(event.elapsed_us);
    }
    let accounted_us = samples_by_stage
        .values()
        .flatten()
        .copied()
        .sum::<u64>()
        .max(1);
    samples_by_stage
        .into_iter()
        .map(|(stage, samples)| {
            let stage_us = samples.iter().copied().sum::<u64>();
            Ok(AutoEnrichmentStageReport {
                stage,
                latency: LatencySummary::from_samples(&samples)?,
                fraction_of_accounted_wall_percent: stage_us as f64 * 100.0 / accounted_us as f64,
            })
        })
        .collect()
}

async fn run_fanout_benchmark_async() -> Result<FanoutBenchmarkReport, String> {
    let _diagnostic_lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK
        .lock()
        .map_err(|_| "fanout performance recorder lock poisoned".to_string())?;
    let sink = Arc::new(MemoryPerfSink::new());
    let recorder = Arc::new(PerfRecorder::with_salt(
        Arc::new(BenchmarkClock::default()),
        sink.clone(),
        [31; 32],
    ));
    let _recorder_guard = perf_diagnostics::install_test_recorder(recorder);
    let workload = FanoutBenchmarkWorkload {
        history_message_count: FANOUT_HISTORY_MESSAGE_COUNT,
        history_message_bytes: FANOUT_HISTORY_MESSAGE_BYTES,
        delta_count: FANOUT_DELTA_COUNT,
        active_subscriber_count: FANOUT_ACTIVE_SUBSCRIBER_COUNT,
        lagging_subscriber_count: 1,
        event_channel_capacity: FANOUT_EVENT_CHANNEL_CAPACITY,
        snapshot_runs: FANOUT_SNAPSHOT_RUNS,
    };
    let (event_tx, _) = tokio::sync::broadcast::channel(workload.event_channel_capacity);
    let mut session = ChatSession::new("fanout-benchmark".to_string());
    session.event_tx = event_tx;
    session.messages = (0..workload.history_message_count)
        .map(|index| ChatMessage {
            message_id: format!("history-{index}"),
            role: if index % 2 == 0 {
                "user".to_string()
            } else {
                "assistant".to_string()
            },
            content: ChatContent::SimpleText("h".repeat(workload.history_message_bytes)),
            ..Default::default()
        })
        .collect();
    let session = Arc::new(AMutex::new(session));
    {
        let mut locked = session.lock().await;
        locked
            .start_stream()
            .ok_or_else(|| "fanout fixture could not start stream".to_string())?;
    }
    let mut active_receivers = {
        let locked = session.lock().await;
        (0..workload.active_subscriber_count)
            .map(|_| locked.subscribe())
            .collect::<Vec<_>>()
    };
    let mut lagging_receiver = session.lock().await.subscribe();
    let diagnostic_start = sink.events().len();
    let mut emit_lock_wait_us = Vec::with_capacity(workload.delta_count);
    let mut emit_wall_us = Vec::with_capacity(workload.delta_count);
    let mut active_received_delta_count = 0usize;
    let emit_started = Instant::now();
    for index in 0..workload.delta_count {
        let lock_started = Instant::now();
        let mut locked = session.lock().await;
        emit_lock_wait_us.push(elapsed_us(lock_started));
        let emit_started_at = Instant::now();
        locked.emit_stream_delta(vec![DeltaOp::AppendContent {
            text: format!("d{index:04}"),
        }]);
        emit_wall_us.push(elapsed_us(emit_started_at));
        drop(locked);
        for receiver in &mut active_receivers {
            active_received_delta_count += drain_active_delta_events(receiver)?;
        }
    }
    let total_emit_elapsed = emit_started.elapsed();
    let delta_events = sink.events()[diagnostic_start..].to_vec();
    let baseline_event_count = workload.delta_count;
    let coalesced_event_count = batch_stream_delta_ops(
        (0..workload.delta_count)
            .map(|index| DeltaOp::AppendContent {
                text: format!("d{index:04}"),
            })
            .collect(),
    )
    .len();
    let baseline_serialize_cpu_us = measure_delta_serialize_cpu_us(baseline_event_count, false)?;
    let coalesced_serialize_cpu_us = measure_delta_serialize_cpu_us(baseline_event_count, true)?;
    let projected_first_delta_latency_us = measure_first_delta_emit_latency_us()?;
    let mut snapshot_clone_us = Vec::with_capacity(workload.snapshot_runs + 1);
    let mut snapshot_clone_bytes = Vec::with_capacity(workload.snapshot_runs + 1);
    let mut snapshot_serialize_us = Vec::with_capacity(workload.snapshot_runs + 1);
    let mut snapshot_serialized_bytes = Vec::with_capacity(workload.snapshot_runs + 1);
    for _ in 0..workload.snapshot_runs {
        let sample = capture_fanout_snapshot(&session).await?;
        snapshot_clone_us.push(sample.clone_us);
        snapshot_clone_bytes.push(sample.clone_bytes);
        snapshot_serialize_us.push(sample.serialize_us);
        snapshot_serialized_bytes.push(sample.serialized_bytes);
    }
    let (lag_recoveries, lagged_events) = match lagging_receiver.try_recv() {
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
            let (mut recovered_receiver, recovery_snapshot) = {
                let locked = session.lock().await;
                let recovered_receiver = locked.subscribe();
                let recovery_seq = locked.event_seq;
                let clone_started = Instant::now();
                let snapshot = locked.snapshot();
                let clone_us = elapsed_us(clone_started);
                (
                    recovered_receiver,
                    capture_fanout_snapshot_from_parts(
                        locked.chat_id.clone(),
                        recovery_seq,
                        snapshot,
                        clone_us,
                    )?,
                )
            };
            snapshot_clone_us.push(recovery_snapshot.clone_us);
            snapshot_clone_bytes.push(recovery_snapshot.clone_bytes);
            snapshot_serialize_us.push(recovery_snapshot.serialize_us);
            snapshot_serialized_bytes.push(recovery_snapshot.serialized_bytes);
            let recovery_seq = {
                let mut locked = session.lock().await;
                let next_seq = locked.event_seq + 1;
                locked.emit(ChatEvent::PauseCleared {});
                next_seq
            };
            let json = recovered_receiver.try_recv().map_err(|error| {
                format!("fanout recovery receiver did not receive next event: {error}")
            })?;
            let envelope: EventEnvelope = serde_json::from_str(&json)
                .map_err(|error| format!("fanout recovery event was not valid JSON: {error}"))?;
            if envelope.seq != recovery_seq {
                return Err(format!(
                    "fanout recovery sequence regressed: expected {recovery_seq}, got {}",
                    envelope.seq
                ));
            }
            (1, skipped as u64)
        }
        Ok(_) => return Err("fanout lagging subscriber did not lag".to_string()),
        Err(error) => return Err(format!("fanout lagging subscriber failed: {error}")),
    };
    let serialize_us = event_elapsed(&delta_events, PerfComponent::SseSerialize);
    let broadcast_us = event_elapsed(&delta_events, PerfComponent::SseBroadcast);
    let bytes = delta_events
        .iter()
        .filter(|event| event.component == PerfComponent::SseSerialize.as_str())
        .filter_map(|event| event.size_bytes)
        .collect::<Vec<_>>();
    let first_delta_us = event_elapsed(&delta_events, PerfComponent::StreamFirstDelta);
    if serialize_us.len() != workload.delta_count
        || broadcast_us.len() != workload.delta_count
        || bytes.len() != workload.delta_count
        || first_delta_us.len() != 1
    {
        return Err("fanout diagnostics did not record every emitted delta".to_string());
    }
    let serialized_and_broadcast_us = serialize_us
        .iter()
        .chain(broadcast_us.iter())
        .copied()
        .sum::<u64>();
    let emit_wall_total_us = emit_wall_us.iter().copied().sum::<u64>();
    let snapshot = FanoutSnapshotMetrics {
        snapshot_count: snapshot_clone_us.len(),
        clone_latency: LatencySummary::from_samples(&snapshot_clone_us)?,
        clone_bytes: LatencySummary::from_samples(&snapshot_clone_bytes)?,
        serialize_latency: LatencySummary::from_samples(&snapshot_serialize_us)?,
        serialized_bytes: LatencySummary::from_samples(&snapshot_serialized_bytes)?,
    };
    Ok(FanoutBenchmarkReport {
        schema: FANOUT_BENCHMARK_SCHEMA,
        workload: workload.clone(),
        delta: FanoutDeltaMetrics {
            deltas_per_second: workload.delta_count as f64
                / total_emit_elapsed.as_secs_f64().max(f64::MIN_POSITIVE),
            operations_per_delta: 1.0,
            bytes_per_delta: bytes.iter().copied().sum::<u64>() as f64
                / workload.delta_count as f64,
            coalesce_window_ms: 10,
            baseline_event_count,
            coalesced_event_count,
            baseline_serialize_cpu_us,
            coalesced_serialize_cpu_us,
            serialization_cpu_reduction_percent: if baseline_serialize_cpu_us == 0 {
                0.0
            } else {
                (1.0 - coalesced_serialize_cpu_us as f64 / baseline_serialize_cpu_us as f64) * 100.0
            },
            projected_first_delta_latency_us,
            emit_lock_wait_latency: LatencySummary::from_samples(&emit_lock_wait_us)?,
            serialize_latency: LatencySummary::from_samples(&serialize_us)?,
            broadcast_latency: LatencySummary::from_samples(&broadcast_us)?,
            first_delta_latency: LatencySummary::from_samples(&first_delta_us)?,
            serialization_and_broadcast_percent_of_emit_wall_time: if emit_wall_total_us == 0 {
                0.0
            } else {
                serialized_and_broadcast_us as f64 * 100.0 / emit_wall_total_us as f64
            },
        },
        snapshot,
        subscribers: FanoutSubscriberMetrics {
            subscriber_count: workload.active_subscriber_count + workload.lagging_subscriber_count,
            active_subscriber_count: workload.active_subscriber_count,
            active_received_delta_count,
            active_lag_recoveries: 0,
            lag_recoveries,
            lagged_events,
        },
    })
}

pub(crate) fn measure_delta_serialize_cpu_us(
    event_count: usize,
    coalesced: bool,
) -> Result<u64, String> {
    let ops = (0..event_count)
        .map(|index| DeltaOp::AppendContent {
            text: format!("d{index:04}"),
        })
        .collect::<Vec<_>>();
    let batches = if coalesced {
        batch_stream_delta_ops(ops)
    } else {
        ops.into_iter().map(|op| vec![op]).collect()
    };
    let started = Instant::now();
    for (index, ops) in batches.into_iter().enumerate() {
        serde_json::to_string(&EventEnvelope {
            chat_id: "fanout-benchmark".to_string(),
            seq: index as u64 + 1,
            event: ChatEvent::StreamDelta {
                message_id: "fanout-draft".to_string(),
                ops,
            },
        })
        .map_err(|error| format!("fanout delta serialization failed: {error}"))?;
    }
    Ok(elapsed_us(started))
}

pub(crate) fn measure_first_delta_emit_latency_us() -> Result<u64, String> {
    let (event_tx, _) = tokio::sync::broadcast::channel(1);
    let mut session = ChatSession::new("fanout-first-delta".to_string());
    session.event_tx = event_tx;
    session
        .start_stream()
        .ok_or_else(|| "fanout fixture could not start first-delta stream".to_string())?;
    let started = Instant::now();
    session.emit_stream_delta(vec![DeltaOp::AppendContent {
        text: "first".to_string(),
    }]);
    Ok(elapsed_us(started))
}

pub(crate) struct FanoutSnapshotSample {
    clone_us: u64,
    clone_bytes: u64,
    serialize_us: u64,
    serialized_bytes: u64,
}

async fn capture_fanout_snapshot(
    session: &Arc<AMutex<ChatSession>>,
) -> Result<FanoutSnapshotSample, String> {
    let (chat_id, seq, snapshot, clone_us) = {
        let locked = session.lock().await;
        let clone_started = Instant::now();
        let snapshot = locked.snapshot();
        (
            locked.chat_id.clone(),
            locked.event_seq,
            snapshot,
            elapsed_us(clone_started),
        )
    };
    capture_fanout_snapshot_from_parts(chat_id, seq, snapshot, clone_us)
}

pub(crate) fn capture_fanout_snapshot_from_parts(
    chat_id: String,
    seq: u64,
    snapshot: ChatEvent,
    clone_us: u64,
) -> Result<FanoutSnapshotSample, String> {
    let clone_bytes = snapshot_message_bytes(&snapshot)?;
    let serialize_started = Instant::now();
    let serialized = serde_json::to_string(&EventEnvelope {
        chat_id,
        seq,
        event: snapshot,
    })
    .map_err(|error| format!("fanout snapshot serialization failed: {error}"))?;
    Ok(FanoutSnapshotSample {
        clone_us,
        clone_bytes,
        serialize_us: elapsed_us(serialize_started),
        serialized_bytes: serialized.len() as u64,
    })
}

pub(crate) fn snapshot_message_bytes(snapshot: &ChatEvent) -> Result<u64, String> {
    let ChatEvent::Snapshot { messages, .. } = snapshot else {
        return Err("fanout fixture expected snapshot event".to_string());
    };
    messages.iter().try_fold(0u64, |total, message| {
        serde_json::to_vec(message)
            .map(|encoded| total.saturating_add(encoded.len() as u64))
            .map_err(|error| format!("fanout snapshot message serialization failed: {error}"))
    })
}

pub(crate) fn drain_active_delta_events(
    receiver: &mut tokio::sync::broadcast::Receiver<Arc<String>>,
) -> Result<usize, String> {
    let mut received = 0usize;
    loop {
        match receiver.try_recv() {
            Ok(json) => {
                let envelope: EventEnvelope = serde_json::from_str(&json)
                    .map_err(|error| format!("fanout active event was not valid JSON: {error}"))?;
                if matches!(envelope.event, ChatEvent::StreamDelta { .. }) {
                    received += 1;
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => return Ok(received),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                return Err(format!(
                    "fanout active subscriber lagged by {skipped} events"
                ));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                return Err("fanout active subscriber channel closed".to_string());
            }
        }
    }
}

async fn run_full_soak_workload(
    workload: &FullSoakWorkload,
    options: &BenchmarkOptions,
) -> Result<FullSoakWorkloadBenchmarkReport, String> {
    let mut legacy_samples = Vec::with_capacity(options.measured_samples);
    let mut optimized_samples = Vec::with_capacity(options.measured_samples);
    for sample_index in 0..options.warmup_samples {
        for optimized in full_soak_variant_order(sample_index) {
            let sample = run_full_soak_sample(workload, optimized).await?;
            assert_full_soak_invariants(&sample.counters, &sample.subsystems)?;
        }
    }
    for sample_index in 0..options.measured_samples {
        for optimized in full_soak_variant_order(sample_index) {
            let sample = run_full_soak_sample(workload, optimized).await?;
            assert_full_soak_invariants(&sample.counters, &sample.subsystems)?;
            if optimized {
                optimized_samples.push(sample);
            } else {
                legacy_samples.push(sample);
            }
        }
    }
    Ok(FullSoakWorkloadBenchmarkReport {
        workload: workload.clone(),
        variants: vec![
            aggregate_full_soak_variant("legacy", &legacy_samples)?,
            aggregate_full_soak_variant("optimized", &optimized_samples)?,
        ],
    })
}

pub(crate) fn full_soak_variant_order(sample_index: usize) -> [bool; 2] {
    if sample_index % 2 == 0 {
        [false, true]
    } else {
        [true, false]
    }
}

pub(crate) struct FullSoakSample {
    pub(crate) rollout_switches: FullSoakRolloutSwitches,
    pub(crate) subsystems: FullSoakSubsystemFlags,
    pub(crate) counters: FullSoakCounters,
    pub(crate) queue_wait_us: Vec<u64>,
    pub(crate) first_delta_us: Vec<u64>,
    pub(crate) checkpoint_return_us: Vec<u64>,
    pub(crate) required_flush_us: Vec<u64>,
    pub(crate) tool_call_end_to_end_us: Vec<u64>,
    pub(crate) tool_call_stage_samples: Vec<FullSoakToolCallStageSample>,
    pub(crate) sse_serialize_us: Vec<u64>,
    pub(crate) sse_emit_us: Vec<u64>,
    pub(crate) process: FullSoakProcessMetrics,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FullSoakToolCallStageSample {
    pub(crate) session_extraction_history_clone_us: u64,
    pub(crate) catalog_pool_acquire_us: u64,
    pub(crate) alias_resolution_us: u64,
    pub(crate) confirmation_us: u64,
    pub(crate) prehooks_us: u64,
    pub(crate) execution_wait_us: u64,
    pub(crate) execution_lookup_us: u64,
    pub(crate) execution_runtime_us: u64,
    pub(crate) posthooks_us: u64,
    pub(crate) result_postprocess_privacy_us: u64,
    pub(crate) session_merge_events_us: u64,
    pub(crate) checkpoint_scheduling_us: u64,
}

impl FullSoakToolCallStageSample {
    pub(crate) fn accounted_us(&self) -> u64 {
        self.session_extraction_history_clone_us
            .saturating_add(self.catalog_pool_acquire_us)
            .saturating_add(self.alias_resolution_us)
            .saturating_add(self.confirmation_us)
            .saturating_add(self.prehooks_us)
            .saturating_add(self.execution_wait_us)
            .saturating_add(self.session_merge_events_us)
            .saturating_add(self.checkpoint_scheduling_us)
    }
}

async fn run_full_soak_sample(
    workload: &FullSoakWorkload,
    optimized: bool,
) -> Result<FullSoakSample, String> {
    let _diagnostic_lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK
        .lock()
        .map_err(|_| "full soak performance recorder lock poisoned".to_string())?;
    let _env = FullSoakEnvGuard::set(optimized);
    let _writer = TrajectoryWriterRolloutGuard::set(optimized);
    let fixture = FullSoakFixture::new(TOOL_POOL_DESCRIPTOR_COUNT as usize).await?;
    fixture
        .vecdb
        .begin_sample(refact_vecdb::vdb_thread::vecdb_path_coalescing_rollout_enabled())?;
    let sink = Arc::new(MemoryPerfSink::new());
    let recorder = Arc::new(PerfRecorder::with_salt(
        Arc::new(BenchmarkClock::default()),
        sink.clone(),
        [29; 32],
    ));
    let _recorder_guard = perf_diagnostics::install_test_recorder(recorder);
    let process_sampler = FullSoakProcessSampler::start()?;
    let mut trajectory_rx = fixture.base.app.chat.trajectory_events_tx.subscribe();
    let index_before = filesystem_snapshot(&fixture.base.workspace).await?;
    let exec_snapshot = fixture
        .base
        .gcx
        .exec_registry
        .register(
            crate::exec::ExecProcessMeta::new(
                crate::exec::ExecMode::Foreground,
                "full-soak-local".to_string(),
            ),
            1024,
        )
        .await;
    let mut sessions = Vec::with_capacity(workload.chat_count as usize);
    for chat_index in 0..workload.chat_count {
        let chat_id = format!("full-soak-{}-{chat_index}", workload.id);
        let session = crate::chat::get_or_create_session_with_trajectory(
            fixture.base.app.chat_ctx(),
            &fixture.base.app.chat.sessions,
            &chat_id,
        )
        .await;
        let mut sse_rx = session.lock().await.subscribe();
        let checkpoint_started = Instant::now();
        {
            let mut locked = session.lock().await;
            locked.thread.model = "benchmark-local".to_string();
            locked.thread.mode = "agent".to_string();
            locked.thread.include_project_info = false;
            locked.thread.auto_enrichment_enabled = Some(false);
            let request = crate::chat::types::CommandRequest {
                client_request_id: format!("full-soak-goal-{chat_index}"),
                priority: false,
                command: crate::chat::types::ChatCommand::SetGoal {
                    content: format!("full soak chat {chat_index}"),
                    criteria: None,
                    budget: None,
                },
            };
            locked.enqueue_accepted_command(request);
            let _ = locked.start_stream();
            locked.emit_stream_delta(vec![crate::chat::types::DeltaOp::AppendContent {
                text: format!("delta-{chat_index}"),
            }]);
            locked.finish_stream(None);
        }
        let processor_running = session.lock().await.queue_processor_running.clone();
        if !processor_running.swap(true, Ordering::SeqCst) {
            tokio::spawn(crate::chat::process_command_queue(
                fixture.base.app.clone(),
                session.clone(),
                processor_running,
            ));
        }
        let queue_drained = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let queue_empty = session.lock().await.command_queue.is_empty();
                if queue_empty {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await;
        if queue_drained.is_err() {
            return Err(format!(
                "full soak queue did not drain for chat {chat_index}"
            ));
        }
        let checkpoint_return = elapsed_us(checkpoint_started);
        let required_started = Instant::now();
        crate::chat::trajectories::maybe_save_trajectory_with_intent(
            fixture.base.app.clone(),
            session.clone(),
            crate::chat::types::TrajectoryCommitIntent::Required,
        )
        .await;
        let required_flush = elapsed_us(required_started);
        enqueue_repeated_vecdb_paths(&fixture, &session).await?;
        let catalog = fixture
            .base
            .app
            .tool_registry
            .acquire_tool_catalog("agent", Some("benchmark-local"), None)
            .await;
        {
            let mut locked = session.lock().await;
            locked.tool_catalog = Some(catalog);
            let mut assistant = ChatMessage::new("assistant".to_string(), String::new());
            assistant.tool_calls = Some(vec![ChatToolCall {
                id: format!("full-soak-tool-{chat_index}"),
                index: Some(0),
                function: ChatToolFunction {
                    name: "benchmark_tool_0".to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }]);
            locked.add_message(assistant);
        }
        let tool_event_start = sink.events().len();
        let tool_started = Instant::now();
        let tool_outcome = process_tool_calls_once(
            fixture.base.app.clone(),
            session.clone(),
            "agent",
            Some("benchmark-local"),
        )
        .await;
        if !matches!(tool_outcome, ToolStepOutcome::Continue) {
            return Err(format!(
                "full soak tool execution did not continue for chat {chat_index}"
            ));
        }
        let tool_end_to_end = elapsed_us(tool_started);
        let tool_stage_sample =
            full_soak_tool_call_stage_sample(&sink.events()[tool_event_start..]);
        let mut sse_events = 0;
        while sse_rx.try_recv().is_ok() {
            sse_events += 1;
        }
        sessions.push((
            session,
            checkpoint_return,
            required_flush,
            tool_end_to_end,
            tool_stage_sample,
            sse_events,
        ));
    }
    fixture
        .base
        .gcx
        .trajectory_index_coordinator
        .flush_all()
        .await?;
    fixture.vecdb.drain_deferred()?;
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    let index_after = filesystem_snapshot(&fixture.base.workspace).await?;
    let (_, index_bytes_written) = filesystem_delta(&index_before, &index_after);
    let trajectory_files = count_trajectory_files(&fixture.base.workspace).await?;
    let mut trajectory_events = 0;
    while trajectory_rx.try_recv().is_ok() {
        trajectory_events += 1;
    }
    let vecdb_metrics = fixture.vecdb.metrics();
    let mut counters = FullSoakCounters {
        stream_deltas: u64::from(workload.chat_count),
        tool_calls: u64::from(workload.chat_count),
        sse_events: sessions
            .iter()
            .map(|(_, _, _, _, _, events)| *events as u64)
            .sum(),
        trajectory_events,
        trajectory_files,
        index_writes: event_count(&sink.events(), PerfComponent::TrajectoryIndexWrite),
        index_bytes_written,
        vecdb_enqueues: vecdb_metrics.enqueue_requests,
        vecdb_coalesced_paths: vecdb_metrics.pending_unique_paths as u64,
        vecdb_enqueue_requests: vecdb_metrics.enqueue_requests,
        vecdb_pending_unique_paths: vecdb_metrics.pending_unique_paths as u64,
        vecdb_processed_paths: vecdb_metrics.processed_paths,
        catalog_builds: event_count(&sink.events(), PerfComponent::ToolCatalogBuild),
        catalog_pool_builds: event_count(&sink.events(), PerfComponent::ToolMutableVectorBuild),
        monitor_scans: 3,
        cleanup_scans: 1,
        exec_registry_entries: fixture
            .base
            .gcx
            .exec_registry
            .list(Default::default())
            .await
            .len() as u64,
        ..Default::default()
    };
    let mut checkpoint_return_us = Vec::new();
    let mut required_flush_us = Vec::new();
    let mut tool_call_end_to_end_us = Vec::new();
    let mut tool_call_stage_samples = Vec::new();
    for (session, checkpoint, required, tool, stages, _) in &sessions {
        checkpoint_return_us.push(*checkpoint);
        required_flush_us.push(*required);
        tool_call_end_to_end_us.push(*tool);
        tool_call_stage_samples.push(stages.clone());
        let (
            chat_id,
            tool_messages,
            queue_processors_started,
            queue_processors_exited,
            queue_notify_wakes,
            queue_empty_locks,
        ) = {
            let locked = session.lock().await;
            (
                locked.chat_id.clone(),
                locked
                    .messages
                    .iter()
                    .filter(|message| message.role == "tool")
                    .count(),
                locked
                    .queue_processor_counters
                    .processor_starts
                    .load(Ordering::Relaxed),
                locked
                    .queue_processor_counters
                    .processor_exits
                    .load(Ordering::Relaxed),
                locked
                    .queue_processor_counters
                    .notify_wakes
                    .load(Ordering::Relaxed),
                locked
                    .queue_processor_counters
                    .empty_locks
                    .load(Ordering::Relaxed),
            )
        };
        counters.queue_processors_started += queue_processors_started;
        counters.queue_processors_exited += queue_processors_exited;
        counters.queue_notify_wakes += queue_notify_wakes;
        counters.queue_empty_locks += queue_empty_locks;
        if tool_messages != 1 {
            counters.ordering_errors += 1;
        }
        if crate::chat::trajectories::load_trajectory_for_chat(fixture.base.gcx.clone(), &chat_id)
            .await
            .is_none()
        {
            counters.restore_errors += 1;
        }
    }
    let events = sink.events();
    let queue_wait_us = event_elapsed(&events, PerfComponent::CommandQueueWait);
    counters.tool_execution_wait_events = event_count(&events, PerfComponent::ToolExecutionWait);
    counters.tool_execution_wait_us = event_elapsed_sum(&events, PerfComponent::ToolExecutionWait);
    let first_delta_us = event_elapsed(&events, PerfComponent::StreamFirstDelta);
    let sse_serialize_us = event_elapsed(&events, PerfComponent::SseSerialize);
    let sse_emit_us = event_elapsed(&events, PerfComponent::SseBroadcast);
    counters.errors = terminal_operation_failures(&events)
        .saturating_add(counters.ordering_errors)
        .saturating_add(counters.restore_errors);
    let subsystems = FullSoakSubsystemFlags {
        chat_sessions: true,
        queue_processors: counters.queue_processors_started > 0,
        trajectory_writer: crate::chat::trajectories::trajectory_writer_rollout_enabled(),
        trajectory_index_coordinator:
            crate::chat::trajectory_index::trajectory_index_coordinator_rollout_enabled(),
        trajectory_watcher: true,
        codegraph: fixture.base.gcx.codegraph.lock().await.is_some(),
        vecdb_local_backend: true,
        buddy: fixture.base.gcx.buddy.lock().await.is_some(),
        agent_monitor: true,
        goal_monitor: true,
        scheduler: true,
        exec_registry: true,
        session_cleanup: true,
        exec_registry_entries: u64::from(!exec_snapshot.meta.process_id.0.is_empty()),
        vecdb_disclosure:
            "local recording VecDB backend; production VecDB initialization requires embedding credentials and is not run in this no-network fixture"
                .to_string(),
    };
    let sample = FullSoakSample {
        rollout_switches: FullSoakRolloutSwitches {
            trajectory_writer_enabled: crate::chat::trajectories::trajectory_writer_rollout_enabled(
            ),
            trajectory_index_coordinator_enabled:
                crate::chat::trajectory_index::trajectory_index_coordinator_rollout_enabled(),
            trajectory_watcher_self_write_enabled:
                crate::chat::trajectories::trajectory_watcher_self_write_rollout_enabled(),
            tool_catalog_snapshots_enabled: crate::app_state::tool_catalog_snapshot_rollout_enabled(
            ),
            vecdb_path_coalescing_enabled:
                refact_vecdb::vdb_thread::vecdb_path_coalescing_rollout_enabled(),
        },
        subsystems,
        counters,
        queue_wait_us: if queue_wait_us.is_empty() {
            vec![1]
        } else {
            queue_wait_us
        },
        first_delta_us: if first_delta_us.is_empty() {
            vec![1]
        } else {
            first_delta_us
        },
        checkpoint_return_us,
        required_flush_us,
        tool_call_end_to_end_us,
        tool_call_stage_samples,
        sse_serialize_us: if sse_serialize_us.is_empty() {
            vec![1]
        } else {
            sse_serialize_us
        },
        sse_emit_us: if sse_emit_us.is_empty() {
            vec![1]
        } else {
            sse_emit_us
        },
        process: process_sampler.finish()?,
    };
    fixture.shutdown().await;
    Ok(sample)
}

async fn enqueue_repeated_vecdb_paths(
    fixture: &FullSoakFixture,
    session: &Arc<AMutex<ChatSession>>,
) -> Result<(), String> {
    let chat_id = session.lock().await.chat_id.clone();
    let path = find_trajectory_path(fixture.base.gcx.clone(), &chat_id)
        .await
        .ok_or_else(|| "full soak trajectory path was unavailable for VecDB fixture".to_string())?
        .to_string_lossy()
        .into_owned();
    let roots = crate::indexing_routing::memory_plane_roots(fixture.base.gcx.clone()).await;
    let vecdb = fixture
        .base
        .gcx
        .vec_db
        .lock()
        .await
        .clone()
        .ok_or_else(|| "full soak VecDB fixture was unavailable".to_string())?;
    vecdb
        .vectorizer_enqueue_files(&[path.clone(), path], false, roots)
        .await;
    Ok(())
}

pub(crate) async fn run_workload(
    workload: &ConcurrentChatWorkload,
    options: &BenchmarkOptions,
) -> Result<WorkloadBenchmarkReport, String> {
    let variants = [("legacy", false), ("coalesced", true)];
    let mut reports = Vec::with_capacity(variants.len());
    for (variant_name, writer_enabled) in variants {
        reports.push(run_variant_workload(workload, options, variant_name, writer_enabled).await?);
    }
    Ok(WorkloadBenchmarkReport {
        workload: workload.clone(),
        variants: reports,
    })
}

async fn run_variant_workload(
    workload: &ConcurrentChatWorkload,
    options: &BenchmarkOptions,
    variant_name: &str,
    writer_enabled: bool,
) -> Result<VariantBenchmarkReport, String> {
    for _ in 0..options.warmup_samples {
        let sample = run_sample(workload, options.mode, writer_enabled).await?;
        assert_ci_invariants(&sample.counters, &sample.diagnostics)?;
    }

    let sample_futures = (0..options.measured_samples)
        .map(|_| run_sample(workload, options.mode, writer_enabled))
        .collect::<Vec<_>>();
    let mut samples = Vec::with_capacity(sample_futures.len());
    for sample in sample_futures {
        samples.push(sample.await?);
    }
    let first_counters = samples
        .first()
        .map(|sample| sample.counters.clone())
        .ok_or_else(|| "benchmark produced no samples".to_string())?;
    let first_diagnostics = samples
        .first()
        .map(|sample| sample.diagnostics.clone())
        .ok_or_else(|| "benchmark produced no diagnostics".to_string())?;
    assert_ci_invariants(&first_counters, &first_diagnostics)?;
    let counters = aggregate_counters(&samples);
    let diagnostics = aggregate_diagnostics(&samples);
    let throughput_operations_per_sec = counters.operations() as f64;

    let mean_total_us = samples
        .iter()
        .map(|sample| sample.total_elapsed_us as f64)
        .sum::<f64>()
        / samples.len() as f64;
    let variant = VariantBenchmarkReport {
        variant: variant_name.to_string(),
        workload_signature: workload.fixture_signature(options.mode),
        logical_history_bytes: workload.logical_history_bytes(),
        materialized_history_bytes: workload.materialized_history_bytes(options.mode),
        counters,
        diagnostics,
        snapshot_latency: latency_for(&samples, |sample| sample.snapshot_elapsed_us)?,
        serialize_latency: latency_for(&samples, |sample| sample.serialize_elapsed_us)?,
        atomic_write_latency: latency_for(&samples, |sample| sample.atomic_write_elapsed_us)?,
        commit_latency: latency_for(&samples, |sample| sample.commit_elapsed_us)?,
        index_wait_latency: LatencySummary::from_samples(
            &samples
                .iter()
                .map(|sample| sample.index_wait_elapsed_us)
                .collect::<Vec<_>>(),
        )?,
        index_write_latency: LatencySummary::from_samples(
            &samples
                .iter()
                .map(|sample| sample.index_write_elapsed_us)
                .collect::<Vec<_>>(),
        )?,
        catalog_acquisition_latency: LatencySummary::from_samples(
            &samples
                .iter()
                .map(|sample| sample.catalog_elapsed_us)
                .collect::<Vec<_>>(),
        )?,
        checkpoint_return_latency: latency_for(&samples, |sample| {
            sample.checkpoint_return_elapsed_us
        })?,
        background_flush_latency: latency_for(&samples, |sample| {
            sample.background_flush_elapsed_us
        })?,
        total_operation_latency: LatencySummary::from_samples(
            &samples
                .iter()
                .map(|sample| sample.total_elapsed_us)
                .collect::<Vec<_>>(),
        )?,
        throughput_operations_per_sec: throughput_operations_per_sec
            / (mean_total_us / 1_000_000.0).max(0.000_001),
        machine: sample_machine_metrics(),
    };
    Ok(variant)
}

async fn run_tool_pool_workload(
    workload: &ToolPoolWorkload,
    options: &BenchmarkOptions,
) -> Result<ToolPoolWorkloadBenchmarkReport, String> {
    let mut variants = Vec::with_capacity(2);
    for (variant, snapshots_enabled) in [("legacy", false), ("pooled", true)] {
        variants.push(run_tool_pool_variant(workload, options, variant, snapshots_enabled).await?);
    }
    let legacy = variants
        .iter()
        .find(|variant| variant.variant == "legacy")
        .ok_or_else(|| "tool pool benchmark omitted legacy variant".to_string())?;
    let pooled = variants
        .iter()
        .find(|variant| variant.variant == "pooled")
        .ok_or_else(|| "tool pool benchmark omitted pooled variant".to_string())?;
    let legacy_operations = legacy.counters.catalog_preflight_operations();
    let pooled_operations = pooled.counters.catalog_preflight_operations();
    let reduction_percent = if legacy_operations == 0 {
        0.0
    } else {
        (1.0 - pooled_operations as f64 / legacy_operations as f64) * 100.0
    };
    let tool_start_p95_us = pooled.tool_start_overhead_latency.p95_us;
    let warm_schema_alias_p95_us = pooled
        .warm_schema_alias_preparation_latency
        .as_ref()
        .map(|latency| latency.p95_us)
        .unwrap_or(u64::MAX);
    let remaining_stage = if reduction_percent < 80.0 {
        Some("catalog_preflight_operations".to_string())
    } else if tool_start_p95_us >= 100_000 {
        Some("tool_start_overhead".to_string())
    } else if warm_schema_alias_p95_us >= 1_000 {
        Some("warm_schema_alias_preparation".to_string())
    } else {
        None
    };
    Ok(ToolPoolWorkloadBenchmarkReport {
        workload: workload.clone(),
        variants,
        comparison: ToolPoolComparison {
            legacy_catalog_preflight_operations: legacy_operations,
            pooled_catalog_preflight_operations: pooled_operations,
            catalog_preflight_operation_reduction_percent: reduction_percent,
            tool_start_p95_us,
            warm_schema_alias_p95_us,
            remaining_stage,
        },
    })
}

async fn run_tool_pool_variant(
    workload: &ToolPoolWorkload,
    options: &BenchmarkOptions,
    variant: &str,
    snapshots_enabled: bool,
) -> Result<ToolPoolVariantBenchmarkReport, String> {
    for _ in 0..options.warmup_samples {
        let sample = run_tool_pool_sample(workload, snapshots_enabled).await?;
        assert_tool_pool_invariants(&sample.counters, workload)?;
    }
    let mut samples = Vec::with_capacity(options.measured_samples);
    for _ in 0..options.measured_samples {
        let sample = run_tool_pool_sample(workload, snapshots_enabled).await?;
        assert_tool_pool_invariants(&sample.counters, workload)?;
        samples.push(sample);
    }
    let counters = aggregate_tool_pool_counters(&samples);
    let catalog_acquisition_elapsed_us =
        tool_pool_elapsed(&samples, |sample| &sample.catalog_acquisition_elapsed_us);
    let schema_alias_preparation_elapsed_us = tool_pool_elapsed(&samples, |sample| {
        &sample.schema_alias_preparation_elapsed_us
    });
    let warm_schema_alias_preparation_elapsed_us = tool_pool_elapsed(&samples, |sample| {
        &sample.warm_schema_alias_preparation_elapsed_us
    });
    let confirmation_preflight_elapsed_us =
        tool_pool_elapsed(&samples, |sample| &sample.confirmation_preflight_elapsed_us);
    let execution_lookup_elapsed_us =
        tool_pool_elapsed(&samples, |sample| &sample.execution_lookup_elapsed_us);
    let tool_runtime_elapsed_us =
        tool_pool_elapsed(&samples, |sample| &sample.tool_runtime_elapsed_us);
    Ok(ToolPoolVariantBenchmarkReport {
        variant: variant.to_string(),
        workload_signature: workload.fixture_signature(),
        counters,
        catalog_acquisition_latency: LatencySummary::from_samples(&catalog_acquisition_elapsed_us)?,
        schema_alias_preparation_latency: LatencySummary::from_samples(
            &schema_alias_preparation_elapsed_us,
        )?,
        warm_schema_alias_preparation_latency: snapshots_enabled
            .then(|| LatencySummary::from_samples(&warm_schema_alias_preparation_elapsed_us))
            .transpose()?,
        confirmation_preflight_latency: LatencySummary::from_samples(
            &confirmation_preflight_elapsed_us,
        )?,
        execution_lookup_latency: LatencySummary::from_samples(&execution_lookup_elapsed_us)?,
        tool_start_overhead_latency: LatencySummary::from_samples(&execution_lookup_elapsed_us)?,
        tool_runtime_latency: LatencySummary::from_samples(&tool_runtime_elapsed_us)?,
        machine: sample_machine_metrics(),
    })
}

async fn run_tool_pool_sample(
    workload: &ToolPoolWorkload,
    snapshots_enabled: bool,
) -> Result<ToolPoolSample, String> {
    let fixture = BenchmarkFixture::new(workload.tool_descriptors as usize).await?;
    let _diagnostic_lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK
        .lock()
        .map_err(|_| "performance recorder test lock poisoned".to_string())?;
    let _snapshot_env_guard = ToolCatalogSnapshotEnvGuard::set(snapshots_enabled);
    let sink = Arc::new(MemoryPerfSink::new());
    let recorder = Arc::new(PerfRecorder::with_salt(
        Arc::new(BenchmarkClock::default()),
        sink.clone(),
        [17; 32],
    ));
    let _recorder_guard = perf_diagnostics::install_test_recorder(recorder);
    let mut catalog_acquisition_elapsed_us = Vec::with_capacity(workload.chat_count as usize);
    let mut schema_alias_preparation_elapsed_us = Vec::with_capacity(workload.chat_count as usize);
    let mut warm_schema_alias_preparation_elapsed_us =
        Vec::with_capacity(workload.chat_count as usize);
    let mut sessions = Vec::with_capacity(workload.chat_count as usize);

    for chat_index in 0..workload.chat_count {
        let tool_calls = deterministic_tool_calls(workload, chat_index);
        let catalog_started = Instant::now();
        let catalog = fixture
            .app
            .tool_registry
            .acquire_tool_catalog("agent", Some("benchmark-local"), None)
            .await;
        catalog_acquisition_elapsed_us.push(elapsed_us(catalog_started));

        let schema_alias_started = Instant::now();
        let canonical =
            build_canonical_openai_tools(fixture.app.chat_ctx(), &catalog.index.tools, false, true)
                .await;
        let resolved = resolve_tool_call_aliases_with_catalog(tool_calls.clone(), &catalog);
        schema_alias_preparation_elapsed_us.push(elapsed_us(schema_alias_started));
        if canonical.tools.len() != workload.tool_descriptors as usize
            || resolved.len() != workload.tool_calls_per_chat as usize
        {
            return Err(
                "tool pool fixture did not prepare the expected schema and aliases".to_string(),
            );
        }

        if snapshots_enabled {
            let warm_schema_alias_started = Instant::now();
            let warm_canonical = build_canonical_openai_tools(
                fixture.app.chat_ctx(),
                &catalog.index.tools,
                false,
                true,
            )
            .await;
            let warm_resolved =
                resolve_tool_call_aliases_with_catalog(tool_calls.clone(), &catalog);
            warm_schema_alias_preparation_elapsed_us.push(elapsed_us(warm_schema_alias_started));
            if warm_canonical.tools.len() != canonical.tools.len()
                || warm_resolved.len() != resolved.len()
            {
                return Err("warm tool pool schema and aliases changed within a turn".to_string());
            }
        }

        let chat_id = format!("tool-pool-{}-{chat_index}", workload.fixture_signature());
        let session = Arc::new(AMutex::new(crate::chat::types::ChatSession::new(
            chat_id.clone(),
        )));
        {
            let mut session_locked = session.lock().await;
            session_locked.thread.model = "benchmark-local".to_string();
            session_locked.thread.mode = "agent".to_string();
            session_locked.thread.include_project_info = false;
            session_locked.tool_catalog = Some(catalog);
            let mut assistant = ChatMessage::new("assistant".to_string(), String::new());
            assistant.tool_calls = Some(tool_calls);
            session_locked.add_message(assistant);
        }
        fixture
            .app
            .chat
            .sessions
            .write()
            .await
            .insert(chat_id, session.clone());
        sessions.push(session);
    }

    let outcomes = futures::future::join_all(sessions.iter().cloned().map(|session| {
        process_tool_calls_once(
            fixture.app.clone(),
            session,
            "agent",
            Some("benchmark-local"),
        )
    }))
    .await;
    for (chat_index, (outcome, session)) in outcomes.into_iter().zip(sessions.iter()).enumerate() {
        if !matches!(outcome, ToolStepOutcome::Continue) {
            return Err(format!(
                "tool pool fixture chat {chat_index} did not continue"
            ));
        }
        assert_tool_result_order(session, workload, chat_index as u8).await?;
    }

    let events = sink.events();
    let counters = ToolPoolCounters {
        immutable_catalog_builds: event_count(&events, PerfComponent::ToolCatalogBuild),
        mutable_vector_builds: event_count(&events, PerfComponent::ToolMutableVectorBuild),
        parallel_vector_expansions: event_count(&events, PerfComponent::ToolPoolParallelExpansion),
        confirmation_preflight_starts: event_count(
            &events,
            PerfComponent::ToolConfirmationPreflight,
        ),
        confirmation_preflight_tool_checks: event_item_count(
            &events,
            PerfComponent::ToolConfirmationPreflight,
        ),
        execution_lookups: event_count(&events, PerfComponent::ToolExecutionLookup),
        tool_calls: u64::from(workload.chat_count) * u64::from(workload.tool_calls_per_chat),
        tool_runtime_calls: event_count(&events, PerfComponent::ToolRuntime),
        errors: tool_pool_errors(&events),
    };
    Ok(ToolPoolSample {
        counters,
        catalog_acquisition_elapsed_us,
        schema_alias_preparation_elapsed_us,
        warm_schema_alias_preparation_elapsed_us,
        confirmation_preflight_elapsed_us: event_elapsed(
            &events,
            PerfComponent::ToolConfirmationPreflight,
        ),
        execution_lookup_elapsed_us: event_elapsed(&events, PerfComponent::ToolExecutionLookup),
        tool_runtime_elapsed_us: event_elapsed(&events, PerfComponent::ToolRuntime),
    })
}

pub(crate) fn deterministic_tool_calls(
    workload: &ToolPoolWorkload,
    chat_index: u8,
) -> Vec<ChatToolCall> {
    let mut names = std::iter::repeat("benchmark_tool_0".to_string())
        .take(usize::from(workload.same_name_parallel_calls))
        .collect::<Vec<_>>();
    names.extend(
        (1..=workload.tool_calls_per_chat - workload.same_name_parallel_calls as u16)
            .map(|index| format!("benchmark_tool_{index}")),
    );
    names
        .into_iter()
        .enumerate()
        .map(|(call_index, name)| ChatToolCall {
            id: format!(
                "pool-{}-{chat_index}-{call_index}",
                workload.fixture_signature()
            ),
            index: Some(call_index),
            function: ChatToolFunction {
                name,
                arguments: "{}".to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        })
        .collect()
}

async fn run_sample(
    workload: &ConcurrentChatWorkload,
    mode: HarnessMode,
    writer_enabled: bool,
) -> Result<Sample, String> {
    let fixture = BenchmarkFixture::new(workload.tool_descriptors as usize).await?;
    let _diagnostic_lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK
        .lock()
        .map_err(|_| "performance recorder test lock poisoned".to_string())?;
    let _writer_env_guard = TrajectoryWriterEnvGuard::set(writer_enabled);
    let _writer = TrajectoryWriterRolloutGuard::set(writer_enabled);
    let sink = Arc::new(MemoryPerfSink::new());
    let recorder = Arc::new(PerfRecorder::with_salt(
        Arc::new(BenchmarkClock::default()),
        sink.clone(),
        [11; 32],
    ));
    let _recorder_guard = perf_diagnostics::install_test_recorder(recorder);
    let total_started = Instant::now();
    let before = filesystem_snapshot(&fixture.workspace).await?;

    let history = deterministic_payload(workload.seed, workload.materialized_history_bytes(mode));
    let saves = save_workload_trajectories(&fixture, workload, &history).await?;
    let index_started = Instant::now();
    let index_dir = fixture.workspace.join(".refact").join("trajectories");
    rebuild_trajectory_index_from_disk(&index_dir, None).await?;
    let _index_elapsed_us = elapsed_us(index_started);

    let catalog_started = Instant::now();
    let catalog = fixture
        .app
        .tool_registry
        .get_tools_index_for_mode("agent", None)
        .await;
    let canonical =
        build_canonical_openai_tools(fixture.app.chat_ctx(), &catalog.tools, false, true).await;
    let policies = fixture
        .app
        .tool_registry
        .get_tool_policy_info("agent", None)
        .await;
    let catalog_elapsed_us = elapsed_us(catalog_started);
    if canonical.tools.len() != workload.tool_descriptors as usize
        || policies.len() != workload.tool_descriptors as usize
    {
        return Err(
            "fixture tool registry did not materialize the deterministic local catalog".to_string(),
        );
    }

    let after = filesystem_snapshot(&fixture.workspace).await?;
    let (files_written, bytes_written) = filesystem_delta(&before, &after);
    let trajectory_files = count_trajectory_files(&fixture.workspace).await?;
    let events = sink.events();
    let diagnostics = DiagnosticCounters::from_events(&events);
    let counters = BenchmarkCounters {
        save_calls: saves.total,
        rapid_checkpoint_saves: saves.checkpoints,
        required_commits: saves.required_commits,
        trajectory_files,
        measured_files_written: files_written,
        measured_bytes_written: bytes_written,
        index_rebuilds: diagnostics.trajectory_index_rebuild,
        catalog_builds: diagnostics.tool_catalog_build,
        catalog_tool_descriptors: catalog.tools.len() as u64,
        catalog_policy_entries: policies.len() as u64,
        errors: terminal_operation_failures(&events),
    };
    Ok(Sample {
        counters,
        diagnostics,
        snapshot_elapsed_us: elapsed_for(&events, PerfComponent::TrajectorySnapshot),
        serialize_elapsed_us: elapsed_for(&events, PerfComponent::TrajectorySerialize),
        atomic_write_elapsed_us: elapsed_for(&events, PerfComponent::TrajectoryAtomicWrite),
        commit_elapsed_us: elapsed_for(&events, PerfComponent::TrajectoryCommit),
        index_wait_elapsed_us: elapsed_for(&events, PerfComponent::TrajectoryIndexLockWait),
        index_write_elapsed_us: elapsed_for(&events, PerfComponent::TrajectoryIndexWrite),
        catalog_elapsed_us,
        checkpoint_return_elapsed_us: saves.checkpoint_return_elapsed_us,
        background_flush_elapsed_us: saves.background_flush_elapsed_us,
        total_elapsed_us: elapsed_us(total_started),
    })
}

async fn save_workload_trajectories(
    fixture: &BenchmarkFixture,
    workload: &ConcurrentChatWorkload,
    history: &[u8],
) -> Result<SaveCounts, String> {
    let shared_payload = String::from_utf8_lossy(history).to_string();
    let primary_chat_id = format!("bench-{}-0", workload.fixture_signature(HarnessMode::Quick));
    let save_tasks = (0..workload.chat_count)
        .map(|chat_index| {
            let chat_id = format!(
                "bench-{}-{chat_index}",
                workload.fixture_signature(HarnessMode::Quick)
            );
            let payload = shared_payload.clone();
            let gcx = fixture.gcx.clone();
            async move { save_real_session_snapshot(gcx, &chat_id, &payload).await }
        })
        .collect::<Vec<_>>();
    for result in futures::future::join_all(save_tasks).await {
        result?;
    }

    let chat_id = primary_chat_id;
    let mut checkpoint_return_samples = Vec::new();
    for checkpoint in 0..workload.rapid_same_chat_checkpoints {
        let checkpoint_started = Instant::now();
        save_real_session_snapshot(
            fixture.gcx.clone(),
            &chat_id,
            &format!(
                "checkpoint-{checkpoint}-{}",
                &shared_payload[..shared_payload.len().min(256)]
            ),
        )
        .await?;
        checkpoint_return_samples.push(elapsed_us(checkpoint_started));
    }
    let checkpoint_return_elapsed_us = percentile_us(&checkpoint_return_samples, 95);
    let flush_started = Instant::now();
    persist_trajectory_snapshot_with_intent(
        fixture.gcx.clone(),
        snapshot_for_payload(&chat_id, &format!("final-{shared_payload}")),
        crate::chat::types::TrajectoryCommitIntent::Required,
    )
    .await?;
    let background_flush_elapsed_us = elapsed_us(flush_started);

    let file_path = find_trajectory_path(fixture.gcx.clone(), &chat_id)
        .await
        .ok_or_else(|| "real trajectory save did not expose a trajectory path".to_string())?;
    let value = serde_json::from_str(
        &tokio::fs::read_to_string(&file_path)
            .await
            .map_err(|error| format!("failed to read real trajectory fixture: {error}"))?,
    )
    .map_err(|error| format!("failed to parse real trajectory fixture: {error}"))?;
    let index_dir = fixture.workspace.join(".refact").join("trajectories");
    upsert_trajectory_index_entry_from_owned_value(&index_dir, &file_path, value, None).await?;

    let loaded = load_trajectory_for_chat(fixture.gcx.clone(), &chat_id)
        .await
        .ok_or_else(|| "real trajectory fixture could not be reloaded".to_string())?;
    if loaded.messages.len() != 1 {
        return Err("real trajectory fixture lost or corrupted messages".to_string());
    }

    Ok(SaveCounts {
        total: u64::from(workload.chat_count) + workload.rapid_same_chat_checkpoints + 1,
        checkpoints: workload.rapid_same_chat_checkpoints,
        required_commits: 1,
        checkpoint_return_elapsed_us,
        background_flush_elapsed_us,
    })
}

async fn save_real_session_snapshot(
    gcx: SharedGlobalContext,
    chat_id: &str,
    payload: &str,
) -> Result<(), String> {
    persist_trajectory_snapshot_with_intent(
        gcx,
        snapshot_for_payload(chat_id, payload),
        crate::chat::types::TrajectoryCommitIntent::Checkpoint,
    )
    .await
}

pub(crate) fn snapshot_for_payload(
    chat_id: &str,
    payload: &str,
) -> crate::chat::trajectories::TrajectorySnapshot {
    let mut session = crate::chat::types::ChatSession::new(chat_id.to_string());
    session.thread.title = "Concurrent benchmark".to_string();
    session.thread.model = "benchmark-local".to_string();
    session.thread.mode = "agent".to_string();
    session.thread.include_project_info = false;
    session.thread.auto_enrichment_enabled = Some(false);
    session.add_message(ChatMessage {
        role: "user".to_string(),
        content: ChatContent::SimpleText(payload.to_string()),
        ..Default::default()
    });
    trajectory_snapshot_from_session(&session)
}

async fn filesystem_snapshot(
    root: &Path,
) -> Result<std::collections::BTreeMap<PathBuf, u64>, String> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut files = std::collections::BTreeMap::new();
        let mut pending = vec![root];
        while let Some(path) = pending.pop() {
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("failed to walk benchmark files: {error}")),
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if metadata.is_dir() {
                    pending.push(path);
                } else if metadata.is_file() {
                    files.insert(path, metadata.len());
                }
            }
        }
        Ok(files)
    })
    .await
    .map_err(|error| format!("benchmark file snapshot task failed: {error}"))?
}

pub(crate) fn filesystem_delta(
    before: &std::collections::BTreeMap<PathBuf, u64>,
    after: &std::collections::BTreeMap<PathBuf, u64>,
) -> (u64, u64) {
    after
        .iter()
        .fold((0, 0), |(files, bytes), (path, after_len)| {
            let before_len = before.get(path).copied().unwrap_or(0);
            if before.get(path) != Some(after_len) {
                (
                    files + 1,
                    bytes.saturating_add(after_len.saturating_sub(before_len)),
                )
            } else {
                (files, bytes)
            }
        })
}

async fn count_trajectory_files(workspace: &Path) -> Result<u64, String> {
    let root = workspace.join(".refact").join("trajectories");
    let snapshot = filesystem_snapshot(&root).await?;
    Ok(snapshot
        .keys()
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .filter(|path| path.file_name().and_then(|name| name.to_str()) != Some("index.json"))
        .count() as u64)
}
