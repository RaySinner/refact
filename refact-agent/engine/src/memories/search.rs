pub use refact_runtime_api::memories_search::*;

#[cfg(test)]
mod firewall_tests {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex as StdMutex};

    use async_trait::async_trait;
    use refact_core::knowledge_frontmatter::KnowledgeFrontmatter;
    use refact_core::memories_format::create_frontmatter;
    use refact_core::vecdb_types::{
        EmbeddingModelConfig, SearchResult, VecDbStatus, VecdbRecord, VecdbSearch,
        VecdbSearchScope,
    };
    use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
    use tokio::sync::Notify;

    use super::memories_search;
    use crate::files_correction::get_project_dirs;

    struct StubVecdb {
        record_path: PathBuf,
        seen_scopes: Arc<StdMutex<Vec<Vec<VecdbSearchScope>>>>,
        embed_calls: Arc<StdMutex<usize>>,
        search_calls: Arc<StdMutex<usize>>,
        embed_gate: Option<(Arc<Notify>, Arc<Notify>)>,
    }

    #[async_trait]
    impl VecdbSearch for StubVecdb {
        async fn vecdb_search(
            &self,
            _query: String,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<SearchResult, String> {
            Ok(SearchResult {
                query_text: String::new(),
                results: vec![],
            })
        }

        async fn get_status(&self) -> Result<VecDbStatus, String> {
            Err("stub has no status".to_string())
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

        fn current_constants(&self) -> (EmbeddingModelConfig, usize) {
            (
                EmbeddingModelConfig {
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
                    embedding_batch: 0,
                    n_ctx: 0,
                },
                0,
            )
        }

        async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, String> {
            *self.embed_calls.lock().unwrap() += 1;
            if let Some((started, release)) = &self.embed_gate {
                started.notify_one();
                release.notified().await;
            }
            Ok(vec![0.1, 0.2, 0.3])
        }

        async fn vecdb_search_with_embedding(
            &self,
            _embedding: &Vec<f32>,
            _top_n: usize,
            _filter_mb: Option<String>,
        ) -> Result<Vec<VecdbRecord>, String> {
            *self.search_calls.lock().unwrap() += 1;
            Ok(vec![VecdbRecord {
                vector: None,
                file_path: self.record_path.clone(),
                start_line: 1,
                end_line: 1,
                distance: 0.1,
                usefulness: 90.0,
            }])
        }

        async fn vecdb_search_scopes_with_embedding(
            &self,
            _embedding: &Vec<f32>,
            scopes: &[VecdbSearchScope],
        ) -> Result<Vec<Vec<VecdbRecord>>, String> {
            *self.search_calls.lock().unwrap() += 1;
            self.seen_scopes.lock().unwrap().push(scopes.to_vec());
            Ok(scopes
                .iter()
                .map(|_| {
                    vec![VecdbRecord {
                        vector: None,
                        file_path: self.record_path.clone(),
                        start_line: 1,
                        end_line: 1,
                        distance: 0.1,
                        usefulness: 90.0,
                    }]
                })
                .collect())
        }
    }

    async fn write_knowledge_note(knowledge_dir: &Path, name: &str, body: &str) -> PathBuf {
        tokio::fs::create_dir_all(knowledge_dir).await.unwrap();
        let path = knowledge_dir.join(name);
        let frontmatter = create_frontmatter(
            Some("Firewall note"),
            &["preference".to_string()],
            &Vec::new(),
            &Vec::new(),
            "domain",
        );
        let md = format!("{}\n\n{}", frontmatter.to_yaml(), body);
        tokio::fs::write(&path, md).await.unwrap();
        path
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_search_uses_one_embedding_and_batched_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        let project_root = get_project_dirs(gcx.clone())
            .await
            .into_iter()
            .next()
            .expect("test workspace must resolve to a project dir");
        let knowledge_dir = project_root.join(KNOWLEDGE_FOLDER_NAME);
        write_knowledge_note(
            &knowledge_dir,
            "note.md",
            "Firewall stub body about codegraph.",
        )
        .await;

        let seen_scopes = Arc::new(StdMutex::new(Vec::<Vec<VecdbSearchScope>>::new()));
        let embed_calls = Arc::new(StdMutex::new(0usize));
        let search_calls = Arc::new(StdMutex::new(0usize));
        let stub: Arc<dyn VecdbSearch> = Arc::new(StubVecdb {
            record_path: PathBuf::new(),
            seen_scopes: seen_scopes.clone(),
            embed_calls: embed_calls.clone(),
            search_calls: search_calls.clone(),
            embed_gate: None,
        });
        *gcx.vec_db.lock().await = Some(stub);

        let _records = memories_search(gcx.clone(), "codegraph", 5, 5, None)
            .await
            .unwrap();

        assert_eq!(
            *embed_calls.lock().unwrap(),
            1,
            "memories_search must embed the query once"
        );
        assert_eq!(*search_calls.lock().unwrap(), 1);
        let scopes = seen_scopes.lock().unwrap().clone();
        let knowledge_str = knowledge_dir.to_string_lossy().to_string();
        assert!(
            scopes
                .iter()
                .flatten()
                .any(|scope| scope.path_prefix.starts_with(&knowledge_str)),
            "expected a scope rooted at {knowledge_str}, got {scopes:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_search_survives_vecdb_reload_while_embedding() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let old_embed_calls = Arc::new(StdMutex::new(0usize));
        let old: Arc<dyn VecdbSearch> = Arc::new(StubVecdb {
            record_path: PathBuf::new(),
            seen_scopes: Arc::new(StdMutex::new(Vec::new())),
            embed_calls: old_embed_calls.clone(),
            search_calls: Arc::new(StdMutex::new(0)),
            embed_gate: Some((started.clone(), release.clone())),
        });
        *gcx.vec_db.lock().await = Some(old);

        let search = tokio::spawn(memories_search(gcx.clone(), "reload", 1, 1, None));
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("search started embedding");

        let replacement: Arc<dyn VecdbSearch> = Arc::new(StubVecdb {
            record_path: PathBuf::new(),
            seen_scopes: Arc::new(StdMutex::new(Vec::new())),
            embed_calls: Arc::new(StdMutex::new(0)),
            search_calls: Arc::new(StdMutex::new(0)),
            embed_gate: None,
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            *gcx.vec_db.lock().await = Some(replacement);
        })
        .await
        .expect("VecDB reload must not wait for the embedding");
        release.notify_one();

        search.await.unwrap().unwrap();
        assert_eq!(*old_embed_calls.lock().unwrap(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_search_falls_back_when_vecdb_absent() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        let project_root = get_project_dirs(gcx.clone())
            .await
            .into_iter()
            .next()
            .expect("test workspace must resolve to a project dir");
        let knowledge_dir = project_root.join(KNOWLEDGE_FOLDER_NAME);
        write_knowledge_note(
            &knowledge_dir,
            "fallback.md",
            "Codegraph firewall fallback keyword body.",
        )
        .await;

        assert!(
            gcx.vec_db.lock().await.is_none(),
            "test fixture must start with vec_db absent"
        );

        let _records = memories_search(gcx.clone(), "codegraph", 5, 5, None)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fallback_search_uses_ready_index_without_reading_the_corpus() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let project_root =
            dunce::simplified(&std::fs::canonicalize(dir.path()).unwrap()).to_path_buf();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![project_root.clone()];
        let knowledge_dir = project_root.join(KNOWLEDGE_FOLDER_NAME);
        let indexed = write_knowledge_note(&knowledge_dir, "indexed.md", "codegraph needle").await;
        for index in 0..1_000 {
            tokio::fs::write(
                knowledge_dir.join(format!("unindexed-{index}.md")),
                "codegraph corpus file that fallback must not read",
            )
            .await
            .unwrap();
        }
        {
            let mut index = gcx.knowledge_index.lock().await;
            let text = tokio::fs::read_to_string(&indexed).await.unwrap();
            let (frontmatter, content_start) = KnowledgeFrontmatter::parse(&text);
            index.add_from_frontmatter(indexed.clone(), &frontmatter, Some(&text[content_start..]));
            index.mark_ready();
        }

        let records = memories_search(gcx, "codegraph", 5, 0, None).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].file_path.as_deref(), Some(indexed.as_path()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fallback_search_discloses_unavailable_index_without_scanning() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        write_knowledge_note(&knowledge_dir, "unavailable.md", "codegraph needle").await;

        let records = memories_search(gcx, "codegraph", 5, 0, None).await.unwrap();
        assert!(records.is_empty());
    }
}
