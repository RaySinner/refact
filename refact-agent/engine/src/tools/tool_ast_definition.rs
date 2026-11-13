pub use refact_tools::tools::tool_ast_definition::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use crate::tools::tools_description::Tool;
#[cfg(test)]
use refact_core::chat_types::ChatMessage;
#[cfg(test)]
use refact_core::chat_types::ChatContent;
#[cfg(test)]
use refact_core::chat_types::ContextEnum;
#[cfg(test)]
use refact_core::chat_types::ContextFile;
#[cfg(test)]
use refact_core::knowledge_index::KnowledgeCard;
#[cfg(test)]
use refact_worktrees::scope::ExecutionScope;
#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::privacy_types::{FilePrivacySettings, PrivacySettings};
    use refact_core::worktree_meta::WorktreeMeta;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    fn no_abort() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[tokio::test]
    async fn symbol_def_hierarchy_sections_include_type_chain() {
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        service
            .index_file("src/base.py", "class A:\n    pass\n", "python")
            .await
            .unwrap();
        service
            .index_file("src/mid.py", "class B(A):\n    pass\n", "python")
            .await
            .unwrap();
        service
            .index_file("src/leaf.py", "class C(B):\n    pass\n", "python")
            .await
            .unwrap();
        service.connect_usages().await.unwrap();

        let defs = service.definitions("B").await.unwrap();
        let sections = type_hierarchy_sections(&service, &defs, &no_abort())
            .await
            .unwrap();
        let rendered = sections.join("\n");

        assert!(rendered.contains("Inheritance for `B`"));
        assert!(rendered.contains("A"));
        assert!(rendered.contains("B"));
        assert!(rendered.contains("C"));
    }

    struct ScopeFixture {
        _temp: tempfile::TempDir,
        worktree: WorktreeMeta,
        root: PathBuf,
        source: PathBuf,
    }

    fn make_scope_fixture() -> ScopeFixture {
        let temp = tempfile::Builder::new()
            .prefix("refact-symbol-def-scope-")
            .tempdir()
            .unwrap();
        let root = temp
            .path()
            .join(".cache")
            .join("refact")
            .join("worktrees")
            .join("wt")
            .join("engine");
        let source = temp.path().join("source");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(source.join("src")).unwrap();
        fs::write(root.join("src").join("lib.rs"), "pub fn shared() {}\n").unwrap();
        fs::write(source.join("src").join("lib.rs"), "pub fn shared() {}\n").unwrap();
        fs::write(
            source.join("src").join("source_only.rs"),
            "pub fn only_source() {}\n",
        )
        .unwrap();
        let root = dunce::simplified(&fs::canonicalize(root).unwrap()).to_path_buf();
        let source = dunce::simplified(&fs::canonicalize(source).unwrap()).to_path_buf();
        let worktree = WorktreeMeta {
            id: "wt-symbol-def".to_string(),
            kind: "chat".to_string(),
            root: root.clone(),
            source_workspace_root: source.clone(),
            repo_root: source.clone(),
            branch: Some("feature".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: Some("base".to_string()),
            task_id: None,
            card_id: None,
            agent_id: None,
            enforce: true,
        };
        ScopeFixture {
            _temp: temp,
            worktree,
            root,
            source,
        }
    }

    async fn scope_gcx(blocked: Vec<String>) -> Arc<crate::global_context::GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            let privacy_settings = gcx.privacy_settings.clone();
            *privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
                privacy_rules: FilePrivacySettings {
                    only_send_to_servers_I_control: vec![],
                    blocked,
                },
                loaded_ts: u64::MAX / 2,
            });
        }
        gcx
    }

    fn related_memory(path: PathBuf, filename: &str, entity: &str) -> KnowledgeCard {
        KnowledgeCard {
            id: path.to_string_lossy().to_string(),
            title: "Related memory".to_string(),
            summary: Some("Related memory summary".to_string()),
            description: None,
            tags: vec![],
            filenames: vec![filename.to_string()],
            entities: vec![entity.to_string()],
            related_files: vec![],
            related_entities: vec![],
            kind: Some("memory".to_string()),
            created: None,
            created_at: None,
            updated: None,
            file_path: path,
        }
    }

    async fn add_related_memory(
        gcx: Arc<crate::global_context::GlobalContext>,
        path: PathBuf,
        filename: &str,
        entity: &str,
    ) {
        gcx.knowledge_index
            .lock()
            .await
            .add_card(related_memory(path, filename, entity));
    }

    fn context_file_names(results: &[ContextEnum]) -> Vec<String> {
        results
            .iter()
            .filter_map(|item| match item {
                ContextEnum::ContextFile(file) => Some(file.file_name.replace('\\', "/")),
                _ => None,
            })
            .collect()
    }

    fn tool_text(results: &[ContextEnum]) -> String {
        results
            .iter()
            .filter_map(|item| match item {
                ContextEnum::ChatMessage(message) => match &message.content {
                    ChatContent::SimpleText(text) => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .replace('\\', "/")
    }

    async fn index_absolute(
        service: &Arc<refact_codegraph::CodeGraphService>,
        path: &PathBuf,
        text: &str,
    ) {
        service
            .index_file(&path.to_string_lossy(), text, "rust")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn symbol_def_source_context_file_remaps_to_worktree() {
        let fixture = make_scope_fixture();
        let gcx = scope_gcx(vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        let source_lib = fixture.source.join("src").join("lib.rs");
        index_absolute(&service, &source_lib, "pub fn shared() {}\n").await;
        service.connect_usages().await.unwrap();

        let worktree_lib = fixture
            .root
            .join("src")
            .join("lib.rs")
            .to_string_lossy()
            .replace('\\', "/");

        let (_corrections, results) = symbol_def_via_codegraph(
            gcx.clone(),
            service,
            &["shared".to_string()],
            "shared",
            &"call".to_string(),
            Some(&scope),
            no_abort(),
        )
        .await
        .unwrap();

        let names = context_file_names(&results);
        let text = tool_text(&results);
        assert!(
            names.iter().any(|n| n == &worktree_lib),
            "expected {worktree_lib} in {names:?}"
        );
        let source_lib_str = fixture
            .source
            .join("src")
            .join("lib.rs")
            .to_string_lossy()
            .replace('\\', "/");
        assert!(
            text.contains("Definitions for `shared`"),
            "definitions header missing: {text}"
        );
        assert!(
            !text.contains(&format!("defined at {source_lib_str}")),
            "definition line must not use source path: {text}"
        );
        let enrichment = results
            .iter()
            .find_map(|result| match result {
                ContextEnum::ChatMessage(message) if message.role == "tool" => {
                    refact_chat_api::tool_enrichment_from_extra(&message.extra)
                }
                _ => None,
            })
            .expect("definition enrichment");
        assert_eq!(enrichment.references[0].target, "shared");
        assert_eq!(enrichment.references[0].status.as_deref(), Some("found"));
        assert!(enrichment
            .references
            .iter()
            .any(|reference| reference.target == "src/lib.rs"));
    }

    #[tokio::test]
    async fn symbol_def_source_only_context_file_is_dropped() {
        let fixture = make_scope_fixture();
        let gcx = scope_gcx(vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        let source_only = fixture.source.join("src").join("source_only.rs");
        index_absolute(&service, &source_only, "pub fn only_source() {}\n").await;
        service.connect_usages().await.unwrap();

        let (corrections, results) = symbol_def_via_codegraph(
            gcx.clone(),
            service,
            &["only_source".to_string()],
            "only_source",
            &"call".to_string(),
            Some(&scope),
            no_abort(),
        )
        .await
        .unwrap();

        let names = context_file_names(&results);
        let text = tool_text(&results);
        assert!(
            names.is_empty(),
            "source-only defs should be dropped: {names:?}"
        );
        assert!(corrections, "dropped defs should count as a correction");
        assert!(
            !text.contains("source_only.rs"),
            "must not leak source path: {text}"
        );
        assert!(
            text.contains("suppressed"),
            "should mention suppression: {text}"
        );
    }

    #[tokio::test]
    async fn symbol_def_blocked_context_file_is_suppressed() {
        let fixture = make_scope_fixture();
        let gcx = scope_gcx(vec!["*.rs".to_string()]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        let worktree_lib = fixture.root.join("src").join("lib.rs");
        index_absolute(&service, &worktree_lib, "pub fn shared() {}\n").await;
        service.connect_usages().await.unwrap();

        let result = symbol_def_via_codegraph(
            gcx.clone(),
            service,
            &["shared".to_string()],
            "shared",
            &"call".to_string(),
            Some(&scope),
            no_abort(),
        )
        .await;

        assert!(result.is_err(), "privacy-blocked defs should error out");
        let err = result.unwrap_err();
        assert!(
            err.contains("Blocked"),
            "error should mention Blocked: {err}"
        );
    }

    #[tokio::test]
    async fn symbol_def_symbol_count_cap_rejects_excess() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                refact_runtime_api::app_state_from_gcx(gcx).await,
                4096,
                20,
                false,
                vec![],
                "chat".to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ));
        let mut tool = ToolAstDefinition {
            config_path: String::new(),
        };
        let many = (0..MAX_SYMBOLS + 5)
            .map(|i| format!("sym{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let args = HashMap::from_iter([("symbols".to_string(), Value::String(many))]);

        let err = tool
            .tool_execute(ccx, &"call".to_string(), &args)
            .await
            .unwrap_err();
        assert!(err.contains("Too many symbols"), "{err}");
    }

    #[tokio::test]
    async fn symbol_def_abort_stops_processing() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        service
            .index_file("src/lib.rs", "pub fn shared() {}\n", "rust")
            .await
            .unwrap();
        service.connect_usages().await.unwrap();

        let abort_flag = Arc::new(AtomicBool::new(true));
        let (_corrections, results) = symbol_def_via_codegraph(
            gcx.clone(),
            service,
            &["shared".to_string()],
            "shared",
            &"call".to_string(),
            None,
            abort_flag,
        )
        .await
        .unwrap();

        let names = context_file_names(&results);
        let text = tool_text(&results);
        assert!(
            names.is_empty(),
            "aborted call should not attach defs: {names:?}"
        );
        assert!(text.contains("Aborted"), "should note the abort: {text}");
    }

    #[tokio::test]
    async fn symbol_def_related_memory_stays_in_sidecar_enrichment() {
        let fixture = make_scope_fixture();
        let gcx = scope_gcx(vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        let worktree_lib = fixture.root.join("src").join("lib.rs");
        index_absolute(&service, &worktree_lib, "pub fn shared() {}\n").await;
        service.connect_usages().await.unwrap();
        let (_corrections, baseline_results) = symbol_def_via_codegraph(
            gcx.clone(),
            service.clone(),
            &["shared".to_string()],
            "shared",
            &"call".to_string(),
            Some(&scope),
            no_abort(),
        )
        .await
        .unwrap();
        let baseline_text = tool_text(&baseline_results);
        let memory_path = fixture
            .root
            .join(".refact")
            .join("knowledge")
            .join("shared.md");
        fs::create_dir_all(memory_path.parent().unwrap()).unwrap();
        fs::write(&memory_path, "related\n").unwrap();
        add_related_memory(
            gcx.clone(),
            memory_path.clone(),
            &worktree_lib.to_string_lossy(),
            "shared",
        )
        .await;

        let (_corrections, results) = symbol_def_via_codegraph(
            gcx,
            service,
            &["shared".to_string()],
            "shared",
            &"call".to_string(),
            Some(&scope),
            no_abort(),
        )
        .await
        .unwrap();

        let text = tool_text(&results);
        assert_eq!(text, baseline_text);
        let enrichment = results
            .iter()
            .find_map(|result| match result {
                ContextEnum::ChatMessage(message) if message.role == "tool" => {
                    refact_chat_api::tool_enrichment_from_extra(&message.extra)
                }
                _ => None,
            })
            .expect("definition enrichment");
        assert!(enrichment.references.iter().any(|reference| {
            reference.target == ".refact/knowledge/shared.md"
                && reference.status.as_deref() == Some("related_memory")
        }));
    }

    #[tokio::test]
    async fn related_memory_lookup_releases_global_index_before_local_lookups() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        add_related_memory(
            gcx.clone(),
            PathBuf::from("/knowledge/shared.md"),
            "src/lib.rs",
            "shared",
        )
        .await;
        let lock = gcx.knowledge_index.lock().await;
        let abort_flag = no_abort();
        let lookup = compute_related_memories(
            &gcx.knowledge_index,
            vec!["src/lib.rs".to_string()],
            "shared",
            &abort_flag,
        );
        tokio::pin!(lookup);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut lookup)
                .await
                .is_err(),
            "related-memory lookup should wait only for the shared index lock"
        );
        drop(lock);
        assert_eq!(lookup.await.len(), 1);
    }

    #[tokio::test]
    async fn aborted_symbol_def_skips_related_memory_lookup() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        service
            .index_file("src/lib.rs", "pub fn shared() {}\n", "rust")
            .await
            .unwrap();
        service.connect_usages().await.unwrap();
        let abort_flag = Arc::new(AtomicBool::new(true));
        let index_lock = gcx.knowledge_index.clone();
        let lock = index_lock.try_lock().expect("test owns index mutex");

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            symbol_def_via_codegraph(
                gcx,
                service,
                &["shared".to_string()],
                "shared",
                &"call".to_string(),
                None,
                abort_flag,
            ),
        )
        .await
        .expect("aborted symbol lookup must not attempt a related-memory index lock")
        .unwrap();
        drop(lock);

        assert!(tool_text(&result.1).contains("Aborted"));
    }

    #[test]
    fn related_memory_identifier_regex_is_static_and_filters_invalid_entities() {
        let first = std::ptr::addr_of!(*IDENTIFIER_RE);
        let second = std::ptr::addr_of!(*IDENTIFIER_RE);
        assert_eq!(first, second);
        assert_eq!(
            related_memory_entities("crate.shared, valid::Thing, invalid-name"),
            vec![
                "Thing".to_string(),
                "crate::shared".to_string(),
                "shared".to_string(),
                "valid::Thing".to_string(),
            ]
        );
    }
}
