pub use refact_http::routers::v1::git::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::path::PathBuf;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use git2::Repository;
#[cfg(test)]
#[allow(unused_imports)]
use hyper::StatusCode;
#[cfg(test)]
#[allow(unused_imports)]
use url::Url;
#[cfg(test)]
#[allow(unused_imports)]
use crate::global_context::GlobalContext;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;
    use tower::ServiceExt;

    fn init_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_at(dir.path());
        (dir, repo)
    }

    fn init_repo_at(path: &Path) -> Repository {
        let repo = Repository::init(path).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        fs::write(path.join("file.txt"), "base\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Initial commit",
            &tree,
            &[],
        )
        .unwrap();
        drop(tree);
        fs::write(path.join("file.txt"), "changed\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        drop(index);
        repo
    }

    async fn post_commit_response(gcx: Arc<GlobalContext>, commit: Value) -> (StatusCode, Value) {
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let router = crate::http::routers::make_refact_http_server(app);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/git-commit")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "commits": [commit] }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn post_commit(gcx: Arc<GlobalContext>, commit: Value) -> Value {
        let (status, response) = post_commit_response(gcx, commit).await;
        assert_eq!(status, StatusCode::OK);
        response
    }

    fn commit_body(project_field: Value, message: &str) -> Value {
        let mut body = json!({
            "commit_message": message,
            "staged_changes": [],
            "unstaged_changes": [],
        });
        body.as_object_mut()
            .unwrap()
            .extend(project_field.as_object().unwrap().clone());
        body
    }

    #[tokio::test]
    async fn git_commit_accepts_plain_workspace_root() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (dir, repo) = init_repo();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![dir.path().to_path_buf()];

        let response = post_commit(
            gcx,
            commit_body(
                json!({ "root": dir.path().to_string_lossy() }),
                "Plain root commit",
            ),
        )
        .await;

        assert_eq!(response["commits_applied"].as_array().unwrap().len(), 1);
        assert!(response["error_log"].as_array().unwrap().is_empty());
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().message(),
            Some("Plain root commit")
        );
    }

    #[tokio::test]
    async fn git_commit_maps_plain_workspace_subdir_to_enclosing_repo() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (dir, repo) = init_repo();
        let subdir = dir.path().join("refact-agent").join("engine");
        fs::create_dir_all(&subdir).unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![subdir.clone()];

        let response = post_commit(
            gcx,
            commit_body(
                json!({ "root": subdir.to_string_lossy() }),
                "Subdir root commit",
            ),
        )
        .await;

        assert_eq!(response["commits_applied"].as_array().unwrap().len(), 1);
        assert!(response["error_log"].as_array().unwrap().is_empty());
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().message(),
            Some("Subdir root commit")
        );
    }

    #[tokio::test]
    async fn git_commit_keeps_legacy_project_path_request_working() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (dir, repo) = init_repo();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![dir.path().to_path_buf()];
        let project_path = Url::from_file_path(dir.path()).unwrap();

        let response = post_commit(
            gcx,
            commit_body(
                json!({ "project_path": project_path }),
                "Legacy path commit",
            ),
        )
        .await;

        assert_eq!(response["commits_applied"].as_array().unwrap().len(), 1);
        assert!(response["error_log"].as_array().unwrap().is_empty());
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().message(),
            Some("Legacy path commit")
        );
    }

    #[tokio::test]
    async fn git_commit_maps_legacy_project_subdir_to_active_enclosing_repo() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (dir, repo) = init_repo();
        let subdir = dir.path().join("refact-agent").join("engine");
        fs::create_dir_all(&subdir).unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![subdir.clone()];

        let response = post_commit(
            gcx,
            commit_body(
                json!({ "project_path": Url::from_file_path(&subdir).unwrap() }),
                "Legacy subdir commit",
            ),
        )
        .await;

        assert_eq!(response["commits_applied"].as_array().unwrap().len(), 1);
        assert!(response["error_log"].as_array().unwrap().is_empty());
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().message(),
            Some("Legacy subdir commit")
        );
    }

    #[tokio::test]
    async fn git_commit_rejects_legacy_project_path_outside_active_roots() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (active_dir, _active_repo) = init_repo();
        let (outside_dir, outside_repo) = init_repo();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() =
            vec![active_dir.path().to_path_buf()];

        let (status, response) = post_commit_response(
            gcx,
            commit_body(
                json!({
                    "project_path": Url::from_file_path(outside_dir.path()).unwrap()
                }),
                "Outside commit",
            ),
        )
        .await;

        assert!(status.is_client_error());
        assert_eq!(
            response["detail"],
            "Git root is not an active workspace root"
        );
        assert_eq!(
            outside_repo
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .message(),
            Some("Initial commit")
        );
    }

    #[tokio::test]
    async fn git_commit_rejects_non_file_legacy_project_url() {
        let gcx = crate::global_context::tests::make_test_gcx().await;

        let (status, response) = post_commit_response(
            gcx,
            commit_body(
                json!({ "project_path": "https://example.com/repository" }),
                "Invalid URL commit",
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["detail"], "Invalid project_path file URL");
    }

    #[cfg(windows)]
    #[test]
    fn git_commit_decodes_windows_legacy_file_url_shape() {
        let project_path = Url::parse("file:///C:/Users/Test%20User/repository").unwrap();

        let decoded = decode_legacy_project_path(&project_path).unwrap();

        assert_eq!(decoded, PathBuf::from(r"C:\Users\Test User\repository"));
    }

    #[tokio::test]
    async fn git_commit_decodes_legacy_localhost_file_url() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let dir = tempfile::Builder::new()
            .prefix("legacy git workspace ")
            .tempdir()
            .unwrap();
        let repo = init_repo_at(dir.path());
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![dir.path().to_path_buf()];
        let mut project_path = Url::from_file_path(dir.path()).unwrap();
        project_path.set_host(Some("localhost")).unwrap();
        assert!(project_path.as_str().contains("%20"));

        let response = post_commit(
            gcx,
            commit_body(
                json!({ "project_path": project_path }),
                "Decoded legacy URL commit",
            ),
        )
        .await;

        assert_eq!(response["commits_applied"].as_array().unwrap().len(), 1);
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().message(),
            Some("Decoded legacy URL commit")
        );
    }
}
