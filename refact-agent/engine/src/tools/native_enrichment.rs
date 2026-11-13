pub use refact_tools::tools::native_enrichment::*;

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_relative_path_requires_a_safe_workspace_path() {
        let root = crate::test_paths::abs("workspace");
        let source = crate::test_paths::abs("workspace/src/lib.rs");
        assert_eq!(
            workspace_relative_path(&source, &[root.clone()]),
            Some("src/lib.rs".to_string())
        );
        assert_eq!(
            workspace_relative_path(Path::new("../secret.rs"), &[root]),
            None
        );
        assert_eq!(
            workspace_relative_path(Path::new("/outside/secret.rs"), &[]),
            None
        );
    }

    #[test]
    fn native_references_are_bounded_and_keep_the_first_range() {
        let root = crate::test_paths::abs("workspace");
        let source = crate::test_paths::abs("workspace/src/lib.rs");
        let mut references = NativeReferences::new();
        references.add_path(&source, &[root.clone()], 2, 4, "match", Some(0.9), "test");
        references.add_path(&source, &[root], 6, 8, "match", Some(0.8), "test");
        assert_eq!(references.references.len(), 2);
        for index in 0..MAX_REFERENCES {
            references.add_symbol(&format!("symbol_{index}"), "definition", "test");
        }

        let mut message = ChatMessage::new("tool".to_string(), "raw result".to_string());
        references.attach(&mut message);
        let enrichment = refact_chat_api::tool_enrichment_from_extra(&message.extra).unwrap();

        assert_eq!(enrichment.references.len(), MAX_REFERENCES);
        assert!(enrichment.truncated);
        assert_eq!(enrichment.references[0].target, "src/lib.rs");
        assert_eq!(
            enrichment.references[0].label.as_deref(),
            Some("src/lib.rs:2-4")
        );
        assert_eq!(
            enrichment.references[1].label.as_deref(),
            Some("src/lib.rs:6-8")
        );
        assert_eq!(message.content.content_text_only(), "raw result");
    }
}
