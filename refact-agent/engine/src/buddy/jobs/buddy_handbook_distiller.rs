pub use refact_runtime_api::buddy_jobs::buddy_handbook_distiller::*;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::knowledge_index::KnowledgeCard;
    use std::path::PathBuf;

    fn card(title: &str, path: &str) -> KnowledgeCard {
        KnowledgeCard {
            id: title.to_string(),
            title: title.to_string(),
            summary: Some(format!("{} summary", title)),
            description: None,
            tags: vec!["testing".to_string()],
            filenames: vec![],
            entities: vec![],
            related_files: vec![],
            related_entities: vec![],
            kind: Some("insight".to_string()),
            created: Some("2026-07-01".to_string()),
            created_at: None,
            updated: None,
            file_path: PathBuf::from(path),
        }
    }

    #[test]
    fn slug_normalizes_tags() {
        assert_eq!(handbook_slug("Memory Garden!"), "memory-garden");
        assert_eq!(handbook_slug("diag:cluster"), "diag-cluster");
    }

    #[test]
    fn render_includes_sources_and_titles() {
        let cards = vec![card("First insight", "/k/a.md"), card("Second", "/k/b.md")];
        let doc = render_handbook_doc("testing", &cards);
        assert!(doc.contains("# Handbook: testing"));
        assert!(doc.contains("## First insight"));
        assert!(doc.contains("source: `/k/a.md`"));
        assert!(doc.contains("2 recurring insight(s)"));
    }
}
