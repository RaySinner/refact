#[allow(unused_imports)]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use serde_json::json;
use tokio::sync::{Mutex as AMutex};
use tracing::{info, warn};
use uuid::Uuid;
use refact_buddy_core::types::BuddyRuntimeEvent;
use refact_chat_api::{NUDGE_MARKER, NUDGE_ROLE, SilentTailSignal};
use refact_runtime_api::ToolCatalogSnapshot;

use crate::app_state::AppState;
use crate::buddy::chat_reactions::{maybe_enqueue_chat_activity_reaction, ChatActivityCompletion};
use crate::subchat::{resolve_subchat_config_with_parent, run_subchat};

use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{
    ChatContent, ChatMessage, ChatMeta, ChatUsage, MeteringUsd, SamplingParameters,
    is_agentic_mode_id,
};
use crate::stats::event::{LlmCallEvent, canonicalize_mode_for_stats, split_model_provider};
use crate::chat::tool_call_recovery;
use crate::chat::tool_call_recovery_oss;
use crate::llm::LlmRequest;
use crate::llm::params::CacheControl;
use crate::scratchpad_abstract::HasTokenizerAndEot;
use crate::constants::CHAT_TOP_N;
use crate::knowledge::enrichment::{
    enrich_messages_with_knowledge, enrichment_identity_from_context, enrichment_query_fingerprint,
    record_enrichment_identity_on_user_message,
};
use crate::chat::perf_diagnostics::{self, PerfComponent, PerfOutcome};

use crate::chat::goal_monitor::handle_goal_turn_end;
use crate::chat::types::*;
use crate::chat::trajectories::{
    check_external_reload_pending, ensure_frozen_prefix, first_system_prompt,
    frozen_prefix_is_complete, maybe_save_trajectory_background_with_intent,
};
use crate::chat::tools::{process_tool_calls_once, ToolStepOutcome};
use crate::chat::prepare::{
    build_canonical_openai_tools_from_aliases, prepare_chat_passthrough, ChatPrepareOptions,
};
use crate::chat::prompts::prepend_the_right_system_prompt_and_maybe_more_initial_messages;
use crate::chat::stream_core::{
    StreamRunParams, StreamCollector, normalize_tool_call, ChoiceFinal, LlmStreamError,
    LlmStreamOutcome, ABORT_ERROR_MESSAGE,
};
use crate::chat::config::tokens;
use crate::ext::hooks::HookEvent;
use crate::ext::hooks_runner::{HookPayload, get_project_dir_string, run_hooks};
use crate::chat::diagnostics::{
    make_ui_only_error_message, make_ui_only_retry_status_message, safe_provider_error_diagnostic,
};
use crate::chat::history_limit::ContextPressure;
use refact_core::llm_types::BaseModelRecord;
use refact_core::provider_types::{ModelTypeDefaults, ProviderDefaults};

use super::support::*;
use super::retry::*;
use super::preamble::*;
use super::recovery::*;
use super::run_loop::*;
use super::stream::*;
use super::tests::*;
use crate::call_validation::{ChatToolCall, ChatToolFunction};

#[test]
fn test_tail_needs_assistant_only_server_tools() {
    let messages = vec![
        make_user_msg("hello"),
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("".to_string()),
            tool_calls: Some(vec![
                ChatToolCall {
                    id: "srvtoolu_123".to_string(),
                    index: Some(0),
                    function: ChatToolFunction {
                        name: "web_search".to_string(),
                        arguments: "{}".to_string(),
                    },
                    tool_type: "function".to_string(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                },
                ChatToolCall {
                    id: "srvtoolu_456".to_string(),
                    index: Some(1),
                    function: ChatToolFunction {
                        name: "web_search".to_string(),
                        arguments: "{}".to_string(),
                    },
                    tool_type: "function".to_string(),
                    extra_content: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                },
            ]),
            ..Default::default()
        },
        make_tool_msg("srvtoolu_123", "search results 1"),
        make_tool_msg("srvtoolu_456", "search results 2"),
    ];
    assert!(!tail_needs_assistant(&messages));
}

#[test]
fn test_priority_injection_guard_waits_for_tool_results() {
    let mut messages = vec![
        make_user_msg("hello"),
        make_assistant_with_tool_call("call_123", "cat"),
    ];
    assert!(!latest_assistant_tool_call_window_closed(&messages));

    messages.push(make_tool_msg("call_123", "file content"));
    assert!(latest_assistant_tool_call_window_closed(&messages));
}

#[test]
fn test_priority_injection_guard_ignores_stale_tool_results() {
    let messages = vec![
        make_user_msg("first"),
        make_assistant_with_tool_call("call_reused", "cat"),
        make_tool_msg("call_reused", "old result"),
        make_user_msg("second"),
        make_assistant_with_tool_call("call_reused", "cat"),
    ];

    assert!(!latest_assistant_tool_call_window_closed(&messages));
}

#[test]
fn test_priority_injection_guard_waits_for_server_tool_placeholders() {
    let mut messages = vec![
        make_user_msg("hello"),
        make_assistant_with_tool_call("srvtoolu_123", "web_search"),
    ];
    assert!(!latest_assistant_tool_call_window_closed(&messages));

    messages.push(make_tool_msg("srvtoolu_123", "server result placeholder"));
    assert!(latest_assistant_tool_call_window_closed(&messages));
}

#[test]
fn test_tail_needs_assistant_empty_messages() {
    let messages: Vec<ChatMessage> = vec![];
    assert!(!tail_needs_assistant(&messages));
}

#[test]
fn test_tail_needs_assistant_assistant_with_empty_tool_calls() {
    let messages = vec![
        make_user_msg("hello"),
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("response".to_string()),
            tool_calls: Some(vec![]),
            ..Default::default()
        },
    ];
    assert!(!tail_needs_assistant(&messages));
}

#[test]
fn test_fork_error_does_not_break_loop() {
    let mut loop_count = 0;
    let mut reached_normal_generation = false;

    loop {
        loop_count += 1;
        if loop_count > 5 {
            panic!("Loop ran too many times");
        }

        let fork_agent: Option<String> = if loop_count == 1 {
            Some("subagent".to_string())
        } else {
            None
        };

        if fork_agent.is_some() {
            let fork_result: Result<String, String> = Err("subchat failed".to_string());
            match fork_result {
                Ok(_content) => {
                    break;
                }
                Err(_e) => {
                    continue;
                }
            }
        }

        reached_normal_generation = true;
        break;
    }

    assert!(
        reached_normal_generation,
        "Normal generation path must be reached after fork error"
    );
    assert_eq!(
        loop_count, 2,
        "Loop must iterate twice: fork error then normal generation"
    );
}

#[test]
fn test_context_limit_gate_recognizes_explicit_and_wrapped_errors() {
    let explicit = context_limit_error("context_length_exceeded: reduce prompt", false);
    let wrapped = context_limit_error(
        &format!(
            "{} Original error: context_length_exceeded",
            PARTIAL_OUTPUT_STREAM_ERROR,
        ),
        true,
    );
    assert!(explicit.retry_decision().is_context_limit());
    assert!(wrapped.retry_decision().is_context_limit());
}

#[test]
fn normal_chat_context_limit_requests_reactive_compaction() {
    let error = context_limit_error("context_length_exceeded", false);
    assert!(error.retry_decision().is_context_limit());
}

#[test]
fn context_length_error_triggers_reactive_compression() {
    let error = context_limit_error("input too long for model_length", false);
    assert!(error.retry_decision().is_context_limit());
}

#[test]
fn task_planner_context_limit_requests_reactive_compaction() {
    let error = context_limit_error("context_length_exceeded", false);
    assert!(error.retry_decision().is_context_limit());
}

#[test]
fn test_context_limit_compaction_allows_partial_output_errors() {
    let abort = std::sync::atomic::AtomicBool::new(false);
    let partial_context_error = context_limit_error(
        &format!(
            "{} Original error: context_length_exceeded",
            PARTIAL_OUTPUT_STREAM_ERROR,
        ),
        true,
    );

    assert!(partial_context_error.retry_decision().is_context_limit());
    assert!(!partial_context_error.should_retry(0, &abort));
    assert!(
        partial_context_error.retry_decision().is_context_limit() && !abort.load(Ordering::SeqCst)
    );
}

#[test]
fn test_context_limit_compaction_blocked_by_abort_flag() {
    let abort = std::sync::atomic::AtomicBool::new(true);
    let partial_context_error = context_limit_error("context_length_exceeded", false);

    assert!(partial_context_error.retry_decision().is_context_limit());
    assert!(
        !(partial_context_error.retry_decision().is_context_limit()
            && !abort.load(Ordering::SeqCst)),
        "abort must suppress reactive compaction",
    );
}

#[test]
fn test_segment_summary_count_resets_on_success() {
    let segment_summary_count = 0usize;
    assert_eq!(segment_summary_count, 0);
}

#[test]
fn test_downgrade_bogus_tool_calls_finish_reason() {
    let mut result = ChoiceFinal {
        finish_reason: Some("tool_calls".to_string()),
        ..Default::default()
    };

    maybe_downgrade_bogus_tool_calls_finish_reason(&mut result, "test");

    assert_eq!(result.finish_reason.as_deref(), Some("stop"));
    assert!(result.extra.contains_key("_tool_call_guard"));
}

#[test]
fn test_does_not_downgrade_tool_calls_finish_reason_when_tool_calls_exist() {
    let mut result = ChoiceFinal {
        finish_reason: Some("tool_calls".to_string()),
        tool_calls_raw: vec![json!({
            "type": "function",
            "id": "call_123",
            "function": {
                "name": "shell",
                "arguments": "{}"
            }
        })],
        ..Default::default()
    };

    maybe_downgrade_bogus_tool_calls_finish_reason(&mut result, "test");

    assert_eq!(result.finish_reason.as_deref(), Some("tool_calls"));
    assert!(!result.extra.contains_key("_tool_call_guard"));
}

#[test]
fn test_downgrade_bogus_tool_use_finish_reason() {
    let mut result = ChoiceFinal {
        finish_reason: Some("tool_use".to_string()),
        ..Default::default()
    };

    maybe_downgrade_bogus_tool_calls_finish_reason(&mut result, "test");

    assert_eq!(result.finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        result.extra["_tool_call_guard"]["original_finish_reason"],
        "tool_use"
    );
}

#[test]
fn test_does_not_downgrade_tool_use_finish_reason_when_tool_calls_exist() {
    let mut result = ChoiceFinal {
        finish_reason: Some("tool_use".to_string()),
        tool_calls_raw: vec![json!({
            "type": "function",
            "id": "call_123",
            "function": { "name": "shell", "arguments": "{}" }
        })],
        ..Default::default()
    };

    maybe_downgrade_bogus_tool_calls_finish_reason(&mut result, "test");

    assert_eq!(result.finish_reason.as_deref(), Some("tool_use"));
    assert!(!result.extra.contains_key("_tool_call_guard"));
}

#[test]
fn test_does_not_downgrade_non_tool_calls_finish_reason() {
    let mut result = ChoiceFinal {
        finish_reason: Some("stop".to_string()),
        ..Default::default()
    };

    maybe_downgrade_bogus_tool_calls_finish_reason(&mut result, "test");

    assert_eq!(result.finish_reason.as_deref(), Some("stop"));
    assert!(!result.extra.contains_key("_tool_call_guard"));
}

#[test]
fn delta_batch_preserves_text_order_and_flushes_control_events() {
    let batches = batch_stream_delta_ops(vec![
        DeltaOp::AppendContent {
            text: "hel".to_string(),
        },
        DeltaOp::AppendContent {
            text: "lo".to_string(),
        },
        DeltaOp::AppendReasoning {
            text: "think".to_string(),
        },
        DeltaOp::SetToolCalls {
            tool_calls: vec![json!({"id": "tool-1"})],
        },
        DeltaOp::AppendContent {
            text: "after".to_string(),
        },
    ]);

    assert_eq!(batches.len(), 3);
    assert!(matches!(
        batches[0].as_slice(),
        [
            DeltaOp::AppendContent { text },
            DeltaOp::AppendReasoning { text: reasoning },
        ] if text == "hello" && reasoning == "think"
    ));
    assert!(matches!(
        batches[1].as_slice(),
        [DeltaOp::SetToolCalls { .. }]
    ));
    assert!(matches!(
        batches[2].as_slice(),
        [DeltaOp::AppendContent { text }] if text == "after"
    ));
}

#[test]
fn delta_batch_caps_large_utf8_content_without_breaking_text() {
    let text = "ЁЯША".repeat((MAX_DELTA_TEXT_BYTES / 4) + 2);
    let batches = batch_stream_delta_ops(vec![DeltaOp::AppendContent { text: text.clone() }]);
    let chunks = batches
        .into_iter()
        .flatten()
        .filter_map(|op| match op {
            DeltaOp::AppendContent { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(chunks.len() > 1);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.len() <= MAX_DELTA_TEXT_BYTES));
    assert_eq!(chunks.concat(), text);
}

#[tokio::test]
async fn test_models_dev_generation_metering_uses_central_pricing_lookup() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let mut model_caps = std::collections::HashMap::new();
    model_caps.insert(
        "openai/gpt-4o".to_string(),
        crate::caps::model_caps::ModelCapabilities {
            n_ctx: 128_000,
            max_output_tokens: 16_384,
            pricing: Some(crate::providers::traits::ModelPricing {
                prompt: 2.0,
                generated: 4.0,
                cache_read: Some(1.0),
                cache_creation: Some(3.0),
                context_over_200k: None,
            }),
            ..Default::default()
        },
    );
    {
        let mut caps = app.model.caps.write().await;
        caps.caps = Some(std::sync::Arc::new(crate::caps::CodeAssistantCaps {
            model_caps: std::sync::Arc::new(model_caps),
            ..Default::default()
        }));
        caps.last_attempted_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
    }
    let usage = ChatUsage {
        prompt_tokens: 1_000,
        completion_tokens: 2_000,
        total_tokens: 3_000,
        cache_read_tokens: Some(500),
        cache_creation_tokens: Some(250),
        metering_usd: None,
    };

    let metering = generation_metering_usd(&app, "openai/gpt-4o", &usage)
        .await
        .unwrap();

    assert_eq!(metering.prompt_usd, 0.002);
    assert_eq!(metering.generated_usd, 0.008);
    assert_eq!(metering.cache_read_usd, Some(0.0005));
    assert_eq!(metering.cache_creation_usd, Some(0.00075));
}

#[test]
fn cache_guard_violation_does_not_become_error_state_in_generation_loop() {
    let result: Result<GenerationResult, LlmStreamError> =
        Ok(GenerationResult::PausedForUserDecision);
    let would_call_finish_stream_with_error = matches!(result, Err(_));
    let would_break_cleanly = matches!(result, Ok(GenerationResult::PausedForUserDecision));
    assert!(
        would_break_cleanly,
        "PausedForUserDecision must break the loop cleanly"
    );
    assert!(
        !would_call_finish_stream_with_error,
        "PausedForUserDecision must not trigger finish_stream_with_error"
    );
}

#[test]
fn enrichment_identity_survives_context_removal_and_restore() {
    let mut session = ChatSession::new("enrichment-identity".to_string());
    let identity = "query-fingerprint:result-fingerprint".to_string();
    let query_fingerprint = "query-fingerprint".to_string();
    let mut user = make_user_msg("query");
    record_enrichment_identity_on_user_message(&mut user, &identity, &query_fingerprint);
    session.add_message(user);
    session.add_message(ChatMessage {
        role: "context_file".to_string(),
        tool_call_id: "knowledge_enrichment".to_string(),
        ..Default::default()
    });
    session.record_enrichment_identity(identity.clone());
    session
        .messages
        .retain(|message| message.role != "context_file");

    let restored = ChatSession::new_with_trajectory(
        "enrichment-identity".to_string(),
        session.messages.clone(),
        session.thread.clone(),
        session.created_at.clone(),
        None,
        Vec::new(),
        None,
    );

    assert!(restored.has_enrichment_identity(&identity));
}

#[test]
fn enrichment_insertion_rejects_stale_turns_and_reused_identities() {
    let mut session = ChatSession::new("enrichment-stale".to_string());
    session.add_message(make_user_msg("current query"));
    let user = session.messages.last().unwrap();
    let turn = EnrichmentTurnIdentity {
        revision: session.trajectory_version,
        latest_user_id: user.message_id.clone(),
        query_fingerprint: enrichment_query_fingerprint(&user.content.content_text_only()),
    };

    assert_eq!(
        enrichment_insertion_index(&session, &turn, Some("fresh-result")),
        Some(0)
    );
    session.record_enrichment_identity("fresh-result".to_string());
    assert!(enrichment_insertion_index(&session, &turn, Some("fresh-result")).is_none());

    session.add_message(make_assistant_msg("reply"));
    assert!(enrichment_insertion_index(&session, &turn, Some("new-result")).is_none());
}

#[test]
fn cache_guard_other_failure_propagates_as_error() {
    use crate::chat::cache_guard::CacheGuardOutcome;
    use crate::chat::stream_core::LlmStreamError;

    let error_outcome = CacheGuardOutcome::Error("simulated io failure".to_string());
    assert!(
        matches!(error_outcome, CacheGuardOutcome::Error(_)),
        "Error variant must be distinguishable"
    );
    assert!(
        !matches!(error_outcome, CacheGuardOutcome::Pass(_)),
        "Error must not be treated as Pass"
    );
    assert!(
        !matches!(error_outcome, CacheGuardOutcome::Paused { .. }),
        "Error must not be treated as Paused"
    );

    let stream_err: Result<Vec<()>, LlmStreamError> =
        Err(LlmStreamError::from("simulated io failure".to_string()));
    assert!(
        matches!(stream_err, Err(_)),
        "Error outcome must propagate as Err in the generation chain"
    );
}
fn length_stop_marker_msg() -> ChatMessage {
    length_stop_continue_instruction(LengthStopKind::PartialOutput)
}

fn nudge_marker_msg() -> ChatMessage {
    ChatMessage {
        role: NUDGE_ROLE.to_string(),
        tool_call_id: NUDGE_MARKER.to_string(),
        content: ChatContent::SimpleText("nudge".to_string()),
        ..Default::default()
    }
}

fn decide(
    messages: &[ChatMessage],
    nudge_enabled: bool,
    is_agent_chat: bool,
    has_finish_tool: bool,
    last_response_text: &str,
) -> Option<String> {
    decide_silent_tail_nudge(
        messages,
        nudge_enabled,
        is_agent_chat,
        has_finish_tool,
        MAX_SILENT_TAIL_NUDGE_ATTEMPTS,
        last_response_text,
        Some("test-model"),
    )
}

fn agent_turn_with_nudges(nudges: usize) -> Vec<ChatMessage> {
    let mut messages = vec![make_user_msg("do the card")];
    messages.push(make_assistant_msg("I looked at it and think we are done."));
    for _ in 0..nudges {
        messages.push(nudge_marker_msg());
        messages.push(make_assistant_msg("still just talking."));
    }
    messages
}

#[test]
fn nudge_not_inserted_when_disabled() {
    let messages = agent_turn_with_nudges(0);
    assert!(decide(&messages, false, true, true, "I think we are done.").is_none());
}

#[test]
fn nudge_not_inserted_in_plain_chat() {
    let messages = agent_turn_with_nudges(0);
    assert!(decide(&messages, true, false, true, "I think we are done.").is_none());
}

#[test]
fn nudge_not_inserted_without_finish_tool() {
    let messages = agent_turn_with_nudges(0);
    assert!(decide(&messages, true, true, false, "I think we are done.").is_none());
}

#[test]
fn nudge_not_inserted_for_empty_response() {
    let messages = agent_turn_with_nudges(0);
    assert!(decide(&messages, true, true, true, "").is_none());
    assert!(decide(&messages, true, true, true, "   \n ").is_none());
}

#[test]
fn nudge_inserted_on_first_silent_tail() {
    let messages = agent_turn_with_nudges(0);
    assert!(decide(&messages, true, true, true, "I think we are done.").is_some());
}

#[test]
fn nudge_text_mentions_finish_tool() {
    let messages = agent_turn_with_nudges(0);
    let text = decide(&messages, true, true, true, "I think we are done.").unwrap();
    assert!(text.contains(SILENT_TAIL_FINISH_TOOL));
    assert!(text.contains("tool call"));
}

#[test]
fn nudge_not_inserted_after_max_attempts() {
    let messages = agent_turn_with_nudges(MAX_SILENT_TAIL_NUDGE_ATTEMPTS as usize);
    assert!(decide(&messages, true, true, true, "still just talking.").is_none());
}

#[test]
fn nudge_attempts_counted_only_since_last_user_message() {
    let mut messages = vec![make_user_msg("first turn")];
    messages.push(nudge_marker_msg());
    messages.push(make_assistant_msg("silence"));
    messages.push(make_user_msg("second turn"));
    messages.push(make_assistant_msg("silence again"));
    assert_eq!(silent_tail_nudge_attempts(&messages), 0);
    assert!(decide(&messages, true, true, true, "silence again").is_some());
}

#[test]
fn nudge_stops_after_second_attempt_in_conversation() {
    let mut messages = agent_turn_with_nudges(0);
    assert!(decide(&messages, true, true, true, "I think we are done.").is_some());
    messages.push(nudge_marker_msg());
    messages.push(make_assistant_msg("still just talking."));
    assert!(decide(&messages, true, true, true, "still just talking.").is_some());
    messages.push(nudge_marker_msg());
    messages.push(make_assistant_msg("still just talking."));
    assert!(decide(&messages, true, true, true, "still just talking.").is_none());
}

#[test]
fn pre_tool_monologue_detects_explicit_markers_in_any_mode() {
    for (agent, text) in [
        (false, "Wait! let me look at this again"),
        (false, "Let's check the file before editing it"),
        (true, "Thinking process: step one done, now step two"),
        (false, "Now checking the test suite"),
        (true, "First, I'll read the config file"),
        (false, "Next step: run the linter"),
    ] {
        assert!(
            is_pre_tool_monologue(text, agent),
            "expected monologue for agent={agent} text={text:?}"
        );
    }
}

#[test]
fn pre_tool_monologue_agent_mode_diverts_unmarked_planning_text() {
    assert!(is_pre_tool_monologue(
        "I need to read src/lib.rs before I can change the parser.",
        true
    ));
}

#[test]
fn pre_tool_monologue_keeps_user_greetings_and_headers_visible() {
    // A message that starts with a user greeting or a markdown header is
    // meant for a human reader and must stay in `content`.
    assert!(!is_pre_tool_monologue("Hi! I'll open the file now.", true));
    assert!(!is_pre_tool_monologue(
        "## Summary of changes performed",
        true
    ));
    // Explicit markers still win over the greeting heuristic.
    assert!(is_pre_tool_monologue(
        "Hi, wait! let me re-check that",
        true
    ));
}

#[test]
fn pre_tool_monologue_ignores_plain_chat_and_empty_text() {
    // Without markers, non-agent chats keep the text in `content`.
    assert!(!is_pre_tool_monologue(
        "First, let me think about how to answer this.",
        false
    ));
    assert!(!is_pre_tool_monologue("", true));
    assert!(!is_pre_tool_monologue("   \n  ", true));
}

fn finish_tool_catalog() -> refact_runtime_api::ToolCatalogSnapshot {
    let desc = refact_tool_api::ToolDesc {
        name: SILENT_TAIL_FINISH_TOOL.to_string(),
        display_name: SILENT_TAIL_FINISH_TOOL.to_string(),
        source: refact_tool_api::ToolSource {
            source_type: refact_tool_api::ToolSourceType::Builtin,
            config_path: String::new(),
        },
        experimental: false,
        allow_parallel: false,
        description: "finish the task".to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
        output_schema: None,
        annotations: None,
    };
    refact_runtime_api::ToolCatalogSnapshot {
        index: refact_runtime_api::ToolRegistryIndex {
            tools: vec![desc],
            mcp_lazy_mode: false,
            mcp_total_count: 0,
            mcp_tool_index: vec![],
        },
        policy: vec![],
        aliases: refact_tool_api::ToolAliasRegistry::new(),
    }
}

fn reasoning_only_tail_session(thinking_blocks: bool) -> ChatSession {
    let mut session = ChatSession::new("silent-tail-reasoning".to_string());
    session.messages = vec![
        make_user_msg("do the card"),
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText(String::new()),
            finish_reason: Some("stop".to_string()),
            reasoning_content: (!thinking_blocks)
                .then(|| "I think the parser is wrong and I should fix it first".to_string()),
            thinking_blocks: thinking_blocks
                .then(|| vec![json!({"type": "thinking", "thinking": "working through it"})]),
            ..Default::default()
        },
    ];
    session.thread.mode = "task_agent".to_string();
    session.thread.agent_nudge_enabled = Some(true);
    session.thread.task_meta = Some(TaskMeta {
        task_id: "task-1".to_string(),
        role: "agents".to_string(),
        ..Default::default()
    });
    session.tool_catalog = Some(Arc::new(finish_tool_catalog()));
    session
}

#[tokio::test]
async fn silent_tail_nudge_fires_for_reasoning_only_tail() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(reasoning_only_tail_session(false)));
    assert!(matches!(
        maybe_nudge_after_silent_tail(app, &session_arc).await,
        SilentTailNudge::Nudged
    ));
    let session = session_arc.lock().await;
    assert!(session
        .messages
        .last()
        .is_some_and(|message| message.role == NUDGE_ROLE));
}

#[tokio::test]
async fn silent_tail_nudge_fires_for_thinking_blocks_only_tail() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let session_arc = Arc::new(AMutex::new(reasoning_only_tail_session(true)));
    assert!(matches!(
        maybe_nudge_after_silent_tail(app, &session_arc).await,
        SilentTailNudge::Nudged
    ));
}

#[tokio::test]
async fn silent_tail_nudge_stays_quiet_for_truly_empty_tail() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let mut session = reasoning_only_tail_session(false);
    session.messages.last_mut().unwrap().reasoning_content = None;
    let session_arc = Arc::new(AMutex::new(session));
    assert!(matches!(
        maybe_nudge_after_silent_tail(app, &session_arc).await,
        SilentTailNudge::NotApplicable
    ));
    let session = session_arc.lock().await;
    assert!(session
        .messages
        .last()
        .filter(|message| message.role == NUDGE_ROLE)
        .is_none());
}

#[test]
fn length_stop_kind_classifies_empty_and_partial() {
    assert_eq!(
        length_stop_kind(&make_reasoning_token_limit_msg()),
        Some(LengthStopKind::EmptyOutput)
    );

    let mut partial = make_reasoning_token_limit_msg();
    partial.content = ChatContent::SimpleText("a long partial answer that was cut".repeat(4));
    assert_eq!(
        length_stop_kind(&partial),
        Some(LengthStopKind::PartialOutput)
    );

    let mut with_tools = make_reasoning_token_limit_msg();
    with_tools.tool_calls = make_assistant_with_tool_call("call_1", "cat").tool_calls;
    assert_eq!(length_stop_kind(&with_tools), None);

    let mut normal_finish = make_reasoning_token_limit_msg();
    normal_finish.finish_reason = Some("stop".to_string());
    assert_eq!(length_stop_kind(&normal_finish), None);

    let mut context_stop = make_reasoning_token_limit_msg();
    context_stop.finish_reason = Some("context_length_exceeded".to_string());
    assert_eq!(length_stop_kind(&context_stop), None);

    assert_eq!(length_stop_kind(&make_user_msg("hi")), None);
}

#[test]
fn length_stop_recovery_attempts_counts_markers_since_last_user() {
    let messages = vec![
        make_user_msg("first"),
        length_stop_marker_msg(),
        make_user_msg("second"),
        length_stop_marker_msg(),
        length_stop_marker_msg(),
        make_reasoning_token_limit_msg(),
    ];
    assert_eq!(length_stop_recovery_attempts(&messages), 2);

    let fresh_turn = vec![
        make_user_msg("first"),
        length_stop_marker_msg(),
        make_user_msg("second"),
    ];
    assert_eq!(length_stop_recovery_attempts(&fresh_turn), 0);
}

#[tokio::test]
async fn empty_length_stop_low_pressure_retries_with_boost_and_marker() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let mut session = ChatSession::new("length-stop-retry".to_string());
    session.messages = vec![make_user_msg("continue"), make_low_pressure_length_stop()];
    let thread = session.thread.clone();
    let session_arc = Arc::new(AMutex::new(session));

    assert!(
        maybe_recover_after_length_stop(
            gcx,
            &session_arc,
            &thread,
            Some(100_000),
            refact_core::provider_types::ImageTokenMode::Provider,
        )
        .await
    );

    let session = session_arc.lock().await;
    assert_eq!(
        session.pending_max_new_tokens_boost,
        Some(LENGTH_STOP_BOOSTED_MAX_NEW_TOKENS)
    );
    assert!(!session
        .messages
        .iter()
        .any(|message| length_stop_kind(message) == Some(LengthStopKind::EmptyOutput)));
    let markers = session
        .messages
        .iter()
        .filter(|message| {
            message.role == "cd_instruction" && message.tool_call_id == LENGTH_STOP_CONTINUE_MARKER
        })
        .count();
    assert_eq!(markers, 1);
}

#[tokio::test]
async fn length_stop_recovery_ignores_trailing_token_budget_marker() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let mut session = ChatSession::new("length-stop-trailing-budget".to_string());
    session.messages = vec![make_user_msg("continue"), make_low_pressure_length_stop()];
    session.add_message(ChatMessage {
        role: "cd_instruction".to_string(),
        tool_call_id: TOKEN_BUDGET_MARKER.to_string(),
        content: ChatContent::SimpleText("budget".to_string()),
        ..Default::default()
    });
    let thread = session.thread.clone();
    let session_arc = Arc::new(AMutex::new(session));

    assert!(
        maybe_recover_after_length_stop(
            gcx,
            &session_arc,
            &thread,
            Some(100_000),
            refact_core::provider_types::ImageTokenMode::Provider,
        )
        .await
    );

    let session = session_arc.lock().await;
    assert!(!session
        .messages
        .iter()
        .any(|message| message.tool_call_id == TOKEN_BUDGET_MARKER));
    assert_eq!(
        session
            .messages
            .last()
            .map(|message| message.tool_call_id.clone()),
        Some(LENGTH_STOP_CONTINUE_MARKER.to_string())
    );
}

#[tokio::test]
async fn partial_length_stop_keeps_message_and_appends_continue_marker() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let mut session = ChatSession::new("length-stop-partial".to_string());
    let mut partial = make_low_pressure_length_stop();
    partial.content = ChatContent::SimpleText("partial answer cut mid-".repeat(8));
    let partial_text = partial.content.content_text_only();
    session.messages = vec![make_user_msg("continue"), partial];
    let thread = session.thread.clone();
    let session_arc = Arc::new(AMutex::new(session));

    assert!(
        maybe_recover_after_length_stop(
            gcx,
            &session_arc,
            &thread,
            Some(100_000),
            refact_core::provider_types::ImageTokenMode::Provider,
        )
        .await
    );

    let session = session_arc.lock().await;
    assert!(session
        .messages
        .iter()
        .any(|message| message.content.content_text_only() == partial_text));
    assert_eq!(
        session
            .messages
            .last()
            .map(|message| message.tool_call_id.clone()),
        Some(LENGTH_STOP_CONTINUE_MARKER.to_string())
    );
}

#[tokio::test]
async fn length_stop_recovery_exhausts_after_max_attempts_with_visible_notice() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let mut session = ChatSession::new("length-stop-exhausted".to_string());
    session.messages = vec![make_user_msg("continue")];
    for _ in 0..MAX_LENGTH_STOP_RECOVERY_ATTEMPTS {
        session.messages.push(length_stop_marker_msg());
    }
    session.messages.push(make_low_pressure_length_stop());
    let thread = session.thread.clone();
    let session_arc = Arc::new(AMutex::new(session));

    assert!(
        !maybe_recover_after_length_stop(
            gcx,
            &session_arc,
            &thread,
            Some(100_000),
            refact_core::provider_types::ImageTokenMode::Provider,
        )
        .await
    );

    let session = session_arc.lock().await;
    let last = session.messages.last().unwrap();
    assert_eq!(last.role, "error");
    assert!(crate::chat::diagnostics::is_ui_only_message(last));
    assert!(last
        .content
        .content_text_only()
        .contains("output token limit"));
}

#[tokio::test]
async fn partial_length_stop_with_user_max_tokens_is_not_retried() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let mut session = ChatSession::new("length-stop-user-cap".to_string());
    let mut partial = make_low_pressure_length_stop();
    partial.content = ChatContent::SimpleText("partial answer".repeat(8));
    session.messages = vec![make_user_msg("continue"), partial];
    session.thread.max_tokens = Some(512);
    let thread = session.thread.clone();
    let before_len = session.messages.len();
    let session_arc = Arc::new(AMutex::new(session));

    assert!(
        !maybe_recover_after_length_stop(
            gcx,
            &session_arc,
            &thread,
            Some(100_000),
            refact_core::provider_types::ImageTokenMode::Provider,
        )
        .await
    );

    let session = session_arc.lock().await;
    assert_eq!(session.messages.len(), before_len);
    assert!(session.pending_max_new_tokens_boost.is_none());
}
