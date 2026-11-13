pub use refact_runtime_api::scope_utils::*;

#[cfg(test)]
mod worktree_scope_read_tools {
    use super::*;
    use std::path::{Path, PathBuf};
    use refact_core::chat_types::ContextFile;
    use crate::tools::ctx::ToolCtx;
    use refact_worktrees::scope::ExecutionScope;
    use refact_runtime_api::GlobalContextCtx;
    use refact_runtime_api::{AtCommand, AtCommandsContext};
    use crate::at_commands::{AtFile, AtTree};
    use crate::at_commands::execute_at::AtCommandMember;
    use refact_core::chat_types::{ChatContent, ContextEnum};
    use refact_core::privacy_types::{FilePrivacySettings, PrivacySettings};
    use crate::tools::tool_cat::ToolCat;
    use crate::tools::tool_tree::ToolTree;
    use crate::tools::tools_description::Tool;
    use refact_worktrees::types::WorktreeMeta;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::fs;
    use tokio::sync::Mutex as AMutex;
    use std::sync::Arc;

    use crate::global_context::GlobalContext;

    fn tool_ctx(gcx: &Arc<GlobalContext>) -> ToolCtx {
        gcx.tool_ctx(gcx.clone())
    }

    struct MockVecdb {
        records: Vec<crate::vecdb::vdb_structs::VecdbRecord>,
    }

    #[async_trait]
    impl crate::vecdb::vdb_structs::VecdbSearch for MockVecdb {
        async fn vecdb_search(
            &self,
            query: String,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<crate::vecdb::vdb_structs::SearchResult, String> {
            Ok(crate::vecdb::vdb_structs::SearchResult {
                query_text: query,
                results: self.records.clone(),
            })
        }

        async fn get_status(&self) -> Result<crate::vecdb::vdb_structs::VecDbStatus, String> {
            Ok(crate::vecdb::vdb_structs::VecDbStatus {
                files_unprocessed: 0,
                files_total: self.records.len(),
                requests_made_since_start: 0,
                vectors_made_since_start: 0,
                db_size: 0,
                db_cache_size: 0,
                state: "done".to_string(),
                queue_additions: false,
                vecdb_max_files_hit: false,
                vecdb_errors: Default::default(),
                cache_writes_failed: 0,
                vectors_missing_from_cache: 0,
            })
        }

        async fn remove_file(&self, _file_path: &PathBuf) -> Result<(), String> {
            Ok(())
        }

        async fn vectorizer_enqueue_files(
            &self,
            _documents: &[String],
            _process_immediately: bool,
            _roots: refact_core::memory_plane::MemoryPlaneRoots,
        ) {
        }

        fn current_constants(&self) -> (crate::vecdb::vdb_structs::EmbeddingModelConfig, usize) {
            (
                crate::vecdb::vdb_structs::EmbeddingModelConfig {
                    model_id: String::new(),
                    endpoint: String::new(),
                    endpoint_style: String::new(),
                    embedding_endpoint_style: String::new(),
                    api_key: String::new(),
                    model_name: String::new(),
                    embedding_size: 0,
                    dimensions: None,
                    query_prefix: String::new(),
                    document_prefix: String::new(),
                    rejection_threshold: 0.0,
                    embedding_batch: 1,
                    n_ctx: 0,
                },
                0,
            )
        }

        async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, String> {
            Ok(vec![])
        }

        async fn vecdb_search_with_embedding(
            &self,
            _embedding: &Vec<f32>,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<Vec<crate::vecdb::vdb_structs::VecdbRecord>, String> {
            Ok(self.records.clone())
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        worktree: WorktreeMeta,
        root: PathBuf,
        source: PathBuf,
        outside: PathBuf,
    }

    fn make_fixture() -> Fixture {
        let temp = tempfile::Builder::new()
            .prefix("refact-worktree-scope-")
            .tempdir()
            .unwrap();
        let root = temp
            .path()
            .join(".cache")
            .join("refact")
            .join("worktrees")
            .join("wt")
            .join("refact-agent")
            .join("engine");
        let source = temp.path().join("source");
        let outside = temp.path().join("outside");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(source.join("src")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            root.join("src").join("lib.rs"),
            "fn worktree_version() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src").join("worktree_only.rs"),
            "pub fn only_worktree() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src").join("new_name.rs"),
            "pub fn renamed_marker() {}\n",
        )
        .unwrap();
        fs::write(
            source.join("src").join("lib.rs"),
            "fn source_version() {}\n",
        )
        .unwrap();
        fs::write(
            source.join("src").join("source_only.rs"),
            "pub fn only_source() {}\n",
        )
        .unwrap();
        fs::write(
            source.join("src").join("deleted_stale.rs"),
            "pub fn deleted_marker() {}\n",
        )
        .unwrap();
        fs::write(
            source.join("src").join("old_name.rs"),
            "pub fn renamed_marker() {}\n",
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("sibling").join("src")).unwrap();
        fs::write(
            temp.path().join("sibling").join("src").join("lib.rs"),
            "fn sibling_version() {}\n",
        )
        .unwrap();
        fs::write(outside.join("allowed.txt"), "outside allowed\n").unwrap();
        fs::write(outside.join("blocked.blocked"), "outside blocked\n").unwrap();
        let root = dunce::simplified(&fs::canonicalize(root).unwrap()).to_path_buf();
        let source = dunce::simplified(&fs::canonicalize(source).unwrap()).to_path_buf();
        let outside = dunce::simplified(&fs::canonicalize(outside).unwrap()).to_path_buf();
        let worktree = WorktreeMeta {
            id: "wt-read-tools".to_string(),
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
        Fixture {
            _temp: temp,
            worktree,
            root,
            source,
            outside,
        }
    }

    async fn make_gcx_with_privacy(
        fixture: &Fixture,
        controlled: Vec<String>,
        blocked: Vec<String>,
    ) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let workspace_files = vec![
            fixture.source.join("src").join("lib.rs"),
            fixture.source.join("src").join("source_only.rs"),
            fixture.source.join("src").join("deleted_stale.rs"),
            fixture.source.join("src").join("old_name.rs"),
        ];
        {
            let locked = gcx.clone();
            let privacy_settings = locked.privacy_settings.clone();
            let workspace_folders = locked.documents_state.workspace_folders.clone();
            let workspace_files_lock = locked.documents_state.workspace_files.clone();
            drop(locked);
            *privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
                privacy_rules: FilePrivacySettings {
                    only_send_to_servers_I_control: controlled,
                    blocked,
                },
                loaded_ts: u64::MAX / 2,
            });
            *workspace_folders.lock().unwrap() = vec![fixture.source.clone()];
            *workspace_files_lock.lock().unwrap() = workspace_files;
        }
        gcx
    }

    async fn make_gcx(fixture: &Fixture, blocked: Vec<String>) -> Arc<GlobalContext> {
        make_gcx_with_privacy(fixture, vec![], blocked).await
    }

    async fn install_mock_vecdb(gcx: Arc<GlobalContext>, records: Vec<(&PathBuf, &str)>) {
        let records = records
            .into_iter()
            .enumerate()
            .map(
                |(idx, (path, _label))| crate::vecdb::vdb_structs::VecdbRecord {
                    vector: None,
                    file_path: path.clone(),
                    start_line: 0,
                    end_line: 0,
                    distance: idx as f32,
                    usefulness: 100.0 - idx as f32,
                },
            )
            .collect();
        *gcx.vec_db.lock().await = Some(Arc::new(MockVecdb { records }));
    }

    async fn make_ccx(
        gcx: Arc<GlobalContext>,
        worktree: WorktreeMeta,
    ) -> Arc<AMutex<AtCommandsContext>> {
        Arc::new(AMutex::new(
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
                Some(worktree),
            )
            .await,
        ))
    }

    fn tool_text(results: &[ContextEnum]) -> String {
        results
            .iter()
            .filter_map(|item| match item {
                ContextEnum::ChatMessage(message) => match &message.content {
                    ChatContent::SimpleText(text) => Some(text.clone()),
                    ChatContent::Multimodal(parts) => Some(
                        parts
                            .iter()
                            .filter(|part| part.m_type == "text")
                            .map(|part| part.m_content.clone())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    ChatContent::ContextFiles(_) => None,
                },
                ContextEnum::ContextFile(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn context_file_names(results: &[ContextEnum]) -> Vec<String> {
        results
            .iter()
            .filter_map(|item| match item {
                ContextEnum::ContextFile(file) => Some(file.file_name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Normalize a path string to forward slashes for cross-platform test comparison.
    /// On Windows, tool results may use backslashes; PathBuf::join also produces backslashes.
    fn norm_str(s: &str) -> String {
        s.replace('\\', "/")
    }

    fn norm(p: std::path::PathBuf) -> String {
        norm_str(&p.to_string_lossy())
    }

    fn cat_args(path: String) -> HashMap<String, Value> {
        HashMap::from_iter([("paths".to_string(), Value::String(path))])
    }

    fn semantic_args(query: &str) -> HashMap<String, Value> {
        HashMap::from_iter([
            ("queries".to_string(), Value::String(query.to_string())),
            ("scope".to_string(), Value::String("workspace".to_string())),
        ])
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_tree_defaults_to_worktree_root() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolTree {
            config_path: String::new(),
        };
        let tool_call_id = "tree-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &HashMap::new())
            .await
            .unwrap();
        let text = tool_text(&results);

        assert!(text.contains("worktree_only.rs"), "{text}");
        assert!(text.contains("src"), "{text}");
        assert!(text.contains("lib.rs"), "{text}");
        assert!(!text.contains("No files found"), "{text}");
        assert!(!text.contains("source_only.rs"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_tree_absolute_worktree_subdir_warns_and_lists() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolTree {
            config_path: String::new(),
        };
        let tool_call_id = "tree-call".to_string();
        let path = fixture.root.join("src").to_string_lossy().to_string();
        let args = HashMap::from_iter([("path".to_string(), Value::String(path))]);

        let (_corrections, results) = tool.tool_execute(ccx, &tool_call_id, &args).await.unwrap();
        let text = tool_text(&results);

        assert!(!text.contains("Worktree scope notices"), "{text}");
        assert!(text.contains("lib.rs"), "{text}");
        assert!(text.contains("worktree_only.rs"), "{text}");
        assert!(!text.contains("No files found"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_tree_hidden_child_dir_still_skipped() {
        let fixture = make_fixture();
        fs::create_dir_all(fixture.root.join(".git")).unwrap();
        fs::write(fixture.root.join(".git").join("config"), "[core]\n").unwrap();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolTree {
            config_path: String::new(),
        };
        let tool_call_id = "tree-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &HashMap::new())
            .await
            .unwrap();
        let text = tool_text(&results);

        assert!(text.contains("lib.rs"), "{text}");
        assert!(!text.contains(".git"), "{text}");
        assert!(!text.contains("config"), "{text}");
        assert!(!text.contains("No files found"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_at_tree_absolute_worktree_subdir_lists() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let at_tree = AtTree::new();
        let mut cmd = AtCommandMember::new("cmd".to_string(), "@tree".to_string(), 1, 6);
        let path = fixture.root.join("src").to_string_lossy().to_string();
        let mut args = vec![AtCommandMember::new("arg".to_string(), path, 7, 10)];

        let (results, replacement) = at_tree.at_execute(ccx, &mut cmd, &mut args).await.unwrap();
        let text = tool_text(&results);

        assert_eq!(replacement, "");
        assert!(!text.contains("Worktree scope notices"), "{text}");
        assert!(text.contains("lib.rs"), "{text}");
        assert!(text.contains("worktree_only.rs"), "{text}");
        assert!(!text.contains("tree(): directory is empty"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_tree_aliases_use_worktree_root() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let tool_call_id = "tree-call".to_string();

        for path in [".", "workspace"] {
            let ccx = make_ccx(gcx.clone(), fixture.worktree.clone()).await;
            let mut tool = ToolTree {
                config_path: String::new(),
            };
            let args = HashMap::from_iter([("path".to_string(), Value::String(path.to_string()))]);

            let (_corrections, results) =
                tool.tool_execute(ccx, &tool_call_id, &args).await.unwrap();
            let text = tool_text(&results);

            assert!(text.contains("worktree_only.rs"), "{text}");
            assert!(!text.contains("source_only.rs"), "{text}");
        }
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_cat_relative_uses_worktree_file() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let tool_call_id = "cat-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args("src/lib.rs".to_string()))
            .await
            .unwrap();
        let names = context_file_names(&results);

        assert_eq!(names, vec![norm(fixture.root.join("src").join("lib.rs"))]);
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_duplicate_relative_path_prefers_worktree() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let tool_call_id = "cat-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args("src/lib.rs".to_string()))
            .await
            .unwrap();
        let names = context_file_names(&results);
        let text = tool_text(&results);

        assert_eq!(names.len(), 1);
        assert_eq!(names[0], norm(fixture.root.join("src").join("lib.rs")));
        assert!(
            !norm_str(&text).contains(&norm(fixture.source.clone())),
            "{text}"
        );
        assert!(!text.contains("sibling"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_cat_absolute_worktree_warns() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let tool_call_id = "cat-call".to_string();
        let path = fixture
            .root
            .join("src")
            .join("lib.rs")
            .to_string_lossy()
            .to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args(path.clone()))
            .await
            .unwrap();
        let text = tool_text(&results);
        let names = context_file_names(&results);

        assert!(!text.contains("Worktree scope notices"), "{text}");
        assert_eq!(names, vec![norm_str(&path)]);
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_cat_absolute_source_remaps() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let tool_call_id = "cat-call".to_string();
        let source_path = fixture
            .source
            .join("src")
            .join("lib.rs")
            .to_string_lossy()
            .to_string();
        let worktree_path = norm(fixture.root.join("src").join("lib.rs"));

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args(source_path))
            .await
            .unwrap();
        let text = tool_text(&results);
        let names = context_file_names(&results);

        assert!(text.contains("mapped to active worktree"), "{text}");
        assert_eq!(names, vec![worktree_path]);
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_cat_outside_allowed_warns_and_blocked_is_rejected() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let tool_call_id = "cat-call".to_string();
        let outside_allowed = fixture
            .outside
            .join("allowed.txt")
            .to_string_lossy()
            .to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args(outside_allowed.clone()))
            .await
            .unwrap();
        let text = tool_text(&results);
        let names = context_file_names(&results);

        assert!(text.contains("STRONG NOTICE"), "{text}");
        assert_eq!(names, vec![norm_str(&outside_allowed)]);

        let blocked_path = fixture
            .outside
            .join("blocked.blocked")
            .to_string_lossy()
            .to_string();
        let gcx = make_gcx(&fixture, vec!["*.blocked".to_string()]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args(blocked_path))
            .await
            .unwrap();
        let text = tool_text(&results);
        assert!(context_file_names(&results).is_empty());
        assert!(text.contains("privacy level Blocked"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_resolve_workspace_scope_lists_worktree_only() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);

        let resolved = resolve_scope_with_execution_scope(&tool_ctx(&gcx), Some(&scope), "workspace")
            .await
            .unwrap();

        assert!(resolved
            .files
            .iter()
            .any(|path| path.ends_with("worktree_only.rs")));
        assert!(resolved
            .files
            .iter()
            .any(|path| path.ends_with("new_name.rs")));
        assert!(!resolved
            .files
            .iter()
            .any(|path| path.ends_with("source_only.rs")));
        assert!(!resolved
            .files
            .iter()
            .any(|path| path.ends_with("deleted_stale.rs")));
        assert!(!resolved
            .files
            .iter()
            .any(|path| path.ends_with("old_name.rs")));
    }

    #[tokio::test]
    async fn worktree_scope_limited_resolution_reports_truncation_notice() {
        let fixture = make_fixture();
        for i in 0..20 {
            fs::write(
                fixture.root.join("src").join(format!("extra_{:02}.rs", i)),
                "pub fn extra() {}\n",
            )
            .unwrap();
        }
        let gcx = make_gcx(&fixture, vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);

        let resolved = resolve_scope_with_execution_scope_limited(
            &tool_ctx(&gcx),
            Some(&scope),
            "workspace",
            2,
            None,
        )
                .await
                .unwrap();

        assert!(
            resolved.notices.iter().any(|n| n.contains("truncated")),
            "expected a truncation notice, got: {:?}",
            resolved.notices
        );
    }

    #[tokio::test]
    async fn model_context_existing_path_allows_controlled_but_not_blocked() {
        let fixture = make_fixture();
        let controlled = fixture.root.join("src/context.controlled");
        let blocked = fixture.root.join("src/context.blocked");
        fs::write(&controlled, "controlled\n").unwrap();
        fs::write(&blocked, "blocked\n").unwrap();
        let gcx = make_gcx_with_privacy(
            &fixture,
            vec!["*.controlled".to_string()],
            vec!["*.blocked".to_string()],
        )
        .await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);

        assert!(resolve_existing_path_with_execution_scope(
            &tool_ctx(&gcx),
            Some(&scope),
            "src/context.controlled",
        )
        .await
        .is_err());
        let resolved = resolve_existing_path_with_execution_scope_for_model_context(
            &tool_ctx(&gcx),
            Some(&scope),
            "src/context.controlled",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(resolved.path, controlled);
        assert!(
            resolve_existing_path_with_execution_scope_for_model_context(
                &tool_ctx(&gcx),
                Some(&scope),
                "src/context.blocked",
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn model_context_limited_scope_includes_controlled_but_omits_blocked() {
        let fixture = make_fixture();
        fs::write(fixture.root.join("src/context.controlled"), "controlled\n").unwrap();
        fs::write(fixture.root.join("src/context.blocked"), "blocked\n").unwrap();
        let gcx = make_gcx_with_privacy(
            &fixture,
            vec!["*.controlled".to_string()],
            vec!["*.blocked".to_string()],
        )
        .await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);

        let strict = resolve_scope_with_execution_scope_limited(
            &tool_ctx(&gcx),
            Some(&scope),
            "src/",
            100,
            None,
        )
        .await
        .unwrap();
        assert!(!strict
            .files
            .iter()
            .any(|path| path.ends_with("context.controlled")));

        let model_context = resolve_scope_with_execution_scope_limited_for_model_context(
            &tool_ctx(&gcx),
            Some(&scope),
            "src/",
            100,
            None,
        )
        .await
        .unwrap();
        assert!(model_context
            .files
            .iter()
            .any(|path| path.ends_with("context.controlled")));
        assert!(!model_context
            .files
            .iter()
            .any(|path| path.ends_with("context.blocked")));
    }

    #[tokio::test]
    async fn legacy_scope_limited_resolution_reports_truncation_notice() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        {
            let mut files = gcx.documents_state.workspace_files.lock().unwrap();
            files.extend((0..10).map(|index| fixture.source.join(format!("extra_{index}.rs"))));
        }

        let resolved =
            resolve_scope_with_execution_scope_limited(&tool_ctx(&gcx), None, "workspace", 2, None)
                .await
                .unwrap();

        assert_eq!(resolved.files.len(), 2);
        assert!(resolved
            .notices
            .iter()
            .any(|notice| notice.contains("truncated")));
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_search_pattern_uses_worktree_only() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = crate::tools::tool_regex_search::ToolRegexSearch {
            config_path: String::new(),
        };
        let tool_call_id = "search-call".to_string();
        let args = HashMap::from_iter([
            ("pattern".to_string(), Value::String("version".to_string())),
            ("scope".to_string(), Value::String("workspace".to_string())),
        ]);

        let (_corrections, results) = tool.tool_execute(ccx, &tool_call_id, &args).await.unwrap();
        let names = context_file_names(&results);
        let text = tool_text(&results);

        let expected = norm(fixture.root.join("src/lib.rs"));
        assert!(
            names.iter().any(|path| path == &expected),
            "expected {expected} in {names:?}"
        );
        assert!(!names.iter().any(|path| path.contains("source_only.rs")));
        assert!(text.contains("worktree_version"), "{text}");
        assert!(!text.contains("source_version"), "{text}");
        assert!(!text.contains(&norm(fixture.source.clone())), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_search_pattern_finds_worktree_only_file_content() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = crate::tools::tool_regex_search::ToolRegexSearch {
            config_path: String::new(),
        };
        let tool_call_id = "search-call".to_string();
        let args = HashMap::from_iter([
            (
                "pattern".to_string(),
                Value::String("only_worktree".to_string()),
            ),
            ("scope".to_string(), Value::String("workspace".to_string())),
        ]);

        let (_corrections, results) = tool.tool_execute(ccx, &tool_call_id, &args).await.unwrap();
        let names = context_file_names(&results);
        let text = tool_text(&results);

        let expected = norm(fixture.root.join("src/worktree_only.rs"));
        assert!(
            names.iter().any(|path| path == &expected),
            "expected {expected} in {names:?}"
        );
        assert!(text.contains("only_worktree"), "{text}");
        assert!(!text.contains("only_source"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_search_pattern_drops_source_only_and_deleted_stale() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let mut tool = crate::tools::tool_regex_search::ToolRegexSearch {
            config_path: String::new(),
        };
        let tool_call_id = "search-call".to_string();

        for pattern in ["only_source", "deleted_marker"] {
            let ccx = make_ccx(gcx.clone(), fixture.worktree.clone()).await;
            let args = HashMap::from_iter([
                ("pattern".to_string(), Value::String(pattern.to_string())),
                ("scope".to_string(), Value::String("workspace".to_string())),
            ]);

            let (corrections, results) =
                tool.tool_execute(ccx, &tool_call_id, &args).await.unwrap();
            let text = tool_text(&results);

            assert!(!corrections);
            assert!(text.contains("No matches found"), "{text}");
            assert!(
                !norm_str(&text).contains(&norm(fixture.source.clone())),
                "{text}"
            );
        }
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_search_pattern_renamed_file_new_name_only() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = crate::tools::tool_regex_search::ToolRegexSearch {
            config_path: String::new(),
        };
        let tool_call_id = "search-call".to_string();
        let args = HashMap::from_iter([
            (
                "pattern".to_string(),
                Value::String("renamed_marker".to_string()),
            ),
            ("scope".to_string(), Value::String("workspace".to_string())),
        ]);

        let (_corrections, results) = tool.tool_execute(ccx, &tool_call_id, &args).await.unwrap();
        let names = context_file_names(&results);
        let text = tool_text(&results);

        let expected = norm(fixture.root.join("src/new_name.rs"));
        assert!(
            names.iter().any(|path| path == &expected),
            "expected {expected} in {names:?}"
        );
        assert!(!names.iter().any(|path| path.contains("old_name.rs")));
        assert!(text.contains("new_name.rs"), "{text}");
        assert!(!text.contains("old_name.rs"), "{text}");
        assert!(!text.contains(&norm(fixture.source.clone())), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_semantic_search_drops_source_only_stale_results() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        install_mock_vecdb(
            gcx.clone(),
            vec![
                (&fixture.source.join("src/lib.rs"), "indexed"),
                (&fixture.source.join("src/source_only.rs"), "source-only"),
                (&fixture.source.join("src/deleted_stale.rs"), "deleted"),
                (&fixture.source.join("src/old_name.rs"), "renamed-old"),
            ],
        )
        .await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = crate::tools::tool_search::ToolSearch {
            config_path: String::new(),
        };
        let tool_call_id = "semantic-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &semantic_args("version"))
            .await
            .unwrap();
        let names = context_file_names(&results);
        let text = tool_text(&results);

        let expected = norm(fixture.root.join("src/lib.rs"));
        assert!(
            names.iter().any(|path| path == &expected),
            "expected {expected} in {names:?}"
        );
        assert!(!names.iter().any(|path| path.contains("source_only.rs")));
        assert!(!names.iter().any(|path| path.contains("deleted_stale.rs")));
        assert!(!names.iter().any(|path| path.contains("old_name.rs")));
        assert!(!text.contains("source_only.rs"), "{text}");
        assert!(!text.contains("deleted_stale.rs"), "{text}");
        assert!(!text.contains("old_name.rs"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_semantic_search_adds_worktree_only_fallback() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        install_mock_vecdb(gcx.clone(), vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = crate::tools::tool_search::ToolSearch {
            config_path: String::new(),
        };
        let tool_call_id = "semantic-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &semantic_args("only_worktree"))
            .await
            .unwrap();
        let names = context_file_names(&results);
        let text = tool_text(&results);

        let expected = norm(fixture.root.join("src/worktree_only.rs"));
        assert!(
            names.iter().any(|path| path == &expected),
            "expected {expected} in {names:?}"
        );
        assert!(
            text.contains("Direct worktree filesystem fallback"),
            "{text}"
        );
        assert!(!text.contains("source_only.rs"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_semantic_search_renamed_file_new_name_found() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        install_mock_vecdb(
            gcx.clone(),
            vec![(&fixture.source.join("src/old_name.rs"), "old")],
        )
        .await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = crate::tools::tool_search::ToolSearch {
            config_path: String::new(),
        };
        let tool_call_id = "semantic-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &semantic_args("renamed_marker"))
            .await
            .unwrap();
        let names = context_file_names(&results);
        let text = tool_text(&results);

        let expected = norm(fixture.root.join("src/new_name.rs"));
        assert!(
            names.iter().any(|path| path == &expected),
            "expected {expected} in {names:?}"
        );
        assert!(!names.iter().any(|path| path.contains("old_name.rs")));
        assert!(text.contains("new_name.rs"), "{text}");
        assert!(!text.contains("old_name.rs"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_missing_worktree_fails_closed() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        fs::remove_dir_all(&fixture.root).unwrap();
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let tool_call_id = "cat-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args("src/lib.rs".to_string()))
            .await
            .unwrap();
        let text = tool_text(&results);

        assert!(
            text.contains("Active worktree is missing or stale"),
            "{text}"
        );
        assert!(
            text.contains("will not fallback to source workspace"),
            "{text}"
        );
        assert!(!text.contains("source_version"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_at_file_source_path_remaps() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let at_file = AtFile::new();
        let mut cmd = AtCommandMember::new("cmd".to_string(), "@file".to_string(), 1, 6);
        let source_path = fixture
            .source
            .join("src")
            .join("lib.rs")
            .to_string_lossy()
            .to_string();
        let worktree_path = norm(fixture.root.join("src").join("lib.rs"));
        let mut args = vec![AtCommandMember::new("arg".to_string(), source_path, 7, 10)];

        let (results, replacement) = at_file.at_execute(ccx, &mut cmd, &mut args).await.unwrap();
        let text = tool_text(&results);
        let names = context_file_names(&results);

        assert!(text.contains("mapped to active worktree"), "{text}");
        assert_eq!(names, vec![worktree_path.clone()]);
        assert_eq!(norm_str(&replacement), worktree_path);
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_ast_source_context_file_remaps_to_worktree() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let source_path = fixture
            .source
            .join("src")
            .join("lib.rs")
            .to_string_lossy()
            .to_string();
        let worktree_path = norm(fixture.root.join("src").join("lib.rs"));
        let context_file = ContextFile {
            file_name: source_path,
            file_content: String::new(),
            line1: 1,
            line2: 1,
            file_rev: None,
            symbols: vec!["worktree_version".to_string()],
            gradient_type: 5,
            usefulness: 100.0,
            skip_pp: false,
        };

        let remapped =
            remap_context_file_for_execution_scope(&tool_ctx(&gcx), Some(&scope), context_file)
                .await
                .unwrap()
                .unwrap();

        assert_eq!(remapped.0.file_name, worktree_path);
        assert!(remapped
            .1
            .join("\n")
            .contains("mapped from source checkout"));
    }

    #[tokio::test]
    async fn model_context_remap_allows_controlled_but_rejects_blocked() {
        let fixture = make_fixture();
        let controlled_source = fixture.source.join("src/context.controlled");
        let controlled_worktree = fixture.root.join("src/context.controlled");
        let blocked_source = fixture.source.join("src/context.blocked");
        let blocked_worktree = fixture.root.join("src/context.blocked");
        fs::write(&controlled_source, "controlled\n").unwrap();
        fs::write(&controlled_worktree, "controlled worktree\n").unwrap();
        fs::write(&blocked_source, "blocked\n").unwrap();
        fs::write(&blocked_worktree, "blocked worktree\n").unwrap();
        let gcx = make_gcx_with_privacy(
            &fixture,
            vec!["*.controlled".to_string()],
            vec!["*.blocked".to_string()],
        )
        .await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let context_file = |path: &Path| ContextFile {
            file_name: path.to_string_lossy().to_string(),
            file_content: String::new(),
            line1: 1,
            line2: 1,
            file_rev: None,
            symbols: vec![],
            gradient_type: 5,
            usefulness: 100.0,
            skip_pp: false,
        };

        assert!(remap_context_file_for_execution_scope(
            &tool_ctx(&gcx),
            Some(&scope),
            context_file(&controlled_source),
        )
        .await
        .is_err());
        let (remapped, notices) = remap_context_files_for_execution_scope_for_model_context(
            &tool_ctx(&gcx),
            Some(&scope),
            vec![context_file(&controlled_source)],
        )
        .await
        .unwrap();
        assert_eq!(remapped[0].file_name, norm(controlled_worktree));
        assert!(notices.join("\n").contains("mapped from source checkout"));
        assert!(remap_context_files_for_execution_scope_for_model_context(
            &tool_ctx(&gcx),
            Some(&scope),
            vec![context_file(&blocked_source)],
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_ast_source_only_context_file_is_dropped() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let source_path = fixture
            .source
            .join("src")
            .join("source_only.rs")
            .to_string_lossy()
            .to_string();
        let context_file = ContextFile {
            file_name: source_path,
            file_content: String::new(),
            line1: 1,
            line2: 1,
            file_rev: None,
            symbols: vec!["only_source".to_string()],
            gradient_type: 5,
            usefulness: 100.0,
            skip_pp: false,
        };

        let remapped =
            remap_context_file_for_execution_scope(&tool_ctx(&gcx), Some(&scope), context_file)
                .await
                .unwrap();

        assert!(remapped.is_none());
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_ast_deleted_source_context_file_is_dropped() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let source_path = fixture
            .source
            .join("src")
            .join("deleted_stale.rs")
            .to_string_lossy()
            .to_string();
        let context_file = ContextFile {
            file_name: source_path,
            file_content: String::new(),
            line1: 1,
            line2: 1,
            file_rev: None,
            symbols: vec!["deleted_marker".to_string()],
            gradient_type: 5,
            usefulness: 100.0,
            skip_pp: false,
        };

        let remapped =
            remap_context_file_for_execution_scope(&tool_ctx(&gcx), Some(&scope), context_file)
                .await
                .unwrap();

        assert!(remapped.is_none());
    }

    #[tokio::test]
    async fn worktree_scope_read_tools_ast_renamed_old_source_context_file_is_dropped() {
        let fixture = make_fixture();
        let gcx = make_gcx(&fixture, vec![]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let source_path = fixture
            .source
            .join("src")
            .join("old_name.rs")
            .to_string_lossy()
            .to_string();
        let context_file = ContextFile {
            file_name: source_path,
            file_content: String::new(),
            line1: 1,
            line2: 1,
            file_rev: None,
            symbols: vec!["renamed_marker".to_string()],
            gradient_type: 5,
            usefulness: 100.0,
            skip_pp: false,
        };

        let remapped =
            remap_context_file_for_execution_scope(&tool_ctx(&gcx), Some(&scope), context_file)
                .await
                .unwrap();

        assert!(remapped.is_none());
    }

    #[tokio::test]
    async fn worktree_scope_directory_listing_filters_env_file() {
        let fixture = make_fixture();
        fs::write(fixture.root.join(".env"), "SECRET=value\n").unwrap();
        let gcx = make_gcx(&fixture, vec!["*.env".to_string()]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let tool_call_id = "cat-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args(".".to_string()))
            .await
            .unwrap();
        let names = context_file_names(&results);
        let text = tool_text(&results);

        assert!(
            !names.iter().any(|n| n.ends_with(".env")),
            "cat('.') should not expose .env: {:?}",
            names
        );
        assert!(!text.contains("SECRET=value"), "{text}");
    }

    #[tokio::test]
    async fn worktree_scope_remap_blocks_env_inside_worktree() {
        let fixture = make_fixture();
        fs::write(fixture.root.join(".env"), "SECRET=value\n").unwrap();
        let gcx = make_gcx(&fixture, vec!["*.env".to_string()]).await;
        let scope = ExecutionScope::from_worktree(&fixture.worktree);
        let env_path = fixture.root.join(".env").to_string_lossy().to_string();
        let context_file = ContextFile {
            file_name: env_path,
            file_content: String::new(),
            line1: 1,
            line2: 1,
            file_rev: None,
            symbols: vec![],
            gradient_type: 5,
            usefulness: 100.0,
            skip_pp: false,
        };

        let result =
            remap_context_file_for_execution_scope(&tool_ctx(&gcx), Some(&scope), context_file)
                .await;
        assert!(
            result.is_err(),
            "privacy-blocked .env inside worktree should return error"
        );
        assert!(
            result.unwrap_err().contains("Blocked"),
            "error should mention privacy level Blocked"
        );
    }

    #[tokio::test]
    async fn worktree_scope_env_file_inside_worktree_is_privacy_checked() {
        let fixture = make_fixture();
        fs::write(fixture.root.join(".env"), "SECRET=value\n").unwrap();
        let gcx = make_gcx(&fixture, vec!["*.env".to_string()]).await;
        let ccx = make_ccx(gcx, fixture.worktree.clone()).await;
        let mut tool = ToolCat {
            config_path: String::new(),
        };
        let tool_call_id = "cat-call".to_string();

        let (_corrections, results) = tool
            .tool_execute(ccx, &tool_call_id, &cat_args(".env".to_string()))
            .await
            .unwrap();
        let text = tool_text(&results);

        assert!(context_file_names(&results).is_empty());
        assert!(text.contains("privacy level Blocked"), "{text}");
    }

    #[test]
    fn scope_filter_construction_handles_special_chars() {
        let apostrophe = "/home/user's project/src/";
        let filter = format!(
            r#"(scope LIKE "{}%" ESCAPE '\')"#,
            escape_path_for_scope_filter(apostrophe)
        );
        assert!(filter.starts_with(r#"(scope LIKE ""#), "{filter}");
        assert!(filter.contains("user's project"), "{filter}");

        let percent = "/home/user/50%/src/";
        let filter = format!(
            r#"(scope LIKE "{}%" ESCAPE '\')"#,
            escape_path_for_scope_filter(percent)
        );
        assert!(filter.contains(r"50\%/"), "{filter}");

        let underscore = "/home/user/my_project/src/";
        let filter = format!(
            r#"(scope LIKE "{}%" ESCAPE '\')"#,
            escape_path_for_scope_filter(underscore)
        );
        assert!(filter.contains(r"my\_project"), "{filter}");
    }

    #[test]
    fn scope_filter_percent_and_underscore_match_literally() {
        let escaped = escape_path_for_scope_filter("/path/50%/a_b/");
        assert!(
            escaped.contains(r"\%"),
            "percent should be escaped: {escaped}"
        );
        assert!(
            escaped.contains(r"\_"),
            "underscore should be escaped: {escaped}"
        );

        let filter = format!(r#"(scope LIKE "{}%" ESCAPE '\')"#, escaped);
        assert!(
            filter.contains("ESCAPE"),
            "filter should include ESCAPE clause: {filter}"
        );
        assert!(
            filter.contains(r"\%/"),
            "escaped percent should appear in filter: {filter}"
        );
        assert!(
            filter.contains(r"\_"),
            "escaped underscore should appear in filter: {filter}"
        );
    }

    #[test]
    fn worktree_scope_gathered_file_validation_uses_worktree_paths() {
        let fixture = make_fixture();
        let scope = ExecutionScope::from_worktree(&fixture.worktree);

        let relative = crate::tools::subagent_phases::resolve_gathered_file_path_with_scope(
            Some(&scope),
            "src/lib.rs",
        )
        .unwrap();
        let source_absolute = crate::tools::subagent_phases::resolve_gathered_file_path_with_scope(
            Some(&scope),
            &fixture.source.join("src").join("lib.rs").to_string_lossy(),
        )
        .unwrap();

        assert_eq!(relative, fixture.root.join("src").join("lib.rs"));
        assert_eq!(source_absolute, fixture.root.join("src").join("lib.rs"));
        assert!(
            crate::tools::subagent_phases::resolve_gathered_file_path_with_scope(
                Some(&scope),
                "src/source_only.rs",
            )
            .is_none()
        );
    }
}
