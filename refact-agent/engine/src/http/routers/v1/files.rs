pub use refact_http::routers::v1::files::*;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use hyper::StatusCode;
    use serde_json::Value;
    use tower::ServiceExt;
    use refact_privacy::{PrivacyPolicy, ShellBehavior, SubagentPolicy, Zone};

    use super::{clamped_tree_limit, MAX_CONTENT_BYTES, MAX_ENTRIES};
    use crate::app_state::AppState;
    use crate::global_context::{GlobalContext, SharedGlobalContext};
    use crate::privacy::{FilePrivacySettings, PrivacySettings};

    async fn test_router(workspace_roots: &[&Path]) -> (SharedGlobalContext, Router) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = workspace_roots
            .iter()
            .map(|path| path.to_path_buf())
            .collect();
        set_privacy(gcx.clone(), Vec::new());
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        (gcx, crate::http::routers::make_refact_http_server(app))
    }

    fn set_privacy(gcx: Arc<GlobalContext>, blocked: Vec<String>) {
        *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: Vec::new(),
                blocked,
            },
            loaded_ts: u64::MAX / 2,
        });
    }

    fn set_privacy_policy(gcx: &Arc<GlobalContext>, policy: PrivacyPolicy) {
        *gcx.privacy_policy_load.write().unwrap() = refact_privacy::PolicyLoad {
            policy: Arc::new(policy),
            error: None,
            source_paths: Vec::new(),
        };
    }

    fn query_uri(route: &str, pairs: &[(&str, String)]) -> String {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in pairs {
            serializer.append_pair(key, value);
        }
        format!("{route}?{}", serializer.finish())
    }

    async fn get_json(router: Router, uri: String) -> (StatusCode, Value) {
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
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn empty_tree_path_returns_workspace_roots() {
        let temp = tempfile::tempdir().unwrap();
        let alpha = temp.path().join("alpha");
        let beta = temp.path().join("Beta");
        tokio::fs::create_dir_all(&alpha).await.unwrap();
        tokio::fs::create_dir_all(&beta).await.unwrap();
        let (_gcx, router) = test_router(&[&beta, &alpha]).await;

        let (status, response) = get_json(
            router,
            query_uri("/v1/files/tree", &[("path", String::new())]),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["path"], "");
        assert_eq!(response["truncated"], false);
        assert_eq!(response["entries"][0]["name"], "alpha");
        assert_eq!(response["entries"][0]["kind"], "dir");
        assert_eq!(response["entries"][0]["size"], Value::Null);
        assert_eq!(response["entries"][0]["ignored"], false);
        assert_eq!(response["entries"][1]["name"], "Beta");
    }

    #[tokio::test]
    async fn tree_is_lazy_sorted_filtered_and_capped() {
        let workspace = tempfile::tempdir().unwrap();
        for directory in ["BDir", "adir", ".git", ".hidden", "node_modules", "target"] {
            tokio::fs::create_dir_all(workspace.path().join(directory))
                .await
                .unwrap();
        }
        tokio::fs::write(workspace.path().join("z.txt"), "zzz")
            .await
            .unwrap();
        tokio::fs::write(workspace.path().join("A.txt"), "a")
            .await
            .unwrap();
        let nested = workspace.path().join("adir").join("nested.txt");
        tokio::fs::write(&nested, "nested").await.unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/tree",
                &[
                    ("path", workspace.path().to_string_lossy().to_string()),
                    ("max_entries", "3".to_string()),
                ],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["truncated"], true);
        assert_eq!(response["entries"].as_array().unwrap().len(), 3);
        assert_eq!(response["entries"][0]["name"], "adir");
        assert_eq!(response["entries"][1]["name"], "BDir");
        assert_eq!(response["entries"][2]["name"], "A.txt");
        assert!(response.to_string().contains("nested.txt") == false);
        assert_eq!(clamped_tree_limit(Some(MAX_ENTRIES + 1)), MAX_ENTRIES);
    }

    #[tokio::test]
    async fn tree_marks_gitignored_entries() {
        let workspace = tempfile::tempdir().unwrap();
        tokio::fs::write(workspace.path().join(".gitignore"), "*.log\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.path().join("debug.log"), "noise")
            .await
            .unwrap();
        tokio::fs::write(workspace.path().join("main.rs"), "fn main() {}")
            .await
            .unwrap();
        git2::Repository::init(workspace.path()).unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/tree",
                &[("path", workspace.path().to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let entries = response["entries"].as_array().unwrap();
        let ignored = entries
            .iter()
            .find(|entry| entry["name"] == "debug.log")
            .unwrap();
        let visible = entries
            .iter()
            .find(|entry| entry["name"] == "main.rs")
            .unwrap();
        assert_eq!(ignored["ignored"], true);
        assert_eq!(visible["ignored"], false);
    }

    #[tokio::test]
    async fn tree_resolves_dotfile_privacy_zones_with_engine_matching() {
        let workspace = tempfile::tempdir().unwrap();
        let secret = workspace.path().join(".env");
        tokio::fs::write(&secret, "TOKEN=secret").await.unwrap();
        let (gcx, router) = test_router(&[workspace.path()]).await;
        *gcx.privacy_settings.write().unwrap() = Arc::new(PrivacySettings {
            privacy_rules: FilePrivacySettings {
                only_send_to_servers_I_control: Vec::new(),
                blocked: Vec::new(),
            },
            loaded_ts: u64::MAX / 2,
        });
        set_privacy_policy(
            &gcx,
            PrivacyPolicy {
                blocked: Vec::new(),
                zones: vec![
                    Zone {
                        name: "secrets".to_string(),
                        patterns: vec![".env*".to_string()],
                        send_to: Vec::new(),
                        on_shell_read: ShellBehavior::Withhold,
                    },
                    Zone {
                        name: "normal".to_string(),
                        patterns: vec!["**".to_string()],
                        send_to: vec!["*".to_string()],
                        on_shell_read: ShellBehavior::Withhold,
                    },
                ],
                subagents: SubagentPolicy::default(),
                ..Default::default()
            },
        );

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/tree",
                &[("path", workspace.path().to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let secret = response["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == ".env")
            .unwrap();
        assert_eq!(secret["privacy_zone"]["name"], "secrets");
        assert_eq!(secret["privacy_zone"]["send_to"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn tree_rejects_path_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, _) = get_json(
            router,
            query_uri(
                "/v1/files/tree",
                &[("path", outside.path().to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    async fn worktree_router(
        source: &Path,
        worktree_id: &str,
    ) -> (PathBuf, SharedGlobalContext, Router) {
        let cache_dir =
            std::env::temp_dir().join(format!("refact-files-wt-{}", uuid::Uuid::new_v4()));
        let config_dir =
            std::env::temp_dir().join(format!("refact-files-cfg-{}", uuid::Uuid::new_v4()));
        let hash = refact_worktrees::service::project_hash_for_path(source);
        let registry_dir = cache_dir.join("worktrees").join(&hash);
        let worktree_root = registry_dir.join(worktree_id);
        std::fs::create_dir_all(&worktree_root).unwrap();
        let registry = refact_worktrees::types::WorktreeRegistry {
            schema_version: 1,
            source_workspace_root: source.to_path_buf(),
            project_hash: hash,
            records: vec![refact_worktrees::types::WorktreeRegistryRecord {
                meta: refact_worktrees::types::WorktreeMeta {
                    id: worktree_id.to_string(),
                    kind: "chat".to_string(),
                    root: worktree_root.clone(),
                    source_workspace_root: source.to_path_buf(),
                    repo_root: source.to_path_buf(),
                    branch: Some("refact/chat/test".to_string()),
                    base_branch: Some("main".to_string()),
                    base_commit: None,
                    task_id: None,
                    card_id: None,
                    agent_id: None,
                    enforce: true,
                },
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                last_seen_at: None,
                references: Vec::new(),
                last_known_status: None,
            }],
        };
        std::fs::write(
            registry_dir.join("index.json"),
            serde_json::to_string_pretty(&registry).unwrap(),
        )
        .unwrap();

        let gcx =
            crate::global_context::tests::make_test_gcx_with_dirs(cache_dir, config_dir).await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.to_path_buf()];
        set_privacy(gcx.clone(), Vec::new());
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        (
            worktree_root,
            gcx,
            crate::http::routers::make_refact_http_server(app),
        )
    }

    async fn post_json(router: Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn write_updates_file_and_reports_new_mtime() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("main.rs");
        tokio::fs::write(&path, "fn main() {}\n").await.unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, response) = post_json(
            router,
            "/v1/files/write",
            serde_json::json!({
                "path": path.to_string_lossy(),
                "content": "fn main() { println!(\"hi\"); }\n",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(response["mtime_ms"].as_u64().is_some());
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "fn main() { println!(\"hi\"); }\n"
        );
    }

    #[tokio::test]
    async fn write_rejects_stale_expected_mtime() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("main.rs");
        tokio::fs::write(&path, "original\n").await.unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, _) = post_json(
            router,
            "/v1/files/write",
            serde_json::json!({
                "path": path.to_string_lossy(),
                "content": "clobbered\n",
                "expected_mtime_ms": 1,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "original\n"
        );
    }

    #[tokio::test]
    async fn write_rejects_privacy_blocked_path() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("blocked.secret");
        tokio::fs::write(&path, "nope\n").await.unwrap();
        let (gcx, router) = test_router(&[workspace.path()]).await;
        set_privacy(gcx, vec!["*.secret".to_string()]);

        let (status, _) = post_json(
            router,
            "/v1/files/write",
            serde_json::json!({
                "path": path.to_string_lossy(),
                "content": "leaked\n",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "nope\n");
    }

    #[tokio::test]
    async fn write_rejects_path_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("main.rs");
        tokio::fs::write(&path, "original\n").await.unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, _) = post_json(
            router,
            "/v1/files/write",
            serde_json::json!({
                "path": path.to_string_lossy(),
                "content": "clobbered\n",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "original\n"
        );
    }

    #[tokio::test]
    async fn write_allows_registered_worktree_path() {
        let source = tempfile::tempdir().unwrap();
        let (worktree_root, _gcx, router) = worktree_router(source.path(), "wt-write").await;
        let path = worktree_root.join("main.rs");
        tokio::fs::write(&path, "old\n").await.unwrap();

        let (status, response) = post_json(
            router,
            "/v1/files/write",
            serde_json::json!({
                "path": path.to_string_lossy(),
                "content": "new\n",
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "new\n");
    }

    #[tokio::test]
    async fn read_allows_registered_worktree_path() {
        let source = tempfile::tempdir().unwrap();
        let (worktree_root, _gcx, router) = worktree_router(source.path(), "wt-read").await;
        let path = worktree_root.join("main.rs");
        tokio::fs::write(&path, "fn main() {}\n").await.unwrap();

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", path.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(response["content"], "fn main() {}\n");
    }

    #[tokio::test]
    async fn tree_allows_registered_worktree_path() {
        let source = tempfile::tempdir().unwrap();
        let (worktree_root, _gcx, router) = worktree_router(source.path(), "wt-tree").await;
        tokio::fs::write(worktree_root.join("main.rs"), "fn main() {}\n")
            .await
            .unwrap();

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/tree",
                &[("path", worktree_root.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert!(response["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "main.rs"));
    }

    #[tokio::test]
    async fn read_rejects_unregistered_cache_worktree_path() {
        let source = tempfile::tempdir().unwrap();
        let (worktree_root, gcx, router) = worktree_router(source.path(), "wt-guard").await;
        let sibling = worktree_root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("other-project")
            .join("wt-unregistered");
        std::fs::create_dir_all(&sibling).unwrap();
        let path = sibling.join("secret.rs");
        tokio::fs::write(&path, "nope\n").await.unwrap();
        assert!(path.starts_with(gcx.cache_dir.join("worktrees")));

        let (status, _) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", path.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn read_rejects_worktree_root_escaping_the_cache_dir() {
        let source = tempfile::tempdir().unwrap();
        let escape_target = tempfile::tempdir().unwrap();
        let path = escape_target.path().join("secret.rs");
        tokio::fs::write(&path, "secret\n").await.unwrap();

        let cache_dir =
            std::env::temp_dir().join(format!("refact-files-esc-{}", uuid::Uuid::new_v4()));
        let config_dir =
            std::env::temp_dir().join(format!("refact-files-esc-cfg-{}", uuid::Uuid::new_v4()));
        let hash = refact_worktrees::service::project_hash_for_path(source.path());
        let registry_dir = cache_dir.join("worktrees").join(&hash);
        std::fs::create_dir_all(&registry_dir).unwrap();
        let escaping_root = escape_target.path().to_path_buf();
        let ascent = "../".repeat(registry_dir.components().count());
        let relative_id = format!(
            "{ascent}{}",
            escaping_root
                .to_string_lossy()
                .trim_start_matches(std::path::MAIN_SEPARATOR)
        );
        let registry = refact_worktrees::types::WorktreeRegistry {
            schema_version: 1,
            source_workspace_root: source.path().to_path_buf(),
            project_hash: hash,
            records: vec![refact_worktrees::types::WorktreeRegistryRecord {
                meta: refact_worktrees::types::WorktreeMeta {
                    id: relative_id,
                    kind: "chat".to_string(),
                    root: escaping_root,
                    source_workspace_root: source.path().to_path_buf(),
                    repo_root: source.path().to_path_buf(),
                    branch: None,
                    base_branch: None,
                    base_commit: None,
                    task_id: None,
                    card_id: None,
                    agent_id: None,
                    enforce: true,
                },
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                last_seen_at: None,
                references: Vec::new(),
                last_known_status: None,
            }],
        };
        std::fs::write(
            registry_dir.join("index.json"),
            serde_json::to_string_pretty(&registry).unwrap(),
        )
        .unwrap();

        let gcx =
            crate::global_context::tests::make_test_gcx_with_dirs(cache_dir, config_dir).await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![source.path().to_path_buf()];
        set_privacy(gcx.clone(), Vec::new());
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        let router = crate::http::routers::make_refact_http_server(app);

        let (status, _) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", path.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn read_rejects_privacy_blocked_path() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("blocked.secret");
        tokio::fs::write(&path, "nope").await.unwrap();
        let (gcx, router) = test_router(&[workspace.path()]).await;
        set_privacy(gcx, vec!["*.secret".to_string()]);

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", path.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(response["detail"].as_str().unwrap().contains("privacy"));
    }

    #[tokio::test]
    async fn traversal_outside_workspace_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside.txt");
        tokio::fs::create_dir_all(workspace.join("nested"))
            .await
            .unwrap();
        tokio::fs::write(&outside, "outside").await.unwrap();
        let (_gcx, router) = test_router(&[&workspace]).await;
        let traversal = workspace
            .join("nested")
            .join("..")
            .join("..")
            .join("outside.txt");

        let (status, _) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", traversal.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = workspace.path().join("escape.txt");
        symlink(outside.path(), &link).unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, _) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", link.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn big_file_is_truncated_on_utf8_boundary() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("big.txt");
        let content = format!("{}é-tail", "a".repeat(MAX_CONTENT_BYTES - 1));
        tokio::fs::write(&path, content.as_bytes()).await.unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", path.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["truncated"], true);
        assert_eq!(
            response["content"].as_str().unwrap().len(),
            MAX_CONTENT_BYTES - 1
        );
        assert_eq!(response["language"], "plaintext");
    }

    #[tokio::test]
    async fn binary_file_returns_metadata_without_content() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("image.bin");
        let bytes = [1_u8, 2, 0, 3, 4];
        tokio::fs::write(&path, bytes).await.unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", path.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["content"], "");
        assert_eq!(response["binary"], true);
        assert_eq!(response["size"], bytes.len());
        assert_eq!(response["truncated"], false);
        assert!(response["mtime_ms"].as_u64().is_some());
    }

    #[tokio::test]
    async fn read_slices_inclusive_line_range_before_capping() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("lines.rs");
        tokio::fs::write(&path, "one\ntwo\nthree\nfour\n")
            .await
            .unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[
                    ("path", path.to_string_lossy().to_string()),
                    ("line_start", "2".to_string()),
                    ("line_end", "3".to_string()),
                ],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["content"], "two\nthree\n");
        assert_eq!(response["line_start"], 2);
        assert_eq!(response["line_end"], 3);
        assert_eq!(response["language"], "rust");
        assert_eq!(response.get("binary"), None);
    }

    #[tokio::test]
    async fn invalid_utf8_is_binary() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("invalid.txt");
        tokio::fs::write(&path, [0xff, 0xfe, b'a']).await.unwrap();
        let (_gcx, router) = test_router(&[workspace.path()]).await;

        let (status, response) = get_json(
            router,
            query_uri(
                "/v1/files/read",
                &[("path", path.to_string_lossy().to_string())],
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["binary"], true);
        assert_eq!(response["content"], "");
    }
}
