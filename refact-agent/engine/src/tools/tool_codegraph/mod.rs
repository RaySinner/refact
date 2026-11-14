pub use refact_tools::tools::tool_codegraph::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
#[cfg(test)]
#[allow(unused_imports)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use git2::{Oid, Repository};
#[cfg(test)]
#[allow(unused_imports)]
use petgraph::algo::tarjan_scc;
#[cfg(test)]
#[allow(unused_imports)]
use petgraph::graph::{DiGraph, NodeIndex};
#[cfg(test)]
#[allow(unused_imports)]
use serde::Serialize;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use sha2::{Digest, Sha256};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::at_command_executor::{
    file_repair_candidates, return_one_candidate_or_a_good_error,
};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use refact_code_intel_api::*;
#[cfg(test)]
#[allow(unused_imports)]
use refact_files::path_utils::{canonicalize_normalized_path, preprocess_path_for_normalization};
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::files_correction::{get_project_dirs, resolve_codegraph_queue_path};
#[cfg(test)]
#[allow(unused_imports)]
use refact_worktrees::path_mappings::registered_worktree_path_mappings;
#[cfg(test)]
#[allow(unused_imports)]
use refact_postprocessing::pp_command_output::OutputFilter;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::scope_utils::resolve_existing_path_with_execution_scope_for_model_context;
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{
    json_schema_from_params, Tool, ToolDesc, ToolSource, ToolSourceType,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_paths::{abs, abs_str};
    use git2::{Repository, Signature, Time};

    #[test]
    fn scoped_project_roots_use_worktree_for_git_and_source_for_graph_keys() {
        let temp = tempfile::tempdir().unwrap();
        let worktree_root = temp.path().join("worktree");
        let source_root = temp.path().join("source");
        std::fs::create_dir_all(worktree_root.join("src")).unwrap();
        std::fs::create_dir_all(source_root.join("src")).unwrap();
        let worktree = refact_worktrees::types::WorktreeMeta {
            id: "codegraph-scope".to_string(),
            kind: "chat".to_string(),
            root: worktree_root.clone(),
            source_workspace_root: source_root.clone(),
            repo_root: source_root.clone(),
            branch: Some("feature/codegraph".to_string()),
            base_branch: Some("main".to_string()),
            base_commit: None,
            task_id: None,
            card_id: None,
            agent_id: None,
            enforce: true,
        };
        let scope = refact_worktrees::scope::ExecutionScope::from_worktree(&worktree);

        let roots = scoped_codegraph_project_roots(Some(&scope))
            .unwrap()
            .unwrap();

        assert_eq!(
            roots.git_root,
            dunce::simplified(&std::fs::canonicalize(worktree_root).unwrap()).to_path_buf()
        );
        assert_eq!(
            roots.indexed_root,
            dunce::simplified(&std::fs::canonicalize(source_root).unwrap()).to_path_buf()
        );

        let missing_source_path = scope
            .resolve_creatable_path(Path::new("src/deleted.rs"))
            .unwrap();
        assert_eq!(
            scoped_indexed_path(&scope, &missing_source_path.path).unwrap(),
            roots.indexed_root.join("src/deleted.rs")
        );
        let source_absolute = roots.indexed_root.join("src/deleted.rs");
        let remapped_source_path = scope.resolve_creatable_path(&source_absolute).unwrap();
        assert_eq!(remapped_source_path.path, missing_source_path.path);
        assert_eq!(
            scoped_indexed_path(&scope, &remapped_source_path.path).unwrap(),
            source_absolute
        );
        assert!(scoped_indexed_path(&scope, temp.path().join("outside.rs").as_path()).is_err());
    }

    #[test]
    fn code_intel_scope_accepts_root_paths_and_rejects_noise_and_outside_paths() {
        let root = abs("workspace/refact");
        assert!(code_intel_path_allowed(&root, "src/main.rs"));
        assert!(code_intel_path_allowed(
            &root,
            &abs_str("workspace/refact/src/main.rs")
        ));
        assert!(!code_intel_path_allowed(&root, "target/debug/generated.rs"));
        assert!(!code_intel_path_allowed(
            &root,
            "web/node_modules/pkg/index.js"
        ));
        assert!(code_intel_path_allowed(
            &root,
            "competitors/rival/src/lib.rs"
        ));
        assert!(!code_intel_path_allowed(
            &root,
            &abs_str("workspace/other/src/main.rs")
        ));

        let competitor_root = abs("workspace/refact/competitors/rival");
        assert!(code_intel_path_allowed(&competitor_root, "src/lib.rs"));
        assert!(code_intel_path_allowed(
            &competitor_root,
            &abs_str("workspace/refact/competitors/rival/src/lib.rs")
        ));
    }

    #[test]
    fn scoped_graph_removes_noise_outside_nodes_and_incident_edges() {
        let graph = refact_codegraph::analytics::GraphData {
            nodes: vec![
                (1, "main".into(), abs_str("workspace/refact/src/main.rs")),
                (2, "generated".into(), "dist/bundle.js".into()),
                (3, "rival".into(), "competitors/rival/src/lib.rs".into()),
                (4, "outside".into(), abs_str("workspace/other/src/lib.rs")),
            ],
            edges: vec![
                (1, 2, "calls".into()),
                (1, 3, "calls".into()),
                (1, 4, "calls".into()),
            ],
        };
        let scoped = scoped_graph_data(&graph, &abs("workspace/refact"));
        let analytics = refact_codegraph::analytics::compute_graph_analytics_from_data(&scoped);

        assert_eq!(
            scoped.nodes,
            vec![
                (1, "main".into(), abs_str("workspace/refact/src/main.rs")),
                (3, "rival".into(), "competitors/rival/src/lib.rs".into()),
            ]
        );
        assert_eq!(scoped.edges, vec![(1, 3, "calls".into())]);
        assert_eq!(analytics.overview.node_count, 2);
        assert_eq!(analytics.overview.edge_count, 1);
        assert!(analytics.overview.top_pagerank.iter().all(|entry| {
            !entry.path.contains("dist/") && !entry.path.starts_with(&abs_str("workspace/other"))
        }));
    }

    #[test]
    fn scoped_dead_symbols_remove_noise_before_enrichment() {
        let symbol = |node_id, path: &str| refact_codegraph::dead_code::DeadSymbol {
            node_id,
            name: format!("dead_{node_id}"),
            path: path.into(),
            line: 1,
            reason: "unreachable".into(),
            confidence: 0.8,
            incoming_edges: 0,
        };
        let scoped = scoped_dead_symbols(
            vec![
                symbol(1, "src/dead.rs"),
                symbol(2, "dist/generated.rs"),
                symbol(3, &abs_str("workspace/other/dead.rs")),
            ],
            Some(&abs("workspace/refact")),
        );

        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].path, "src/dead.rs");
    }

    #[tokio::test]
    async fn scoped_clone_view_recomputes_counts_and_percentage() {
        let duplicate = branchy_hotspot_source(1, 2, 3).repeat(8);
        let analysis = CloneAnalysis {
            generation: 1,
            clones: Vec::new(),
            duplication_pct: 0.9,
            files: 3,
            text_by_path: HashMap::from([
                ("src/a.rs".into(), ("rust".into(), duplicate.clone())),
                ("src/b.rs".into(), ("rust".into(), duplicate.clone())),
                ("dist/generated.rs".into(), ("rust".into(), duplicate)),
            ]),
            tokens_by_path: HashMap::new(),
            duplicated_tokens_by_path: HashMap::new(),
        };

        let scoped = scoped_clone_analysis(&analysis, &abs("workspace/refact"))
            .await
            .unwrap();
        assert_eq!(scoped.files, 2);
        assert!(!scoped.clones.is_empty());
        assert!(scoped.duplication_pct > 0.0);
        assert!(scoped
            .duplicated_tokens_by_path
            .values()
            .all(|tokens| *tokens > 0));
        assert!(!scoped.text_by_path.contains_key("dist/generated.rs"));
    }

    #[tokio::test]
    async fn scoped_clone_view_drops_duplication_with_only_excluded_counterpart() {
        let duplicate = branchy_hotspot_source(1, 2, 3).repeat(8);
        let analysis = CloneAnalysis {
            generation: 1,
            clones: Vec::new(),
            duplication_pct: 1.0,
            files: 2,
            text_by_path: HashMap::from([
                ("src/a.rs".into(), ("rust".into(), duplicate.clone())),
                ("dist/generated.rs".into(), ("rust".into(), duplicate)),
            ]),
            tokens_by_path: HashMap::new(),
            duplicated_tokens_by_path: HashMap::from([("src/a.rs".into(), 500)]),
        };

        let scoped = scoped_clone_analysis(&analysis, &abs("workspace/refact"))
            .await
            .unwrap();

        assert_eq!(scoped.files, 1);
        assert!(scoped.clones.is_empty());
        assert_eq!(scoped.duplication_pct, 0.0);
        assert_eq!(scoped.duplicated_tokens_by_path.get("src/a.rs"), Some(&0));
    }

    #[test]
    fn code_why_matching_is_token_aware_and_order_independent() {
        assert!(code_why_statement_matches(
            "Use the CAT service",
            &code_why_tokens("cat")
        ));
        assert!(code_why_statement_matches(
            "Cache invalidation: use event-driven updates.",
            &code_why_tokens("updates, cache")
        ));
        assert!(code_why_statement_matches(
            "Unicode café routing is enabled",
            &code_why_tokens("routing café")
        ));
        assert!(!code_why_statement_matches(
            "Concatenate the values",
            &code_why_tokens("cat")
        ));
        assert!(!code_why_statement_matches(
            "anything",
            &code_why_tokens("...")
        ));
    }

    fn commit_file(repo: &Repository, path: &str, contents: &str, msg: &str) -> git2::Oid {
        commit_file_at(
            repo,
            path,
            contents,
            msg,
            "Tester",
            "tester@example.com",
            1_700_000_000,
        )
    }

    fn commit_file_at(
        repo: &Repository,
        path: &str,
        contents: &str,
        msg: &str,
        name: &str,
        email: &str,
        ts: i64,
    ) -> git2::Oid {
        let workdir = repo.workdir().unwrap();
        let full_path = workdir.join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full_path, contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let time = Time::new(ts, 0);
        let sig = Signature::new(name, email, &time).unwrap();
        let parents: Vec<git2::Commit> = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .unwrap()
    }

    fn two_branch_repo() -> (tempfile::TempDir, git2::Oid) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let main_oid = commit_file(&repo, "file.txt", "main\n", "main branch history");
        let main_commit = repo.find_commit(main_oid).unwrap();
        repo.branch("side", &main_commit, false).unwrap();
        repo.set_head("refs/heads/side").unwrap();
        commit_file(&repo, "file.txt", "side\n", "side branch history");
        repo.set_head("refs/heads/main").unwrap();
        (dir, main_oid)
    }

    fn code_map_node_record(
        id: i64,
        kind: &str,
        name: &str,
        path: &str,
    ) -> (i64, String, String, String, Option<String>) {
        (
            id,
            kind.to_string(),
            name.to_string(),
            path.to_string(),
            None,
        )
    }

    fn code_map_centrality(
        pagerank: &[(&str, f64)],
        betweenness: &[(&str, f64)],
    ) -> refact_codegraph::analytics::FileCentrality {
        refact_codegraph::analytics::FileCentrality {
            top_pagerank: pagerank
                .iter()
                .map(|(path, score)| ((*path).to_string(), *score))
                .collect(),
            top_betweenness: betweenness
                .iter()
                .map(|(path, score)| ((*path).to_string(), *score))
                .collect(),
        }
    }

    fn code_map_markdown_args(budget_tokens: usize) -> CodeMapArgs {
        CodeMapArgs {
            query: None,
            format: CodeMapFormat::Markdown,
            budget_tokens,
        }
    }

    fn code_map_output_fixture(args: CodeMapArgs) -> CodeMapResponse {
        assemble_code_map_response(
            vec![
                (
                    "payments/api.rs".to_string(),
                    "pub fn charge() { validate(); }\n".to_string(),
                ),
                (
                    "auth/login.rs".to_string(),
                    "pub fn login() {}\n".to_string(),
                ),
            ],
            vec![
                code_map_node_record(1, "function", "charge", "payments/api.rs"),
                code_map_node_record(2, "function", "validate", "payments/api.rs"),
                code_map_node_record(3, "function", "login", "auth/login.rs"),
            ],
            vec![(1, 2, "calls".to_string())],
            code_map_centrality(&[("payments/api.rs", 3.0), ("auth/login.rs", 1.0)], &[]),
            Some(PrBlastIndexState {
                queued: 0,
                cross_file_edges: 1,
                cross_file_ready: true,
            }),
            None,
            None,
            &args,
        )
    }

    #[test]
    fn scoped_code_map_inputs_remove_noise_pages_and_links() {
        let root = abs("workspace/refact");
        let (files, nodes, edges, centrality) = scoped_code_map_inputs(
            &root,
            vec![
                ("src/main.rs".into(), "fn main() {}".into()),
                ("dist/generated.js".into(), "function generated() {}".into()),
                (
                    abs_str("workspace/other/outside.rs"),
                    "fn outside() {}".into(),
                ),
            ],
            vec![
                code_map_node_record(1, "function", "main", "src/main.rs"),
                code_map_node_record(2, "function", "generated", "dist/generated.js"),
                code_map_node_record(
                    3,
                    "function",
                    "outside",
                    &abs_str("workspace/other/outside.rs"),
                ),
            ],
            vec![(1, 2, "calls".into()), (2, 3, "calls".into())],
            code_map_centrality(&[("src/main.rs", 0.123)], &[]),
        );
        assert_eq!(centrality.top_pagerank, vec![("src/main.rs".into(), 0.123)]);
        let output = assemble_code_map_response(
            files,
            nodes,
            edges,
            centrality,
            None,
            None,
            Some(&root),
            &code_map_markdown_args(2_000),
        );

        assert_eq!(output.files_count, 1);
        assert!(output.pages.iter().all(|page| {
            page.paths.iter().all(|path| path == "src/main.rs")
                && page
                    .links
                    .iter()
                    .all(|link| link.target_path == "src/main.rs")
        }));
        assert!(output
            .pages
            .iter()
            .flat_map(|page| page.links.iter())
            .next()
            .is_none());
    }

    #[test]
    fn code_map_marks_real_hotspot() {
        let mut intel = refact_git_intel::GitIntel::default();
        intel.file_churn.insert("src/hot.rs".to_string(), 20);
        for index in 0..10 {
            intel.file_churn.insert(format!("src/cold{index}.rs"), 1);
        }
        let files_text = vec![
            ("src/hot.rs".to_string(), "fn hot() {}\n".to_string()),
            ("src/cold0.rs".to_string(), "fn cold() {}\n".to_string()),
        ];
        let nodes = code_map_nodes_from_records(vec![
            code_map_node_record(1, "function", "hot", "src/hot.rs"),
            code_map_node_record(2, "function", "cold", "src/cold0.rs"),
        ]);
        let centrality = code_map_centrality(&[], &[]);
        let files = build_code_map_files(&files_text, &nodes, &centrality, Some(&intel), None);

        let hot = files
            .iter()
            .find(|file| file.parsed.file_info.path == "src/hot.rs")
            .unwrap();
        let cold = files
            .iter()
            .find(|file| file.parsed.file_info.path == "src/cold0.rs")
            .unwrap();
        assert!(hot.is_hotspot);
        assert!(!cold.is_hotspot);

        let input = code_map_selection_input(files, &nodes, &[], &centrality);
        let pages = refact_codewiki::select_pages(&input);
        let hot_score = pages
            .iter()
            .find(|page| page.id == "file:src/hot.rs")
            .unwrap()
            .score;
        let cold_score = pages
            .iter()
            .find(|page| page.id == "file:src/cold0.rs")
            .unwrap()
            .score;
        assert!(hot_score > cold_score);
    }

    #[test]
    fn code_map_symbols_use_node_kinds() {
        let nodes = code_map_nodes_from_records(vec![
            code_map_node_record(1, "struct", "Thing", "src/model.rs"),
            code_map_node_record(2, "function", "build", "src/model.rs"),
            code_map_node_record(3, "typealias", "Id", "src/model.rs"),
        ]);
        let symbols = code_map_symbols_by_path(&nodes);
        let symbols = symbols.get("src/model.rs").unwrap();
        let kinds = symbols
            .iter()
            .map(|symbol| symbol.kind.as_str())
            .collect::<BTreeSet<_>>();

        assert!(kinds.contains("struct"));
        assert!(kinds.contains("function"));
        assert!(kinds.contains("type_alias"));
        assert!(symbols.iter().all(|symbol| symbol.visibility.is_empty()));

        let output = assemble_code_map_response(
            vec![(
                "src/model.rs".to_string(),
                "struct Thing; fn build() {}\n".to_string(),
            )],
            vec![
                code_map_node_record(1, "struct", "Thing", "src/model.rs"),
                code_map_node_record(2, "function", "build", "src/model.rs"),
                code_map_node_record(3, "typealias", "Id", "src/model.rs"),
            ],
            Vec::new(),
            code_map_centrality(&[("src/model.rs", 1.0)], &[]),
            None,
            None,
            None,
            &code_map_markdown_args(1_000),
        );
        let page = output.pages.first().unwrap();
        assert_eq!(page.symbols.get("function"), Some(&1));
        assert_eq!(page.symbols.get("struct"), Some(&1));
        assert_eq!(page.symbols.get("type_alias"), Some(&1));
        assert!(page.visibility.is_empty());
    }

    #[test]
    fn code_map_links_from_graph_edges() {
        let output = assemble_code_map_response(
            vec![
                ("src/a.rs".to_string(), "fn a() { b(); }\n".to_string()),
                ("src/b.rs".to_string(), "fn b() {}\n".to_string()),
            ],
            vec![
                code_map_node_record(1, "function", "a", "src/a.rs"),
                code_map_node_record(2, "function", "b", "src/b.rs"),
            ],
            vec![(1, 2, "calls".to_string())],
            code_map_centrality(&[("src/a.rs", 2.0), ("src/b.rs", 1.0)], &[]),
            None,
            None,
            None,
            &code_map_markdown_args(2_000),
        );

        assert!(output
            .pages
            .iter()
            .any(|page| page.links.iter().any(|link| {
                link.target_path == "src/b.rs" && link.labels.contains(&"calls".to_string())
            })));
        assert!(output
            .backlink_hubs
            .iter()
            .any(|hub| hub.path == "src/b.rs"));
    }

    #[test]
    fn code_map_query_returns_relevant_page() {
        let mut args = code_map_markdown_args(2_000);
        args.query = Some("payments".to_string());

        let output = code_map_output_fixture(args);

        assert_eq!(output.query.as_deref(), Some("payments"));
        assert!(output
            .pages
            .iter()
            .any(|page| page.paths.contains(&"payments/api.rs".into())));
        assert!(!output
            .pages
            .iter()
            .any(|page| page.paths.contains(&"auth/login.rs".into())));
    }

    #[test]
    fn claude_md_format_renders() {
        let output = code_map_output_fixture(CodeMapArgs {
            query: Some("payments".to_string()),
            format: CodeMapFormat::ClaudeMd,
            budget_tokens: 4_000,
        });

        let markdown = output.markdown.unwrap();
        assert!(markdown.contains(refact_codewiki::claude_md::BEGIN_MARKER));
        assert!(markdown.contains("### Selected Pages"));
        assert!(markdown.contains("payments/api.rs"));
    }

    #[test]
    fn budget_bounds_total_tokens() {
        let budget = 40;
        let output = code_map_output_fixture(code_map_markdown_args(budget));

        assert!(output.pages.iter().all(|page| {
            refact_codewiki::token_budget::estimate_tokens(&page.content) <= budget
        }));
    }

    #[test]
    fn git_mining_cache_does_not_store_new_head_results_under_old_head_key() {
        let (dir, main_oid) = two_branch_repo();
        let repo_path = dir.path();
        let max = 10;
        let key = (repo_path.to_path_buf(), Some(main_oid), max);
        assert_eq!(git_head_oid(repo_path).unwrap(), Some(main_oid));

        let history = cached_mine_history_with(repo_path, max, |path, _head, _base, max| {
            let repo = Repository::open(path).unwrap();
            repo.set_head("refs/heads/side").unwrap();
            refact_git_intel::mine_history(path, max)
        })
        .unwrap();

        assert_eq!(history.commit_records[0].message, "side branch history");
        assert!(git_cache().lock().unwrap().history.get(&key).is_none());
    }

    #[test]
    fn code_why_assembles_multi_kind_sources() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(
            &repo,
            "src/a.rs",
            "a\n",
            "introduce codegraph index because symbol search needs persisted edges",
        );
        commit_file(
            &repo,
            "src/b.rs",
            "b\n",
            "switch to codegraph instead of ast maps because cross-file search needs edges",
        );
        commit_file(
            &repo,
            "src/c.rs",
            "c\n",
            "replace codegraph queue polling because incremental indexing needs lower latency",
        );
        commit_file(
            &repo,
            "src/d.rs",
            "d\n",
            "Merge branch feature/codegraph\n\n## Why\nSwitch codegraph traversal to sqlite because graph queries need persistence.",
        );
        let adr_path = dir.path().join("docs/adr/001-codegraph.md");
        std::fs::create_dir_all(adr_path.parent().unwrap()).unwrap();
        std::fs::write(
            &adr_path,
            "# Use codegraph for symbol search\n\n## Decision\nUse codegraph because symbol search needs relationships.\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("CHANGELOG.md"),
            "## Unreleased\n\n### Changed\n- switch codegraph results to evidence-ranked output\n",
        )
        .unwrap();

        let intel = refact_git_intel::mine_history(dir.path(), 20).unwrap();
        let mut sources = git_code_why_sources(&intel);
        for (order, (relative, kind)) in code_why_file_kinds(dir.path()).into_iter().enumerate() {
            sources.push(CodeWhySource {
                kind: kind.to_string(),
                text: std::fs::read_to_string(dir.path().join(&relative)).unwrap(),
                reference: path_ref(&relative),
                priority: code_why_priority(kind),
                order: usize::MAX / 2 + order,
            });
        }
        let sources = cap_code_why_sources(sources);
        let kinds: BTreeSet<String> = sources.iter().map(|source| source.kind.clone()).collect();

        assert!(kinds.contains("commit"));
        assert!(kinds.contains("pr"));
        assert!(kinds.contains("adr"));
        assert!(kinds.contains("changelog"));
        assert_eq!(
            sources.first().map(|source| source.kind.as_str()),
            Some("adr")
        );
    }

    #[test]
    fn code_why_finds_prose_decision_end_to_end() {
        let statement =
            "switch codegraph storage to sqlite instead of memory because graph queries need persistence";
        let sources = vec![
            CodeWhySource {
                kind: "adr".to_string(),
                text: format!("# {statement}\n\n## Decision\n{statement}\n"),
                reference: "docs/adr/001-codegraph.md".to_string(),
                priority: code_why_priority("adr"),
                order: 0,
            },
            CodeWhySource {
                kind: "commit".to_string(),
                text: statement.to_string(),
                reference: "abcdef123456".to_string(),
                priority: code_why_priority("commit"),
                order: 1,
            },
        ];

        let output = code_why_response("codegraph", &sources, 2).unwrap();

        let decision = output.decisions.first().unwrap();
        assert_eq!(decision.summary, statement);
        assert!(decision
            .provenance_tags
            .iter()
            .any(|tag| tag.contains("verbatim")));
        assert_eq!(decision.corroboration, 2);
        assert_eq!(decision.source_ref, "docs/adr/001-codegraph.md");
    }

    #[test]
    fn code_why_file_sources_bounded_and_prioritized() {
        let mut git_sources = Vec::new();
        for index in 0..CODE_WHY_SOURCE_LIMIT {
            git_sources.push(CodeWhySource {
                kind: "commit".to_string(),
                text: format!("commit decision {index}"),
                reference: format!("commit:{index:03}"),
                priority: code_why_priority("commit"),
                order: index,
            });
        }
        git_sources.push(CodeWhySource {
            kind: "pr".to_string(),
            text: "merge decision".to_string(),
            reference: "pr:001".to_string(),
            priority: code_why_priority("pr"),
            order: CODE_WHY_SOURCE_LIMIT,
        });
        let candidates = vec![
            CodeWhyFileCandidate {
                relative: PathBuf::from("docs/adr/002.md"),
                kind: "adr",
                reference: "docs/adr/002.md".to_string(),
                priority: code_why_priority("adr"),
                order: usize::MAX / 2 + 1,
            },
            CodeWhyFileCandidate {
                relative: PathBuf::from("CHANGELOG.md"),
                kind: "changelog",
                reference: "CHANGELOG.md".to_string(),
                priority: code_why_priority("changelog"),
                order: usize::MAX / 2 + 2,
            },
            CodeWhyFileCandidate {
                relative: PathBuf::from("docs/adr/001.md"),
                kind: "adr",
                reference: "docs/adr/001.md".to_string(),
                priority: code_why_priority("adr"),
                order: usize::MAX / 2,
            },
        ];

        let selected =
            select_code_why_file_candidates(candidates, &git_sources, CODE_WHY_SOURCE_LIMIT);
        let selected_refs = selected
            .iter()
            .map(|candidate| candidate.reference.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            selected_refs,
            vec!["docs/adr/001.md", "docs/adr/002.md", "CHANGELOG.md"]
        );

        let mut sources = git_sources;
        sources.extend(selected.into_iter().map(|candidate| CodeWhySource {
            kind: candidate.kind.to_string(),
            text: candidate.reference.clone(),
            reference: candidate.reference,
            priority: candidate.priority,
            order: candidate.order,
        }));
        let sources = cap_code_why_sources(sources);
        let first_refs = sources
            .iter()
            .take(5)
            .map(|source| source.reference.as_str())
            .collect::<Vec<_>>();

        assert_eq!(sources.len(), CODE_WHY_SOURCE_LIMIT);
        assert_eq!(
            first_refs,
            vec![
                "docs/adr/001.md",
                "docs/adr/002.md",
                "CHANGELOG.md",
                "pr:001",
                "commit:000"
            ]
        );
        assert!(!sources
            .iter()
            .any(|source| source.reference == "commit:796"));
        assert!(!sources
            .iter()
            .any(|source| source.reference == "commit:797"));
        assert!(!sources
            .iter()
            .any(|source| source.reference == "commit:798"));
        assert!(!sources
            .iter()
            .any(|source| source.reference == "commit:799"));
    }

    fn allow_all_privacy(gcx: &Arc<crate::global_context::GlobalContext>) {
        let loaded_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        *gcx.privacy_settings.write().unwrap() = Arc::new(refact_core::privacy_types::PrivacySettings {
            privacy_rules: refact_core::privacy_types::FilePrivacySettings {
                only_send_to_servers_I_control: Vec::new(),
                blocked: Vec::new(),
            },
            loaded_ts,
        });
    }

    #[tokio::test]
    async fn assemble_code_why_sources_bounds_file_reads_before_reading() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        allow_all_privacy(&gcx);
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(
            &repo,
            "src/a.rs",
            "a\n",
            "switch code why source selection because file reads need bounds",
        );
        std::fs::create_dir_all(dir.path().join("docs/adr")).unwrap();
        for index in 0..(CODE_WHY_SOURCE_LIMIT + 20) {
            std::fs::write(
                dir.path().join(format!("docs/adr/{index:04}.md")),
                format!("# ADR {index}\n\nDecision {index}\n"),
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("CHANGELOG.md"), "# Changelog\n").unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        let intel = refact_git_intel::mine_history(dir.path(), 20).unwrap();
        let git_sources = git_code_why_sources(&intel);
        let candidate_count = code_why_file_candidates(dir.path()).len();
        let selected_before_reads = code_why_selected_candidate_count_before_reads(
            dir.path(),
            &git_sources,
            CODE_WHY_SOURCE_LIMIT,
        );
        CODE_WHY_FILE_READ_ATTEMPTS.store(0, std::sync::atomic::Ordering::Relaxed);

        let sources = assemble_code_why_sources(gcx, dir.path(), &intel).await;
        let read_attempts = CODE_WHY_FILE_READ_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed);

        assert!(candidate_count > CODE_WHY_SOURCE_LIMIT);
        assert!(selected_before_reads <= CODE_WHY_SOURCE_LIMIT);
        assert_eq!(read_attempts, selected_before_reads);
        assert!(sources.len() <= CODE_WHY_SOURCE_LIMIT);
        let first_kinds = sources
            .iter()
            .take(4)
            .map(|source| source.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(first_kinds, vec!["adr", "adr", "adr", "adr"]);
        let first_non_adr = sources
            .iter()
            .position(|source| source.kind != "adr")
            .unwrap_or(sources.len());
        let changelog_pos = sources.iter().position(|source| source.kind == "changelog");
        let pr_pos = sources.iter().position(|source| source.kind == "pr");
        let commit_pos = sources.iter().position(|source| source.kind == "commit");
        if let Some(changelog_pos) = changelog_pos {
            assert!(sources[..changelog_pos]
                .iter()
                .all(|source| source.kind == "adr"));
            if let Some(pr_pos) = pr_pos {
                assert!(changelog_pos < pr_pos);
            }
            if let Some(commit_pos) = commit_pos {
                assert!(changelog_pos < commit_pos);
            }
        } else {
            assert_eq!(first_non_adr, sources.len());
        }
        if let (Some(pr_pos), Some(commit_pos)) = (pr_pos, commit_pos) {
            assert!(pr_pos < commit_pos);
        }
    }

    #[test]
    fn cached_mine_history_incremental_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file(
            &repo,
            "src/a.rs",
            "a\n",
            "introduce codegraph history because decisions need provenance",
        );

        let first = cached_mine_history_with(dir.path(), 20, |path, _head, base, max| {
            assert!(base.is_none());
            refact_git_intel::mine_history_incremental(path, base, max)
        })
        .unwrap();
        let first_head = first.last_commit_id.clone();
        commit_file(
            &repo,
            "src/b.rs",
            "b\n",
            "switch codegraph history to incremental because repeated mining is expensive",
        );

        let second = cached_mine_history_with(dir.path(), 20, |path, _head, base, max| {
            let base = base.expect("previous cached history is reused as incremental base");
            assert_eq!(base.last_commit_id, first_head);
            refact_git_intel::mine_history_incremental(path, Some(base), max)
        })
        .unwrap();

        assert_eq!(second.commits_analyzed, first.commits_analyzed + 1);
        assert_ne!(second.last_commit_id, first.last_commit_id);
    }

    #[test]
    #[ignore]
    fn code_why_real_repo_smoke() {
        let repo_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let intel = refact_git_intel::mine_history(repo_path, CODE_WHY_HISTORY_COMMITS).unwrap();
        let sources = git_code_why_sources(&intel);
        let output = code_why_response("codegraph", &sources, intel.commits_analyzed).unwrap();

        assert_eq!(output.query, "codegraph");
    }

    #[test]
    fn pr_blast_reviewers_are_ownership_based_stable_and_bot_filtered() {
        let report = refact_codegraph::pr_blast::BlastReport {
            changed_files: vec![abs_str("repo/src/core.rs")],
            directly_impacted: vec![refact_codegraph::pr_blast::BlastImpact {
                path: "src/caller.rs".to_string(),
                symbol: "caller".to_string(),
                distance: 1,
                via: "calls".to_string(),
                kind: refact_codegraph::pr_blast::ImpactKind::Behavioral,
            }],
            transitively_impacted: Vec::new(),
            impacted_file_count: 1,
            risk_score: 0.1,
        };
        let mut intel = refact_git_intel::GitIntel::default();
        intel.file_authors.insert(
            "src/core.rs".to_string(),
            HashMap::from([
                ("b@example.com".to_string(), 1),
                ("a@example.com".to_string(), 1),
                ("builder[bot]@users.noreply.github.com".to_string(), 10),
            ]),
        );
        intel.file_authors.insert(
            "src/caller.rs".to_string(),
            HashMap::from([
                ("b@example.com".to_string(), 1),
                ("a@example.com".to_string(), 1),
                ("review-agent@example.com".to_string(), 10),
            ]),
        );

        let reviewers = pr_blast_suggested_reviewers(&report, Some(&abs("repo")), Some(&intel));

        assert_eq!(reviewers.len(), 2);
        assert_eq!(reviewers[0].author, "a@example.com");
        assert_eq!(reviewers[1].author, "b@example.com");
        assert!((reviewers[0].score - reviewers[1].score).abs() < 1e-9);
    }

    #[test]
    fn pr_blast_reviewers_are_empty_without_repo_or_intel() {
        let report = refact_codegraph::pr_blast::BlastReport {
            changed_files: vec!["src/core.rs".to_string()],
            directly_impacted: Vec::new(),
            transitively_impacted: Vec::new(),
            impacted_file_count: 0,
            risk_score: 0.0,
        };
        let intel = refact_git_intel::GitIntel::default();

        assert!(pr_blast_suggested_reviewers(&report, None, Some(&intel)).is_empty());
        assert!(pr_blast_suggested_reviewers(&report, Some(&abs("repo")), None).is_empty());
        assert!(pr_blast_is_bot_author("robot[bot]@example.com"));
        assert!(pr_blast_is_bot_author("review-agent@example.com"));
        assert!(pr_blast_is_bot_author(
            "123+noreply@users.noreply.github.com"
        ));
    }

    fn branchy_hotspot_source(a: i32, b: i32, c: i32) -> String {
        format!(
            "pub fn hot(x: i32) -> i32 {{\n    if x > 0 {{\n        if x > 1 {{\n            for i in 0..x {{\n                if i % 2 == 0 {{\n                    return {a};\n                }}\n            }}\n            return {b};\n        }}\n    }}\n    {c}\n}}\n"
        )
    }

    fn brain_method_source(caller_count: usize) -> String {
        let mut src = "fn brain(x: i32) -> i32 {\n".to_string();
        for i in 0..75 {
            src.push_str(&format!(
                "    if x > {i} {{ if x % 2 == 0 {{ if x < 100 {{ if x != 42 {{ return {i}; }} }} }} }}\n"
            ));
        }
        src.push_str("    0\n}\n");
        for i in 0..caller_count {
            src.push_str(&format!("fn caller_{i}(x: i32) -> i32 {{ brain(x) }}\n"));
        }
        src
    }

    async fn service_with_file(
        path: &str,
        text: &str,
        lang: &str,
    ) -> Arc<refact_codegraph::CodeGraphService> {
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        service.index_file(path, text, lang).await.unwrap();
        service.connect_usages().await.unwrap();
        service
    }

    #[tokio::test]
    async fn brain_method_downgraded_at_zero_fanin() {
        let text = brain_method_source(0);
        let service = service_with_file("src/a.rs", &text, "rust").await;
        let health = refact_codehealth::analyze("rust", &text);
        let findings = refact_codehealth::biomarkers::detect_biomarkers("rust", &text);
        let original = findings
            .iter()
            .find(|finding| finding.biomarker == "brain_method")
            .unwrap()
            .severity;

        let snapshot = health_graph_snapshot(Some(&service), None).await;
        let enriched =
            enrich_health_findings(snapshot.as_ref(), "src/a.rs", &health.functions, findings);
        let brain = enriched
            .iter()
            .find(|finding| finding.finding.biomarker == "brain_method")
            .unwrap();

        assert_eq!(brain.finding.severity, downgrade_health_severity(original));
        assert!(brain.finding.detail.contains("low fan-in"));
    }

    #[tokio::test]
    async fn annotated_at_high_fanin() {
        let text = brain_method_source(5);
        let service = service_with_file("src/a.rs", &text, "rust").await;
        let health = refact_codehealth::analyze("rust", &text);
        let findings = refact_codehealth::biomarkers::detect_biomarkers("rust", &text);
        let original = findings
            .iter()
            .find(|finding| finding.biomarker == "brain_method")
            .unwrap()
            .severity;

        let snapshot = health_graph_snapshot(Some(&service), None).await;
        let enriched =
            enrich_health_findings(snapshot.as_ref(), "src/a.rs", &health.functions, findings);
        let brain = enriched
            .iter()
            .find(|finding| finding.finding.biomarker == "brain_method")
            .unwrap();

        assert_eq!(brain.finding.severity, original);
        assert!(brain.finding.detail.contains("called from 5 sites"));
    }

    #[tokio::test]
    async fn scoped_health_graph_excludes_generated_fanin_and_hot_path() {
        let active = brain_method_source(0);
        let excluded = r#"
fn main() { brain(0); }
fn caller_1() { brain(1); }
fn caller_2() { brain(2); }
fn caller_3() { brain(3); }
fn caller_4() { brain(4); }
fn caller_5() { brain(5); }
"#;
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        service
            .index_file("src/a.rs", &active, "rust")
            .await
            .unwrap();
        service
            .index_file("dist/generated.rs", excluded, "rust")
            .await
            .unwrap();
        service.connect_usages().await.unwrap();
        let health = refact_codehealth::analyze("rust", &active);
        let brain = health
            .functions
            .iter()
            .find(|function| function.name == "brain")
            .unwrap();
        let key = function_key(brain);

        let unscoped = health_graph_snapshot(Some(&service), None).await.unwrap();
        let unscoped_context = health_graph_context(&unscoped, "src/a.rs", &health.functions);
        assert!(unscoped_context.fan_in.get(&key).copied().unwrap_or(0) >= 5);
        assert!(unscoped_context.hot_path.contains(&key));

        let scoped = health_graph_snapshot_with_scope(
            Some(&service),
            Some(&abs("workspace/refact")),
            Some(&abs("workspace/refact")),
        )
        .await
        .unwrap();
        let scoped_context = health_graph_context(&scoped, "src/a.rs", &health.functions);
        assert_eq!(scoped_context.fan_in.get(&key), Some(&0));
        assert!(!scoped_context.hot_path.contains(&key));
        assert!(scoped
            .cached
            .data
            .nodes
            .iter()
            .all(|(_, _, path)| !path.contains("dist/")));
    }

    #[tokio::test]
    async fn perf_finding_hot_path_from_route() {
        let text = r#"
from fastapi import FastAPI
import requests
app = FastAPI()

@app.get("/users")
def handler():
    return helper()

def helper():
    for u in urls:
        for p in pages:
            requests.get(p)

def orphan():
    for u in urls:
        for p in pages:
            requests.get(p)
"#;
        let service = service_with_file("src/api.py", text, "python").await;
        let health = refact_codehealth::analyze("python", text);
        let findings = refact_codehealth::perf::detect_perf("python", text);
        let helper = health
            .functions
            .iter()
            .find(|function| function.name == "helper")
            .unwrap();
        let orphan = health
            .functions
            .iter()
            .find(|function| function.name == "orphan")
            .unwrap();

        let snapshot = health_graph_snapshot(Some(&service), None).await;
        let enriched =
            enrich_health_findings(snapshot.as_ref(), "src/api.py", &health.functions, findings);

        assert!(enriched.iter().any(|finding| {
            finding.hot_path
                && finding.finding.dimension
                    == refact_codehealth::biomarkers::Dimension::Performance
                && helper.line1 <= finding.finding.line
                && finding.finding.line <= function_line2(helper)
        }));
        assert!(enriched.iter().any(|finding| {
            !finding.hot_path
                && finding.finding.dimension
                    == refact_codehealth::biomarkers::Dimension::Performance
                && orphan.line1 <= finding.finding.line
                && finding.finding.line <= function_line2(orphan)
        }));
    }

    #[tokio::test]
    async fn build_git_meta_fills_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file_at(
            &repo,
            "src/a.rs",
            &branchy_hotspot_source(1, 1, 1),
            "introduce hot",
            "Alice",
            "alice@example.com",
            1_600_000_000,
        );
        commit_file_at(
            &repo,
            "src/a.rs",
            &branchy_hotspot_source(2, 1, 1),
            "fix crash in hot",
            "Bob",
            "bob@example.com",
            1_600_000_100,
        );
        commit_file_at(
            &repo,
            "src/a.rs",
            &branchy_hotspot_source(2, 2, 1),
            "update hot",
            "Carol",
            "carol@example.com",
            1_600_000_200,
        );
        let intel = refact_git_intel::mine_history(dir.path(), 20).unwrap();
        let ranges = vec![("hot".to_string(), 1, 11)];

        let meta = build_git_meta(&intel, dir.path(), "src/a.rs", None, Some(&ranges));

        assert_eq!(meta.prior_defect_count, 1);
        assert!(meta.is_stable);
        assert_eq!(meta.functions.len(), 1);
        assert_eq!(meta.functions[0].name, "hot");
        assert!(meta.functions[0].modification_count >= 3);
        assert!(meta.functions[0].ccn >= 4);
        assert!(meta.repo_function_mod_p80.is_some());
        assert_eq!(meta.repo_commit_counts.get("src/a.rs"), Some(&3));
    }

    #[tokio::test]
    async fn hidden_coupling_fires_without_import_edge() {
        let dir = tempfile::tempdir().unwrap();
        let mut intel = refact_git_intel::GitIntel::default();
        intel.file_churn.insert("src/a.rs".into(), 6);
        intel.file_churn.insert("src/b.rs".into(), 6);
        intel
            .co_change
            .insert(("src/a.rs".into(), "src/b.rs".into()), 6);
        intel.commit_records = (0..6)
            .map(|i| refact_git_intel::CommitRecord {
                oid: Some(format!("c{i}")),
                ts: 1_700_000_000 + i,
                author: "dev@example.com".into(),
                committer: "dev@example.com".into(),
                message: format!("change {i}"),
                files: vec![("src/a.rs".into(), 1, 0), ("src/b.rs".into(), 1, 0)],
            })
            .collect();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();

        let meta = build_git_meta(&intel, dir.path(), "src/a.rs", None, None);
        let findings = refact_codehealth::git_biomarkers::git_biomarkers(&meta);

        assert!(findings
            .iter()
            .any(|finding| finding.biomarker == "hidden_coupling"));
    }

    #[tokio::test]
    async fn suppressed_with_import_edge() {
        let dir = tempfile::tempdir().unwrap();
        let mut intel = refact_git_intel::GitIntel::default();
        intel.file_churn.insert("src/a.rs".into(), 6);
        intel.file_churn.insert("src/b.rs".into(), 6);
        intel
            .co_change
            .insert(("src/a.rs".into(), "src/b.rs".into()), 6);
        intel.commit_records = (0..6)
            .map(|i| refact_git_intel::CommitRecord {
                oid: Some(format!("c{i}")),
                ts: 1_700_000_000 + i,
                author: "dev@example.com".into(),
                committer: "dev@example.com".into(),
                message: format!("change {i}"),
                files: vec![("src/a.rs".into(), 1, 0), ("src/b.rs".into(), 1, 0)],
            })
            .collect();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() { b(); }\n").unwrap();
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        service
            .index_file("src/a.rs", "fn a() { b(); }\n", "rust")
            .await
            .unwrap();
        service
            .index_file("src/b.rs", "fn b() {}\n", "rust")
            .await
            .unwrap();
        service.connect_usages().await.unwrap();

        let snapshot = health_graph_snapshot(Some(&service), Some(dir.path())).await;
        let meta = build_git_meta(&intel, dir.path(), "src/a.rs", snapshot.as_ref(), None);
        let findings = refact_codehealth::git_biomarkers::git_biomarkers(&meta);

        assert!(meta.import_edges.contains("src/b.rs"));
        assert!(!findings
            .iter()
            .any(|finding| finding.biomarker == "hidden_coupling"));
    }

    #[tokio::test]
    async fn build_git_risk_assembly_reaches_dead_detectors() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file_at(
            &repo,
            "src/a.rs",
            &branchy_hotspot_source(1, 1, 1),
            "introduce hot",
            "Alice",
            "alice@example.com",
            1_600_000_000,
        );
        commit_file_at(
            &repo,
            "src/a.rs",
            &branchy_hotspot_source(2, 1, 1),
            "fix crash in hot",
            "Bob",
            "bob@example.com",
            1_600_000_100,
        );
        commit_file_at(
            &repo,
            "src/a.rs",
            &branchy_hotspot_source(2, 2, 1),
            "fix regression in hot",
            "Carol",
            "carol@example.com",
            1_600_000_200,
        );
        commit_file_at(
            &repo,
            "src/a.rs",
            &branchy_hotspot_source(2, 2, 2),
            "update hot again",
            "Dana",
            "dana@example.com",
            1_600_000_300,
        );
        let intel = refact_git_intel::mine_history(dir.path(), 20).unwrap();

        let assembly = build_git_risk_assembly(&intel, dir.path(), None, None, 1, None).await;
        let finding_names = assembly.files[0]
            .findings
            .iter()
            .map(|finding| finding.biomarker.as_str())
            .collect::<HashSet<_>>();

        assert!(finding_names.contains("prior_defect"));
        assert!(finding_names.contains("function_hotspot"));
        assert!(finding_names.contains("code_age_volatility"));
    }

    #[test]
    fn git_meta_reads_worktree_when_indexed_root_differs() {
        let temp = tempfile::tempdir().unwrap();
        let git_root = temp.path().join("worktree");
        let indexed_root = temp.path().join("source");
        std::fs::create_dir_all(git_root.join("src")).unwrap();
        std::fs::create_dir_all(indexed_root.join("src")).unwrap();
        let repo = Repository::init(&git_root).unwrap();
        commit_file_at(
            &repo,
            "src/a.rs",
            "fn worktree_only() {\n    if true {}\n}\n",
            "active",
            "Alice",
            "alice@example.com",
            1_700_000_000,
        );
        std::fs::write(indexed_root.join("src/a.rs"), "fn source_only() {}\n").unwrap();
        let intel = refact_git_intel::mine_history(&git_root, 20).unwrap();
        let ranges = function_ranges_from_disk(&git_root, "src/a.rs");

        let meta = build_git_meta(&intel, &git_root, "src/a.rs", None, Some(&ranges));

        assert_eq!(meta.nloc, 3);
        assert!(ranges.iter().any(|(name, _, _)| name == "worktree_only"));
        assert!(!ranges.iter().any(|(name, _, _)| name == "source_only"));
        assert_eq!(
            code_intel_relative_path(
                &indexed_root,
                &indexed_root.join("src/a.rs").to_string_lossy()
            ),
            Some(PathBuf::from("src/a.rs"))
        );
    }

    fn shared_clone_body(name: &str) -> String {
        let mut src = format!("fn {name}(input: i32) -> i32 {{\n    let mut total = input;\n");
        for _ in 0..30 {
            src.push_str("    total = total + input;\n    total = total - input;\n");
        }
        src.push_str("    total\n}\n");
        src
    }

    #[test]
    fn git_risk_tool_and_http_share_assembly_text() {
        let mut intel = refact_git_intel::GitIntel::default();
        intel.file_churn.insert("src/a.rs".into(), 3);
        intel.file_authors.insert(
            "src/a.rs".into(),
            HashMap::from([
                ("alice@example.com".into(), 2),
                ("bob@example.com".into(), 1),
            ]),
        );
        intel.commit_records = vec![refact_git_intel::CommitRecord {
            oid: Some("abcdef1234567890".into()),
            ts: 1,
            author: "alice@example.com".into(),
            committer: "alice@example.com".into(),
            message: "large risky change".into(),
            files: vec![("src/a.rs".into(), 200, 10)],
        }];
        let meta = refact_codehealth::git_biomarkers::GitMeta {
            file_path: "src/a.rs".into(),
            change_entropy: 0.0,
            change_entropy_pct: 0.0,
            commit_count_90d: 1,
            commit_count_total: 3,
            is_hotspot: true,
            is_stable: false,
            churn_percentile: 1.0,
            lines_added_90d: 200,
            lines_deleted_90d: 10,
            nloc: 20,
            contributor_count: 2,
            primary_owner_commit_pct: 0.67,
            primary_owner_name: "alice@example.com".into(),
            recent_owner_name: "alice@example.com".into(),
            recent_owner_commit_pct: 1.0,
            bus_factor: 2,
            prior_defect_count: 1,
            repo_active_contributors_90d: Some(2),
            repo_function_mod_p80: None,
            co_change_partners: Vec::new(),
            top_authors: vec![
                ("alice@example.com".into(), 2),
                ("bob@example.com".into(), 1),
            ],
            functions: Vec::new(),
            repo_commit_counts: HashMap::from([("src/a.rs".into(), 3)]),
            import_edges: HashSet::new(),
        };
        let findings = refact_codehealth::git_biomarkers::git_biomarkers(&meta);
        let assembly = GitRiskAssembly {
            files: vec![GitRiskFileAssembly {
                meta,
                findings,
                temporal_score: 1.0,
                churn_risk: 0.5,
            }],
            recent_commit_risks: recent_commit_risk_summaries(&intel),
        };

        let response = git_risk_response(&intel, &assembly);

        assert!(response
            .hotspots
            .iter()
            .any(|hotspot| hotspot.path == "src/a.rs"));
        assert!(response
            .findings
            .iter()
            .any(|finding| finding.path == "src/a.rs"));
        assert!(!response.recent_commit_risks.is_empty());
    }

    fn empty_health_ctx<'a>(
        trend_findings: &'a [refact_codehealth::biomarkers::Finding],
        git_function_fact_paths: &'a HashSet<String>,
    ) -> HealthAnalysisContext<'a> {
        HealthAnalysisContext {
            repo_root: None,
            intel: None,
            graph: None,
            coverage: None,
            trend_findings,
            git_function_fact_paths,
        }
    }

    fn duplicate_health_source() -> String {
        let mut body = String::new();
        for name in ["alpha", "beta"] {
            body.push_str(&format!(
                "fn {name}(input: i32) -> i32 {{\n    let mut total = input;\n"
            ));
            for _ in 0..30 {
                body.push_str("    total = total + input;\n    total = total - input;\n");
            }
            body.push_str("    total\n}\n");
        }
        body
    }

    #[test]
    fn dense_short_clone_does_not_trigger_health_dry_finding() {
        let dense_line = (0..30)
            .map(|_| "total = total + input; total = total - input;")
            .collect::<Vec<_>>()
            .join(" ");
        let src = format!(
            "fn alpha(input: i32) -> i32 {{ let mut total = input; {dense_line} total }}\nfn beta(value: i32) -> i32 {{ let mut total = value; {} total }}\n",
            dense_line.replace("input", "value")
        );
        let clones = refact_codehealth::duplication::detect_clones("rust", &src);

        assert!(
            clones.iter().any(|clone| clone.token_len >= 60
                && clone
                    .a_end_line
                    .saturating_sub(clone.line_a)
                    .saturating_add(1)
                    <= 3
                && clone
                    .b_end_line
                    .saturating_sub(clone.line_b)
                    .saturating_add(1)
                    <= 3),
            "expected dense short clone, got {clones:?}"
        );
        assert!(health_dry_findings("src/dense.rs", "rust", &src, 1.0).is_empty());
    }

    #[tokio::test]
    async fn structural_only_fixture_score_unchanged() {
        reset_health_cache_for_tests();
        let src = "fn branchy(x: i32) -> i32 {\n    if x > 0 {\n        if x > 1 {\n            return x;\n        }\n    }\n    0\n}\n";
        let trends = Vec::new();
        let git_paths = HashSet::new();
        let ctx = empty_health_ctx(&trends, &git_paths);

        let analysis =
            analyze_health_file_shared("src/a.rs".to_string(), src.to_string(), &ctx).unwrap();
        let mut expected_findings = refact_codehealth::biomarkers::detect_biomarkers("rust", src);
        expected_findings.extend(refact_codehealth::perf::detect_perf("rust", src));
        let expected = refact_codehealth::scoring::score_file(&expected_findings);

        assert_eq!(analysis.defect_score, expected.defect);
        assert_eq!(analysis.maintainability_score, expected.maintainability);
        assert_eq!(analysis.performance_score, expected.performance);
    }

    #[tokio::test]
    async fn health_cache_hits_on_unchanged_content_and_invalidates_on_edit() {
        reset_health_cache_for_tests();
        let trends = Vec::new();
        let git_paths = HashSet::new();
        let ctx = empty_health_ctx(&trends, &git_paths);
        let src = "fn a() -> i32 { 1 }\n".to_string();

        let first = analyze_health_file_shared(
            "src/health_cache_probe_unique.rs".to_string(),
            src.clone(),
            &ctx,
        )
        .unwrap();
        let second =
            analyze_health_file_shared("src/health_cache_probe_unique.rs".to_string(), src, &ctx)
                .unwrap();
        let third = analyze_health_file_shared(
            "src/health_cache_probe_unique.rs".to_string(),
            "fn a() -> i32 { if true { 1 } else { 0 } }\n".to_string(),
            &ctx,
        )
        .unwrap();

        assert!(!first.cache_hit);
        assert!(second.cache_hit);
        assert!(!third.cache_hit);
        // NOTE: no global-stats assertion here — the cache/counters are process-global
        // and other parallel tests also exercise analyze_health_file_shared; the three
        // per-call cache_hit flags above are the race-free proof of hit/invalidate.
    }

    #[test]
    fn health_cache_is_bounded_and_uses_lru_eviction() {
        reset_health_cache_for_tests();
        let trends = Vec::new();
        let git_paths = HashSet::new();
        let ctx = empty_health_ctx(&trends, &git_paths);
        let text = "fn probe() {}\n".to_string();

        for index in 0..=HEALTH_ANALYSIS_CACHE_CAPACITY {
            analyze_health_file_shared(format!("src/cache_{index}.rs"), text.clone(), &ctx)
                .unwrap();
        }

        let cache = health_analysis_cache().lock().unwrap();
        assert!(cache.files.len() <= HEALTH_ANALYSIS_CACHE_CAPACITY);
        assert!(cache.lru.len() <= HEALTH_ANALYSIS_CACHE_CAPACITY);
    }

    #[tokio::test]
    async fn health_cache_separates_graph_scope_and_generation() {
        reset_health_cache_for_tests();
        let text = brain_method_source(0);
        let service = service_with_file("src/health_graph_cache_probe.rs", &text, "rust").await;
        let trends = Vec::new();
        let git_paths = HashSet::new();

        let global = health_graph_snapshot(Some(&service), Some(&abs("workspace/refact")))
            .await
            .unwrap();
        let global_ctx = HealthAnalysisContext {
            repo_root: None,
            intel: None,
            graph: Some(&global),
            coverage: None,
            trend_findings: &trends,
            git_function_fact_paths: &git_paths,
        };
        let first = analyze_health_file_shared(
            "src/health_graph_cache_probe.rs".into(),
            text.clone(),
            &global_ctx,
        )
        .unwrap();
        let global_hit = analyze_health_file_shared(
            "src/health_graph_cache_probe.rs".into(),
            text.clone(),
            &global_ctx,
        )
        .unwrap();

        let scoped = health_graph_snapshot_with_scope(
            Some(&service),
            Some(&abs("workspace/refact")),
            Some(&abs("workspace/refact")),
        )
        .await
        .unwrap();
        let scoped_ctx = HealthAnalysisContext {
            graph: Some(&scoped),
            ..global_ctx
        };
        let scoped_first = analyze_health_file_shared(
            "src/health_graph_cache_probe.rs".into(),
            text.clone(),
            &scoped_ctx,
        )
        .unwrap();

        let other_indexed_root =
            health_graph_snapshot(Some(&service), Some(&abs("workspace/other")))
                .await
                .unwrap();
        let other_indexed_ctx = HealthAnalysisContext {
            graph: Some(&other_indexed_root),
            ..global_ctx
        };
        let other_indexed_first = analyze_health_file_shared(
            "src/health_graph_cache_probe.rs".into(),
            text.clone(),
            &other_indexed_ctx,
        )
        .unwrap();

        service
            .index_file(
                "src/health_graph_generation_probe.rs",
                "fn changed() {}",
                "rust",
            )
            .await
            .unwrap();
        let next_generation = health_graph_snapshot(Some(&service), Some(&abs("workspace/refact")))
            .await
            .unwrap();
        let next_generation_ctx = HealthAnalysisContext {
            graph: Some(&next_generation),
            ..global_ctx
        };
        let generation_first = analyze_health_file_shared(
            "src/health_graph_cache_probe.rs".into(),
            text,
            &next_generation_ctx,
        )
        .unwrap();

        assert!(!first.cache_hit);
        assert!(global_hit.cache_hit);
        assert!(!scoped_first.cache_hit);
        assert!(!other_indexed_first.cache_hit);
        assert!(!generation_first.cache_hit);
        assert_ne!(global.scope_signature, scoped.scope_signature);
        assert_ne!(global.scope_signature, other_indexed_root.scope_signature);
        assert_ne!(global.graph_generation, next_generation.graph_generation);
        let expected_indexed_root = abs_str("workspace/refact");
        assert_eq!(
            global
                .import_index
                .as_ref()
                .map(|index| index.indexed_root.as_str()),
            Some(expected_indexed_root.as_str())
        );
    }

    #[tokio::test]
    async fn health_cache_separates_codegraph_service_instances() {
        reset_health_cache_for_tests();
        let text = brain_method_source(0);
        let first_service =
            service_with_file("src/health_service_cache_probe.rs", &text, "rust").await;
        let second_service =
            service_with_file("src/health_service_cache_probe.rs", &text, "rust").await;
        let first_graph =
            health_graph_snapshot(Some(&first_service), Some(&abs("workspace/refact")))
                .await
                .unwrap();
        let second_graph =
            health_graph_snapshot(Some(&second_service), Some(&abs("workspace/refact")))
                .await
                .unwrap();
        assert_eq!(first_graph.graph_generation, second_graph.graph_generation);
        assert_eq!(first_graph.scope_signature, second_graph.scope_signature);
        assert_ne!(first_graph.service_identity, second_graph.service_identity);

        let trends = Vec::new();
        let git_paths = HashSet::new();
        let first_ctx = HealthAnalysisContext {
            repo_root: None,
            intel: None,
            graph: Some(&first_graph),
            coverage: None,
            trend_findings: &trends,
            git_function_fact_paths: &git_paths,
        };
        let second_ctx = HealthAnalysisContext {
            graph: Some(&second_graph),
            ..first_ctx
        };
        let first = analyze_health_file_shared(
            "src/health_service_cache_probe.rs".into(),
            text.clone(),
            &first_ctx,
        )
        .unwrap();
        let second = analyze_health_file_shared(
            "src/health_service_cache_probe.rs".into(),
            text,
            &second_ctx,
        )
        .unwrap();

        assert!(!first.cache_hit);
        assert!(!second.cache_hit);
    }

    #[tokio::test]
    async fn health_cache_does_not_pin_codegraph_service() {
        reset_health_cache_for_tests();
        let text = "fn cached() {}\n".to_string();
        let service = service_with_file("src/weak_cache_probe.rs", &text, "rust").await;
        let weak = Arc::downgrade(&service);
        let graph = health_graph_snapshot(Some(&service), None).await.unwrap();
        let trends = Vec::new();
        let git_paths = HashSet::new();
        let ctx = HealthAnalysisContext {
            repo_root: None,
            intel: None,
            graph: Some(&graph),
            coverage: None,
            trend_findings: &trends,
            git_function_fact_paths: &git_paths,
        };
        analyze_health_file_shared("src/weak_cache_probe.rs".into(), text, &ctx).unwrap();

        drop(graph);
        drop(service);
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn health_scores_include_git_and_dry_categories() {
        reset_health_cache_for_tests();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let src = duplicate_health_source();
        std::fs::write(dir.path().join("src/a.rs"), &src).unwrap();
        let mut intel = refact_git_intel::GitIntel::default();
        intel.file_churn.insert("src/a.rs".to_string(), 6);
        intel.fix_commit_counts.insert("src/a.rs".to_string(), 3);
        intel.commits_analyzed = 6;
        let trends = Vec::new();
        let git_paths = top_git_function_fact_paths(Some(&intel));
        let ctx = HealthAnalysisContext {
            repo_root: Some(dir.path()),
            intel: Some(&intel),
            graph: None,
            coverage: None,
            trend_findings: &trends,
            git_function_fact_paths: &git_paths,
        };

        let analysis = analyze_health_file_shared("src/a.rs".to_string(), src, &ctx).unwrap();
        let categories = analysis
            .findings
            .iter()
            .map(|finding| finding.finding.category.as_str())
            .collect::<HashSet<_>>();
        let impacts = analysis
            .health_impact
            .iter()
            .map(|impact| impact.biomarker.as_str())
            .collect::<HashSet<_>>();

        assert!(
            categories.contains("duplication"),
            "{:?}",
            analysis.findings
        );
        assert!(
            categories.contains("organizational"),
            "{:?}",
            analysis.findings
        );
        assert!(impacts.contains("prior_defect") || impacts.contains("dry_violation"));
    }

    #[tokio::test]
    async fn no_coverage_file_means_zero_coverage_findings() {
        reset_health_cache_for_tests();
        let trends = Vec::new();
        let git_paths = HashSet::new();
        let ctx = empty_health_ctx(&trends, &git_paths);

        let analysis = analyze_health_file_shared(
            "src/a.rs".to_string(),
            "fn a() -> i32 { 1 }\n".to_string(),
            &ctx,
        )
        .unwrap();

        assert!(analysis.findings.iter().all(|finding| {
            finding.finding.category != "test_coverage"
                && finding.finding.category != "coverage_gradient"
        }));
    }

    #[test]
    fn worktree_coverage_matches_relative_and_worktree_absolute_paths() {
        let temp = tempfile::tempdir().unwrap();
        let indexed_root = temp.path().join("source");
        let worktree_root = temp.path().join("worktree");
        let relative_worktree_path = worktree_root.join("src/relative.rs");
        let absolute_worktree_path = worktree_root.join("src/absolute.rs");
        let lcov = format!(
            "TN:\nSF:src/relative.rs\nDA:1,0\nend_of_record\nTN:\nSF:{}\nDA:1,0\nend_of_record\n",
            absolute_worktree_path.to_string_lossy()
        );
        let coverage = refact_codehealth::coverage::detect_and_parse(&lcov).unwrap();

        let relative = coverage_file_for_path(
            &coverage,
            &indexed_root.join("src/relative.rs").to_string_lossy(),
            Some(&relative_worktree_path),
            Some(&worktree_root),
        )
        .unwrap();
        let absolute = coverage_file_for_path(
            &coverage,
            &indexed_root.join("src/absolute.rs").to_string_lossy(),
            Some(&absolute_worktree_path),
            Some(&worktree_root),
        )
        .unwrap();

        assert_eq!(relative.path.replace('\\', "/"), "src/relative.rs");
        assert_eq!(Path::new(&absolute.path), absolute_worktree_path.as_path());
    }

    #[test]
    fn coverage_basename_fallback_requires_report_wide_uniqueness() {
        let ambiguous = refact_codehealth::coverage::detect_and_parse(
            "TN:\nSF:src/shared.rs\nDA:1,0\nend_of_record\nTN:\nSF:tests/shared.rs\nDA:1,1\nend_of_record\n",
        )
        .unwrap();
        let unique = refact_codehealth::coverage::detect_and_parse(
            "TN:\nSF:generated/shared.rs\nDA:1,0\nend_of_record\nTN:\nSF:other.rs\nDA:1,1\nend_of_record\n",
        )
        .unwrap();

        assert!(coverage_file_for_path(
            &ambiguous,
            &abs_str("indexed/worktree/shared.rs"),
            None,
            None,
        )
        .is_none());
        assert_eq!(
            coverage_file_for_path(&unique, &abs_str("indexed/worktree/shared.rs"), None, None)
                .unwrap()
                .path,
            "generated/shared.rs"
        );
    }

    #[test]
    fn coverage_prefers_exact_directory_match_over_same_basename() {
        let coverage = refact_codehealth::coverage::detect_and_parse(
            "TN:\nSF:src/shared.rs\nDA:1,0\nend_of_record\nTN:\nSF:tests/shared.rs\nDA:1,1\nend_of_record\n",
        )
        .unwrap();

        let matched = coverage_file_for_path(&coverage, "src/shared.rs", None, None).unwrap();
        assert_eq!(matched.path, "src/shared.rs");
    }

    #[tokio::test]
    async fn trend_snapshot_persisted_and_trend_finding_fires() {
        reset_health_cache_for_tests();
        let service = refact_codegraph::CodeGraphService::open_in_memory().unwrap();
        let snapshots = vec![
            health_snap(1, "src/a.rs", 10.0),
            health_snap(2, "src/a.rs", 8.8),
            health_snap(3, "src/a.rs", 7.6),
        ];
        persist_health_snapshots(&service, &snapshots)
            .await
            .unwrap();
        let mut loaded = load_health_snapshots(&service).await;
        push_health_snapshot(&mut loaded, health_snap(4, "src/a.rs", 6.4));
        let trends = refact_codehealth::trends::evaluate_trends(&loaded);
        let git_paths = HashSet::new();
        let ctx = empty_health_ctx(&trends, &git_paths);

        let analysis = analyze_health_file_shared(
            "src/a.rs".to_string(),
            "fn a() -> i32 { 1 }\n".to_string(),
            &ctx,
        )
        .unwrap();
        persist_health_snapshots(&service, &loaded).await.unwrap();
        let snapshots = load_health_snapshots(&service).await;

        assert!(analysis
            .findings
            .iter()
            .any(|finding| finding.finding.biomarker == "health_declining"));
        assert_eq!(snapshots.len(), 4);
    }

    fn health_snap(ts: i64, path: &str, score: f64) -> refact_codehealth::trends::HealthSnapshot {
        refact_codehealth::trends::HealthSnapshot {
            ts,
            per_file: HashMap::from([(path.to_string(), score)]),
            aggregate: score,
        }
    }

    #[test]
    fn basename_collision_does_not_cross_bind() {
        let matched = health_stored_path_for_request(
            vec!["a/util.rs".to_string(), "b/util.rs".to_string()],
            "util.rs",
            Some(&abs("repo")),
        );

        assert_eq!(matched, None);
    }

    #[tokio::test]
    async fn clone_cache_reuses_until_generation_bump() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let service = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        service
            .index_file("src/a.rs", &shared_clone_body("alpha"), "rust")
            .await
            .unwrap();
        service
            .index_file("src/b.rs", &shared_clone_body("beta"), "rust")
            .await
            .unwrap();
        *gcx.codegraph.lock().await = Some(service.clone());

        let first = cached_cross_file_clones(gcx.clone()).await.unwrap();
        let second = cached_cross_file_clones(gcx.clone()).await.unwrap();

        assert!(
            !first.clones.is_empty(),
            "expected cross-file clone between a.rs and b.rs"
        );
        assert!(
            Arc::ptr_eq(&first, &second),
            "same generation must serve the cached analysis"
        );

        service
            .index_file("src/c.rs", "fn gamma() -> i32 { 3 }\n", "rust")
            .await
            .unwrap();
        let third = cached_cross_file_clones(gcx.clone()).await.unwrap();
        assert!(
            !Arc::ptr_eq(&second, &third),
            "generation bump must rebuild the clone analysis"
        );
    }
}
