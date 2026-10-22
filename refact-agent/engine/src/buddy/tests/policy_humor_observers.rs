use super::*;

// =============================================================================
// Policy tests
// =============================================================================
fn make_opp_with_priority(id: &str, priority: BuddyPriority) -> BuddyOpportunity {
    let mut opp = make_opportunity(id, id);
    opp.priority = priority;
    opp
}

#[test]
fn policy_drops_when_proactive_disabled() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let mut settings = BuddySettings::default();
    settings.proactive_enabled = false;
    let opp = make_opp_with_priority("opp1", BuddyPriority::Normal);
    let queue = OpportunityQueue::new();
    let result = evaluate(&opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Drop {
            reason: "proactive_disabled"
        }
    ));
}

#[test]
fn policy_quiet_mode_drops_non_critical() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let mut settings = BuddySettings::default();
    settings.quiet_mode = true;
    let queue = OpportunityQueue::new();

    let normal_opp = make_opp_with_priority("opp-normal", BuddyPriority::Normal);
    let result = evaluate(&normal_opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Drop {
            reason: "quiet_mode"
        }
    ));

    let critical_opp = make_opp_with_priority("opp-critical", BuddyPriority::Critical);
    let result = evaluate(&critical_opp, &settings, &queue);
    assert!(matches!(result, PolicyDecision::Surface { .. }));
}

#[test]
fn policy_unread_cap_drops() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::{OpportunityQueue, MAX_UNREAD};
    let settings = BuddySettings::default();
    let mut queue = OpportunityQueue::new();
    for i in 0..MAX_UNREAD {
        push_opportunity(
            &mut queue,
            make_opportunity(&format!("pre-{}", i), &format!("ck-pre-{}", i)),
        );
    }
    assert_eq!(queue.unread_count(), MAX_UNREAD);
    let opp = make_opp_with_priority("new-opp", BuddyPriority::Normal);
    let result = evaluate(&opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Drop {
            reason: "unread_cap"
        }
    ));
}

#[test]
fn policy_dismissed_24h_drops() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let settings = BuddySettings::default();
    let mut queue = OpportunityQueue::new();
    let opp = make_opportunity("opp-dm", "key-dm");
    push_opportunity(&mut queue, opp.clone());
    queue.dismiss("opp-dm");
    let new_opp = make_opportunity("opp-new", "key-dm");
    let result = evaluate(&new_opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Drop {
            reason: "dismissed_24h"
        }
    ));
}

#[test]
fn policy_cooldown_drops() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let settings = BuddySettings::default();
    let mut queue = OpportunityQueue::new();
    push_opportunity(&mut queue, make_opportunity("opp-cd", "cooldown-key"));
    assert!(queue.cooldown_active("cooldown-key"));
    let new_opp = make_opportunity("opp-cd2", "cooldown-key");
    let result = evaluate(&new_opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Drop { reason: "cooldown" }
    ));
}

#[test]
fn policy_humor_blocked_by_keyword() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let settings = BuddySettings::default();
    let queue = OpportunityQueue::new();
    let mut opp = make_opp_with_priority("opp-auth", BuddyPriority::Normal);
    opp.summary = "auth token expired".to_string();
    let result = evaluate(&opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Surface {
            humor_allowed: false
        }
    ));
}

#[test]
fn policy_humor_blocked_by_priority() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let settings = BuddySettings::default();
    let queue = OpportunityQueue::new();
    let opp = make_opp_with_priority("opp-high", BuddyPriority::High);
    let result = evaluate(&opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Surface {
            humor_allowed: false
        }
    ));
}

#[test]
fn policy_humor_off_setting() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let mut settings = BuddySettings::default();
    settings.humor_level = HumorLevel::Off;
    let queue = OpportunityQueue::new();
    let opp = make_opp_with_priority("opp-off", BuddyPriority::Normal);
    let result = evaluate(&opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Surface {
            humor_allowed: false
        }
    ));
}

#[test]
fn policy_humor_disabled_setting() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let mut settings = BuddySettings::default();
    settings.humor_enabled = false;
    settings.humor_level = HumorLevel::Normal;
    let queue = OpportunityQueue::new();
    let opp = make_opp_with_priority("opp-disabled", BuddyPriority::Normal);
    let result = evaluate(&opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Surface {
            humor_allowed: false
        }
    ));
}

#[test]
fn policy_default_surfaces() {
    use super::policy::{evaluate, PolicyDecision};
    use super::opportunities::OpportunityQueue;
    let settings = BuddySettings::default();
    let queue = OpportunityQueue::new();
    let opp = make_opp_with_priority("opp-ok", BuddyPriority::Normal);
    let result = evaluate(&opp, &settings, &queue);
    assert!(matches!(
        result,
        PolicyDecision::Surface {
            humor_allowed: true
        }
    ));
}

// =============================================================================
// Humor tests
// =============================================================================

use std::sync::atomic::{AtomicU32, Ordering};

struct CountingGenerator {
    count: std::sync::Arc<AtomicU32>,
    lines: Vec<String>,
}

#[async_trait::async_trait]
impl super::humor::HumorGenerator for CountingGenerator {
    async fn generate(
        &self,
        _kind: BuddyFactKind,
        _summary: String,
        _gcx: AppState,
    ) -> Vec<String> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.lines.clone()
    }
}

struct EmptyGenerator;

#[async_trait::async_trait]
impl super::humor::HumorGenerator for EmptyGenerator {
    async fn generate(
        &self,
        _kind: BuddyFactKind,
        _summary: String,
        _gcx: AppState,
    ) -> Vec<String> {
        vec![]
    }
}

#[tokio::test]
async fn humor_uses_cache_before_calling_generator() {
    use super::humor::HumorService;
    let count = std::sync::Arc::new(AtomicU32::new(0));
    let gen = std::sync::Arc::new(CountingGenerator {
        count: count.clone(),
        lines: vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
            "line4".to_string(),
        ],
    });
    let mut svc = HumorService::new_with_generator(gen);
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let pulse = BuddyPulse::default();
    let kind = BuddyFactKind::TaskStuck;

    for i in 0..4 {
        let mut opp = make_opportunity(&format!("opp-h{}", i), &format!("ck-h{}", i));
        apply_humor_plan(&mut svc, &mut opp, kind, &pulse, app.clone()).await;
        assert!(opp.humor.is_some(), "call {} must have humor", i);
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "generator must be called exactly once for 4 calls with 4-line batch"
    );
}

#[tokio::test]
async fn humor_budget_enforced() {
    use super::humor::HumorService;
    let gen = std::sync::Arc::new(CountingGenerator {
        count: std::sync::Arc::new(AtomicU32::new(0)),
        lines: vec!["ha".to_string()],
    });
    let mut svc = HumorService::new_with_generator(gen);
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let pulse = BuddyPulse::default();

    let kinds = [
        BuddyFactKind::TaskStuck,
        BuddyFactKind::TrajectoryClutter,
        BuddyFactKind::ChatRetryStreak,
        BuddyFactKind::MemoryOrphan,
        BuddyFactKind::ModePromptOverlap,
        BuddyFactKind::IntegrationFailing,
    ];

    for (i, &kind) in kinds.iter().enumerate() {
        let mut opp = make_opportunity(&format!("opp-b{}", i), &format!("ck-b{}", i));
        apply_humor_plan(&mut svc, &mut opp, kind, &pulse, app.clone()).await;
        if i < 5 {
            assert!(opp.humor.is_some(), "call {} should have humor", i);
        } else {
            assert!(
                opp.humor.is_none(),
                "6th distinct kind must be blocked by budget"
            );
        }
    }
}

#[tokio::test]
async fn humor_no_fallback_on_empty_lines() {
    use super::humor::HumorService;
    let mut svc = HumorService::new_with_generator(std::sync::Arc::new(EmptyGenerator));
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let pulse = BuddyPulse::default();
    let mut opp = make_opportunity("opp-nf", "ck-nf");
    apply_humor_plan(
        &mut svc,
        &mut opp,
        BuddyFactKind::TaskStuck,
        &pulse,
        app.clone(),
    )
    .await;
    assert!(
        opp.humor.is_none(),
        "humor must remain None when generator returns empty — no fallback"
    );
}

#[tokio::test]
async fn humor_cache_expiry() {
    use super::humor::HumorService;
    let count = std::sync::Arc::new(AtomicU32::new(0));
    let gen = std::sync::Arc::new(CountingGenerator {
        count: count.clone(),
        lines: vec!["line1".to_string(), "line2".to_string()],
    });
    let mut svc = HumorService::new_with_generator(gen);
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let pulse = BuddyPulse::default();
    let kind = BuddyFactKind::TaskStuck;

    let mut opp1 = make_opportunity("opp-ex1", "ck-ex1");
    apply_humor_plan(&mut svc, &mut opp1, kind, &pulse, app.clone()).await;
    assert!(opp1.humor.is_some());
    assert_eq!(count.load(Ordering::SeqCst), 1, "one generation so far");

    let future = chrono::Utc::now() + Duration::hours(2);
    svc.cache_purge_expired(future);

    let mut opp2 = make_opportunity("opp-ex2", "ck-ex2");
    apply_humor_plan(&mut svc, &mut opp2, kind, &pulse, app.clone()).await;
    assert!(opp2.humor.is_some());
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "cache expiry must trigger a fresh generation"
    );
}

struct SlowTimeoutGenerator;

#[async_trait::async_trait]
impl super::humor::HumorGenerator for SlowTimeoutGenerator {
    async fn generate(
        &self,
        _kind: BuddyFactKind,
        _summary: String,
        _gcx: AppState,
    ) -> Vec<String> {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        vec!["too late".to_string()]
    }
}

#[tokio::test]
async fn humor_timeout_returns_none_quickly() {
    use super::humor::{HumorService, HUMOR_TIMEOUT_SECS};
    let mut svc = HumorService::new_with_generator(std::sync::Arc::new(SlowTimeoutGenerator));
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let pulse = BuddyPulse::default();
    let mut opp = make_opportunity("opp-timeout", "ck-timeout");
    let start = std::time::Instant::now();
    apply_humor_plan(&mut svc, &mut opp, BuddyFactKind::TaskStuck, &pulse, app).await;
    let elapsed = start.elapsed();
    assert!(opp.humor.is_none(), "timed out humor must remain None");
    assert!(
        elapsed < tokio::time::Duration::from_secs(HUMOR_TIMEOUT_SECS + 2),
        "humor timeout took too long: {:?}",
        elapsed
    );
}

// =============================================================================
// Observer tests
// =============================================================================
struct SleepObserver {
    id: &'static str,
}

#[async_trait::async_trait]
impl super::observers::BuddyObserver for SleepObserver {
    fn id(&self) -> &'static str {
        self.id
    }

    fn cadence_seconds(&self) -> u64 {
        1
    }

    fn requires_setting(&self, _settings: &BuddySettings) -> bool {
        true
    }

    async fn observe(
        &self,
        _gcx: AppState,
        _ctx: &super::observers::ObserverContext,
    ) -> Vec<BuddyFact> {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        vec![make_fact(
            self.id,
            BuddyFactKind::TaskStuck,
            chrono::Utc::now(),
        )]
    }
}

#[tokio::test]
async fn parallel_observer_execution_not_sequentially_additive() {
    let observers: Vec<Arc<dyn super::observers::BuddyObserver>> = vec![
        Arc::new(SleepObserver { id: "sleep-a" }),
        Arc::new(SleepObserver { id: "sleep-b" }),
    ];
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let start = std::time::Instant::now();
    let facts = super::actor::observe_buddy_facts_parallel(
        observers,
        app,
        std::env::temp_dir(),
        chrono::Utc::now(),
    )
    .await;
    let elapsed = start.elapsed();
    assert_eq!(facts.len(), 2);
    assert!(
        elapsed < tokio::time::Duration::from_millis(1700),
        "parallel observers took too long: {:?}",
        elapsed
    );
}

#[test]
fn task_health_emits_stuck_fact() {
    use super::observers::task_health::{detect_task_health_facts, TaskHealthEntry};
    let now = chrono::Utc::now();
    let heartbeat = now - Duration::hours(5);
    let meta = make_task_meta("t1", "Fix bug", TaskStatus::Active, &now.to_rfc3339());
    let board = TaskBoard {
        schema_version: 1,
        rev: 0,
        columns: vec![],
        cards: vec![make_board_card("c1", "doing", Some("agent-1"), None)],
    };
    let entries = vec![TaskHealthEntry {
        meta,
        board,
        last_heartbeat: Some(heartbeat),
        touched_files: vec![],
    }];
    let facts = detect_task_health_facts(&entries, now);
    assert!(
        facts.iter().any(|f| f.kind == BuddyFactKind::TaskStuck),
        "stuck fact must be emitted"
    );
}

#[test]
fn task_health_no_fact_for_completed() {
    use super::observers::task_health::{detect_task_health_facts, TaskHealthEntry};
    let now = chrono::Utc::now();
    let heartbeat = now - Duration::minutes(20);
    let meta = make_task_meta("t1", "Done task", TaskStatus::Completed, &now.to_rfc3339());
    let board = TaskBoard {
        schema_version: 1,
        rev: 0,
        columns: vec![],
        cards: vec![make_board_card("c1", "doing", Some("agent-1"), None)],
    };
    let entries = vec![TaskHealthEntry {
        meta,
        board,
        last_heartbeat: Some(heartbeat),
        touched_files: vec![],
    }];
    let facts = detect_task_health_facts(&entries, now);
    assert!(
        facts.iter().all(|f| f.kind != BuddyFactKind::TaskStuck),
        "completed task must not emit stuck fact"
    );
}

#[test]
fn trajectory_clutter_threshold() {
    use super::observers::trajectory_clutter::detect_trajectory_clutter_facts;
    let now = chrono::Utc::now();
    let facts = detect_trajectory_clutter_facts("hash123", 51, 0, 0, now);
    assert!(
        facts
            .iter()
            .any(|f| f.kind == BuddyFactKind::TrajectoryClutter),
        "fact must be emitted when total > 50"
    );
    let facts_under = detect_trajectory_clutter_facts("hash123", 50, 0, 0, now);
    assert!(
        facts_under.is_empty(),
        "no fact when total <= 50 and untitled <= 15"
    );
}

#[tokio::test]
async fn trajectory_scan_caps_file_reads() {
    use super::observers::trajectory_clutter::{scan_trajectories_dir, MAX_TRAJECTORY_SCAN_FILES};
    let dir = tempfile::tempdir().unwrap();
    for i in 0..(MAX_TRAJECTORY_SCAN_FILES + 20) {
        tokio::fs::write(
            dir.path().join(format!("trajectory_{i:03}.json")),
            r#"{"title":"","created_at":"2026-01-01T00:00:00Z"}"#,
        )
        .await
        .unwrap();
    }
    let (total, untitled, _) = scan_trajectories_dir(dir.path()).await;
    assert_eq!(total, (MAX_TRAJECTORY_SCAN_FILES + 20) as u32);
    assert!(untitled <= MAX_TRAJECTORY_SCAN_FILES as u32);
}

#[test]
fn observer_scan_budget_constants_are_bounded() {
    assert!(
        super::observers::task_health::MAX_TASK_CLUSTER_ENTRIES <= 200,
        "task cluster duplicate scan must stay capped"
    );
    assert!(
        super::observers::customization_drift::MAX_MODE_OVERLAP_CANDIDATES <= 100,
        "mode overlap scan must stay capped"
    );
    assert!(
        super::observers::git_pressure::MAX_DIFF_COMMITS <= 200,
        "git revwalk scan must stay capped"
    );
}

#[test]
fn git_pressure_uncommitted() {
    use super::observers::git_pressure::{count_uncommitted, detect_git_pressure_facts};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    let repo = git2::Repository::init(path).unwrap();
    {
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }
    drop(repo);

    for i in 0..30 {
        std::fs::write(path.join(format!("file_{}.rs", i)), b"fn foo() {}").unwrap();
    }

    let count = count_uncommitted(path).unwrap_or(0);
    assert!(count > 25, "expected >25 uncommitted, got {}", count);

    let now = chrono::Utc::now();
    let facts = detect_git_pressure_facts(path, now);
    assert!(
        facts
            .iter()
            .any(|f| f.kind == BuddyFactKind::UncommittedPressure),
        "uncommitted pressure fact must be emitted"
    );
}

#[tokio::test]
async fn memory_garden_caps_file_count() {
    use super::observers::memory_garden::scan_knowledge_dir_count_for_test;
    let dir = tempfile::tempdir().unwrap();
    for i in 0..600 {
        std::fs::write(dir.path().join(format!("memory_{:03}.md", i)), "# Memory\n").unwrap();
    }
    let count = scan_knowledge_dir_count_for_test(dir.path().to_path_buf()).await;
    assert!(count <= 500, "scanned too many memory files: {}", count);
}

#[tokio::test]
async fn memory_garden_skips_large_files() {
    use super::observers::memory_garden::scan_knowledge_dir_count_for_test;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("large.md"), vec![b'x'; 300 * 1024]).unwrap();
    let count = scan_knowledge_dir_count_for_test(dir.path().to_path_buf()).await;
    assert_eq!(count, 0, "large memory file must be ignored");
}

#[test]
fn git_pressure_discover_works_from_subdir() {
    use super::observers::git_pressure::count_uncommitted;
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    {
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }
    drop(repo);
    let nested = dir.path().join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    for i in 0..30 {
        std::fs::write(dir.path().join(format!("file_{}.rs", i)), b"fn foo() {}").unwrap();
    }
    let count = count_uncommitted(&nested).unwrap_or(0);
    assert!(count > 25, "discover from subdir saw {} files", count);
}

#[test]
fn git_pressure_untracked_dir_scan_is_not_recursive() {
    use super::observers::git_pressure::count_uncommitted;
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    {
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }
    drop(repo);
    let nested = dir.path().join("untracked").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    for i in 0..30 {
        std::fs::write(nested.join(format!("file_{}.rs", i)), b"fn foo() {}").unwrap();
    }

    let count = count_uncommitted(dir.path()).unwrap_or(0);

    assert_eq!(count, 1);
}

#[test]
fn git_diff_widening_counts_single_large_recent_commit_deterministically() {
    use super::observers::git_pressure::git_diff_widening;
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let initial = {
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap()
    };
    for dir_name in ["a", "b", "c"] {
        std::fs::create_dir_all(dir.path().join(dir_name)).unwrap();
        let lines = (0..220)
            .map(|i| format!("line {dir_name} {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join(dir_name).join("file.txt"), lines).unwrap();
    }
    {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.find_commit(initial).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "large", &tree, &[&parent])
            .unwrap();
    }
    drop(repo);

    let first = git_diff_widening(dir.path(), chrono::Utc::now()).unwrap();
    let second = git_diff_widening(dir.path(), chrono::Utc::now()).unwrap();
    assert!(first.0 > 500);
    assert_eq!(first, second);
    assert_eq!(
        first.1,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

#[test]
fn diagnostic_cluster_threshold() {
    use super::observers::diagnostic_cluster::detect_diagnostic_cluster_facts;
    let now = chrono::Utc::now();
    let ts = (now - Duration::minutes(5)).to_rfc3339();
    let diags: Vec<DiagnosticContext> = (0..3)
        .map(|i| DiagnosticContext {
            error_type: "timeout".to_string(),
            error_message: format!("timeout error {}", i),
            source_file: None,
            tool_name: None,
            chat_id: None,
            model_id: None,
            collected_at: ts.clone(),
            severity: DiagnosticSeverity::High,
            occurrences: None,
        })
        .collect();
    let facts = detect_diagnostic_cluster_facts(&diags, now);
    assert!(
        facts
            .iter()
            .any(|f| f.kind == BuddyFactKind::DiagnosticCluster),
        "cluster fact must be emitted for 3 same-type diagnostics"
    );
}

#[test]
fn frontend_error_burst() {
    use super::observers::diagnostic_cluster::detect_diagnostic_cluster_facts;
    let now = chrono::Utc::now();
    let ts = (now - Duration::minutes(2)).to_rfc3339();
    let diags: Vec<DiagnosticContext> = (0..5)
        .map(|i| DiagnosticContext {
            error_type: "js_error".to_string(),
            error_message: format!("uncaught {}", i),
            source_file: None,
            tool_name: Some("frontend".to_string()),
            chat_id: None,
            model_id: None,
            collected_at: ts.clone(),
            severity: DiagnosticSeverity::Medium,
            occurrences: None,
        })
        .collect();
    let facts = detect_diagnostic_cluster_facts(&diags, now);
    assert!(
        facts
            .iter()
            .any(|f| f.kind == BuddyFactKind::FrontendErrorBurst),
        "frontend burst fact must be emitted for 5 frontend diagnostics"
    );
}

#[test]
fn ephemeral_debug_renders_placeholder() {
    use super::observers::Ephemeral;
    let e = Ephemeral::new("secret".to_string());
    assert_eq!(format!("{:?}", e), "<ephemeral>");
    assert_eq!(e.as_ref(), "secret");
}

#[test]
fn provider_health_default_missing() {
    use super::observers::provider_health::detect_provider_health_facts;
    use crate::caps::DefaultModels;
    let now = chrono::Utc::now();
    let defaults = DefaultModels {
        completion_default_model: String::new(),
        chat_model_2: String::new(),
        task_planner_agent_model: String::new(),
        chat_default_model: "openai/gpt-4o".to_string(),
        chat_thinking_model: String::new(),
        chat_light_model: String::new(),
        chat_buddy_model: String::new(),
    };
    let facts = detect_provider_health_facts(&defaults, &["openai/gpt-4o".to_string()], &[], now);
    assert!(
        facts
            .iter()
            .any(|f| f.kind == BuddyFactKind::DefaultModelMissing),
        "must emit DefaultModelMissing when chat_buddy_model is empty"
    );
}

#[test]
fn provider_health_broken_ref() {
    use super::observers::provider_health::detect_provider_health_facts;
    use crate::caps::DefaultModels;
    let now = chrono::Utc::now();
    let defaults = DefaultModels {
        completion_default_model: String::new(),
        chat_model_2: String::new(),
        task_planner_agent_model: String::new(),
        chat_default_model: "openai/gpt-4o".to_string(),
        chat_thinking_model: String::new(),
        chat_light_model: String::new(),
        chat_buddy_model: String::new(),
    };
    let facts = detect_provider_health_facts(&defaults, &[], &[], now);
    assert!(
        facts
            .iter()
            .any(|f| f.kind == BuddyFactKind::BrokenModelReference),
        "must emit BrokenModelReference when default model is not in available list"
    );
}

#[test]
fn provider_health_surfaces_same_missing_model_per_field() {
    use super::facts::FactStore;
    use super::observers::provider_health::detect_provider_health_facts;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    use crate::caps::DefaultModels;
    let now = chrono::Utc::now();
    let defaults = DefaultModels {
        completion_default_model: String::new(),
        chat_model_2: String::new(),
        task_planner_agent_model: String::new(),
        chat_default_model: "openai/gpt-4o".to_string(),
        chat_thinking_model: String::new(),
        chat_light_model: "refact/gpt-4.1-nano".to_string(),
        chat_buddy_model: "refact/gpt-4.1-nano".to_string(),
    };
    let facts = detect_provider_health_facts(&defaults, &["openai/gpt-4o".to_string()], &[], now);
    let broken: Vec<_> = facts
        .iter()
        .filter(|f| f.kind == BuddyFactKind::BrokenModelReference)
        .collect();
    assert_eq!(broken.len(), 2);
    assert!(broken
        .iter()
        .any(|f| f.key == "provider:broken_ref:chat_light_model:refact/gpt-4.1-nano"));
    assert!(broken
        .iter()
        .any(|f| f.key == "provider:broken_ref:chat_buddy_model:refact/gpt-4.1-nano"));

    let mut store = FactStore::new();
    store.ingest_many(facts);
    let opps =
        OpportunityDetector::new().detect(&store, &BuddyPulse::default(), &OpportunityQueue::new());
    let unavailable_opps: Vec<_> = opps
        .iter()
        .filter(|(opp, _)| {
            opp.kind == BuddyOpportunityKind::ProviderTuning
                && opp.summary.contains("not available")
        })
        .collect();
    assert_eq!(unavailable_opps.len(), 2);
    assert!(unavailable_opps.iter().any(|(opp, _)| {
        opp.cooldown_key == "provider:broken_ref:chat_light_model:refact/gpt-4.1-nano"
    }));
    assert!(unavailable_opps.iter().any(|(opp, _)| {
        opp.cooldown_key == "provider:broken_ref:chat_buddy_model:refact/gpt-4.1-nano"
    }));
}

#[test]
fn provider_health_no_emit_when_ok() {
    use super::observers::provider_health::detect_provider_health_facts;
    use crate::caps::DefaultModels;
    let now = chrono::Utc::now();
    let defaults = DefaultModels {
        completion_default_model: "starcoder".to_string(),
        chat_model_2: "openai/gpt-4o-mini".to_string(),
        task_planner_agent_model: "openai/gpt-4o-mini".to_string(),
        chat_default_model: "openai/gpt-4o".to_string(),
        chat_thinking_model: "openai/o1".to_string(),
        chat_light_model: "openai/gpt-4o-mini".to_string(),
        chat_buddy_model: "openai/gpt-4o-mini".to_string(),
    };
    let chat_models = vec![
        "openai/gpt-4o".to_string(),
        "openai/o1".to_string(),
        "openai/gpt-4o-mini".to_string(),
    ];
    let completion_models = vec!["starcoder".to_string()];
    let facts = detect_provider_health_facts(&defaults, &chat_models, &completion_models, now);
    let interesting: Vec<_> = facts
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                BuddyFactKind::DefaultModelMissing | BuddyFactKind::BrokenModelReference
            )
        })
        .collect();
    assert!(
        interesting.is_empty(),
        "must emit no model facts when all defaults are set and present in available list"
    );
}

#[test]
fn provider_health_checks_completion_default() {
    use super::observers::provider_health::detect_provider_health_facts;
    use crate::caps::DefaultModels;
    let now = chrono::Utc::now();
    let defaults = DefaultModels {
        completion_default_model: "missing-completion".to_string(),
        chat_model_2: String::new(),
        task_planner_agent_model: String::new(),
        chat_default_model: "openai/gpt-4o".to_string(),
        chat_thinking_model: "openai/o1".to_string(),
        chat_light_model: "openai/gpt-4o-mini".to_string(),
        chat_buddy_model: "openai/gpt-4o-mini".to_string(),
    };
    let chat_models = vec![
        "openai/gpt-4o".to_string(),
        "openai/o1".to_string(),
        "openai/gpt-4o-mini".to_string(),
    ];
    let facts = detect_provider_health_facts(&defaults, &chat_models, &[], now);
    assert!(facts.iter().any(|f| {
        f.kind == BuddyFactKind::BrokenModelReference
            && f.payload.get("field").and_then(|v| v.as_str()) == Some("completion_model")
            && f.payload.get("model_id").and_then(|v| v.as_str()) == Some("missing-completion")
    }));
}

#[test]
fn provider_health_chat_default_uses_chat_namespace() {
    use super::observers::provider_health::detect_provider_health_facts;
    use crate::caps::DefaultModels;
    let now = chrono::Utc::now();
    let defaults = DefaultModels {
        completion_default_model: "starcoder".to_string(),
        chat_model_2: String::new(),
        task_planner_agent_model: String::new(),
        chat_default_model: "shared/model".to_string(),
        chat_thinking_model: String::new(),
        chat_light_model: String::new(),
        chat_buddy_model: String::new(),
    };
    let completion_models = vec!["starcoder".to_string(), "shared/model".to_string()];
    let facts = detect_provider_health_facts(&defaults, &[], &completion_models, now);
    assert!(facts.iter().any(|f| {
        f.kind == BuddyFactKind::BrokenModelReference
            && f.payload.get("field").and_then(|v| v.as_str()) == Some("chat_model")
            && f.payload.get("model_id").and_then(|v| v.as_str()) == Some("shared/model")
    }));
}

#[test]
fn mcp_auth_authenticated_token_12h_away_is_not_expired() {
    use super::observers::mcp_auth::{detect_mcp_auth_facts, McpSessionSnapshot};
    use crate::integrations::mcp::session_mcp::MCPAuthStatus;
    let now = chrono::Utc::now();
    let expires_12h = now.timestamp_millis() + 12 * 3600 * 1000;
    let snaps = vec![McpSessionSnapshot {
        id: "github-mcp".to_string(),
        auth_status: MCPAuthStatus::Authenticated,
        failed_calls: 0,
        expires_at_ms: Some(expires_12h),
        has_refresh_token: true,
    }];
    let facts = detect_mcp_auth_facts(&snaps, now);
    assert!(!facts
        .iter()
        .any(|f| f.kind == BuddyFactKind::McpAuthExpired));
}

#[test]
fn mcp_auth_authenticated_token_exactly_five_minutes_away_is_not_expired() {
    use super::observers::mcp_auth::{detect_mcp_auth_facts, McpSessionSnapshot};
    use crate::integrations::mcp::session_mcp::MCPAuthStatus;
    let now = chrono::Utc::now();
    let snaps = vec![McpSessionSnapshot {
        id: "github-mcp".to_string(),
        auth_status: MCPAuthStatus::Authenticated,
        failed_calls: 0,
        expires_at_ms: Some(now.timestamp_millis() + 5 * 60 * 1000),
        has_refresh_token: false,
    }];
    let facts = detect_mcp_auth_facts(&snaps, now);
    assert!(!facts
        .iter()
        .any(|f| f.kind == BuddyFactKind::McpAuthExpired));
}

#[test]
fn mcp_auth_authenticated_token_within_four_minutes_is_expired() {
    use super::observers::mcp_auth::{detect_mcp_auth_facts, McpSessionSnapshot};
    use crate::integrations::mcp::session_mcp::MCPAuthStatus;
    let now = chrono::Utc::now();
    let snaps = vec![McpSessionSnapshot {
        id: "github-mcp".to_string(),
        auth_status: MCPAuthStatus::Authenticated,
        failed_calls: 0,
        expires_at_ms: Some(now.timestamp_millis() + 4 * 60 * 1000),
        has_refresh_token: false,
    }];
    let facts = detect_mcp_auth_facts(&snaps, now);
    assert_eq!(
        facts
            .iter()
            .filter(|f| f.kind == BuddyFactKind::McpAuthExpired)
            .count(),
        1
    );
}

#[test]
fn mcp_auth_authenticated_expired_token_is_expired() {
    use super::observers::mcp_auth::{detect_mcp_auth_facts, McpSessionSnapshot};
    use crate::integrations::mcp::session_mcp::MCPAuthStatus;
    let now = chrono::Utc::now();
    let snaps = vec![McpSessionSnapshot {
        id: "github-mcp".to_string(),
        auth_status: MCPAuthStatus::Authenticated,
        failed_calls: 0,
        expires_at_ms: Some(now.timestamp_millis() - 1000),
        has_refresh_token: false,
    }];
    let facts = detect_mcp_auth_facts(&snaps, now);
    assert_eq!(
        facts
            .iter()
            .filter(|f| f.kind == BuddyFactKind::McpAuthExpired)
            .count(),
        1
    );
}

#[test]
fn mcp_auth_authenticated_expired_token_with_refresh_token_is_not_expired() {
    use super::observers::mcp_auth::{detect_mcp_auth_facts, McpSessionSnapshot};
    use crate::integrations::mcp::session_mcp::MCPAuthStatus;
    let now = chrono::Utc::now();
    let snaps = vec![McpSessionSnapshot {
        id: "github-mcp".to_string(),
        auth_status: MCPAuthStatus::Authenticated,
        failed_calls: 0,
        expires_at_ms: Some(now.timestamp_millis() - 1000),
        has_refresh_token: true,
    }];
    let facts = detect_mcp_auth_facts(&snaps, now);
    assert!(!facts
        .iter()
        .any(|f| f.kind == BuddyFactKind::McpAuthExpired));
}

#[test]
fn mcp_auth_failure_statuses_are_expired_without_expiry() {
    use super::observers::mcp_auth::{detect_mcp_auth_facts, McpSessionSnapshot};
    use crate::integrations::mcp::session_mcp::MCPAuthStatus;
    let now = chrono::Utc::now();
    let snaps = vec![
        McpSessionSnapshot {
            id: "login-mcp".to_string(),
            auth_status: MCPAuthStatus::NeedsLogin,
            failed_calls: 0,
            expires_at_ms: None,
            has_refresh_token: false,
        },
        McpSessionSnapshot {
            id: "reauth-mcp".to_string(),
            auth_status: MCPAuthStatus::NeedsReauth,
            failed_calls: 0,
            expires_at_ms: None,
            has_refresh_token: true,
        },
        McpSessionSnapshot {
            id: "error-mcp".to_string(),
            auth_status: MCPAuthStatus::Error("failed".to_string()),
            failed_calls: 0,
            expires_at_ms: None,
            has_refresh_token: true,
        },
    ];
    let facts = detect_mcp_auth_facts(&snaps, now);
    assert_eq!(
        facts
            .iter()
            .filter(|f| f.kind == BuddyFactKind::McpAuthExpired)
            .count(),
        3
    );
}

#[test]
fn mcp_auth_failure_count() {
    use super::observers::mcp_auth::{detect_mcp_auth_facts, McpSessionSnapshot};
    use crate::integrations::mcp::session_mcp::MCPAuthStatus;
    let now = chrono::Utc::now();
    let snaps = vec![McpSessionSnapshot {
        id: "github-mcp".to_string(),
        auth_status: MCPAuthStatus::NotApplicable,
        failed_calls: 3,
        expires_at_ms: None,
        has_refresh_token: false,
    }];
    let facts = detect_mcp_auth_facts(&snaps, now);
    assert!(
        facts
            .iter()
            .any(|f| f.kind == BuddyFactKind::IntegrationFailing),
        "must emit IntegrationFailing when failure_count >= 3"
    );
}

async fn make_tool_ccx(
    gcx: AppState,
) -> Arc<tokio::sync::Mutex<crate::at_commands::at_commands::AtCommandsContext>> {
    Arc::new(tokio::sync::Mutex::new(
        crate::at_commands::at_commands::AtCommandsContext::new_from_app(
            gcx,
            4000,
            20,
            false,
            vec![],
            String::new(),
            None,
            String::new(),
            None,
            None,
        )
        .await,
    ))
}

#[tokio::test]
async fn buddy_open_view_errors_without_service() {
    use crate::tools::tool_buddy_open_view::ToolBuddyOpenView;
    use crate::tools::tools_description::Tool;
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let ccx = make_tool_ccx(app.clone()).await;
    let mut tool = ToolBuddyOpenView {
        config_path: String::new(),
    };
    let mut args = std::collections::HashMap::new();
    args.insert("page".to_string(), serde_json::json!({ "type": "buddy" }));
    let err = tool
        .tool_execute(ccx, &"tool-call".to_string(), &args)
        .await
        .unwrap_err();
    assert!(err.contains("buddy service not initialized"));
}

#[tokio::test]
async fn buddy_open_setup_flow_emits_setup_mode() {
    use crate::tools::tool_buddy_open_setup_flow::ToolBuddyOpenSetupFlow;
    use crate::tools::tools_description::Tool;
    let (svc, mut rx) = make_service_with_events();
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    *app.buddy.buddy.lock().await = Some(svc);
    let ccx = make_tool_ccx(app.clone()).await;
    let mut tool = ToolBuddyOpenSetupFlow {
        config_path: String::new(),
    };
    let mut args = std::collections::HashMap::new();
    args.insert("flow".to_string(), serde_json::json!("setup_mcp"));
    tool.tool_execute(ccx, &"tool-call".to_string(), &args)
        .await
        .unwrap();
    match rx.try_recv().unwrap() {
        super::events::BuddyEvent::NavigationRequest {
            page: BuddyPage::SetupMode { mode },
        } => assert_eq!(mode, "setup_mcp"),
        event => panic!("unexpected event: {:?}", event),
    }
}

#[tokio::test]
async fn buddy_setup_speech_tools_error_without_service() {
    use crate::tools::tool_buddy_open_setup_flow::ToolBuddyOpenSetupFlow;
    use crate::tools::tool_buddy_say::{ToolBuddyRenderControls, ToolBuddySay};
    use crate::tools::tools_description::Tool;
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let ccx = make_tool_ccx(app.clone()).await;

    let mut setup = ToolBuddyOpenSetupFlow {
        config_path: String::new(),
    };
    let mut setup_args = std::collections::HashMap::new();
    setup_args.insert("flow".to_string(), serde_json::json!("setup_mcp"));
    let setup_err = setup
        .tool_execute(ccx.clone(), &"setup-call".to_string(), &setup_args)
        .await
        .unwrap_err();
    assert!(setup_err.contains("buddy service not initialized"));

    let mut say = ToolBuddySay {
        config_path: String::new(),
    };
    let mut say_args = std::collections::HashMap::new();
    say_args.insert("text".to_string(), serde_json::json!("hello"));
    let say_err = say
        .tool_execute(ccx.clone(), &"say-call".to_string(), &say_args)
        .await
        .unwrap_err();
    assert!(say_err.contains("buddy service not initialized"));

    let mut controls = ToolBuddyRenderControls {
        config_path: String::new(),
    };
    let mut control_args = std::collections::HashMap::new();
    control_args.insert(
        "controls".to_string(),
        serde_json::json!([
            {"id": "setup", "label": "Setup", "action": "open_setup"}
        ]),
    );
    let controls_err = controls
        .tool_execute(ccx, &"controls-call".to_string(), &control_args)
        .await
        .unwrap_err();
    assert!(controls_err.contains("buddy service not initialized"));
}

#[test]
fn buddy_open_setup_flow_schema_excludes_configurator() {
    use crate::tools::tool_buddy_open_setup_flow::ToolBuddyOpenSetupFlow;
    use crate::tools::tools_description::Tool;
    let tool = ToolBuddyOpenSetupFlow {
        config_path: String::new(),
    };
    let desc = tool.tool_description();
    let flows: Vec<&str> = desc.input_schema["properties"]["flow"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(flows.contains(&"setup"));
    assert!(flows.contains(&"setup_mcp"));
    assert!(flows.contains(&"setup_subagents"));
    assert!(!flows.contains(&"configurator"));
}

#[test]
fn chat_pattern_no_secret_leakage() {
    use super::observers::chat_pattern::run_chat_pattern_observer_sync;
    let messages = vec![
        chat_msg("user", "my key is sk-FAKEKEYABCDEFGHIJKL12345"),
        chat_msg("user", "actually use Bearer abcdef-leak-token instead"),
        chat_msg("user", "wait try again"),
        chat_msg("user", "sorry undo"),
    ];
    let facts = run_chat_pattern_observer_sync(&messages, "chat-1");
    let serialized = serde_json::to_string(&facts).unwrap();
    assert!(!serialized.contains("sk-FAKEKEYABCDEFGHIJKL12345"));
    assert!(!serialized.contains("Bearer abcdef-leak-token"));
    assert!(!serialized.contains("actually"));
    assert!(!serialized.contains("undo"));
    assert!(facts
        .iter()
        .any(|f| matches!(f.kind, BuddyFactKind::ChatRetryStreak)));
}

#[test]
fn chat_pattern_retry_streak_count() {
    use super::observers::chat_pattern::count_retry_streak;
    let messages = vec![
        chat_msg("user", "please do X"),
        chat_msg("assistant", "done"),
        chat_msg("user", "actually no"),
        chat_msg("user", "wait try again"),
        chat_msg("user", "sorry that was wrong"),
    ];
    let count = count_retry_streak(&messages);
    assert!(count >= 3, "expected >= 3 retry streak, got {}", count);
}

#[test]
fn chat_pattern_no_streak_for_normal_conversation() {
    use super::observers::chat_pattern::count_retry_streak;
    let messages = vec![
        chat_msg("user", "please implement feature X"),
        chat_msg("assistant", "done"),
        chat_msg("user", "great, now do Y"),
        chat_msg("user", "and also Z"),
    ];
    let count = count_retry_streak(&messages);
    assert_eq!(count, 0, "expected 0 retry streak for normal chat");
}

#[test]
fn observer_registry_has_10_entries() {
    use super::observers::build_observer_registry;
    let registry = build_observer_registry();
    assert_eq!(
        registry.len(),
        10,
        "build_observer_registry must return 10 observers"
    );
    let ids: Vec<&str> = registry.iter().map(|o| o.id()).collect();
    assert!(ids.contains(&"task_health"));
    assert!(ids.contains(&"trajectory_clutter"));
    assert!(ids.contains(&"git_pressure"));
    assert!(ids.contains(&"worktree_hygiene"));
    assert!(ids.contains(&"diagnostic_cluster"));
    assert!(ids.contains(&"chat_pattern"));
    assert!(ids.contains(&"customization_drift"));
    assert!(ids.contains(&"memory_garden"));
    assert!(ids.contains(&"mcp_auth"));
    assert!(ids.contains(&"provider_health"));
}

fn buddy_worktree_inventory(
    total: usize,
    abandoned_clean: usize,
    dirty: usize,
    stale: usize,
) -> crate::worktrees::types::WorktreeInventory {
    use crate::worktrees::types::{WorktreeInventory, WorktreeInventorySummary};
    WorktreeInventory {
        project_hash: "project".to_string(),
        source_workspace_root: std::path::PathBuf::from("/repo"),
        generated_at: chrono::Utc::now().to_rfc3339(),
        summary: WorktreeInventorySummary {
            total,
            total_registered: total,
            abandoned_clean,
            dirty,
            stale,
            clean: total.saturating_sub(dirty + stale),
            ..WorktreeInventorySummary::default()
        },
        worktrees: Vec::new(),
        cleanup_candidates: (0..abandoned_clean)
            .map(|idx| format!("wt-{idx}"))
            .collect(),
    }
}

#[test]
fn buddy_worktree_observer_emits_worktree_hygiene_fact() {
    let facts = super::observers::worktree_hygiene::detect_worktree_hygiene_facts(
        buddy_worktree_inventory(5, 2, 2, 1),
        chrono::Utc::now(),
    );
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, BuddyFactKind::WorktreeHygiene);
    assert_eq!(
        facts[0].payload["summary"]["abandoned_clean"].as_u64(),
        Some(2)
    );
}

#[test]
fn buddy_worktree_opportunity_generated_when_abandoned_clean_exists() {
    use super::facts::FactStore;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    let mut store = FactStore::new();
    let fact = super::observers::worktree_hygiene::detect_worktree_hygiene_facts(
        buddy_worktree_inventory(5, 2, 2, 1),
        chrono::Utc::now(),
    )
    .remove(0);
    store.ingest(fact);
    let opps =
        OpportunityDetector::new().detect(&store, &BuddyPulse::default(), &OpportunityQueue::new());
    let opp = opps
        .iter()
        .find(|(opp, _)| opp.kind == BuddyOpportunityKind::WorktreeCleanup)
        .expect("worktree cleanup opportunity");
    assert!(opp.0.summary.contains("2 clean abandoned"));
}

#[test]
fn buddy_worktree_no_opportunity_when_no_candidates() {
    use super::facts::FactStore;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    let mut store = FactStore::new();
    let fact = super::observers::worktree_hygiene::detect_worktree_hygiene_facts(
        buddy_worktree_inventory(3, 0, 2, 1),
        chrono::Utc::now(),
    )
    .remove(0);
    store.ingest(fact);
    let opps =
        OpportunityDetector::new().detect(&store, &BuddyPulse::default(), &OpportunityQueue::new());
    assert!(!opps
        .iter()
        .any(|(opp, _)| opp.kind == BuddyOpportunityKind::WorktreeCleanup));
}

#[test]
fn buddy_worktree_suggestion_text_includes_counts() {
    let inventory = buddy_worktree_inventory(5, 2, 2, 1);
    let suggestion =
        super::jobs::proactive_suggestions::worktree_hygiene_suggestion_from_inventory(&inventory)
            .expect("suggestion");
    assert!(suggestion.description.contains("5 worktrees"));
    assert!(suggestion.description.contains("2 clean abandoned"));
    let actions = suggestion
        .controls
        .iter()
        .map(|control| control.action.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        vec![
            "review_worktree_cleanup",
            "create_worktrees_pulse",
            "dismiss_suggestion"
        ]
    );
    assert!(
        !suggestion
            .controls
            .iter()
            .any(|control| control.label.starts_with("Clean ")),
        "no control may promise cleanup it does not perform"
    );
}

#[tokio::test]
async fn buddy_worktree_pulse_includes_worktree_stats() {
    use crate::worktrees::types::CreateWorktreeRequest;
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("repo");
    std::fs::create_dir_all(&source).unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&source)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["checkout", "-b", "main"])
        .current_dir(&source)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&source)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(&source)
        .output()
        .unwrap();
    std::fs::write(source.join("file.txt"), "hello\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(&source)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(&source)
        .output()
        .unwrap();
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let cache_dir = app.paths.cache_dir.clone();
    let service =
        crate::worktrees::service::WorktreeService::new(cache_dir, source.clone()).unwrap();
    let created = service
        .create_worktree(CreateWorktreeRequest {
            branch: Some("refact/chat/buddy-worktree-pulse".to_string()),
            kind: Some("chat".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut registry = service.load_registry().await.unwrap();
    let record = registry
        .records
        .iter_mut()
        .find(|record| record.meta.id == created.worktree.meta.id)
        .unwrap();
    let ts = (chrono::Utc::now() - Duration::hours(48)).to_rfc3339();
    record.updated_at = ts.clone();
    record.last_seen_at = Some(ts);
    service.save_registry(&registry).await.unwrap();
    let store = super::facts::FactStore::new();
    let pulse = super::pulse::build_pulse(app, &source, &store).await;
    assert_eq!(pulse.worktrees.total_registered, 1);
    assert_eq!(pulse.worktrees.abandoned_clean, 1);
}

// =============================================================================
// T-7: Detector rules + Actor wiring + Pulse aggregation
// =============================================================================
#[test]
fn detector_emits_task_health_for_stuck() {
    use super::facts::FactStore;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    let now = chrono::Utc::now();
    let mut store = FactStore::new();
    store.ingest(BuddyFact {
        kind: BuddyFactKind::TaskStuck,
        key: "task:stuck:t1".to_string(),
        source: "test",
        payload: serde_json::json!({"task_id": "t1"}),
        seen_at: now,
        confidence: 0.9,
    });
    let pulse = BuddyPulse::default();
    let queue = OpportunityQueue::new();
    let opps = OpportunityDetector::new().detect(&store, &pulse, &queue);
    assert_eq!(opps.len(), 1, "must emit 1 TaskHealth opportunity");
    assert_eq!(opps[0].0.kind, BuddyOpportunityKind::TaskHealth);
}

#[test]
fn detector_dedupes_via_cooldown_key() {
    use super::facts::FactStore;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    let now = chrono::Utc::now();
    let mut store = FactStore::new();
    // Two facts with different keys but same task_id → same cooldown_key → 1 opportunity
    store.ingest(BuddyFact {
        kind: BuddyFactKind::TaskStuck,
        key: "task:stuck:dup-a".to_string(),
        source: "test",
        payload: serde_json::json!({"task_id": "dup-task"}),
        seen_at: now,
        confidence: 1.0,
    });
    store.ingest(BuddyFact {
        kind: BuddyFactKind::TaskStuck,
        key: "task:stuck:dup-b".to_string(),
        source: "test",
        payload: serde_json::json!({"task_id": "dup-task"}),
        seen_at: now,
        confidence: 1.0,
    });
    let pulse = BuddyPulse::default();
    let queue = OpportunityQueue::new();
    let opps = OpportunityDetector::new().detect(&store, &pulse, &queue);
    assert_eq!(
        opps.len(),
        1,
        "same cooldown_key must dedupe to 1 opportunity"
    );
}

#[test]
fn detector_skips_when_queue_cooldown_active() {
    use super::facts::FactStore;
    use super::opportunities::{OpportunityDetector, OpportunityQueue};
    let now = chrono::Utc::now();
    let mut store = FactStore::new();
    store.ingest(BuddyFact {
        kind: BuddyFactKind::TaskStuck,
        key: "task:stuck:cd-task".to_string(),
        source: "test",
        payload: serde_json::json!({"task_id": "cd-task"}),
        seen_at: now,
        confidence: 1.0,
    });
    let pulse = BuddyPulse::default();
    let mut queue = OpportunityQueue::new();
    // Push an opp with the same cooldown_key to activate cooldown
    push_opportunity(
        &mut queue,
        make_opportunity("existing-opp", "task_health:stuck:cd-task"),
    );
    assert!(queue.cooldown_active("task_health:stuck:cd-task"));
    let opps = OpportunityDetector::new().detect(&store, &pulse, &queue);
    assert!(opps.is_empty(), "must skip when queue cooldown is active");
}

#[tokio::test]
async fn pulse_builds_all_subpulses() {
    use super::facts::FactStore;
    use super::pulse::build_pulse;
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let store = FactStore::new();
    let pulse = build_pulse(app, std::path::Path::new("/tmp"), &store).await;
    assert!(pulse.generated_at.is_some(), "generated_at must be set");
    let _ = pulse.tasks.total;
    let _ = pulse.trajectories.total;
    let _ = pulse.providers.defaults_ok;
    let _ = pulse.customization.modes;
    let _ = pulse.diagnostics.last_hour;
}

#[tokio::test]
async fn actor_observer_tick_pipeline() {
    let (tx, mut rx) = broadcast::channel(32);
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    let now = chrono::Utc::now();
    svc.fact_store.ingest(BuddyFact {
        kind: BuddyFactKind::TaskStuck,
        key: "task:stuck:test-task".to_string(),
        source: "test",
        payload: serde_json::json!({"task_id": "test-task"}),
        seen_at: now,
        confidence: 0.9,
    });
    svc.detect_and_surface();
    assert_eq!(
        svc.opportunity_queue.iter().count(),
        1,
        "must have 1 opportunity"
    );
    let event = rx
        .try_recv()
        .expect("must receive OpportunityProduced event");
    assert!(
        matches!(event, super::events::BuddyEvent::OpportunityProduced { .. }),
        "event must be OpportunityProduced"
    );
}

#[tokio::test]
async fn actor_pulse_broadcast_60s() {
    let (tx, mut rx) = broadcast::channel(32);
    let mut svc = BuddyService::new(
        std::env::temp_dir().join(format!("buddy-test-{}", uuid::Uuid::new_v4())),
        default_buddy_state(),
        BuddySettings::default(),
        Vec::new(),
        super::runtime_queue::RuntimeQueue::new(),
        tx,
        None,
    );
    let new_pulse = BuddyPulse {
        generated_at: Some(chrono::Utc::now()),
        ..BuddyPulse::default()
    };
    svc.set_pulse(new_pulse);
    let event = rx.try_recv().expect("must receive PulseUpdated event");
    assert!(
        matches!(event, super::events::BuddyEvent::PulseUpdated { .. }),
        "event must be PulseUpdated"
    );
}

#[tokio::test]
async fn actor_humor_attached_when_allowed() {
    use super::humor::{HumorGenerator, HumorService};
    struct MockGen;
    #[async_trait::async_trait]
    impl HumorGenerator for MockGen {
        async fn generate(
            &self,
            _kind: BuddyFactKind,
            _summary: String,
            _gcx: AppState,
        ) -> Vec<String> {
            vec!["Test joke".to_string()]
        }
    }
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = crate::app_state::AppState::from_gcx(gcx.clone()).await;
    let mut svc = make_service();
    svc.humor_service = Arc::new(tokio::sync::Mutex::new(HumorService::new_with_generator(
        std::sync::Arc::new(MockGen),
    )));
    let mut opp = make_opportunity("humor-opp", "humor-opp-key");
    opp.humor_allowed = true;
    opp.priority = BuddyPriority::Normal;
    opp.summary = "test summary".to_string();
    let pulse = BuddyPulse::default();
    let humor_service = svc.humor_service.clone();
    let mut humor = humor_service.lock().await;
    apply_humor_plan(&mut humor, &mut opp, BuddyFactKind::TaskStuck, &pulse, app).await;
    assert!(opp.humor.is_some(), "humor must be attached when allowed");
    assert_eq!(opp.humor.as_deref(), Some("Test joke"));
}

#[tokio::test]
async fn actor_persistence_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    super::storage::bootstrap_buddy_storage(root).await.unwrap();
    let mut svc = make_service();
    push_opportunity(
        &mut svc.opportunity_queue,
        make_opportunity("opp-persist-1", "ck-persist-1"),
    );
    push_opportunity(
        &mut svc.opportunity_queue,
        make_opportunity("opp-persist-2", "ck-persist-2"),
    );
    let mut state = svc.state.clone();
    state.opportunities = svc.opportunity_queue.snapshot();
    super::state::save_state(root, &state).await.unwrap();
    let loaded = super::state::load_state(root).await;
    assert_eq!(loaded.opportunities.len(), 2, "opportunities must persist");
    let queue = super::opportunities::OpportunityQueue::from_state(
        loaded.opportunities,
        loaded.dismissed_history,
    );
    assert_eq!(
        queue.iter().count(),
        2,
        "queue must be reconstructed from state"
    );
}
