pub use refact_tools::tools::review_agents::*;

pub mod config;
pub mod contract;
pub mod prompts;
pub mod runner;
pub mod stages;

#[cfg(test)]
#[allow(unused_imports)]
use std::future::Future;
#[cfg(test)]
#[allow(unused_imports)]
use std::pin::Pin;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use futures::FutureExt;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Semaphore;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_types::StageRun;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use refact_core::chat_types::{ChatModelType};
    use refact_chat_api::{SubchatParameters};
    use crate::llm::params::CacheControl;
    use refact_chat_api::ExplicitSubchatSpec;
    use crate::tools::review_agents::runner::stage_trace_chat_id;
    use crate::tools::review_agents::stages::embedded_catalog;
    use crate::tools::review_types::StageStatusKind;

    fn spec_for(stage_id: &str) -> Arc<crate::tools::review_agents::stages::StageSpec> {
        Arc::new(
            embedded_catalog()
                .into_iter()
                .find(|spec| spec.id == stage_id)
                .expect("stage id is present in the embedded catalog"),
        )
    }

    fn job(label: &str, stage_id: &str) -> StageJob {
        StageJob {
            spec: spec_for(stage_id),
            label: label.to_string(),
            subchat: ExplicitSubchatSpec {
                params: SubchatParameters {
                    subchat_model_type: ChatModelType::Default,
                    subchat_model: "m".to_string(),
                    subchat_n_ctx: 100000,
                    subchat_max_new_tokens: 8000,
                    subchat_temperature: None,
                    subchat_tokens_for_rag: 0,
                    subchat_reasoning_effort: None,
                    subchat_cache_control: CacheControl::Off,
                },
                model: "m".to_string(),
                autonomous_no_confirm: true,
            },
            max_steps: 5,
            prompt: String::new(),
            trace_chat_id: stage_trace_chat_id("rv-1", label),
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    fn params(parallel_depth: usize) -> ScheduleParams {
        ScheduleParams {
            parallel_depth,
            idle_timeout: Duration::from_secs(120),
            grace: Duration::from_millis(40),
        }
    }

    struct CountingExecutor {
        active: AtomicUsize,
        peak: AtomicUsize,
        delay: Duration,
    }

    #[async_trait]
    impl StageExecutor for CountingExecutor {
        async fn execute(&self, job: StageJob, _activity: Arc<AtomicU64>) -> StageProduct {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            StageProduct {
                run: StageRun::ok(&job.label, Some("m".to_string()), 1),
                findings: vec![],
                verdicts: vec![],
                metering: serde_json::Map::new(),
                raw: None,
            }
        }
    }

    struct PanickingExecutor;

    #[async_trait]
    impl StageExecutor for PanickingExecutor {
        async fn execute(&self, _job: StageJob, _activity: Arc<AtomicU64>) -> StageProduct {
            panic!("stage exploded");
        }
    }

    fn never_finishing() -> BoxedProduct {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            unreachable!()
        })
    }

    #[tokio::test]
    async fn review_scheduler_never_exceeds_parallel_depth() {
        let executor = Arc::new(CountingExecutor {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            delay: Duration::from_millis(30),
        });
        let jobs: Vec<StageJob> = [
            "mechanical",
            "diff",
            "impact",
            "spec",
            "security",
            "simplicity",
        ]
        .iter()
        .map(|id| job(id, id))
        .collect();

        let products = run_stage_jobs(executor.clone(), jobs, params(2), None).await;

        assert_eq!(products.len(), 6);
        assert!(products
            .iter()
            .all(|product| product.run.status == StageStatusKind::Ok));
        assert!(executor.peak.load(Ordering::SeqCst) <= 2);
        assert!(executor.peak.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn review_scheduler_turns_a_panicking_stage_into_a_failed_row() {
        let products = run_stage_jobs(
            Arc::new(PanickingExecutor),
            vec![job("spec", "spec")],
            params(2),
            None,
        )
        .await;

        assert_eq!(products[0].run.status, StageStatusKind::Failed);
        assert_eq!(products[0].run.reason.as_deref(), Some("stage panicked"));
        assert_eq!(
            products[0].run.trace_chat_id.as_deref(),
            Some("subchat-rv-1-spec")
        );
    }

    #[test]
    fn review_scheduler_idle_limit_never_drops_below_the_floor() {
        let mut schedule = params(1);
        schedule.idle_timeout = Duration::from_secs(1);
        assert_eq!(schedule.idle_limit(), MIN_IDLE_TIMEOUT);

        schedule.idle_timeout = Duration::from_secs(300);
        assert_eq!(schedule.idle_limit(), Duration::from_secs(300));
    }

    #[tokio::test]
    async fn review_watchdog_kills_a_silent_stage() {
        let activity = Arc::new(AtomicU64::new(now_ms()));
        let abort = Arc::new(AtomicBool::new(false));

        let product = watch_stage(
            "diff".to_string(),
            "subchat-rv-1-diff".to_string(),
            Duration::from_millis(60),
            Duration::from_millis(10),
            Duration::from_millis(40),
            activity,
            abort.clone(),
            None,
            never_finishing(),
        )
        .await;

        assert_eq!(product.run.status, StageStatusKind::TimedOut);
        assert!(abort.load(Ordering::SeqCst));
        assert!(product
            .run
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("stopped responding")));
        assert_eq!(
            product.run.trace_chat_id.as_deref(),
            Some("subchat-rv-1-diff")
        );
        assert!(product.findings.is_empty());
    }

    async fn assert_heartbeating_stage_survives(
        iterations: usize,
        beat: Duration,
        idle_limit: Duration,
        poll: Duration,
    ) -> StageProduct {
        let activity = Arc::new(AtomicU64::new(now_ms()));
        let abort = Arc::new(AtomicBool::new(false));
        let heartbeat = activity.clone();
        let work: BoxedProduct = Box::pin(async move {
            let started = now_ms();
            for _ in 0..iterations {
                tokio::time::sleep(beat).await;
                heartbeat.store(now_ms(), Ordering::Relaxed);
            }
            StageProduct {
                run: StageRun::ok(
                    "diff",
                    Some("m".to_string()),
                    now_ms().saturating_sub(started),
                ),
                findings: vec![],
                verdicts: vec![],
                metering: serde_json::Map::new(),
                raw: None,
            }
        });

        let product = watch_stage(
            "diff".to_string(),
            "subchat-rv-1-diff".to_string(),
            idle_limit,
            poll,
            Duration::from_millis(40),
            activity,
            abort.clone(),
            None,
            work,
        )
        .await;

        assert_eq!(product.run.status, StageStatusKind::Ok);
        assert!(product.run.reason.is_none());
        assert!(abort.load(Ordering::SeqCst));
        product
    }

    #[ignore = "wall-clock sensitive: flakes when the whole lib suite runs in parallel"]
    #[tokio::test]
    async fn review_watchdog_keeps_an_actively_working_stage_alive_past_the_idle_limit() {
        assert_heartbeating_stage_survives(
            10,
            Duration::from_millis(30),
            Duration::from_millis(60),
            Duration::from_millis(10),
        )
        .await;
    }

    #[tokio::test]
    async fn review_watchdog_never_kills_a_stage_that_stays_active_indefinitely() {
        let idle_limit = Duration::from_millis(40);
        let product = assert_heartbeating_stage_survives(
            60,
            Duration::from_millis(10),
            idle_limit,
            Duration::from_millis(5),
        )
        .await;
        assert!(product.run.duration_ms >= 10 * idle_limit.as_millis() as u64);
    }

    #[tokio::test]
    async fn review_watchdog_streaming_stamps_keep_a_silent_channel_stage_alive_until_the_stream_stops(
    ) {
        let activity = Arc::new(AtomicU64::new(now_ms()));
        let abort = Arc::new(AtomicBool::new(false));
        let streaming_stamp = activity.clone();
        let stage_abort = abort.clone();
        let idle_limit = Duration::from_millis(60);
        let salvage_job = job("diff", "diff");
        let work: BoxedProduct = Box::pin(async move {
            let started = now_ms();
            while now_ms().saturating_sub(started) < 5 * idle_limit.as_millis() as u64 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                streaming_stamp.store(now_ms(), Ordering::Relaxed);
            }
            while !stage_abort.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let transcript = r#"{"findings":[{"title":"t","severity":"high","file":"src/lib.rs","line_start":4,"line_end":6,"claim":"c","evidence":"e","reproduction":null,"fix":null}],"summary":"partial","coverage":{"files_read":["src/lib.rs"],"commands_run":[]}}"#;
            let output = crate::tools::review_agents::contract::parse_stage_output(transcript)
                .expect("the salvage transcript carries a valid contract block");
            crate::tools::review_agents::runner::findings_product(
                "diff",
                &salvage_job,
                output,
                "m".to_string(),
                now_ms().saturating_sub(started),
            )
        });

        let product = watch_stage(
            "diff".to_string(),
            "subchat-rv-1-diff".to_string(),
            idle_limit,
            Duration::from_millis(5),
            Duration::from_secs(2),
            activity,
            abort.clone(),
            None,
            work,
        )
        .await;

        assert!(product.run.duration_ms >= 5 * idle_limit.as_millis() as u64);
        assert_eq!(product.run.status, StageStatusKind::TimedOut);
        assert!(product
            .run
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("stopped responding")));
        assert_eq!(product.findings.len(), 1);
        assert!(product
            .run
            .summary
            .as_deref()
            .is_some_and(|summary| summary.contains("salvaged from partial run")));
        assert!(abort.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn review_watchdog_salvages_findings_from_a_soft_aborted_stage() {
        let activity = Arc::new(AtomicU64::new(now_ms()));
        let abort = Arc::new(AtomicBool::new(false));
        let stage_abort = abort.clone();
        let salvage_job = job("diff", "diff");
        let work: BoxedProduct = Box::pin(async move {
            while !stage_abort.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let transcript = r#"{"findings":[{"title":"t","severity":"high","file":"src/lib.rs","line_start":4,"line_end":6,"claim":"c","evidence":"e","reproduction":null,"fix":null}],"summary":"partial","coverage":{"files_read":["src/lib.rs"],"commands_run":[]}}"#;
            let output = crate::tools::review_agents::contract::parse_stage_output(transcript)
                .expect("the salvage transcript carries a valid contract block");
            crate::tools::review_agents::runner::findings_product(
                "diff",
                &salvage_job,
                output,
                "m".to_string(),
                77,
            )
        });

        let product = watch_stage(
            "diff".to_string(),
            "subchat-rv-1-diff".to_string(),
            Duration::from_millis(80),
            Duration::from_millis(10),
            Duration::from_secs(2),
            activity,
            abort.clone(),
            None,
            work,
        )
        .await;

        assert_eq!(product.run.status, StageStatusKind::TimedOut);
        assert!(product
            .run
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("stopped responding")));
        assert_eq!(product.findings.len(), 1);
        assert_eq!(product.findings[0].title, "t");
        assert_eq!(product.run.findings, 1);
        assert_eq!(product.run.coverage.files_read, ["src/lib.rs"]);
        assert!(product
            .run
            .summary
            .as_deref()
            .is_some_and(|summary| summary.contains("salvaged from partial run")));
        assert!(abort.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn review_watchdog_parent_abort_propagates_to_the_stage_flag() {
        let activity = Arc::new(AtomicU64::new(now_ms()));
        let abort = Arc::new(AtomicBool::new(false));
        let parent = Arc::new(AtomicBool::new(true));
        let stage_abort = abort.clone();
        let work: BoxedProduct = Box::pin(async move {
            while !stage_abort.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            StageProduct {
                run: StageRun::ok("diff", Some("m".to_string()), 5),
                findings: vec![],
                verdicts: vec![],
                metering: serde_json::Map::new(),
                raw: None,
            }
        });

        let product = watch_stage(
            "diff".to_string(),
            "subchat-rv-1-diff".to_string(),
            Duration::from_secs(600),
            Duration::from_millis(10),
            Duration::from_secs(2),
            activity,
            abort.clone(),
            Some(parent),
            work,
        )
        .await;

        assert!(abort.load(Ordering::SeqCst));
        assert_eq!(product.run.status, StageStatusKind::Ok);
    }

    #[tokio::test]
    async fn review_scheduler_leaves_no_stage_abort_flag_unset() {
        let executor = Arc::new(CountingExecutor {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            delay: Duration::from_millis(10),
        });
        let jobs = vec![
            job("diff", "diff"),
            job("spec", "spec"),
            job("impact", "impact"),
        ];
        let flags: Vec<Arc<AtomicBool>> = jobs.iter().map(|job| job.abort.clone()).collect();

        let products = run_stage_jobs(executor, jobs, params(1), None).await;

        assert_eq!(products.len(), 3);
        assert!(products
            .iter()
            .all(|product| product.run.status == StageStatusKind::Ok));
        assert!(flags.iter().all(|flag| flag.load(Ordering::SeqCst)));
    }
}
