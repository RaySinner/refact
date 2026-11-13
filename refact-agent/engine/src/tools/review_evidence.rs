pub use refact_tools::tools::review_evidence::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::path::PathBuf;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_scope::{paths_match, ReviewScope};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_types::ReviewFinding;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::review_scope::DiffHunks;
    use crate::tools::review_types::{ReviewSeverity, ScopeMode};

    const FILE: &str = "fn one() {\n    let value = 1;\n}\n\nfn two() {\n    return Ok(());\n}\n";

    fn scope_for(root: &std::path::Path, file: &std::path::Path) -> ReviewScope {
        ReviewScope {
            mode: ScopeMode::Strict,
            requested: vec![file.to_path_buf()],
            files: vec![file.to_path_buf()],
            dropped_files: vec![],
            changed_files: vec![],
            focus: None,
            plan: None,
            base: None,
            head: None,
            diff_patch: None,
            patch_total_bytes: 0,
            hunks: DiffHunks::default(),
            repo_root: Some(root.to_path_buf()),
            expansion: None,
        }
    }

    fn finding(file: &str, line_start: u32, line_end: u32, evidence: &str) -> ReviewFinding {
        ReviewFinding {
            id: String::new(),
            stage: "diff".to_string(),
            model: None,
            title: "title".to_string(),
            severity: ReviewSeverity::High,
            file: file.to_string(),
            line_start,
            line_end,
            claim: "claim".to_string(),
            evidence: evidence.to_string(),
            evidence_present: false,
            reproduction: None,
            fix: None,
            introduced_by_diff: false,
            out_of_scope: false,
            reported_by: vec![],
            locations: vec![],
            disputed: None,
        }
    }

    #[tokio::test]
    async fn review_evidence_confirms_quote_inside_the_window() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lib.rs");
        std::fs::write(&path, FILE).unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let scope = scope_for(temp.path(), &path);
        let mut findings = vec![finding("lib.rs", 6, 6, "return Ok(());")];

        let outcome = verify_evidence(gcx, &scope, &mut findings).await;

        assert!(findings[0].evidence_present);
        assert_eq!(findings[0].line_start, 6);
        assert_eq!(outcome.present, 1);
        assert_eq!(outcome.relocated, 0);
    }

    #[tokio::test]
    async fn review_evidence_relocates_a_quote_found_elsewhere_in_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lib.rs");
        std::fs::write(&path, FILE).unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let scope = scope_for(temp.path(), &path);
        let mut findings = vec![finding("lib.rs", 120, 124, "    return Ok(());")];

        let outcome = verify_evidence(gcx, &scope, &mut findings).await;

        assert!(findings[0].evidence_present);
        assert_eq!(findings[0].line_start, 6);
        assert_eq!(findings[0].line_end, 6);
        assert_eq!(outcome.relocated, 1);
    }

    #[tokio::test]
    async fn review_evidence_rejects_a_quote_that_is_not_in_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lib.rs");
        std::fs::write(&path, FILE).unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let scope = scope_for(temp.path(), &path);
        let mut findings = vec![
            finding("lib.rs", 2, 2, "let value = 999;"),
            finding("nowhere.rs", 1, 1, "let value = 1;"),
        ];

        let outcome = verify_evidence(gcx, &scope, &mut findings).await;

        assert!(!findings[0].evidence_present);
        assert!(!findings[1].evidence_present);
        assert_eq!(outcome.present, 0);
        assert_eq!(outcome.unreadable, 1);
    }

    #[tokio::test]
    async fn review_evidence_accepts_quotes_with_line_number_prefixes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lib.rs");
        std::fs::write(&path, FILE).unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let scope = scope_for(temp.path(), &path);
        let mut findings = vec![finding(
            "lib.rs",
            5,
            7,
            "5: fn two() {\n6:     return Ok(());",
        )];

        verify_evidence(gcx, &scope, &mut findings).await;

        assert!(findings[0].evidence_present);
    }

    #[test]
    fn review_evidence_normalizes_whitespace_and_drops_blank_lines() {
        assert_eq!(
            normalize_snippet("  let   a = 1;  \n\n\tlet b = 2;\n"),
            "let a = 1;\nlet b = 2;"
        );
    }
}
