use serde::Deserialize;

use refact_core::chat_types::{ChatContent, ChatMessage};
use refact_core::custom_error::MapErrToString;
use refact_core::json_utils;

pub const SUBAGENT_ID: &str = "follow_up";

#[derive(Deserialize, Clone)]
pub struct FollowUpResponse {
    pub follow_ups: Vec<String>,
    pub topic_changed: bool,
}

pub fn system_prompt(system_prompt: Option<&str>) -> Result<String, String> {
    system_prompt.map(str::to_string).ok_or_else(|| {
        format!(
            "messages.system_prompt not defined for subagent '{}'",
            SUBAGENT_ID
        )
    })
}

pub fn last_response_text(messages: &[ChatMessage]) -> Result<String, String> {
    messages
        .last()
        .and_then(|last_m| match &last_m.content {
            ChatContent::SimpleText(text) => Some(text.clone()),
            _ => None,
        })
        .ok_or("No follow-up message was generated".to_string())
}

pub fn parse_follow_up_response(text: &str) -> Result<FollowUpResponse, String> {
    json_utils::extract_json_object(text).map_err_with_prefix("Failed to parse json:")
}

pub fn make_conversation(messages: &Vec<ChatMessage>, system_prompt: &str) -> Vec<ChatMessage> {
    let mut history_message = "*Conversation:*\n".to_string();
    for m in messages.iter().rev().take(2) {
        let content = m.content.to_text_with_image_placeholders();
        let char_count = content.chars().count();
        let limited_content = if char_count > 5000 {
            let skip_count = char_count - 5000;
            format!(
                "...{}",
                content.chars().skip(skip_count).collect::<String>()
            )
        } else {
            content
        };
        let message_row = match m.role.as_str() {
            "user" => format!("👤:{}\n\n", limited_content),
            "assistant" => format!("🤖:{}\n\n", limited_content),
            _ => continue,
        };
        history_message.insert_str(0, &message_row);
    }
    vec![
        ChatMessage::new("system".to_string(), system_prompt.to_string()),
        ChatMessage::new("user".to_string(), history_message),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_conversation_keeps_last_two_dialog_turns() {
        let messages = vec![
            ChatMessage::new("user".to_string(), "first".to_string()),
            ChatMessage::new("assistant".to_string(), "second".to_string()),
            ChatMessage::new("user".to_string(), "third".to_string()),
        ];
        let conversation = make_conversation(&messages, "system prompt");

        assert_eq!(conversation.len(), 2);
        assert_eq!(conversation[0].content.content_text_only(), "system prompt");
        let history = conversation[1].content.content_text_only();
        assert!(!history.contains("first"));
        assert!(history.contains("third"));
        assert!(history.contains("second"));
    }

    #[test]
    fn make_conversation_truncates_long_content() {
        let messages = vec![
            ChatMessage::new("user".to_string(), "a".repeat(6000)),
            ChatMessage::new("user".to_string(), "b".repeat(6000)),
        ];
        let history = make_conversation(&messages, "sys")[1]
            .content
            .content_text_only();
        assert!(history.contains("..."));
        assert!(history.chars().count() < 11_000);
    }

    #[test]
    fn system_prompt_requires_config_value() {
        assert_eq!(
            system_prompt(None).unwrap_err(),
            "messages.system_prompt not defined for subagent 'follow_up'"
        );
        assert_eq!(system_prompt(Some("hello")).unwrap(), "hello");
    }

    #[test]
    fn last_response_text_reads_final_simple_text() {
        let messages = vec![ChatMessage::new("assistant".to_string(), "ok".to_string())];
        assert_eq!(last_response_text(&messages).unwrap(), "ok");
        assert_eq!(
            last_response_text(&[]).unwrap_err(),
            "No follow-up message was generated"
        );
    }

    #[test]
    fn parse_follow_up_response_extracts_json_object() {
        let response = parse_follow_up_response(
            "sure\n```json\n{\"follow_ups\": [\"a\"], \"topic_changed\": false}\n```",
        )
        .unwrap();
        assert_eq!(response.follow_ups, vec!["a".to_string()]);
        assert!(!response.topic_changed);
        assert!(parse_follow_up_response("no json here")
            .unwrap_err()
            .starts_with("Failed to parse json:"));
    }
}
