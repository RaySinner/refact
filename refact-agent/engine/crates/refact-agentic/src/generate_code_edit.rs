use refact_core::chat_types::{ChatContent, ChatMessage};

pub const SUBAGENT_ID: &str = "code_edit";

pub fn validate_request(code: &str, instruction: &str) -> Result<(), String> {
    if code.is_empty() {
        return Err("The provided code is empty".to_string());
    }
    if instruction.is_empty() {
        return Err("The instruction is empty".to_string());
    }
    Ok(())
}

pub fn build_messages(
    system_prompt: Option<&str>,
    code: &str,
    instruction: &str,
    cursor_file: &str,
    cursor_line: i32,
) -> Result<Vec<ChatMessage>, String> {
    let system_prompt = system_prompt.ok_or_else(|| {
        format!(
            "messages.system_prompt not defined for subagent '{}'",
            SUBAGENT_ID
        )
    })?;

    let user_message = format!(
        "File: {} (line {})\n\nCode to edit:\n```\n{}\n```\n\nInstruction: {}",
        cursor_file, cursor_line, code, instruction
    );

    Ok(vec![
        ChatMessage {
            role: "system".to_string(),
            content: ChatContent::SimpleText(system_prompt.to_string()),
            ..Default::default()
        },
        ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText(user_message),
            ..Default::default()
        },
    ])
}

pub fn extract_edited_code(messages: &[ChatMessage]) -> Result<String, String> {
    let edited_code = messages
        .last()
        .and_then(|msg| match &msg.content {
            ChatContent::SimpleText(text) => Some(text.clone()),
            _ => None,
        })
        .ok_or("No edited code was generated".to_string())?;

    Ok(remove_markdown_fences(&edited_code))
}

pub fn remove_markdown_fences(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        let lines: Vec<&str> = trimmed.lines().collect();
        if lines.len() >= 2 {
            if let Some(end_idx) = lines.iter().rposition(|l| l.trim() == "```") {
                if end_idx > 0 {
                    let start_idx = 1;
                    if start_idx < end_idx {
                        return lines[start_idx..end_idx].join("\n");
                    }
                }
            }
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_markdown_fences_with_language() {
        let input = "```python\ndef hello():\n    print('world')\n```";
        assert_eq!(
            remove_markdown_fences(input),
            "def hello():\n    print('world')"
        );
    }

    #[test]
    fn test_remove_markdown_fences_without_language() {
        let input = "```\nsome code\n```";
        assert_eq!(remove_markdown_fences(input), "some code");
    }

    #[test]
    fn test_remove_markdown_fences_no_fences() {
        let input = "plain code without fences";
        assert_eq!(remove_markdown_fences(input), "plain code without fences");
    }

    #[test]
    fn test_remove_markdown_fences_with_whitespace() {
        let input = "  ```rust\nfn main() {}\n```  ";
        assert_eq!(remove_markdown_fences(input), "fn main() {}");
    }

    #[test]
    fn validate_request_rejects_empty_inputs() {
        assert_eq!(
            validate_request("", "instruction").unwrap_err(),
            "The provided code is empty"
        );
        assert_eq!(
            validate_request("code", "").unwrap_err(),
            "The instruction is empty"
        );
        assert!(validate_request("code", "instruction").is_ok());
    }

    #[test]
    fn build_messages_requires_system_prompt() {
        let error = build_messages(None, "code", "fix", "a.rs", 3).unwrap_err();
        assert_eq!(
            error,
            "messages.system_prompt not defined for subagent 'code_edit'"
        );
    }

    #[test]
    fn build_messages_places_file_and_instruction() {
        let messages = build_messages(
            Some("system"),
            "let x = 1;",
            "make it two",
            "src/a.rs",
            7,
        )
        .unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[0].content.content_text_only(), "system");
        assert_eq!(messages[1].role, "user");
        let user = messages[1].content.content_text_only();
        assert!(user.contains("File: src/a.rs (line 7)"));
        assert!(user.contains("let x = 1;"));
        assert!(user.contains("Instruction: make it two"));
    }

    #[test]
    fn extract_edited_code_unwraps_fences_and_rejects_empty() {
        let messages = vec![ChatMessage::new(
            "assistant".to_string(),
            "```rust\nfn main() {}\n```".to_string(),
        )];
        assert_eq!(extract_edited_code(&messages).unwrap(), "fn main() {}");
        assert_eq!(
            extract_edited_code(&[]).unwrap_err(),
            "No edited code was generated"
        );
    }
}
