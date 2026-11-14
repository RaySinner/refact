pub mod config;
pub mod contract;
pub mod prompts;
pub mod runner;
pub mod stages;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use tokio::sync::Semaphore;

use serde_json::Value;

use refact_runtime_api::global_context::GlobalContext;
use crate::tools::review_agents::runner::{
    monitor_ctx, now_ms, run_stage, ProgressSink, StageCtx, StageJob, StageProduct, StageProgress,
};
use refact_tool_api::review_types::StageRun;

pub const WATCHDOG_POLL: Duration = Duration::from_secs(5);
pub const MIN_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
pub const HARVEST_GRACE: Duration = Duration::from_secs(10);

pub type BoxedProduct = Pin<Box<dyn Future<Output = StageProduct> + Send>>;

#[async_trait]
pub trait StageExecutor: Send + Sync {
    async fn execute(&self, job: StageJob, activity: Arc<AtomicU64>) -> StageProduct;

    fn abort_flag(&self) -> Option<Arc<AtomicBool>> {
        None
    }
}

pub struct SubchatExecutor {
    pub gcx: Arc<GlobalContext>,
    pub ctx: StageCtx,
}

#[async_trait]
impl StageExecutor for SubchatExecutor {
    async fn execute(&self, job: StageJob, activity: Arc<AtomicU64>) -> StageProduct {
        let (ctx, forwarder) = monitor_ctx(&self.ctx, activity);
        let product = run_stage(self.gcx.clone(), ctx, job).await;
        forwarder.abort();
        product
    }

    fn abort_flag(&self) -> Option<Arc<AtomicBool>> {
        Some(self.ctx.abort_flag.clone())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScheduleParams {
    pub parallel_depth: usize,
    pub idle_timeout: Duration,
    pub grace: Duration,
}

impl ScheduleParams {
    pub fn idle_limit(&self) -> Duration {
        self.idle_timeout.max(MIN_IDLE_TIMEOUT)
    }
}

fn stage_product(run: StageRun) -> StageProduct {
    StageProduct {
        run,
        findings: vec![],
        verdicts: vec![],
        metering: serde_json::Map::new(),
        raw: None,
    }
}

fn idle_timed_out(label: &str, trace: &str, idle_ms: u64, started: u64) -> StageProduct {
    stage_product(
        StageRun::timed_out(
            label,
            None,
            now_ms().saturating_sub(started),
            &format!(
                "no activity for {}s; the stage stopped responding",
                idle_ms / 1000
            ),
        )
        .with_trace_chat_id(Some(trace.to_string())),
    )
}

fn cancelled_before_start(label: &str) -> StageProduct {
    stage_product(StageRun::not_run(
        label,
        "review cancelled before the stage started",
    ))
}

pub fn panicked(label: &str, trace: &str, started: u64) -> StageProduct {
    stage_product(
        StageRun::failed(
            label,
            None,
            now_ms().saturating_sub(started),
            "stage panicked",
        )
        .with_trace_chat_id(Some(trace.to_string())),
    )
}

pub fn salvaged(mut product: StageProduct, kill: StageRun) -> StageProduct {
    product.run.status = kill.status;
    product.run.reason = match (kill.reason, product.run.reason.take()) {
        (Some(kill_reason), Some(original)) if original != kill_reason => {
            Some(format!("{kill_reason}; {original}"))
        }
        (kill_reason, original) => kill_reason.or(original),
    };
    product.run.summary = Some(match product.run.summary.take() {
        Some(summary) => format!("{summary}; salvaged from partial run"),
        None => "salvaged from partial run".to_string(),
    });
    product
}

#[allow(clippy::too_many_arguments)]
pub async fn watch_stage(
    label: String,
    trace: String,
    idle_limit: Duration,
    poll: Duration,
    grace: Duration,
    activity: Arc<AtomicU64>,
    abort: Arc<AtomicBool>,
    parent_abort: Option<Arc<AtomicBool>>,
    work: BoxedProduct,
) -> StageProduct {
    let started = now_ms();
    activity.store(started, Ordering::Relaxed);
    let idle_limit_ms = idle_limit.as_millis() as u64;
    let poll = poll.max(Duration::from_millis(1));
    tokio::pin!(work);
    let fallback = loop {
        tokio::select! {
            product = &mut work => {
                abort.store(true, Ordering::SeqCst);
                return product;
            }
            _ = tokio::time::sleep(poll) => {
                if parent_abort
                    .as_ref()
                    .is_some_and(|parent| parent.load(Ordering::SeqCst))
                {
                    abort.store(true, Ordering::SeqCst);
                }
                let idle_ms = now_ms().saturating_sub(activity.load(Ordering::Relaxed));
                if idle_ms > idle_limit_ms {
                    break idle_timed_out(&label, &trace, idle_ms, started);
                }
            }
        }
    };
    abort.store(true, Ordering::SeqCst);
    match tokio::time::timeout(grace.max(Duration::from_millis(1)), &mut work).await {
        Ok(product) => salvaged(product, fallback.run),
        Err(_) => fallback,
    }
}

fn stage_progress(product: &StageProduct) -> StageProgress {
    let metering_number = |key: &str| {
        product
            .metering
            .get(key)
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    StageProgress {
        label: product.run.name.clone(),
        status: product.run.status,
        duration_ms: product.run.duration_ms,
        tokens: (metering_number("tokens_in") + metering_number("tokens_out")) as u64,
        cost_usd: product.metering.get("cost_usd").and_then(Value::as_f64),
    }
}

pub async fn run_stage_jobs(
    executor: Arc<dyn StageExecutor>,
    jobs: Vec<StageJob>,
    params: ScheduleParams,
    progress: Option<ProgressSink>,
) -> Vec<StageProduct> {
    let semaphore = Arc::new(Semaphore::new(params.parallel_depth.max(1)));
    let idle_limit = params.idle_limit();
    let poll = WATCHDOG_POLL
        .min(idle_limit / 2)
        .max(Duration::from_millis(1));
    let parent_abort = executor.abort_flag();
    let futures = jobs.into_iter().map(|job| {
        let executor = executor.clone();
        let semaphore = semaphore.clone();
        let parent_abort = parent_abort.clone();
        let progress = progress.clone();
        async move {
            let label = job.label.clone();
            let trace = job.trace_chat_id.clone();
            let abort = job.abort.clone();
            let Ok(_permit) = semaphore.acquire_owned().await else {
                abort.store(true, Ordering::SeqCst);
                return cancelled_before_start(&label);
            };
            if parent_abort
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                abort.store(true, Ordering::SeqCst);
                return cancelled_before_start(&label);
            }
            let activity = Arc::new(AtomicU64::new(now_ms()));
            let panic_label = label.clone();
            let panic_trace = trace.clone();
            let stamp = activity.clone();
            let started = now_ms();
            let work: BoxedProduct = Box::pin(async move {
                match std::panic::AssertUnwindSafe(executor.execute(job, stamp))
                    .catch_unwind()
                    .await
                {
                    Ok(product) => product,
                    Err(_) => panicked(&panic_label, &panic_trace, started),
                }
            });
            let product = watch_stage(
                label,
                trace,
                idle_limit,
                poll,
                params.grace,
                activity,
                abort,
                parent_abort,
                work,
            )
            .await;
            if let Some(sink) = progress.as_ref() {
                sink(stage_progress(&product));
            }
            product
        }
    });
    futures::future::join_all(futures).await
}
