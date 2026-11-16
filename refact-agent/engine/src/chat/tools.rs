pub use refact_tools::chat_tools::*;

// The engine-side test surface keeps the prelude the moved file used to own:
// `mod tests` and the two cfg(test) helpers below reach these names through
// `use super::*`, and the crate's own imports are private to the crate.
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use chrono::Utc;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use crate::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use crate::at_commands::at_commands::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::call_validation::{
    ChatContent, ChatMessage, ChatToolCall, ChatToolFunction, ContextEnum, ContextFile,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::types::*;
#[cfg(test)]
#[allow(unused_imports)]
use refact_buddy_core::user_action::UserAction;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::{ToolCatalogSnapshot, TurnToolPool};
#[cfg(test)]
#[allow(unused_imports)]
use refact_tool_api::{MatchConfirmDenyResult, ToolDesc};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
    use crate::chat::perf_diagnostics::{MemoryPerfSink, PerfClock, PerfRecorder};
    use serial_test::serial;

    struct TestPerfClock {
        now: AtomicU64,
    }

    impl TestPerfClock {
        fn new() -> Self {
            Self {
                now: AtomicU64::new(0),
            }
        }
    }

    impl PerfClock for TestPerfClock {
        fn now_us(&self) -> u64 {
            self.now.fetch_add(1, AtomicOrdering::SeqCst)
        }
    }

    fn install_perf_recorder() -> (perf_diagnostics::TestRecorderGuard, Arc<MemoryPerfSink>) {
        let sink = Arc::new(MemoryPerfSink::new());
        let recorder = Arc::new(PerfRecorder::with_salt(
            Arc::new(TestPerfClock::new()),
            sink.clone(),
            [9; 32],
        ));
        (perf_diagnostics::install_test_recorder(recorder), sink)
    }

    #[derive(Clone)]
    struct DeterministicToolRegistry {
        confirmation_result: MatchConfirmDenyResult,
        runtime_failure: bool,
    }

    fn deterministic_tool_desc(name: &str) -> refact_tool_api::ToolDesc {
        refact_tool_api::ToolDesc {
            name: name.to_string(),
            experimental: false,
            allow_parallel: true,
            description: format!("Deterministic {name} tool"),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            output_schema: None,
            annotations: None,
            display_name: name.to_string(),
            source: refact_tool_api::ToolSource {
                source_type: refact_tool_api::ToolSourceType::Builtin,
                config_path: String::new(),
            },
        }
    }

    struct DeterministicTool {
        name: String,
        confirmation_result: MatchConfirmDenyResult,
        runtime_failure: bool,
    }

    #[async_trait::async_trait]
    impl crate::tools::tools_description::Tool for DeterministicTool {
        async fn tool_execute(
            &mut self,
            _ccx: Arc<AMutex<AtCommandsContext>>,
            tool_call_id: &String,
            _args: &std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<(bool, Vec<crate::call_validation::ContextEnum>), String> {
            if self.runtime_failure {
                return Err("deterministic runtime failure".to_string());
            }
            let delay = if self.name == "sleep" {
                std::time::Duration::from_secs(8)
            } else {
                std::time::Duration::from_millis(20)
            };
            tokio::time::sleep(delay).await;
            let mut message =
                ChatMessage::new("tool".to_string(), "deterministic result".to_string());
            message.tool_call_id = tool_call_id.clone();
            Ok((
                false,
                vec![crate::call_validation::ContextEnum::ChatMessage(message)],
            ))
        }

        fn tool_description(&self) -> refact_tool_api::ToolDesc {
            deterministic_tool_desc(&self.name)
        }

        async fn match_against_confirm_deny(
            &self,
            _ccx: Arc<AMutex<AtCommandsContext>>,
            _args: &std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<refact_tool_api::MatchConfirmDeny, String> {
            Ok(refact_tool_api::MatchConfirmDeny {
                result: self.confirmation_result.clone(),
                command: self.name.clone(),
                rule: "deterministic confirmation".to_string(),
            })
        }
    }

    impl DeterministicToolRegistry {
        fn tools(&self) -> Vec<Box<dyn crate::tools::tools_description::Tool + Send>> {
            ["alpha", "beta", "sleep"]
                .into_iter()
                .map(|name| {
                    Box::new(DeterministicTool {
                        name: name.to_string(),
                        confirmation_result: self.confirmation_result.clone(),
                        runtime_failure: self.runtime_failure,
                    }) as Box<dyn crate::tools::tools_description::Tool + Send>
                })
                .collect()
        }
    }

    #[derive(Clone)]
    struct BarrierToolRegistry {
        started: Arc<AtomicUsize>,
        barrier: Arc<tokio::sync::Barrier>,
    }

    struct BarrierTool {
        started: Arc<AtomicUsize>,
        barrier: Arc<tokio::sync::Barrier>,
    }

    #[async_trait::async_trait]
    impl crate::tools::tools_description::Tool for BarrierTool {
        async fn tool_execute(
            &mut self,
            _ccx: Arc<AMutex<AtCommandsContext>>,
            tool_call_id: &String,
            _args: &std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<(bool, Vec<crate::call_validation::ContextEnum>), String> {
            self.started.fetch_add(1, AtomicOrdering::SeqCst);
            self.barrier.wait().await;
            let mut message = ChatMessage::new("tool".to_string(), "released".to_string());
            message.tool_call_id = tool_call_id.clone();
            Ok((
                false,
                vec![crate::call_validation::ContextEnum::ChatMessage(message)],
            ))
        }

        fn tool_description(&self) -> refact_tool_api::ToolDesc {
            deterministic_tool_desc("parallel_barrier")
        }

        async fn match_against_confirm_deny(
            &self,
            _ccx: Arc<AMutex<AtCommandsContext>>,
            _args: &std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<refact_tool_api::MatchConfirmDeny, String> {
            Ok(refact_tool_api::MatchConfirmDeny {
                result: MatchConfirmDenyResult::PASS,
                command: "parallel_barrier".to_string(),
                rule: "parallel test".to_string(),
            })
        }
    }

    impl BarrierToolRegistry {
        fn tools(&self) -> Vec<Box<dyn crate::tools::tools_description::Tool + Send>> {
            vec![Box::new(BarrierTool {
                started: self.started.clone(),
                barrier: self.barrier.clone(),
            })]
        }
    }

    fn deterministic_tool_call(id: &str, name: &str) -> ChatToolCall {
        ChatToolCall {
            id: id.to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        }
    }

    async fn process_deterministic_tool_turn(
        registry: DeterministicToolRegistry,
        tool_calls: Vec<ChatToolCall>,
    ) -> (
        ToolStepOutcome,
        Arc<MemoryPerfSink>,
        Arc<AMutex<ChatSession>>,
    ) {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let base_app = crate::app_state::app_state_from_gcx(gcx).await;
        let test_tools = registry.clone();
        let app = AppState {
            tool_registry: Arc::new(
                crate::app_state::AppToolRegistry::with_fixture_tool_factory(
                    base_app.gcx.clone(),
                    Arc::new(move || test_tools.tools()),
                ),
            ),
            ..base_app
        };
        let session = Arc::new(AMutex::new(ChatSession::new("same-turn-tools".to_string())));
        {
            let mut locked = session.lock().await;
            locked.thread.model = "model".to_string();
            let mut assistant = ChatMessage::new("assistant".to_string(), String::new());
            assistant.tool_calls = Some(tool_calls);
            locked.add_message(assistant);
        }
        app.chat
            .sessions
            .write()
            .await
            .insert("same-turn-tools".to_string(), session.clone());

        let outcome = process_tool_calls_once(app, session.clone(), "agent", None).await;
        (outcome, sink, session)
    }

    fn recorded_components(sink: &MemoryPerfSink) -> Vec<&'static str> {
        sink.events().iter().map(|event| event.component).collect()
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn same_turn_tool_diagnostics_cover_stages_without_confirmation_wait() {
        let (outcome, sink, _) = process_deterministic_tool_turn(
            DeterministicToolRegistry {
                confirmation_result: MatchConfirmDenyResult::PASS,
                runtime_failure: false,
            },
            vec![
                deterministic_tool_call("tool-alpha", "alpha"),
                deterministic_tool_call("tool-beta", "beta"),
            ],
        )
        .await;

        assert!(matches!(outcome, ToolStepOutcome::Continue));
        let components = recorded_components(&sink);
        for component in [
            PerfComponent::ToolCatalogBuild,
            PerfComponent::ToolSessionExtraction,
            PerfComponent::ToolCatalogPoolAcquire,
            PerfComponent::ToolAliasResolution,
            PerfComponent::ToolConfirmationPreflight,
            PerfComponent::ToolPolicyLookup,
            PerfComponent::ToolPreHook,
            PerfComponent::ToolExecutionWait,
            PerfComponent::ToolRuntime,
            PerfComponent::ToolPostHook,
            PerfComponent::ToolResultMerge,
            PerfComponent::ToolResultPostprocess,
            PerfComponent::ToolSessionMergeEvents,
            PerfComponent::ToolCheckpointScheduling,
        ] {
            assert!(
                components.contains(&component.as_str()),
                "missing {component:?}"
            );
        }
        assert!(components
            .iter()
            .all(|component| *component != "tool.confirmation_wait"));
        let runtime_events = sink
            .events()
            .into_iter()
            .filter(|event| event.component == PerfComponent::ToolRuntime.as_str())
            .collect::<Vec<_>>();
        assert_eq!(runtime_events.len(), 2);
        assert!(runtime_events.iter().all(|event| {
            event.outcome == PerfOutcome::Success.as_str()
                && event.batch_size == Some(1)
                && event.execution_class.is_none()
        }));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn more_than_thirty_two_parallel_safe_tools_start_together() {
        const TOOL_COUNT: usize = 40;
        let started = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(TOOL_COUNT + 1));
        let registry = BarrierToolRegistry {
            started: started.clone(),
            barrier: barrier.clone(),
        };
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let base_app = crate::app_state::app_state_from_gcx(gcx).await;
        let fixture = registry.clone();
        let app = AppState {
            tool_registry: Arc::new(
                crate::app_state::AppToolRegistry::with_fixture_tool_factory(
                    base_app.gcx.clone(),
                    Arc::new(move || fixture.tools()),
                ),
            ),
            ..base_app
        };
        let session = Arc::new(AMutex::new(ChatSession::new(
            "unbounded-parallel-tools".to_string(),
        )));
        {
            let mut locked = session.lock().await;
            locked.thread.model = "model".to_string();
            let mut assistant = ChatMessage::new("assistant".to_string(), String::new());
            assistant.tool_calls = Some(
                (0..TOOL_COUNT)
                    .map(|index| {
                        deterministic_tool_call(&format!("parallel-{index}"), "parallel_barrier")
                    })
                    .collect(),
            );
            locked.add_message(assistant);
        }
        app.chat
            .sessions
            .write()
            .await
            .insert("unbounded-parallel-tools".to_string(), session.clone());

        let execution = tokio::spawn(process_tool_calls_once(app, session, "agent", None));
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while started.load(AtomicOrdering::SeqCst) < TOOL_COUNT {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("all parallel-safe tools started before any completed");

        barrier.wait().await;
        let outcome = execution.await.expect("tool execution task");
        assert!(matches!(outcome, ToolStepOutcome::Continue));
    }

    #[tokio::test]
    async fn catalog_aliases_remain_stable_when_tool_descriptors_change_mid_batch() {
        let initial = ToolCatalogSnapshot {
            index: refact_runtime_api::ToolRegistryIndex {
                tools: vec![deterministic_tool_desc(
                    "very_long_tool_name_that_exceeds_the_64_character_limit_for_provider_apis",
                )],
                mcp_lazy_mode: false,
                mcp_total_count: 0,
                mcp_tool_index: Vec::new(),
            },
            policy: Vec::new(),
            aliases: refact_tool_api::build_registry_from_names(&vec![
                "very_long_tool_name_that_exceeds_the_64_character_limit_for_provider_apis"
                    .to_string(),
            ]),
        };
        let alias = initial
            .aliases
            .get_alias(&initial.index.tools[0].name)
            .unwrap();
        let call = deterministic_tool_call("call-1", alias);

        let resolved = resolve_tool_call_aliases_with_catalog(vec![call], &initial);

        assert_eq!(resolved[0].function.name, initial.index.tools[0].name);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn app_registry_runtime_starts_after_catalog_build() {
        let (outcome, sink, _) = process_deterministic_tool_turn(
            DeterministicToolRegistry {
                confirmation_result: MatchConfirmDenyResult::PASS,
                runtime_failure: false,
            },
            vec![deterministic_tool_call("tool-alpha", "alpha")],
        )
        .await;

        assert!(matches!(outcome, ToolStepOutcome::Continue));
        let events = sink.events();
        let runtime_index = events
            .iter()
            .position(|event| event.component == PerfComponent::ToolRuntime.as_str())
            .expect("runtime event");
        let catalog_index = events
            .iter()
            .position(|event| event.component == PerfComponent::ToolCatalogBuild.as_str())
            .expect("catalog build event");
        assert!(catalog_index < runtime_index);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.component == PerfComponent::ToolRuntime.as_str())
                .count(),
            1
        );
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn same_turn_tool_diagnostics_record_runtime_failure() {
        let (outcome, sink, _) = process_deterministic_tool_turn(
            DeterministicToolRegistry {
                confirmation_result: MatchConfirmDenyResult::PASS,
                runtime_failure: true,
            },
            vec![deterministic_tool_call("tool-alpha", "alpha")],
        )
        .await;

        assert!(matches!(outcome, ToolStepOutcome::Continue));
        let runtime_events = sink
            .events()
            .into_iter()
            .filter(|event| event.component == PerfComponent::ToolRuntime.as_str())
            .collect::<Vec<_>>();
        assert_eq!(runtime_events.len(), 1);
        assert_eq!(runtime_events[0].outcome, PerfOutcome::Failure.as_str());
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn same_turn_confirmation_pause_does_not_record_wait_time() {
        let (outcome, sink, _) = process_deterministic_tool_turn(
            DeterministicToolRegistry {
                confirmation_result: MatchConfirmDenyResult::CONFIRMATION,
                runtime_failure: false,
            },
            vec![deterministic_tool_call("tool-alpha", "alpha")],
        )
        .await;

        assert!(matches!(outcome, ToolStepOutcome::Paused));
        let components = recorded_components(&sink);
        assert!(components.contains(&PerfComponent::ToolConfirmationPreflight.as_str()));
        assert!(components.contains(&PerfComponent::ToolPolicyLookup.as_str()));
        assert!(components
            .iter()
            .all(|component| *component != "tool.confirmation_wait"));
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn tool_call_timing_is_stamped_at_invocation_and_completion() {
        let before = tool_timestamp_ms().unwrap();
        let (outcome, _, session) = process_deterministic_tool_turn(
            DeterministicToolRegistry {
                confirmation_result: MatchConfirmDenyResult::PASS,
                runtime_failure: false,
            },
            vec![deterministic_tool_call("tool-alpha", "alpha")],
        )
        .await;
        let after = tool_timestamp_ms().unwrap();

        assert!(matches!(outcome, ToolStepOutcome::Continue));
        let session = session.lock().await;
        let tool_call = session.messages[0]
            .tool_calls
            .as_ref()
            .unwrap()
            .first()
            .unwrap();
        let started_at_ms = tool_call.started_at_ms.unwrap();
        let completed_at_ms = tool_call.completed_at_ms.unwrap();
        assert!((before..=after).contains(&started_at_ms));
        assert!((started_at_ms..=after).contains(&completed_at_ms));
        assert!(completed_at_ms.saturating_sub(started_at_ms) >= 20);
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn sleep_tool_call_records_eight_second_duration() {
        let (outcome, _, session) = process_deterministic_tool_turn(
            DeterministicToolRegistry {
                confirmation_result: MatchConfirmDenyResult::PASS,
                runtime_failure: false,
            },
            vec![ChatToolCall {
                id: "tool-sleep".to_string(),
                index: None,
                function: crate::call_validation::ChatToolFunction {
                    name: "sleep".to_string(),
                    arguments: r#"{"duration_ms":8000,"description":"Wait eight seconds"}"#
                        .to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            }],
        )
        .await;

        assert!(matches!(outcome, ToolStepOutcome::Continue));
        let session = session.lock().await;
        let tool_call = session.messages[0]
            .tool_calls
            .as_ref()
            .unwrap()
            .first()
            .unwrap();
        let duration_ms = tool_call
            .completed_at_ms
            .unwrap()
            .saturating_sub(tool_call.started_at_ms.unwrap());
        assert!(
            (7_500..=9_000).contains(&duration_ms),
            "duration: {duration_ms}ms"
        );
    }

    fn sample_worktree() -> (tempfile::TempDir, crate::worktrees::types::WorktreeMeta) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worktree");
        let source = temp.path().join("source");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&source).unwrap();
        let root = dunce::simplified(&fs::canonicalize(root).unwrap()).to_path_buf();
        let source = dunce::simplified(&fs::canonicalize(source).unwrap()).to_path_buf();
        (
            temp,
            crate::worktrees::types::WorktreeMeta {
                id: "wt-tools".to_string(),
                kind: "chat".to_string(),
                root,
                source_workspace_root: source.clone(),
                repo_root: source,
                branch: Some("feature".to_string()),
                base_branch: Some("main".to_string()),
                base_commit: Some("base".to_string()),
                task_id: Some("task-1".to_string()),
                card_id: Some("card-1".to_string()),
                agent_id: Some("agent-1".to_string()),
                enforce: true,
            },
        )
    }

    #[tokio::test]
    async fn subchat_worktree_tool_execution_context_inherits_thread_scope() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (_temp, worktree) = sample_worktree();
        let task_meta = TaskMeta {
            task_id: "task-1".to_string(),
            role: "agents".to_string(),
            agent_id: Some("agent-1".to_string()),
            card_id: Some("card-1".to_string()),
            planner_chat_id: Some("planner-task-1-1".to_string()),
        };
        let thread = ThreadParams {
            id: "chat-tools".to_string(),
            model: "model".to_string(),
            task_meta: Some(task_meta.clone()),
            worktree: Some(worktree.clone()),
            ..Default::default()
        };
        let ccx =
            build_tool_execution_context(crate::app_state::app_state_from_gcx(gcx).await, 4096, &[], &thread, None)
                .await
                .unwrap();
        let ccx = ccx.lock().await;

        assert_eq!(ccx.task_meta, Some(task_meta));
        assert_eq!(ccx.execution_scope_root(), Some(worktree.root.clone()));
        assert_eq!(ccx.execution_scope_worktree(), Some(worktree));
    }

    #[test]
    fn test_is_server_executed_tool_with_prefix() {
        assert!(is_server_executed_tool("srvtoolu_abc123"));
        assert!(is_server_executed_tool("srvtoolu_"));
        assert!(is_server_executed_tool("srvtoolu_very_long_id_here"));
    }

    #[test]
    fn test_is_server_executed_tool_without_prefix() {
        assert!(!is_server_executed_tool("call_abc123"));
        assert!(!is_server_executed_tool("toolu_abc123"));
        assert!(!is_server_executed_tool(""));
        assert!(!is_server_executed_tool("srvtoolu"));
        assert!(!is_server_executed_tool("SRVTOOLU_abc"));
    }

    #[test]
    fn tool_execution_messages_prefers_prepared_prompt_history() {
        let raw = vec![ChatMessage::new("user".to_string(), "raw".to_string())];
        let prepared = vec![ChatMessage::new("user".to_string(), "prepared".to_string())];

        let selected_prepared = tool_execution_messages(&raw, &prepared);
        let selected_raw = tool_execution_messages(&raw, &[]);

        assert_eq!(selected_prepared[0].content.content_text_only(), "prepared");
        assert_eq!(selected_raw[0].content.content_text_only(), "raw");
    }

    #[test]
    fn shell_privacy_pending_result_uses_confirmation_pause() {
        let tool_call = ChatToolCall {
            id: "tc-shell".to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: "shell".to_string(),
                arguments: "{}".to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        };
        let mut result = ChatMessage::new("tool".to_string(), "pending".to_string());
        result.tool_call_id = tool_call.id.clone();
        result.extra.insert(
            "privacy_shell".to_string(),
            serde_json::json!({"ask_pending": true}),
        );

        let reasons = shell_privacy_approval_reasons(&[tool_call], &[result]);

        assert_eq!(reasons.len(), 1);
        assert!(is_shell_privacy_approval(&reasons[0]));
        assert_eq!(reasons[0].tool_call_id, "tc-shell");
    }

    #[test]
    fn test_rewrite_cc_native_grep_tool_call() {
        let mut tc = ChatToolCall {
            id: "call-1".to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: "Grep".to_string(),
                arguments: r#"{"pattern":"needle","path":"src"}"#.to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        };

        rewrite_cc_native_stub_tool_call(&mut tc);

        assert_eq!(tc.function.name, "search_pattern");
        let args = tc.function.parse_args().unwrap();
        assert_eq!(args.get("pattern").and_then(|v| v.as_str()), Some("needle"));
        assert_eq!(args.get("scope").and_then(|v| v.as_str()), Some("src"));
    }

    #[test]
    fn test_rewrite_cc_native_glob_tool_call() {
        let mut tc = ChatToolCall {
            id: "call-1".to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: "Glob".to_string(),
                arguments: r#"{"pattern":"src/**/*.rs"}"#.to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        };

        rewrite_cc_native_stub_tool_call(&mut tc);

        assert_eq!(tc.function.name, "glob");
        let args = tc.function.parse_args().unwrap();
        assert_eq!(
            args.get("pattern").and_then(|v| v.as_str()),
            Some("src/**/*.rs")
        );
        assert!(args.get("path").is_none());
        assert!(args.get("scope").is_none());
    }

    #[test]
    fn test_rewrite_cc_native_glob_preserves_path() {
        let mut tc = ChatToolCall {
            id: "call-2".to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: "Glob".to_string(),
                arguments: r#"{"pattern":"*.rs","path":"engine/src"}"#.to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        };

        rewrite_cc_native_stub_tool_call(&mut tc);

        assert_eq!(tc.function.name, "glob");
        let args = tc.function.parse_args().unwrap();
        assert_eq!(args.get("pattern").and_then(|v| v.as_str()), Some("*.rs"));
        assert_eq!(
            args.get("path").and_then(|v| v.as_str()),
            Some("engine/src")
        );
    }

    #[test]
    fn test_mixed_cc_batch_preserves_native_stub_rewrites() {
        let mut glob = ChatToolCall {
            id: "call-glob".to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: "Glob".to_string(),
                arguments: r#"{"pattern":"src/**/*.rs"}"#.to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        };
        let mut grep = ChatToolCall {
            id: "call-grep".to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: "Grep".to_string(),
                arguments: r#"{"pattern":"needle"}"#.to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        };

        rewrite_cc_native_stub_tool_call(&mut glob);
        rewrite_cc_native_stub_tool_call(&mut grep);

        assert_eq!(glob.function.name, "glob");
        assert_eq!(grep.function.name, "search_pattern");
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn test_resolve_tool_call_aliases_handles_mixed_cc_batch_per_call() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();
        let (_guard, sink) = install_perf_recorder();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let calls = vec![
            ChatToolCall {
                id: "call-cat".to_string(),
                index: None,
                function: crate::call_validation::ChatToolFunction {
                    name: "t_cat".to_string(),
                    arguments: r#"{"paths":"src/lib.rs"}"#.to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            },
            ChatToolCall {
                id: "call-tree".to_string(),
                index: None,
                function: crate::call_validation::ChatToolFunction {
                    name: "tree".to_string(),
                    arguments: "{}".to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            },
            ChatToolCall {
                id: "call-glob".to_string(),
                index: None,
                function: crate::call_validation::ChatToolFunction {
                    name: "Glob".to_string(),
                    arguments: r#"{"pattern":"src/**/*.rs"}"#.to_string(),
                },
                tool_type: "function".to_string(),
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            },
        ];

        let resolved = resolve_tool_call_aliases(app, calls, "agent", None).await;
        let names = resolved
            .into_iter()
            .map(|call| call.function.name)
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["cat", "tree", "glob"]);
        let components: Vec<_> = sink.events().iter().map(|event| event.component).collect();
        assert!(components.contains(&PerfComponent::ToolCatalogBuild.as_str()));
        assert!(components.contains(&PerfComponent::ToolAliasResolution.as_str()));
    }

    #[test]
    fn test_is_editing_tool() {
        assert!(is_editing_tool("patch"));
        assert!(is_editing_tool("text_edit"));
        assert!(is_editing_tool("create_textdoc"));
        assert!(is_editing_tool("update_textdoc"));
        assert!(is_editing_tool("update_textdoc_regex"));
        assert!(is_editing_tool("update_textdoc_by_lines"));
        assert!(is_editing_tool("undo_textdoc"));
        assert!(is_editing_tool("mv"));
    }

    #[test]
    fn test_tool_result_truncation_exempt_tools() {
        assert!(is_tool_result_truncation_exempt("code_review"));
        assert!(is_tool_result_truncation_exempt("subagent"));
        assert!(is_tool_result_truncation_exempt("agent_wait"));
        assert!(is_tool_result_truncation_exempt("agent_result"));
        assert!(!is_tool_result_truncation_exempt("cat"));
        assert!(!is_tool_result_truncation_exempt("shell"));
        assert!(!is_tool_result_truncation_exempt("web_search"));
        assert!(!is_tool_result_truncation_exempt(""));
    }

    #[test]
    fn test_is_not_editing_tool() {
        assert!(!is_editing_tool("shell"));
        assert!(!is_editing_tool("cat"));
        assert!(!is_editing_tool("search"));
        assert!(!is_editing_tool(""));
        assert!(!is_editing_tool("PATCH"));
    }

    #[test]
    fn test_auto_approve_dangerous_commands_approves_all_tools() {
        let thread = ThreadParams {
            auto_approve_dangerous_commands: true,
            ..Default::default()
        };

        assert!(should_auto_approve_confirmation(&thread, "shell"));
        assert!(should_auto_approve_confirmation(&thread, "rm"));
        assert!(should_auto_approve_confirmation(&thread, "cat"));
        assert!(should_auto_approve_confirmation(&thread, "ask_questions"));
    }

    #[test]
    fn test_auto_approve_editing_tools_stays_limited_to_editing_tools() {
        let thread = ThreadParams {
            auto_approve_editing_tools: true,
            ..Default::default()
        };

        assert!(should_auto_approve_confirmation(&thread, "apply_patch"));
        assert!(should_auto_approve_confirmation(&thread, "mv"));
        assert!(!should_auto_approve_confirmation(&thread, "cat"));
        assert!(!should_auto_approve_confirmation(&thread, "shell"));
    }

    #[test]
    fn test_autonomous_no_confirm_bypasses_confirmation() {
        let mut thread = ThreadParams::default();
        let (changed, _) = crate::chat::queue::apply_setparams_patch(
            &mut thread,
            &serde_json::json!({"autonomous_no_confirm": true}),
        );

        assert!(changed);
        assert!(should_auto_approve_confirmation(&thread, "shell"));
        assert!(should_auto_approve_confirmation(&thread, "cat"));
        assert!(should_auto_approve_confirmation(&thread, "apply_patch"));
    }

    #[test]
    fn test_autonomous_allowlist_rejects_unlisted_tool() {
        let result = compute_final_action(
            &MatchConfirmDenyResult::CONFIRMATION,
            None,
            false,
            "unsafe_tool",
        );
        assert_eq!(result, "ask");
        let thread = ThreadParams {
            autonomous_no_confirm: true,
            ..Default::default()
        };
        assert!(should_auto_approve_confirmation(&thread, "any_tool"));
    }

    #[test]
    fn test_user_chat_still_requires_approval() {
        let thread = ThreadParams::default();

        assert!(!should_auto_approve_confirmation(&thread, "apply_patch"));
        assert!(!should_auto_approve_confirmation(&thread, "mv"));
    }

    #[tokio::test]
    async fn escalation_does_not_require_confirmation() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let thread = ThreadParams {
            id: "escalation-confirmation".to_string(),
            model: "model".to_string(),
            ..Default::default()
        };
        let session = ChatSession::new(thread.id.clone());
        app.chat
            .sessions
            .write()
            .await
            .insert(thread.id.clone(), Arc::new(AMutex::new(session)));
        let tool_call = ChatToolCall {
            id: "tc-escalate".to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: "shell".to_string(),
                arguments: serde_json::json!({
                    "command": "printf ok",
                    "description": "Print ok",
                    "escalate": {
                        "mode": "full_access",
                        "justification": "write the requested system path"
                    }
                })
                .to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        };

        let (confirmations, denials) = check_tools_confirmation(
            app.clone(),
            &[tool_call],
            &[],
            &thread,
            None,
            "agent",
            None,
            &[],
            "",
        )
        .await;

        assert!(denials.is_empty());
        assert!(confirmations.is_empty());
        let session = app
            .chat
            .sessions
            .read()
            .await
            .get(&thread.id)
            .cloned()
            .unwrap();
        let session = session.lock().await;
        assert!(session.post_tool_side_effects.is_empty());
    }

    #[serial(trajectory_perf)]
    #[tokio::test]
    async fn invalid_escalation_argument_is_still_denied() {
        let _lock = perf_diagnostics::PERF_RECORDER_TEST_LOCK.lock().unwrap();

        let (_guard, sink) = install_perf_recorder();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let thread = ThreadParams {
            id: "escalation-invalid".to_string(),
            model: "model".to_string(),
            ..Default::default()
        };
        let session = ChatSession::new(thread.id.clone());
        app.chat
            .sessions
            .write()
            .await
            .insert(thread.id.clone(), Arc::new(AMutex::new(session)));
        let tool_call = ChatToolCall {
            id: "tc-escalate-invalid".to_string(),
            index: None,
            function: crate::call_validation::ChatToolFunction {
                name: "shell".to_string(),
                arguments: serde_json::json!({
                    "command": "printf ok",
                    "description": "Print ok",
                    "escalate": {"mode": "full_access", "justification": "   "}
                })
                .to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        };

        let (confirmations, denials) = check_tools_confirmation(
            app.clone(),
            &[tool_call],
            &[],
            &thread,
            None,
            "agent",
            None,
            &[],
            "",
        )
        .await;

        assert!(confirmations.is_empty());
        assert_eq!(denials.len(), 1);
        assert!(denials[0].rule.contains("justification"));
        let components: Vec<_> = sink.events().iter().map(|event| event.component).collect();
        assert!(components.contains(&PerfComponent::ToolConfirmationPreflight.as_str()));
        assert!(components.contains(&PerfComponent::ToolPolicyLookup.as_str()));
    }

    #[test]
    fn ordinary_confirmation_keeps_existing_auto_approval_behavior() {
        let thread = ThreadParams {
            auto_approve_dangerous_commands: true,
            ..Default::default()
        };
        let reason = PauseReason {
            reason_type: "confirmation".to_string(),
            tool_name: "shell".to_string(),
            command: "printf ok".to_string(),
            rule: "matched confirmation rule".to_string(),
            tool_call_id: "tc-shell".to_string(),
            integr_config_path: None,
        };

        assert!(should_auto_approve_confirmation(&thread, &reason.tool_name));
    }

    #[test]
    fn test_tool_config_default() {
        let config = refact_tool_api::ToolConfig::default();
        assert!(config.enabled);
        assert!(config.allow_parallel.is_none());
    }

    #[test]
    fn test_tool_config_serde_roundtrip() {
        let config = refact_tool_api::ToolConfig {
            enabled: true,
            allow_parallel: Some(false),
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: refact_tool_api::ToolConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.enabled, config.enabled);
        assert_eq!(parsed.allow_parallel, config.allow_parallel);
    }

    #[test]
    fn test_tool_config_serde_skip_none() {
        let config = refact_tool_api::ToolConfig {
            enabled: true,
            allow_parallel: None,
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(!yaml.contains("allow_parallel"));
    }

    #[test]
    fn test_tool_desc_default_allow_parallel() {
        let yaml = r#"
name: test_tool
description: A test tool
input_schema:
  type: object
  properties: {}
  required: []
display_name: Test Tool
source:
  source_type: builtin
  config_path: ""
"#;
        let desc: refact_tool_api::ToolDesc = serde_yaml::from_str(yaml).unwrap();
        assert!(!desc.allow_parallel);
    }

    #[test]
    fn test_yaml_cannot_enable_parallel_for_unsafe_tools() {
        // Test that YAML override can only DISABLE parallelism, not enable it
        // This is the security policy: tools that declared allow_parallel=false
        // cannot be overridden to true via YAML config

        // Tool declares allow_parallel=false (unsafe tool)
        let tool_allow_parallel = false;

        // YAML tries to override to true
        let config_override = Some(true);

        // Security policy: if tool declared false, ignore override-to-true
        let effective = if tool_allow_parallel {
            config_override.unwrap_or(true)
        } else {
            false // ignore override-to-true for safety
        };

        assert!(
            !effective,
            "YAML should not be able to enable parallelism for unsafe tools"
        );
    }

    #[test]
    fn test_yaml_can_disable_parallel_for_safe_tools() {
        // Test that YAML override CAN disable parallelism for safe tools

        // Tool declares allow_parallel=true (safe tool)
        let tool_allow_parallel = true;

        // YAML overrides to false
        let config_override = Some(false);

        // Policy: if tool declared true, YAML can disable it
        let effective = if tool_allow_parallel {
            config_override.unwrap_or(true)
        } else {
            false
        };

        assert!(
            !effective,
            "YAML should be able to disable parallelism for safe tools"
        );
    }

    #[test]
    fn test_barrier_scheduling_logic() {
        // Test the barrier scheduling algorithm:
        // - Parallel tools (P) can run concurrently
        // - Non-parallel tools (X) act as barriers
        // Pattern: P1, P2, X, P3, P4 should execute as:
        //   [P1, P2] concurrently -> X alone -> [P3, P4] concurrently

        let tool_allow_parallel = |name: &str| -> bool {
            match name {
                "cat" | "tree" | "search" => true, // parallel
                "apply_patch" | "shell" => false,  // non-parallel (barriers)
                _ => false,
            }
        };

        // Simulate tool call sequence
        let tool_calls = vec!["cat", "tree", "apply_patch", "search", "cat"];

        let mut batches: Vec<Vec<&str>> = Vec::new();
        let mut current_batch: Vec<&str> = Vec::new();

        for tool_name in &tool_calls {
            if tool_allow_parallel(tool_name) {
                current_batch.push(tool_name);
            } else {
                // Flush parallel batch before barrier
                if !current_batch.is_empty() {
                    batches.push(current_batch.clone());
                    current_batch.clear();
                }
                // Barrier tool runs alone
                batches.push(vec![tool_name]);
            }
        }
        // Flush remaining parallel batch
        if !current_batch.is_empty() {
            batches.push(current_batch);
        }

        // Expected: [[cat, tree], [apply_patch], [search, cat]]
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0], vec!["cat", "tree"]);
        assert_eq!(batches[1], vec!["apply_patch"]);
        assert_eq!(batches[2], vec!["search", "cat"]);
    }

    #[test]
    fn test_result_ordering_preserved() {
        // Test that results are sorted by original index regardless of completion order
        let mut results: Vec<(usize, &str)> = vec![
            (2, "result_2"),
            (0, "result_0"),
            (4, "result_4"),
            (1, "result_1"),
            (3, "result_3"),
        ];

        // Sort by index (same as in execute_tools_inner)
        results.sort_by_key(|(idx, _)| *idx);

        let ordered: Vec<&str> = results.iter().map(|(_, r)| *r).collect();
        assert_eq!(
            ordered,
            vec!["result_0", "result_1", "result_2", "result_3", "result_4"]
        );
    }

    #[test]
    fn test_corrections_aggregation() {
        // Test that corrections from multiple tools are properly aggregated
        // Simulates the aggregation logic in execute_tools_inner

        // Results: (idx, had_corrections, msgs, files)
        let results: Vec<(usize, bool, Vec<&str>, Vec<&str>)> = vec![
            (0, false, vec!["msg0"], vec![]),
            (1, true, vec!["msg1"], vec![]), // This tool had corrections
            (2, false, vec!["msg2"], vec![]),
        ];

        let any_corrections = results
            .iter()
            .any(|(_, had_corrections, _, _)| *had_corrections);
        assert!(
            any_corrections,
            "Should detect that at least one tool had corrections"
        );

        // Test with no corrections
        let results_no_corrections: Vec<(usize, bool, Vec<&str>, Vec<&str>)> = vec![
            (0, false, vec!["msg0"], vec![]),
            (1, false, vec!["msg1"], vec![]),
        ];

        let any_corrections = results_no_corrections
            .iter()
            .any(|(_, had_corrections, _, _)| *had_corrections);
        assert!(
            !any_corrections,
            "Should detect no corrections when all tools succeeded cleanly"
        );
    }

    #[test]
    fn test_allowed_tool_still_denied_if_tool_says_deny() {
        assert_eq!(
            compute_final_action(&MatchConfirmDenyResult::DENY, None, true, "shell"),
            "deny"
        );
        assert_eq!(
            compute_final_action(&MatchConfirmDenyResult::DENY, Some("auto"), true, "shell"),
            "deny"
        );
        assert_eq!(
            compute_final_action(&MatchConfirmDenyResult::DENY, Some("ask"), true, "shell"),
            "deny"
        );
    }

    #[test]
    fn test_allowed_tool_auto_approves_confirmation() {
        assert_eq!(
            compute_final_action(&MatchConfirmDenyResult::CONFIRMATION, None, true, "shell"),
            "auto"
        );
        assert_eq!(
            compute_final_action(
                &MatchConfirmDenyResult::CONFIRMATION,
                Some("ask"),
                true,
                "shell"
            ),
            "auto"
        );
    }

    #[test]
    fn test_allowed_tool_respects_mode_deny() {
        assert_eq!(
            compute_final_action(
                &MatchConfirmDenyResult::CONFIRMATION,
                Some("deny"),
                true,
                "shell"
            ),
            "deny"
        );
        assert_eq!(
            compute_final_action(&MatchConfirmDenyResult::PASS, Some("deny"), true, "shell"),
            "deny"
        );
    }

    #[test]
    fn test_empty_allowed_tools_no_change() {
        assert_eq!(
            compute_final_action(&MatchConfirmDenyResult::CONFIRMATION, None, false, "shell"),
            "ask"
        );
        assert_eq!(
            compute_final_action(&MatchConfirmDenyResult::PASS, None, false, "shell"),
            "auto"
        );
        assert_eq!(
            compute_final_action(
                &MatchConfirmDenyResult::CONFIRMATION,
                Some("ask"),
                false,
                "shell"
            ),
            "ask"
        );
        assert_eq!(
            compute_final_action(
                &MatchConfirmDenyResult::CONFIRMATION,
                Some("auto"),
                false,
                "shell"
            ),
            "auto"
        );
        assert_eq!(
            compute_final_action(
                &MatchConfirmDenyResult::CONFIRMATION,
                Some("deny"),
                false,
                "shell"
            ),
            "deny"
        );
    }

    #[test]
    fn test_always_ask_tools_override_auto() {
        assert_eq!(
            compute_final_action(
                &MatchConfirmDenyResult::PASS,
                Some("auto"),
                true,
                "compress_chat_probe"
            ),
            "ask"
        );
        assert_eq!(
            compute_final_action(&MatchConfirmDenyResult::PASS, None, true, "handoff_to_mode"),
            "auto"
        );
    }

    #[tokio::test]
    async fn tool_approved_pushed_on_decision() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        {
            let user_activity = app.buddy.user_activity.clone();
            let mut ring = user_activity.lock().await;
            ring.push(UserAction::ToolApproved {
                tool_name: "cat".to_string(),
                chat_id: "chat-1".to_string(),
                ts: Utc::now(),
            });
        }

        let user_activity = app.buddy.user_activity.clone();
        let ring = user_activity.lock().await;
        assert!(ring.snapshot().iter().any(|action| matches!(
            action,
            UserAction::ToolApproved { tool_name, chat_id, .. }
                if tool_name == "cat" && chat_id == "chat-1"
        )));
    }

    async fn make_gcx_with_buddy() -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let mut state = refact_buddy_core::state::default_buddy_state();
        state.identity.name = "Pixel".to_string();
        let service = crate::buddy::actor::BuddyService::new(
            std::env::temp_dir().join(format!("buddy-tool-voice-test-{}", uuid::Uuid::new_v4())),
            state,
            refact_buddy_core::settings::BuddySettings::default(),
            Vec::new(),
            refact_buddy_core::runtime_queue::RuntimeQueue::new(),
            tx,
            None,
        );
        let buddy_arc = app.buddy.buddy.clone();
        *buddy_arc.lock().await = Some(service);
        app
    }

    #[tokio::test]
    async fn execute_tools_uses_voice_for_runtime_event_title() {
        let (service, renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
            vec![Some("voice tool title".to_string())],
        );
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let gcx = make_gcx_with_buddy().await;

        let (title, speech_text) = tool_runtime_event_lines(gcx, "cat", "Thread").await;

        assert_eq!(title, "voice tool title");
        assert_eq!(
            speech_text.as_deref(),
            Some("Using cat to help with 'Thread'...")
        );
        assert_eq!(renderer.intent_kinds(), vec!["runtime:started".to_string()]);
    }

    #[test]
    fn tool_runtime_event_lines_falls_back_on_empty_voice() {
        let (title, speech_text) = runtime_event_lines_with_fallback(
            "   ".to_string(),
            Some("\t".to_string()),
            "Running cat in 'Thread'".to_string(),
            Some("Using cat to help with 'Thread'...".to_string()),
        );

        assert_eq!(title, "Running cat in 'Thread'");
        assert_eq!(
            speech_text.as_deref(),
            Some("Using cat to help with 'Thread'...")
        );
        assert!(!title.trim().is_empty());
    }

    #[test]
    fn tool_runtime_event_uses_short_timeout() {
        assert_eq!(
            crate::buddy::voice_service::VOICE_RUNTIME_EVENT_TIMEOUT_MS,
            1500
        );
    }
}

#[cfg(test)]
async fn tool_runtime_event_lines(
    app: AppState,
    tool_name: &str,
    chat_label: &str,
) -> (String, Option<String>) {
    let (fallback_title, fallback_speech) =
        fallback_tool_runtime_event_lines(tool_name, chat_label);
    let workflow_summary = format!("Using {} to help with '{}'...", tool_name, chat_label);
    let Some((title, speech)) = app
        .buddy_event_sink
        .render_runtime_event_fast(tool_name, &workflow_summary, "started")
        .await
    else {
        return (fallback_title, fallback_speech);
    };
    runtime_event_lines_with_fallback(title, speech, fallback_title, fallback_speech)
}

#[cfg(test)]
fn runtime_event_lines_with_fallback(
    title: String,
    speech: Option<String>,
    fallback_title: String,
    fallback_speech: Option<String>,
) -> (String, Option<String>) {
    let title = if title.trim().is_empty() {
        fallback_title
    } else {
        title
    };
    let speech = match speech {
        Some(s) if !s.trim().is_empty() => Some(s),
        _ => fallback_speech,
    };
    (title, speech)
}

#[cfg(test)]
mod interruptible_wait_tests {
    use super::*;

    #[tokio::test]
    async fn live_sleep_execution_yields_to_append_and_when_idle() {
        use crate::tools::tools_description::Tool;
        use refact_core::chat_types::{PendingDelivery, PushMode};
        for push in [PushMode::Append, PushMode::WhenIdle] {
            let gcx = crate::global_context::tests::make_test_gcx().await;
            let base_app = crate::app_state::app_state_from_gcx(gcx).await;
            let app = AppState {
                tool_registry: Arc::new(
                    crate::app_state::AppToolRegistry::with_fixture_tool_factory(
                        base_app.gcx.clone(),
                        Arc::new(|| {
                            vec![Box::new(crate::tools::tool_sleep::ToolSleep {
                                config_path: String::new(),
                            })
                                as Box<dyn crate::tools::tools_description::Tool + Send>]
                        }),
                    ),
                ),
                ..base_app
            };
            let session = Arc::new(AMutex::new(ChatSession::new("live-wait".into())));
            app.chat
                .sessions
                .write()
                .await
                .insert("live-wait".into(), session.clone());
            let ccx = Arc::new(AMutex::new(
                crate::at_commands::at_commands::at_commands_new_from_app(
                    app.clone(),
                    4096,
                    20,
                    false,
                    vec![],
                    "live-wait".into(),
                    None,
                    "model".into(),
                    None,
                    None,
                )
                .await,
            ));
            let desc = crate::tools::tool_sleep::ToolSleep {
                config_path: String::new(),
            }
            .tool_description();
            let catalog = Arc::new(ToolCatalogSnapshot {
                index: refact_runtime_api::ToolRegistryIndex {
                    tools: vec![desc],
                    mcp_lazy_mode: false,
                    mcp_total_count: 0,
                    mcp_tool_index: vec![],
                },
                policy: vec![],
                aliases: refact_tool_api::ToolAliasRegistry::new(),
            });
            let tool_call = ChatToolCall {
                id: "live-sleep".into(),
                index: Some(0),
                tool_type: "function".into(),
                function: crate::call_validation::ChatToolFunction {
                    name: "sleep".into(),
                    arguments: serde_json::json!({"duration_ms":30000,"description":"wait"})
                        .to_string(),
                },
                extra_content: None,
                started_at_ms: None,
                completed_at_ms: None,
            };
            {
                let mut locked = session.lock().await;
                locked.turn_depth = 1;
                locked.add_message(ChatMessage {
                    role: "assistant".into(),
                    tool_calls: Some(vec![tool_call.clone()]),
                    ..Default::default()
                });
            }
            let run = tokio::spawn(async move {
                execute_single_tool(
                    app,
                    ccx,
                    0,
                    tool_call,
                    catalog,
                    None,
                    Arc::new(Default::default()),
                    true,
                    1,
                    "agent",
                    None,
                )
                .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    if session.lock().await.runtime.waiting_interruptible {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("sleep did not enter wait wrapper");
            {
                let mut locked = session.lock().await;
                locked
                    .enqueue_delivery(PendingDelivery::with_id(
                        "incoming",
                        vec![ChatMessage {
                            role: "user".into(),
                            content: ChatContent::SimpleText("wake up".into()),
                            ..Default::default()
                        }],
                        push,
                        "test",
                        true,
                    ))
                    .unwrap();
            }
            let (_, corrected, messages, _) =
                tokio::time::timeout(std::time::Duration::from_secs(10), run)
                    .await
                    .unwrap()
                    .unwrap();
            assert!(!corrected);
            assert_eq!(messages[0].extra["sleep"]["interrupted"], true);
            assert!(messages[0].extra["sleep"]["reason"]
                .as_str()
                .unwrap()
                .contains("message arrived"));
            let mut locked = session.lock().await;
            assert!(!locked.abort_flag.load(Ordering::SeqCst));
            assert!(!locked.runtime.waiting_interruptible);
            for message in messages {
                locked.add_message(message);
            }
            assert_eq!(
                locked.drain_pending_deliveries(),
                (vec!["incoming".into()], true)
            );
            assert_eq!(locked.drain_pending_deliveries(), (vec![], false));
            assert_eq!(locked.messages[0].role, "assistant");
            assert_eq!(locked.messages[1].role, "tool");
            assert!(locked
                .messages
                .iter()
                .skip(2)
                .any(|message| message.content.content_text_only().contains("wake up")));
        }
    }

    #[tokio::test]
    async fn guard_clears_runtime_on_finish_and_drop() {
        let session = Arc::new(AMutex::new(ChatSession::new("wait-guard".into())));
        let mut first = InterruptibleWaitGuard::begin(session.clone()).await;
        let second = InterruptibleWaitGuard::begin(session.clone()).await;
        assert!(session.lock().await.runtime.waiting_interruptible);
        first.finish().await;
        assert!(session.lock().await.runtime.waiting_interruptible);
        drop(second);
        assert!(!session.lock().await.runtime.waiting_interruptible);
        assert_eq!(session.lock().await.interruptible_waits, 0);
    }

    #[tokio::test]
    async fn guard_cleans_up_on_panic() {
        let session = Arc::new(AMutex::new(ChatSession::new("wait-panic".into())));
        let copy = session.clone();
        assert!(tokio::spawn(async move {
            let _guard = InterruptibleWaitGuard::begin(copy).await;
            panic!("test panic");
        })
        .await
        .is_err());
        assert!(!session.lock().await.runtime.waiting_interruptible);
    }
}
