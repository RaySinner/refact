mod workflow_migration {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/buddy/tests/workflow_migration.rs"
    ));
}

use super::*;
use crate::buddy::autonomous_workflows::AUTONOMOUS_BUDDY_WORKFLOWS;
use crate::buddy::scheduler::BuddyJobContext;
use crate::buddy::settings::{save_settings, BuddySettings};
use crate::buddy::types::{BuddyFact, BuddyJobState, BuddyOnboarding, BuddyPetState, BuddyPulse};
use crate::call_validation::{ChatContent, ChatToolCall, ChatToolFunction};
use crate::stats::event::LlmCallEvent;

fn create_symlink_dir_for_test(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_dir(target, link);

    match result {
        Ok(()) => true,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) || err.raw_os_error() == Some(1314) =>
        {
            false
        }
        Err(err) => panic!("failed to create test symlink {link:?} -> {target:?}: {err}"),
    }
}

fn init_temp_git_repo() -> (tempfile::TempDir, git2::Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    {
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }
    (dir, repo)
}

fn git_add(repo: &git2::Repository, path: &str) {
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
}

#[test]
fn scan_heavy_autonomous_jobs_record_empty_results_for_scheduler_backoff() {
    assert!(BuddyBehaviorLearnerJob.records_empty_result());
    assert!(BuddyModelCostOptimizerJob.records_empty_result());
    assert!(SecurityWhispererJob.records_empty_result());
    assert!(DependencyRadarJob.records_empty_result());
    assert!(DocsGardenerJob.records_empty_result());
    assert!(ArchitectureDriftWatcherJob.records_empty_result());
}

fn stream_corrupted_diagnostic() -> DiagnosticContext {
    DiagnosticContext {
        error_type: "parse".to_string(),
        error_message: "stream ended unexpectedly while decoding response body".to_string(),
        source_file: Some("chat/generation.rs".to_string()),
        tool_name: None,
        chat_id: Some("chat-1".to_string()),
        model_id: Some("gpt-4".to_string()),
        collected_at: Utc::now().to_rfc3339(),
        severity: DiagnosticSeverity::Medium,
        occurrences: None,
    }
}

#[test]
fn format_diagnostic_summary_includes_category_and_recoverable_for_stream_corrupted() {
    let summary = format_diagnostic_summary(&stream_corrupted_diagnostic());
    assert!(summary.contains("category=StreamCorrupted"), "{summary}");
    assert!(summary.contains("recoverable=true"), "{summary}");
    assert!(summary.contains("severity=Medium"), "{summary}");
}

#[test]
fn diagnostic_evidence_preserves_occurrence_telemetry_for_stream_corrupted() {
    let mut ctx = context_with_last_result(None);
    ctx.recent_diagnostics = (0..5).map(|_| stream_corrupted_diagnostic()).collect();
    let evidence = diagnostic_evidence(&ctx).expect("expected diagnostic evidence");
    assert_eq!(evidence.repeated_error_type.as_deref(), Some("parse"));
    assert_eq!(evidence.repeated_count, 5);
    assert!(evidence
        .summaries
        .iter()
        .any(|s| s.contains("category=StreamCorrupted") && s.contains("recoverable=true")));
}

fn test_fact(kind: BuddyFactKind, payload: serde_json::Value) -> BuddyFact {
    BuddyFact {
        kind,
        key: format!("test:{:?}", kind),
        source: test_fact_source(kind),
        payload,
        seen_at: Utc::now(),
        confidence: 0.9,
    }
}

fn test_fact_source(kind: BuddyFactKind) -> &'static str {
    match kind {
        BuddyFactKind::TaskStuck
        | BuddyFactKind::TaskAbandoned
        | BuddyFactKind::TaskClusterDuplicate => "task_health",
        BuddyFactKind::TrajectoryClutter => "trajectory_clutter",
        BuddyFactKind::ChatRetryStreak => "chat_pattern",
        BuddyFactKind::MemoryOrphan
        | BuddyFactKind::MemoryStaleConflict
        | BuddyFactKind::MemoryRecurringLesson => "memory_garden",
        BuddyFactKind::ModePromptOverlap
        | BuddyFactKind::SkillTriggerWeak
        | BuddyFactKind::AgentsMdGapDetected => "customization_drift",
        BuddyFactKind::DefaultModelMissing | BuddyFactKind::BrokenModelReference => {
            "provider_health"
        }
        BuddyFactKind::McpAuthExpired | BuddyFactKind::IntegrationFailing => "mcp_auth",
        BuddyFactKind::DiagnosticCluster | BuddyFactKind::FrontendErrorBurst => {
            "diagnostic_cluster"
        }
        BuddyFactKind::GitDiffWidening | BuddyFactKind::UncommittedPressure => "git_pressure",
        BuddyFactKind::WorktreeHygiene => "worktree_hygiene",
    }
}

fn test_llm_event(
    i: u64,
    success: bool,
    duration_ms: u64,
    tokens: usize,
    cost: f64,
) -> LlmCallEvent {
    test_llm_event_for_model(
        i,
        success,
        duration_ms,
        tokens,
        cost,
        "anthropic/claude-test",
        "anthropic",
        "claude-test",
    )
}

fn test_llm_event_for_model(
    i: u64,
    success: bool,
    duration_ms: u64,
    tokens: usize,
    cost: f64,
    model_id: &str,
    provider: &str,
    model: &str,
) -> LlmCallEvent {
    LlmCallEvent {
        id: format!("event-{i}"),
        ts_start: format!("2026-02-{:02}T00:00:00Z", i + 1),
        ts_end: format!("2026-02-{:02}T00:00:01Z", i + 1),
        duration_ms,
        chat_id: format!("chat-{i}"),
        root_chat_id: None,
        mode: "agent".to_string(),
        task_id: None,
        task_role: None,
        agent_id: None,
        card_id: None,
        model_id: model_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        messages_count: 3,
        tools_count: 0,
        max_tokens: 4096,
        temperature: Some(0.0),
        success,
        error_message: if success {
            None
        } else {
            Some("timeout".to_string())
        },
        finish_reason: if success {
            Some("stop".to_string())
        } else {
            None
        },
        attempt_n: 1,
        retry_reason: None,
        prompt_tokens: tokens / 2,
        completion_tokens: tokens / 2,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        total_tokens: tokens,
        cost_usd: Some(cost),
    }
}

fn context_with_last_result(last_result: Option<String>) -> BuddyJobContext {
    BuddyJobContext {
        identity_name: "Pixel".to_string(),
        personality: Default::default(),
        onboarding: BuddyOnboarding::default(),
        recent_diagnostics: vec![],
        project_root: std::path::PathBuf::from("/tmp/project"),
        job_state: BuddyJobState {
            last_result,
            ..Default::default()
        },
        workflow_summaries: vec![],
        total_workflow_runs: 0,
        suggestion_state: vec![],
        pet: BuddyPetState::default(),
        active_quest: None,
        settings: BuddySettings::default(),
        pulse: BuddyPulse::default(),
        facts: vec![],
        recent_activities: vec![],
    }
}

async fn make_gcx_with_buddy() -> AppState {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let (tx, _) = tokio::sync::broadcast::channel(16);
    let mut state = crate::buddy::state::default_buddy_state();
    state.identity.name = "Pixel".to_string();
    let service = crate::buddy::actor::BuddyService::new(
        std::env::temp_dir().join(format!("buddy-voice-test-{}", uuid::Uuid::new_v4())),
        state,
        BuddySettings::default(),
        Vec::new(),
        crate::buddy::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    let app = AppState::from_gcx(gcx).await;
    let buddy_arc = app.buddy.buddy.clone();
    *buddy_arc.lock().await = Some(service);
    app
}

fn write_behavior_trajectory(path: &Path, id: &str, modified_secs: i64) {
    std::fs::write(
        path,
        serde_json::json!({
            "id": id,
            "updated_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(modified_secs, 0)).unwrap();
}

fn write_internal_behavior_trajectory(path: &Path, id: &str, modified_secs: i64) {
    std::fs::write(
        path,
        serde_json::json!({
            "id": id,
            "updated_at": "2026-01-02T00:00:00Z",
            "link_type": "internal:buddy"
        })
        .to_string(),
    )
    .unwrap();
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(modified_secs, 0)).unwrap();
}

fn behavior_candidate_file_names(candidates: &[BehaviorTrajectoryCandidate]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| {
            candidate
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect()
}

#[test]
fn signal_hash_is_stable_and_changes_with_signal() {
    let first = signal_hash(["refact_error_detective", "a", "b"]);
    let second = signal_hash(["refact_error_detective", "a", "b"]);
    let changed = signal_hash(["refact_error_detective", "a", "c"]);
    let boundary_a = signal_hash(["ab", "c"]);
    let boundary_b = signal_hash(["a", "bc"]);

    assert_eq!(first, second);
    assert_ne!(first, changed);
    assert_ne!(boundary_a, boundary_b);
    assert_eq!(first.len(), 64);
}

#[test]
fn default_signal_hash_uses_redacted_capped_prompt_and_evidence() {
    let prompt = "Review failed login traces";
    let first_evidence = "request failed password=alpha-secret token=first-token";
    let second_evidence = "request failed password=beta-secret token=second-token";
    let first = AutonomousBuddyChatSpec::new(
        "buddy_security_whisperer",
        "Security Whisperer",
        prompt,
        first_evidence,
    );
    let second = AutonomousBuddyChatSpec::new(
        "buddy_security_whisperer",
        "Security Whisperer",
        prompt,
        second_evidence,
    );
    let expected = signal_hash([
        "buddy_security_whisperer",
        "Security Whisperer",
        redact_and_cap_prompt(prompt).as_str(),
        redact_and_cap_evidence(first_evidence).as_str(),
    ]);
    let raw_first = signal_hash([
        "buddy_security_whisperer",
        "Security Whisperer",
        prompt,
        first_evidence,
    ]);
    let raw_second = signal_hash([
        "buddy_security_whisperer",
        "Security Whisperer",
        prompt,
        second_evidence,
    ]);

    assert_ne!(raw_first, raw_second);
    assert_eq!(first.signal_hash, second.signal_hash);
    assert_eq!(first.signal_hash, expected);
    assert_ne!(first.signal_hash, raw_first);

    let displayed = first.with_display("🛡️", "Security", "high");
    assert_eq!(displayed.icon, "🛡️");
    assert_eq!(displayed.badge, "Security");
    assert_eq!(displayed.priority, "high");
}

#[test]
fn last_result_json_round_trips_and_malformed_values_fallback() {
    let result = AutonomousLastResult {
        signal_hash: "hash-a".to_string(),
        chat_id: "chat-a".to_string(),
        completed_at: "2026-01-01T00:00:00Z".to_string(),
        status: None,
        failure_category: None,
        consecutive_failures: None,
    };
    let serialized = serialize_last_autonomous_result(&result);

    assert_eq!(
        parse_last_autonomous_result(Some(&serialized)),
        Some(result)
    );
    assert_eq!(parse_last_autonomous_result(Some("legacy-value")), None);
    assert_eq!(parse_last_autonomous_result(Some("{")), None);
    assert_eq!(parse_last_autonomous_result(Some("{}")), None);
    assert_eq!(parse_last_autonomous_result(None), None);

    let dynamic = AutonomousLastResult::new("hash-b", "chat-b");
    assert_eq!(dynamic.signal_hash, "hash-b");
    assert_eq!(dynamic.chat_id, "chat-b");
    assert!(!dynamic.completed_at.is_empty());
}

#[tokio::test]
async fn autonomous_activity_links_to_generated_buddy_chat() {
    let spec = AutonomousBuddyChatSpec::new(
        MEMORY_GARDENER_WORKFLOW_ID,
        "Memory Gardener",
        "Review memory signals",
        "memory evidence",
    )
    .with_display("🌱", "Memory", "normal");

    let gcx = crate::global_context::tests::make_test_gcx().await;
    let gcx = AppState::from_gcx(gcx).await;
    let activity = autonomous_activity(gcx, &spec, "buddy-chat-1", "Buddy").await;

    assert_eq!(activity.activity_type, MEMORY_GARDENER_WORKFLOW_ID);
    assert_eq!(activity.chat_id.as_deref(), Some("buddy-chat-1"));
    assert!(activity.description.contains("Buddy saved"));
}

#[tokio::test]
async fn voice_unavailable_falls_back_to_deterministic_string() {
    let spec = AutonomousBuddyChatSpec::new(
        MEMORY_GARDENER_WORKFLOW_ID,
        "Memory Gardener",
        "Review memory signals",
        "memory evidence",
    )
    .with_display("🌱", "Memory", "normal");
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let gcx = AppState::from_gcx(gcx).await;

    let activity = autonomous_activity(gcx.clone(), &spec, "buddy-chat-1", "Pixel").await;
    let event = autonomous_runtime_event(gcx, &spec, "buddy-chat-1", "Pixel").await;

    assert_eq!(activity.title, "Memory Gardener report saved");
    assert_eq!(event.title, "Memory Gardener report ready");
    assert_eq!(
        event.description.as_deref(),
        Some("Open Pixel's saved chat for Memory Gardener details.")
    );
}

#[tokio::test]
async fn autonomous_runtime_event_uses_voice_service_when_available() {
    let (service, renderer) =
        crate::buddy::voice_service::test_voice_service_with_responses(vec![Some(
            "voice ready".to_string(),
        )]);
    let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
    let spec = AutonomousBuddyChatSpec::new(
        MEMORY_GARDENER_WORKFLOW_ID,
        "Memory Gardener",
        "Review memory signals",
        "memory evidence",
    )
    .with_display("🌱", "Memory", "normal");
    let gcx = make_gcx_with_buddy().await;

    let event = autonomous_runtime_event(gcx, &spec, "buddy-chat-1", "Pixel").await;

    assert_eq!(event.title, "voice ready");
    assert_eq!(
        renderer.intent_kinds(),
        vec!["runtime:completed".to_string()]
    );
}

#[test]
fn same_signal_uses_parsed_last_result() {
    let result = AutonomousLastResult {
        signal_hash: "same".to_string(),
        chat_id: "chat".to_string(),
        completed_at: "2026-01-01T00:00:00Z".to_string(),
        status: None,
        failure_category: None,
        consecutive_failures: None,
    };
    let ctx = context_with_last_result(Some(serialize_last_autonomous_result(&result)));
    let malformed_ctx = context_with_last_result(Some("same".to_string()));

    assert!(same_signal(&ctx, "same"));
    assert!(!same_signal(&ctx, "different"));
    assert!(!same_signal(&malformed_ctx, "same"));
}

#[test]
fn failed_last_result_backoff_ignores_signal_hash_drift_when_category_matches() {
    let result = AutonomousLastResult {
        signal_hash: "old-signal".to_string(),
        chat_id: "failed:buddy_dependency_radar:broken_model_reference".to_string(),
        completed_at: Utc::now().to_rfc3339(),
        status: Some("failed".to_string()),
        failure_category: Some(WorkflowFailureCategory::ToolFailed),
        consecutive_failures: Some(2),
    };
    let ctx = context_with_last_result(Some(serialize_last_autonomous_result(&result)));

    assert!(same_signal_or_failed_backoff(&ctx, "new-signal", 60));
}

fn assistant_message(text: &str) -> ChatMessage {
    ChatMessage::new("assistant".to_string(), text.to_string())
}

fn assistant_tool_call(tool_call_id: &str, tool_name: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: ChatContent::SimpleText(String::new()),
        tool_calls: Some(vec![ChatToolCall {
            id: tool_call_id.to_string(),
            index: Some(0),
            function: ChatToolFunction {
                name: tool_name.to_string(),
                arguments: "{}".to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        }]),
        ..Default::default()
    }
}

fn tool_result(tool_call_id: &str, failed: bool) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: ChatContent::SimpleText("ok".to_string()),
        tool_call_id: tool_call_id.to_string(),
        tool_failed: Some(failed),
        ..Default::default()
    }
}

#[test]
fn autonomous_subchat_success_requires_report_or_successful_buddy_tool() {
    assert!(!autonomous_subchat_successful(&[]));
    assert!(autonomous_subchat_successful(&[assistant_message(
        "Report ready"
    )]));
    assert!(!autonomous_subchat_successful(&[
        assistant_tool_call("call-1", "buddy_runtime_event"),
        tool_result("call-1", true),
    ]));
    assert!(autonomous_subchat_successful(&[
        assistant_tool_call("call-1", "buddy_runtime_event"),
        tool_result("call-1", false),
    ]));
}

#[test]
fn memory_fact_filtering_builds_metadata_only_evidence() {
    let mut ctx = context_with_last_result(None);
    ctx.pulse.memory.orphan = 2;
    ctx.pulse.memory.stale_conflicts = 1;
    ctx.facts = vec![
        test_fact(
            BuddyFactKind::MemoryOrphan,
            serde_json::json!({
                "memory_ids": ["mem-a", "mem-b"],
                "count": 2,
                "body": "full memory body should not appear password=secret"
            }),
        ),
        test_fact(
            BuddyFactKind::MemoryStaleConflict,
            serde_json::json!({
                "doc_ids": ["doc-a", "doc-b"],
                "conflict_summary": "prefer x vs avoid x",
                "body": "full doc body should not appear"
            }),
        ),
    ];

    let gardener = memory_gardener_evidence(&ctx).unwrap();
    let conflict = knowledge_conflict_evidence(&ctx).unwrap();

    assert!(gardener.evidence.contains("mem-a"));
    assert!(gardener.evidence.contains("memory_pulse"));
    assert!(!gardener.evidence.contains("memory_stale_conflict"));
    assert!(!gardener.evidence.contains("stale_conflicts"));
    assert!(!gardener.evidence.contains("full memory body"));
    assert!(conflict.evidence.contains("doc-a"));
    assert!(conflict.evidence.contains("prefer x vs avoid x"));
    assert!(!conflict.evidence.contains("full doc body"));
}

#[test]
fn lone_stale_conflict_routes_only_to_conflict_resolver() {
    let mut ctx = context_with_last_result(None);
    ctx.pulse.memory.total = 4;
    ctx.pulse.memory.stale_conflicts = 1;
    ctx.facts = vec![test_fact(
        BuddyFactKind::MemoryStaleConflict,
        serde_json::json!({
            "doc_ids": ["doc-a", "doc-b"],
            "conflict_summary": "prefer short answers vs detailed answers"
        }),
    )];

    assert_eq!(
        memory_route_for_fact_kind(BuddyFactKind::MemoryStaleConflict),
        Some(MemoryRouting::KnowledgeConflictResolver)
    );
    assert!(memory_gardener_evidence(&ctx).is_none());
    let conflict = knowledge_conflict_evidence(&ctx).unwrap();
    assert!(conflict.evidence.contains("memory_stale_conflict"));

    ctx.facts.clear();
    assert!(memory_gardener_evidence(&ctx).is_none());
    assert!(knowledge_conflict_evidence(&ctx).is_none());
}

#[test]
fn pulse_only_stale_conflict_does_not_trigger_conflict_report() {
    let mut ctx = context_with_last_result(None);
    ctx.pulse.memory.stale_conflicts = 3;

    assert!(knowledge_conflict_evidence(&ctx).is_none());

    ctx.facts = vec![test_fact(
        BuddyFactKind::MemoryStaleConflict,
        serde_json::json!({"count": 3}),
    )];
    assert!(knowledge_conflict_evidence(&ctx).is_none());

    ctx.facts = vec![test_fact(
        BuddyFactKind::MemoryStaleConflict,
        serde_json::json!({"summary": "doc-a contradicts doc-b"}),
    )];
    assert!(knowledge_conflict_evidence(&ctx).is_some());
}

#[test]
fn fact_payload_arrays_are_sorted_and_deduped_for_stable_signal() {
    let mut first = context_with_last_result(None);
    first.pulse.memory.orphan = 1;
    first.facts = vec![test_fact(
        BuddyFactKind::MemoryOrphan,
        serde_json::json!({
            "memory_ids": ["mem-b", "mem-a", "mem-b"],
            "doc_ids": ["doc-c", "doc-a", "doc-c"],
            "tags": ["rust", "agent", "rust"]
        }),
    )];
    let mut second = context_with_last_result(None);
    second.pulse.memory.orphan = 1;
    second.facts = vec![test_fact(
        BuddyFactKind::MemoryOrphan,
        serde_json::json!({
            "memory_ids": ["mem-a", "mem-b"],
            "doc_ids": ["doc-a", "doc-c"],
            "tags": ["agent", "rust"]
        }),
    )];

    let first_evidence = memory_gardener_evidence(&first).unwrap();
    let second_evidence = memory_gardener_evidence(&second).unwrap();
    let first_spec = build_spec(MEMORY_GARDENER_WORKFLOW_ID, first_evidence.clone());
    let second_spec = build_spec(MEMORY_GARDENER_WORKFLOW_ID, second_evidence.clone());

    assert_eq!(first_evidence.evidence, second_evidence.evidence);
    assert!(first_evidence.evidence.contains("memory_ids=mem-a, mem-b"));
    assert!(first_evidence.evidence.contains("doc_ids=doc-a, doc-c"));
    assert!(first_evidence.evidence.contains("tags=agent, rust"));
    assert_eq!(first_spec.signal_hash, second_spec.signal_hash);
}

#[test]
fn behavior_preference_candidates_validate_and_dedupe() {
    let snippets = vec![
        TrajectoryUserSnippet {
            trajectory_id: "chat-a".to_string(),
            title: "A".to_string(),
            mode: "agent".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            text: "I prefer concise answers with bullet lists. Thanks.".to_string(),
        },
        TrajectoryUserSnippet {
            trajectory_id: "chat-b".to_string(),
            title: "B".to_string(),
            mode: "agent".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            text: "I prefer concise answers with bullet lists.".to_string(),
        },
        TrajectoryUserSnippet {
            trajectory_id: "chat-c".to_string(),
            title: "C".to_string(),
            mode: "agent".to_string(),
            updated_at: "2026-01-03T00:00:00Z".to_string(),
            text: "Please use Rust examples when explaining async code.".to_string(),
        },
        TrajectoryUserSnippet {
            trajectory_id: "chat-d".to_string(),
            title: "D".to_string(),
            mode: "agent".to_string(),
            updated_at: "2026-01-04T00:00:00Z".to_string(),
            text: "I prefer token=private in every command.".to_string(),
        },
    ];

    let candidates = behavior_preference_candidates(&snippets);

    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.statement.contains("concise answers")));
    assert!(candidates
        .iter()
        .any(|candidate| candidate.statement.contains("Rust examples")));
    assert!(!candidates
        .iter()
        .any(|candidate| candidate.statement.contains("token")));
}

#[test]
fn behavior_preference_write_selection_is_capped_to_two() {
    let snippets = vec![
        TrajectoryUserSnippet {
            trajectory_id: "chat-a".to_string(),
            title: "A".to_string(),
            mode: "agent".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            text: "I prefer concise answers with bullet lists.".to_string(),
        },
        TrajectoryUserSnippet {
            trajectory_id: "chat-b".to_string(),
            title: "B".to_string(),
            mode: "agent".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            text: "Please use Rust examples when explaining async code.".to_string(),
        },
        TrajectoryUserSnippet {
            trajectory_id: "chat-c".to_string(),
            title: "C".to_string(),
            mode: "agent".to_string(),
            updated_at: "2026-01-03T00:00:00Z".to_string(),
            text: "Please keep summaries short before detailed sections.".to_string(),
        },
    ];
    let candidates = behavior_preference_candidates(&snippets);

    assert!(candidates.len() > MAX_BEHAVIOR_PREFERENCE_WRITES);
    assert_eq!(
        candidates
            .iter()
            .take(MAX_BEHAVIOR_PREFERENCE_WRITES)
            .count(),
        2
    );
}

#[tokio::test]
async fn oversized_trajectory_json_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let small_path = dir.path().join("small.json");
    let large_path = dir.path().join("large.json");
    tokio::fs::write(
        &small_path,
        serde_json::json!({"messages": [{"role": "user", "content": "I prefer concise answers."}]})
            .to_string(),
    )
    .await
    .unwrap();
    tokio::fs::write(&large_path, format!("{{\"pad\":\"{}\"}}", "x".repeat(128)))
        .await
        .unwrap();

    assert!(read_bounded_trajectory_json(&small_path, 256)
        .await
        .is_some());
    assert!(read_bounded_trajectory_json(&large_path, 32)
        .await
        .is_none());
}

#[test]
fn behavior_trajectory_meta_collection_skips_oversized_files() {
    let dir = tempfile::tempdir().unwrap();
    let small_path = dir.path().join("small.json");
    let large_path = dir.path().join("large.json");
    std::fs::write(
        &small_path,
        serde_json::json!({
            "id": "small",
            "title": "Small",
            "mode": "agent",
            "updated_at": "2026-01-02T00:00:00Z",
            "messages": [{"role": "user", "content": "I prefer concise answers."}]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &large_path,
        format!(
            "{{\"id\":\"large\",\"updated_at\":\"2026-01-03T00:00:00Z\",\"pad\":\"{}\"}}",
            "x".repeat((MAX_BEHAVIOR_TRAJECTORY_BYTES as usize) + 1)
        ),
    )
    .unwrap();

    let mut metas = Vec::new();
    let mut seen = HashSet::new();
    collect_behavior_trajectory_metas_from_dir(dir.path(), &mut metas, &mut seen);

    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].id, "small");
}

#[test]
fn behavior_trajectory_candidate_collection_is_bounded_and_recent_biased() {
    let dir = tempfile::tempdir().unwrap();
    for idx in 0..5 {
        let path = dir.path().join(format!("chat_{idx}.json"));
        write_behavior_trajectory(&path, &format!("chat-{idx}"), 100 + idx as i64);
    }

    let candidates =
        collect_behavior_trajectory_candidates_from_dirs(&[dir.path().to_path_buf()], 3);
    let mut metas = Vec::new();
    let mut seen = HashSet::new();
    collect_behavior_trajectory_metas_from_candidates(candidates, &mut metas, &mut seen);

    assert_eq!(metas.len(), 3);
    assert_eq!(metas[0].id, "chat-4");
    assert_eq!(metas[1].id, "chat-3");
    assert_eq!(metas[2].id, "chat-2");
}

#[test]
fn recent_internal_trajectories_do_not_crowd_out_displayable_candidate_cap() {
    let dir = tempfile::tempdir().unwrap();
    for idx in 0..5 {
        write_internal_behavior_trajectory(
            &dir.path().join(format!("internal_{idx}.json")),
            &format!("internal-{idx}"),
            1_000 + idx as i64,
        );
    }
    for idx in 0..3 {
        write_behavior_trajectory(
            &dir.path().join(format!("displayable_{idx}.json")),
            &format!("displayable-{idx}"),
            100 + idx as i64,
        );
    }

    let candidates =
        collect_behavior_trajectory_candidates_from_dirs(&[dir.path().to_path_buf()], 3);
    let mut metas = Vec::new();
    let mut seen = HashSet::new();
    collect_behavior_trajectory_metas_from_candidates(candidates, &mut metas, &mut seen);

    assert_eq!(
        metas
            .iter()
            .map(|meta| meta.id.as_str())
            .collect::<Vec<_>>(),
        vec!["displayable-2", "displayable-1", "displayable-0"]
    );
    assert!(metas.iter().all(|meta| !meta.id.starts_with("internal-")));
}

#[test]
fn behavior_trajectory_candidate_scan_stops_at_visit_budget() {
    let dir = tempfile::tempdir().unwrap();
    for idx in 0..8 {
        write_behavior_trajectory(
            &dir.path().join(format!("chat_{idx}.json")),
            &format!("chat-{idx}"),
            100 + idx as i64,
        );
    }

    let (candidates, stats) = collect_behavior_trajectory_candidates_from_dirs_with_stats(
        &[dir.path().to_path_buf()],
        8,
        4,
    );

    assert_eq!(stats.visited_entries, 4);
    assert_eq!(candidates.len(), 4);
    assert_eq!(stats.matching_files_considered, candidates.len());
    assert!(stats.matching_files_considered < 8);
}

#[test]
fn behavior_trajectory_candidate_scan_skips_symlinked_directories() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real");
    let loop_dir = dir.path().join("loop");
    let linked_dir = dir.path().join("linked");
    std::fs::create_dir_all(&real_dir).unwrap();
    write_behavior_trajectory(&real_dir.join("real.json"), "real", 200);

    if !create_symlink_dir_for_test(dir.path(), &loop_dir)
        || !create_symlink_dir_for_test(&real_dir, &linked_dir)
    {
        return;
    }

    let (candidates, stats) = collect_behavior_trajectory_candidates_from_dirs_with_stats(
        &[dir.path().to_path_buf()],
        8,
        16,
    );
    let names = behavior_candidate_file_names(&candidates);

    assert_eq!(names, vec!["real.json"]);
    assert_eq!(stats.matching_files_considered, 1);
    assert!(stats.visited_entries <= 4);
}

#[test]
fn behavior_trajectory_recent_bias_applies_within_visit_budget() {
    let dir = tempfile::tempdir().unwrap();
    let visited_dir = dir.path().join("visited");
    let unvisited_dir = dir.path().join("unvisited");
    std::fs::create_dir_all(&visited_dir).unwrap();
    std::fs::create_dir_all(&unvisited_dir).unwrap();
    for idx in 0..4 {
        write_behavior_trajectory(
            &visited_dir.join(format!("chat_{idx}.json")),
            &format!("chat-{idx}"),
            100 + idx as i64,
        );
    }
    write_behavior_trajectory(&unvisited_dir.join("chat_newest.json"), "newest", 10_000);

    let (candidates, stats) = collect_behavior_trajectory_candidates_from_dirs_with_stats(
        &[visited_dir, unvisited_dir],
        2,
        4,
    );
    let names = behavior_candidate_file_names(&candidates);

    assert_eq!(stats.visited_entries, 4);
    assert_eq!(names, vec!["chat_3.json", "chat_2.json"]);
}

#[test]
fn behavior_trajectory_meta_collection_includes_newer_files_beyond_cap_boundary() {
    let dir = tempfile::tempdir().unwrap();
    for idx in 0..4 {
        let path = dir.path().join(format!("a_old_{idx}.json"));
        write_behavior_trajectory(&path, &format!("old-{idx}"), 100 + idx as i64);
    }
    let new_path = dir.path().join("z_new.json");
    write_behavior_trajectory(&new_path, "newest", 10_000);

    let candidates =
        collect_behavior_trajectory_candidates_from_dirs(&[dir.path().to_path_buf()], 3);
    let mut metas = Vec::new();
    let mut seen = HashSet::new();
    collect_behavior_trajectory_metas_from_candidates(candidates, &mut metas, &mut seen);

    assert!(metas.iter().any(|meta| meta.id == "newest"));
    assert_eq!(metas.len(), 3);
}

#[test]
fn behavior_trajectory_meta_accepts_realistic_schema_aliases() {
    let path = PathBuf::from("realistic.json");
    let meta = parse_behavior_trajectory_meta(
        &serde_json::json!({
            "chat_id": "chat-realistic",
            "title": "Realistic Chat",
            "mode": "agent",
            "created_at": "2026-01-01T00:00:00Z",
            "last_message_at": "2026-01-02T00:00:00Z",
            "messages": [{
                "id": "nested-message-id",
                "role": "user",
                "content": "I prefer concise answers."
            }]
        })
        .to_string(),
        path.clone(),
    )
    .unwrap();

    assert_eq!(meta.id, "chat-realistic");
    assert_eq!(meta.title, "Realistic Chat");
    assert_eq!(meta.mode, "agent");
    assert_eq!(meta.updated_at, "2026-01-02T00:00:00Z");
    assert_eq!(meta.path, path);
}

#[test]
fn behavior_trajectory_meta_excludes_internal_link_types() {
    let meta = parse_behavior_trajectory_meta(
        &serde_json::json!({
            "id": "internal-report",
            "title": "Internal report",
            "updated_at": "2026-01-02T00:00:00Z",
            "link_type": "internal:buddy-autonomous",
            "messages": [{"role": "user", "content": "internal prompt"}]
        })
        .to_string(),
        PathBuf::from("internal.json"),
    );

    assert!(meta.is_none());
}

#[test]
fn behavior_trajectory_meta_uses_top_level_id_only() {
    let meta = parse_behavior_trajectory_meta(
        &serde_json::json!({
            "title": "Nested Only",
            "created_at": "2026-01-01T00:00:00Z",
            "messages": [{"id": "nested-message-id", "role": "user"}]
        })
        .to_string(),
        PathBuf::from("nested.json"),
    );

    assert!(meta.is_none());
}

#[test]
fn habit_evidence_is_deterministic_for_task_status_and_fact_order() {
    let mut first = context_with_last_result(None);
    first.pulse.tasks.recent_stuck_alerts_1h = 1;
    first.pulse.tasks.by_status = HashMap::from([
        ("completed".to_string(), 2),
        ("active".to_string(), 1),
        ("planning".to_string(), 3),
    ]);
    first.facts = vec![
        test_fact(
            BuddyFactKind::ChatRetryStreak,
            serde_json::json!({"count": 4, "summary": "retry streak"}),
        ),
        test_fact(
            BuddyFactKind::TaskStuck,
            serde_json::json!({"count": 1, "summary": "stuck task"}),
        ),
    ];
    let mut second = context_with_last_result(None);
    second.pulse.tasks.recent_stuck_alerts_1h = 1;
    second.pulse.tasks.by_status = HashMap::from([
        ("planning".to_string(), 3),
        ("completed".to_string(), 2),
        ("active".to_string(), 1),
    ]);
    second.facts = first.facts.iter().rev().cloned().collect();

    let first_evidence = habit_evidence(&first).unwrap();
    let second_evidence = habit_evidence(&second).unwrap();
    let first_spec = build_spec(USER_HABIT_COACH_WORKFLOW_ID, first_evidence.clone());
    let second_spec = build_spec(USER_HABIT_COACH_WORKFLOW_ID, second_evidence.clone());

    assert_eq!(first_evidence.evidence, second_evidence.evidence);
    assert!(
        first_evidence
            .evidence
            .find("task_status active=1")
            .unwrap()
            < first_evidence
                .evidence
                .find("task_status completed=2")
                .unwrap()
    );
    assert_eq!(first_spec.signal_hash, second_spec.signal_hash);
}

#[test]
fn habit_evidence_labels_current_task_state_and_recent_alerts() {
    let mut ctx = context_with_last_result(None);
    ctx.pulse.tasks.total = 5;
    ctx.pulse.tasks.by_status =
        HashMap::from([("planning".to_string(), 4), ("active".to_string(), 1)]);
    ctx.pulse.tasks.recent_stuck_alerts_1h = 2;
    ctx.pulse.tasks.recent_abandoned_alerts_24h = 3;

    let evidence = habit_evidence(&ctx).unwrap().evidence;

    assert!(evidence.contains("task_pulse current_total=5"));
    assert!(evidence.contains("recent_stuck_alerts_1h=2"));
    assert!(evidence.contains("recent_abandoned_alerts_24h=3"));
    assert!(evidence.contains("task_status planning=4"));
    assert!(!evidence.contains("task_pulse total=5 stuck=2 abandoned=3"));
}

#[test]
fn model_cost_trigger_uses_aggregate_stats_only() {
    let events = (0..8)
        .map(|i| test_llm_event(i, i >= 4, 25_000, 50_000, 0.20))
        .collect::<Vec<_>>();

    let evidence = model_cost_evidence_from_events(&events).unwrap();

    assert!(evidence.evidence.contains("failure_rate_bucket"));
    assert!(evidence.evidence.contains("total_tokens_bucket"));
    assert!(evidence.evidence.contains("cost_usd_bucket"));
    assert!(!evidence.evidence.contains("timeout"));
    assert!(!evidence.evidence.contains("chat-"));
    assert!(!evidence.evidence.contains("total_tokens=400000"));
}

#[test]
fn model_cost_evidence_excludes_buddy_autonomous_report_events() {
    let mut events = (0..5)
        .map(|i| test_llm_event(i, true, 1_000, 1_000, 0.01))
        .collect::<Vec<_>>();
    for (idx, meta) in AUTONOMOUS_BUDDY_WORKFLOWS.iter().enumerate() {
        let mut report = test_llm_event(20 + idx as u64, false, 60_000, 1_000_000, 10.0);
        report.mode = "buddy".to_string();
        report.chat_id = format!("buddy-{}-report", meta.id);
        events.push(report);
    }

    assert!(model_cost_evidence_from_events(&events).is_none());

    let input_events = model_cost_input_events(&events);
    assert_eq!(input_events.len(), 5);
    assert!(input_events
        .iter()
        .all(|event| !is_buddy_autonomous_report_event(event)));
    for meta in AUTONOMOUS_BUDDY_WORKFLOWS {
        assert!(is_autonomous_buddy_workflow_identifier(&format!(
            "buddy-{}-report",
            meta.id
        )));
    }
}

#[test]
fn model_cost_input_includes_normal_buddy_mode_user_events() {
    let mut buddy_event = test_llm_event(1, true, 1_000, 1_000, 0.01);
    buddy_event.mode = "buddy".to_string();
    buddy_event.chat_id = "user-buddy-conversation".to_string();
    let mut report_event = test_llm_event(2, true, 1_000, 1_000, 0.01);
    report_event.mode = "buddy".to_string();
    report_event.chat_id = "buddy-refact_error_detective-report".to_string();

    let input_events = model_cost_input_events(&[buddy_event.clone(), report_event]);

    assert!(is_autonomous_buddy_workflow_identifier(
        "buddy-refact_error_detective-550e8400-e29b-41d4-a716-446655440000"
    ));
    assert!(!is_autonomous_buddy_workflow_identifier(
        "user-buddy-conversation"
    ));
    assert_eq!(input_events.len(), 1);
    assert_eq!(input_events[0].id, buddy_event.id);
    assert_eq!(input_events[0].mode, "buddy");
}

#[test]
fn model_cost_signal_is_bucketed_against_one_report_event_noise() {
    let events = (0..8)
        .map(|i| test_llm_event(i, i >= 4, 25_000, 50_000, 0.20))
        .collect::<Vec<_>>();
    let first = build_spec(
        MODEL_COST_OPTIMIZER_WORKFLOW_ID,
        model_cost_evidence_from_events(&events).unwrap(),
    );
    let mut with_report = events.clone();
    let mut report = test_llm_event(40, true, 1_000, 1_000, 0.01);
    report.mode = "buddy".to_string();
    report.chat_id = "buddy-buddy_model_cost_optimizer-report".to_string();
    with_report.push(report);
    let second = build_spec(
        MODEL_COST_OPTIMIZER_WORKFLOW_ID,
        model_cost_evidence_from_events(&with_report).unwrap(),
    );

    assert_eq!(first.signal_hash, second.signal_hash);
}

#[test]
fn model_cost_evidence_has_stable_model_provider_tie_breakers() {
    let ordered = vec![
        test_llm_event_for_model(0, true, 25_000, 100_000, 0.50, "z/model", "z", "model"),
        test_llm_event_for_model(1, true, 25_000, 100_000, 0.50, "a/model", "a", "model"),
        test_llm_event_for_model(2, true, 25_000, 100_000, 0.50, "m/model", "m", "model"),
        test_llm_event_for_model(3, true, 25_000, 100_000, 0.50, "a/model", "a", "model"),
        test_llm_event_for_model(4, true, 25_000, 100_000, 0.50, "z/model", "z", "model"),
        test_llm_event_for_model(5, true, 25_000, 100_000, 0.50, "m/model", "m", "model"),
    ];
    let reordered = vec![
        ordered[2].clone(),
        ordered[0].clone(),
        ordered[5].clone(),
        ordered[1].clone(),
        ordered[4].clone(),
        ordered[3].clone(),
    ];

    let first = model_cost_evidence_from_events(&ordered).unwrap();
    let second = model_cost_evidence_from_events(&reordered).unwrap();

    assert_eq!(first.evidence, second.evidence);
    assert!(
        first.evidence.find("model id=a/model").unwrap()
            < first.evidence.find("model id=m/model").unwrap()
    );
    assert!(
        first.evidence.find("provider name=a").unwrap()
            < first.evidence.find("provider name=m").unwrap()
    );
}

#[test]
fn same_signal_skips_unchanged_memory_gardener_evidence() {
    let mut ctx = context_with_last_result(None);
    ctx.pulse.memory.orphan = 1;
    ctx.facts = vec![test_fact(
        BuddyFactKind::MemoryOrphan,
        serde_json::json!({"memory_ids": ["mem-a"], "count": 1}),
    )];
    let spec = build_spec(
        MEMORY_GARDENER_WORKFLOW_ID,
        memory_gardener_evidence(&ctx).unwrap(),
    );
    ctx.job_state.last_result = Some(serialize_last_autonomous_result(&AutonomousLastResult {
        signal_hash: spec.signal_hash.clone(),
        chat_id: "chat-a".to_string(),
        completed_at: "2026-01-01T00:00:00Z".to_string(),
        status: None,
        failure_category: None,
        consecutive_failures: None,
    }));

    assert!(same_signal(&ctx, &spec.signal_hash));
    assert!(!same_signal(&ctx, "different"));
}

#[test]
fn redaction_and_capping_remove_obvious_raw_secrets() {
    let raw = "Bearer abcdef12345 password=plainsecret sk-abcdef123456 ghp_abcdef1234567890";
    let redacted = redact_and_cap_text(raw, 512);
    let capped = redact_and_cap_text(&format!("{} {}", raw, "x".repeat(256)), 64);

    assert!(!redacted.contains("abcdef12345"));
    assert!(!redacted.contains("plainsecret"));
    assert!(!redacted.contains("sk-abcdef123456"));
    assert!(!redacted.contains("ghp_abcdef1234567890"));
    assert!(redacted.contains("[REDACTED"));
    assert!(capped.len() <= 64);
    assert!(!capped.contains("plainsecret"));
}

#[test]
fn redaction_scans_beyond_final_cap_without_partial_secret_leaks() {
    let raw = format!(
        "{} password=plainsecret token=othertoken {}",
        "x".repeat(40),
        "y".repeat(10_000)
    );
    let capped = redact_and_cap_text(&raw, 96);

    assert!(capped.len() <= 96);
    assert!(!capped.contains("plainsecret"));
    assert!(!capped.contains("othertoken"));
    assert!(capped.contains("[REDACTED"));
}

#[test]
fn bounded_redaction_window_does_not_split_secret_tokens() {
    let raw = format!("{} sk-{}", "x".repeat(32), "a".repeat(128));
    let (window, truncated) = bounded_redaction_window(&raw, 48);

    assert!(truncated);
    assert!(!window.contains("sk-"));
}

#[test]
fn rendered_autonomous_prompt_contains_no_raw_obvious_secrets() {
    let spec = AutonomousBuddyChatSpec::new(
        "buddy_security_whisperer",
        "Security Whisperer",
        "Check Bearer rawtokenvalue and secret=promptsecret",
        "Found password=evidencesecret sk-abcdef123456 ghp_abcdef1234567890",
    );
    let rendered = render_autonomous_template(
        "Task:\n{{prompt}}\nEvidence:\n{{evidence}}\nSignal:\n{{signal_hash}}",
        &spec,
    );

    for raw in [
        "rawtokenvalue",
        "promptsecret",
        "evidencesecret",
        "sk-abcdef123456",
        "ghp_abcdef1234567890",
    ] {
        assert!(!rendered.contains(raw), "raw secret leaked: {rendered}");
    }
    assert!(rendered.contains("[REDACTED"));
}

#[test]
fn rendered_error_detective_prompt_includes_memory_key_instruction() {
    let stable = signal_hash([ERROR_DETECTIVE_WORKFLOW_ID, "diagnostic-signal"]);
    let spec = AutonomousBuddyChatSpec::new(
        ERROR_DETECTIVE_WORKFLOW_ID,
        "Error Detective",
        "Investigate diagnostics",
        "Diagnostic signal evidence",
    )
    .with_signal_hash(stable.clone());
    let rendered = render_autonomous_template("Task:\n{{prompt}}\nEvidence:\n{{evidence}}", &spec);
    let expected_key = format!("diag:signal:{}", &stable[..16]);

    assert!(rendered.contains(&format!("Memory idempotency key: {expected_key}")));
    assert!(rendered.contains(MEMORY_KEY_SAVE_INSTRUCTION));
}

#[test]
fn dependency_manifest_classifier_recognizes_common_manifests() {
    assert_eq!(
        classify_dependency_manifest("package.json"),
        Some("javascript")
    );
    assert_eq!(
        classify_dependency_manifest("web/pnpm-lock.yaml"),
        Some("javascript")
    );
    assert_eq!(
        classify_dependency_manifest("refact-agent/engine/Cargo.toml"),
        Some("rust")
    );
    assert_eq!(
        classify_dependency_manifest("requirements.txt"),
        Some("python")
    );
    assert_eq!(classify_dependency_manifest("go.mod"), Some("go"));
    assert_eq!(classify_dependency_manifest("README.md"), None);
}

#[test]
fn dependency_manifest_trigger_uses_changed_manifests_or_many_local_manifests() {
    let changed = DependencyManifestEvidence {
        changed_manifests: vec![PathStatus {
            path: "Cargo.toml".to_string(),
            status: "modified".to_string(),
        }],
        ..Default::default()
    };
    let many = DependencyManifestEvidence {
        manifest_counts: BTreeMap::from([("javascript".to_string(), 12)]),
        total_manifest_count: 12,
        ..Default::default()
    };
    let quiet = DependencyManifestEvidence {
        total_manifest_count: 2,
        ..Default::default()
    };

    assert!(dependency_manifest_trigger(&changed));
    assert!(dependency_manifest_trigger(&many));
    assert!(!dependency_manifest_trigger(&quiet));
}

#[test]
fn docs_and_code_path_classifiers_split_docs_from_code() {
    assert!(is_docs_path("README.md"));
    assert!(is_docs_path("docs/setup.mdx"));
    assert!(is_docs_path("AGENTS.md"));
    assert!(!is_docs_path("src/main.rs"));
    assert!(is_code_path("src/main.rs"));
    assert!(is_code_path("refact-agent/gui/src/App.tsx"));
    assert!(!is_code_path("docs/architecture.md"));
    assert!(!is_code_path("package-lock.json"));
}

#[test]
fn docs_trigger_detects_code_without_docs_and_missing_docs() {
    let code_without_docs = DocsEvidence {
        changed_code_paths: vec![PathStatus {
            path: "src/lib.rs".to_string(),
            status: "modified".to_string(),
        }],
        has_readme: true,
        has_agents: true,
        docs_file_count: 1,
        ..Default::default()
    };
    let with_docs = DocsEvidence {
        changed_code_paths: code_without_docs.changed_code_paths.clone(),
        changed_doc_paths: vec![PathStatus {
            path: "README.md".to_string(),
            status: "modified".to_string(),
        }],
        has_readme: true,
        has_agents: true,
        docs_file_count: 1,
    };
    let missing_agents = DocsEvidence {
        has_readme: true,
        has_agents: false,
        docs_file_count: 1,
        ..Default::default()
    };

    assert!(docs_trigger(&code_without_docs));
    assert!(!docs_trigger(&with_docs));
    assert!(docs_trigger(&missing_agents));
}

#[test]
fn dependency_manifest_count_skips_symlinked_directories() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real");
    let linked_dir = dir.path().join("linked");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::write(real_dir.join("Cargo.toml"), "[package]\nname='real'\n").unwrap();

    if !create_symlink_dir_for_test(&real_dir, &linked_dir) {
        return;
    }

    let counts = count_dependency_manifests(dir.path());

    assert_eq!(counts.get("rust"), Some(&1));
}

#[test]
fn docs_file_count_skips_symlinked_directories() {
    let dir = tempfile::tempdir().unwrap();
    let docs_dir = dir.path().join("docs");
    let linked_dir = dir.path().join("linked-docs");
    std::fs::create_dir_all(&docs_dir).unwrap();
    std::fs::write(docs_dir.join("guide.md"), "# Guide\n").unwrap();

    if !create_symlink_dir_for_test(&docs_dir, &linked_dir) {
        return;
    }

    let count = count_docs_files(dir.path());

    assert_eq!(count, 1);
}

#[test]
fn architecture_grouping_and_thresholds_detect_drift() {
    let mut same_subsystem = Vec::new();
    for idx in 0..8 {
        same_subsystem.push(PathStatus {
            path: format!("src/chat/file_{idx}.rs"),
            status: "modified".to_string(),
        });
    }
    let evidence = ArchitectureEvidence {
        changed_file_count: same_subsystem.len(),
        additions: 10,
        deletions: 5,
        path_groups: architecture_groups(&same_subsystem),
    };
    assert_eq!(evidence.path_groups[0].group, "src/chat");
    assert_eq!(evidence.path_groups[0].count, 8);
    assert!(architecture_trigger(&evidence));

    let broad = ArchitectureEvidence {
        changed_file_count: 12,
        additions: 10,
        deletions: 5,
        path_groups: vec![
            PathGroup {
                group: "a".to_string(),
                count: 3,
            },
            PathGroup {
                group: "b".to_string(),
                count: 3,
            },
            PathGroup {
                group: "c".to_string(),
                count: 2,
            },
            PathGroup {
                group: "d".to_string(),
                count: 2,
            },
            PathGroup {
                group: "e".to_string(),
                count: 2,
            },
        ],
    };
    assert!(architecture_trigger(&broad));

    let large_stats = ArchitectureEvidence {
        changed_file_count: 2,
        additions: 500,
        deletions: 250,
        path_groups: vec![PathGroup {
            group: "src".to_string(),
            count: 2,
        }],
    };
    assert!(architecture_trigger(&large_stats));

    let small = ArchitectureEvidence {
        changed_file_count: 2,
        additions: 20,
        deletions: 10,
        path_groups: vec![PathGroup {
            group: "src".to_string(),
            count: 2,
        }],
    };
    assert!(!architecture_trigger(&small));
}

#[test]
fn security_scanner_redacts_secret_values() {
    let raw = "const token = \"ghp_abcdefghijklmnopqrstuvwxyz\"; password=plainsecret";
    let findings = scan_security_findings("src/config.ts", raw, 10);
    let rendered = render_security_evidence(&findings);

    assert!(!findings.is_empty());
    assert!(!rendered.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
    assert!(!rendered.contains("plainsecret"));
    assert!(rendered.contains("[REDACTED"));
    assert!(rendered.contains("src/config.ts"));
}

#[test]
fn security_scanner_detects_quoted_assignments_and_authorization_headers() {
    let raw = r#"
        api_key = "super-secret-key"
        token: 'quoted-token'
        password = "quoted-password"
        Authorization: "Bearer secret-auth-token"
    "#;
    let findings = scan_security_findings("src/config.ts", raw, 10);
    let rendered = render_security_evidence(&findings);

    assert!(findings
        .iter()
        .any(|finding| finding.kind == "assigned_secret"));
    assert!(findings
        .iter()
        .any(|finding| finding.kind == "authorization_header"));
    assert!(!rendered.contains("super-secret-key"));
    assert!(!rendered.contains("quoted-token"));
    assert!(!rendered.contains("quoted-password"));
    assert!(!rendered.contains("secret-auth-token"));
    assert!(rendered.contains("[REDACTED"));
}

#[test]
fn security_scanner_detects_json_style_quoted_secret_keys() {
    let raw = r#"{
        "api_key": "json-api-secret",
        "password": "json-password-secret",
        "Authorization": "Bearer json-auth-secret"
    }"#;
    let findings = scan_security_findings("config.json", raw, 10);
    let rendered = render_security_evidence(&findings);

    assert!(findings
        .iter()
        .any(|finding| finding.kind == "assigned_secret"));
    assert!(findings
        .iter()
        .any(|finding| finding.kind == "authorization_header"));
    assert!(!rendered.contains("json-api-secret"));
    assert!(!rendered.contains("json-password-secret"));
    assert!(!rendered.contains("json-auth-secret"));
    assert!(rendered.contains("[REDACTED"));
}

#[test]
fn security_scanner_detects_compound_credential_keys() {
    let raw = r#"{
        "client_secret": "client-secret-value",
        "access_token": "access-token-value",
        "refresh-token": "refresh-token-value",
        "private_key": "private-key-value"
    }
    id_token = "id-token-value"
    "#;
    let findings = scan_security_findings("config.json", raw, 10);
    let rendered = render_security_evidence(&findings);

    assert!(findings
        .iter()
        .any(|finding| finding.kind == "assigned_secret"));
    for secret in [
        "client-secret-value",
        "access-token-value",
        "refresh-token-value",
        "private-key-value",
        "id-token-value",
    ] {
        assert!(!rendered.contains(secret));
    }
    assert!(rendered.contains("[REDACTED"));
}

#[test]
fn staged_index_only_secret_is_detected_after_worktree_cleaned() {
    let (dir, repo) = init_temp_git_repo();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/config.ts"),
        "const token = \"ghp_abcdefghijklmnopqrstuvwxyz\";\n",
    )
    .unwrap();
    git_add(&repo, "src/config.ts");
    std::fs::write(
        root.join("src/config.ts"),
        "const token = \"not-secret\";\n",
    )
    .unwrap();

    let evidence = collect_local_git_evidence(root, true).unwrap();
    let rendered = render_security_evidence(&evidence.security_findings);

    assert_eq!(evidence.security_findings.len(), 1);
    assert_eq!(evidence.security_findings[0].path, "src/config.ts");
    assert_eq!(evidence.security_findings[0].source, "index");
    assert!(rendered.contains("source=index"));
    assert!(rendered.contains("kind=github_token"));
    assert!(rendered.contains("[REDACTED"));
    assert!(!rendered.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn mixed_staged_and_worktree_versions_scan_both_sources() {
    let (dir, repo) = init_temp_git_repo();
    let root = dir.path();
    std::fs::write(root.join("config.env"), "token=stagedsecret\n").unwrap();
    git_add(&repo, "config.env");
    std::fs::write(root.join("config.env"), "token=worktreesecret\n").unwrap();

    let evidence = collect_local_git_evidence(root, true).unwrap();
    let rendered = render_security_evidence(&evidence.security_findings);
    let mut keys = evidence
        .security_findings
        .iter()
        .map(|finding| {
            (
                finding.path.as_str(),
                finding.source.as_str(),
                finding.kind.as_str(),
                finding.preview.as_str(),
            )
        })
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();

    assert_eq!(keys.len(), evidence.security_findings.len());
    assert!(evidence
        .security_findings
        .iter()
        .any(|finding| finding.source == "index"));
    assert!(evidence
        .security_findings
        .iter()
        .any(|finding| finding.source == "worktree"));
    assert!(rendered.contains("source=index"));
    assert!(rendered.contains("source=worktree"));
    assert!(!rendered.contains("stagedsecret"));
    assert!(!rendered.contains("worktreesecret"));
    assert!(rendered.contains("[REDACTED"));
}

#[test]
fn large_untracked_file_is_skipped_by_diff_stats() {
    let (dir, repo) = init_temp_git_repo();
    let root = dir.path();
    std::fs::write(root.join("small.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(
        root.join("large.txt"),
        format!("{}\n", "x".repeat(MAX_UNTRACKED_CONTENT_BYTES as usize + 1)),
    )
    .unwrap();

    let (additions, deletions) = diff_stats(&repo).unwrap();

    assert_eq!(additions, 3);
    assert_eq!(deletions, 0);
}

#[test]
fn binary_untracked_file_does_not_fail_evidence_collection() {
    let (dir, repo) = init_temp_git_repo();
    let root = dir.path();
    std::fs::write(root.join("binary.dat"), b"abc\0def\xff").unwrap();
    std::fs::write(root.join("notes.txt"), "hello\nworld\n").unwrap();

    let evidence = collect_local_git_evidence(root, true).unwrap();
    let (additions, deletions) = diff_stats(&repo).unwrap();

    assert_eq!(additions, 2);
    assert_eq!(deletions, 0);
    assert_eq!(evidence.additions, 2);
    assert_eq!(evidence.deletions, 0);
    assert!(evidence.security_findings.is_empty());
    assert!(evidence
        .changed_paths
        .iter()
        .any(|path| path.path == "binary.dat"));
}

#[test]
fn unchanged_signal_produces_no_chat_in_helper_logic() {
    let spec = build_autonomous_job_spec(
        dependency_radar_definition(),
        "Local dependency manifest evidence:\n- total_manifest_count: 12".to_string(),
    );
    let result = AutonomousLastResult {
        signal_hash: spec.signal_hash.clone(),
        chat_id: "chat-a".to_string(),
        completed_at: "2026-01-01T00:00:00Z".to_string(),
        status: None,
        failure_category: None,
        consecutive_failures: None,
    };
    let ctx = context_with_last_result(Some(serialize_last_autonomous_result(&result)));

    assert!(same_signal(&ctx, &spec.signal_hash));
}

#[test]
fn local_signal_should_run_helper_skips_same_signal() {
    let evidence = DependencyManifestEvidence {
        manifest_counts: BTreeMap::from([("javascript".to_string(), 12)]),
        total_manifest_count: 12,
        ..Default::default()
    };
    let spec = build_autonomous_job_spec(
        dependency_radar_definition(),
        render_dependency_evidence(&evidence),
    );
    let result = AutonomousLastResult {
        signal_hash: spec.signal_hash.clone(),
        chat_id: "chat-a".to_string(),
        completed_at: "2026-01-01T00:00:00Z".to_string(),
        status: None,
        failure_category: None,
        consecutive_failures: None,
    };
    let ctx = context_with_last_result(Some(serialize_last_autonomous_result(&result)));

    assert!(dependency_manifest_trigger(&evidence));
    assert!(same_signal(&ctx, &spec.signal_hash));
}

#[tokio::test]
async fn dependency_radar_should_run_does_not_scan_git() {
    let job = DependencyRadarJob;
    let mut ctx = context_with_last_result(None);
    ctx.pulse.git.uncommitted_files = 1;
    let gcx = AppState::from_gcx(crate::global_context::tests::make_test_gcx().await).await;
    reset_local_git_evidence_scan_count(&ctx.project_root);

    assert!(job.should_run(gcx, &ctx).await);

    assert_eq!(local_git_evidence_scan_count(&ctx.project_root), 0);
}

#[tokio::test]
async fn dependency_radar_execute_scans_git_once_without_should_run_duplication() {
    let (dir, _repo) = init_temp_git_repo();
    let root = dir.path();
    std::fs::write(root.join("notes.txt"), "local note\n").unwrap();
    let mut ctx = context_with_last_result(None);
    ctx.project_root = root.to_path_buf();
    ctx.pulse.git.uncommitted_files = 1;
    let job = DependencyRadarJob;
    let gcx = AppState::from_gcx(crate::global_context::tests::make_test_gcx().await).await;

    assert!(job.should_run(gcx.clone(), &ctx).await);
    reset_local_git_evidence_scan_count(&ctx.project_root);
    let root = ctx.project_root.clone();
    let result = job.execute(gcx, ctx).await;

    assert_eq!(local_git_evidence_scan_count(&root), 1);
    assert!(result.activity.is_none());
    assert!(result.runtime_event.is_none());
    assert!(result.last_result.is_none());
}

#[tokio::test]
async fn same_signal_execution_returns_no_visible_output_and_preserves_last_result() {
    let evidence = DependencyManifestEvidence {
        manifest_counts: BTreeMap::from([("javascript".to_string(), 12)]),
        total_manifest_count: 12,
        ..Default::default()
    };
    let spec = build_autonomous_job_spec(
        dependency_radar_definition(),
        render_dependency_evidence(&evidence),
    );
    let stored = serialize_last_autonomous_result(&AutonomousLastResult {
        signal_hash: spec.signal_hash.clone(),
        chat_id: "chat-a".to_string(),
        completed_at: "2026-01-01T00:00:00Z".to_string(),
        status: None,
        failure_category: None,
        consecutive_failures: None,
    });
    let ctx = context_with_last_result(Some(stored.clone()));
    let gcx = AppState::from_gcx(crate::global_context::tests::make_test_gcx().await).await;

    let result = execute_dependency_radar_with_evidence(gcx, &ctx, evidence).await;

    assert!(result.activity.is_none());
    assert!(result.runtime_event.is_none());
    assert!(result.speech.is_none());
    assert!(result.suggestion.is_none());
    assert_eq!(result.last_result.as_deref(), Some(stored.as_str()));
}

#[test]
fn autonomous_failure_result_classifies_model_context_and_tool_errors() {
    let spec = AutonomousBuddyChatSpec::new(
        DEPENDENCY_RADAR_WORKFLOW_ID,
        "Dependency Radar",
        "Prompt",
        "Evidence",
    );
    let ctx = context_with_last_result(None);

    let model = autonomous_failure_result(
        &ctx,
        &spec,
        "OpenAI 404: model refact/gpt-4.1-nano not found".to_string(),
    )
    .workflow_failure
    .expect("model failure");
    assert_eq!(model.category, WorkflowFailureCategory::ModelUnavailable);
    assert!(model.summary.contains("Model unavailable"));

    let context = autonomous_failure_result(
        &ctx,
        &spec,
        "This model's maximum context length is 128000 tokens".to_string(),
    )
    .workflow_failure
    .expect("context failure");
    assert_eq!(context.category, WorkflowFailureCategory::ContextTooLarge);

    let tool = autonomous_failure_result(
        &ctx,
        &spec,
        "Error: tool 'buddy_log_activity' not found".to_string(),
    )
    .workflow_failure
    .expect("tool failure");
    assert_eq!(tool.category, WorkflowFailureCategory::ToolUnavailable);
}

#[tokio::test]
async fn autonomous_success_result_includes_visible_runtime_event_with_stable_dedupe() {
    let ctx = context_with_last_result(None);
    let gcx = AppState::from_gcx(crate::global_context::tests::make_test_gcx().await).await;
    let spec = build_autonomous_job_spec(
        dependency_radar_definition(),
        "Local dependency manifest evidence:\n- total_manifest_count: 12".to_string(),
    );
    let expected_key = format!("{}:{}", spec.workflow_id, spec.signal_hash);

    let result = autonomous_success_result(gcx, &ctx, spec, "chat-success".to_string()).await;

    let event = result.runtime_event.expect("runtime event");
    assert_eq!(event.status, "completed");
    assert_eq!(event.dedupe_key.as_deref(), Some(expected_key.as_str()));
    assert_eq!(event.chat_id.as_deref(), Some("chat-success"));
    assert_eq!(event.ttl_ms, Some(30_000));
    assert!(event
        .speech_text
        .as_deref()
        .is_some_and(|text| { text.contains("Dependency Radar") && text.contains("details") }));
    assert!(result.activity.is_some());
    assert!(result.last_result.is_some());
}

#[tokio::test]
async fn autonomous_chat_disabled_when_kill_switch_off() {
    let dir = tempfile::tempdir().unwrap();
    crate::buddy::storage::bootstrap_buddy_storage(dir.path())
        .await
        .unwrap();
    let mut settings = BuddySettings::default();
    settings.autonomous_chats_enabled = false;
    save_settings(dir.path(), &settings).await.unwrap();
    let gcx = crate::global_context::tests::make_test_gcx().await;
    {
        *crate::app_state::AppState::from_gcx(gcx.clone())
            .await
            .workspace
            .documents_state
            .workspace_folders
            .lock()
            .unwrap() = vec![dir.path().to_path_buf()];
    }
    let gcx = AppState::from_gcx(gcx).await;
    let spec = AutonomousBuddyChatSpec::new(
        "buddy_security_whisperer",
        "Security Whisperer",
        "Prompt",
        "Evidence",
    );
    let err = run_autonomous_buddy_chat(gcx, spec).await.unwrap_err();

    assert_eq!(err, "autonomous chats disabled");
}

#[tokio::test]
async fn build_autonomous_messages_render_safe_user_prompt() {
    let gcx = AppState::from_gcx(crate::global_context::tests::make_test_gcx().await).await;
    let spec = AutonomousBuddyChatSpec::new(
        "buddy_security_whisperer",
        "Security Whisperer",
        "Check token=promptsecret",
        "Found password=evidencesecret",
    );
    let (messages, max_steps, tools) = build_autonomous_messages(gcx, &spec).await.unwrap();

    assert_eq!(max_steps, 10);
    assert!(tools.contains(&"buddy_runtime_event".to_string()));
    assert_eq!(messages.len(), 2);
    let ChatContent::SimpleText(user_prompt) = &messages[1].content else {
        panic!("expected simple text user prompt");
    };
    assert!(!user_prompt.contains("promptsecret"));
    assert!(!user_prompt.contains("evidencesecret"));
    assert!(user_prompt.contains("[REDACTED"));
}

#[tokio::test]
async fn run_autonomous_buddy_chat_rejects_invalid_workflow_id() {
    let gcx = AppState::from_gcx(crate::global_context::tests::make_test_gcx().await).await;
    let spec = AutonomousBuddyChatSpec::new("../bad", "Bad", "Prompt", "Evidence");
    let err = run_autonomous_buddy_chat(gcx, spec).await.unwrap_err();

    assert!(err.contains("invalid autonomous buddy workflow id"));
}
