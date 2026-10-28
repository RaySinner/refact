#[allow(unused_imports)]
use std::sync::Arc;
use serde_json::json;
use tokio::sync::{Mutex as AMutex};

use crate::app_state::AppState;

use crate::call_validation::{ChatContent, ChatMessage, ChatUsage};
use crate::scratchpad_abstract::HasTokenizerAndEot;

use crate::chat::types::*;
use crate::chat::trajectories::first_system_prompt;
use crate::chat::stream_core::{StreamCollector, LlmStreamError};
use crate::chat::config::tokens;
use refact_core::llm_types::BaseModelRecord;
use refact_core::provider_types::{ModelTypeDefaults, ProviderDefaults};

use super::support::*;
use super::retry::*;
use super::preamble::*;
use super::recovery::*;
use super::run_loop::*;
use super::stream::*;
use crate::call_validation::{ChatToolCall, ChatToolFunction};

#[tokio::test]
async fn completion_side_effects_do_not_block_generation_on_buddy_lock() {
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx).await;
    let buddy_guard = app.buddy.buddy.lock().await;
    let session_arc = Arc::new(AMutex::new(ChatSession::new(
        "completion-side-effects".to_string(),
    )));

    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        spawn_chat_completion_side_effects(
            app.clone(),
            session_arc.clone(),
            "completion-side-effects".to_string(),
            "Completion Side Effects".to_string(),
            false,
        ),
    )
    .await
    .expect("completion side effects must detach before waiting for Buddy");

    assert_eq!(session_arc.lock().await.post_turn_task_handles.len(), 1);
    drop(buddy_guard);
    session_arc.lock().await.close_event_channel();
}

pub(in crate::chat::generation) fn make_user_msg(content: &str) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: ChatContent::SimpleText(content.to_string()),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn make_event_msg(content: &str) -> ChatMessage {
    crate::chat::internal_roles::event(
        crate::chat::internal_roles::EventSubkind::SystemNotice,
        "test.generation",
        serde_json::json!({}),
        content.to_string(),
    )
}

pub(in crate::chat::generation) fn make_assistant_msg(content: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: ChatContent::SimpleText(content.to_string()),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn make_reasoning_token_limit_msg() -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: ChatContent::SimpleText(String::new()),
        finish_reason: Some("length".to_string()),
        reasoning_content: Some("still thinking".to_string()),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn high_pressure_usage() -> ChatUsage {
    ChatUsage {
        prompt_tokens: 85_000,
        completion_tokens: 0,
        total_tokens: 85_000,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        metering_usd: None,
    }
}

pub(in crate::chat::generation) fn make_high_pressure_length_stop() -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: ChatContent::SimpleText(String::new()),
        finish_reason: Some("length".to_string()),
        reasoning_content: Some("still thinking".to_string()),
        usage: Some(high_pressure_usage()),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn make_low_pressure_length_stop() -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: ChatContent::SimpleText(String::new()),
        finish_reason: Some("length".to_string()),
        usage: Some(ChatUsage {
            prompt_tokens: 100,
            completion_tokens: 0,
            total_tokens: 100,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            metering_usd: None,
        }),
        ..Default::default()
    }
}

#[test]
fn mcp_lazy_hint_is_inserted_once_without_inlining_the_catalog() {
    let mut session = ChatSession::new("mcp-lazy".to_string());
    session.add_message(ChatMessage::new("system".to_string(), "system".to_string()));
    session.add_message(make_user_msg("hello"));

    assert!(inject_mcp_lazy_hint(&mut session, 52));
    assert!(!inject_mcp_lazy_hint(&mut session, 52));

    let hint = &session.messages[1];
    assert_eq!(hint.role, "cd_instruction");
    assert_eq!(hint.tool_call_id, MCP_LAZY_INDEX_MARKER);
    let text = hint.content.content_text_only();
    assert!(text.contains("52 tools available"));
    assert!(text.contains("mcp_tool_search"));
    assert!(text.contains("\"query\": \".\""));
    assert!(text.contains("\"max_results\": 52"));
    assert!(!text.contains("Available MCP tools"));
    assert!(text.len() < 300);
}

#[test]
fn goal_budget_pursuit_usage_requires_marker() {
    let mut session = ChatSession::new("goal-generation".to_string());
    session.install_goal("agent", "ship it", true, GoalBudget::default());
    session.add_message(make_assistant_msg("not a pursuit"));
    session.messages.last_mut().unwrap().usage = Some(ChatUsage {
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        metering_usd: None,
    });
    assert!(latest_goal_pursuit_usage(&session).is_none());

    session.messages.last_mut().unwrap().extra.insert(
        "goal_pursuit".to_string(),
        json!({"account_progress": true}),
    );
    let usage = latest_goal_pursuit_usage(&session).unwrap();
    assert_eq!(usage.total_tokens, 15);
    assert!(session.goal_record_progress_from_usage(&usage));
    assert_eq!(session.goal.as_ref().unwrap().progress.turns_used, 1);
    assert_eq!(session.goal.as_ref().unwrap().progress.tokens_used, 15);
    assert_eq!(session.goal.as_ref().unwrap().progress.no_progress_turns, 1);
}

#[test]
fn goal_budget_pursuit_usage_records_expiring_turn() {
    let mut session = ChatSession::new("goal-generation-expiring".to_string());
    session.install_goal(
        "agent",
        "ship it",
        true,
        GoalBudget {
            max_turns: Some(10),
            max_minutes: Some(1),
            max_tokens: Some(1_000),
            max_cost_cents: None,
            cooldown_ms: 1_500,
            no_progress_token_threshold: 10,
            no_progress_turns: Some(2),
            explicit: false,
        },
    );
    session.goal.as_mut().unwrap().progress.started_at_ms = 1;
    let mut assistant = make_assistant_msg("late pursuit");
    assistant.usage = Some(ChatUsage {
        prompt_tokens: 10,
        completion_tokens: 15,
        total_tokens: 25,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        metering_usd: None,
    });
    assistant.extra.insert(
        "goal_pursuit".to_string(),
        json!({"account_progress": true}),
    );
    session.add_message(assistant);

    let usage = latest_goal_pursuit_usage(&session).unwrap();
    assert!(session.goal_record_progress_from_usage(&usage));
    assert_eq!(session.goal.as_ref().unwrap().progress.turns_used, 1);
    assert_eq!(session.goal.as_ref().unwrap().progress.tokens_used, 25);
    assert_eq!(session.goal_status, Some(GoalStatus::BudgetExhausted));
}

pub(in crate::chat::generation) fn make_assistant_with_tool_call(
    tool_call_id: &str,
    tool_name: &str,
) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: ChatContent::SimpleText("".to_string()),
        tool_calls: Some(vec![ChatToolCall {
            id: tool_call_id.to_string(),
            index: Some(0),
            function: ChatToolFunction {
                name: tool_name.to_string(),
                arguments: "{}".to_string(),
            },
            tool_type: "function".to_string(),
            extra_content: None,
            started_at_ms: None,
            completed_at_ms: None,
        }]),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn make_tool_msg(tool_call_id: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        tool_call_id: tool_call_id.to_string(),
        content: ChatContent::SimpleText(content.to_string()),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn make_long_user_msg(token_estimate: usize) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: ChatContent::SimpleText("x".repeat(token_estimate.saturating_mul(4))),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn make_context_file_msg() -> ChatMessage {
    ChatMessage {
        role: "context_file".to_string(),
        content: ChatContent::SimpleText("file content".to_string()),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn context_limit_error(
    message: &str,
    partial_output_emitted: bool,
) -> LlmStreamError {
    LlmStreamError {
        message: message.to_string(),
        partial_output_emitted,
    }
}

pub(in crate::chat::generation) fn claude_code_model() -> BaseModelRecord {
    BaseModelRecord {
        wire_format: crate::llm::WireFormat::AnthropicMessages,
        auth_token: "cc-oauth-token".to_string(),
        ..Default::default()
    }
}

pub(in crate::chat::generation) fn shared_model_defaults(
) -> (ProviderDefaults, crate::caps::DefaultModels) {
    (
        ProviderDefaults {
            chat: ModelTypeDefaults {
                temperature: Some(0.1),
                ..Default::default()
            },
            task_planner_agent_model: ModelTypeDefaults {
                temperature: Some(0.3),
                ..Default::default()
            },
            ..Default::default()
        },
        crate::caps::DefaultModels {
            chat_default_model: "shared/model".to_string(),
            task_planner_agent_model: "shared/model".to_string(),
            ..Default::default()
        },
    )
}

pub(in crate::chat::generation) fn final_context_limit_error_bound() -> usize {
    context_limit_final_error_message("").len()
        + crate::chat::diagnostics::SAFE_PROVIDER_ERROR_DIAGNOSTIC_MAX_CHARS
}

pub(in crate::chat::generation) fn partial_output_error_bound() -> usize {
    PARTIAL_OUTPUT_STREAM_ERROR.len()
        + " Original error: ".len()
        + crate::chat::diagnostics::SAFE_PROVIDER_ERROR_DIAGNOSTIC_MAX_CHARS
}

pub(in crate::chat::generation) fn assert_context_limit_secret_redacted(text: &str) {
    assert!(!text.contains("sk-test-secret"), "secret leaked: {text}");
    assert!(
        !text.contains("Authorization: Bearer sk-test-secret"),
        "raw authorization header leaked: {text}"
    );
    assert!(
        !text.contains("Bearer sk-test-secret"),
        "raw bearer token leaked: {text}"
    );
    assert!(
        text.contains("[REDACTED"),
        "redaction marker missing: {text}"
    );
}

#[test]
fn context_limit_log_error_redacts_provider_secret() {
    let text = safe_context_limit_error_for_log(
        "context_length_exceeded: Authorization: Bearer sk-test-secret",
    );

    assert_context_limit_secret_redacted(&text);
    assert!(
        text.len() <= crate::chat::diagnostics::SAFE_PROVIDER_ERROR_DIAGNOSTIC_MAX_CHARS,
        "len={}",
        text.len()
    );
}

#[test]
fn model_type_defaults_for_thread_keeps_normal_chat_defaults_when_model_is_shared() {
    let (user_defaults, defaults) = shared_model_defaults();
    let thread = ThreadParams::default();

    let selected =
        model_type_defaults_for_thread(&user_defaults, &defaults, &thread, "shared/model");

    assert_eq!(selected.temperature, Some(0.1));
}

#[test]
fn model_type_defaults_for_thread_uses_task_planner_defaults_for_task_agents() {
    let (user_defaults, defaults) = shared_model_defaults();
    let thread = ThreadParams {
        task_meta: Some(TaskMeta {
            task_id: "task-1".to_string(),
            role: "agents".to_string(),
            agent_id: Some("agent-1".to_string()),
            card_id: Some("T-1".to_string()),
            planner_chat_id: Some("planner-chat".to_string()),
        }),
        ..Default::default()
    };

    let selected =
        model_type_defaults_for_thread(&user_defaults, &defaults, &thread, "shared/model");

    assert_eq!(selected.temperature, Some(0.3));
}

#[test]
fn context_limit_final_error_redacts_provider_secret() {
    let text = context_limit_final_error_message(
        "context_length_exceeded: Authorization: Bearer sk-test-secret",
    );

    assert!(text.starts_with("Context too large and context reconstruction failed"));
    assert!(text.contains("Explicitly rebuild"));
    assert!(text.contains("Original error:"));
    assert_context_limit_secret_redacted(&text);
    assert!(
        text.len() <= final_context_limit_error_bound(),
        "len={}",
        text.len()
    );
}

#[test]
fn context_limit_final_error_windows_huge_provider_error() {
    let far_tail = "FAR_TAIL_MARKER";
    let error = format!(
        "context_length_exceeded: Authorization: Bearer sk-test-secret {} {}",
        "tail ".repeat(100_000),
        far_tail,
    );

    let text = context_limit_final_error_message(&error);

    assert_context_limit_secret_redacted(&text);
    assert!(text.contains(crate::chat::diagnostics::SAFE_PROVIDER_ERROR_DIAGNOSTIC_TRUNCATED));
    assert!(!text.contains(far_tail), "far-tail marker leaked: {text}");
    assert!(
        text.len() <= final_context_limit_error_bound(),
        "len={}",
        text.len()
    );
}

#[test]
fn partial_output_stream_error_redacts_provider_secret() {
    let text = partial_output_stream_error_message(
        "provider failed: Authorization: Bearer sk-test-secret",
    );

    assert!(text.starts_with(PARTIAL_OUTPUT_STREAM_ERROR));
    assert_context_limit_secret_redacted(&text);
    assert!(
        text.len() <= partial_output_error_bound(),
        "len={}",
        text.len()
    );
}

#[test]
fn partial_output_stream_error_windows_huge_provider_error() {
    let far_tail = "FAR_TAIL_MARKER";
    let error = format!(
        "provider failed: Authorization: Bearer sk-test-secret {} {}",
        "tail ".repeat(100_000),
        far_tail,
    );

    let text = partial_output_stream_error_message(&error);

    assert_context_limit_secret_redacted(&text);
    assert!(text.contains(crate::chat::diagnostics::SAFE_PROVIDER_ERROR_DIAGNOSTIC_TRUNCATED));
    assert!(!text.contains(far_tail), "far-tail marker leaked: {text}");
    assert!(
        text.len() <= partial_output_error_bound(),
        "len={}",
        text.len()
    );
}

#[test]
fn wrapped_partial_output_context_limit_error_is_sanitized() {
    let wrapped = format!(
        "{} Original error: context_length_exceeded: Authorization: Bearer sk-test-secret",
        PARTIAL_OUTPUT_STREAM_ERROR,
    );
    let error = context_limit_error(&wrapped, true);

    let final_message = context_limit_final_error_message(&error.message);
    let wrapped_final = format!(
        "{} Original error: {}",
        PARTIAL_OUTPUT_STREAM_ERROR, final_message,
    );

    assert_context_limit_secret_redacted(&wrapped_final);
    assert!(
        wrapped_final.len()
            <= PARTIAL_OUTPUT_STREAM_ERROR.len() + 17 + final_context_limit_error_bound()
    );
}

#[test]
fn normalize_stop_reason_openai_length() {
    assert_eq!(
        normalize_stop_reason("length"),
        Some(NormalizedStopReason::ProviderLengthStop)
    );
    assert_eq!(
        normalize_stop_reason("max_output_tokens"),
        Some(NormalizedStopReason::ProviderLengthStop)
    );
}

#[test]
fn normalize_stop_reason_anthropic_max_tokens() {
    assert_eq!(
        normalize_stop_reason("max_tokens"),
        Some(NormalizedStopReason::ProviderLengthStop)
    );
}

#[test]
fn normalize_stop_reason_gemini_max_tokens() {
    assert_eq!(
        normalize_stop_reason("MAX_TOKENS"),
        Some(NormalizedStopReason::ProviderLengthStop)
    );
}

#[test]
fn normalize_stop_reason_generic_context_length() {
    assert_eq!(
        normalize_stop_reason("context_length_exceeded: input too long for model_length"),
        Some(NormalizedStopReason::ContextLengthStop)
    );
}

#[test]
fn test_claude_code_identity_generated_once_per_session() {
    let model = claude_code_model();
    let mut session = ChatSession::new("cc-identity".to_string());

    let first = ensure_claude_code_identity_for_test(&mut session, &model).unwrap();
    let version_after_first = session.trajectory_version;
    let second = ensure_claude_code_identity_for_test(&mut session, &model).unwrap();

    assert_eq!(first, second);
    assert_eq!(session.thread.claude_code_identity, Some(first));
    assert_eq!(session.trajectory_version, version_after_first);
    assert!(session.trajectory_dirty);
}

#[test]
fn test_claude_code_identity_reuses_deserialized_identity() {
    let model = claude_code_model();
    let identity: crate::llm::ClaudeCodeIdentity = serde_json::from_str(
        r#"{
                "device_id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "session_id":"bbbbbbbb-cccc-4ddd-8eee-ffffffffffff"
            }"#,
    )
    .unwrap();
    let mut session = ChatSession::new("cc-reload".to_string());
    session.thread.claude_code_identity = Some(identity.clone());

    let reused = ensure_claude_code_identity_for_test(&mut session, &model).unwrap();

    assert_eq!(reused, identity);
    assert_eq!(session.trajectory_version, 0);
    assert!(!session.trajectory_dirty);
}

#[test]
fn test_claude_code_identity_skips_non_claude_code_models() {
    let mut session = ChatSession::new("not-cc".to_string());
    let model = BaseModelRecord {
        wire_format: crate::llm::WireFormat::AnthropicMessages,
        auth_token: String::new(),
        ..Default::default()
    };

    assert!(ensure_claude_code_identity_for_test(&mut session, &model).is_none());
    assert!(session.thread.claude_code_identity.is_none());
    assert_eq!(session.trajectory_version, 0);
}

#[test]
fn token_budget_instruction_uses_only_latest_payload_and_suffix() {
    use refact_core::active_context::{make_reconstruction_report, ReconstructionMetadata};
    let mut active = make_user_msg("short active request");
    active.message_id = "payload-budget".into();
    let report =
        make_reconstruction_report(vec![active], ReconstructionMetadata::default()).unwrap();
    let mut session = ChatSession::new("budget-archive".into());
    session.add_message(make_user_msg(&"archived".repeat(20_000)));
    let mut old_marker = ChatMessage::new("cd_instruction".into(), "old budget".into());
    old_marker.tool_call_id = TOKEN_BUDGET_MARKER.into();
    session.add_message(old_marker);
    session.add_message(report);
    let before = serde_json::to_value(&session.messages).unwrap();
    let image_mode = refact_core::provider_types::ImageTokenMode::Provider;
    assert!(!maybe_inject_token_budget_instruction(
        &mut session,
        10000,
        2,
        image_mode
    ));
    let active = refact_core::active_context::active_context(&session.messages).unwrap();
    let used = refact_chat_history::trajectory_ops::approx_token_count_for_image_mode(
        &active.messages,
        image_mode,
    );
    assert!(maybe_inject_token_budget_instruction(
        &mut session,
        10000,
        1,
        image_mode
    ));
    assert_eq!(
        serde_json::to_value(&session.messages[..3]).unwrap(),
        before
    );
    assert!(session
        .messages
        .last()
        .unwrap()
        .content
        .content_text_only()
        .contains(&format!("~{} tokens remaining", 10000 - used)));
}

pub(in crate::chat::generation) fn identified(id: &str, mut message: ChatMessage) -> ChatMessage {
    message.message_id = id.to_string();
    message
}

pub(in crate::chat::generation) fn marked_context_file(id: &str, marker: &str) -> ChatMessage {
    let mut message = ChatMessage::new("context_file".into(), format!("{marker} files"));
    message.message_id = id.to_string();
    message.tool_call_id = marker.to_string();
    message
}

/// A handed-off chat starts as a single reconstruction report whose payload begins
/// with context files, exactly like the mode-transition destination.
pub(in crate::chat::generation) fn handed_off_session() -> ChatSession {
    use refact_core::active_context::{make_reconstruction_report, ReconstructionMetadata};
    let payload = vec![
        identified(
            "rebuilt-files",
            ChatMessage::new("context_file".into(), "rebuilt files".into()),
        ),
        identified(
            "rebuilt-memory",
            ChatMessage::new("context_file".into(), "rebuilt memory".into()),
        ),
        identified("previous", make_user_msg("## Previous Conversation")),
        identified("summary", make_user_msg("## Summary")),
    ];
    let mut session = ChatSession::new("handoff-destination".into());
    session.add_message(ChatMessage::new("system".into(), "archived prompt".into()));
    session.add_message(
        make_reconstruction_report(payload, ReconstructionMetadata::default()).unwrap(),
    );
    session
}

pub(in crate::chat::generation) fn active_roles(session: &ChatSession) -> Vec<String> {
    refact_core::active_context::active_context(&session.messages)
        .unwrap()
        .messages
        .iter()
        .map(|m| m.role.clone())
        .collect()
}

#[test]
fn preamble_lands_at_the_head_of_a_reconstructed_context() {
    let mut session = handed_off_session();
    let archive = serde_json::to_value(&session.messages[0]).unwrap();
    let report_id = session.messages[1].message_id.clone();
    let mut events = session.subscribe();
    let active = refact_core::active_context::active_context(&session.messages)
        .unwrap()
        .messages;
    let mut with_preamble = vec![ChatMessage::new("system".into(), "fresh prompt".into())];
    with_preamble.extend(active[..2].iter().cloned());
    with_preamble.push(marked_context_file(
        "project",
        crate::chat::system_context::PROJECT_CONTEXT_MARKER,
    ));
    with_preamble.push(marked_context_file("memories", "memories_context"));
    with_preamble.extend(active[2..].iter().cloned());

    assert_eq!(persist_preamble_messages(&mut session, &with_preamble), 3);
    assert_eq!(
        active_roles(&session),
        vec![
            "system",
            "context_file",
            "context_file",
            "context_file",
            "context_file",
            "user",
            "user"
        ]
    );
    let active = refact_core::active_context::active_context(&session.messages)
        .unwrap()
        .messages;
    assert_eq!(active[0].content.content_text_only(), "fresh prompt");
    assert_eq!(
        active[1].tool_call_id,
        crate::chat::system_context::PROJECT_CONTEXT_MARKER
    );
    assert_eq!(active[2].tool_call_id, "memories_context");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(serde_json::to_value(&session.messages[0]).unwrap(), archive);
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|m| m.role == "compression_report")
            .count(),
        1
    );
    let linearized =
        crate::chat::linearize::apply_summarization_linearize(session.messages.clone()).unwrap();
    assert!(crate::chat::history_limit::validate_chat_history(&linearized).is_ok());
    assert_eq!(
        crate::chat::trajectories::first_system_prompt(&linearized).as_deref(),
        Some("fresh prompt")
    );

    let mut updated_report = 0;
    while let Ok(event) = events.try_recv() {
        let event: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert_ne!(event["type"], "message_added");
        if event["type"] == "message_updated" {
            assert_eq!(event["message_id"], report_id);
            updated_report += 1;
        }
    }
    assert_eq!(updated_report, 3);

    assert_eq!(persist_preamble_messages(&mut session, &with_preamble), 0);
}

#[test]
fn mcp_lazy_hint_targets_the_active_context() {
    let mut session = handed_off_session();
    let mut archived_hint = ChatMessage::new("cd_instruction".into(), "archived".into());
    archived_hint.tool_call_id = MCP_LAZY_INDEX_MARKER.into();
    session.insert_message(1, archived_hint);
    assert_eq!(
        persist_preamble_messages(
            &mut session,
            &[ChatMessage::new("system".into(), "fresh prompt".into())]
        ),
        1
    );

    assert!(inject_mcp_lazy_hint(&mut session, 7));
    assert_eq!(
        active_roles(&session)[..3],
        ["system", "cd_instruction", "context_file"]
    );
    assert!(!inject_mcp_lazy_hint(&mut session, 7));
    assert_eq!(session.messages.len(), 3);
}

#[tokio::test]
async fn resolved_new_chat_default_persists_through_checkpoint_writer() {
    let root = tempfile::tempdir().unwrap();
    let gcx = crate::global_context::tests::make_test_gcx().await;
    let app = AppState::from_gcx(gcx.clone()).await;
    *app.workspace
        .documents_state
        .workspace_folders
        .lock()
        .unwrap() = vec![root.path().canonicalize().unwrap()];
    let mut session = ChatSession::new("resolved-default-snapshot".into());
    session.thread.context_tokens_cap = Some(100_000);
    session.add_message(make_user_msg("ready for first request"));
    let session = Arc::new(AMutex::new(session));
    let mut thread = session.lock().await.thread.clone();
    assert!(thread.auto_compression_cap_pending);
    resolve_session_compression_cap(&app, &session, &mut thread, 128_000).await;
    assert_eq!(thread.auto_compression_cap, Some(90_000));
    assert!(!thread.auto_compression_cap_pending);
    crate::chat::trajectories::flush_trajectory_for_session(app, session.clone())
        .await
        .unwrap();
    let restored = crate::chat::trajectories::load_trajectory_for_chat(&gcx.chat_ctx(gcx.clone()), &thread.id)
        .await
        .unwrap();
    assert_eq!(restored.thread.auto_compression_cap, Some(90_000));
    assert!(!restored.thread.auto_compression_cap_pending);
    assert!(session.lock().await.draft_message.is_none());
}

#[test]
fn test_token_budget_skips_after_tool_call_when_not_low() {
    let mut session = ChatSession::new("test".to_string());
    for idx in 0..TOKEN_BUDGET_CADENCE {
        session.messages.push(make_user_msg(&format!("user {idx}")));
    }
    session
        .messages
        .push(make_assistant_with_tool_call("call_123", "cat"));

    assert!(!maybe_inject_token_budget_instruction(
        &mut session,
        10_000,
        TOKEN_BUDGET_CADENCE,
        refact_core::provider_types::ImageTokenMode::Provider,
    ));
    assert!(!session
        .messages
        .iter()
        .any(|msg| msg.role == "cd_instruction" && msg.tool_call_id == TOKEN_BUDGET_MARKER));
}

#[test]
fn test_token_budget_skips_after_tool_call_even_when_below_ten_percent_left() {
    let mut session = ChatSession::new("test".to_string());
    session.messages.push(make_long_user_msg(920));
    for idx in 0..TOKEN_BUDGET_CADENCE {
        session.messages.push(make_user_msg(&format!("user {idx}")));
    }
    session
        .messages
        .push(make_assistant_with_tool_call("call_123", "cat"));

    assert!(!maybe_inject_token_budget_instruction(
        &mut session,
        1_000,
        TOKEN_BUDGET_CADENCE,
        refact_core::provider_types::ImageTokenMode::Provider,
    ));
    assert!(!session
        .messages
        .iter()
        .any(|msg| msg.role == "cd_instruction" && msg.tool_call_id == TOKEN_BUDGET_MARKER));
}

#[test]
fn test_tail_needs_assistant_ends_with_assistant_no_tools() {
    let messages = vec![make_user_msg("hello"), make_assistant_msg("response")];
    assert!(!tail_needs_assistant(&messages));
}

#[test]
fn test_tail_needs_assistant_ends_with_user() {
    let messages = vec![make_user_msg("hello")];
    assert!(tail_needs_assistant(&messages));
}

#[test]
fn test_tail_needs_assistant_ends_with_event() {
    let messages = vec![make_event_msg("synthetic prompt")];
    assert!(tail_needs_assistant(&messages));
}

#[test]
fn test_tail_needs_assistant_ends_with_tool_from_client() {
    let messages = vec![
        make_user_msg("hello"),
        make_assistant_with_tool_call("call_123", "cat"),
        make_tool_msg("call_123", "file content"),
    ];
    assert!(tail_needs_assistant(&messages));
}

#[test]
fn test_tail_needs_assistant_ends_with_tool_from_server() {
    let messages = vec![
        make_user_msg("hello"),
        make_assistant_with_tool_call("srvtoolu_123", "web_search"),
        make_tool_msg("srvtoolu_123", "search results"),
    ];
    assert!(!tail_needs_assistant(&messages));
}

#[test]
fn test_reasoning_token_limit_stop_detects_thought_only_length_finish() {
    let message = make_reasoning_token_limit_msg();
    assert!(is_reasoning_token_limit_stop(&message));
}

#[test]
fn test_reasoning_token_limit_stop_ignores_context_length_stop() {
    let mut message = make_reasoning_token_limit_msg();
    message.finish_reason = Some("context_length_exceeded".to_string());

    assert!(!is_reasoning_token_limit_stop(&message));
}

#[test]
fn test_reasoning_token_limit_stop_ignores_visible_answer() {
    let mut message = make_reasoning_token_limit_msg();
    message.content = ChatContent::SimpleText("visible answer".to_string());
    assert!(!is_reasoning_token_limit_stop(&message));
}

#[test]
fn length_stop_empty_reasoning_high_pressure_requests_compression() {
    let message = make_high_pressure_length_stop();
    let messages = vec![make_user_msg("continue"), message.clone()];

    assert!(is_high_pressure_length_stop(
        &message,
        &messages,
        100_000,
        false,
        refact_core::provider_types::ImageTokenMode::Provider
    ));
}

#[test]
fn low_pressure_length_stop_does_not_trigger_compression() {
    let message = make_low_pressure_length_stop();
    let messages = vec![make_user_msg("continue"), message.clone()];

    assert!(!is_high_pressure_length_stop(
        &message,
        &messages,
        100_000,
        false,
        refact_core::provider_types::ImageTokenMode::Provider
    ));
}

#[test]
fn token_budget_marker_not_injected_after_length_stop() {
    let mut session = ChatSession::new("length-stop-budget-marker".to_string());
    session.messages = vec![make_user_msg("continue"), make_low_pressure_length_stop()];

    assert!(!maybe_inject_token_budget_instruction(
        &mut session,
        100_000,
        1,
        refact_core::provider_types::ImageTokenMode::Provider,
    ));
    assert_eq!(session.messages.len(), 2);
}

#[test]
fn length_stop_visible_answer_without_high_pressure_does_not_request_compression() {
    let mut message = make_high_pressure_length_stop();
    message.content = ChatContent::SimpleText("ordinary visible answer".repeat(20));
    message.usage = Some(ChatUsage {
        prompt_tokens: 100,
        completion_tokens: 200,
        total_tokens: 300,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        metering_usd: None,
    });
    let messages = vec![make_user_msg("continue"), message.clone()];

    assert!(!is_high_pressure_length_stop(
        &message,
        &messages,
        100_000,
        false,
        refact_core::provider_types::ImageTokenMode::Provider
    ));
}

#[test]
fn length_stop_with_tool_calls_does_not_request_compression() {
    let mut message = make_high_pressure_length_stop();
    message.tool_calls = make_assistant_with_tool_call("call_123", "cat").tool_calls;
    let messages = vec![make_user_msg("continue"), message.clone()];

    assert!(!is_high_pressure_length_stop(
        &message,
        &messages,
        100_000,
        false,
        refact_core::provider_types::ImageTokenMode::Provider
    ));
}

#[test]
fn max_output_tokens_alias_is_treated_like_length() {
    let mut message = make_high_pressure_length_stop();
    message.finish_reason = Some("max_output_tokens".to_string());
    let messages = vec![make_user_msg("continue"), message.clone()];

    assert!(is_high_pressure_length_stop(
        &message,
        &messages,
        100_000,
        false,
        refact_core::provider_types::ImageTokenMode::Provider
    ));
}

#[test]
fn test_tail_needs_assistant_empty_assistant_discarded() {
    let messages = vec![
        make_user_msg("hello"),
        make_assistant_with_tool_call("call_123", "cat"),
        make_tool_msg("call_123", "file content"),
    ];
    assert!(tail_needs_assistant(&messages));
}

#[test]
fn test_tail_needs_assistant_context_file_after_tool() {
    let messages = vec![
        make_user_msg("hello"),
        make_assistant_with_tool_call("call_123", "cat"),
        make_tool_msg("call_123", "file content"),
        make_context_file_msg(),
    ];
    assert!(tail_needs_assistant(&messages));
}

#[test]
fn test_tail_needs_assistant_multiple_tool_calls_mixed() {
    let messages = vec![
        make_user_msg("hello"),
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText("".to_string()),
            tool_calls: Some(vec![
                ChatToolCall {
                    id: "call_123".to_string(),
                    index: Some(0),
                    function: ChatToolFunction {
                        name: "cat".to_string(),
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
        make_tool_msg("call_123", "file content"),
        make_tool_msg("srvtoolu_456", "search results"),
    ];
    assert!(tail_needs_assistant(&messages));
}
