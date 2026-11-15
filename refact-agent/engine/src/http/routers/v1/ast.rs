pub use refact_http::routers::v1::ast::*;

#[cfg(test)]
use hyper::StatusCode;
#[cfg(test)]
use refact_runtime_api::AppState;
#[cfg(test)]
use url::Url;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::extract::State;
    use hyper::body::to_bytes;

    use super::*;

    async fn app_with_workspace_file(text: &str) -> (AppState, tempfile::TempDir, PathBuf) {
        let workspace = tempfile::tempdir().unwrap();
        let file_path = workspace.path().join("empty.rs");
        std::fs::write(&file_path, text).unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.privacy_settings.write().unwrap() = Arc::new(crate::privacy::PrivacySettings {
            privacy_rules: crate::privacy::FilePrivacySettings {
                only_send_to_servers_I_control: vec![],
                blocked: vec![],
            },
            loaded_ts: chrono::Utc::now().timestamp() as u64 + 60,
        });
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        *gcx.documents_state.workspace_files.lock().unwrap() = vec![file_path.clone()];

        (crate::app_state::app_state_from_gcx(gcx).await, workspace, file_path)
    }

    fn ast_file_symbols_body(file_path: &PathBuf) -> hyper::body::Bytes {
        let body = serde_json::json!({
            "file_url": Url::from_file_path(file_path).unwrap(),
        });
        hyper::body::Bytes::from(serde_json::to_vec(&body).unwrap())
    }

    #[tokio::test]
    async fn ast_file_symbols_empty_doc_defs_stays_200() {
        let (app, _workspace, file_path) = app_with_workspace_file("").await;
        let codegraph = refact_codegraph::CodeGraphService::open_in_memory().unwrap();
        *app.gcx.codegraph.lock().await = Some(Arc::new(codegraph));

        let response = handle_v1_ast_file_symbols(State(app), ast_file_symbols_body(&file_path))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body()).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn ast_file_symbols_doc_defs_error_returns_500() {
        let (app, _workspace, file_path) = app_with_workspace_file("fn present() {}\n").await;
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("codegraph.sqlite");
        let codegraph = refact_codegraph::CodeGraphService::open(db_path.clone()).unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("DROP TABLE nodes").unwrap();
        *app.gcx.codegraph.lock().await = Some(Arc::new(codegraph));

        let err =
            match handle_v1_ast_file_symbols(State(app), ast_file_symbols_body(&file_path)).await {
                Ok(response) => panic!("expected doc_defs error, got {}", response.status()),
                Err(err) => err,
            };

        assert_eq!(err.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
