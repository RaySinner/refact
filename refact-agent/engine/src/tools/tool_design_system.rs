pub use refact_tools::tools::tool_design_system::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use regex::Regex;
#[cfg(test)]
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Map, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use walkdir::{DirEntry, WalkDir};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_code_intel_api::ToolJson;
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;

    fn source(path: &Path, relative: &str, text: &str) -> SourceFile {
        SourceFile {
            absolute: path.join(relative),
            relative: relative.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn css_custom_properties_preserve_light_and_dark_variants_in_dtcg() {
        let root = Path::new("/tmp/design-system-css");
        let files = vec![source(
            root,
            "styles/tokens.css",
            ":root, .dark { --brand-color: #112233; --space-sm: 8px; }\n.light { --brand-color: #fefefe; }",
        )];
        let tokens = extract_tokens(&files);
        assert_eq!(tokens["--brand-color"].variants["dark"], "#112233");
        assert_eq!(tokens["--brand-color"].variants["light"], "#fefefe");
        let dtcg = build_dtcg(&tokens);
        assert_eq!(dtcg["color"]["brand-color"]["$type"], "color");
        assert_eq!(
            dtcg["color"]["brand-color"]["$extensions"]["org.refact.variants"]["light"],
            "#fefefe"
        );
        assert_eq!(dtcg["spacing"]["space-sm"]["$value"], "8px");
    }

    #[test]
    fn tailwind_config_tokens_are_grouped_by_design_category() {
        let root = Path::new("/tmp/design-system-tailwind");
        let files = vec![source(
            root,
            "tailwind.config.ts",
            "export default { theme: { colors: { brand: '#123456' }, spacing: { card: '18px' }, zIndex: { modal: 700 } } }",
        )];
        let tokens = extract_tokens(&files);
        assert_eq!(tokens["colors.brand"].category, "color");
        assert_eq!(tokens["spacing.card"].category, "spacing");
        assert_eq!(tokens["zIndex.modal"].category, "z-index");
    }

    #[test]
    fn theme_yaml_tokens_are_extracted() {
        let root = Path::new("/tmp/design-system-theme-yaml");
        let files = vec![source(
            root,
            "theme.yaml",
            "colors:\n  brand: '#654321'\nspacing:\n  panel: 20px\n",
        )];
        let tokens = extract_tokens(&files);
        assert_eq!(tokens["colors.brand"].variants["default"], "#654321");
        assert_eq!(tokens["spacing.panel"].variants["default"], "20px");
    }

    #[test]
    fn drift_detection_ignores_tokenized_values_and_suggests_nearest_tokens() {
        let root = Path::new("/tmp/design-system-drift");
        let files = vec![
            source(
                root,
                "styles/tokens.css",
                ":root { --brand: #112233; --space-2: 8px; --z-modal: 700; }",
            ),
            source(
                root,
                "Button.module.css",
                ".ok { color: var(--brand); padding: var(--space-2); }\n.bad { color: #102234; padding: 9px; z-index: 710; }",
            ),
        ];
        let tokens = extract_tokens(&files);
        let (count, truncated, findings) = detect_drift(&files, &tokens, 10);
        assert_eq!(count, 3);
        assert!(!truncated);
        assert_eq!(findings[0].path, "Button.module.css");
        assert_eq!(findings[0].line, 2);
        assert_eq!(findings[0].nearest_token.as_deref(), Some("--brand"));
        assert_eq!(findings[1].nearest_token.as_deref(), Some("--space-2"));
        assert_eq!(findings[2].nearest_token.as_deref(), Some("--z-modal"));
    }

    #[test]
    fn component_inventory_extracts_props_variants_and_usage_counts_without_codegraph() {
        let root = Path::new("/tmp/design-system-components");
        let files = vec![
            source(
                root,
                "Button.tsx",
                "export interface ButtonProps { variant?: 'primary' | 'ghost'; disabled?: boolean }\nexport function Button({ variant, disabled }: ButtonProps) { return <button /> }",
            ),
            source(root, "Page.tsx", "export const Page = () => <Button />;"),
        ];
        let (inventory_source, count, _, components) =
            inventory_components(root, root, &files, None);
        assert_eq!(inventory_source, "filesystem");
        assert_eq!(count, 2);
        let button = components
            .iter()
            .find(|component| component.name == "Button")
            .unwrap();
        assert_eq!(button.props, vec!["disabled", "variant"]);
        assert_eq!(button.variants["variant"], vec!["ghost", "primary"]);
        assert_eq!(button.approx_usage_count, 1);
        assert_eq!(button.usage_count, button.approx_usage_count);
        assert_eq!(button.method, "identifier-grep");
        assert!(button.heuristic);
    }

    #[test]
    fn drift_findings_declare_themselves_as_heuristics() {
        let root = Path::new("/tmp/design-system-heuristic");
        let files = vec![
            source(root, "styles/tokens.css", ":root { --brand: #112233; }"),
            source(root, "Card.module.css", ".bad { color: #102234; }"),
        ];
        let tokens = extract_tokens(&files);
        let (_, _, findings) = detect_drift(&files, &tokens, 10);
        assert!(!findings.is_empty());
        for finding in &findings {
            assert!(finding.heuristic, "{finding:?}");
            assert_eq!(finding.method, "regex-scan-nearest-token");
        }
    }

    #[test]
    fn component_inventory_is_bounded_and_ranked_by_usage() {
        let root = Path::new("/tmp/design-system-bounded");
        let mut files = Vec::new();
        for index in 0..(MAX_COMPONENTS + 5) {
            files.push(source(
                root,
                Box::leak(format!("Comp{index}.tsx").into_boxed_str()),
                Box::leak(
                    format!("export function Comp{index}() {{ return null }}").into_boxed_str(),
                ),
            ));
        }
        files.push(source(
            root,
            "Page.tsx",
            "export const Page = () => <Comp0 />;",
        ));
        let (_, total, truncated, components) = inventory_components(root, root, &files, None);
        assert!(truncated, "{total} components should exceed the cap");
        assert_eq!(components.len(), MAX_COMPONENTS);
        assert_eq!(components.first().unwrap().name, "Comp0");
        assert_eq!(components.first().unwrap().approx_usage_count, 1);
        assert!(components
            .windows(2)
            .all(|pair| pair[0].approx_usage_count >= pair[1].approx_usage_count));
    }

    #[test]
    fn design_system_output_is_bounded_instead_of_unlimited() {
        let filter = design_system_output_filter();
        assert!(!filter.skip);
        assert_eq!(filter.limit_chars, MAX_OUTPUT_CHARS);
        assert_eq!(filter.limit_tokens, Some(MAX_OUTPUT_TOKENS));
        assert!(filter.limit_chars < usize::MAX);
    }

    #[test]
    fn empty_scope_reports_a_confident_empty_design_system() {
        let root = Path::new("/tmp/design-system-empty");
        let report = analyze_design_system(root, root, &[], None, true, true, 10, 0, false);
        assert!(!report.detected);
        assert_eq!(report.token_count, 0);
        assert_eq!(report.component_count, 0);
        assert_eq!(report.drift_count, 0);
        assert_eq!(report.looked_for.len(), 4);
    }

    #[test]
    fn repo_tokens_css_extracts_real_categorized_tokens() {
        let engine = Path::new(env!("CARGO_MANIFEST_DIR"));
        let path = engine.join("../gui/src/styles/tokens.css");
        let text = std::fs::read_to_string(&path).unwrap();
        let files = vec![SourceFile {
            absolute: path,
            relative: "refact-agent/gui/src/styles/tokens.css".to_string(),
            text,
        }];
        let tokens = extract_tokens(&files);
        assert!(tokens.len() > 80, "extracted {} tokens", tokens.len());
        assert_eq!(tokens["--rf-color-accent"].category, "color");
        assert_eq!(tokens["--rf-space-3"].category, "spacing");
        assert_eq!(tokens["--rf-radius-card"].category, "radius");
        assert_eq!(tokens["--rf-dur-fast"].category, "motion");
        assert!(tokens["--rf-bg"].variants.contains_key("dark"));
        assert!(tokens["--rf-bg"].variants.contains_key("light"));
        let dtcg = build_dtcg(&tokens);
        assert_eq!(dtcg["color"]["rf-bg"]["$type"], "color");
    }

    #[tokio::test]
    async fn component_inventory_uses_codegraph_usage_counts_when_available() {
        let root = tempfile::tempdir().unwrap();
        let component_path = root.path().join("Button.tsx");
        let page_path = root.path().join("Page.tsx");
        let component = "export interface ButtonProps { variant?: 'primary' | 'ghost' }\nexport function Button({ variant }: ButtonProps) { return variant }";
        let page = "export function Page() { return Button({ variant: 'primary' }) }";
        std::fs::write(&component_path, component).unwrap();
        std::fs::write(&page_path, page).unwrap();
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        service
            .index_file(&component_path.to_string_lossy(), component, "typescript")
            .await
            .unwrap();
        service
            .index_file(&page_path.to_string_lossy(), page, "typescript")
            .await
            .unwrap();
        service.connect_usages().await.unwrap();
        let graph = load_graph_facts(Some(&service)).await.unwrap();
        let files = vec![
            source(root.path(), "Button.tsx", component),
            source(root.path(), "Page.tsx", page),
        ];
        let (inventory_source, _, _, components) =
            inventory_components(root.path(), root.path(), &files, Some(&graph));
        let button = components
            .iter()
            .find(|component| component.name == "Button")
            .unwrap();
        assert_eq!(inventory_source, "codegraph+filesystem");
        assert_eq!(button.approx_usage_count, 1);
        assert_eq!(button.method, "codegraph-incoming-edges");
        assert!(!button.heuristic);
        assert!(button.source.starts_with("codegraph-generation-"));
    }
}
