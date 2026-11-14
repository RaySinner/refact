use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::tool_agent::ToolAgentHandle;
use refact_agents_core::tool_agent::ToolAgentSpec;
use refact_runtime_api::AppState;
use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_runtime_api::trajectory_runtime::save_trajectory_as_with_intent_checked;
use refact_chat_api::types::{ChatSession, PushMode, SessionState, ThreadParams, TrajectoryCommitIntent};
use refact_runtime_api::global_context::GlobalContext;
use refact_postprocessing::pp_command_output::OutputFilter;
use refact_runtime_api::tool_ctx::ToolCtx;
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
use refact_tool_api::review_merge::{merge_findings, rank_findings};
use crate::tools::review_scope::{build_review_scope, DiffAttribution, ReviewScope, ScopeRequest};
use refact_tool_api::review_types::{
    Dispute, ReviewDepth, ReviewFinding, ReviewOutcome, ReviewReport, ReviewSeverity, ScopeMode,
    StageRun, StageStatusKind,
};
use crate::tools::subagent_phases::resolve_gathered_file_path;
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};
use refact_worktrees::scope::ExecutionScope;

pub struct ToolCodeReview {
    pub config_path: String,
    pub stage_executor_override: Option<Arc<dyn StageExecutor>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewArgs {
    pub what_to_check: Option<String>,
    pub files: Vec<String>,
    pub base: Option<String>,
    pub plan: Option<String>,
    pub scope_mode: ScopeMode,
    pub stages: Option<Vec<String>>,
    pub depth: Option<ReviewDepth>,
    pub parallel_depth: Option<usize>,
    pub variants: Option<usize>,
    pub browser_scenario: Option<String>,
    pub wait: bool,
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

pub fn parse_args(args: &HashMap<String, Value>) -> Result<ReviewArgs, String> {
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

pub fn format_duration(duration_ms: u64) -> String {
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

pub fn scratch_dir(gcx_project: Option<&Path>, review_id: &str) -> Option<PathBuf> {
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

pub fn apply_browser_scenario(
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

pub struct StagePlan {
    pub jobs: Vec<StageJob>,
    pub rows: Vec<StageRun>,
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

pub fn apply_verdicts(
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

pub fn review_refs(report: &ReviewReport, roots: &[PathBuf]) -> (Value, bool) {
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

pub async fn open_review_chat(
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

pub struct SteeringInbox {
    pub app: AppState,
    pub agent_id: Option<String>,
    pub session: Arc<AMutex<ChatSession>>,
}

impl SteeringInbox {
    pub async fn drain(&self, idle: bool) -> Vec<ChatMessage> {
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

    pub async fn accept(
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

    pub async fn persist(&self) {
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
                refact_agents_core::delivery::publish_runner_queue(&self.app, agent_id).await;
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

pub async fn close_review_chat(app: &AppState, chat_id: &str) {
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

pub struct ReviewRunInput {
    pub gcx: Arc<GlobalContext>,
    pub tool: ToolCtx,
    pub ctx: StageCtx,
    pub cfg: ReviewConfig,
    pub args: ReviewArgs,
    pub app: AppState,
    pub session: Arc<AMutex<ChatSession>>,
    pub child_chat_id: String,
    pub stage_executor: Option<Arc<dyn StageExecutor>>,
    pub background: bool,
}

pub struct ReviewRunOutcome {
    pub markdown: String,
    pub metering: serde_json::Map<String, Value>,
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
        refact_agents_core::push::push_completion_to_parent(app.clone(), &record).await
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
