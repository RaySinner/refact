pub use refact_http::routers::v1::code_lens::*;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use hyper::StatusCode;
#[cfg(test)]
use url::Url;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use hyper::body::to_bytes;
    use std::path::Path;

    fn code_lens_body(path: &Path) -> hyper::body::Bytes {
        let body = serde_json::json!({
            "uri": Url::from_file_path(path).unwrap(),
        });
        hyper::body::Bytes::from(serde_json::to_vec(&body).unwrap())
    }

    #[test]
    fn debug_output_includes_codegraph_doc_def_usages() {
        let store = refact_codegraph::Store::open_in_memory().unwrap();
        store
            .index_file_graph(
                "src/widget.rs",
                "fn caller() {\n    helper();\n}\nfn helper() {}\n",
                "rust",
            )
            .unwrap();
        store.connect_usages().unwrap();

        let defs = refact_codegraph::facade::doc_defs(&store, "src/widget.rs").unwrap();
        let usages = store.doc_usages("src/widget.rs").unwrap();
        let output = build_code_lens_output(&defs, &usages, true);

        assert!(output.iter().any(|lens| {
            lens.line1 == 2 && lens.debug_string.as_deref() == Some("↗src/widget.rs::helper")
        }));
    }

    #[tokio::test]
    async fn code_lens_empty_doc_defs_stays_200() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let codegraph = refact_codegraph::CodeGraphService::open_in_memory().unwrap();
        *gcx.codegraph.lock().await = Some(Arc::new(codegraph));
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("empty.rs");

        let response = handle_v1_code_lens(State(app), code_lens_body(&file_path))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body()).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], 1);
        assert_eq!(json["code_lens"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn code_lens_doc_defs_error_returns_500() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("codegraph.sqlite");
        let codegraph = refact_codegraph::CodeGraphService::open(db_path.clone()).unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("DROP TABLE nodes").unwrap();
        *gcx.codegraph.lock().await = Some(Arc::new(codegraph));
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let file_path = db_dir.path().join("error.rs");

        let err = match handle_v1_code_lens(State(app), code_lens_body(&file_path)).await {
            Ok(response) => panic!("expected doc_defs error, got {}", response.status()),
            Err(err) => err,
        };

        assert_eq!(err.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
