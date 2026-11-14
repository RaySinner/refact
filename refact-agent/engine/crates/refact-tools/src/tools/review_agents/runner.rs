use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex as AMutex;

use refact_core::chat_types::{ChatContent, ChatMessage};
use refact_runtime_api::GlobalContextCtx;
use refact_chat_api::TaskMeta;
use refact_runtime_api::global_context::GlobalContext;
use refact_chat_api::{ExplicitSubchatSpec, TraceParent};
use crate::tools::review_agents::contract::{
    parse_stage_output, parse_verdict_output, StageOutput, Verdict, VerdictOutput, REPAIR_PROMPT,
};
use crate::tools::review_agents::prompts::REVIEW_SYSTEM_PROMPT;
use crate::tools::review_agents::stages::{StageContract, StageSpec};
use refact_tool_api::review_types::{ReviewFinding, StageRun, StageStatusKind};
use crate::tools::subagent_phases::get_last_assistant_content;
use refact_worktrees::types::WorktreeMeta;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn merge_metering(
    target: &mut serde_json::Map<String, Value>,
    source: serde_json::Map<String, Value>,
) {
    for (key, value) in source {
        match (target.get_mut(&key), value) {
            (Some(Value::Number(existing)), Value::Number(incoming)) => {
                let sum = existing.as_f64().unwrap_or(0.0) + incoming.as_f64().unwrap_or(0.0);
                *existing = serde_json::Number::from_f64(sum)
                    .unwrap_or_else(|| serde_json::Number::from(0));
            }
            (Some(slot), incoming) => {
                *slot = incoming;
            }
            (None, incoming) => {
                target.insert(key, incoming);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct StageProgress {
    pub label: String,
    pub status: StageStatusKind,
    pub duration_ms: u64,
    pub tokens: u64,
    pub cost_usd: Option<f64>,
}

pub type ProgressSink = Arc<dyn Fn(StageProgress) + Send + Sync>;

pub fn usage_from_messages(messages: &[ChatMessage]) -> (u64, u64, Option<f64>) {
    let mut tokens_in = 0u64;
    let mut tokens_out = 0u64;
    let mut cost_usd = None;
    for message in messages {
        if message.role != "assistant" {
            continue;
        }
        let Some(usage) = message.usage.as_ref() else {
            continue;
        };
        tokens_in += usage.prompt_tokens as u64;
        tokens_out += usage.completion_tokens as u64;
        if let Some(metering) = usage.metering_usd.as_ref() {
            cost_usd = Some(cost_usd.unwrap_or(0.0) + metering.total_usd);
        }
    }
    (tokens_in, tokens_out, cost_usd)
}

#[derive(Clone)]
pub struct StageCtx {
    pub review_id: String,
    pub tool_call_id: String,
    pub subchat_tx: Arc<AMutex<UnboundedSender<Value>>>,
    pub abort_flag: Arc<AtomicBool>,
    pub depth: usize,
    pub task_meta: Option<TaskMeta>,
    pub worktree: Option<WorktreeMeta>,
    pub chat_id: String,
    /// Trace owner for the stage subchats. For a review this is the review
    /// chat (`subchat-<review_id>`), so every stage nests under it.
    pub trace_parent_chat_id: String,
    pub root_chat_id: String,
    pub activity: Option<Arc<AtomicU64>>,
}

pub struct StageJob {
    pub spec: Arc<StageSpec>,
    pub label: String,
    pub subchat: ExplicitSubchatSpec,
    pub max_steps: usize,
    pub prompt: String,
    pub trace_chat_id: String,
    pub abort: Arc<AtomicBool>,
}

pub fn stage_trace_chat_id(review_id: &str, label: &str) -> String {
    let slug: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("subchat-{review_id}-{slug}")
}

pub fn monitor_ctx(
    ctx: &StageCtx,
    activity: Arc<AtomicU64>,
) -> (StageCtx, tokio::task::JoinHandle<()>) {
    let (monitor_tx, mut monitor_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let parent_tx = ctx.subchat_tx.clone();
    let stamp = activity.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(message) = monitor_rx.recv().await {
            activity.store(now_ms(), Ordering::Relaxed);
            let sender = parent_tx.lock().await;
            let _ = sender.send(message);
        }
    });
    let mut monitored = ctx.clone();
    monitored.subchat_tx = Arc::new(AMutex::new(monitor_tx));
    monitored.activity = Some(stamp);
    (monitored, forwarder)
}

pub struct StageProduct {
    pub run: StageRun,
    pub findings: Vec<ReviewFinding>,
    pub verdicts: Vec<Verdict>,
    pub metering: serde_json::Map<String, Value>,
    pub raw: Option<String>,
}

impl StageProduct {
    fn empty(run: StageRun) -> Self {
        Self {
            run,
            findings: vec![],
            verdicts: vec![],
            metering: serde_json::Map::new(),
            raw: None,
        }
    }
}

pub fn inherited_worktree(ctx: &StageCtx) -> Option<WorktreeMeta> {
    ctx.worktree.clone()
}

pub async fn run_stage(gcx: Arc<GlobalContext>, ctx: StageCtx, job: StageJob) -> StageProduct {
    let trace_chat_id = job.trace_chat_id.clone();
    let mut product = run_stage_inner(gcx, ctx, job).await;
    product.run = product.run.with_trace_chat_id(Some(trace_chat_id));
    product
}

/// Build the subchat config for one stage. The stage subchat is attributed to
/// `ctx.trace_parent_chat_id` (the review chat), so every stage nests under the
/// review's own trajectory instead of the calling chat.
pub async fn stage_subchat_config(
    gcx: Arc<GlobalContext>,
    ctx: &StageCtx,
    job: &StageJob,
) -> Result<refact_chat_api::SubchatConfig, String> {
    let launcher = gcx.chat_ctx(gcx.clone()).subchat.clone();
    let attribution = format!("review_{}", job.spec.id);
    launcher
        .resolve_subchat_config_with_explicit_params(
            &attribution,
            &job.subchat,
            true,
            Some(job.trace_chat_id.clone()),
            Some(job.spec.display_title()),
            Some(ctx.trace_parent_chat_id.clone()),
            Some("review_stage".to_string()),
            Some(ctx.root_chat_id.clone()),
            Some(job.spec.tools()),
            job.max_steps,
            false,
            "agent".to_string(),
            ctx.task_meta.clone(),
            inherited_worktree(ctx),
            Some(ctx.tool_call_id.clone()),
            Some(ctx.subchat_tx.clone()),
            Some(job.abort.clone()),
            ctx.depth + 1,
        )
        .await
}

async fn run_stage_inner(gcx: Arc<GlobalContext>, ctx: StageCtx, job: StageJob) -> StageProduct {
    let started = now_ms();
    let launcher = gcx.chat_ctx(gcx.clone()).subchat.clone();
    let model = job.subchat.model.clone();
    let label = job.label.clone();
    let mut metering = serde_json::Map::new();
    let attribution = format!("review_{}", job.spec.id);

    let mut config = match stage_subchat_config(gcx.clone(), &ctx, &job).await {
        Ok(config) => config,
        Err(error) => {
            return StageProduct::empty(StageRun::failed(
                &label,
                Some(model),
                now_ms().saturating_sub(started),
                &error,
            ));
        }
    };
    config.soft_abort = true;
    config.activity_stamp = ctx.activity.clone();

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: ChatContent::SimpleText(REVIEW_SYSTEM_PROMPT.to_string()),
            ..Default::default()
        },
        ChatMessage::new("user".to_string(), job.prompt.clone()),
    ];

    let result = match launcher.run_subchat(messages, config).await {
        Ok(result) => result,
        Err(error) => {
            return StageProduct::empty(StageRun::failed(
                &label,
                Some(model),
                now_ms().saturating_sub(started),
                &error,
            ));
        }
    };
    merge_metering(&mut metering, result.metering.clone());
    let (tokens_in, tokens_out, cost_usd) = usage_from_messages(&result.messages);
    metering.insert("tokens_in".to_string(), Value::from(tokens_in));
    metering.insert("tokens_out".to_string(), Value::from(tokens_out));
    if let Some(cost_usd) = cost_usd {
        metering.insert("cost_usd".to_string(), Value::from(cost_usd));
    }
    let text = get_last_assistant_content(&result.messages);

    let parsed = match job.spec.contract {
        StageContract::Findings => parse_stage_output(&text)
            .map(Parsed::Findings)
            .map_err(|error| error.to_string()),
        StageContract::Verdicts => parse_verdict_output(&text)
            .map(Parsed::Verdicts)
            .map_err(|error| error.to_string()),
    };

    let parsed = match parsed {
        Ok(parsed) => Ok(parsed),
        Err(first_error) if job.abort.load(Ordering::SeqCst) => {
            Err(format!("output_contract: {first_error}"))
        }
        Err(first_error) => {
            tracing::info!("review: {label} broke the output contract ({first_error}), repairing");
            let mut retry_messages = result.messages.clone();
            retry_messages.push(ChatMessage::new(
                "user".to_string(),
                format!("{REPAIR_PROMPT} {first_error}"),
            ));
            match launcher
                .run_subchat_once_with_explicit_params(
                    &attribution,
                &job.subchat,
                retry_messages,
                ctx.tool_call_id.clone(),
                ctx.subchat_tx.clone(),
                job.abort.clone(),
                ctx.depth,
                ctx.task_meta.clone(),
                ctx.worktree.clone(),
                TraceParent::rooted(&ctx.trace_parent_chat_id, &ctx.root_chat_id),
            )
            .await
            {
                Ok(retry) => {
                    merge_metering(&mut metering, retry.metering.clone());
                    let retry_text = get_last_assistant_content(&retry.messages);
                    let repaired = match job.spec.contract {
                        StageContract::Findings => parse_stage_output(&retry_text)
                            .map(Parsed::Findings)
                            .map_err(|error| error.to_string()),
                        StageContract::Verdicts => parse_verdict_output(&retry_text)
                            .map(Parsed::Verdicts)
                            .map_err(|error| error.to_string()),
                    };
                    repaired.map_err(|error| format!("output_contract: {error}"))
                }
                Err(retry_error) => Err(format!("output_contract: repair failed: {retry_error}")),
            }
        }
    };

    let duration = now_ms().saturating_sub(started);
    match parsed {
        Ok(Parsed::Findings(output)) => {
            let mut product = findings_product(&label, &job, output, model, duration);
            merge_metering(&mut product.metering, metering);
            product.raw = Some(text);
            product
        }
        Ok(Parsed::Verdicts(output)) => {
            let mut product = verdicts_product(&label, output, model, duration);
            merge_metering(&mut product.metering, metering);
            product.raw = Some(text);
            product
        }
        Err(reason) => {
            let mut product =
                StageProduct::empty(StageRun::failed(&label, Some(model), duration, &reason));
            product.metering = metering;
            product.raw = Some(text);
            product
        }
    }
}

pub enum Parsed {
    Findings(StageOutput),
    Verdicts(VerdictOutput),
}

pub fn findings_product(
    label: &str,
    job: &StageJob,
    output: StageOutput,
    model: String,
    duration_ms: u64,
) -> StageProduct {
    let findings: Vec<ReviewFinding> = output
        .findings
        .into_iter()
        .map(|raw| ReviewFinding {
            id: String::new(),
            stage: job.spec.id.clone(),
            model: Some(model.clone()),
            title: raw.title,
            severity: raw.severity,
            file: raw.file,
            line_start: raw.line_start,
            line_end: raw.line_end,
            claim: raw.claim,
            evidence: raw.evidence,
            evidence_present: false,
            reproduction: raw.reproduction,
            fix: raw.fix,
            introduced_by_diff: false,
            out_of_scope: false,
            reported_by: vec![job.spec.id.clone()],
            locations: vec![],
            disputed: None,
        })
        .collect();

    let mut run = StageRun::ok(label, Some(model), duration_ms);
    run.findings = findings.len();
    run.coverage = output.coverage;
    run.summary = summary_with_rejections(output.summary, &output.rejected);
    StageProduct {
        run,
        findings,
        verdicts: vec![],
        metering: serde_json::Map::new(),
        raw: None,
    }
}

fn verdicts_product(
    label: &str,
    output: VerdictOutput,
    model: String,
    duration_ms: u64,
) -> StageProduct {
    let mut run = StageRun::ok(label, Some(model), duration_ms);
    run.findings = output.verdicts.iter().filter(|v| !v.supported).count();
    run.coverage = output.coverage;
    run.summary = summary_with_rejections(output.summary, &output.rejected);
    StageProduct {
        run,
        findings: vec![],
        verdicts: output.verdicts,
        metering: serde_json::Map::new(),
        raw: None,
    }
}

pub fn summary_with_rejections(summary: String, rejected: &[String]) -> Option<String> {
    let summary = summary.trim().to_string();
    match (summary.is_empty(), rejected.is_empty()) {
        (true, true) => None,
        (false, true) => Some(summary),
        (true, false) => Some(format!("{} malformed entries skipped", rejected.len())),
        (false, false) => Some(format!(
            "{summary} ({} malformed entries skipped)",
            rejected.len()
        )),
    }
}
