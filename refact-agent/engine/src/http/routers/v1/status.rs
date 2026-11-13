pub use refact_http::routers::v1::status::*;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::codegraph::CodeGraphService;
    use crate::vecdb::vdb_structs::VecDbStatus;
    use indexmap::IndexMap;

    fn vecdb_status(state: &str) -> VecDbStatus {
        VecDbStatus {
            files_unprocessed: 0,
            files_total: 0,
            requests_made_since_start: 0,
            vectors_made_since_start: 0,
            db_size: 0,
            db_cache_size: 0,
            state: state.to_string(),
            queue_additions: false,
            vecdb_max_files_hit: false,
            vecdb_errors: IndexMap::new(),
            cache_writes_failed: 0,
            vectors_missing_from_cache: 0,
        }
    }

    #[tokio::test]
    async fn rag_status_vecdb_disabled_reports_turned_off() {
        let gcx = crate::global_context::tests::make_test_gcx().await;

        let status = get_rag_status(gcx).await;

        assert!(status.vecdb.is_none());
        assert_eq!(status.vecdb_alive, "turned_off");
        assert_eq!(status.vec_db_error, "");
        assert!(status.ast.is_none());
        assert_eq!(status.ast_alive, LEGACY_AST_ALIVE);
    }

    #[test]
    fn rag_status_vecdb_disabled_ignores_stale_error() {
        let (status, alive, error) =
            vecdb_rag_status_fields(false, "old vecdb error".to_string(), Ok(None));

        assert!(status.is_none());
        assert_eq!(alive, "turned_off");
        assert_eq!(error, "");
    }

    #[test]
    fn rag_status_vecdb_requested_reports_starting_before_service_exists() {
        let (status, alive, error) = vecdb_rag_status_fields(true, String::new(), Ok(None));

        assert!(status.is_none());
        assert_eq!(alive, "starting");
        assert_eq!(error, "");
    }

    #[test]
    fn rag_status_vecdb_service_reports_working() {
        let (status, alive, error) =
            vecdb_rag_status_fields(true, String::new(), Ok(Some(vecdb_status("done"))));

        assert_eq!(alive, "working");
        assert_eq!(status.expect("vecdb status").state, "done");
        assert_eq!(error, "");
    }

    #[test]
    fn rag_status_vecdb_service_starting_state_surfaces_starting() {
        let (status, alive, error) =
            vecdb_rag_status_fields(true, String::new(), Ok(Some(vecdb_status("starting"))));

        assert_eq!(alive, "starting");
        assert_eq!(status.expect("vecdb status").state, "starting");
        assert_eq!(error, "");
    }

    #[test]
    fn rag_status_vecdb_stored_error_reports_error() {
        let (status, alive, error) =
            vecdb_rag_status_fields(true, "embedding config missing".to_string(), Ok(None));

        assert!(status.is_none());
        assert_eq!(alive, "error");
        assert_eq!(error, "embedding config missing");
    }

    #[test]
    fn rag_status_vecdb_status_error_reports_error() {
        let (status, alive, error) =
            vecdb_rag_status_fields(true, String::new(), Err("status failed".to_string()));

        assert!(status.is_none());
        assert_eq!(alive, "error");
        assert_eq!(error, "status failed");
    }
    #[tokio::test]
    async fn rag_status_codegraph_state_tracks_queue_and_errors() {
        let gcx = crate::global_context::tests::make_test_gcx().await;

        let status = get_rag_status(gcx.clone()).await;
        assert_eq!(status.codegraph_alive, "turned_off");
        let codegraph = status.codegraph.expect("codegraph status must be present");
        assert_eq!(codegraph.state, "turned_off");
        assert_eq!(codegraph.queued, 0);
        assert_eq!(codegraph.cross_file_edges, 0);
        assert!(!codegraph.cross_file_ready);
        assert_eq!(codegraph.throughput_files_per_min, 0.0);
        assert_eq!(codegraph.eta_seconds, None);
        assert_eq!(codegraph.error, "");
        assert_eq!(status.codegraph_error, "");

        let service = Arc::new(CodeGraphService::open_in_memory().unwrap());
        service.enqueue_files(&["src/main.rs".to_string()]);
        *gcx.codegraph.lock().await = Some(service.clone());

        let status = get_rag_status(gcx.clone()).await;
        assert_eq!(status.codegraph_alive, "indexing");
        let codegraph = status.codegraph.expect("codegraph status must be present");
        assert_eq!(codegraph.state, "indexing");
        assert_eq!(codegraph.queued, 1);
        assert!(!codegraph.cross_file_ready);
        assert_eq!(codegraph.throughput_files_per_min, 0.0);
        assert_eq!(codegraph.eta_seconds, None);
        assert_eq!(codegraph.error, "");

        service.drain_batch(10);
        service
            .index_file(
                "src/main.rs",
                "fn main() { helper(); }\nfn helper() {}",
                "rust",
            )
            .await
            .unwrap();
        service.connect_usages().await.unwrap();
        service.mark_initial_index_done();

        let status = get_rag_status(gcx.clone()).await;
        assert_eq!(status.codegraph_alive, "working");
        let codegraph = status.codegraph.expect("codegraph status must be present");
        assert_eq!(codegraph.state, "working");
        assert_eq!(codegraph.queued, 0);
        assert_eq!(codegraph.throughput_files_per_min, 0.0);
        assert_eq!(codegraph.eta_seconds, None);
        assert_eq!(codegraph.counts.files, 1);
        assert_eq!(codegraph.cross_file_edges, 0);
        assert!(codegraph.cross_file_ready);
        assert_eq!(codegraph.error, "");

        *gcx.codegraph_error.lock().unwrap() = "store unavailable".to_string();

        let status = get_rag_status(gcx.clone()).await;
        assert_eq!(status.codegraph_alive, "error");
        assert_eq!(status.codegraph_error, "store unavailable");
        let codegraph = status.codegraph.expect("codegraph status must be present");
        assert_eq!(codegraph.state, "error");
        assert_eq!(codegraph.error, "store unavailable");

        *gcx.codegraph_error.lock().unwrap() = String::new();

        let status = get_rag_status(gcx.clone()).await;
        assert_eq!(status.codegraph_alive, "working");
        assert_eq!(status.codegraph_error, "");
        let codegraph = status.codegraph.expect("codegraph status must be present");
        assert_eq!(codegraph.state, "working");
        assert_eq!(codegraph.error, "");
    }

    #[tokio::test]
    async fn rag_status_codegraph_open_error_surfaces_without_service() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.codegraph_error.lock().unwrap() = "open failed".to_string();

        let status = get_rag_status(gcx.clone()).await;

        assert_eq!(status.codegraph_alive, "error");
        assert_eq!(status.codegraph_error, "open failed");
        let codegraph = status.codegraph.expect("error status must be present");
        assert_eq!(codegraph.state, "error");
        assert_eq!(codegraph.error, "open failed");
        assert_eq!(codegraph.queued, 0);
        assert_eq!(codegraph.cross_file_edges, 0);
        assert!(!codegraph.cross_file_ready);
        assert_eq!(codegraph.throughput_files_per_min, 0.0);
        assert_eq!(codegraph.eta_seconds, None);
        assert_eq!(codegraph.counts, refact_codegraph::Counts::default());
    }

    #[tokio::test]
    async fn throughput_and_eta_fields_present() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let service = Arc::new(CodeGraphService::open_in_memory().unwrap());
        service.enqueue_files(&[
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "src/c.rs".to_string(),
        ]);
        service.record_index_completions(6);
        *gcx.codegraph.lock().await = Some(service);

        let status = get_rag_status(gcx).await;
        let codegraph = status.codegraph.expect("codegraph status must be present");

        assert_eq!(codegraph.queued, 3);
        assert!(codegraph.throughput_files_per_min > 0.0);
        assert!(codegraph.eta_seconds.is_some());
        let json = serde_json::to_value(&codegraph).unwrap();
        assert!(json.get("throughput_files_per_min").is_some());
        assert!(json.get("eta_seconds").is_some());
    }
}
