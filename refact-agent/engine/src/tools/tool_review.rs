use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::sync::Mutex as AMutex;

use crate::agents::tool_agent::{ToolAgentHandle, ToolAgentSpec};
use crate::app_state::AppState;
use crate::at_commands::at_commands::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use crate::chat::trajectories::save_trajectory_as_with_intent_checked;
use crate::chat::types::{ChatSession, PushMode, SessionState, ThreadParams, TrajectoryCommitIntent};
use crate::global_context::GlobalContext;
use crate::postprocessing::pp_command_output::OutputFilter;
use crate::tools::ctx::ToolCtx;
use crate::tools::review_agents::config::{
    load_review_config, slot_model_id, stage_subchat_spec, ReviewConfig,
};
use crate::tools::review_agents::prompts::{build_adversarial_prompt, build_stage_prompt};
use crate::tools::review_agents::runner::{
    merge_metering, now_ms, stage_trace_chat_id, ProgressSink, StageCtx, StageJob, StageProgress,
};
use crate::tools::review_agents::stages::{load_stage_catalog, select_stages, StagePhase, StageSpec};
use crate::tools::review_agents::{
    run_stage_jobs, ScheduleParams, StageExecutor, SubchatExecutor, HARVEST_GRACE,
};
use crate::tools::review_evidence::verify_evidence;
use crate::tools::review_merge::{merge_findings, rank_findings};
use crate::tools::review_scope::{build_review_scope, DiffAttribution, ReviewScope, ScopeRequest};
use crate::tools::review_types::{
    Dispute, ReviewDepth, ReviewFinding, ReviewOutcome, ReviewReport, ReviewSeverity, ScopeMode,
    StageRun, StageStatusKind,
};
use crate::tools::subagent_phases::resolve_gathered_file_path;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use crate::worktrees::scope::ExecutionScope;

pub struct ToolCodeReview {
    pub config_path: String,
    pub(crate) stage_executor_override: Option<Arc<dyn StageExecutor>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewArgs {
    what_to_check: Option<String>,
    files: Vec<String>,
    base: Option<String>,
    plan: Option<String>,
    scope_mode: ScopeMode,
    stages: Option<Vec<String>>,
    depth: Option<ReviewDepth>,
    parallel_depth: Option<usize>,
    variants: Option<usize>,
    browser_scenario: Option<String>,
    wait: bool,
}

fn optional_string(args: &HashMap<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_list(args: &HashMap<String, Value>, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn positive_number(args: &HashMap<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let number = value
                .as_u64()
                .ok_or_else(|| format!("argument `{key}` must be a positive integer"))?;
            if number == 0 {
                return Err(format!("argument `{key}` must be at least 1"));
            }
            Ok(Some(number))
        }
    }
}

fn parse_args(args: &HashMap<String, Value>) -> Result<ReviewArgs, String> {
    let files = string_list(args, "files");
    let base = optional_string(args, "base");
    let wait = match args.get("wait") {
        None | Some(Value::Null) => true,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "argument `wait` must be a boolean".to_string())?,
    };
    let depth = match optional_string(args, "depth") {
        Some(value) => Some(
            ReviewDepth::parse(&value)
                .ok_or_else(|| format!("invalid depth '{value}', expected: normal, deep"))?,
        ),
        None => None,
    };
    let scope_mode = match optional_string(args, "scope_mode") {
        Some(value) => ScopeMode::parse(&value).ok_or_else(|| {
            format!("invalid scope_mode '{value}', expected: strict, adjacent, broad")
        })?,
        None if !files.is_empty() || base.is_some() => ScopeMode::Strict,
        None => ScopeMode::Broad,
    };
    let stages = {
        let requested = string_list(args, "stages");
        (!requested.is_empty()).then_some(requested)
    };
    let browser_requested = match args.get("browser") {
        None | Some(Value::Null) => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "argument `browser` must be a boolean".to_string())?,
    };
    let browser_scenario = optional_string(args, "browser_scenario");
    if browser_requested && browser_scenario.is_none() {
        return Err(
            "argument `browser_scenario` is required when `browser` is true: name the URL or route, the states to reach, and what must be true".to_string(),
        );
    }
    let variants = positive_number(args, "variants")?;
    if let Some(variants) = variants {
        if variants > 3 {
            return Err("argument `variants` must be between 1 and 3".to_string());
        }
    }
    Ok(ReviewArgs {
        what_to_check: optional_string(args, "what_to_check"),
        files,
        base,
        plan: optional_string(args, "plan"),
        scope_mode,
        stages,
        depth,
        parallel_depth: positive_number(args, "parallel_depth")?.map(|value| value as usize),
        variants: variants.map(|value| value as usize),
        browser_scenario,
        wait,
    })
}

fn format_duration(duration_ms: u64) -> String {
    let seconds = duration_ms / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    format!("{}m{:02}s", seconds / 60, seconds % 60)
}

fn markdown_cell(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn scratch_dir(gcx_project: Option<&Path>, review_id: &str) -> Option<PathBuf> {
    gcx_project.map(|root| root.join(".refact").join("review_scratch").join(review_id))
}

async fn write_scratch(dir: &Path, stage: &str, payload: &Value) -> bool {
    if tokio::fs::create_dir_all(dir).await.is_err() {
        return false;
    }
    let path = dir.join(format!("{stage}.json"));
    match serde_json::to_string_pretty(payload) {
        Ok(text) => tokio::fs::write(path, text).await.is_ok(),
        Err(_) => false,
    }
}

const BROWSER_STAGE: &str = "browser";

fn apply_browser_scenario(
    catalog: &[StageSpec],
    scheduled: &mut Vec<StageSpec>,
    skipped: &mut Vec<(String, String)>,
    scenario: Option<&str>,
) {
    match scenario {
        None => {
            if let Some(index) = scheduled.iter().position(|spec| spec.id == BROWSER_STAGE) {
                scheduled.remove(index);
                skipped.push((
                    BROWSER_STAGE.to_string(),
                    "no browser_scenario given".to_string(),
                ));
            }
        }
        Some(_) => {
            if scheduled.iter().any(|spec| spec.id == BROWSER_STAGE) {
                return;
            }
            let Some(spec) = catalog.iter().find(|spec| spec.id == BROWSER_STAGE) else {
                return;
            };
            skipped.retain(|(id, _)| id != BROWSER_STAGE);
            scheduled.push(spec.clone());
        }
    }
}

struct StagePlan {
    jobs: Vec<StageJob>,
    rows: Vec<StageRun>,
}

#[allow(clippy::too_many_arguments)]
async fn plan_jobs(
    gcx: Arc<GlobalContext>,
    cfg: &ReviewConfig,
    review_id: &str,
    scope: &ReviewScope,
    specs: Vec<StageSpec>,
    variants: usize,
    browser_scenario: Option<&str>,
) -> StagePlan {
    let mut jobs = Vec::new();
    let mut rows = Vec::new();
    let slots = cfg.settings.slots_for_variants(variants);
    for spec in specs {
        let overrides = cfg.settings.stage_override(&spec.id);
        overrides.warn_if_deprecated(&spec.id);
        if overrides.enabled == Some(false) {
            rows.push(StageRun::not_run(
                &spec.id,
                "disabled in review_agents.yaml",
            ));
            continue;
        }
        let stage_slots = match overrides.model_slot {
            Some(slot) => vec![slot],
            None if spec.phase == StagePhase::PostMerge => vec![slots[0]],
            None => slots.clone(),
        };
        let spec = Arc::new(spec);
        for (index, slot) in stage_slots.iter().enumerate() {
            let model = match slot_model_id(gcx.clone(), *slot).await {
                Ok(model) => model,
                Err(error) => {
                    rows.push(StageRun::not_run(&spec.id, &error));
                    continue;
                }
            };
            let label = if stage_slots.len() > 1 {
                format!("{}@{}", spec.id, slot.label())
            } else {
                spec.id.clone()
            };
            if index > 0 && spec.writes_allowed {
                continue;
            }
            let trace_chat_id = stage_trace_chat_id(review_id, &label);
            jobs.push(StageJob {
                spec: spec.clone(),
                label,
                subchat: stage_subchat_spec(&cfg.base_params, &cfg.settings, model),
                max_steps: overrides.max_steps.unwrap_or(cfg.settings.max_steps).max(1),
                prompt: build_stage_prompt(
                    &spec,
                    scope,
                    (spec.id == BROWSER_STAGE)
                        .then_some(browser_scenario)
                        .flatten(),
                    &[],
                ),
                trace_chat_id,
                abort: Arc::new(AtomicBool::new(false)),
            });
        }
    }
    StagePlan { jobs, rows }
}

fn apply_verdicts(
    findings: &mut [ReviewFinding],
    stage: &str,
    verdicts: &[crate::tools::review_agents::contract::Verdict],
) -> usize {
    let mut disputed = 0;
    for verdict in verdicts {
        if verdict.supported {
            continue;
        }
        if let Some(finding) = findings.iter_mut().find(|finding| finding.id == verdict.id) {
            finding.disputed = Some(Dispute {
                stage: stage.to_string(),
                reason: match verdict.reason.trim() {
                    "" => "the adversarial pass could not support this claim".to_string(),
                    reason => reason.to_string(),
                },
            });
            disputed += 1;
        }
    }
    disputed
}

fn stage_line(run: &StageRun) -> String {
    let status = match run.status {
        StageStatusKind::Ok => format!("ok {}", format_duration(run.duration_ms)),
        StageStatusKind::TimedOut => format!("timed out {}", format_duration(run.duration_ms)),
        StageStatusKind::Failed => format!("failed {}", format_duration(run.duration_ms)),
        StageStatusKind::NotRun => "not run".to_string(),
    };
    let mut line = format!("{} {status}", run.name);
    if let Some(reason) = run.reason.as_deref() {
        line.push_str(&format!(" ({})", markdown_cell(reason)));
    }
    if run.status != StageStatusKind::Ok {
        if let Some(trace) = run.trace_chat_id.as_deref() {
            line.push_str(&format!(" [transcript {}]", markdown_cell(trace)));
        }
    }
    line
}

fn coverage_line(run: &StageRun) -> Option<String> {
    let coverage = &run.coverage;
    let mut parts = Vec::new();
    if !coverage.files_read.is_empty() {
        parts.push(format!("{} file(s) read", coverage.files_read.len()));
    }
    if !coverage.commands_run.is_empty() {
        let failed = coverage
            .commands_run
            .iter()
            .filter(|command| command.exit != 0)
            .count();
        parts.push(format!(
            "{} command(s) run, {failed} non-zero",
            coverage.commands_run.len()
        ));
    }
    if !coverage.tools_unavailable.is_empty() {
        parts.push(format!(
            "unavailable: {}",
            markdown_cell(&coverage.tools_unavailable.join(", "))
        ));
    }
    if let Some(stopped) = coverage.stopped_early.as_deref() {
        parts.push(format!("stopped early: {}", markdown_cell(stopped)));
    }
    if let Some(summary) = run.summary.as_deref() {
        parts.push(markdown_cell(summary));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn render_finding(finding: &ReviewFinding) -> String {
    let mut line = format!(
        "\n- [{}] {}:{}-{} — {}",
        finding.id,
        markdown_cell(&finding.file),
        finding.line_start,
        finding.line_end,
        markdown_cell(&finding.claim),
    );
    let mut tags = vec![format!("stage: {}", finding.reported_by.join("+"))];
    if let Some(reproduction) = finding.reproduction.as_deref() {
        tags.push(format!("repro: {}", markdown_cell(reproduction)));
    }
    if !finding.evidence_present {
        tags.push("evidence not found in file".to_string());
    }
    if !finding.introduced_by_diff {
        tags.push("pre-existing".to_string());
    }
    if finding.out_of_scope {
        tags.push("out of scope".to_string());
    }
    if let Some(dispute) = finding.disputed.as_ref() {
        tags.push(format!("disputed: {}", markdown_cell(&dispute.reason)));
    }
    line.push_str(&format!("\n  - {}", tags.join(" · ")));
    if let Some(fix) = finding.fix.as_deref() {
        line.push_str(&format!("\n  - fix: {}", markdown_cell(fix)));
    }
    if !finding.locations.is_empty() {
        let extra = finding
            .locations
            .iter()
            .map(|location| {
                format!(
                    "{}:{}-{}",
                    location.file, location.line_start, location.line_end
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        line.push_str(&format!("\n  - also at: {}", markdown_cell(&extra)));
    }
    line
}

fn incomplete_stage_lines(report: &ReviewReport) -> String {
    report
        .incomplete_stages()
        .into_iter()
        .map(|run| {
            format!(
                "\n- {}: {} ({})",
                run.name,
                run.status.as_str(),
                markdown_cell(run.reason.as_deref().unwrap_or("no reason reported"))
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

pub fn render_review_markdown(report: &ReviewReport) -> String {
    let scope = &report.scope;
    let badge = match report.outcome {
        ReviewOutcome::Reviewed => "",
        ReviewOutcome::Partial => " ⚠️ PARTIAL",
        ReviewOutcome::Inconclusive => " ⚠️ INCONCLUSIVE",
    };
    let mut header = format!(
        "## Review{badge} · {} file(s) requested → {} reviewed ({}",
        scope.requested_files, scope.reviewed_files, scope.mode
    );
    if let Some(expansion) = scope.expansion.as_deref() {
        header.push_str(&format!(", {}", markdown_cell(expansion)));
    }
    header.push(')');
    match report.diff.base.as_deref() {
        Some(base) => header.push_str(&format!(
            " · base {}..{} ({} changed file(s), {} hunk(s))",
            markdown_cell(base),
            markdown_cell(report.diff.head.as_deref().unwrap_or("HEAD")),
            report.diff.changed_files,
            report.diff.hunks
        )),
        None => header.push_str(" · no git diff available"),
    }
    header.push_str(&format!(
        " · depth {} · {}",
        report.depth,
        format_duration(report.duration_ms)
    ));

    let mut output = header;
    output.push_str("\n\nStages: ");
    output.push_str(
        &report
            .stages
            .iter()
            .map(stage_line)
            .collect::<Vec<_>>()
            .join(" · "),
    );

    let facts = report.facts();
    let hypotheses = report.hypotheses();
    output.push_str(&format!(
        "\n\nFindings: {} supported ({} with a reproduction) · {} hypotheses · {} duplicate(s) merged · {} pre-existing · {} out of scope",
        facts.len(),
        report.reproduced(),
        hypotheses.len(),
        report.duplicates_merged,
        report.pre_existing(),
        scope.out_of_scope_findings,
    ));

    for severity in [
        ReviewSeverity::Blocker,
        ReviewSeverity::High,
        ReviewSeverity::Medium,
        ReviewSeverity::Low,
        ReviewSeverity::Note,
    ] {
        let bucket: Vec<&ReviewFinding> = facts
            .iter()
            .copied()
            .filter(|finding| finding.severity == severity)
            .collect();
        if bucket.is_empty() {
            continue;
        }
        output.push_str(&format!("\n\n### {} ({})", severity.as_str(), bucket.len()));
        for finding in bucket {
            output.push_str(&render_finding(finding));
        }
    }
    if facts.is_empty() {
        let completed = report.completed_stages();
        let incomplete = report.stages.len() - completed;
        match report.outcome {
            ReviewOutcome::Reviewed => output.push_str("\n\n### No supported findings"),
            ReviewOutcome::Inconclusive => {
                output.push_str(&format!(
                    "\n\n### Nothing was checked — this is NOT a pass\n\nEvery stage of this review ended without producing a result, so no part of the change was actually reviewed. Do not read the empty findings list as approval. Incomplete stage(s) ({incomplete}):{}",
                    incomplete_stage_lines(report)
                ));
            }
            ReviewOutcome::Partial => {
                output.push_str(&format!(
                    "\n\n### No supported findings from the {completed} stage(s) that completed\n\n{incomplete} stage(s) did not complete, so their part of the change was not reviewed:{}",
                    incomplete_stage_lines(report)
                ));
            }
        }
    } else if report.outcome != ReviewOutcome::Reviewed {
        let incomplete = report.stages.len() - report.completed_stages();
        output.push_str(&format!(
            "\n\n### Stages that did not complete ({incomplete}){}",
            incomplete_stage_lines(report)
        ));
    }

    if !hypotheses.is_empty() {
        output.push_str(&format!(
            "\n\n### Hypotheses ({}) — no reproduction and no verified evidence; you decide",
            hypotheses.len()
        ));
        for finding in hypotheses {
            output.push_str(&render_finding(finding));
        }
    }

    let coverage: Vec<String> = report
        .stages
        .iter()
        .filter_map(|run| coverage_line(run).map(|line| format!("\n- {}: {line}", run.name)))
        .collect();
    if !coverage.is_empty() {
        output.push_str("\n\n### Stage coverage");
        for line in coverage {
            output.push_str(&line);
        }
    }
    if !scope.dropped_files.is_empty() {
        output.push_str(&format!(
            "\n\n{} file(s) were not reviewed (max_files cap); the full list is in the machine-readable report.",
            scope.dropped_files.len()
        ));
    }
    if let Some(dir) = report.scratch_dir.as_deref() {
        output.push_str(&format!("\n\nRaw per-stage output: {dir}"));
    }
    output.push_str(
        "\n\nThe machine-readable ReviewReport is attached to this tool result's metadata.",
    );
    output
}

fn review_refs(report: &ReviewReport, roots: &[PathBuf]) -> (Value, bool) {
    let mut references = Vec::new();
    for finding in &report.findings {
        if references.len() == 32 {
            return (Value::Array(references), true);
        }
        let path = Path::new(&finding.file);
        let relative = if path.is_absolute() {
            roots.iter().find_map(|root| {
                path.strip_prefix(root)
                    .ok()
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
            })
        } else {
            Some(path.to_string_lossy().replace('\\', "/"))
        };
        let Some(relative) = relative else {
            continue;
        };
        references.push(json!({
            "id": finding.id,
            "severity": finding.severity.as_str(),
            "path": relative,
            "line1": finding.line_start,
            "line2": finding.line_end,
            "stage": finding.stage,
            "hypothesis": finding.is_hypothesis(),
        }));
    }
    (Value::Array(references), false)
}

fn review_agent_focus(args: &ReviewArgs) -> String {
    args.what_to_check
        .clone()
        .or_else(|| (!args.files.is_empty()).then(|| args.files.join(", ")))
        .unwrap_or_else(|| "current change".to_string())
}

fn review_agent_prompt(args: &ReviewArgs, depth: &str) -> String {
    let mut prompt = format!("Review the current change; focus: {}", review_agent_focus(args));
    if !args.files.is_empty() {
        prompt.push_str(&format!("; files: {}", args.files.join(", ")));
    }
    if let Some(base) = args.base.as_deref() {
        prompt.push_str(&format!("; base: {base}"));
    }
    prompt.push_str(&format!("; depth: {depth}"));
    prompt
}

fn review_agent_summary(report: &ReviewReport) -> String {
    let facts = report.facts();
    let blockers = facts
        .iter()
        .filter(|finding| finding.severity == ReviewSeverity::Blocker)
        .count();
    format!(
        "{} stages · {} findings ({} blocker) · {} hypotheses",
        report.stages.len(),
        facts.len(),
        blockers,
        report.hypotheses().len(),
    )
}

fn review_chat_thread(
    ctx: &StageCtx,
    child_chat_id: &str,
    title: String,
    model: String,
) -> ThreadParams {
    ThreadParams {
        id: child_chat_id.to_string(),
        title,
        model,
        task_meta: ctx.task_meta.clone(),
        worktree: ctx.worktree.clone(),
        parent_id: Some(ctx.chat_id.clone()),
        link_type: Some("tool".to_string()),
        root_chat_id: Some(ctx.root_chat_id.clone()),
        ..Default::default()
    }
}

async fn open_review_chat(
    app: &AppState,
    thread: &ThreadParams,
    request: &ChatMessage,
) -> Arc<AMutex<ChatSession>> {
    let session = ChatSession::new_with_trajectory(
        thread.id.clone(),
        vec![request.clone()],
        thread.clone(),
        chrono::Utc::now().to_rfc3339(),
        None,
        Vec::new(),
        None,
    );
    let session_arc = Arc::new(AMutex::new(session));
    {
        let mut sessions = app.chat.sessions.write().await;
        sessions.insert(thread.id.clone(), session_arc.clone());
    }
    session_arc
        .lock()
        .await
        .set_runtime_state(SessionState::Starting, None);
    session_arc
}

async fn save_review_chat(
    app: &AppState,
    thread: &ThreadParams,
    messages: &[ChatMessage],
    intent: TrajectoryCommitIntent,
) -> bool {
    if let Err(error) =
        save_trajectory_as_with_intent_checked(&app.chat_ctx(), thread, messages, intent).await
    {
        tracing::warn!("review: could not save the review chat trajectory: {error}");
        return false;
    }
    true
}

struct SteeringInbox {
    app: AppState,
    agent_id: Option<String>,
    session: Arc<AMutex<ChatSession>>,
}

impl SteeringInbox {
    async fn drain(&self, idle: bool) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        if let Some(agent_id) = self.agent_id.as_deref() {
            loop {
                let drained = if idle {
                    self.app.agents.finish_delivery_turn(agent_id).await
                } else {
                    self.app.agents.drain_deliveries(agent_id, false).await
                };
                match drained {
                    Ok(deliveries) => {
                        let sealed = !deliveries.iter().any(|delivery| delivery.wake);
                        for delivery in deliveries {
                            messages.extend(delivery.stamp_messages());
                        }
                        if !idle || sealed {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!("review: could not drain steering deliveries: {error}");
                        break;
                    }
                }
            }
            messages.extend(
                self.app
                    .agents
                    .drain_inbox(agent_id)
                    .await
                    .into_iter()
                    .map(|message| ChatMessage::new("user".to_string(), message.text)),
            );
        }
        let mut session = self.session.lock().await;
        for delivery in std::mem::take(&mut session.pending_deliveries) {
            if idle || delivery.push != PushMode::WhenIdle {
                messages.extend(delivery.stamp_messages());
            } else {
                session.pending_deliveries.push_back(delivery);
            }
        }
        for message in &mut messages {
            message.role = "user".to_string();
        }
        messages
    }

    async fn accept(
        &self,
        messages: Vec<ChatMessage>,
        agent: Option<&ToolAgentHandle>,
        done_count: u32,
    ) -> Vec<String> {
        let texts = steering_texts(&messages);
        for message in &messages {
            if let Some(agent) = agent {
                let preview: String = message
                    .content
                    .content_text_only()
                    .chars()
                    .take(60)
                    .collect();
                agent
                    .progress(format!("user steering: {preview}"), done_count)
                    .await;
            }
        }
        if !messages.is_empty() {
            let mut session = self.session.lock().await;
            let mut trace = session.messages.clone();
            trace.extend(messages);
            session.mirror_runner_messages(trace);
            drop(session);
            self.persist().await;
        }
        texts
    }

    async fn persist(&self) {
        let (thread, messages) = {
            let session = self.session.lock().await;
            (session.thread.clone(), session.messages.clone())
        };
        if save_review_chat(
            &self.app,
            &thread,
            &messages,
            TrajectoryCommitIntent::Required,
        )
        .await
        {
            if let Some(agent_id) = self.agent_id.as_deref() {
                let landed = messages
                    .iter()
                    .filter_map(refact_core::chat_types::delivery_id_of_message)
                    .collect::<std::collections::HashSet<_>>();
                for delivery in self.app.agents.pending_deliveries(agent_id).await {
                    if !landed.contains(delivery.id.as_str()) {
                        continue;
                    }
                    if let Err(error) = self
                        .app
                        .agents
                        .acknowledge_delivery(agent_id, &delivery.id)
                        .await
                    {
                        tracing::warn!(
                            "review: could not acknowledge a steering delivery: {error}"
                        );
                    }
                }
                crate::agents::delivery::publish_runner_queue(&self.app, agent_id).await;
            }
        }
    }
}

fn steering_texts(messages: &[ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.content.content_text_only().trim().to_string())
        .filter(|text| !text.is_empty())
        .collect()
}

async fn close_review_chat(app: &AppState, chat_id: &str) {
    let removed = {
        let mut sessions = app.chat.sessions.write().await;
        sessions.remove(chat_id)
    };
    if let Some(session_arc) = removed {
        let mut session = session_arc.lock().await;
        session.set_runtime_state(SessionState::Idle, None);
        session.close_event_channel();
        session.queue_notify.notify_waiters();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_review(
    gcx: Arc<GlobalContext>,
    tool: &ToolCtx,
    ctx: StageCtx,
    cfg: ReviewConfig,
    args: ReviewArgs,
    metering: &mut serde_json::Map<String, Value>,
    stage_executor: Option<Arc<dyn StageExecutor>>,
    progress: Option<ProgressSink>,
    agent: Option<Arc<ToolAgentHandle>>,
    steering: &SteeringInbox,
) -> Result<ReviewReport, String> {
    let review_cancelled = || {
        agent.as_ref().is_some_and(|agent| agent.aborted()) || ctx.abort_flag.load(Ordering::SeqCst)
    };
    let started = now_ms();
    let execution_scope = ctx.worktree.as_ref().map(ExecutionScope::from_worktree);
    let mut requested = Vec::new();
    for file in &args.files {
        if let Some(path) =
            resolve_gathered_file_path(gcx.clone(), execution_scope.as_ref(), file).await
        {
            if !requested.contains(&path) {
                requested.push(path);
            }
        }
    }
    let requested_count = requested.len();

    let scope = build_review_scope(
        gcx.clone(),
        ScopeRequest {
            requested,
            mode: args.scope_mode,
            base: args.base.clone(),
            focus: args.what_to_check.clone(),
            plan: args.plan.clone(),
            max_files: cfg.settings.max_files,
        },
    )
    .await;

    let depth = cfg.settings.depth_or_default(args.depth);
    let catalog = load_stage_catalog(gcx.clone()).await;
    let mut selection = select_stages(
        catalog.clone(),
        depth,
        args.stages.as_deref(),
        &scope.file_strings(),
    )?;
    apply_browser_scenario(
        &catalog,
        &mut selection.scheduled,
        &mut selection.skipped,
        args.browser_scenario.as_deref(),
    );

    let mut stage_rows: Vec<StageRun> = selection
        .skipped
        .iter()
        .map(|(id, reason)| StageRun::not_run(id, reason))
        .collect();

    let (post_merge, parallel): (Vec<StageSpec>, Vec<StageSpec>) = selection
        .scheduled
        .into_iter()
        .partition(|spec| spec.phase == StagePhase::PostMerge);

    let variants = args.variants.unwrap_or(cfg.settings.variants).clamp(1, 3);
    let plan = plan_jobs(
        gcx.clone(),
        &cfg,
        &ctx.review_id,
        &scope,
        parallel,
        variants,
        args.browser_scenario.as_deref(),
    )
    .await;
    stage_rows.extend(plan.rows);

    let schedule = ScheduleParams {
        parallel_depth: args
            .parallel_depth
            .unwrap_or(cfg.settings.parallel_depth)
            .max(1),
        idle_timeout: Duration::from_secs(cfg.settings.idle_timeout_secs),
        grace: HARVEST_GRACE,
    };

    let scratch = scratch_dir(
        tool.files
            .project_dirs()
            .await
            .first()
            .map(PathBuf::as_path),
        &ctx.review_id,
    );
    let executor: Arc<dyn StageExecutor> = match stage_executor {
        Some(executor) => executor,
        None => Arc::new(SubchatExecutor {
            gcx: gcx.clone(),
            ctx: ctx.clone(),
        }),
    };

    let mut findings: Vec<ReviewFinding> = Vec::new();
    let mut scratch_written = false;
    let mut done_count = 0;
    for product in run_stage_jobs(executor.clone(), plan.jobs, schedule, progress.clone()).await {
        done_count += 1;
        merge_metering(metering, product.metering);
        if let (Some(dir), Some(raw)) = (scratch.as_ref(), product.raw.as_ref()) {
            scratch_written |= write_scratch(
                dir,
                &product.run.name,
                &json!({"run": product.run, "raw": raw}),
            )
            .await;
        }
        findings.extend(product.findings);
        stage_rows.push(product.run);
    }

    verify_evidence(gcx.clone(), &scope, &mut findings).await;
    let attribution = DiffAttribution::new(&scope);
    for finding in &mut findings {
        finding.introduced_by_diff = attribution
            .introduced(&finding.file, finding.line_start, finding.line_end)
            .await;
        finding.out_of_scope = !scope.in_scope(&finding.file);
    }
    let (mut findings, duplicates_merged) = merge_findings(findings);
    rank_findings(&mut findings);

    let mut applied_steering: Vec<String> = Vec::new();
    for spec in post_merge {
        if review_cancelled() {
            stage_rows.push(StageRun::not_run(&spec.id, "review cancelled"));
            continue;
        }
        if findings.is_empty() {
            stage_rows.push(StageRun::not_run(&spec.id, "no findings to check"));
            continue;
        }
        let overrides = cfg.settings.stage_override(&spec.id);
        overrides.warn_if_deprecated(&spec.id);
        if overrides.enabled == Some(false) {
            stage_rows.push(StageRun::not_run(
                &spec.id,
                "disabled in review_agents.yaml",
            ));
            continue;
        }
        let slot = overrides.model_slot.unwrap_or(cfg.settings.model_slot);
        let model = match slot_model_id(gcx.clone(), slot).await {
            Ok(model) => model,
            Err(error) => {
                stage_rows.push(StageRun::not_run(&spec.id, &error));
                continue;
            }
        };
        if review_cancelled() {
            stage_rows.push(StageRun::not_run(&spec.id, "review cancelled"));
            continue;
        }
        let arrived = steering.drain(false).await;
        applied_steering.extend(steering.accept(arrived, agent.as_deref(), done_count).await);
        let prompt = build_adversarial_prompt(&spec, &scope, &findings, &applied_steering);
        let label = spec.id.clone();
        let job = StageJob {
            spec: Arc::new(spec),
            trace_chat_id: stage_trace_chat_id(&ctx.review_id, &label),
            label,
            subchat: stage_subchat_spec(&cfg.base_params, &cfg.settings, model),
            max_steps: overrides.max_steps.unwrap_or(cfg.settings.max_steps).max(1),
            prompt,
            abort: Arc::new(AtomicBool::new(false)),
        };
        for mut product in
            run_stage_jobs(executor.clone(), vec![job], schedule, progress.clone()).await
        {
            done_count += 1;
            merge_metering(metering, std::mem::take(&mut product.metering));
            if let (Some(dir), Some(raw)) = (scratch.as_ref(), product.raw.as_ref()) {
                scratch_written |= write_scratch(
                    dir,
                    &product.run.name,
                    &json!({"run": product.run, "raw": raw}),
                )
                .await;
            }
            let disputed = apply_verdicts(&mut findings, &product.run.name, &product.verdicts);
            let mut run = product.run;
            run.findings = disputed;
            stage_rows.push(run);
        }
    }

    let mut scope_summary = scope.summary();
    scope_summary.requested_files = requested_count;
    scope_summary.out_of_scope_findings = findings
        .iter()
        .filter(|finding| finding.out_of_scope)
        .count();

    let mut report = ReviewReport {
        depth: depth.as_str().to_string(),
        outcome: ReviewOutcome::Inconclusive,
        scope: scope_summary,
        diff: scope.diff_summary(),
        stages: stage_rows,
        findings,
        duration_ms: now_ms().saturating_sub(started),
        duplicates_merged,
        scratch_dir: scratch
            .filter(|_| scratch_written)
            .map(|dir| dir.to_string_lossy().to_string()),
    };
    report.outcome = report.derive_outcome();
    Ok(report)
}

struct ReviewRunInput {
    gcx: Arc<GlobalContext>,
    tool: ToolCtx,
    ctx: StageCtx,
    cfg: ReviewConfig,
    args: ReviewArgs,
    app: AppState,
    session: Arc<AMutex<ChatSession>>,
    child_chat_id: String,
    stage_executor: Option<Arc<dyn StageExecutor>>,
    background: bool,
}

struct ReviewRunOutcome {
    markdown: String,
    metering: serde_json::Map<String, Value>,
}

async fn run_review_tool(
    input: ReviewRunInput,
    agent: Option<(ToolAgentHandle, Arc<ToolAgentHandle>)>,
) -> Result<ReviewRunOutcome, String> {
    let ReviewRunInput {
        gcx,
        tool,
        ctx,
        cfg,
        args,
        app,
        session,
        child_chat_id,
        stage_executor,
        background,
    } = input;

    let (progress_sink, progress_task) = match agent.as_ref().map(|(_, shared)| shared.clone()) {
        Some(shared) => {
            let (sender, mut receiver) = mpsc::unbounded_channel::<StageProgress>();
            let sink: ProgressSink = Arc::new(move |progress| {
                let _ = sender.send(progress);
            });
            let task = tokio::spawn(async move {
                let mut done: u32 = 0;
                while let Some(progress) = receiver.recv().await {
                    done += 1;
                    shared
                        .progress(
                            format!("{}: {}", progress.label, progress.status.as_str()),
                            done,
                        )
                        .await;
                    shared.add_usage(progress.tokens, progress.cost_usd).await;
                }
            });
            (Some(sink), Some(task))
        }
        None => (None, None),
    };

    let ctx_abort = ctx.abort_flag.clone();
    let mut metering = serde_json::Map::new();
    let steering = SteeringInbox {
        app: app.clone(),
        agent_id: agent
            .as_ref()
            .map(|(_, shared)| shared.agent_id().to_string()),
        session: session.clone(),
    };
    let report = match run_review(
        gcx,
        &tool,
        ctx,
        cfg,
        args,
        &mut metering,
        stage_executor,
        progress_sink.clone(),
        agent.as_ref().map(|(_, shared)| shared.clone()),
        &steering,
    )
    .await
    {
        Ok(report) => report,
        Err(error) => {
            drop(progress_sink);
            if let Some(task) = progress_task {
                let _ = task.await;
            }
            if let Some((handle, _)) = agent {
                let _ = handle.fail(error.clone()).await;
            }
            close_review_chat(&app, &child_chat_id).await;
            return Err(error);
        }
    };
    drop(progress_sink);
    if let Some(task) = progress_task {
        let _ = task.await;
    }

    let cancelled = agent.as_ref().is_some_and(|(_, shared)| shared.aborted())
        || ctx_abort.load(Ordering::SeqCst);

    let late_steering = if cancelled {
        0
    } else {
        let arrived = steering.drain(true).await;
        let count = arrived.len();
        let done_count = report
            .stages
            .iter()
            .filter(|run| run.status != StageStatusKind::NotRun)
            .count() as u32;
        steering
            .accept(
                arrived,
                agent.as_ref().map(|(_, shared)| shared.as_ref()),
                done_count,
            )
            .await;
        count
    };

    let status = if cancelled { "cancelled" } else { "completed" };
    if let Some((_, shared)) = agent.as_ref() {
        for (key, value) in shared.extras(status, None) {
            metering.insert(key, value);
        }
    }

    let mut markdown = render_review_markdown(&report);
    if cancelled {
        markdown = markdown.replacen("## Review", "## Review — cancelled", 1);
    }
    if late_steering > 0 {
        markdown.push_str(&format!(
            "\n> Note: {late_steering} user message(s) arrived after the last stage and were not applied.\n"
        ));
    }

    {
        let mut session = session.lock().await;
        let mut messages = session.messages.clone();
        messages.push(ChatMessage::new("assistant".to_string(), markdown.clone()));
        session.mirror_runner_messages(messages);
    }
    steering.persist().await;
    close_review_chat(&app, &child_chat_id).await;

    match agent {
        Some((handle, _)) if cancelled => {
            let _ = handle.cancelled(Some("review cancelled".to_string())).await;
        }
        Some((handle, _)) if background => {
            let _ = handle.complete(markdown.clone()).await;
        }
        Some((handle, _)) => {
            let mut summary = review_agent_summary(&report);
            if late_steering > 0 {
                summary.push_str(&format!(
                    "\n> Note: {late_steering} user message(s) arrived after the last stage and were not applied.\n"
                ));
            }
            let _ = handle.complete(summary).await;
        }
        None => {}
    }

    metering.insert(
        "review_report".to_string(),
        serde_json::to_value(&report)
            .map_err(|error| format!("failed to serialize review report: {error}"))?,
    );
    let (refs, truncated) = review_refs(&report, &tool.files.project_dirs().await);
    metering.insert("review_refs".to_string(), refs);
    metering.insert("review_refs_truncated".to_string(), Value::Bool(truncated));

    Ok(ReviewRunOutcome { markdown, metering })
}

async fn push_review_completion(app: &AppState, agent_id: &str) {
    let Ok(record) = app.agents.get_any(agent_id).await else {
        return;
    };
    if let Err(error) =
        crate::agents::push::push_completion_to_parent(app.clone(), &record).await
    {
        tracing::warn!("review: could not push the completion into the parent chat: {error}");
    }
}

fn review_started_tool_result(
    tool_call_id: &str,
    focus: &str,
    extra: serde_json::Map<String, Value>,
) -> Vec<ContextEnum> {
    let field = |key: &str| {
        extra
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let child_chat_id = field("child_chat_id");
    let text = [
        format!("✓ Started background review: {focus}"),
        format!("- agent_id: {}", field("background_agent_id")),
        "- status: running".to_string(),
        format!("- child_chat_id: {child_chat_id}"),
        format!("- model: {}", field("model")),
        String::new(),
        format!("Open the child trajectory: [view](refact://chat/{child_chat_id})"),
        String::new(),
        "The completion will be pushed back into this chat automatically.".to_string(),
    ]
    .join("\n");
    vec![ContextEnum::ChatMessage(ChatMessage {
        role: "tool".to_string(),
        content: ChatContent::SimpleText(text),
        tool_call_id: tool_call_id.to_string(),
        preserve: Some(true),
        extra,
        output_filter: Some(OutputFilter::no_limits()),
        ..Default::default()
    })]
}

#[async_trait]
impl Tool for ToolCodeReview {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "review".to_string(),
            display_name: "Code Review".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Run a multi-stage agentic code review of the current change. Each stage is an agent that investigates with tools and reports findings with quoted evidence and, where it could get one, an executed reproduction. Findings the pipeline could verify are reported as facts; the rest are reported as hypotheses for you to triage. Every stage reports what it read, ran, and could not do. The machine-readable ReviewReport is attached to the tool result metadata. The review runs as a live background agent you can watch and cancel; its trajectory is a chat you can open and write to. Pass `wait: false` to return immediately and collect the report with `agent_wait` + `agent_result`.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "what_to_check": {
                        "type": "string",
                        "description": "Optional. Focus text: what to review and what to look for."
                    },
                    "files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional. Files to review. When given, scope_mode defaults to strict."
                    },
                    "base": {
                        "type": "string",
                        "description": "Optional. Git ref to diff against; the review uses the merge-base of this ref and HEAD. Defaults to the upstream or main branch."
                    },
                    "plan": {
                        "type": "string",
                        "description": "Optional. Plan or acceptance criteria the change must satisfy; drives the spec stage."
                    },
                    "scope_mode": {
                        "type": "string",
                        "enum": ["strict", "adjacent", "broad"],
                        "description": "Optional. strict reports only on the requested and changed files, adjacent adds one dependency hop, broad allows anything. Defaults to strict when files or base are given, otherwise broad."
                    },
                    "stages": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional. Explicit stage list, overrides depth. Unknown names are rejected with the catalog."
                    },
                    "browser": {
                        "type": "boolean",
                        "description": "Optional. Run the browser stage. Requires browser_scenario; the browser stage never runs without one."
                    },
                    "browser_scenario": {
                        "type": "string",
                        "description": "What the browser stage must drive: the URL or route, the states to reach, the interactions to perform, and what must be true afterwards. Supplying it enables the browser stage at any depth."
                    },
                    "depth": {
                        "type": "string",
                        "enum": ["normal", "deep"],
                        "description": "Optional. normal runs the cheap stages; deep adds tests, execution, browser and the adversarial pass."
                    },
                    "parallel_depth": {
                        "type": "integer",
                        "description": "Optional. Stage agents running concurrently. Defaults to 4; use 1-2 on a laptop."
                    },
                    "variants": {
                        "type": "integer",
                        "description": "Optional. Model variants per stage, 1-3. Defaults to 1."
                    },
                    "wait": {
                        "type": "boolean",
                        "default": true,
                        "description": "Block until the review finishes and return the report inline (default). Set false to return immediately with a live background-agent card; then use agent_wait and agent_result to collect the report."
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let args = parse_args(args)?;
        let (gcx, tool, ctx, app, child_chat_id) = {
            let ccx_lock = ccx.lock().await;
            let gcx = ccx_lock.app.gcx.clone();
            let tool = ccx_lock.tool.clone();
            let app = ccx_lock.app.clone();
            let review_id = format!("rv-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
            let child_chat_id = format!("subchat-{review_id}");
            let ctx = StageCtx {
                review_id,
                tool_call_id: tool_call_id.clone(),
                subchat_tx: ccx_lock.subchat_tx.clone(),
                abort_flag: ccx_lock.abort_flag.clone(),
                depth: ccx_lock.subchat_depth,
                task_meta: ccx_lock.task_meta.clone(),
                worktree: ccx_lock.execution_scope_worktree(),
                chat_id: ccx_lock.chat_id.clone(),
                trace_parent_chat_id: child_chat_id.clone(),
                root_chat_id: ccx_lock.root_chat_id.clone(),
                activity: None,
            };
            (gcx, tool, ctx, app, child_chat_id)
        };

        let cfg = load_review_config(gcx.clone()).await?;
        let guardrails = cfg.guardrails_prompt.clone();
        let depth = cfg.settings.depth_or_default(args.depth);
        let focus = review_agent_focus(&args);
        let model = slot_model_id(gcx.clone(), cfg.settings.model_slot)
            .await
            .unwrap_or_default();
        let review_thread = review_chat_thread(
            &ctx,
            &child_chat_id,
            format!("Review: {focus}"),
            model.clone(),
        );
        let review_request = ChatMessage::new(
            "user".to_string(),
            review_agent_prompt(&args, depth.as_str()),
        );
        let review_session = open_review_chat(&app, &review_thread, &review_request).await;
        save_review_chat(
            &app,
            &review_thread,
            std::slice::from_ref(&review_request),
            TrajectoryCommitIntent::Checkpoint,
        )
        .await;

        let agent = match ToolAgentHandle::begin(
            &app,
            &ctx.chat_id,
            &ctx.root_chat_id,
            ToolAgentSpec {
                tool_name: "review".to_string(),
                title: format!("Review: {focus}"),
                prompt: review_agent_prompt(&args, depth.as_str()),
                model: model.clone(),
                model_type: None,
                target_files: args.files.clone(),
                child_chat_id: child_chat_id.clone(),
                parent_tool_call_id: tool_call_id.clone(),
            },
        )
        .await
        {
            Ok(handle) => {
                let shared = Arc::new(handle.clone());
                Some((handle, shared))
            }
            Err(error) => {
                tracing::warn!("review: tool agent unavailable, continuing without it: {error}");
                None
            }
        };

        let background = !args.wait && agent.is_some();
        let input = ReviewRunInput {
            gcx,
            tool,
            ctx,
            cfg,
            args,
            app: app.clone(),
            session: review_session,
            child_chat_id,
            stage_executor: self.stage_executor_override.clone(),
            background,
        };

        if background {
            let agent_id = agent
                .as_ref()
                .map(|(handle, _)| handle.agent_id().to_string())
                .unwrap_or_default();
            let mut extra = agent
                .as_ref()
                .map(|(_, shared)| shared.extras("running", None))
                .unwrap_or_default();
            extra.insert("review_started".to_string(), Value::Bool(true));
            tokio::spawn(async move {
                if let Err(error) = run_review_tool(input, agent).await {
                    tracing::warn!("review: background run failed: {error}");
                }
                push_review_completion(&app, &agent_id).await;
            });
            return Ok((
                false,
                review_started_tool_result(tool_call_id, &focus, extra),
            ));
        }

        let outcome = run_review_tool(input, agent).await?;
        Ok((
            false,
            vec![
                ContextEnum::ChatMessage(ChatMessage {
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(outcome.markdown),
                    tool_calls: None,
                    tool_call_id: tool_call_id.clone(),
                    usage: None,
                    preserve: Some(true),
                    extra: outcome.metering,
                    output_filter: Some(OutputFilter::no_limits()),
                    ..Default::default()
                }),
                ContextEnum::ChatMessage(ChatMessage {
                    role: "cd_instruction".to_string(),
                    content: ChatContent::SimpleText(guardrails),
                    ..Default::default()
                }),
            ],
        ))
    }

    fn tool_depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    use crate::agents::registry::InboxMessage;
    use crate::agents::types::{BgAgentKind, BgAgentStatus};
    use crate::caps::caps::{BaseModelRecord, ChatModelRecord, CodeAssistantCaps};
    use crate::chat::types::{PendingDelivery, PushMode};
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
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
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
        let loaded =
            crate::chat::trajectories::load_trajectory_for_chat(&app.chat_ctx(), &child_chat_id)
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
        let loaded =
            crate::chat::trajectories::load_trajectory_for_chat(&app.chat_ctx(), &child_chat_id)
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
        let loaded =
            crate::chat::trajectories::load_trajectory_for_chat(&app.chat_ctx(), &child_chat_id)
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
        let loaded = crate::chat::trajectories::load_trajectory_for_chat(
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
            let loaded = crate::chat::trajectories::load_trajectory_for_chat(
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
        let loaded =
            crate::chat::trajectories::load_trajectory_for_chat(&app.chat_ctx(), &thread.id)
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
