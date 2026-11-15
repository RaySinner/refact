pub use refact_http::routers::v1::knowledge_ops::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::Path;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use axum::extract::State;
#[cfg(test)]
#[allow(unused_imports)]
use axum::http::{Response, StatusCode};
#[cfg(test)]
#[allow(unused_imports)]
use hyper::Body;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::custom_error::ScratchError;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::knowledge_frontmatter::KnowledgeFrontmatter;
#[cfg(test)]
#[allow(unused_imports)]
use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::global_context::GlobalContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::knowledge_graph::build_knowledge_graph;

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::body::Bytes;
    use serde_json::json;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    async fn test_gcx_with_workspace(dir: &Path) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.to_path_buf()];
        }
        gcx
    }

    fn active_frontmatter(id: &str) -> KnowledgeFrontmatter {
        KnowledgeFrontmatter {
            id: Some(id.to_string()),
            title: Some(id.to_string()),
            tags: strings(&["http-test"]),
            status: Some("active".to_string()),
            kind: Some("domain".to_string()),
            ..Default::default()
        }
    }

    async fn write_memory(path: &Path, id: &str, body: &str) {
        write_memory_frontmatter(path, active_frontmatter(id), body).await;
    }

    async fn write_memory_frontmatter(path: &Path, frontmatter: KnowledgeFrontmatter, body: &str) {
        tokio::fs::write(path, format!("{}\n\n{}", frontmatter.to_yaml(), body))
            .await
            .unwrap();
    }

    async fn read_frontmatter_body(path: &Path) -> (KnowledgeFrontmatter, String) {
        let text = tokio::fs::read_to_string(path).await.unwrap();
        let (frontmatter, content_start) = KnowledgeFrontmatter::parse(&text);
        (frontmatter, text[content_start..].trim().to_string())
    }

    fn create_file_symlink(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            return std::os::unix::fs::symlink(target, link).is_ok();
        }
        #[cfg(windows)]
        {
            return std::os::windows::fs::symlink_file(target, link).is_ok();
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    fn create_dir_symlink(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            return std::os::unix::fs::symlink(target, link).is_ok();
        }
        #[cfg(windows)]
        {
            return std::os::windows::fs::symlink_dir(target, link).is_ok();
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    async fn update_status(
        gcx: Arc<GlobalContext>,
        path: &Path,
        status: &str,
    ) -> Result<Response<Body>, ScratchError> {
        handle_v1_knowledge_update_memory(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Bytes::from(
                json!({
                    "file_path": path.to_string_lossy(),
                    "status": status,
                    "auto_link": false,
                })
                .to_string(),
            ),
        )
        .await
    }

    async fn delete_memory(
        gcx: Arc<GlobalContext>,
        path: &Path,
        archive: bool,
    ) -> Result<Response<Body>, ScratchError> {
        handle_v1_knowledge_delete_memory(
            axum::extract::State(crate::app_state::app_state_from_gcx(gcx.clone()).await),
            Bytes::from(
                json!({
                    "file_path": path.to_string_lossy(),
                    "archive": archive,
                })
                .to_string(),
            ),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auto_link_all_memories_links_similar_active_docs() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        tokio::fs::write(dir.path().join("shared.rs"), "fn shared() {}")
            .await
            .unwrap();
        let first_path = knowledge_dir.join("first.md");
        let second_path = knowledge_dir.join("second.md");
        let mut first = active_frontmatter("first");
        first.tags = strings(&["component:foo", "workflow:bar"]);
        first.filenames = strings(&["shared.rs"]);
        let mut second = active_frontmatter("second");
        second.tags = strings(&["component:foo", "workflow:bar"]);
        second.filenames = strings(&["shared.rs"]);
        write_memory_frontmatter(&first_path, first, "First body").await;
        write_memory_frontmatter(&second_path, second, "Second body").await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let stats = auto_link_all_memories(gcx).await.unwrap();

        let (first, _) = read_frontmatter_body(&first_path).await;
        let (second, _) = read_frontmatter_body(&second_path).await;
        assert!(first.links.contains(&"second".to_string()));
        assert!(second.links.contains(&"first".to_string()));
        assert!(stats.docs_updated >= 1);
        assert!(stats.links_added >= 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auto_link_all_memories_skips_trajectory_targets() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let active_path = knowledge_dir.join("active.md");
        let trajectory_path = knowledge_dir.join("trajectory.md");
        let mut active = active_frontmatter("active");
        active.tags = strings(&["component:foo", "workflow:bar"]);
        let mut trajectory = active_frontmatter("trajectory");
        trajectory.kind = Some("trajectory".to_string());
        trajectory.tags = strings(&["component:foo", "workflow:bar"]);
        write_memory_frontmatter(&active_path, active, "Active body").await;
        write_memory_frontmatter(&trajectory_path, trajectory, "Trajectory body").await;
        let before = tokio::fs::read_to_string(&trajectory_path).await.unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let stats = auto_link_all_memories(gcx).await.unwrap();

        let (active, _) = read_frontmatter_body(&active_path).await;
        let after = tokio::fs::read_to_string(&trajectory_path).await.unwrap();
        assert!(!active.links.contains(&"trajectory".to_string()));
        assert_eq!(before, after);
        assert_eq!(stats.docs_updated, 0);
        assert_eq!(stats.links_added, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auto_link_all_memories_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let first_path = knowledge_dir.join("first.md");
        let second_path = knowledge_dir.join("second.md");
        let mut first = active_frontmatter("first");
        first.tags = strings(&["component:foo", "workflow:bar"]);
        let mut second = active_frontmatter("second");
        second.tags = strings(&["component:foo", "workflow:bar"]);
        write_memory_frontmatter(&first_path, first, "First body").await;
        write_memory_frontmatter(&second_path, second, "Second body").await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let first_stats = auto_link_all_memories(gcx.clone()).await.unwrap();
        let second_stats = auto_link_all_memories(gcx).await.unwrap();

        assert!(first_stats.links_added >= 1);
        assert_eq!(second_stats.docs_updated, 0);
        assert_eq!(second_stats.links_added, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auto_link_all_memories_preserves_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let first_path = knowledge_dir.join("first.md");
        let second_path = knowledge_dir.join("second.md");
        let missing = "missing/not-in-workspace.rs";
        let mut first = active_frontmatter("first");
        first.tags = strings(&["component:foo", "workflow:bar"]);
        first.filenames = strings(&[missing]);
        let mut second = active_frontmatter("second");
        second.tags = strings(&["component:foo", "workflow:bar"]);
        write_memory_frontmatter(&first_path, first, "First body").await;
        write_memory_frontmatter(&second_path, second, "Second body").await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let stats = auto_link_all_memories(gcx).await.unwrap();

        let (first, _) = read_frontmatter_body(&first_path).await;
        assert!(stats.links_added >= 1);
        assert!(first.links.contains(&"second".to_string()));
        assert_eq!(first.filenames, strings(&[missing]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_status_accepts_aliases_with_canonical_frontmatter() {
        let cases = [
            ("needs-review", "proposed"),
            ("stale", "deprecated"),
            ("obsolete", "deprecated"),
            ("inactive", "archived"),
            ("archive", "archived"),
        ];

        for (alias, expected) in cases {
            let dir = tempfile::tempdir().unwrap();
            let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
            tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
            let path = knowledge_dir.join(format!("{}.md", alias.replace('-', "_")));
            let body = format!("Body for {alias}");
            write_memory(&path, alias, &body).await;
            let gcx = test_gcx_with_workspace(dir.path()).await;

            let response = update_status(gcx, &path, alias).await.unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let (frontmatter, updated_body) = read_frontmatter_body(&path).await;
            assert_eq!(frontmatter.status.as_deref(), Some(expected));
            assert_eq!(updated_body, body);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_status_rejects_invalid_status() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("invalid-status.md");
        write_memory(&path, "invalid-status", "Invalid status body").await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let err = update_status(gcx.clone(), &path, "unknown-status")
            .await
            .unwrap_err();

        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("Invalid status"));
        let empty_err = update_status(gcx, &path, "").await.unwrap_err();
        assert_eq!(empty_err.status_code, StatusCode::BAD_REQUEST);
        let (frontmatter, body) = read_frontmatter_body(&path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("active"));
        assert_eq!(body, "Invalid status body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_rejects_symlinked_knowledge_root() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let real_knowledge = dir.path().join("real-knowledge");
        tokio::fs::create_dir_all(workspace.join(".refact"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(&real_knowledge).await.unwrap();
        let real_path = real_knowledge.join("memory.md");
        write_memory(&real_path, "memory", "Body").await;
        if !create_dir_symlink(&real_knowledge, &workspace.join(KNOWLEDGE_FOLDER_NAME)) {
            return;
        }
        let gcx = test_gcx_with_workspace(&workspace).await;
        let symlink_path = workspace.join(KNOWLEDGE_FOLDER_NAME).join("memory.md");

        let err = update_status(gcx, &symlink_path, "pinned")
            .await
            .unwrap_err();

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert!(err.message.contains("symlink"));
        let (frontmatter, body) = read_frontmatter_body(&real_path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("active"));
        assert_eq!(body, "Body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn archive_delete_rejects_symlinked_knowledge_root() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let real_knowledge = dir.path().join("real-knowledge");
        tokio::fs::create_dir_all(workspace.join(".refact"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(&real_knowledge).await.unwrap();
        let real_path = real_knowledge.join("memory.md");
        write_memory(&real_path, "memory", "Body").await;
        if !create_dir_symlink(&real_knowledge, &workspace.join(KNOWLEDGE_FOLDER_NAME)) {
            return;
        }
        let gcx = test_gcx_with_workspace(&workspace).await;
        let symlink_path = workspace.join(KNOWLEDGE_FOLDER_NAME).join("memory.md");

        let err = delete_memory(gcx, &symlink_path, true).await.unwrap_err();

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert!(err.message.contains("symlink"));
        let (frontmatter, body) = read_frontmatter_body(&real_path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("active"));
        assert_eq!(body, "Body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_rejects_symlink_file_inside_knowledge_root() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let outside_path = dir.path().join("outside.md");
        write_memory(&outside_path, "outside", "Outside body").await;
        let link = knowledge_dir.join("link.md");
        if !create_file_symlink(&outside_path, &link) {
            return;
        }
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let err = update_status(gcx, &link, "pinned").await.unwrap_err();

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert!(err.message.contains("symlink"));
        let (frontmatter, body) = read_frontmatter_body(&outside_path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("active"));
        assert_eq!(body, "Outside body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hard_delete_rejects_symlink_file_inside_knowledge_root() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let outside_path = dir.path().join("outside.md");
        write_memory(&outside_path, "outside", "Outside body").await;
        let link = knowledge_dir.join("link.md");
        if !create_file_symlink(&outside_path, &link) {
            return;
        }
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let err = delete_memory(gcx, &link, false).await.unwrap_err();

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert!(err.message.contains("symlink"));
        assert!(outside_path.exists());
        assert!(link.exists());
        let (frontmatter, body) = read_frontmatter_body(&outside_path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("active"));
        assert_eq!(body, "Outside body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reactivating_memory_clears_archive_metadata() {
        let cases = ["active", "proposed", "pinned"];

        for status in cases {
            let dir = tempfile::tempdir().unwrap();
            let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
            tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
            let path = knowledge_dir.join(format!("reactivate-{status}.md"));
            let mut frontmatter = active_frontmatter(status);
            frontmatter.status = Some("deprecated".to_string());
            frontmatter.deprecated_at = Some("2026-05-01".to_string());
            frontmatter.superseded_by = Some("new-memory".to_string());
            write_memory_frontmatter(&path, frontmatter, "Reactivated body").await;
            let gcx = test_gcx_with_workspace(dir.path()).await;

            let response = update_status(gcx, &path, status).await.unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let (frontmatter, body) = read_frontmatter_body(&path).await;
            assert_eq!(frontmatter.status.as_deref(), Some(status));
            assert_eq!(frontmatter.deprecated_at, None);
            assert_eq!(frontmatter.superseded_by, None);
            assert_eq!(body, "Reactivated body");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_pinned_preserves_file_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("pinned.md");
        let body = "# Pinned\n\nOriginal body";
        write_memory(&path, "pinned", body).await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let response = update_status(gcx, &path, "pinned").await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(path.exists());
        let (frontmatter, updated_body) = read_frontmatter_body(&path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("pinned"));
        assert!(frontmatter.is_active());
        assert_eq!(updated_body, body);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_proposed_preserves_file_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("proposed.md");
        let body = "# Proposed\n\nOriginal body";
        write_memory(&path, "proposed", body).await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let response = update_status(gcx, &path, "needs-review").await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(path.exists());
        let (frontmatter, updated_body) = read_frontmatter_body(&path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("proposed"));
        assert!(frontmatter.is_active());
        assert_eq!(updated_body, body);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_archived_preserves_body_and_makes_memory_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("archived.md");
        let body = "Archived body";
        write_memory(&path, "archived", body).await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let response = update_status(gcx.clone(), &path, "archived").await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(path.exists());
        let (frontmatter, updated_body) = read_frontmatter_body(&path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("archived"));
        assert!(!frontmatter.is_active());
        assert_eq!(updated_body, body);
        let kg = build_knowledge_graph(gcx.clone()).await;
        assert!(kg.active_docs().all(|doc| doc.path != path));
        let found = crate::memories::load_memories_by_tags(&gcx, &["http-test"], 10)
            .await
            .unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_deprecated_preserves_body_and_makes_memory_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("deprecated.md");
        let body = "Deprecated body";
        write_memory(&path, "deprecated", body).await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let response = update_status(gcx.clone(), &path, "stale").await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(path.exists());
        let (frontmatter, updated_body) = read_frontmatter_body(&path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("deprecated"));
        assert!(!frontmatter.is_active());
        assert_eq!(updated_body, body);
        let kg = build_knowledge_graph(gcx.clone()).await;
        assert!(kg.active_docs().all(|doc| doc.path != path));
        let found = crate::memories::load_memories_by_tags(&gcx, &["http-test"], 10)
            .await
            .unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn archive_delete_endpoint_is_non_destructive() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("archive-delete.md");
        let body = "Archive delete body";
        write_memory(&path, "archive-delete", body).await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let response = delete_memory(gcx.clone(), &path, true).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(path.exists());
        let (frontmatter, updated_body) = read_frontmatter_body(&path).await;
        assert_eq!(frontmatter.status.as_deref(), Some("archived"));
        assert!(!frontmatter.is_active());
        assert_eq!(updated_body, body);
        let kg = build_knowledge_graph(gcx.clone()).await;
        assert!(kg.active_docs().all(|doc| doc.path != path));
        let similar = kg.find_similar_docs(&["http-test".to_string()], &[], &[]);
        assert!(similar.iter().all(|(id, _)| id != "archive-delete"));
        let found = crate::memories::load_memories_by_tags(&gcx, &["http-test"], 10)
            .await
            .unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hard_delete_endpoint_removes_file_only_without_archive_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let archived_path = knowledge_dir.join("archive-delete.md");
        let hard_delete_path = knowledge_dir.join("hard-delete.md");
        write_memory(&archived_path, "archive-delete", "Archive delete body").await;
        write_memory(&hard_delete_path, "hard-delete", "Hard delete body").await;
        let gcx = test_gcx_with_workspace(dir.path()).await;

        let archive_response = delete_memory(gcx.clone(), &archived_path, true)
            .await
            .unwrap();
        let hard_delete_response = delete_memory(gcx, &hard_delete_path, false).await.unwrap();

        assert_eq!(archive_response.status(), StatusCode::OK);
        assert_eq!(hard_delete_response.status(), StatusCode::OK);
        assert!(archived_path.exists());
        assert!(!hard_delete_path.exists());
    }
}

