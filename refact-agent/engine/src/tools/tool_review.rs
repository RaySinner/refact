pub use refact_tools::tools::tool_review::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::time::Duration;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::mpsc;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use crate::agents::tool_agent::ToolAgentHandle;
#[cfg(test)]
#[allow(unused_imports)]
use refact_agents_core::tool_agent::ToolAgentSpec;
#[cfg(test)]
#[allow(unused_imports)]
use crate::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::trajectory_runtime::save_trajectory_as_with_intent_checked;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::types::{ChatSession, PushMode, SessionState, ThreadParams, TrajectoryCommitIntent};
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::ctx::ToolCtx;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::config::{
    load_review_config, slot_model_id, stage_subchat_spec, ReviewConfig,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::prompts::{build_adversarial_prompt, build_stage_prompt};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::runner::{
    merge_metering, now_ms, stage_trace_chat_id, ProgressSink, StageCtx, StageJob, StageProgress,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::stages::{load_stage_catalog, select_stages, StagePhase, StageSpec};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::{
    run_stage_jobs, ScheduleParams, StageExecutor, SubchatExecutor, HARVEST_GRACE,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_evidence::verify_evidence;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_merge::{merge_findings, rank_findings};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_scope::{build_review_scope, DiffAttribution, ReviewScope, ScopeRequest};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_types::{
    Dispute, ReviewDepth, ReviewFinding, ReviewOutcome, ReviewReport, ReviewSeverity, ScopeMode,
    StageRun, StageStatusKind,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::subagent_phases::resolve_gathered_file_path;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::scope::ExecutionScope;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    use crate::agents::registry::InboxMessage;
    use crate::agents::types::{BgAgentKind, BgAgentStatus};
    use crate::caps::caps::{BaseModelRecord, ChatModelRecord, CodeAssistantCaps};
    use refact_chat_api::types::{PendingDelivery, PushMode};
    use crate::tools::review_agents::contract::Verdict;
    use crate::tools::review_agents::runner::{stage_subchat_config, StageProduct};
    use crate::tools::review_types::{
        CommandRun, FindingLocation, ReviewDiffSummary, ReviewScopeSummary, StageCoverage,
    };

    const COMPLETION_PUSH_TEXT: &str = "[background subagent finished]";

    fn args_map(pairs: Vec<(&str, Value)>) -> HashMap<String, Value> {
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    fn finding(id: &str, severity: ReviewSeverity) -> ReviewFinding {
        ReviewFinding {
            id: id.to_string(),
            stage: "diff".to_string(),
            model: Some("model-a".to_string()),
            title: "Error dropped".to_string(),
            severity,
            file: "src/lib.rs".to_string(),
            line_start: 10,
            line_end: 14,
            claim: "The error arm returns success.".to_string(),
            evidence: "return Ok(());".to_string(),
            evidence_present: true,
            reproduction: Some("cargo test -p thing".to_string()),
            fix: Some("Propagate the error.".to_string()),
            introduced_by_diff: true,
            out_of_scope: false,
            reported_by: vec!["diff".to_string()],
            locations: vec![],
            disputed: None,
        }
    }

    fn report(findings: Vec<ReviewFinding>, stages: Vec<StageRun>) -> ReviewReport {
        let mut report = ReviewReport {
            depth: "normal".to_string(),
            outcome: ReviewOutcome::Inconclusive,
            scope: ReviewScopeSummary {
                mode: "strict".to_string(),
                requested_files: 12,
                reviewed_files: 14,
                files: vec!["src/lib.rs".to_string()],
                focus: None,
                expansion: Some("+2 dependency edges".to_string()),
                out_of_scope_findings: 1,
                dropped_files: vec![],
            },
            diff: ReviewDiffSummary {
                base: Some("1a2b3c".to_string()),
                head: Some("HEAD".to_string()),
                changed_files: 7,
                hunks: 31,
            },
            stages,
            findings,
            duration_ms: 252_000,
            duplicates_merged: 3,
            scratch_dir: Some(".refact/review_scratch/rv-1".to_string()),
        };
        report.outcome = report.derive_outcome();
        report
    }

    #[test]
    fn tool_review_args_default_scope_mode_to_strict_when_files_are_given() {
        let with_files = parse_args(&args_map(vec![("files", json!(["src/lib.rs"]))])).unwrap();
        let with_base = parse_args(&args_map(vec![("base", json!("main"))])).unwrap();
        let bare = parse_args(&args_map(vec![])).unwrap();

        assert_eq!(with_files.scope_mode, ScopeMode::Strict);
        assert_eq!(with_files.files, ["src/lib.rs"]);
        assert_eq!(with_base.scope_mode, ScopeMode::Strict);
        assert_eq!(bare.scope_mode, ScopeMode::Broad);
        assert_eq!(bare.stages, None);
        assert_eq!(bare.variants, None);
    }

    #[test]
    fn tool_review_args_reject_out_of_range_values() {
        assert!(parse_args(&args_map(vec![("depth", json!("max"))]))
            .unwrap_err()
            .contains("invalid depth"));
        assert!(
            parse_args(&args_map(vec![("scope_mode", json!("everything"))]))
                .unwrap_err()
                .contains("invalid scope_mode")
        );
        assert!(parse_args(&args_map(vec![("variants", json!(4))]))
            .unwrap_err()
            .contains("between 1 and 3"));
        assert!(parse_args(&args_map(vec![("parallel_depth", json!(0))]))
            .unwrap_err()
            .contains("at least 1"));
        assert_eq!(
            parse_args(&args_map(vec![
                ("stages", json!(["diff", "security"])),
                ("parallel_depth", json!(2))
            ]))
            .unwrap()
            .stages,
            Some(vec!["diff".to_string(), "security".to_string()])
        );
    }

    #[test]
    fn tool_review_browser_flag_requires_a_scenario() {
        let error = parse_args(&args_map(vec![("browser", json!(true))])).unwrap_err();
        assert!(error.contains("`browser_scenario` is required"));

        let with_scenario = parse_args(&args_map(vec![
            ("browser", json!(true)),
            (
                "browser_scenario",
                json!("open /settings, expect no console errors"),
            ),
        ]))
        .unwrap();
        assert_eq!(
            with_scenario.browser_scenario.as_deref(),
            Some("open /settings, expect no console errors")
        );

        let scenario_only =
            parse_args(&args_map(vec![("browser_scenario", json!("open /"))])).unwrap();
        assert_eq!(scenario_only.browser_scenario.as_deref(), Some("open /"));
        assert!(parse_args(&args_map(vec![]))
            .unwrap()
            .browser_scenario
            .is_none());
        assert!(parse_args(&args_map(vec![("browser", json!("yes"))]))
            .unwrap_err()
            .contains("must be a boolean"));
    }

    #[test]
    fn tool_review_browser_stage_runs_only_with_a_scenario() {
        let catalog = crate::tools::review_agents::stages::embedded_catalog();
        let browser = catalog
            .iter()
            .find(|spec| spec.id == "browser")
            .unwrap()
            .clone();

        let mut scheduled = vec![browser.clone()];
        let mut skipped = Vec::new();
        apply_browser_scenario(&catalog, &mut scheduled, &mut skipped, None);
        assert!(scheduled.is_empty());
        assert_eq!(
            skipped,
            vec![(
                "browser".to_string(),
                "no browser_scenario given".to_string()
            )]
        );

        let mut scheduled = Vec::new();
        let mut skipped = vec![(
            "browser".to_string(),
            "no matching files in scope".to_string(),
        )];
        apply_browser_scenario(&catalog, &mut scheduled, &mut skipped, Some("open /"));
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].id, "browser");
        assert!(skipped.is_empty());

        let mut scheduled = vec![browser];
        let mut skipped = Vec::new();
        apply_browser_scenario(&catalog, &mut scheduled, &mut skipped, Some("open /"));
        assert_eq!(scheduled.len(), 1);
    }

    #[test]
    fn tool_review_render_reports_scope_stages_and_fact_vs_hypothesis() {
        let mut hypothesis = finding("rf-2", ReviewSeverity::Medium);
        hypothesis.reproduction = None;
        hypothesis.evidence_present = false;
        hypothesis.claim = "Maybe the cache is stale.".to_string();
        let mut merged = finding("rf-3", ReviewSeverity::High);
        merged.reported_by = vec!["diff".to_string(), "impact".to_string()];
        merged.locations = vec![FindingLocation {
            file: "src/other.rs".to_string(),
            line_start: 4,
            line_end: 6,
        }];
        let mut timed_out =
            StageRun::timed_out("dependencies", None, 360_000, "no activity for 360s");
        timed_out.coverage = StageCoverage {
            files_read: vec!["a.rs".to_string(), "b.rs".to_string()],
            commands_run: vec![CommandRun {
                cmd: "cargo check".to_string(),
                exit: 101,
            }],
            tools_unavailable: vec![],
            stopped_early: Some("idle".to_string()),
        };

        let markdown = render_review_markdown(&report(
            vec![finding("rf-1", ReviewSeverity::Blocker), merged, hypothesis],
            vec![
                StageRun::ok("mechanical", Some("m".to_string()), 192_000),
                timed_out,
                StageRun::not_run("execution", "depth normal"),
            ],
        ));

        assert!(markdown.starts_with(
            "## Review ⚠️ PARTIAL · 12 file(s) requested → 14 reviewed (strict, +2 dependency edges) · base 1a2b3c..HEAD (7 changed file(s), 31 hunk(s)) · depth normal · 4m12s"
        ));
        assert!(markdown.contains("Stages: mechanical ok 3m12s · dependencies timed out 6m00s (no activity for 360s) · execution not run (depth normal)"));
        assert!(markdown.contains("Findings: 2 supported (2 with a reproduction) · 1 hypotheses · 3 duplicate(s) merged · 0 pre-existing · 1 out of scope"));
        assert!(markdown.contains("### blocker (1)"));
        assert!(markdown.contains("### high (1)"));
        assert!(markdown.contains("repro: cargo test -p thing"));
        assert!(markdown.contains("stage: diff+impact"));
        assert!(markdown.contains("also at: src/other.rs:4-6"));
        assert!(markdown.contains("### Hypotheses (1)"));
        assert!(markdown.contains("evidence not found in file"));
        assert!(markdown.contains("dependencies: 2 file(s) read; 1 command(s) run, 1 non-zero"));
        assert!(markdown.contains("Raw per-stage output: .refact/review_scratch/rv-1"));
        assert!(!markdown.contains("confidence"));
    }

    #[test]
    fn tool_review_render_states_when_nothing_was_supported() {
        let markdown = render_review_markdown(&report(
            vec![],
            vec![StageRun::ok("diff", Some("m".to_string()), 1000)],
        ));

        assert!(markdown.starts_with("## Review · 12 file(s) requested"));
        assert!(markdown.contains("### No supported findings"));
        assert!(!markdown.contains("### Hypotheses"));
        assert!(!markdown.contains("INCONCLUSIVE"));
    }

    #[test]
    fn tool_review_render_marks_an_all_stages_dead_review_inconclusive_not_clean() {
        let markdown = render_review_markdown(&report(
            vec![],
            vec![
                StageRun::timed_out("diff", None, 360_000, "no activity for 120s")
                    .with_trace_chat_id(Some("subchat-rv-1-diff".to_string())),
                StageRun::timed_out("security", None, 360_000, "no activity for 120s")
                    .with_trace_chat_id(Some("subchat-rv-1-security".to_string())),
                StageRun::failed("spec", None, 12, "output_contract"),
            ],
        ));

        assert!(markdown.starts_with("## Review ⚠️ INCONCLUSIVE · 12 file(s) requested"));
        assert!(
            !markdown.contains("### No supported findings"),
            "an inconclusive review must never render the clean-bill header: {markdown}"
        );
        assert!(markdown.contains("### Nothing was checked — this is NOT a pass"));
        assert!(markdown.contains("Do not read the empty findings list as approval"));
        assert!(markdown.contains("Incomplete stage(s) (3):"));
        assert!(markdown.contains("- diff: timed out (no activity for 120s)"));
        assert!(markdown.contains("- spec: failed (output_contract)"));
        assert!(markdown.contains("[transcript subchat-rv-1-diff]"));
    }

    #[test]
    fn tool_review_render_partial_says_how_many_stages_completed() {
        let markdown = render_review_markdown(&report(
            vec![],
            vec![
                StageRun::ok("diff", Some("m".to_string()), 1000),
                StageRun::timed_out("security", None, 360_000, "no activity for 360s"),
            ],
        ));

        assert!(markdown.starts_with("## Review ⚠️ PARTIAL · 12 file(s) requested"));
        assert!(!markdown.contains("### No supported findings\n"));
        assert!(markdown.contains("### No supported findings from the 1 stage(s) that completed"));
        assert!(markdown.contains("1 stage(s) did not complete"));
        assert!(markdown.contains("- security: timed out (no activity for 360s)"));
    }

    #[test]
    fn tool_review_render_lists_files_that_were_never_reviewed() {
        let mut with_dropped = report(
            vec![],
            vec![StageRun::ok("diff", Some("m".to_string()), 1000)],
        );
        with_dropped.scope.dropped_files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];

        let markdown = render_review_markdown(&with_dropped);

        assert!(markdown.contains("2 file(s) were not reviewed"));
        assert!(!markdown.contains("\n- src/a.rs"));
        assert!(!markdown.contains("\n- src/b.rs"));
    }

    #[test]
    fn tool_review_render_omits_the_scratch_path_when_nothing_was_written() {
        let mut nothing_written = report(
            vec![],
            vec![StageRun::timed_out(
                "diff",
                None,
                1000,
                "no activity for 360s",
            )],
        );
        nothing_written.scratch_dir = None;

        let markdown = render_review_markdown(&nothing_written);

        assert!(!markdown.contains("Raw per-stage output"));
    }

    #[test]
    fn tool_review_adversarial_verdicts_only_dispute_matching_ids() {
        let mut findings = vec![
            finding("rf-1", ReviewSeverity::High),
            finding("rf-2", ReviewSeverity::Low),
        ];
        let verdicts = vec![
            Verdict {
                id: "rf-1".to_string(),
                supported: false,
                reason: "guarded at src/lib.rs:4".to_string(),
            },
            Verdict {
                id: "rf-2".to_string(),
                supported: true,
                reason: String::new(),
            },
            Verdict {
                id: "rf-missing".to_string(),
                supported: false,
                reason: "unknown".to_string(),
            },
        ];

        let disputed = apply_verdicts(&mut findings, "adversarial", &verdicts);

        assert_eq!(disputed, 1);
        assert_eq!(
            findings[0].disputed.as_ref().unwrap().reason,
            "guarded at src/lib.rs:4"
        );
        assert!(findings[0].is_hypothesis());
        assert!(findings[1].disputed.is_none());
        assert!(!findings[1].is_hypothesis());
    }

    #[test]
    fn tool_review_refs_are_relative_and_flag_hypotheses() {
        let mut hypothesis = finding("rf-2", ReviewSeverity::Low);
        hypothesis.reproduction = None;
        hypothesis.evidence_present = false;
        hypothesis.file = crate::test_paths::abs_str("repo/src/other.rs");
        let report = report(
            vec![finding("rf-1", ReviewSeverity::High), hypothesis],
            vec![],
        );

        let (refs, truncated) = review_refs(&report, &[crate::test_paths::abs("repo")]);

        assert!(!truncated);
        let refs = refs.as_array().unwrap();
        assert_eq!(refs[0]["path"], "src/lib.rs");
        assert_eq!(refs[0]["hypothesis"], false);
        assert_eq!(refs[1]["path"], "src/other.rs");
        assert_eq!(refs[1]["hypothesis"], true);
        assert_eq!(refs[1]["severity"], "low");
    }

    #[test]
    fn tool_review_duration_formatting() {
        assert_eq!(format_duration(999), "0s");
        assert_eq!(format_duration(59_000), "59s");
        assert_eq!(format_duration(192_000), "3m12s");
        assert_eq!(format_duration(3_600_000), "60m00s");
    }

    struct FakeStageExecutor {
        release: Option<Arc<AtomicBool>>,
        started: Option<mpsc::UnboundedSender<()>>,
        executed: AtomicUsize,
    }

    #[async_trait]
    impl StageExecutor for FakeStageExecutor {
        async fn execute(&self, job: StageJob, _activity: Arc<AtomicU64>) -> StageProduct {
            self.executed.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = self.started.as_ref() {
                let _ = started.send(());
            }
            if let Some(release) = self.release.as_ref() {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
                while !release.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            let mut metering = serde_json::Map::new();
            metering.insert("tokens_in".to_string(), json!(10));
            metering.insert("tokens_out".to_string(), json!(20));
            StageProduct {
                run: StageRun::ok(&job.label, Some("test-provider/test-model".to_string()), 1),
                findings: vec![],
                verdicts: vec![],
                metering,
                raw: None,
            }
        }
    }

    async fn install_test_caps(gcx: &Arc<GlobalContext>) {
        let model_id = "test-provider/test-model".to_string();
        let mut caps = CodeAssistantCaps::default();
        caps.chat_models.insert(
            model_id.clone(),
            Arc::new(ChatModelRecord {
                base: BaseModelRecord {
                    id: model_id.clone(),
                    name: model_id.clone(),
                    n_ctx: 128_000,
                    endpoint: "https://example.invalid/v1/chat/completions".to_string(),
                    ..Default::default()
                },
                max_output_tokens: Some(128_000),
                ..Default::default()
            }),
        );
        caps.defaults.chat_default_model = model_id.clone();
        caps.defaults.chat_model_2 = model_id.clone();
        caps.defaults.chat_light_model = model_id.clone();
        caps.defaults.chat_thinking_model = model_id;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut state = gcx.caps_state.write().await;
        state.caps = Some(Arc::new(caps));
        state.last_attempted_ts = now;
    }

    async fn review_test_env_in(
        chat_id: &str,
        workspace: Option<&std::path::Path>,
    ) -> (Arc<GlobalContext>, AppState, Arc<AMutex<AtCommandsContext>>) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        install_test_caps(&gcx).await;
        let app = refact_runtime_api::app_state_from_gcx(gcx.clone()).await;
        if let Some(workspace) = workspace {
            std::fs::create_dir_all(workspace).unwrap();
            *app.workspace.documents_state.workspace_folders.lock().unwrap() = vec![
                refact_files::path_utils::canonicalize_normalized_path(workspace.to_path_buf()),
            ];
        }
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app.clone(),
                4096,
                20,
                false,
                vec![],
                chat_id.to_string(),
                Some(chat_id.to_string()),
                "test-provider/test-model".to_string(),
                None,
                None,
            )
            .await,
        ));
        (gcx, app, ccx)
    }

    async fn review_test_env(
        chat_id: &str,
    ) -> (
        Arc<GlobalContext>,
        crate::app_state::AppState,
        Arc<AMutex<AtCommandsContext>>,
    ) {
        review_test_env_in(chat_id, None).await
    }

    fn review_tool(executor: Option<Arc<dyn StageExecutor>>) -> ToolCodeReview {
        ToolCodeReview {
            config_path: String::new(),
            stage_executor_override: executor,
        }
    }

    fn fake_executor(release: Option<Arc<AtomicBool>>) -> Arc<FakeStageExecutor> {
        Arc::new(FakeStageExecutor {
            release,
            started: None,
            executed: AtomicUsize::new(0),
        })
    }

    fn tool_result_message(results: &[ContextEnum]) -> &ChatMessage {
        results
            .iter()
            .find_map(|item| match item {
                ContextEnum::ChatMessage(message) if message.role == "tool" => Some(message),
                _ => None,
            })
            .expect("tool message in results")
    }

    fn tool_result_text(results: &[ContextEnum]) -> String {
        match &tool_result_message(results).content {
            ChatContent::SimpleText(text) => text.clone(),
            other => panic!("expected plain text tool result, got {other:?}"),
        }
    }

    async fn wait_for_review_agent(
        app: &crate::app_state::AppState,
    ) -> crate::agents::types::BackgroundAgent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(record) = app
                .agents
                .list_all()
                .await
                .into_iter()
                .find(|record| record.kind == BgAgentKind::Tool && record.config_name == "review")
            {
                return record;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "tool agent did not appear"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn review_creates_a_tool_agent_bound_to_the_tool_call() {
        let (_gcx, app, ccx) = review_test_env("review-agent-parent").await;
        let mut tool = review_tool(Some(fake_executor(None)));
        let tool_call_id = "review-call-1".to_string();

        let (_ok, _results) = tool
            .tool_execute(ccx, &tool_call_id, &HashMap::new())
            .await
            .unwrap();

        let record = app
            .agents
            .list_all()
            .await
            .into_iter()
            .find(|record| record.kind == BgAgentKind::Tool && record.config_name == "review")
            .expect("tool agent record");
        assert_eq!(record.parent_tool_call_id.as_deref(), Some("review-call-1"));
        assert!(record
            .child_chat_id
            .as_deref()
            .is_some_and(|id| id.starts_with("subchat-rv-")));
    }

    #[tokio::test]
    async fn review_result_extras_carry_the_tool_agent_contract() {
        let (_gcx, _app, ccx) = review_test_env("review-extras").await;
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, results) = tool
            .tool_execute(ccx, &"review-call-2".to_string(), &HashMap::new())
            .await
            .unwrap();

        let message = tool_result_message(&results);
        let extra = &message.extra;
        assert_eq!(
            extra.get("background_agent_kind").expect("kind"),
            &json!("tool")
        );
        assert_eq!(
            extra.get("background_agent_status").expect("status"),
            &json!("completed")
        );
        assert!(extra
            .get("background_agent_id")
            .is_some_and(|value| value.is_string()));
        assert!(extra
            .get("child_chat_id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("subchat-rv-")));
        assert_eq!(extra.get("parent_chat_id").expect("parent"), &json!("review-extras"));
        assert!(extra
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| !model.is_empty()));
        assert_eq!(extra.get("title").expect("title"), &json!("Review: current change"));
        assert!(extra.get("review_report").is_some(), "review_report kept");
        assert!(extra.get("review_refs").is_some(), "review_refs kept");
        assert!(extra.get("review_refs_truncated").is_some());
    }

    #[tokio::test]
    async fn review_agent_records_steps_and_tokens() {
        let (_gcx, app, ccx) = review_test_env("review-tokens").await;
        let executor = fake_executor(None);
        let mut tool = review_tool(Some(executor.clone()));

        let (_ok, _results) = tool
            .tool_execute(ccx, &"review-call-3".to_string(), &HashMap::new())
            .await
            .unwrap();

        let executed = executor.executed.load(Ordering::SeqCst);
        assert!(executed >= 1, "expected stages to run through the executor");
        let record = wait_for_review_agent(&app).await;
        assert!(
            record.step_count as usize >= executed,
            "step_count {} must cover {} completed stages",
            record.step_count,
            executed
        );
        assert_eq!(
            record.tokens_used,
            (executed as u64) * 30,
            "tokens_used must be summed from the fake stage usage"
        );
    }

    #[tokio::test]
    async fn cancelling_the_review_agent_marks_it_cancelled_and_returns_a_partial_report() {
        let (_gcx, app, ccx) = review_test_env("review-cancel").await;
        let release = Arc::new(AtomicBool::new(false));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel::<()>();
        let mut tool = review_tool(Some(Arc::new(FakeStageExecutor {
            release: Some(release.clone()),
            started: Some(started_tx),
            executed: AtomicUsize::new(0),
        })));
        let chat_id = "review-cancel".to_string();
        let run = tokio::spawn(async move {
            tool.tool_execute(
                ccx,
                &"review-cancel-call".to_string(),
                &HashMap::new(),
            )
            .await
        });

        let record = wait_for_review_agent(&app).await;
        tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
            .await
            .expect("a stage must start while the review is still running")
            .expect("stage start signal");
        assert_eq!(
            app.agents.get_any(&record.agent_id).await.expect("record").status,
            BgAgentStatus::Running
        );
        app.agents
            .cancel(&chat_id, &record.agent_id, None)
            .await
            .expect("cancel");
        release.store(true, Ordering::SeqCst);

        let (_ok, results) = run.await.unwrap().expect("tool result despite cancellation");
        let markdown = tool_result_text(&results);
        assert!(markdown.contains("## Review"), "{markdown}");
        assert!(markdown.contains("cancelled"), "{markdown}");
        let message = tool_result_message(&results);
        assert_eq!(
            message.extra.get("background_agent_status").expect("status"),
            &json!("cancelled")
        );
        let record = app.agents.get_any(&record.agent_id).await.expect("record");
        assert_eq!(record.status, BgAgentStatus::Cancelled);
    }

    #[tokio::test]
    async fn review_without_a_registry_failure_still_returns_a_report() {
        let (gcx, _app, ccx) = review_test_env("review-degraded").await;
        let storage_root = gcx.cache_dir.join(".refact").join("background_agents");
        std::fs::remove_dir_all(&storage_root).ok();
        std::fs::write(&storage_root, b"not a directory").unwrap();
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, results) = tool
            .tool_execute(ccx, &"review-call-5".to_string(), &HashMap::new())
            .await
            .expect("review must complete despite the tool agent registry failure");

        let markdown = tool_result_text(&results);
        assert!(markdown.contains("## Review"), "{markdown}");
        let message = tool_result_message(&results);
        assert!(
            message.extra.get("background_agent_id").is_none(),
            "degraded run must not carry tool agent extras"
        );
    }

    async fn review_child_chat_id(app: &AppState) -> String {
        app.agents
            .list_all()
            .await
            .into_iter()
            .find(|record| record.kind == BgAgentKind::Tool && record.config_name == "review")
            .and_then(|record| record.child_chat_id)
            .expect("the review tool agent carries the child chat id")
    }

    #[tokio::test]
    async fn review_chat_trajectory_contains_the_request_and_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app, ccx) = review_test_env_in("review-trajectory", Some(dir.path())).await;
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, _results) = tool
            .tool_execute(ccx, &"review-trajectory-call".to_string(), &HashMap::new())
            .await
            .unwrap();

        let child_chat_id = review_child_chat_id(&app).await;
        let loaded = refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
            &app.chat_ctx(),
            &child_chat_id,
        )
        .await
        .expect("the review chat trajectory is readable");

        assert_eq!(loaded.messages.len(), 2, "{:?}", loaded.messages);
        assert_eq!(loaded.messages[0].role, "user");
        let request = loaded.messages[0].content.content_text_only();
        assert!(request.contains("focus: current change"), "{request}");
        assert!(request.contains("depth: normal"), "{request}");
        assert_eq!(loaded.messages[1].role, "assistant");
        assert!(
            loaded.messages[1]
                .content
                .content_text_only()
                .starts_with("## Review"),
            "{}",
            loaded.messages[1].content.content_text_only()
        );
    }

    #[tokio::test]
    async fn review_chat_link_type_is_tool_and_parent_is_the_calling_chat() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app, ccx) = review_test_env_in("review-link", Some(dir.path())).await;
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, _results) = tool
            .tool_execute(ccx, &"review-link-call".to_string(), &HashMap::new())
            .await
            .unwrap();

        let child_chat_id = review_child_chat_id(&app).await;
        let loaded = refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
            &app.chat_ctx(),
            &child_chat_id,
        )
        .await
        .expect("the review chat trajectory is readable");

        assert_eq!(loaded.thread.id, child_chat_id);
        assert_eq!(loaded.thread.link_type.as_deref(), Some("tool"));
        assert_eq!(loaded.thread.parent_id.as_deref(), Some("review-link"));
        assert_eq!(loaded.thread.root_chat_id.as_deref(), Some("review-link"));
        assert_eq!(loaded.thread.title, "Review: current change");
    }

    #[tokio::test]
    async fn stage_traces_are_nested_under_the_review_chat() {
        let (gcx, _app, _ccx) = review_test_env("review-nested").await;
        let (tx, _rx) = mpsc::unbounded_channel::<Value>();
        let ctx = StageCtx {
            review_id: "rv-nested".to_string(),
            tool_call_id: "tc-nested".to_string(),
            subchat_tx: Arc::new(AMutex::new(tx)),
            abort_flag: Arc::new(AtomicBool::new(false)),
            depth: 0,
            task_meta: None,
            worktree: None,
            chat_id: "review-nested".to_string(),
            trace_parent_chat_id: "subchat-rv-nested".to_string(),
            root_chat_id: "review-nested".to_string(),
            activity: None,
        };
        let job = StageJob {
            spec: Arc::new(
                crate::tools::review_agents::stages::embedded_catalog()
                    .into_iter()
                    .find(|spec| spec.id == "diff")
                    .unwrap(),
            ),
            label: "diff".to_string(),
            subchat: crate::subchat::ExplicitSubchatSpec {
                params: refact_chat_api::SubchatParameters {
                    subchat_model_type: refact_core::chat_types::ChatModelType::Default,
                    subchat_model: "test-provider/test-model".to_string(),
                    subchat_n_ctx: 100_000,
                    subchat_max_new_tokens: 8000,
                    subchat_temperature: None,
                    subchat_tokens_for_rag: 0,
                    subchat_reasoning_effort: None,
                    subchat_cache_control: crate::llm::params::CacheControl::Off,
                },
                model: "test-provider/test-model".to_string(),
                autonomous_no_confirm: true,
            },
            max_steps: 10,
            prompt: String::new(),
            trace_chat_id: stage_trace_chat_id("rv-nested", "diff"),
            abort: Arc::new(AtomicBool::new(false)),
        };

        let config = stage_subchat_config(gcx, &ctx, &job)
            .await
            .expect("stage subchat config");

        assert_eq!(config.parent_id.as_deref(), Some("subchat-rv-nested"));
        assert_ne!(config.parent_id.as_deref(), Some(ctx.chat_id.as_str()));
        assert_eq!(config.root_chat_id.as_deref(), Some("review-nested"));
        assert_eq!(config.chat_id.as_deref(), Some("subchat-rv-nested-diff"));
    }

    #[tokio::test]
    async fn review_chat_session_is_live_during_the_run_and_closed_after() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app, ccx) = review_test_env_in("review-session", Some(dir.path())).await;
        let release = Arc::new(AtomicBool::new(false));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel::<()>();
        let mut tool = review_tool(Some(Arc::new(FakeStageExecutor {
            release: Some(release.clone()),
            started: Some(started_tx),
            executed: AtomicUsize::new(0),
        })));
        let run = tokio::spawn(async move {
            tool.tool_execute(ccx, &"review-session-call".to_string(), &HashMap::new())
                .await
        });

        tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
            .await
            .expect("a stage must start while the review is still running")
            .expect("stage start signal");
        let (chat_id, session_arc) = {
            let sessions = app.chat.sessions.read().await;
            sessions
                .iter()
                .find(|(id, _)| id.starts_with("subchat-rv-"))
                .map(|(id, session_arc)| (id.clone(), session_arc.clone()))
                .expect("the review chat has a live session")
        };
        {
            let session = session_arc.lock().await;
            assert_eq!(session.runtime.state, SessionState::Starting);
            assert_eq!(session.thread.link_type.as_deref(), Some("tool"));
            assert_eq!(session.thread.parent_id.as_deref(), Some("review-session"));
        }

        release.store(true, Ordering::SeqCst);
        let (_ok, _results) = run.await.unwrap().expect("review result");

        let session_after = app.chat.sessions.read().await.get(&chat_id).cloned();
        if let Some(session_after) = session_after {
            assert!(session_after.lock().await.closed);
        }
    }

    #[tokio::test]
    async fn review_survives_a_trajectory_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".refact"), b"not a directory").unwrap();
        let (_gcx, _app, ccx) = review_test_env_in("review-no-trajectory", Some(dir.path())).await;
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, results) = tool
            .tool_execute(ccx, &"review-no-trajectory-call".to_string(), &HashMap::new())
            .await
            .expect("review must return a report even when the trajectory cannot be written");

        let markdown = tool_result_text(&results);
        assert!(markdown.contains("## Review"), "{markdown}");
    }

    #[test]
    fn parse_args_defaults_wait_to_true() {
        assert!(parse_args(&args_map(vec![])).unwrap().wait);
        assert!(parse_args(&args_map(vec![("wait", Value::Null)]))
            .unwrap()
            .wait);
        assert!(parse_args(&args_map(vec![("wait", json!(true))]))
            .unwrap()
            .wait);
    }

    #[test]
    fn parse_args_reads_wait_false() {
        assert!(!parse_args(&args_map(vec![("wait", json!(false))]))
            .unwrap()
            .wait);
    }

    #[test]
    fn parse_args_rejects_non_boolean_wait() {
        let error = parse_args(&args_map(vec![("wait", json!("later"))])).unwrap_err();
        assert_eq!(error, "argument `wait` must be a boolean");
    }

    fn background_agent_id(results: &[ContextEnum]) -> String {
        tool_result_message(results)
            .extra
            .get("background_agent_id")
            .and_then(Value::as_str)
            .expect("background agent id in the tool result extras")
            .to_string()
    }

    async fn wait_for_review_terminal(
        app: &AppState,
        parent_chat_id: &str,
        agent_id: &str,
    ) -> crate::agents::types::BackgroundAgent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let record = app
                .agents
                .get(parent_chat_id, agent_id)
                .await
                .expect("agent record");
            if record.status.is_terminal() {
                return record;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the background review did not finish, status {}",
                record.status.as_str()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn wait_false_returns_immediately_with_running_extras() {
        let (_gcx, app, ccx) = review_test_env("review-background-fast").await;
        let release = Arc::new(AtomicBool::new(false));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel::<()>();
        let mut tool = review_tool(Some(Arc::new(FakeStageExecutor {
            release: Some(release.clone()),
            started: Some(started_tx),
            executed: AtomicUsize::new(0),
        })));

        let (_ok, results) = tokio::time::timeout(
            Duration::from_secs(10),
            tool.tool_execute(
                ccx,
                &"review-background-fast-call".to_string(),
                &args_map(vec![("wait", json!(false))]),
            ),
        )
        .await
        .expect("wait=false must return before the review finishes")
        .unwrap();

        let message = tool_result_message(&results);
        let extra = &message.extra;
        assert_eq!(
            extra.get("background_agent_kind").expect("kind"),
            &json!("tool")
        );
        assert_eq!(
            extra.get("background_agent_status").expect("status"),
            &json!("running")
        );
        assert_eq!(extra.get("review_started").expect("flag"), &json!(true));
        assert!(extra.get("review_report").is_none(), "no report yet");
        let child_chat_id = extra
            .get("child_chat_id")
            .and_then(Value::as_str)
            .expect("child chat id");
        assert!(child_chat_id.starts_with("subchat-rv-"), "{child_chat_id}");

        let markdown = tool_result_text(&results);
        assert!(
            markdown.starts_with("✓ Started background review: current change"),
            "{markdown}"
        );
        assert!(markdown.contains("- status: running"), "{markdown}");
        assert!(
            markdown.contains(&format!("[view](refact://chat/{child_chat_id})")),
            "{markdown}"
        );
        assert!(
            markdown.contains("The completion will be pushed back into this chat automatically."),
            "{markdown}"
        );

        let record = app
            .agents
            .get_any(&background_agent_id(&results))
            .await
            .expect("background agent record");
        assert_eq!(
            record.status,
            BgAgentStatus::Running,
            "the review must still be running when wait=false returns"
        );

        tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
            .await
            .expect("the background run must reach a stage after the tool returned")
            .expect("stage start signal");
        release.store(true, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn wait_false_completion_marks_the_agent_completed() {
        let (_gcx, app, ccx) = review_test_env("review-background-done").await;
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, results) = tool
            .tool_execute(
                ccx,
                &"review-background-done-call".to_string(),
                &args_map(vec![("wait", json!(false))]),
            )
            .await
            .unwrap();

        let agent_id = background_agent_id(&results);
        let record = wait_for_review_terminal(&app, "review-background-done", &agent_id).await;

        assert_eq!(record.status, BgAgentStatus::Completed);
        let summary = record.result_summary.expect("result summary");
        assert!(summary.starts_with("## Review"), "{summary}");
    }

    #[tokio::test]
    async fn agent_result_returns_the_review_report() {
        let (_gcx, app, ccx) = review_test_env("review-background-result").await;
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, results) = tool
            .tool_execute(
                ccx.clone(),
                &"review-background-result-call".to_string(),
                &args_map(vec![("wait", json!(false))]),
            )
            .await
            .unwrap();

        let agent_id = background_agent_id(&results);
        wait_for_review_terminal(&app, "review-background-result", &agent_id).await;

        let mut result_tool = crate::tools::tool_background_agents::ToolAgentResult {
            config_path: String::new(),
        };
        let (_ok, output) = result_tool
            .tool_execute(
                ccx,
                &"agent-result-call".to_string(),
                &args_map(vec![("agent_id", json!(agent_id))]),
            )
            .await
            .unwrap();

        let text = tool_result_text(&output);
        assert!(text.contains("## Review"), "{text}");
        assert!(text.contains("Stages:"), "{text}");
    }

    #[tokio::test]
    async fn wait_false_pushes_the_completion_into_the_parent_chat() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app, ccx) = review_test_env_in("review-background-push", Some(dir.path())).await;
        let parent = Arc::new(AMutex::new(ChatSession::new(
            "review-background-push".to_string(),
        )));
        parent
            .lock()
            .await
            .queue_processor_running
            .store(true, Ordering::SeqCst);
        app.chat
            .sessions
            .write()
            .await
            .insert("review-background-push".to_string(), parent.clone());
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, results) = tool
            .tool_execute(
                ccx,
                &"review-background-push-call".to_string(),
                &args_map(vec![("wait", json!(false))]),
            )
            .await
            .unwrap();

        let agent_id = background_agent_id(&results);
        wait_for_review_terminal(&app, "review-background-push", &agent_id).await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let landed = {
                let session = parent.lock().await;
                let in_messages = session
                    .messages
                    .iter()
                    .any(|message| message.content.content_text_only().contains(COMPLETION_PUSH_TEXT));
                let in_queue = session.pending_deliveries.iter().any(|delivery| {
                    delivery
                        .messages
                        .iter()
                        .any(|message| message.content.content_text_only().contains(COMPLETION_PUSH_TEXT))
                });
                in_messages || in_queue
            };
            let record = app
                .agents
                .get("review-background-push", &agent_id)
                .await
                .expect("agent record");
            let recorded = record
                .completion_message_id
                .as_deref()
                .is_some_and(|id| id != "pending" && id != "deferred");
            if landed && recorded {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the review completion was never pushed into the parent chat: landed={landed}, id={:?}",
                record.completion_message_id
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn wait_true_keeps_returning_the_report_inline() {
        let (_gcx, app, ccx) = review_test_env("review-wait-true").await;
        let mut tool = review_tool(Some(fake_executor(None)));

        let (_ok, results) = tool
            .tool_execute(
                ccx,
                &"review-wait-true-call".to_string(),
                &args_map(vec![("wait", json!(true))]),
            )
            .await
            .unwrap();

        let markdown = tool_result_text(&results);
        assert!(markdown.starts_with("## Review"), "{markdown}");
        assert!(!markdown.contains("Started background review"), "{markdown}");

        let message = tool_result_message(&results);
        assert_eq!(
            message.extra.get("background_agent_status").expect("status"),
            &json!("completed")
        );
        assert!(message.extra.get("review_started").is_none());
        assert!(message.extra.get("review_report").is_some());

        let agent_id = background_agent_id(&results);
        let record = wait_for_review_terminal(&app, "review-wait-true", &agent_id).await;
        assert_eq!(record.status, BgAgentStatus::Completed);
        let summary = record.result_summary.expect("result summary");
        assert!(!summary.starts_with("## Review"), "{summary}");
        assert!(summary.contains("stages"), "{summary}");
    }

    struct SteeringStageExecutor {
        app: AppState,
        prompts: Arc<AMutex<Vec<(String, String)>>>,
        steer_on: Option<&'static str>,
        steer_text: &'static str,
        via_chat: bool,
        cancel_after_steering: bool,
        steered: AtomicBool,
    }

    impl SteeringStageExecutor {
        fn new(app: AppState, prompts: Arc<AMutex<Vec<(String, String)>>>) -> Self {
            Self {
                app,
                prompts,
                steer_on: None,
                steer_text: "",
                via_chat: false,
                cancel_after_steering: false,
                steered: AtomicBool::new(false),
            }
        }

        fn steering(mut self, on: &'static str, text: &'static str, via_chat: bool) -> Self {
            self.steer_on = Some(on);
            self.steer_text = text;
            self.via_chat = via_chat;
            self
        }

        async fn send_steering(&self) {
            let record = app_review_agent(&self.app).await;
            if self.via_chat {
                let chat_id = record.child_chat_id.clone().expect("review child chat id");
                let delivery = PendingDelivery::new(
                    vec![ChatMessage::new(
                        "user".to_string(),
                        self.steer_text.to_string(),
                    )],
                    PushMode::Append,
                    "user",
                    true,
                );
                crate::chat::delivery::deliver_to_chat(self.app.clone(), &chat_id, delivery)
                    .await
                    .expect("steering delivery into the review chat");
            } else {
                self.app
                    .agents
                    .push_inbox(
                        &record.agent_id,
                        InboxMessage {
                            from: "user".to_string(),
                            text: self.steer_text.to_string(),
                            queued_at: chrono::Utc::now(),
                        },
                    )
                    .await
                    .expect("steering message into the review agent inbox");
            }
            if self.cancel_after_steering {
                self.app
                    .agents
                    .cancel(&record.parent_chat_id, &record.agent_id, None)
                    .await
                    .expect("cancel review");
            }
        }
    }

    #[async_trait]
    impl StageExecutor for SteeringStageExecutor {
        async fn execute(&self, job: StageJob, _activity: Arc<AtomicU64>) -> StageProduct {
            self.prompts
                .lock()
                .await
                .push((job.label.clone(), job.prompt.clone()));
            if self.steer_on == Some(job.label.as_str())
                && !self.steered.swap(true, Ordering::SeqCst)
            {
                self.send_steering().await;
            }
            StageProduct {
                run: StageRun::ok(&job.label, Some("test-provider/test-model".to_string()), 1),
                findings: vec![finding("rf-1", ReviewSeverity::High)],
                verdicts: vec![],
                metering: serde_json::Map::new(),
                raw: None,
            }
        }
    }

    async fn app_review_agent(app: &AppState) -> crate::agents::types::BackgroundAgent {
        app.agents
            .list_all()
            .await
            .into_iter()
            .find(|record| record.kind == BgAgentKind::Tool && record.config_name == "review")
            .expect("review tool agent record")
    }

    fn captured_prompt(prompts: &[(String, String)], label: &str) -> String {
        prompts
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, prompt)| prompt.clone())
            .unwrap_or_else(|| {
                panic!(
                    "no prompt captured for {label}: {:?}",
                    prompts
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>()
                )
            })
    }

    const STEERING_HEADER: &str = "# User steering (arrived while the review was running)";

    async fn run_review_capturing_prompts(
        chat_id: &str,
        steer: Option<(&'static str, &'static str, bool)>,
    ) -> Vec<(String, String)> {
        let (_gcx, app, ccx) = review_test_env(chat_id).await;
        let prompts = Arc::new(AMutex::new(Vec::new()));
        let mut executor = SteeringStageExecutor::new(app.clone(), prompts.clone());
        if let Some((label, text, via_chat)) = steer {
            executor = executor.steering(label, text, via_chat);
        }
        let mut tool = review_tool(Some(Arc::new(executor)));

        tool.tool_execute(
            ccx,
            &format!("{chat_id}-call"),
            &args_map(vec![("stages", json!(["diff", "adversarial"]))]),
        )
        .await
        .expect("review result");

        let captured = prompts.lock().await.clone();
        captured
    }

    #[tokio::test]
    async fn steering_messages_are_injected_into_the_adversarial_prompt() {
        let prompts = run_review_capturing_prompts(
            "review-steering-prompt",
            Some(("diff", "please also check the retry path", false)),
        )
        .await;

        let diff = captured_prompt(&prompts, "diff");
        assert!(!diff.contains(STEERING_HEADER), "{diff}");
        let adversarial = captured_prompt(&prompts, "adversarial");
        assert!(adversarial.contains(STEERING_HEADER), "{adversarial}");
        assert!(
            adversarial.contains("- please also check the retry path"),
            "{adversarial}"
        );
    }

    #[tokio::test]
    async fn steering_messages_are_written_to_the_review_trajectory() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app, ccx) = review_test_env_in("review-steering-trace", Some(dir.path())).await;
        let prompts = Arc::new(AMutex::new(Vec::new()));
        let executor = Arc::new(
            SteeringStageExecutor::new(app.clone(), prompts.clone()).steering(
                "diff",
                "look at the retry path too",
                true,
            ),
        );
        let mut tool = review_tool(Some(executor));

        tool.tool_execute(
            ccx,
            &"review-steering-trace-call".to_string(),
            &args_map(vec![("stages", json!(["diff", "adversarial"]))]),
        )
        .await
        .expect("review result");

        let child_chat_id = review_child_chat_id(&app).await;
        let loaded = refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
            &app.chat_ctx(),
            &child_chat_id,
        )
        .await
        .expect("the review chat trajectory is readable");

        let steering: Vec<&ChatMessage> = loaded
            .messages
            .iter()
            .filter(|message| {
                message.role == "user"
                    && message
                        .content
                        .content_text_only()
                        .contains("look at the retry path too")
            })
            .collect();
        assert_eq!(steering.len(), 1, "{:?}", loaded.messages);
        assert_eq!(loaded.messages.last().unwrap().role, "assistant");
        let record = app_review_agent(&app).await;
        assert!(record.pending_deliveries.is_empty());
        assert_eq!(record.delivery_ids.len(), 1);
        assert_eq!(steering[0].extra["delivery"]["id"], record.delivery_ids[0]);
        let adversarial = captured_prompt(&prompts.lock().await, "adversarial");
        assert!(adversarial.contains("look at the retry path too"));
    }

    #[tokio::test]
    async fn empty_inbox_keeps_prompts_unchanged() {
        let steered = run_review_capturing_prompts(
            "review-steering-empty-steered",
            Some(("diff", "please also check the retry path", false)),
        )
        .await;
        let plain = run_review_capturing_prompts("review-steering-empty-plain", None).await;

        let block = format!("{STEERING_HEADER}\n- please also check the retry path\n\n");
        assert_eq!(
            captured_prompt(&steered, "diff"),
            captured_prompt(&plain, "diff")
        );
        let plain_adversarial = captured_prompt(&plain, "adversarial");
        assert!(
            !plain_adversarial.contains(STEERING_HEADER),
            "{plain_adversarial}"
        );
        assert_eq!(
            captured_prompt(&steered, "adversarial"),
            format!("{block}{plain_adversarial}")
        );
    }

    #[tokio::test]
    async fn late_steering_is_noted_in_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app, ccx) = review_test_env_in("review-steering-late", Some(dir.path())).await;
        let prompts = Arc::new(AMutex::new(Vec::new()));
        let executor = Arc::new(
            SteeringStageExecutor::new(app.clone(), prompts.clone()).steering(
                "adversarial",
                "check the retry path",
                false,
            ),
        );
        let mut tool = review_tool(Some(executor));

        let (_ok, results) = tool
            .tool_execute(
                ccx,
                &"review-steering-late-call".to_string(),
                &args_map(vec![("stages", json!(["diff", "adversarial"]))]),
            )
            .await
            .expect("review result");

        let markdown = tool_result_text(&results);
        assert!(
            markdown.contains(
                "> Note: 1 user message(s) arrived after the last stage and were not applied."
            ),
            "{markdown}"
        );
        let adversarial = captured_prompt(&prompts.lock().await.clone(), "adversarial");
        assert!(!adversarial.contains(STEERING_HEADER), "{adversarial}");
        let record = app_review_agent(&app).await;
        assert!(record.result_summary.unwrap().contains(
            "> Note: 1 user message(s) arrived after the last stage and were not applied."
        ));
        let loaded = refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
            &app.chat_ctx(),
            record.child_chat_id.as_deref().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(loaded.messages[1].role, "user");
        assert_eq!(
            loaded.messages[1].content.content_text_only(),
            "check the retry path"
        );
        assert_eq!(loaded.messages[2].content.content_text_only(), markdown);
    }

    #[tokio::test]
    async fn background_review_applies_steering_and_reports_late_messages() {
        for stage in ["diff", "adversarial"] {
            let dir = tempfile::tempdir().unwrap();
            let parent_chat_id = format!("review-background-steering-{stage}");
            let (_gcx, app, ccx) = review_test_env_in(&parent_chat_id, Some(dir.path())).await;
            let prompts = Arc::new(AMutex::new(Vec::new()));
            let executor = SteeringStageExecutor::new(app.clone(), prompts.clone()).steering(
                stage,
                "verify retry behavior",
                true,
            );
            let mut tool = review_tool(Some(Arc::new(executor)));
            let (_, results) = tool
                .tool_execute(
                    ccx,
                    &"background-steering-call".to_string(),
                    &args_map(vec![
                        ("stages", json!(["diff", "adversarial"])),
                        ("wait", json!(false)),
                    ]),
                )
                .await
                .unwrap();
            let record =
                wait_for_review_terminal(&app, &parent_chat_id, &background_agent_id(&results))
                    .await;
            let adversarial = captured_prompt(&prompts.lock().await, "adversarial");
            let late = stage == "adversarial";
            assert_eq!(adversarial.contains(STEERING_HEADER), !late);
            assert_eq!(adversarial.contains("verify retry behavior"), !late);
            let summary = record.result_summary.unwrap();
            assert_eq!(
                summary.contains(
                    "> Note: 1 user message(s) arrived after the last stage and were not applied."
                ),
                late
            );
            let loaded = refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
                &app.chat_ctx(),
                record.child_chat_id.as_deref().unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(loaded.messages[1].role, "user");
            assert_eq!(
                loaded.messages[1].content.content_text_only(),
                "verify retry behavior"
            );
            assert_eq!(loaded.messages[2].content.content_text_only(), summary);
            assert!(record.pending_deliveries.is_empty());
        }
    }

    #[tokio::test]
    async fn cancelled_review_does_not_drain_steering() {
        let (_gcx, app, ccx) = review_test_env("review-cancel-steering").await;
        let prompts = Arc::new(AMutex::new(Vec::new()));
        let mut executor = SteeringStageExecutor::new(app.clone(), prompts.clone()).steering(
            "diff",
            "do not consume after cancellation",
            true,
        );
        executor.cancel_after_steering = true;
        let mut tool = review_tool(Some(Arc::new(executor)));
        let (_, results) = tool
            .tool_execute(
                ccx,
                &"cancel-steering-call".to_string(),
                &args_map(vec![("stages", json!(["diff", "adversarial"]))]),
            )
            .await
            .unwrap();
        assert!(tool_result_text(&results).contains("cancelled"));
        assert_eq!(prompts.lock().await.len(), 1);
        let record = app_review_agent(&app).await;
        assert_eq!(record.status, BgAgentStatus::Cancelled);
        assert_eq!(record.pending_deliveries.len(), 1);
        assert!(record.delivery_ids.is_empty());
    }

    #[tokio::test]
    async fn steering_is_visible_live_and_acknowledged_only_after_a_successful_save() {
        let dir = tempfile::tempdir().unwrap();
        let (_gcx, app, _ccx) = review_test_env_in("review-steering-ack", Some(dir.path())).await;
        let thread = ThreadParams {
            id: "subchat-review-steering-ack".to_string(),
            root_chat_id: Some("review-steering-ack".to_string()),
            parent_id: Some("review-steering-ack".to_string()),
            link_type: Some("tool".to_string()),
            ..Default::default()
        };
        let request = ChatMessage::new("user".to_string(), "Review request".to_string());
        let session = open_review_chat(&app, &thread, &request).await;
        let handle = ToolAgentHandle::begin(
            &app,
            "review-steering-ack",
            "review-steering-ack",
            ToolAgentSpec {
                tool_name: "review".to_string(),
                title: "Review".to_string(),
                prompt: "Review".to_string(),
                model: "test-provider/test-model".to_string(),
                model_type: None,
                target_files: vec![],
                child_chat_id: thread.id.clone(),
                parent_tool_call_id: "ack-call".to_string(),
            },
        )
        .await
        .unwrap();
        let inbox = SteeringInbox {
            app: app.clone(),
            agent_id: Some(handle.agent_id().to_string()),
            session: session.clone(),
        };
        let body = json!({
            "client_request_id": "steering-request",
            "type": "user_message",
            "content": "check the retry path",
        });
        let response = crate::chat::handle_v1_chat_command(
            axum::extract::State(app.clone()),
            axum::extract::Path(thread.id.clone()),
            hyper::body::Bytes::from(body.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), hyper::StatusCode::OK);
        assert_eq!(
            app.agents.pending_deliveries(handle.agent_id()).await.len(),
            1
        );
        std::fs::write(dir.path().join(".refact"), b"not a directory").unwrap();
        let messages = inbox.drain(false).await;
        assert_eq!(
            inbox.accept(messages, Some(&handle), 3).await,
            vec!["check the retry path"]
        );
        assert_eq!(
            session.lock().await.messages[1].content.content_text_only(),
            "check the retry path"
        );
        assert_eq!(
            app.agents.pending_deliveries(handle.agent_id()).await.len(),
            1
        );
        let record = app.agents.get_any(handle.agent_id()).await.unwrap();
        assert_eq!(record.step_count, 3);
        assert_eq!(
            record.progress.as_deref(),
            Some("user steering: check the retry path")
        );
        assert!(record.delivery_ids.is_empty());
        std::fs::remove_file(dir.path().join(".refact")).unwrap();
        inbox.persist().await;
        assert!(app
            .agents
            .pending_deliveries(handle.agent_id())
            .await
            .is_empty());
        let loaded = refact_runtime_api::trajectory_runtime::load_trajectory_for_chat(
            &app.chat_ctx(),
            &thread.id,
        )
        .await
        .unwrap();
        assert_eq!(loaded.messages[1].role, "user");
        assert_eq!(
            loaded.messages[1].content.content_text_only(),
            "check the retry path"
        );
        close_review_chat(&app, &thread.id).await;
        handle.complete("done".to_string()).await.unwrap();
    }
}
