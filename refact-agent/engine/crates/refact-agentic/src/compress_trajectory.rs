use refact_core::chat_types::ChatMessage;
use refact_privacy::FileRecord;

#[derive(Debug, Clone, PartialEq)]
pub struct CompressedTrajectory {
    pub text: String,
    pub records: Vec<FileRecord>,
}

pub fn source_records(messages: &[ChatMessage]) -> Result<Vec<FileRecord>, String> {
    refact_privacy::records_to_carry(messages).map_err(|error| error.to_string())
}

/// Messages fed to reconstruction: the active-context view unless the trajectory
/// still needs an explicit legacy rebuild.
pub fn reconstruction_input(messages: &[ChatMessage]) -> Result<Vec<ChatMessage>, String> {
    use refact_core::active_context::{active_context, legacy_rebuild_input, requires_explicit_rebuild};
    if requires_explicit_rebuild(messages) {
        legacy_rebuild_input(messages)
    } else {
        active_context(messages).map(|view| view.messages)
    }
    .map_err(|e| e.to_string())
}

pub fn render_messages(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| format!("[{}]\n{}", m.role, m.content.content_text_only()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn compress(
    input: &[ChatMessage],
    reconstructed: &[ChatMessage],
) -> Result<CompressedTrajectory, String> {
    Ok(CompressedTrajectory {
        text: render_messages(reconstructed),
        records: source_records(input)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::chat_types::ChatContent;
    use refact_privacy::{Attribution, PrivacyRecord};

    fn message(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: ChatContent::SimpleText(text.into()),
            ..Default::default()
        }
    }

    #[test]
    fn source_records_unions_and_deduplicates_privacy_metadata() {
        let secret = FileRecord {
            path: ".env".to_string(),
            zone: "secrets".to_string(),
            attribution: Attribution::Declared,
        };
        let internal = FileRecord {
            path: "src/lib.rs".to_string(),
            zone: "internal".to_string(),
            attribution: Attribution::Observed,
        };
        let inert = FileRecord {
            path: "src/main.rs".to_string(),
            zone: "normal".to_string(),
            attribution: Attribution::Observed,
        };
        let mut first = message("user", "first");
        first.extra.insert(
            "privacy".to_string(),
            serde_json::to_value(PrivacyRecord {
                files: vec![secret.clone(), internal.clone(), inert],
            })
            .unwrap(),
        );
        let mut second = message("assistant", "second");
        second.extra.insert(
            "privacy".to_string(),
            serde_json::to_value(PrivacyRecord {
                files: vec![secret.clone()],
            })
            .unwrap(),
        );

        assert_eq!(
            source_records(&[first, second]).unwrap(),
            vec![secret, internal]
        );
    }

    #[test]
    fn render_messages_tags_each_role() {
        let rendered = render_messages(&[
            message("user", "hello"),
            message("assistant", "hi there"),
        ]);
        assert_eq!(rendered, "[user]\nhello\n\n[assistant]\nhi there");
    }

    #[test]
    fn compress_renders_reconstructed_messages_and_keeps_source_records() {
        let mut input = message("user", "original");
        input.extra.insert(
            "privacy".to_string(),
            serde_json::to_value(PrivacyRecord {
                files: vec![FileRecord {
                    path: "src/lib.rs".to_string(),
                    zone: "internal".to_string(),
                    attribution: Attribution::Observed,
                }],
            })
            .unwrap(),
        );

        let compressed = compress(&[input], &[message("user", "summary")]).unwrap();

        assert_eq!(compressed.text, "[user]\nsummary");
        assert_eq!(compressed.records.len(), 1);
        assert_eq!(compressed.records[0].path, "src/lib.rs");
    }
}
