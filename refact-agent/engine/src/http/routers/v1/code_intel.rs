pub use refact_http::routers::v1::code_intel::*;

#[cfg(test)]
use std::collections::BTreeSet;
#[cfg(test)]
use hyper::StatusCode;

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use git2::{Repository, Signature, Time};
    use hyper::body::to_bytes;
    use tower::ServiceExt;

    use super::*;
    use crate::http::routers::make_refact_http_server;

    fn commit_file_as(
        repo: &Repository,
        path: &str,
        contents: &str,
        msg: &str,
        name: &str,
        email: &str,
    ) {
        let workdir = repo.workdir().unwrap();
        let full_path = workdir.join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full_path, contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let time = Time::new(1_700_000_000, 0);
        let sig = Signature::new(name, email, &time).unwrap();
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .unwrap();
    }

    async fn router_with_codegraph() -> axum::Router {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        codegraph
            .index_file(
                "src/lib.rs",
                "fn main() { helper(); }\nfn helper() {}\nfn orphan() {}\n",
                "rust",
            )
            .await
            .unwrap();
        codegraph.connect_usages().await.unwrap();
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        make_refact_http_server(app)
    }

    async fn router_with_absolute_codegraph() -> (axum::Router, String) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let core_path = repo.join("src/core.rs").to_string_lossy().to_string();
        let caller_path = repo.join("src/caller.rs").to_string_lossy().to_string();
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        codegraph
            .index_file(&core_path, "pub fn core() {}\n", "rust")
            .await
            .unwrap();
        codegraph
            .index_file(&caller_path, "fn caller() { core(); }\n", "rust")
            .await
            .unwrap();
        codegraph.connect_usages().await.unwrap();
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        (make_refact_http_server(app), caller_path)
    }

    async fn router_with_pr_blast_codegraph() -> (axum::Router, tempfile::TempDir) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file_as(
            &repo,
            "src/core.rs",
            "pub fn core() {}\n",
            "introduce core",
            "Caller Owner",
            "aaa-owner@example.com",
        );
        commit_file_as(
            &repo,
            "src/caller.rs",
            "fn caller() { core(); }\n",
            "introduce caller",
            "Other Caller Owner",
            "zzz-caller@example.com",
        );
        commit_file_as(
            &repo,
            "src/caller.rs",
            "fn caller() { core(); }\n// touched\n",
            "update caller",
            "Caller Owner",
            "aaa-owner@example.com",
        );
        commit_file_as(
            &repo,
            "src/base.py",
            "class Base:\n    pass\n",
            "introduce base",
            "Base Owner",
            "zzz-base@example.com",
        );
        commit_file_as(
            &repo,
            "src/derived.py",
            "class Derived(Base):\n    pass\n",
            "introduce derived",
            "Derived Owner",
            "aaa-owner@example.com",
        );
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        codegraph
            .index_file("src/core.rs", "pub fn core() {}\n", "rust")
            .await
            .unwrap();
        codegraph
            .index_file("src/caller.rs", "fn caller() { core(); }\n", "rust")
            .await
            .unwrap();
        codegraph
            .index_file("src/base.py", "class Base:\n    pass\n", "python")
            .await
            .unwrap();
        codegraph
            .index_file(
                "src/derived.py",
                "class Derived(Base):\n    pass\n",
                "python",
            )
            .await
            .unwrap();
        codegraph.connect_usages().await.unwrap();
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        (make_refact_http_server(app), dir)
    }

    async fn router_with_partial_pr_blast_codegraph() -> axum::Router {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        codegraph
            .index_file("src/core.rs", "pub fn core() {}\n", "rust")
            .await
            .unwrap();
        codegraph
            .index_file("src/caller.rs", "fn caller() { core(); }\n", "rust")
            .await
            .unwrap();
        codegraph.connect_usages().await.unwrap();
        codegraph.enqueue_files(&["src/pending.rs".to_string()]);
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        make_refact_http_server(app)
    }

    fn git_risk_source(a: i32, b: i32, c: i32) -> String {
        format!(
            "pub fn alpha(x: i32) -> i32 {{\n    if x > 0 {{\n        if x > 1 {{\n            for i in 0..x {{\n                if i % 2 == 0 {{\n                    return {a};\n                }}\n            }}\n            return {b};\n        }}\n    }}\n    {c}\n}}\n"
        )
    }

    async fn router_with_partial_codegraph() -> axum::Router {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        codegraph
            .index_file(
                "src/lib.rs",
                "fn main() { helper(); }\nfn helper() {}\nfn orphan() {}\n",
                "rust",
            )
            .await
            .unwrap();
        codegraph.connect_usages().await.unwrap();
        codegraph.enqueue_files(&["src/pending.rs".to_string()]);
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        make_refact_http_server(app)
    }

    async fn router_with_filtered_duplication_codegraph() -> axum::Router {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        let cloned = co_change_clone_source("shared_alpha", "shared");
        codegraph
            .index_file("src/a.rs", &cloned, "rust")
            .await
            .unwrap();
        codegraph
            .index_file("src/copy.rs", &cloned, "rust")
            .await
            .unwrap();
        codegraph
            .index_file(
                "src/b.rs",
                "fn independent(value: i32) -> i32 { value + 1 }\n",
                "rust",
            )
            .await
            .unwrap();
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        make_refact_http_server(app)
    }

    async fn router_with_overlapping_duplication_codegraph() -> axum::Router {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        let a = co_change_clone_source("shared_alpha", "shared-a");
        let b = co_change_clone_source("shared_beta", "shared-b");
        let c = co_change_clone_source("shared_gamma", "shared-c");
        codegraph.index_file("src/a.rs", &a, "rust").await.unwrap();
        codegraph.index_file("src/b.rs", &b, "rust").await.unwrap();
        codegraph.index_file("src/c.rs", &c, "rust").await.unwrap();
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        make_refact_http_server(app)
    }

    async fn router_with_empty_dead_code_partial_index() -> axum::Router {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        codegraph
            .index_file("src/lib.rs", "fn main() {}\n", "rust")
            .await
            .unwrap();
        codegraph.connect_usages().await.unwrap();
        codegraph.enqueue_files(&["src/pending.rs".to_string()]);
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        make_refact_http_server(app)
    }

    async fn router_with_git_risk_codegraph(
        nested_workspace: bool,
    ) -> (axum::Router, tempfile::TempDir) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        commit_file_as(
            &repo,
            "src/a.rs",
            &git_risk_source(1, 1, 1),
            "introduce alpha",
            "Alice",
            "alice@example.com",
        );
        commit_file_as(
            &repo,
            "src/b.rs",
            "pub fn beta() -> i32 { 2 }\n",
            "introduce beta",
            "Bob",
            "bob@example.com",
        );
        commit_file_as(
            &repo,
            "src/a.rs",
            &git_risk_source(2, 1, 1),
            "fix crash in alpha",
            "Bob",
            "bob@example.com",
        );
        commit_file_as(
            &repo,
            "src/a.rs",
            &git_risk_source(3, 1, 1),
            "update alpha",
            "Carol",
            "carol@example.com",
        );
        let workspace = if nested_workspace {
            let workspace = dir.path().join("refact-agent").join("engine");
            std::fs::create_dir_all(&workspace).unwrap();
            workspace
        } else {
            dir.path().to_path_buf()
        };
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace];
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        codegraph
            .index_file("src/a.rs", &git_risk_source(3, 1, 1), "rust")
            .await
            .unwrap();
        codegraph.connect_usages().await.unwrap();
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        (make_refact_http_server(app), dir)
    }

    async fn router_with_non_repo_git_risk_codegraph() -> (axum::Router, tempfile::TempDir) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let dir = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        (make_refact_http_server(app), dir)
    }

    async fn router_without_codegraph() -> axum::Router {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        make_refact_http_server(app)
    }

    async fn get_json(router: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body()).await.unwrap();
        let json = serde_json::from_slice(&body).unwrap();
        (status, json)
    }

    async fn post_json(
        router: axum::Router,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body()).await.unwrap();
        let json = serde_json::from_slice(&body).unwrap();
        (status, json)
    }

    async fn assert_get_off_detail(uri: &str) {
        let router = router_without_codegraph().await;

        let (status, json) = get_json(router, uri).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, serde_json::json!({"detail": "codegraph turned off"}));
    }

    async fn assert_post_off_detail(uri: &str, body: serde_json::Value) {
        let router = router_without_codegraph().await;

        let (status, json) = post_json(router, uri, body).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, serde_json::json!({"detail": "codegraph turned off"}));
    }

    #[tokio::test]
    async fn code_intel_overview_off_returns_detail() {
        assert_get_off_detail("/v1/code-intel/overview").await;
    }

    #[tokio::test]
    async fn code_intel_graph_off_returns_detail() {
        assert_get_off_detail("/v1/code-intel/graph").await;
    }

    #[tokio::test]
    async fn code_intel_communities_off_returns_detail() {
        assert_get_off_detail("/v1/code-intel/communities").await;
    }

    #[tokio::test]
    async fn code_intel_dead_code_off_returns_detail() {
        assert_get_off_detail("/v1/code-intel/dead-code").await;
    }

    #[tokio::test]
    async fn code_intel_health_off_returns_detail() {
        assert_get_off_detail("/v1/code-intel/health").await;
    }

    #[tokio::test]
    async fn code_intel_git_risk_off_returns_detail() {
        assert_get_off_detail("/v1/code-intel/git-risk").await;
    }

    #[tokio::test]
    async fn code_intel_duplication_off_returns_detail() {
        assert_get_off_detail("/v1/code-intel/duplication").await;
    }

    #[tokio::test]
    async fn code_intel_pr_blast_off_returns_detail() {
        assert_post_off_detail(
            "/v1/code-intel/pr-blast",
            serde_json::json!({"changed_files": ["src/lib.rs"]}),
        )
        .await;
    }

    #[tokio::test]
    async fn code_intel_security_scan_off_returns_detail() {
        assert_post_off_detail(
            "/v1/code-intel/security-scan",
            serde_json::json!({"lang": "rust", "text": "fn main() {}"}),
        )
        .await;
    }

    #[tokio::test]
    async fn code_intel_overview_populated_returns_shape() {
        let router = router_with_codegraph().await;

        let (status, json) = get_json(router, "/v1/code-intel/overview").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["counts"]["files"], 1);
        assert!(json["counts"]["nodes"].as_i64().unwrap() > 0);
        assert!(json["scc_count"].is_number());
        assert!(json["top_pagerank"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["symbol"].as_str() == Some("helper")
                    && entry["path"].as_str() == Some("src/lib.rs")
                    && entry["score"].is_number()
            }));
        assert_eq!(json["index_state"]["queued"], 0);
        assert_eq!(json["index_state"]["cross_file_ready"], true);
        assert!(json["file_centrality"]["top_pagerank"].is_array());
        assert!(json["community_count"].is_number());
        assert!(json["dead_code_count"].is_number());
    }

    #[tokio::test]
    async fn code_intel_graph_populated_returns_bounded_shape() {
        let router = router_with_codegraph().await;

        let (status, json) = get_json(router, "/v1/code-intel/graph?limit=1").await;

        assert_eq!(status, StatusCode::OK);
        let nodes = json["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0]["id"].is_i64());
        assert!(nodes[0]["name"].is_string());
        assert_eq!(nodes[0]["path"], "src/lib.rs");
        assert_eq!(json["index_state"]["queued"], 0);
        assert_eq!(json["index_state"]["cross_file_ready"], true);
        assert!(json["edges"].is_array());
    }

    #[tokio::test]
    async fn all_four_endpoints_report_index_state() {
        let router = router_with_partial_codegraph().await;

        let (overview_status, overview) = get_json(router.clone(), "/v1/code-intel/overview").await;
        let (graph_status, graph) = get_json(router.clone(), "/v1/code-intel/graph").await;
        let (communities_status, communities) =
            get_json(router.clone(), "/v1/code-intel/communities").await;
        let (dead_status, dead_code) = get_json(router, "/v1/code-intel/dead-code").await;

        assert_eq!(overview_status, StatusCode::OK);
        assert_eq!(graph_status, StatusCode::OK);
        assert_eq!(communities_status, StatusCode::OK);
        assert_eq!(dead_status, StatusCode::OK);
        assert_eq!(overview["index_state"]["queued"], 1);
        assert_eq!(overview["index_state"]["cross_file_ready"], false);
        assert_eq!(graph["index_state"]["queued"], 1);
        assert_eq!(graph["index_state"]["cross_file_ready"], false);
        let communities = communities.as_array().unwrap();
        assert!(!communities.is_empty());
        assert!(communities.iter().all(|entry| {
            entry["index_state"]["queued"] == 1 && entry["index_state"]["cross_file_ready"] == false
        }));
        assert!(!dead_code["entries"].as_array().unwrap().is_empty());
        assert_eq!(dead_code["index_state"]["queued"], 1);
        assert_eq!(dead_code["index_state"]["cross_file_ready"], false);
        assert_eq!(dead_code["partial"], true);
    }

    #[tokio::test]
    async fn code_intel_dead_code_empty_response_keeps_index_state() {
        let router = router_with_empty_dead_code_partial_index().await;

        let (status, json) = get_json(router, "/v1/code-intel/dead-code").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["entries"].as_array().unwrap().len(), 0);
        assert_eq!(json["index_state"]["queued"], 1);
        assert_eq!(json["index_state"]["cross_file_ready"], false);
        assert_eq!(json["partial"], true);
        assert!(json["warning"]
            .as_str()
            .unwrap()
            .contains("index still building"));
    }

    #[tokio::test]
    async fn code_intel_health_populated_returns_shape() {
        let router = router_with_codegraph().await;

        let (status, json) = get_json(router, "/v1/code-intel/health?limit=1").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["aggregate"]["file_count"], 1);
        assert!(json["aggregate"]["avg_score"].is_number());
        let files = json["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], "src/lib.rs");
        assert!(files[0]["score"].is_number());
        assert!(files[0]["grade"].is_string());
        assert!(files[0]["functions"].is_array());
        assert!(files[0]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|finding| finding["hot_path"].is_boolean()));
        assert_eq!(json["index_state"]["cross_file_ready"], true);
        assert!(files[0]["health_impact"].is_array());
        assert!(files[0]["maintainability_index"].is_number());
        assert!(files[0]["maintainability_signal"].is_number());
        assert!(json["aggregate"]["avg_maintainability_index"].is_number());
    }

    #[tokio::test]
    async fn code_intel_git_risk_repo_root_returns_shape() {
        let (router, _dir) = router_with_git_risk_codegraph(false).await;

        let (status, json) = get_json(router, "/v1/code-intel/git-risk?limit=2").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["commits_analyzed"], 4);
        let hotspots = json["hotspots"].as_array().unwrap();
        assert!(hotspots.iter().any(|entry| {
            entry["path"].as_str() == Some("src/a.rs")
                && entry["churn"].as_u64() == Some(3)
                && entry["risk"]
                    .as_f64()
                    .is_some_and(|risk| (0.0..=1.0).contains(&risk))
        }));
        assert!(json["ownership"].as_array().unwrap().iter().any(|entry| {
            entry["path"].as_str() == Some("src/a.rs")
                && entry["top_owner"].as_str().is_some()
                && entry["bus_factor"].as_u64().is_some_and(|bus| bus >= 1)
        }));
        assert!(json["findings"].as_array().unwrap().iter().any(|finding| {
            finding["path"].as_str() == Some("src/a.rs")
                && finding["biomarker"].as_str() == Some("prior_defect")
        }));
        assert!(!json["recent_commit_risks"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn code_intel_git_risk_nested_workspace_uses_enclosing_repo() {
        let (router, _dir) = router_with_git_risk_codegraph(true).await;

        let (status, json) = get_json(router, "/v1/code-intel/git-risk?limit=2").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["commits_analyzed"], 4);
        assert!(json["hotspots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"].as_str() == Some("src/a.rs")));
    }

    #[tokio::test]
    async fn code_intel_git_risk_non_repo_returns_empty_payload() {
        let (router, _dir) = router_with_non_repo_git_risk_codegraph().await;

        let (status, json) = get_json(router, "/v1/code-intel/git-risk").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["commits_analyzed"], 0);
        for field in [
            "hotspots",
            "ownership",
            "co_change",
            "coupling",
            "reviewers",
            "findings",
            "recent_commit_risks",
        ] {
            assert_eq!(json[field].as_array().unwrap().len(), 0, "{field}");
        }
    }

    #[tokio::test]
    async fn code_intel_duplication_populated_returns_shape() {
        let router = router_with_codegraph().await;

        let (status, json) = get_json(router, "/v1/code-intel/duplication?limit=1").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["aggregate"]["file_count"], 1);
        assert!(json["aggregate"]["duplication_pct"].is_number());
        assert!(json["clones"].is_array());
        assert!(json["dry_violations"].is_array());
        assert!(json["test_smells"].is_array());
    }

    #[tokio::test]
    async fn duplication_filtered_pct_is_scope_consistent() {
        let router = router_with_filtered_duplication_codegraph().await;

        let (unfiltered_status, unfiltered) =
            get_json(router.clone(), "/v1/code-intel/duplication?limit=5").await;
        let (filtered_status, filtered) =
            get_json(router, "/v1/code-intel/duplication?path=src/b.rs&limit=5").await;

        assert_eq!(unfiltered_status, StatusCode::OK);
        assert_eq!(filtered_status, StatusCode::OK);
        assert!(unfiltered["aggregate"]["duplication_pct"]
            .as_f64()
            .is_some_and(|pct| pct > 0.0));
        assert_eq!(filtered["aggregate"]["file_count"], 1);
        assert_eq!(filtered["aggregate"]["clone_pair_count"], 0);
        assert_eq!(filtered["aggregate"]["duplication_pct"].as_f64(), Some(0.0));
        assert_eq!(
            filtered["aggregate"]["duplication_percent"].as_f64(),
            Some(0.0)
        );
    }

    #[tokio::test]
    async fn duplication_filtered_pct_uses_union_token_counts() {
        let router = router_with_overlapping_duplication_codegraph().await;

        let (status, json) =
            get_json(router, "/v1/code-intel/duplication?path=src/a.rs&limit=10").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["aggregate"]["file_count"], 1);
        assert!(json["aggregate"]["clone_pair_count"].as_u64().unwrap() >= 2);
        let filtered_pct = json["aggregate"]["duplication_pct"].as_f64().unwrap();
        assert!(
            filtered_pct > 0.9 && filtered_pct <= 1.0,
            "filtered pct out of range: {filtered_pct}"
        );
        let filtered_percent = json["aggregate"]["duplication_percent"].as_f64().unwrap();
        assert!(
            (filtered_percent - filtered_pct * 100.0).abs() < 1e-9,
            "percent must mirror pct: {filtered_percent} vs {filtered_pct}"
        );
    }

    #[test]
    fn code_intel_graph_selection_uses_node_ids_for_duplicate_names() {
        let nodes = vec![
            (1, "render".to_string(), "src/a.rs".to_string()),
            (2, "render".to_string(), "src/b.rs".to_string()),
            (3, "helper".to_string(), "src/c.rs".to_string()),
        ];
        let centrality = refact_codegraph::analytics::NodeCentrality {
            top_pagerank: vec![(2, 10.0), (3, 9.0), (1, 1.0)],
            top_betweenness: Vec::new(),
        };

        let selected = select_graph_node_ids(&nodes, &centrality, 1);

        assert_eq!(selected, BTreeSet::from([2]));
    }

    #[tokio::test]
    async fn code_intel_pr_blast_resolves_noncanonical_path() {
        let (router, caller_path) = router_with_absolute_codegraph().await;

        let (status, json) = post_json(
            router,
            "/v1/code-intel/pr-blast",
            serde_json::json!({"changed_files": [" .\\src\\core.rs "], "max_depth": 2}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["impacted_file_count"], 1);
        assert!(json["directly_impacted"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |impact| impact["path"].as_str() == Some(caller_path.as_str())
                    && impact["symbol"].as_str() == Some("caller")
            ));
    }

    #[tokio::test]
    async fn code_intel_pr_blast_returns_reviewers_kind_and_ready_state() {
        let (router, _dir) = router_with_pr_blast_codegraph().await;

        let (status, json) = post_json(
            router,
            "/v1/code-intel/pr-blast",
            serde_json::json!({"changed_files": ["src/core.rs"], "max_depth": 2}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["partial"], false);
        assert_eq!(json["index_state"]["queued"], 0);
        assert_eq!(json["index_state"]["cross_file_ready"], true);
        assert!(json["index_state"]["cross_file_edges"].as_i64().unwrap() > 0);
        assert_eq!(
            json["suggested_reviewers"][0]["author"],
            "aaa-owner@example.com"
        );
        assert!(json["directly_impacted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|impact| impact["path"].as_str() == Some("src/caller.rs")
                && impact["symbol"].as_str() == Some("caller")
                && impact["via"].as_str() == Some("calls")
                && impact["kind"].as_str() == Some("behavioral")));
    }

    #[tokio::test]
    async fn code_intel_pr_blast_returns_structural_inherits_kind() {
        let (router, _dir) = router_with_pr_blast_codegraph().await;

        let (status, json) = post_json(
            router,
            "/v1/code-intel/pr-blast",
            serde_json::json!({"changed_files": ["src/base.py"], "max_depth": 2}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["directly_impacted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|impact| impact["path"].as_str() == Some("src/derived.py")
                && impact["symbol"].as_str() == Some("Derived")
                && impact["via"].as_str() == Some("inherits")
                && impact["kind"].as_str() == Some("structural")));
    }

    #[tokio::test]
    async fn code_intel_pr_blast_reports_partial_index_state() {
        let router = router_with_partial_pr_blast_codegraph().await;

        let (status, json) = post_json(
            router,
            "/v1/code-intel/pr-blast",
            serde_json::json!({"changed_files": ["src/core.rs"], "max_depth": 2}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["partial"], true);
        assert_eq!(json["index_state"]["queued"], 1);
        assert_eq!(json["index_state"]["cross_file_ready"], false);
        assert!(json["warning"]
            .as_str()
            .unwrap()
            .starts_with("⚠ index still building (1 files queued"));
    }

    fn commit_files_together(repo: &Repository, files: &[(&str, &str)], msg: &str) {
        let workdir = repo.workdir().unwrap();
        let mut index = repo.index().unwrap();
        for (path, contents) in files {
            let full_path = workdir.join(path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full_path, contents).unwrap();
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let time = Time::new(1_700_000_100, 0);
        let sig = Signature::new("Tester", "tester@example.com", &time).unwrap();
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .unwrap();
    }

    fn co_change_clone_source(name: &str, marker: &str) -> String {
        let mut src =
            format!("// {marker}\nfn {name}(input: i32) -> i32 {{\n    let mut total = input;\n");
        for _ in 0..30 {
            src.push_str("    total = total + input;\n    total = total - input;\n");
        }
        src.push_str("    total\n}\n");
        src
    }

    async fn router_with_co_change_duplication() -> (axum::Router, tempfile::TempDir) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let dir = tempfile::tempdir().unwrap();
        // Canonical root: on Windows runners TMP is an 8.3 short path (RUNNER~1);
        // production canonicalizes the project root, so the fixture must too or
        // the repo-relative co-change join misses.
        let root =
            crate::files_correction::canonical_path(dir.path().to_string_lossy().to_string());
        let repo = Repository::init(&root).unwrap();
        let a_v1 = co_change_clone_source("alpha", "v1");
        let b_v1 = co_change_clone_source("beta", "v1");
        commit_files_together(
            &repo,
            &[("src/a.rs", &a_v1), ("src/b.rs", &b_v1)],
            "first co-change",
        );
        let a_v2 = co_change_clone_source("alpha", "v2");
        let b_v2 = co_change_clone_source("beta", "v2");
        commit_files_together(
            &repo,
            &[("src/a.rs", &a_v2), ("src/b.rs", &b_v2)],
            "second co-change",
        );
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![root.clone()];

        let a_abs = root.join("src").join("a.rs").to_string_lossy().to_string();
        let b_abs = root.join("src").join("b.rs").to_string_lossy().to_string();
        let codegraph = Arc::new(refact_codegraph::CodeGraphService::open_in_memory().unwrap());
        codegraph.index_file(&a_abs, &a_v2, "rust").await.unwrap();
        codegraph.index_file(&b_abs, &b_v2, "rust").await.unwrap();
        codegraph.connect_usages().await.unwrap();
        *gcx.codegraph.lock().await = Some(codegraph);
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        (make_refact_http_server(app), dir)
    }

    #[tokio::test]
    async fn code_intel_duplication_co_change_joins_absolute_paths() {
        let (router, _dir) = router_with_co_change_duplication().await;

        let (status, json) = get_json(router, "/v1/code-intel/duplication?limit=5").await;

        assert_eq!(status, StatusCode::OK);
        let clones = json["clones"].as_array().unwrap();
        assert!(
            !clones.is_empty(),
            "expected a cross-file clone between absolute-path files: {json}"
        );
        assert_eq!(
            clones[0]["co_change"].as_u64(),
            Some(2),
            "co-change join must translate absolute store paths to repo-relative git keys: {json}"
        );
    }
}
