pub use refact_tools::tools::review_agents::stages::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::BTreeMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use serde::Deserialize;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::review_types::ReviewDepth;
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn review_stage_stale_budget_minutes_key_still_parses_and_is_carried_for_the_warning() {
        let stale = "id: diff\ntask: look\nbudget_minutes: 20\n";
        let spec = parse_stage("stale.yaml", stale).unwrap();
        assert_eq!(spec.budget_minutes, Some(20));
    }

    #[test]
    fn review_stage_catalog_ships_every_stage_with_a_task_and_contract() {
        let catalog = embedded_catalog();
        let ids: Vec<&str> = catalog.iter().map(|spec| spec.id.as_str()).collect();

        for expected in STAGE_ORDER {
            assert!(ids.contains(expected), "missing stage {expected}");
        }
        for spec in &catalog {
            assert!(!spec.task.trim().is_empty(), "{} has no task", spec.id);
        }
        let adversarial = catalog
            .iter()
            .find(|spec| spec.id == "adversarial")
            .unwrap();
        assert_eq!(adversarial.phase, StagePhase::PostMerge);
        assert_eq!(adversarial.contract, StageContract::Verdicts);
        let execution = catalog.iter().find(|spec| spec.id == "execution").unwrap();
        assert!(execution.writes_allowed);
        assert!(!catalog
            .iter()
            .filter(|spec| spec.id != "execution")
            .any(|spec| spec.writes_allowed));
    }

    // search_semantic is gated on vecdb, which a test gcx has no cheap way to provide, so it is
    // exempt here; every other stage tool must resolve against the real registry.
    const VECDB_GATED_STAGE_TOOLS: &[&str] = &["search_semantic"];

    #[tokio::test]
    async fn review_stage_catalog_references_only_registered_tools() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.codegraph.lock().await = Some(std::sync::Arc::new(
            refact_codegraph::CodeGraphService::open_in_memory().unwrap(),
        ));
        let registered: HashSet<String> = crate::tools::tools_list::get_available_tools(gcx)
            .await
            .into_iter()
            .map(|tool| tool.tool_description().name)
            .collect();

        let mut missing = Vec::new();
        for spec in embedded_catalog() {
            for tool in spec.tools() {
                if VECDB_GATED_STAGE_TOOLS.contains(&tool.as_str()) {
                    continue;
                }
                if !registered.contains(&tool) {
                    missing.push(format!("{}: {tool}", spec.id));
                }
            }
        }

        assert!(missing.is_empty(), "unregistered stage tools: {missing:?}");
    }

    #[test]
    fn review_stage_every_stage_can_run_shell() {
        for spec in embedded_catalog() {
            assert!(
                spec.tools().contains(&"shell".to_string()),
                "{} cannot run shell",
                spec.id
            );
        }
    }

    #[test]
    fn review_stage_applies_when_skips_browser_for_a_rust_only_diff() {
        let catalog = embedded_catalog();
        let rust_only = strings(&["src/lib.rs", "src/tools/mod.rs"]);

        let selection =
            select_stages(catalog.clone(), ReviewDepth::Deep, None, &rust_only).unwrap();
        let scheduled: Vec<&str> = selection
            .scheduled
            .iter()
            .map(|spec| spec.id.as_str())
            .collect();

        assert!(!scheduled.contains(&"browser"));
        assert!(scheduled.contains(&"execution"));
        assert!(selection
            .skipped
            .iter()
            .any(|(id, reason)| id == "browser" && reason.contains("matching files")));

        let with_gui = strings(&["src/lib.rs", "gui/src/App.tsx"]);
        let selection = select_stages(catalog, ReviewDepth::Deep, None, &with_gui).unwrap();
        assert!(selection.scheduled.iter().any(|spec| spec.id == "browser"));
    }

    #[test]
    fn review_stage_depth_normal_excludes_deep_and_opt_in_stages() {
        let files = strings(&["gui/src/App.tsx"]);

        let selection =
            select_stages(embedded_catalog(), ReviewDepth::Normal, None, &files).unwrap();
        let scheduled: Vec<&str> = selection
            .scheduled
            .iter()
            .map(|spec| spec.id.as_str())
            .collect();

        assert_eq!(
            scheduled,
            [
                "mechanical",
                "diff",
                "impact",
                "spec",
                "security",
                "dependencies",
                "simplicity"
            ]
        );
        assert!(selection
            .skipped
            .iter()
            .any(|(id, reason)| id == "concurrency" && reason == "opt-in stage"));
        assert!(selection
            .skipped
            .iter()
            .any(|(id, reason)| id == "execution" && reason == "depth normal"));
    }

    #[test]
    fn review_stage_explicit_stage_list_overrides_depth_and_rejects_unknown_names() {
        let files = strings(&["src/lib.rs"]);
        let requested = strings(&["concurrency", "mechanical"]);

        let selection = select_stages(
            embedded_catalog(),
            ReviewDepth::Normal,
            Some(&requested),
            &files,
        )
        .unwrap();
        let scheduled: Vec<&str> = selection
            .scheduled
            .iter()
            .map(|spec| spec.id.as_str())
            .collect();
        assert_eq!(scheduled, ["mechanical", "concurrency"]);

        let error = select_stages(
            embedded_catalog(),
            ReviewDepth::Normal,
            Some(&strings(&["typo_stage"])),
            &files,
        )
        .unwrap_err();
        assert!(error.contains("unknown stage(s): typo_stage"));
        assert!(error.contains("available stages:"));
    }

    #[test]
    fn review_stage_glob_and_extension_matching() {
        let applies = AppliesWhen {
            always: false,
            extensions: strings(&["tsx"]),
            path_globs: strings(&["**/migrations/*.sql"]),
        };

        assert!(applies.matches(&strings(&["gui/src/App.tsx"])));
        assert!(applies.matches(&strings(&["db/migrations/001_init.sql"])));
        assert!(!applies.matches(&strings(&["src/lib.rs"])));
        assert!(AppliesWhen {
            always: true,
            ..Default::default()
        }
        .matches(&[]));
    }
}
