pub use refact_http::routers::v1::git_read::*;

#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use git2::Repository;
#[cfg(test)]
use hyper::StatusCode;
#[cfg(test)]
use serde_json::{Value, json};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn init_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, repo)
    }

    fn commit_file(repo: &Repository, root: &Path, message: &str) {
        fs::write(root.join("file.txt"), "base\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &[])
            .unwrap();
    }

    async fn get_json(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[test]
    fn status_shapes_staged_unstaged_and_untracked_files() {
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "Initial commit");
        fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("new.txt")).unwrap();
        index.write().unwrap();
        fs::write(dir.path().join("untracked.txt"), "untracked\n").unwrap();

        let status = status_for_repo(dir.path()).unwrap();

        assert!(!status.head_detached);
        assert!(status.branch.is_some());
        assert!(status
            .staged
            .iter()
            .any(|change| { change.relative_path == "new.txt" && change.status == "ADDED" }));
        assert!(status
            .unstaged
            .iter()
            .any(|change| { change.relative_path == "file.txt" && change.status == "MODIFIED" }));
        assert!(status
            .unstaged
            .iter()
            .any(|change| { change.relative_path == "untracked.txt" && change.status == "ADDED" }));
    }

    #[test]
    fn diff_shapes_staged_and_unstaged_path_filtered_patches() {
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "Initial commit");
        fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        fs::write(dir.path().join("file.txt"), "unstaged\n").unwrap();

        let staged = diff_for_repo(dir.path(), Some("file.txt"), true).unwrap();
        let unstaged = diff_for_repo(dir.path(), Some("file.txt"), false).unwrap();

        assert!(staged.patch.contains("+staged"));
        assert!(!staged.patch.contains("+unstaged"));
        assert!(unstaged.patch.contains("+unstaged"));
        assert!(!staged.truncated);
        assert!(!unstaged.truncated);
    }

    #[test]
    fn diff_for_repo_preserves_content_prefixes_and_cleans_metadata_origins() {
        let (dir, repo) = init_repo();
        let base = (1..=24)
            .map(|line| format!("line {line:02}\n"))
            .collect::<String>();
        fs::write(dir.path().join("file.txt"), &base).unwrap();
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

        let modified = base
            .replace("line 02\n", "changed 02\n")
            .replace("line 22\n", "changed 22\n");
        fs::write(dir.path().join("file.txt"), modified).unwrap();
        fs::write(
            dir.path().join("image.bin"),
            b"\x89PNG\r\n\x1a\n\0binary payload",
        )
        .unwrap();

        let patch = diff_for_repo(dir.path(), None, false).unwrap().patch;
        let lines = patch.lines().collect::<Vec<_>>();

        assert!(lines.iter().any(|line| *line == "-line 02"));
        assert!(lines.iter().any(|line| *line == "+changed 02"));
        assert!(lines.iter().any(|line| *line == " line 03"));
        assert!(lines.iter().filter(|line| line.starts_with("@@ ")).count() >= 2);
        assert!(lines
            .iter()
            .any(|line| *line == "diff --git a/image.bin b/image.bin"));
        assert!(lines
            .iter()
            .any(|line| *line == "Binary files /dev/null and b/image.bin differ"));
        assert!(!patch.contains("Fdiff"));
        assert!(!patch.contains("BBinary"));
        assert!(!patch.contains("H@@"));
    }

    #[test]
    fn log_shapes_commit_identity_author_and_message() {
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "Subject line\n\nMessage body");

        let log = log_for_repo(dir.path(), 50, 0).unwrap();

        assert_eq!(log.commits.len(), 1);
        assert_eq!(log.commits[0].short_oid.len(), 12);
        assert_eq!(log.commits[0].author_name, "Test User");
        assert_eq!(log.commits[0].author_email, "test@example.com");
        assert_eq!(log.commits[0].message_first_line, "Subject line");
        assert_eq!(log.commits[0].message, "Subject line\n\nMessage body");
    }

    #[test]
    fn repo_relative_paths_rejects_absolute_and_parent_paths() {
        assert!(repo_relative_paths(vec!["/tmp/file".to_string()]).is_err());
        assert!(repo_relative_paths(vec!["../file".to_string()]).is_err());
        assert_eq!(
            repo_relative_paths(vec!["src/lib.rs".to_string()]).unwrap(),
            vec![PathBuf::from("src/lib.rs")]
        );
    }

    #[test]
    fn resolve_enclosing_root_matches_exact_subdir_and_rejects_outside() {
        let roots = vec![PathBuf::from("/repo"), PathBuf::from("/repo/nested")];
        assert_eq!(
            resolve_enclosing_root(&roots, Path::new("/repo")),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(
            resolve_enclosing_root(&roots, Path::new("/repo/refact-agent/engine")),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(
            resolve_enclosing_root(&roots, Path::new("/repo/nested/src")),
            Some(PathBuf::from("/repo/nested"))
        );
        assert_eq!(resolve_enclosing_root(&roots, Path::new("/outside")), None);
        assert_eq!(
            resolve_enclosing_root(&roots, Path::new("/repo-sibling")),
            None
        );
    }

    #[test]
    fn effective_git_roots_from_discovers_enclosing_repo_for_subdir_workspace() {
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "Initial commit");
        let subdir = dir.path().join("refact-agent").join("engine");
        fs::create_dir_all(&subdir).unwrap();
        let canonical_repo = dunce::canonicalize(dir.path()).unwrap();

        let roots = effective_git_roots_from(Vec::new(), &[subdir]);

        assert_eq!(roots, vec![canonical_repo]);
    }

    #[test]
    fn effective_git_roots_from_keeps_single_root_when_workspace_is_repo_root() {
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "Initial commit");
        let canonical_repo = dunce::canonicalize(dir.path()).unwrap();

        let roots =
            effective_git_roots_from(vec![canonical_repo.clone()], &[dir.path().to_path_buf()]);

        assert_eq!(roots, vec![canonical_repo]);
    }

    #[test]
    fn effective_git_roots_from_skips_non_repo_and_bare_repo_folders() {
        let plain = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        Repository::init_bare(bare.path()).unwrap();

        let roots = effective_git_roots_from(
            Vec::new(),
            &[plain.path().to_path_buf(), bare.path().to_path_buf()],
        );

        assert_eq!(roots, Vec::<PathBuf>::new());
    }

    #[test]
    fn effective_git_roots_from_dedupes_covered_and_repeated_folders() {
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "Initial commit");
        let sub_a = dir.path().join("crates").join("a");
        let sub_b = dir.path().join("crates").join("b");
        fs::create_dir_all(&sub_a).unwrap();
        fs::create_dir_all(&sub_b).unwrap();
        let canonical_repo = dunce::canonicalize(dir.path()).unwrap();

        let discovered = effective_git_roots_from(Vec::new(), &[sub_a.clone(), sub_b.clone()]);
        assert_eq!(discovered, vec![canonical_repo.clone()]);

        let with_known_root = effective_git_roots_from(
            vec![canonical_repo.clone()],
            &[dir.path().to_path_buf(), sub_a, sub_b],
        );
        assert_eq!(with_known_root, vec![canonical_repo]);
    }

    #[tokio::test]
    async fn selected_roots_discovers_enclosing_repo_for_subdir_workspace_folder() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "Initial commit");
        let subdir = dir.path().join("refact-agent").join("engine");
        fs::create_dir_all(&subdir).unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![subdir.clone()];
        let canonical_repo = dunce::canonicalize(dir.path()).unwrap();

        let all_roots = selected_roots(gcx.clone(), None).await.unwrap();
        assert_eq!(all_roots, vec![canonical_repo.clone()]);

        let status = status_for_repo(&all_roots[0]).unwrap();
        assert!(status.branch.is_some());
        let log = log_for_repo(&all_roots[0], 10, 0).unwrap();
        assert_eq!(log.commits.len(), 1);
        let branches = branches_for_repo(&all_roots[0]).unwrap();
        assert!(branches.current.is_some());

        let mapped = selected_roots(gcx.clone(), Some(subdir.to_string_lossy().to_string()))
            .await
            .unwrap();
        assert_eq!(mapped, vec![canonical_repo]);

        let outside = tempfile::tempdir().unwrap();
        let error = selected_roots(gcx, Some(outside.path().to_string_lossy().to_string()))
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn selected_roots_filters_by_canonical_active_root() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let active = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() =
            vec![active.path().to_path_buf()];

        let selected = selected_roots(
            gcx.clone(),
            Some(active.path().to_string_lossy().to_string()),
        )
        .await
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0], dunce::canonicalize(active.path()).unwrap());

        let error = selected_roots(gcx, Some(other.path().to_string_lossy().to_string()))
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn selected_roots_maps_subdir_and_symlink_to_enclosing_root() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let active = tempfile::tempdir().unwrap();
        let subdir = active.path().join("refact-agent").join("engine");
        fs::create_dir_all(&subdir).unwrap();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() =
            vec![active.path().to_path_buf()];
        let canonical_root = dunce::canonicalize(active.path()).unwrap();

        let selected = selected_roots(gcx.clone(), Some(subdir.to_string_lossy().to_string()))
            .await
            .unwrap();
        assert_eq!(selected, vec![canonical_root.clone()]);

        #[cfg(unix)]
        {
            let link_holder = tempfile::tempdir().unwrap();
            let link = link_holder.path().join("engine-link");
            std::os::unix::fs::symlink(&subdir, &link).unwrap();
            let selected = selected_roots(gcx.clone(), Some(link.to_string_lossy().to_string()))
                .await
                .unwrap();
            assert_eq!(selected, vec![canonical_root.clone()]);
        }

        let outside = tempfile::tempdir().unwrap();
        let error = selected_roots(gcx, Some(outside.path().to_string_lossy().to_string()))
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn git_reads_skip_missing_roots_and_serve_valid_repo() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let (dir, repo) = init_repo();
        commit_file(&repo, dir.path(), "Initial commit");
        let missing = dir.path().join("missing-root");
        *gcx.documents_state.workspace_folders.lock().unwrap() = Vec::new();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() =
            vec![missing, dir.path().to_path_buf()];
        let canonical_root = dunce::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .to_string();
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let router = crate::http::routers::make_refact_http_server(app);

        let (status, payload) = get_json(&router, "/v1/git/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["roots"].as_array().unwrap().len(), 1);
        assert_eq!(payload["roots"][0]["root"], canonical_root);
        assert!(payload["roots"][0]["branch"].is_string());

        let (status, payload) = get_json(&router, "/v1/git/branches").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["roots"].as_array().unwrap().len(), 1);
        assert_eq!(payload["roots"][0]["root"], canonical_root);
        assert!(payload["roots"][0]["current"].is_string());

        let (status, payload) = get_json(&router, "/v1/git/log").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["roots"].as_array().unwrap().len(), 1);
        assert_eq!(payload["roots"][0]["root"], canonical_root);
        assert_eq!(payload["roots"][0]["commits"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn git_read_explicit_missing_root_returns_not_found() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let holder = tempfile::tempdir().unwrap();
        let missing = holder.path().join("missing-root");
        *gcx.documents_state.workspace_folders.lock().unwrap() = Vec::new();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![missing.clone()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let router = crate::http::routers::make_refact_http_server(app);
        let uri = format!("/v1/git/status?root={}", missing.to_string_lossy());

        let (status, payload) = get_json(&router, &uri).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(payload["code"], "not_found");
        assert_eq!(payload["error"], "Git root not found");
    }

    #[tokio::test]
    async fn git_reads_return_empty_roots_when_all_configured_roots_are_missing() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let holder = tempfile::tempdir().unwrap();
        let missing = holder.path().join("missing-root");
        *gcx.documents_state.workspace_folders.lock().unwrap() = Vec::new();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() = vec![missing];
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let router = crate::http::routers::make_refact_http_server(app);

        for uri in ["/v1/git/status", "/v1/git/branches", "/v1/git/log"] {
            let (status, payload) = get_json(&router, uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(payload, json!({ "roots": [] }), "{uri}");
        }
    }

    #[tokio::test]
    async fn auth_matrix_git_stage_rejects_non_relative_paths() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let active = tempfile::tempdir().unwrap();
        Repository::init(active.path()).unwrap();
        *gcx.documents_state.workspace_vcs_roots.lock().unwrap() =
            vec![active.path().to_path_buf()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let router = crate::http::routers::make_refact_http_server(app);

        for bad_path in ["../evil", "/etc/passwd"] {
            let body = json!({
                "root": active.path().to_string_lossy(),
                "paths": [bad_path],
            })
            .to_string();
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/git/stage")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
            let payload: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad_path}");
            assert_eq!(payload["code"], "bad_request", "{bad_path}");
        }
    }
}
