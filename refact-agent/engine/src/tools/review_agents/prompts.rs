pub use refact_tools::tools::review_agents::prompts::*;

#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_agents::stages::{StageContract, StageSpec};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_scope::ReviewScope;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_types::ReviewFinding;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::review_agents::stages::embedded_catalog;
    use crate::tools::review_scope::DiffHunks;
    use crate::tools::review_types::{ReviewSeverity, ScopeMode};
    use std::path::PathBuf;

    fn scope(mode: ScopeMode) -> ReviewScope {
        ReviewScope {
            mode,
            requested: vec![PathBuf::from("src/lib.rs")],
            files: vec![PathBuf::from("src/lib.rs")],
            dropped_files: vec![],
            changed_files: vec![PathBuf::from("src/lib.rs")],
            focus: Some("browser lifecycle".to_string()),
            plan: Some("must never leak a profile".to_string()),
            base: Some("abc123".to_string()),
            head: Some("def456".to_string()),
            diff_patch: Some("@@ -1,2 +1,3 @@\n+let value = 1;\n".to_string()),
            patch_total_bytes: 0,
            hunks: DiffHunks::default(),
            repo_root: None,
            expansion: None,
        }
    }

    fn stage(id: &str) -> StageSpec {
        embedded_catalog()
            .into_iter()
            .find(|spec| spec.id == id)
            .unwrap()
    }

    fn finding() -> ReviewFinding {
        ReviewFinding {
            id: "rf-1234abcd".to_string(),
            stage: "diff".to_string(),
            model: None,
            title: "Error dropped".to_string(),
            severity: ReviewSeverity::High,
            file: "src/lib.rs".to_string(),
            line_start: 10,
            line_end: 12,
            claim: "The error arm returns success.".to_string(),
            evidence: "return Ok(());".to_string(),
            evidence_present: true,
            reproduction: None,
            fix: None,
            introduced_by_diff: true,
            out_of_scope: false,
            reported_by: vec!["diff".to_string()],
            locations: vec![],
            disputed: None,
        }
    }

    #[test]
    fn review_prompt_carries_task_scope_diff_and_contract() {
        let prompt = build_stage_prompt(&stage("mechanical"), &scope(ScopeMode::Strict), None, &[]);

        assert!(prompt.contains("# Your job this run: mechanical"));
        assert!(prompt.contains("browser lifecycle"));
        assert!(prompt.contains("must never leak a profile"));
        assert!(prompt.contains("```diff"));
        assert!(prompt.contains("base abc123"));
        assert!(prompt.contains("Scope mode is strict"));
        assert!(prompt.contains("Report only on the files listed above"));
        assert!(prompt.contains("\"line_start\":10"));
        assert!(prompt.contains("commands_run"));
        assert!(!prompt.contains("confidence"));
    }

    #[test]
    fn review_prompt_uses_verdict_contract_for_the_adversarial_stage() {
        let prompt = build_adversarial_prompt(
            &stage("adversarial"),
            &scope(ScopeMode::Broad),
            &[finding()],
            &[],
        );

        assert!(prompt.contains("rf-1234abcd"));
        assert!(prompt.contains("src/lib.rs:10-12"));
        assert!(prompt.contains("return Ok(());"));
        assert!(prompt.contains("reproduction: none given"));
        assert!(prompt.contains("\"verdict\":\"supported|unsupported\""));
        assert!(!prompt.contains("\"findings\":["));
    }

    #[test]
    fn review_prompt_truncates_a_huge_diff_on_a_file_boundary_and_quantifies_it() {
        let first = format!("diff --git a/a.rs b/a.rs\n{}", "+é\n".repeat(200));
        let second = format!("diff --git a/b.rs b/b.rs\n{}", "+x\n".repeat(200));
        let patch = format!("{first}{second}");
        let total = patch.len();

        let rendered = truncate_diff(&patch, first.len() + 20, total + 5_000);

        assert!(rendered.starts_with("diff --git a/a.rs b/a.rs"));
        assert!(!rendered.contains("diff --git a/b.rs b/b.rs"));
        assert!(rendered.contains(&format!(
            "⚠️ showing {} of {} bytes (limit: review_diff_char_cap = {})",
            first.len(),
            total + 5_000,
            first.len() + 20
        )));
        assert!(rendered.contains("💡 Raise review_diff_char_cap in trajectory settings"));
        assert!(!rendered.contains("[diff truncated]"));
    }

    #[test]
    fn review_prompt_keeps_a_diff_that_fits_under_the_cap_intact() {
        let patch = "diff --git a/a.rs b/a.rs\n+one\n";

        assert_eq!(truncate_diff(patch, 4_096, patch.len()), patch);
    }

    #[test]
    fn review_prompt_reads_the_diff_cap_from_the_runtime_setting() {
        assert_eq!(
            diff_char_cap(),
            refact_core::trajectory_settings::current().review_diff_char_cap
        );
        assert!(diff_char_cap() >= 4_096);
    }

    #[test]
    fn review_prompt_names_the_files_that_were_dropped_from_scope() {
        let mut truncated = scope(ScopeMode::Strict);
        truncated.dropped_files = vec![
            PathBuf::from("src/dropped_a.rs"),
            PathBuf::from("src/dropped_b.rs"),
        ];

        let prompt = build_stage_prompt(&stage("diff"), &truncated, None, &[]);

        assert!(prompt.contains("# Files NOT reviewed (dropped by the max_files cap) (2)"));
        assert!(prompt.contains("- src/dropped_a.rs"));
        assert!(prompt.contains("- src/dropped_b.rs"));
    }

    #[test]
    fn review_prompt_makes_the_browser_scenario_authoritative() {
        let prompt = build_stage_prompt(
            &stage("browser"),
            &scope(ScopeMode::Broad),
            Some("  open /settings, toggle dark mode, expect no console errors  "),
            &[],
        );

        assert!(prompt.contains("# Scenario the caller requires you to run"));
        assert!(prompt.contains("open /settings, toggle dark mode"));
        assert!(prompt.contains("do not substitute a scenario of your own"));
        assert!(!build_stage_prompt(
            &stage("browser"),
            &scope(ScopeMode::Broad),
            Some("   "),
            &[]
        )
        .contains("# Scenario the caller requires"));
    }

    #[test]
    fn review_prompt_puts_user_steering_first() {
        let steering = vec!["  please also check the retry path  ".to_string()];

        let stage_prompt = build_stage_prompt(
            &stage("mechanical"),
            &scope(ScopeMode::Strict),
            None,
            &steering,
        );
        assert!(stage_prompt.starts_with(
            "# User steering (arrived while the review was running)\n- please also check the retry path\n\n# Your job this run: mechanical"
        ));

        let adversarial = build_adversarial_prompt(
            &stage("adversarial"),
            &scope(ScopeMode::Broad),
            &[finding()],
            &steering,
        );
        assert!(adversarial.starts_with(
            "# User steering (arrived while the review was running)\n- please also check the retry path\n\n# Your job this run: adversarial"
        ));
    }

    #[test]
    fn review_prompt_is_unchanged_without_steering() {
        let steering: Vec<String> = Vec::new();
        let blank = vec!["   ".to_string()];

        let stage_prompt = build_stage_prompt(
            &stage("mechanical"),
            &scope(ScopeMode::Strict),
            None,
            &steering,
        );
        let adversarial = build_adversarial_prompt(
            &stage("adversarial"),
            &scope(ScopeMode::Broad),
            &[finding()],
            &steering,
        );

        assert!(stage_prompt.starts_with("# Your job this run: mechanical"));
        assert!(adversarial.starts_with("# Your job this run: adversarial"));
        assert!(!stage_prompt.contains("# User steering"));
        assert!(!adversarial.contains("# User steering"));
        assert_eq!(
            build_stage_prompt(
                &stage("mechanical"),
                &scope(ScopeMode::Strict),
                None,
                &blank
            ),
            stage_prompt
        );

        let mut spec = stage("diff");
        spec.task = "Review this change".to_string();
        spec.fallback = None;
        spec.preferred_tools.clear();
        let scope = scope(ScopeMode::Strict);
        assert_eq!(
            build_stage_prompt(&spec, &scope, None, &[]),
            format!("# Your job this run: diff\nReview this change\n\n{}\n{FINDINGS_CONTRACT}\n", scope_block(&scope))
        );
        assert_eq!(
            build_adversarial_prompt(&spec, &scope, &[], &[]),
            format!("# Your job this run: diff\nReview this change\n\n{}\n# Findings to check\n\n{VERDICTS_CONTRACT}\n", scope_block(&scope))
        );
    }

    #[test]
    fn review_system_prompt_states_the_no_finding_and_evidence_rules() {
        assert!(REVIEW_SYSTEM_PROMPT.contains("NO_FINDING is a valid"));
        assert!(REVIEW_SYSTEM_PROMPT.contains("quote the exact source lines"));
        assert!(REVIEW_SYSTEM_PROMPT.contains("set it to null"));
    }
}
