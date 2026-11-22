use std::collections::{HashMap, HashSet};

use refact_chat_history::chat_compress::{
    build_maximal_full_sweep_request, compress_chat_apply_head_output,
    insert_current_compression_report, CompressChatApplyOutput, RECOVERY_PLAIN_TEXT_FLOOR,
};
use refact_chat_history::compression_exemption::{
    event_source, event_subkind, exemption_for, CompressionExemption,
};
use refact_core::chat_types::{ChatContent, ChatMessage};

const LARGE_CORPUS: &str = include_str!("fixtures/large_corpus.json");

fn large_corpus() -> Vec<ChatMessage> {
    serde_json::from_str(LARGE_CORPUS).expect("large corpus fixture should deserialize")
}

fn plan_jsons(messages: &[ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == "plan")
        .map(|message| serde_json::to_string(message).expect("plan should serialize"))
        .collect()
}

fn event_ids_for_source(messages: &[ChatMessage], subkind: &str, source: &str) -> Vec<String> {
    messages
        .iter()
        .filter(|message| {
            message.role == "event"
                && event_subkind(message) == Some(subkind)
                && event_source(message) == source
        })
        .map(|message| message.message_id.clone())
        .collect()
}

fn assistant_texts(messages: &[ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| message.content.content_text_only())
        .collect()
}

fn plain_text_message(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: ChatContent::SimpleText(text.to_string()),
        ..Default::default()
    }
}

fn oversized_text(marker: &str) -> String {
    format!("{marker}{}", "x".repeat(RECOVERY_PLAIN_TEXT_FLOOR))
}

fn recovery_sweep(
    head_messages: Vec<ChatMessage>,
    immutable_tail: &[ChatMessage],
) -> CompressChatApplyOutput {
    let drop_context_files = HashSet::new();
    let drop_memories = HashSet::new();
    let truncate_tool_outputs = HashSet::new();
    let drop_tool_outputs = HashSet::new();
    let drop_context_messages = HashSet::new();
    let tool_call_names = HashMap::new();
    let request = build_maximal_full_sweep_request(
        &drop_context_files,
        &drop_memories,
        &truncate_tool_outputs,
        &drop_tool_outputs,
        &drop_context_messages,
        &tool_call_names,
    );

    compress_chat_apply_head_output(head_messages, immutable_tail, &request)
}

#[test]
fn large_corpus_preserves_plans_without_history_limit_rewrites() {
    let messages = large_corpus();
    let plans = plan_jsons(&messages);

    assert_eq!(plans.len(), 5);
    for message in messages.iter().filter(|message| message.role == "plan") {
        assert_eq!(exemption_for(message), CompressionExemption::Never);
    }
}

#[test]
fn large_corpus_event_exemption_contract_is_stable() {
    let messages = large_corpus();

    for source in ["exec:build", "exec:test"] {
        assert!(!event_ids_for_source(&messages, "process_completed", source).is_empty());
    }

    for subkind in ["summarization_marker", "system_notice", "cancellation_note"] {
        let anchors: Vec<&ChatMessage> = messages
            .iter()
            .filter(|message| message.role == "event" && event_subkind(message) == Some(subkind))
            .collect();
        assert!(!anchors.is_empty(), "fixture should contain {subkind}");
        assert!(anchors
            .iter()
            .all(|message| exemption_for(message) == CompressionExemption::PreserveAnchor));
    }
}

#[test]
fn large_corpus_assistant_content_available_for_segment_summarizer() {
    let original_assistant_texts = assistant_texts(&large_corpus());

    assert!(!original_assistant_texts.is_empty());
    assert!(original_assistant_texts
        .iter()
        .any(|text| !text.trim().is_empty()));
}

#[test]
fn recovery_preserves_last_detailed_assistant_report_and_late_references() {
    let early_user = oversized_text("early user\n");
    let early_assistant = oversized_text("early assistant\n");
    let preserved_text = oversized_text("explicitly preserved user\n");
    let final_report = oversized_text("final source-backed report\n");
    let mut preserved_user = plain_text_message("user", &preserved_text);
    preserved_user.preserve = Some(true);
    let final_assistant = plain_text_message("assistant", &final_report);
    let final_assistant_json = serde_json::to_string(&final_assistant).unwrap();
    let preserved_user_json = serde_json::to_string(&preserved_user).unwrap();
    let messages = vec![
        plain_text_message("user", &early_user),
        plain_text_message("assistant", &early_assistant),
        preserved_user,
        final_assistant,
        plain_text_message("user", "Use the preceding report."),
        plain_text_message("assistant", "The preceding report contains the evidence."),
    ];

    let output = recovery_sweep(messages, &[]);

    assert_eq!(output.stats.plain_text_reduced, 2);
    assert!(output.report.is_some());
    assert!(output.messages[0]
        .content
        .content_text_only()
        .starts_with("[compressed by deterministic recovery sweep:"));
    assert!(output.messages[1]
        .content
        .content_text_only()
        .starts_with("[compressed by deterministic recovery sweep:"));
    assert_eq!(
        serde_json::to_string(&output.messages[2]).unwrap(),
        preserved_user_json
    );
    assert_eq!(
        serde_json::to_string(&output.messages[3]).unwrap(),
        final_assistant_json
    );
    assert_eq!(
        output.messages[4].content.content_text_only(),
        "Use the preceding report."
    );
    assert_eq!(
        output.messages[5].content.content_text_only(),
        "The preceding report contains the evidence."
    );
}

#[test]
fn recovery_single_oversized_assistant_is_not_a_reduction() {
    let messages = vec![plain_text_message(
        "assistant",
        &oversized_text("only detailed report\n"),
    )];
    let before = serde_json::to_string(&messages).unwrap();

    let output = recovery_sweep(messages, &[]);

    assert_eq!(output.stats.plain_text_reduced, 0);
    assert!(!output.stats.has_meaningful_mutation());
    assert!(output.report.is_none());
    assert_eq!(serde_json::to_string(&output.messages).unwrap(), before);
}

#[test]
fn recovery_preserves_head_report_and_immutable_tail_byte_identically() {
    let final_report = oversized_text("head report\n");
    let head = vec![
        plain_text_message("user", &oversized_text("head user\n")),
        plain_text_message("assistant", &final_report),
    ];
    let immutable_tail = vec![
        plain_text_message("user", &oversized_text("tail user\n")),
        plain_text_message("assistant", &oversized_text("tail assistant\n")),
    ];
    let tail_json = serde_json::to_string(&immutable_tail).unwrap();
    let CompressChatApplyOutput {
        mut messages,
        stats,
        report,
        affected_boundary,
        modifiable_len,
        ..
    } = recovery_sweep(head, &immutable_tail);

    assert_eq!(stats.plain_text_reduced, 1);
    insert_current_compression_report(
        &mut messages,
        report,
        affected_boundary,
        modifiable_len,
    );
    messages.extend_from_slice(&immutable_tail);

    assert!(messages
        .iter()
        .any(|message| message.content.content_text_only() == final_report));
    assert_eq!(
        serde_json::to_string(&messages[messages.len() - immutable_tail.len()..]).unwrap(),
        tail_json
    );
}
