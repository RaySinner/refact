use refact_core::chat_types::{ChatContent, ChatMessage};

pub const SUBAGENT_ID: &str = "commit_message";

/// The diff command for a detected VCS, or `None` when the VCS is unknown.
pub fn diff_command_for_vcs(vcs_type: &str) -> Option<&'static str> {
    match vcs_type {
        "git" => Some("git"),
        "svn" => Some("svn"),
        "hg" => Some("hg"),
        _ => None,
    }
}

pub fn validate_diff(diff: &str) -> Result<(), String> {
    if diff.is_empty() {
        return Err("The provided diff is empty".to_string());
    }
    Ok(())
}

pub fn build_messages(
    diff: &str,
    commit_message_prompt: Option<&str>,
    diff_only_prompt: Option<&str>,
    diff_with_user_text_prompt: Option<&str>,
) -> Result<Vec<ChatMessage>, String> {
    if let Some(text) = commit_message_prompt {
        let system_prompt = diff_with_user_text_prompt.ok_or_else(|| {
            format!(
                "prompts.diff_with_user_text not defined for subagent '{}'",
                SUBAGENT_ID
            )
        })?;
        Ok(vec![
            ChatMessage {
                role: "system".to_string(),
                content: ChatContent::SimpleText(system_prompt.to_string()),
                ..Default::default()
            },
            ChatMessage {
                role: "user".to_string(),
                content: ChatContent::SimpleText(format!(
                    "Commit message:\n```\n{}\n```\nDiff:\n```\n{}\n```\n",
                    text, diff
                )),
                ..Default::default()
            },
        ])
    } else {
        let system_prompt = diff_only_prompt.ok_or_else(|| {
            format!(
                "prompts.diff_only not defined for subagent '{}'",
                SUBAGENT_ID
            )
        })?;
        Ok(vec![
            ChatMessage {
                role: "system".to_string(),
                content: ChatContent::SimpleText(system_prompt.to_string()),
                ..Default::default()
            },
            ChatMessage {
                role: "user".to_string(),
                content: ChatContent::SimpleText(format!("Diff:\n```\n{}\n```\n", diff)),
                ..Default::default()
            },
        ])
    }
}

pub fn extract_commit_message(messages: &[ChatMessage]) -> Result<String, String> {
    let commit_message = messages
        .last()
        .and_then(|last_m| match &last_m.content {
            ChatContent::SimpleText(text) => Some(text.clone()),
            _ => None,
        })
        .ok_or("No commit message was generated".to_string())?;

    let code_blocks = remove_fencing(&commit_message);
    if !code_blocks.is_empty() {
        Ok(code_blocks[0].clone())
    } else {
        Ok(commit_message)
    }
}

pub fn remove_fencing(message: &String) -> Vec<String> {
    let trimmed_message = message.trim();
    if !trimmed_message.contains("```") {
        return Vec::new();
    }
    if trimmed_message.contains("``````") {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut in_code_block = false;

    for (_i, part) in trimmed_message.split("```").enumerate() {
        if in_code_block {
            let part_lines: Vec<&str> = part.lines().collect();
            if !part_lines.is_empty() {
                let start_idx = if part_lines[0].trim().split_whitespace().count() <= 1
                    && part_lines.len() > 1
                {
                    1
                } else {
                    0
                };
                if start_idx < part_lines.len() {
                    let code_block = part_lines[start_idx..].join("\n");
                    if !code_block.is_empty() {
                        results.push(code_block.trim().to_string());
                    }
                }
            }
        }

        in_code_block = !in_code_block;
    }
    if !results.is_empty() {
        results
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_fencing() {
        let input = "Simple text without fencing".to_string();
        assert_eq!(remove_fencing(&input), Vec::<String>::new());
    }

    #[test]
    fn test_simple_fencing() {
        let input = "```\nCode block\n```".to_string();
        assert_eq!(remove_fencing(&input), vec!["Code block".to_string()]);
    }

    #[test]
    fn test_language_tag() {
        let input = "```rust\nfn main() {\n    println!(\"Hello\");\n}\n```".to_string();
        assert_eq!(
            remove_fencing(&input),
            vec!["fn main() {\n    println!(\"Hello\");\n}".to_string()]
        );
    }

    #[test]
    fn test_text_before_and_after() {
        let input = "Text before\nText before\n```\nCode block\n```\nText after".to_string();
        assert_eq!(remove_fencing(&input), vec!["Code block".to_string()]);
    }

    #[test]
    fn test_multiple_code_blocks() {
        let input = "First paragraph\n```\nFirst code\n```\nMiddle text\n```python\ndef hello():\n    print('world')\n```\nLast paragraph".to_string();
        assert_eq!(
            remove_fencing(&input),
            vec![
                "First code".to_string(),
                "def hello():\n    print('world')".to_string()
            ]
        );
    }

    #[test]
    fn test_empty_code_block() {
        let input = "Text with `````` empty block".to_string();
        assert_eq!(remove_fencing(&input), Vec::<String>::new());
    }

    #[test]
    fn diff_command_for_vcs_maps_known_controls_only() {
        assert_eq!(diff_command_for_vcs("git"), Some("git"));
        assert_eq!(diff_command_for_vcs("svn"), Some("svn"));
        assert_eq!(diff_command_for_vcs("hg"), Some("hg"));
        assert_eq!(diff_command_for_vcs("fossil"), None);
    }

    #[test]
    fn validate_diff_rejects_empty() {
        assert_eq!(
            validate_diff("").unwrap_err(),
            "The provided diff is empty"
        );
        assert!(validate_diff("diff --git a b").is_ok());
    }

    #[test]
    fn build_messages_uses_user_text_prompt_when_provided() {
        let messages = build_messages("diff body", Some("hint"), None, Some("sys")).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.content_text_only(), "sys");
        let user = messages[1].content.content_text_only();
        assert!(user.contains("Commit message:\n```\nhint\n```"));
        assert!(user.contains("Diff:\n```\ndiff body\n```"));
    }

    #[test]
    fn build_messages_uses_diff_only_prompt_without_user_text() {
        let messages = build_messages("diff body", None, Some("diff-only"), Some("sys")).unwrap();
        assert_eq!(messages[0].content.content_text_only(), "diff-only");
        assert_eq!(
            messages[1].content.content_text_only(),
            "Diff:\n```\ndiff body\n```\n"
        );
    }

    #[test]
    fn build_messages_reports_missing_prompts() {
        assert_eq!(
            build_messages("diff", Some("hint"), None, None).unwrap_err(),
            "prompts.diff_with_user_text not defined for subagent 'commit_message'"
        );
        assert_eq!(
            build_messages("diff", None, None, Some("sys")).unwrap_err(),
            "prompts.diff_only not defined for subagent 'commit_message'"
        );
    }

    #[test]
    fn extract_commit_message_prefers_first_code_block() {
        let messages = vec![ChatMessage::new(
            "assistant".to_string(),
            "```\nfeat: add reeds\n```".to_string(),
        )];
        assert_eq!(
            extract_commit_message(&messages).unwrap(),
            "feat: add reeds"
        );

        let plain = vec![ChatMessage::new("assistant".to_string(), "raw".to_string())];
        assert_eq!(extract_commit_message(&plain).unwrap(), "raw");
        assert_eq!(
            extract_commit_message(&[]).unwrap_err(),
            "No commit message was generated"
        );
    }
}
