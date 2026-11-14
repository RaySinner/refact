pub use refact_tools::tools::review_agents::runner::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::mpsc::UnboundedSender;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::TaskMeta;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::{ExplicitSubchatSpec, TraceParent};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::contract::{
    parse_stage_output, parse_verdict_output, StageOutput, Verdict, VerdictOutput, REPAIR_PROMPT,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::prompts::REVIEW_SYSTEM_PROMPT;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::stages::{StageContract, StageSpec};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_types::{ReviewFinding, StageRun, StageStatusKind};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::subagent_phases::get_last_assistant_content;
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::types::WorktreeMeta;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::review_agents::contract::parse_stage_output;
    use crate::tools::review_agents::stages::embedded_catalog;

    fn job(stage_id: &str) -> StageJob {
        StageJob {
            spec: Arc::new(
                embedded_catalog()
                    .into_iter()
                    .find(|spec| spec.id == stage_id)
                    .unwrap(),
            ),
            label: stage_id.to_string(),
            subchat: ExplicitSubchatSpec {
                params: refact_chat_api::SubchatParameters {
                    subchat_model_type: refact_core::chat_types::ChatModelType::Default,
                    subchat_model: "test-model".to_string(),
                    subchat_n_ctx: 100000,
                    subchat_max_new_tokens: 8000,
                    subchat_temperature: None,
                    subchat_tokens_for_rag: 0,
                    subchat_reasoning_effort: None,
                    subchat_cache_control: crate::llm::params::CacheControl::Off,
                },
                model: "test-model".to_string(),
                autonomous_no_confirm: true,
            },
            max_steps: 10,
            prompt: String::new(),
            trace_chat_id: stage_trace_chat_id("rv-1", stage_id),
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    fn ctx_with_worktree(worktree: Option<WorktreeMeta>) -> StageCtx {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        StageCtx {
            review_id: "rv-1".to_string(),
            tool_call_id: "tc-1".to_string(),
            subchat_tx: Arc::new(AMutex::new(tx)),
            abort_flag: Arc::new(AtomicBool::new(false)),
            depth: 0,
            task_meta: None,
            worktree,
            chat_id: "chat".to_string(),
            trace_parent_chat_id: "chat".to_string(),
            root_chat_id: "chat".to_string(),
            activity: None,
        }
    }

    #[test]
    fn review_runner_every_stage_runs_in_the_caller_worktree() {
        let meta = WorktreeMeta {
            id: "wt-1".to_string(),
            kind: "chat".to_string(),
            root: std::path::PathBuf::from("/tmp/wt-1"),
            source_workspace_root: std::path::PathBuf::from("/tmp/workspace"),
            repo_root: std::path::PathBuf::from("/tmp/workspace"),
            branch: Some("refact/chat/abc".to_string()),
            base_branch: None,
            base_commit: None,
            task_id: None,
            card_id: None,
            agent_id: None,
            enforce: false,
        };

        let inherited = inherited_worktree(&ctx_with_worktree(Some(meta.clone())));

        assert_eq!(inherited.as_ref().map(|w| w.id.as_str()), Some("wt-1"));
        assert_eq!(
            inherited.and_then(|w| w.branch),
            Some("refact/chat/abc".to_string())
        );
        assert!(inherited_worktree(&ctx_with_worktree(None)).is_none());
    }

    #[test]
    fn review_runner_maps_contract_findings_onto_review_findings() {
        let output = parse_stage_output(
            r#"{"findings":[{"title":"t","severity":"high","file":"src/lib.rs","line_start":4,"line_end":6,"claim":"c","evidence":"e","reproduction":"cargo test","fix":"f"}],"summary":"ok","coverage":{"files_read":["src/lib.rs"],"commands_run":[{"cmd":"cargo test","exit":1}]}}"#,
        )
        .unwrap();

        let product = findings_product("diff", &job("diff"), output, "m".to_string(), 42);

        assert_eq!(product.run.findings, 1);
        assert_eq!(product.run.duration_ms, 42);
        assert_eq!(product.run.coverage.commands_run[0].exit, 1);
        assert_eq!(product.findings[0].stage, "diff");
        assert_eq!(product.findings[0].reported_by, ["diff"]);
        assert!(!product.findings[0].evidence_present);
        assert!(!product.findings[0].introduced_by_diff);
        assert_eq!(product.run.summary.as_deref(), Some("ok"));
    }

    #[test]
    fn review_runner_records_malformed_entries_in_the_stage_summary() {
        assert_eq!(
            summary_with_rejections("checked".to_string(), &["bad".to_string()]),
            Some("checked (1 malformed entries skipped)".to_string())
        );
        assert_eq!(summary_with_rejections(String::new(), &[]), None);
    }

    #[test]
    fn review_runner_trace_chat_id_is_deterministic_and_slug_safe() {
        assert_eq!(
            stage_trace_chat_id("rv-b08fc869", "diff"),
            "subchat-rv-b08fc869-diff"
        );
        assert_eq!(
            stage_trace_chat_id("rv-1", "impact@thinking"),
            "subchat-rv-1-impact-thinking"
        );
    }

    #[tokio::test]
    async fn review_runner_monitor_ctx_forwards_messages_and_stamps_activity() {
        let (parent_tx, mut parent_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let mut ctx = ctx_with_worktree(None);
        ctx.subchat_tx = Arc::new(AMutex::new(parent_tx));
        let activity = Arc::new(AtomicU64::new(1));

        let (monitored, forwarder) = monitor_ctx(&ctx, activity.clone());
        monitored
            .subchat_tx
            .lock()
            .await
            .send(serde_json::json!({"subchat_id": "progress"}))
            .unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(2), parent_rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(received["subchat_id"], "progress");
        assert!(activity.load(Ordering::Relaxed) > 1);
        forwarder.abort();
    }

    #[test]
    fn review_runner_metering_sums_numbers_and_replaces_other_values() {
        let mut target = serde_json::Map::new();
        target.insert("tokens".to_string(), serde_json::json!(10));
        target.insert("model".to_string(), serde_json::json!("a"));
        let mut source = serde_json::Map::new();
        source.insert("tokens".to_string(), serde_json::json!(5));
        source.insert("model".to_string(), serde_json::json!("b"));
        source.insert("new".to_string(), serde_json::json!(1));

        merge_metering(&mut target, source);

        assert_eq!(target["tokens"], serde_json::json!(15.0));
        assert_eq!(target["model"], serde_json::json!("b"));
        assert_eq!(target["new"], serde_json::json!(1));
    }
}
