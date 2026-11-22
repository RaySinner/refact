pub use refact_tools::tools::tool_compress_chat::*;
pub use refact_chat_history::chat_compress::{
    build_maximal_full_sweep_request, compress_chat_apply_head_output, find_preserve_cutoff,
    insert_current_compression_report, remove_invalid_tool_calls_and_tool_calls_results_before,
    CompressChatApplyOutput, CompressChatApplyRequest, CompressChatApplyStats,
    RECOVERY_PLAIN_TEXT_FLOOR, TOOL_OUTPUT_TRUNCATE_LIMIT,
};

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{HashMap, HashSet};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum, ContextFile};
#[cfg(test)]
#[allow(unused_imports)]
use refact_tool_api::IntegrationConfirmation;
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{ json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType, };
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_history::history_limit::{compress_duplicate_context_files, compute_context_budget};
#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_history::trajectory_ops::{ build_compression_report_message_with_fingerprint, insert_compression_report_at_boundary_scoped, is_memory_path, should_preserve_tool, };
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::string_utils::redact_sensitive;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::SessionState;
#[cfg(test)]
#[allow(unused_imports)]
use sha2::{Digest, Sha256};

#[cfg(test)]
fn compress_chat_apply_head_messages(
    head_messages: Vec<ChatMessage>,
    immutable_tail: &[ChatMessage],
    request: &CompressChatApplyRequest,
) -> (Vec<ChatMessage>, CompressChatApplyStats, usize) {
    let CompressChatApplyOutput {
        mut messages,
        stats,
        before_tokens,
        report,
        affected_boundary,
        modifiable_len,
        ..
    } = compress_chat_apply_head_output(head_messages, immutable_tail, request);
    insert_current_compression_report(&mut messages, report, affected_boundary, modifiable_len);
    (messages, stats, before_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_chat_history::trajectory_ops::{build_compression_report_message, TOOLS_TO_PRESERVE};
    use refact_core::chat_types::{ChatToolCall, ChatToolFunction};

    fn user_message(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    fn event_message(text: &str) -> ChatMessage {
        refact_chat_api::internal_roles::event(
            refact_chat_api::internal_roles::EventSubkind::SystemNotice,
            "test.compress_chat",
            json!({}),
            text.to_string(),
        )
    }

    fn assistant_message(text: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    fn system_message(text: &str) -> ChatMessage {
        ChatMessage {
            role: "system".to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    fn context_file_message(
        tool_call_id: &str,
        file_name: &str,
        file_content: &str,
    ) -> ChatMessage {
        ChatMessage {
            role: "context_file".to_string(),
            tool_call_id: tool_call_id.to_string(),
            content: ChatContent::ContextFiles(vec![ContextFile {
                file_name: file_name.to_string(),
                file_content: file_content.to_string(),
                line1: 1,
                line2: 1,
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    fn assistant_tool_call_message(tool_call_id: &str, tool_name: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: ChatContent::SimpleText(String::new()),
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

    fn tool_message(tool_call_id: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            tool_call_id: tool_call_id.to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    fn diff_message(tool_call_id: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: "diff".to_string(),
            tool_call_id: tool_call_id.to_string(),
            content: ChatContent::SimpleText(text.to_string()),
            ..Default::default()
        }
    }

    fn message_texts(messages: &[ChatMessage]) -> Vec<String> {
        messages
            .iter()
            .map(|message| message.content.content_text_only())
            .collect()
    }

    fn target_result<'a>(
        output: &'a CompressChatApplyOutput,
        operation: &str,
        target_id: &str,
    ) -> &'a CompressChatTargetResult {
        output
            .target_results
            .iter()
            .find(|result| result.operation == operation && result.target_id == target_id)
            .expect("target result should exist")
    }

    fn secrets_record() -> refact_privacy::FileRecord {
        refact_privacy::FileRecord {
            path: ".env".to_string(),
            zone: "secrets".to_string(),
            attribution: refact_privacy::Attribution::Observed,
        }
    }

    fn assert_secrets_record(message: &ChatMessage) {
        let privacy: refact_privacy::PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![secrets_record()]);
    }

    fn apply_request<'a>(
        drop_context_files: &'a HashSet<String>,
        drop_memories: &'a HashSet<String>,
        truncate_tool_outputs: &'a HashSet<String>,
        drop_tool_outputs: &'a HashSet<String>,
        drop_context_messages: &'a HashSet<String>,
        tool_call_names: &'a HashMap<String, String>,
    ) -> CompressChatApplyRequest<'a> {
        CompressChatApplyRequest {
            drop_context_files,
            drop_all_context_files: false,
            drop_memories,
            drop_all_memories: false,
            truncate_tool_outputs,
            truncate_all_tool_outputs: false,
            drop_tool_outputs,
            drop_context_messages,
            dedup_context_files: false,
            drop_project_information: false,
            strip_images: false,
            reduce_plain_text: false,
            strength: "conservative",
            preserve_last_turns: Some(1),
            target_tokens: None,
            tool_call_names,
        }
    }

    #[test]
    fn static_transform_preserves_legacy_summary_metadata() {
        let mut context_msg = context_file_message("ctx", "old.rs", "old context");
        context_msg.message_id = "cf-1".to_string();
        let mut summary = assistant_message("segment summary");
        summary.message_id = "sum-1".to_string();
        summary.extra.insert(
            "compression".to_string(),
            serde_json::json!({
                "kind": "llm_segment_summary",
                "insert_mode": "source_preserving",
                "source_message_ids": ["cf-1", "a-2"],
                "summarized_source_message_ids": ["cf-1", "a-2"],
                "preserved_source_message_ids": ["cf-1"],
            }),
        );
        let messages = vec![
            user_message("q"),
            context_msg,
            summary,
            user_message("tail"),
        ];
        let drop_context_files = HashSet::from(["old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let (messages, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.context_messages_dropped, 1);
        let summary = messages
            .iter()
            .find(|message| message.message_id == "sum-1")
            .expect("summary survives");
        let compression = summary.extra.get("compression").expect("metadata kept");
        assert_eq!(
            compression["source_message_ids"],
            serde_json::json!(["cf-1", "a-2"])
        );
        assert_eq!(
            compression["summarized_source_message_ids"],
            serde_json::json!(["cf-1", "a-2"])
        );
        assert_eq!(
            compression["preserved_source_message_ids"],
            serde_json::json!(["cf-1"])
        );
        assert!(refact_core::active_context::active_context(&messages).is_err());
    }

    fn assert_preserved_tail_unchanged(
        before: &[ChatMessage],
        after: &[ChatMessage],
        turns: usize,
    ) {
        let cutoff = find_preserve_cutoff(before, turns);
        let expected = serde_json::to_string(&before[cutoff..]).unwrap();
        let actual_start = after.len() - (before.len() - cutoff);
        let actual = serde_json::to_string(&after[actual_start..]).unwrap();
        assert_eq!(actual, expected);
    }

    fn compression_report_index(messages: &[ChatMessage]) -> usize {
        messages
            .iter()
            .position(|message| message.role == "compression_report")
            .expect("expected compression_report message")
    }

    fn compression_report_count(messages: &[ChatMessage]) -> usize {
        messages
            .iter()
            .filter(|message| message.role == "compression_report")
            .count()
    }

    fn compression_report_metadata(message: &ChatMessage) -> Value {
        message.extra["compression_report"].clone()
    }

    fn compression_report_metadatas(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .filter(|message| message.role == "compression_report")
            .map(compression_report_metadata)
            .collect()
    }

    #[test]
    fn cleanup_does_not_count_stale_prior_result_for_later_same_id_call() {
        let mut modifiable_prefix = vec![
            assistant_tool_call_message("call_reused", "shell"),
            tool_message("call_reused", "first result"),
            assistant_tool_call_message("call_reused", "shell"),
        ];

        let changed =
            remove_invalid_tool_calls_and_tool_calls_results_before(&mut modifiable_prefix, &[]);

        assert!(changed);
        assert_eq!(message_texts(&modifiable_prefix), vec!["", "first result"]);
        assert_eq!(modifiable_prefix[0].role, "assistant");
        assert_eq!(modifiable_prefix[1].role, "tool");
    }

    #[test]
    fn cleanup_removes_orphan_result_before_any_assistant_call_from_prefix() {
        let mut modifiable_prefix = vec![
            tool_message("call_orphan", "orphan result"),
            assistant_tool_call_message("call_orphan", "shell"),
            tool_message("call_orphan", "real result"),
        ];

        let changed =
            remove_invalid_tool_calls_and_tool_calls_results_before(&mut modifiable_prefix, &[]);

        assert!(changed);
        assert_eq!(message_texts(&modifiable_prefix), vec!["", "real result"]);
    }

    #[test]
    fn cleanup_preserves_assistant_followed_by_matching_result() {
        let mut modifiable_prefix = vec![
            assistant_tool_call_message("call_valid", "shell"),
            tool_message("call_valid", "valid result"),
        ];
        let before = serde_json::to_string(&modifiable_prefix).unwrap();

        let changed =
            remove_invalid_tool_calls_and_tool_calls_results_before(&mut modifiable_prefix, &[]);

        assert!(!changed);
        assert_eq!(serde_json::to_string(&modifiable_prefix).unwrap(), before);
    }

    #[test]
    fn cleanup_reused_ids_match_only_later_results_provider_order_safely() {
        let mut modifiable_prefix = vec![
            assistant_tool_call_message("call_reused", "shell"),
            tool_message("call_reused", "first result"),
            assistant_tool_call_message("call_reused", "shell"),
            diff_message("call_reused", "second result"),
        ];

        let changed =
            remove_invalid_tool_calls_and_tool_calls_results_before(&mut modifiable_prefix, &[]);

        assert!(!changed);
        assert_eq!(
            message_texts(&modifiable_prefix),
            vec!["", "first result", "", "second result"]
        );
    }

    #[test]
    fn cleanup_uses_immutable_tail_results_without_dropping_tail_messages() {
        let mut modifiable_prefix = vec![assistant_tool_call_message("tail_call", "shell")];
        let immutable_tail = vec![tool_message("tail_call", "tail result")];

        let changed = remove_invalid_tool_calls_and_tool_calls_results_before(
            &mut modifiable_prefix,
            &immutable_tail,
        );

        assert!(!changed);
        assert_eq!(modifiable_prefix.len(), 1);
        assert_eq!(message_texts(&immutable_tail), vec!["tail result"]);
    }

    fn stable_existing_report_for_manual_context_drop() -> ChatMessage {
        // Derive the report by running the same manual compression on the same
        // pre-state: replays dedupe by the deterministic operation fingerprint.
        let messages = vec![
            user_message("old user"),
            context_file_message("old_context", "old.rs", "old context"),
            assistant_message("old assistant"),
        ];
        let drop_context_files = HashSet::from(["old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let (after, _, _) = compress_chat_apply_head_messages(messages, &[], &request);
        after
            .into_iter()
            .find(|message| message.role == "compression_report")
            .expect("probe run must produce a compression report")
    }

    fn report_op_fingerprint(message: &ChatMessage) -> Option<String> {
        message
            .extra
            .get("compression_report")?
            .get("op_fingerprint")?
            .as_str()
            .map(ToString::to_string)
    }

    #[test]
    fn preserve_cutoff_zero_turns_makes_all_messages_modifiable() {
        let messages = vec![user_message("one"), assistant_message("two")];

        assert_eq!(find_preserve_cutoff(&messages, 0), messages.len());
    }

    #[test]
    fn preserve_cutoff_no_user_messages_makes_all_messages_preserved() {
        let messages = vec![assistant_message("one"), assistant_message("two")];

        assert_eq!(find_preserve_cutoff(&messages, 2), 0);
    }

    #[test]
    fn preserve_cutoff_fewer_user_turns_than_requested_preserves_all() {
        let messages = vec![user_message("one"), assistant_message("two")];

        assert_eq!(find_preserve_cutoff(&messages, 2), 0);
    }

    #[test]
    fn preserve_cutoff_preserves_requested_tail_turns() {
        let messages = vec![
            user_message("one"),
            assistant_message("two"),
            user_message("three"),
            assistant_message("four"),
        ];

        assert_eq!(find_preserve_cutoff(&messages, 1), 2);
    }

    #[test]
    fn preserve_cutoff_treats_event_notices_as_non_user_turns() {
        let messages = vec![
            event_message("synthetic notice"),
            assistant_message("two"),
            user_message("real user"),
            assistant_message("four"),
        ];

        assert_eq!(find_preserve_cutoff(&messages, 1), 2);
        assert_eq!(find_preserve_cutoff(&messages, 2), 0);
    }

    #[test]
    fn apply_drop_all_memories_preserves_tail_context_files() {
        let messages = vec![
            user_message("old user"),
            context_file_message("old_memory", "/repo/.refact/knowledge/old.md", "old memory"),
            user_message("tail user"),
            context_file_message(
                "tail_memory",
                "/repo/.refact/knowledge/tail.md",
                "tail memory",
            ),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.drop_all_memories = true;

        let (after, stats, _) = compress_chat_apply_head_messages(messages.clone(), &[], &request);

        assert_eq!(stats.memory_dropped, 1);
        assert_preserved_tail_unchanged(&messages, &after, 1);
        let report_idx = compression_report_index(&after);
        assert!(after[report_idx + 1..]
            .iter()
            .any(|message| message.content.content_text_only().contains("tail memory")));
        assert!(after
            .iter()
            .any(|message| message.content.content_text_only().contains("tail memory")));
    }

    #[test]
    fn apply_context_drop_options_preserve_tail_context_file_messages() {
        let messages = vec![
            user_message("old user"),
            context_file_message("old_context", "old.rs", "old context"),
            user_message("tail user"),
            context_file_message("tail_context", "tail.rs", "tail context"),
        ];
        let drop_context_files = HashSet::from(["tail.rs".to_string(), "old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages =
            HashSet::from(["tail_context".to_string(), "old_context".to_string()]);
        let tool_call_names = HashMap::new();
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let (after, stats, _) = compress_chat_apply_head_messages(messages.clone(), &[], &request);

        assert_eq!(stats.context_messages_dropped, 1);
        assert_preserved_tail_unchanged(&messages, &after, 1);
    }

    #[test]
    fn apply_all_preserved_noop_does_not_prepend_report_before_tail() {
        let messages = vec![
            user_message("tail user"),
            assistant_message("tail assistant"),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let (after, stats, _) = compress_chat_apply_head_messages(messages.clone(), &[], &request);

        assert!(!stats.has_meaningful_mutation());
        assert_eq!(compression_report_count(&after), 0);
        assert_eq!(
            serde_json::to_string(&after).unwrap(),
            serde_json::to_string(&messages).unwrap()
        );
    }

    #[test]
    fn apply_noop_modifiable_head_does_not_add_report() {
        let messages = vec![
            user_message("old user"),
            assistant_message("old assistant"),
            user_message("tail user"),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let (after, stats, _) = compress_chat_apply_head_messages(messages.clone(), &[], &request);

        assert!(!stats.has_meaningful_mutation());
        assert_eq!(compression_report_count(&after), 0);
        assert_eq!(
            serde_json::to_string(&after).unwrap(),
            serde_json::to_string(&messages).unwrap()
        );
    }

    #[test]
    fn apply_tool_drop_and_truncate_preserve_tail_tool_results() {
        let long_old = "old tool output ".repeat(30);
        let long_tail = "tail tool output ".repeat(30);
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("old_call", "shell"),
            tool_message("old_call", &long_old),
            user_message("tail user"),
            assistant_tool_call_message("tail_drop_call", "shell"),
            tool_message("tail_drop_call", "tail drop output"),
            assistant_tool_call_message("tail_truncate_call", "shell"),
            tool_message("tail_truncate_call", &long_tail),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs =
            HashSet::from(["old_call".to_string(), "tail_truncate_call".to_string()]);
        let drop_tool_outputs = HashSet::from(["tail_drop_call".to_string()]);
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([
            ("old_call".to_string(), "shell".to_string()),
            ("tail_drop_call".to_string(), "shell".to_string()),
            ("tail_truncate_call".to_string(), "shell".to_string()),
        ]);
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let (after, stats, _) = compress_chat_apply_head_messages(messages.clone(), &[], &request);

        assert_eq!(stats.tool_truncated, 1);
        assert_eq!(stats.tool_dropped, 0);
        assert!(after.iter().any(|message| message
            .content
            .content_text_only()
            .starts_with("Tool result compressed:")));
        assert!(compression_report_index(&after) <= find_preserve_cutoff(&messages, 1));
        assert_preserved_tail_unchanged(&messages, &after, 1);
    }

    #[test]
    fn apply_tool_truncate_preserves_agentic_tool_results() {
        let long_output = "agentic tool output ".repeat(30);
        for tool_name in TOOLS_TO_PRESERVE {
            let messages = vec![
                user_message("old user"),
                assistant_tool_call_message("call_agentic", tool_name),
                tool_message("call_agentic", &long_output),
            ];
            let drop_context_files = HashSet::new();
            let drop_memories = HashSet::new();
            let truncate_tool_outputs = HashSet::from(["call_agentic".to_string()]);
            let drop_tool_outputs = HashSet::new();
            let drop_context_messages = HashSet::new();
            let tool_call_names =
                HashMap::from([("call_agentic".to_string(), (*tool_name).to_string())]);
            let request = apply_request(
                &drop_context_files,
                &drop_memories,
                &truncate_tool_outputs,
                &drop_tool_outputs,
                &drop_context_messages,
                &tool_call_names,
            );

            let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

            assert_eq!(stats.tool_truncated, 0, "{tool_name} should be preserved");
            assert!(after.iter().any(|message| {
                message.role == "tool" && message.content.content_text_only() == long_output
            }));
        }
    }

    #[test]
    fn apply_tool_drop_and_truncate_preserves_explicit_preserve_results() {
        let long_output = "explicit preserved tool output ".repeat(30);
        let mut drop_tool = tool_message("drop_call", &long_output);
        drop_tool.preserve = Some(true);
        let mut truncate_tool = tool_message("truncate_call", &long_output);
        truncate_tool.preserve = Some(true);
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("drop_call", "shell"),
            drop_tool,
            assistant_tool_call_message("truncate_call", "shell"),
            truncate_tool,
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::from(["truncate_call".to_string()]);
        let drop_tool_outputs = HashSet::from(["drop_call".to_string()]);
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([
            ("drop_call".to_string(), "shell".to_string()),
            ("truncate_call".to_string(), "shell".to_string()),
        ]);
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.tool_truncated, 0);
        assert_eq!(stats.tool_dropped, 0);
        assert_eq!(
            after
                .iter()
                .filter(|message| message.role == "tool"
                    && message.content.content_text_only() == long_output)
                .count(),
            2
        );
    }

    #[test]
    fn apply_tool_truncate_preview_redacts_bearer_token() {
        let secret = "secret-token-123";
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("old_call", "shell"),
            tool_message(
                "old_call",
                &format!("request failed with Bearer {} {}", secret, "x".repeat(1000)),
            ),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::from(["old_call".to_string()]);
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([("old_call".to_string(), "shell".to_string())]);
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.tool_truncated, 1);
        let tool_text = after
            .iter()
            .find(|message| message.role == "tool")
            .unwrap()
            .content
            .content_text_only();
        assert!(tool_text.contains("Bearer [REDACTED]"));
        assert!(!tool_text.contains(secret));
    }

    #[test]
    fn summarization_records_survive_ctx_apply_tool_truncation() {
        let mut tool = tool_message("old_call", &"tool output ".repeat(100));
        refact_privacy::attach_record(&mut tool, secrets_record());
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("old_call", "shell"),
            tool,
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::from(["old_call".to_string()]);
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([("old_call".to_string(), "shell".to_string())]);
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.tool_truncated, 1);
        let truncated = after
            .iter()
            .find(|message| message.role == "tool")
            .expect("tool result should survive");
        assert!(truncated
            .content
            .content_text_only()
            .starts_with("Tool result compressed:"));
        assert_secrets_record(truncated);
    }

    #[test]
    fn apply_tool_truncate_preview_redacts_api_key() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz";
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("old_call", "shell"),
            tool_message(
                "old_call",
                &format!(
                    "request failed with api_key={} {}",
                    secret,
                    "x".repeat(1000)
                ),
            ),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::from(["old_call".to_string()]);
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([("old_call".to_string(), "shell".to_string())]);
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.tool_truncated, 1);
        let tool_text = after
            .iter()
            .find(|message| message.role == "tool")
            .unwrap()
            .content
            .content_text_only();
        assert!(tool_text.contains("api_key=[REDACTED]"));
        assert!(!tool_text.contains(secret));
    }

    #[test]
    fn apply_drop_all_memories_detects_relative_memory_path() {
        let messages = vec![
            context_file_message("memory", ".refact/knowledge/foo.md", "memory"),
            context_file_message("source", "src/lib.rs", "source"),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.drop_all_memories = true;
        request.preserve_last_turns = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.memory_dropped, 1);
        assert!(!after
            .iter()
            .any(|message| message.content.content_text_only().contains("foo.md")));
        assert!(after
            .iter()
            .any(|message| message.content.content_text_only().contains("source")));
    }

    #[test]
    fn apply_drop_all_memories_detects_windows_task_path() {
        let messages = vec![
            context_file_message(
                "memory",
                r#"C:\repo\.refact\tasks\task-id\memories\note.md"#,
                "memory",
            ),
            context_file_message("source", "src/lib.rs", "source"),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.drop_all_memories = true;
        request.preserve_last_turns = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.memory_dropped, 1);
        assert!(!after
            .iter()
            .any(|message| message.content.content_text_only().contains("note.md")));
        assert!(after
            .iter()
            .any(|message| message.content.content_text_only().contains("source")));
    }

    #[test]
    fn apply_drop_all_memories_preserves_non_memory_paths() {
        let messages = vec![context_file_message(
            "source",
            "tests/.refact_fixture/tasks/example.rs",
            "fixture source",
        )];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.drop_all_memories = true;
        request.preserve_last_turns = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.memory_dropped, 0);
        assert!(after.iter().any(|message| message
            .content
            .content_text_only()
            .contains("fixture source")));
        assert_eq!(compression_report_count(&after), 0);
    }

    #[test]
    fn apply_inserts_compression_report_before_preserved_tail() {
        let messages = vec![
            user_message("old user"),
            context_file_message("old_context", "old.rs", "old context"),
            user_message("tail user"),
            assistant_message("tail assistant"),
        ];
        let drop_context_files = HashSet::from(["old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let (after, stats, before_tokens) =
            compress_chat_apply_head_messages(messages.clone(), &[], &request);

        assert_eq!(stats.context_messages_dropped, 1);
        let report_idx = compression_report_index(&after);
        assert_eq!(
            after[report_idx + 1].content.content_text_only(),
            "tail user"
        );
        let metadata = &after[report_idx].extra["compression_report"];
        assert_eq!(metadata["kind"], json!("chat_compression_report"));
        assert_eq!(metadata["context_files_removed"], json!(1));
        assert_eq!(metadata["context_messages_dropped"], json!(1));
        assert_eq!(metadata["tool_results_truncated"], json!(0));
        assert!(after[report_idx]
            .content
            .content_text_only()
            .contains("- Context messages dropped: 1"));
        assert_eq!(metadata["tokens_before"], json!(before_tokens));
        assert!(metadata["tokens_after"].as_u64().unwrap() <= before_tokens as u64);
    }

    #[test]
    fn apply_report_after_first_user_when_leading_systems_and_first_user_preserved() {
        let messages = vec![
            system_message("root prompt"),
            system_message("workspace details"),
            context_file_message("old_context", "old.rs", "old context"),
            user_message("tail user"),
            assistant_message("tail assistant"),
        ];
        let drop_context_files = HashSet::from(["old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.context_messages_dropped, 1);
        let report_idx = compression_report_index(&after);
        let first_user_idx = after
            .iter()
            .position(|message| message.role == "user")
            .unwrap();
        assert!(report_idx > first_user_idx);
        let roles: Vec<_> = after.iter().map(|message| message.role.as_str()).collect();
        assert_eq!(
            roles,
            vec![
                "system",
                "system",
                "user",
                "compression_report",
                "assistant"
            ]
        );
    }

    #[test]
    fn apply_noop_with_existing_report_returns_no_current_report_metadata() {
        let old_report = build_compression_report_message(1, 1, 0, 100, 80);
        let old_metadata = compression_report_metadata(&old_report);
        let messages = vec![
            user_message("old user"),
            old_report,
            assistant_message("answer"),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let CompressChatApplyOutput {
            mut messages,
            stats,
            report,
            affected_boundary,
            modifiable_len,
            ..
        } = compress_chat_apply_head_output(messages, &[], &request);
        let current_metadata = insert_current_compression_report(
            &mut messages,
            report,
            affected_boundary,
            modifiable_len,
        );

        assert!(!stats.has_meaningful_mutation());
        assert_eq!(current_metadata, None);
        assert_eq!(compression_report_count(&messages), 1);
        assert_eq!(compression_report_metadatas(&messages), vec![old_metadata]);
    }

    #[test]
    fn apply_preserved_tail_old_report_does_not_drive_current_metadata() {
        let old_report = build_compression_report_message(0, 0, 1, 1000, 900);
        let old_metadata = compression_report_metadata(&old_report);
        let messages = vec![
            user_message("old user"),
            context_file_message("old_context", "old.rs", "old context"),
            user_message("tail user"),
            old_report,
            assistant_message("tail assistant"),
        ];
        let drop_context_files = HashSet::from(["old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let CompressChatApplyOutput {
            mut messages,
            stats,
            report,
            affected_boundary,
            modifiable_len,
            ..
        } = compress_chat_apply_head_output(messages, &[], &request);
        let current_metadata = insert_current_compression_report(
            &mut messages,
            report,
            affected_boundary,
            modifiable_len,
        )
        .expect("current invocation should create report metadata");

        assert_eq!(stats.context_messages_dropped, 1);
        assert_ne!(current_metadata, old_metadata);
        assert_eq!(compression_report_count(&messages), 2);
        assert!(compression_report_metadatas(&messages).contains(&old_metadata));
    }

    #[test]
    fn apply_repeated_equivalent_manual_compression_dedupes() {
        let existing_report = stable_existing_report_for_manual_context_drop();
        let existing_fingerprint = report_op_fingerprint(&existing_report);
        assert!(existing_fingerprint.is_some());
        let messages = vec![
            user_message("old user"),
            existing_report,
            context_file_message("old_context", "old.rs", "old context"),
            assistant_message("old assistant"),
        ];
        let drop_context_files = HashSet::from(["old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.context_messages_dropped, 1);
        assert_eq!(compression_report_count(&after), 1);
        let surviving = after
            .iter()
            .find(|message| message.role == "compression_report")
            .expect("a compression report must remain");
        assert_eq!(report_op_fingerprint(surviving), existing_fingerprint);
    }

    #[test]
    fn apply_same_sets_with_different_options_keeps_both_reports() {
        let existing_report = stable_existing_report_for_manual_context_drop();
        let existing_fingerprint = report_op_fingerprint(&existing_report);
        assert!(existing_fingerprint.is_some());
        let messages = vec![
            user_message("old user"),
            existing_report,
            context_file_message("old_context", "old.rs", "old context"),
            assistant_message("old assistant"),
        ];
        let drop_context_files = HashSet::from(["old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);
        // Same explicit id sets, but a different mutation-driving option: this is a
        // distinct operation and must not dedupe the earlier report.
        request.drop_project_information = true;

        let (after, _, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(compression_report_count(&after), 2);
        let fingerprints: Vec<Option<String>> = after
            .iter()
            .filter(|message| message.role == "compression_report")
            .map(report_op_fingerprint)
            .collect();
        assert!(fingerprints.contains(&existing_fingerprint));
        assert!(fingerprints.iter().any(|fp| fp != &existing_fingerprint));
    }

    #[test]
    fn apply_non_equivalent_manual_reports_remain() {
        let existing_report = build_compression_report_message(0, 0, 1, 1000, 800);
        let existing_metadata = compression_report_metadata(&existing_report);
        let messages = vec![
            user_message("old user"),
            existing_report,
            context_file_message("old_context", "old.rs", "old context"),
            assistant_message("old assistant"),
        ];
        let drop_context_files = HashSet::from(["old.rs".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages, &[], &request);

        assert_eq!(stats.context_messages_dropped, 1);
        assert_eq!(compression_report_count(&after), 2);
        assert!(compression_report_metadatas(&after).contains(&existing_metadata));
    }

    #[test]
    fn apply_drop_project_information_preserves_tail_system_messages() {
        let messages = vec![
            system_message("root prompt"),
            system_message("old project workspace details"),
            user_message("tail user"),
            system_message("tail project workspace details"),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.drop_project_information = true;

        let (after, stats, _) = compress_chat_apply_head_messages(messages.clone(), &[], &request);

        assert_eq!(stats.project_info_dropped, 1);
        assert!(!after.iter().any(|message| message
            .content
            .content_text_only()
            .contains("old project workspace details")));
        let report_idx = compression_report_index(&after);
        assert_eq!(
            after[report_idx - 1].content.content_text_only(),
            "tail user"
        );
        assert!(after.iter().any(|message| message
            .content
            .content_text_only()
            .contains("tail project workspace details")));
    }

    #[test]
    fn apply_aggressive_combination_preserves_tail_byte_identically() {
        let messages = vec![
            system_message("root prompt"),
            user_message("old user"),
            assistant_message("old assistant"),
            context_file_message("old_memory", "/repo/.refact/knowledge/old.md", "old memory"),
            user_message("middle user"),
            assistant_message("middle assistant"),
            user_message("tail user"),
            context_file_message(
                "tail_memory",
                "/repo/.refact/knowledge/tail.md",
                "tail memory",
            ),
            assistant_tool_call_message("tail_call", "shell"),
            tool_message("tail_call", &"tail tool output ".repeat(30)),
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::from(["tail_call".to_string()]);
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([("tail_call".to_string(), "shell".to_string())]);
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.drop_all_memories = true;
        request.drop_project_information = true;
        request.strength = "aggressive";
        request.target_tokens = Some(0);

        let (after, stats, _) = compress_chat_apply_head_messages(messages.clone(), &[], &request);

        assert_eq!(stats.memory_dropped, 1);
        assert_eq!(stats.tool_truncated, 0);
        assert_preserved_tail_unchanged(&messages, &after, 1);
    }

    #[test]
    fn probe_tool_id_applies_with_real_savings_and_survives_reload() {
        let original = "provider visible tool output ".repeat(80);
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("known_call", "shell"),
            tool_message("known_call", &original),
        ];
        let target_id = compression_tool_output_target_id(&messages[2]).unwrap();
        assert_eq!(target_id, "tool_output:tool_call:known_call");
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::from([target_id.clone()]);
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([("known_call".to_string(), "shell".to_string())]);
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let output = compress_chat_apply_head_output(messages, &[], &request);

        assert!(output.tokens_saved > 0);
        assert!(output.report.is_some());
        let result = target_result(&output, "truncate_tool_outputs", &target_id);
        assert!(result.applied);
        assert_eq!(result.reason, None);
        assert!(result.tokens_saved > 0);
        let truncated = output
            .messages
            .iter()
            .find(|message| message.role == "tool")
            .unwrap()
            .content
            .content_text_only();
        assert!(truncated.len() < original.len());
        let restored: Vec<ChatMessage> =
            serde_json::from_str(&serde_json::to_string(&output.messages).unwrap()).unwrap();
        assert_eq!(
            restored
                .iter()
                .find(|message| message.role == "tool")
                .unwrap()
                .content
                .content_text_only(),
            truncated
        );
    }

    #[test]
    fn empty_tool_call_id_uses_message_fallback_and_empty_target_is_unsupported() {
        let mut fallback = tool_message("", &"fallback output ".repeat(80));
        fallback.message_id = "fallback-message".to_string();
        let unsupported = tool_message("", &"unsupported output ".repeat(80));
        let fallback_id = compression_tool_output_target_id(&fallback).unwrap();
        assert_eq!(fallback_id, "tool_output:message:fallback-message");
        assert_eq!(compression_tool_output_target_id(&unsupported), None);
        let messages = vec![user_message("old user"), fallback, unsupported];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::from([fallback_id.clone()]);
        let drop_tool_outputs = HashSet::from([String::new()]);
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let output = compress_chat_apply_head_output(messages, &[], &request);

        assert!(target_result(&output, "truncate_tool_outputs", &fallback_id).applied);
        let unsupported_result = target_result(&output, "drop_tool_outputs", "");
        assert!(!unsupported_result.applied);
        assert_eq!(unsupported_result.reason.as_deref(), Some("missing_stable_id"));
        assert!(output.messages.iter().any(|message| {
            message.role == "tool" && message.content.content_text_only().starts_with("unsupported output")
        }));
    }

    #[test]
    fn context_and_memory_canonical_ids_apply_to_their_own_types() {
        let mut context = context_file_message("", "old.rs", &"old context ".repeat(30));
        context.message_id = "context-message".to_string();
        let memory = context_file_message(
            "memory-call",
            "/repo/.refact/knowledge/old.md",
            &"old memory ".repeat(30),
        );
        let context_id = compression_context_message_target_id(&context).unwrap();
        let memory_id = compression_context_file_target_id(&ContextFile {
            file_name: "/repo/.refact/knowledge/old.md".to_string(),
            ..Default::default()
        });
        assert_eq!(context_id, "context_message:message:context-message");
        assert!(memory_id.starts_with("memory:"));
        let messages = vec![user_message("old user"), context, memory];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::from([memory_id.clone()]);
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::from([context_id.clone()]);
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let output = compress_chat_apply_head_output(messages, &[], &request);

        assert!(target_result(&output, "drop_context_messages", &context_id).applied);
        assert!(target_result(&output, "drop_memories", &memory_id).applied);
        assert_eq!(output.stats.context_messages_dropped, 2);
        assert_eq!(output.stats.memory_dropped, 1);
        assert!(output.tokens_saved > 0);
    }

    #[test]
    fn apply_reports_protected_preserved_and_mismatched_targets() {
        let protected_name = TOOLS_TO_PRESERVE[0];
        let protected = tool_message("protected", &"protected output ".repeat(30));
        let tail = tool_message("tail", &"tail output ".repeat(30));
        let protected_id = compression_tool_output_target_id(&protected).unwrap();
        let tail_id = compression_tool_output_target_id(&tail).unwrap();
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("protected", protected_name),
            protected,
            user_message("tail user"),
            assistant_tool_call_message("tail", "shell"),
            tail,
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::from([protected_id.clone(), tail_id.clone()]);
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::from([tail_id.clone()]);
        let tool_call_names = HashMap::from([
            ("protected".to_string(), protected_name.to_string()),
            ("tail".to_string(), "shell".to_string()),
        ]);
        let request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );

        let output = compress_chat_apply_head_output(messages, &[], &request);

        assert_eq!(
            target_result(&output, "truncate_tool_outputs", &protected_id)
                .reason
                .as_deref(),
            Some("protected_tool")
        );
        assert_eq!(
            target_result(&output, "truncate_tool_outputs", &tail_id)
                .reason
                .as_deref(),
            Some("preserve_tail")
        );
        assert_eq!(
            target_result(&output, "drop_context_messages", &tail_id)
                .reason
                .as_deref(),
            Some("mismatched_id")
        );
        assert_eq!(output.tokens_saved, 0);
        assert!(output.report.is_none());
    }

    #[test]
    fn apply_reports_mixed_applied_no_savings_and_missing_targets() {
        let long = tool_message("long", &"long output ".repeat(80));
        let short = tool_message("short", "tiny");
        let long_id = compression_tool_output_target_id(&long).unwrap();
        let short_id = compression_tool_output_target_id(&short).unwrap();
        let missing_id = "tool_output:tool_call:missing".to_string();
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("long", "shell"),
            long,
            assistant_tool_call_message("short", "shell"),
            short,
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs =
            HashSet::from([long_id.clone(), short_id.clone(), missing_id.clone()]);
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([
            ("long".to_string(), "shell".to_string()),
            ("short".to_string(), "shell".to_string()),
        ]);
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let output = compress_chat_apply_head_output(messages, &[], &request);

        assert!(target_result(&output, "truncate_tool_outputs", &long_id).applied);
        assert_eq!(
            target_result(&output, "truncate_tool_outputs", &short_id)
                .reason
                .as_deref(),
            Some("no_savings")
        );
        assert_eq!(
            target_result(&output, "truncate_tool_outputs", &missing_id)
                .reason
                .as_deref(),
            Some("missing_or_already_dropped")
        );
        assert_eq!(output.stats.tool_truncated, 1);
    }

    #[test]
    fn repeated_apply_is_a_diagnosed_noop_without_false_success() {
        let tool = tool_message("drop", &"drop output ".repeat(80));
        let target_id = compression_tool_output_target_id(&tool).unwrap();
        let messages = vec![
            user_message("old user"),
            assistant_tool_call_message("drop", "shell"),
            tool,
        ];
        let drop_context_files = HashSet::new();
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::from([target_id.clone()]);
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::from([("drop".to_string(), "shell".to_string())]);
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let first = compress_chat_apply_head_output(messages, &[], &request);
        assert!(target_result(&first, "drop_tool_outputs", &target_id).applied);
        let before_second = serde_json::to_string(&first.messages).unwrap();
        let second = compress_chat_apply_head_output(first.messages, &[], &request);

        assert_eq!(second.tokens_saved, 0);
        assert!(second.report.is_none());
        assert_eq!(
            target_result(&second, "drop_tool_outputs", &target_id)
                .reason
                .as_deref(),
            Some("no_savings")
        );
        assert_eq!(serde_json::to_string(&second.messages).unwrap(), before_second);
    }

    #[test]
    fn memory_path_cannot_be_dropped_through_context_file_targets() {
        let memory = context_file_message(
            "memory-call",
            "/repo/.refact/knowledge/old.md",
            &"old memory ".repeat(30),
        );
        let messages = vec![user_message("old user"), memory];
        let drop_context_files = HashSet::from(["/repo/.refact/knowledge/old.md".to_string()]);
        let drop_memories = HashSet::new();
        let truncate_tool_outputs = HashSet::new();
        let drop_tool_outputs = HashSet::new();
        let drop_context_messages = HashSet::new();
        let tool_call_names = HashMap::new();
        let mut request = apply_request(
            &drop_context_files,
            &drop_memories,
            &truncate_tool_outputs,
            &drop_tool_outputs,
            &drop_context_messages,
            &tool_call_names,
        );
        request.preserve_last_turns = Some(0);

        let output = compress_chat_apply_head_output(messages, &[], &request);

        assert_eq!(output.stats.context_files_dropped, 0);
        assert_eq!(output.stats.memory_dropped, 0);
        assert_eq!(
            target_result(
                &output,
                "drop_context_files",
                "/repo/.refact/knowledge/old.md"
            )
            .reason
            .as_deref(),
            Some("mismatched_id")
        );
    }
}
