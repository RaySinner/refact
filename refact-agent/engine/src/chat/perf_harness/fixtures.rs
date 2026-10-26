use super::*;

#[derive(Clone)]
pub(crate) struct BenchmarkFixture {
    _temp_dir: Arc<tempfile::TempDir>,
    workspace: PathBuf,
    gcx: SharedGlobalContext,
    app: AppState,
}

impl BenchmarkFixture {
    async fn new(tool_count: usize) -> Result<Self, String> {
        let temp_dir = Arc::new(
            tempfile::tempdir()
                .map_err(|error| format!("failed to create benchmark fixture: {error}"))?,
        );
        let workspace = temp_dir.path().join("workspace");
        let cache_dir = temp_dir.path().join("cache");
        let config_dir = temp_dir.path().join("config");
        tokio::fs::create_dir_all(&workspace)
            .await
            .map_err(|error| format!("failed to create benchmark workspace: {error}"))?;
        tokio::fs::create_dir_all(workspace.join(".refact").join("trajectories"))
            .await
            .map_err(|error| format!("failed to create benchmark trajectories root: {error}"))?;
        let gcx =
            crate::global_context::tests::make_test_gcx_with_dirs(cache_dir, config_dir).await;
        *gcx.documents_state
            .workspace_folders
            .lock()
            .map_err(|_| "benchmark workspace folders lock poisoned".to_string())? =
            vec![canonicalize_normalized_path(workspace.clone())];
        let mut app = AppState::from_gcx(gcx.clone()).await;
        app.tool_registry = Arc::new(AppToolRegistry::with_fixture_tool_factory(
            gcx.clone(),
            deterministic_tool_factory(tool_count),
        ));
        Ok(Self {
            _temp_dir: temp_dir,
            workspace,
            gcx,
            app,
        })
    }
}

#[derive(Default)]
pub(crate) struct AutoEnrichmentVecdb {
    records: Vec<VecdbRecord>,
    delay: std::time::Duration,
    active_searches: AtomicUsize,
    max_concurrent_searches: AtomicUsize,
}

impl AutoEnrichmentVecdb {
    pub(crate) fn enter_search(&self) -> AutoEnrichmentSearchGuard<'_> {
        let active = self.active_searches.fetch_add(1, Ordering::SeqCst) + 1;
        let mut observed = self.max_concurrent_searches.load(Ordering::SeqCst);
        while active > observed {
            match self.max_concurrent_searches.compare_exchange(
                observed,
                active,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
        AutoEnrichmentSearchGuard { vecdb: self }
    }
}

pub(crate) struct AutoEnrichmentSearchGuard<'a> {
    vecdb: &'a AutoEnrichmentVecdb,
}

impl Drop for AutoEnrichmentSearchGuard<'_> {
    pub(crate) fn drop(&mut self) {
        self.vecdb.active_searches.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl VecdbSearch for AutoEnrichmentVecdb {
    async fn vecdb_search(
        &self,
        query: String,
        _top_n: usize,
        _filter_mb: Option<String>,
    ) -> Result<SearchResult, String> {
        Ok(SearchResult {
            query_text: query,
            results: Vec::new(),
        })
    }

    async fn get_status(&self) -> Result<VecDbStatus, String> {
        Ok(VecDbStatus {
            files_unprocessed: 0,
            files_total: self.records.len(),
            requests_made_since_start: 0,
            vectors_made_since_start: 0,
            db_size: 0,
            db_cache_size: 0,
            state: "local_auto_enrichment_fixture".to_string(),
            queue_additions: false,
            vecdb_max_files_hit: false,
            vecdb_errors: Default::default(),
            cache_writes_failed: 0,
            vectors_missing_from_cache: 0,
        })
    }

    async fn remove_file(&self, _file_path: &PathBuf) -> Result<(), String> {
        Ok(())
    }

    async fn vectorizer_enqueue_files(
        &self,
        _documents: &[String],
        _process_immediately: bool,
        _roots: MemoryPlaneRoots,
    ) {
    }

    pub(crate) fn current_constants(&self) -> (EmbeddingModelConfig, usize) {
        (
            EmbeddingModelConfig {
                model_id: "local-auto-enrichment".to_string(),
                endpoint: String::new(),
                endpoint_style: String::new(),
                embedding_endpoint_style: String::new(),
                api_key: String::new(),
                model_name: "local-auto-enrichment".to_string(),
                embedding_size: 3,
                dimensions: Some(3),
                query_prefix: String::new(),
                document_prefix: String::new(),
                rejection_threshold: 0.0,
                embedding_batch: 1,
                n_ctx: 0,
            },
            0,
        )
    }

    async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, String> {
        let _guard = self.enter_search();
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(vec![0.1, 0.2, 0.3])
    }

    async fn vecdb_search_with_embedding(
        &self,
        _embedding: &Vec<f32>,
        _top_n: usize,
        _filter_mb: Option<String>,
    ) -> Result<Vec<VecdbRecord>, String> {
        let _guard = self.enter_search();
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(self.records.clone())
    }

    async fn vecdb_search_scopes_with_embedding(
        &self,
        _embedding: &Vec<f32>,
        scopes: &[VecdbSearchScope],
    ) -> Result<Vec<Vec<VecdbRecord>>, String> {
        let _guard = self.enter_search();
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(scopes
            .iter()
            .map(|scope| {
                self.records
                    .iter()
                    .filter(|record| {
                        record
                            .file_path
                            .to_string_lossy()
                            .starts_with(&scope.path_prefix)
                    })
                    .take(scope.top_n)
                    .cloned()
                    .collect()
            })
            .collect())
    }
}

pub(crate) struct AutoEnrichmentFixture {
    _temp_dir: Arc<tempfile::TempDir>,
    gcx: SharedGlobalContext,
    excluded_paths: HashSet<PathBuf>,
    vecdb: Option<Arc<AutoEnrichmentVecdb>>,
}

impl AutoEnrichmentFixture {
    async fn new(workload: &AutoEnrichmentWorkload) -> Result<Self, String> {
        let temp_dir = Arc::new(
            tempfile::tempdir()
                .map_err(|error| format!("failed to create enrichment fixture: {error}"))?,
        );
        let cache_dir = temp_dir.path().join("cache");
        let config_dir = temp_dir.path().join("config");
        let gcx =
            crate::global_context::tests::make_test_gcx_with_dirs(cache_dir, config_dir).await;
        let mut roots = Vec::with_capacity(workload.root_count);
        for root_index in 0..workload.root_count {
            let root = temp_dir.path().join(format!("root-{root_index}"));
            tokio::fs::create_dir_all(root.join(crate::file_filter::KNOWLEDGE_FOLDER_NAME))
                .await
                .map_err(|error| format!("failed to create enrichment root: {error}"))?;
            roots.push(root);
        }
        *gcx.documents_state
            .workspace_folders
            .lock()
            .map_err(|_| "enrichment workspace folders lock poisoned".to_string())? = roots.clone();

        let mut records = Vec::with_capacity(workload.knowledge_file_count);
        let mut excluded_paths = HashSet::new();
        for file_index in 0..workload.knowledge_file_count {
            let root = &roots[file_index % roots.len()];
            let path = root
                .join(crate::file_filter::KNOWLEDGE_FOLDER_NAME)
                .join(format!("memory-{file_index}.md"));
            let tags = vec!["fixture".to_string()];
            let empty = Vec::new();
            let mut frontmatter = crate::memories::create_frontmatter(
                Some("Auto enrichment fixture"),
                &tags,
                &empty,
                &empty,
                "memory",
            );
            if file_index < workload.privacy_exclusion_count {
                frontmatter.source_chat_id = Some("auto-enrichment-current-chat".to_string());
                excluded_paths.insert(path.clone());
            }
            let content = format!(
                "{}\n\nEnrichment codegraph fixture result number {file_index}.",
                frontmatter.to_yaml()
            );
            tokio::fs::write(&path, content)
                .await
                .map_err(|error| format!("failed to write enrichment memory: {error}"))?;
            records.push(VecdbRecord {
                vector: None,
                file_path: path,
                start_line: 1,
                end_line: 1,
                distance: 0.1,
                usefulness: 95.0,
            });
        }

        let vecdb = match workload.vecdb_mode {
            AutoEnrichmentVecdbMode::Unavailable => None,
            AutoEnrichmentVecdbMode::Warm => Some(Arc::new(AutoEnrichmentVecdb {
                records,
                delay: std::time::Duration::ZERO,
                ..Default::default()
            })),
            AutoEnrichmentVecdbMode::Cold => Some(Arc::new(AutoEnrichmentVecdb {
                records,
                delay: std::time::Duration::from_millis(1),
                ..Default::default()
            })),
            AutoEnrichmentVecdbMode::Empty => Some(Arc::new(AutoEnrichmentVecdb {
                records: Vec::new(),
                delay: std::time::Duration::from_millis(1),
                ..Default::default()
            })),
        };
        *gcx.vec_db.lock().await = vecdb.clone().map(|backend| backend as Arc<dyn VecdbSearch>);
        if matches!(workload.vecdb_mode, AutoEnrichmentVecdbMode::Unavailable) {
            let index = crate::knowledge_index::build_knowledge_index(gcx.clone()).await;
            *gcx.knowledge_index.lock().await = index;
        }
        Ok(Self {
            _temp_dir: temp_dir,
            gcx,
            excluded_paths,
            vecdb,
        })
    }
}

pub(crate) struct FullSoakFixture {
    base: BenchmarkFixture,
    background_tasks: crate::background_tasks::BackgroundTasksHolder,
    vecdb: Arc<FullSoakVecdb>,
}

impl FullSoakFixture {
    async fn new(tool_count: usize) -> Result<Self, String> {
        let base = BenchmarkFixture::new(tool_count).await?;
        *base
            .gcx
            .documents_state
            .workspace_files
            .lock()
            .map_err(|_| "full soak fixture workspace files lock poisoned".to_string())? =
            vec![base.workspace.join("fixture.rs")];
        tokio::fs::write(
            base.workspace.join("fixture.rs"),
            "pub fn full_soak_fixture() -> usize { 36 }\n",
        )
        .await
        .map_err(|error| format!("failed to write full soak fixture source: {error}"))?;
        let service = Arc::new(
            crate::codegraph::CodeGraphService::open_in_memory().map_err(|error| {
                format!("failed to create in-memory CodeGraph fixture: {error}")
            })?,
        );
        *base.gcx.codegraph.lock().await = Some(service);
        let vecdb = Arc::new(FullSoakVecdb::default());
        *base.gcx.vec_db.lock().await = Some(vecdb.clone());
        crate::chat::start_session_cleanup_task(base.app.chat_ctx());
        crate::chat::start_trajectory_watcher(base.gcx.clone());
        let background_tasks =
            crate::background_tasks::start_full_soak_background_tasks(base.gcx.clone()).await;
        Ok(Self {
            base,
            background_tasks,
            vecdb,
        })
    }

    async fn shutdown(mut self) {
        self.base.gcx.shutdown_flag.store(true, Ordering::SeqCst);
        self.background_tasks.abort().await;
    }
}

#[derive(Default)]
pub(crate) struct FullSoakVecdb {
    deferred_queue: StdMutex<Option<refact_vecdb::vdb_thread::VecdbDeferredQueueProbe>>,
}

impl FullSoakVecdb {
    pub(crate) fn begin_sample(&self, coalescing_enabled: bool) -> Result<(), String> {
        let mut probe = self
            .deferred_queue
            .lock()
            .map_err(|_| "full soak VecDB probe lock poisoned".to_string())?;
        *probe = Some(refact_vecdb::vdb_thread::VecdbDeferredQueueProbe::new(
            coalescing_enabled,
        ));
        Ok(())
    }

    pub(crate) fn drain_deferred(&self) -> Result<(), String> {
        let mut probe = self
            .deferred_queue
            .lock()
            .map_err(|_| "full soak VecDB probe lock poisoned".to_string())?;
        if let Some(probe) = probe.as_mut() {
            probe.drain_after_cooldown();
        }
        Ok(())
    }

    pub(crate) fn metrics(&self) -> refact_vecdb::vdb_thread::VecdbDeferredQueueMetrics {
        self.deferred_queue
            .lock()
            .ok()
            .and_then(|probe| probe.as_ref().map(|probe| probe.metrics()))
            .unwrap_or(refact_vecdb::vdb_thread::VecdbDeferredQueueMetrics {
                enqueue_requests: 0,
                pending_unique_paths: 0,
                processed_paths: 0,
            })
    }
}

#[async_trait]
impl VecdbSearch for FullSoakVecdb {
    async fn vecdb_search(
        &self,
        query: String,
        _top_n: usize,
        _filter_mb: Option<String>,
    ) -> Result<SearchResult, String> {
        Ok(SearchResult {
            query_text: query,
            results: Vec::new(),
        })
    }

    async fn get_status(&self) -> Result<VecDbStatus, String> {
        Ok(VecDbStatus {
            files_unprocessed: 0,
            files_total: self.metrics().processed_paths as usize,
            requests_made_since_start: 0,
            vectors_made_since_start: 0,
            db_size: 0,
            db_cache_size: 0,
            state: "local_fixture".to_string(),
            queue_additions: true,
            vecdb_max_files_hit: false,
            vecdb_errors: Default::default(),
            cache_writes_failed: 0,
            vectors_missing_from_cache: 0,
        })
    }

    async fn remove_file(&self, _file_path: &PathBuf) -> Result<(), String> {
        Ok(())
    }

    async fn vectorizer_enqueue_files(
        &self,
        documents: &[String],
        process_immediately: bool,
        _roots: MemoryPlaneRoots,
    ) {
        if !process_immediately {
            if let Ok(mut probe) = self.deferred_queue.lock() {
                if let Some(probe) = probe.as_mut() {
                    probe.enqueue_deferred_paths(documents);
                }
            }
        }
    }

    pub(crate) fn current_constants(&self) -> (EmbeddingModelConfig, usize) {
        (
            EmbeddingModelConfig {
                model_id: "full-soak-local".to_string(),
                endpoint: String::new(),
                endpoint_style: String::new(),
                embedding_endpoint_style: String::new(),
                api_key: String::new(),
                model_name: "full-soak-local".to_string(),
                embedding_size: 0,
                dimensions: None,
                query_prefix: String::new(),
                document_prefix: String::new(),
                rejection_threshold: 0.0,
                embedding_batch: 1,
                n_ctx: 0,
            },
            0,
        )
    }

    async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, String> {
        Ok(Vec::new())
    }

    async fn vecdb_search_with_embedding(
        &self,
        _embedding: &Vec<f32>,
        _top_n: usize,
        _filter_mb: Option<String>,
    ) -> Result<Vec<VecdbRecord>, String> {
        Ok(Vec::new())
    }
}

pub(crate) struct FullSoakEnvGuard {
    values: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl FullSoakEnvGuard {
    pub(crate) fn set(optimized: bool) -> Self {
        let values = [
            crate::chat::trajectories::TRAJECTORY_WRITER_ENV,
            crate::chat::trajectory_index::TRAJECTORY_INDEX_COORDINATOR_ENV,
            crate::chat::trajectories::TRAJECTORY_WATCHER_SELF_WRITE_ENV,
            TOOL_CATALOG_SNAPSHOTS_ENV,
            refact_vecdb::vdb_thread::VECDB_PATH_COALESCING_ENV,
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect::<Vec<_>>();
        let value = if optimized { "1" } else { "0" };
        for (key, _) in &values {
            std::env::set_var(key, value);
        }
        Self { values }
    }
}

impl Drop for FullSoakEnvGuard {
    pub(crate) fn drop(&mut self) {
        for (key, value) in self.values.drain(..) {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

pub(crate) struct ToolCatalogSnapshotEnvGuard {
    previous: Option<std::ffi::OsString>,
}

impl ToolCatalogSnapshotEnvGuard {
    pub(crate) fn set(enabled: bool) -> Self {
        let previous = std::env::var_os("REFACT_TOOL_CATALOG_SNAPSHOTS");
        std::env::set_var(
            "REFACT_TOOL_CATALOG_SNAPSHOTS",
            if enabled { "1" } else { "0" },
        );
        Self { previous }
    }
}

impl Drop for ToolCatalogSnapshotEnvGuard {
    pub(crate) fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var("REFACT_TOOL_CATALOG_SNAPSHOTS", previous);
        } else {
            std::env::remove_var("REFACT_TOOL_CATALOG_SNAPSHOTS");
        }
    }
}

pub(crate) struct SaveCounts {
    total: u64,
    checkpoints: u64,
    required_commits: u64,
    checkpoint_return_elapsed_us: u64,
    background_flush_elapsed_us: u64,
}

pub(crate) struct TrajectoryWriterEnvGuard {
    previous: Option<std::ffi::OsString>,
}

impl TrajectoryWriterEnvGuard {
    pub(crate) fn set(enabled: bool) -> Self {
        let previous = std::env::var_os(crate::chat::trajectories::TRAJECTORY_WRITER_ENV);
        std::env::set_var(
            crate::chat::trajectories::TRAJECTORY_WRITER_ENV,
            if enabled { "1" } else { "0" },
        );
        Self { previous }
    }
}

impl Drop for TrajectoryWriterEnvGuard {
    pub(crate) fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var(crate::chat::trajectories::TRAJECTORY_WRITER_ENV, previous);
        } else {
            std::env::remove_var(crate::chat::trajectories::TRAJECTORY_WRITER_ENV);
        }
    }
}

pub(crate) struct BenchmarkClock {
    origin: Instant,
}

impl Default for BenchmarkClock {
    pub(crate) fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl PerfClock for BenchmarkClock {
    pub(crate) fn now_us(&self) -> u64 {
        self.origin
            .elapsed()
            .as_micros()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

pub(crate) struct BenchmarkTool {
    name: String,
}

#[async_trait]
impl Tool for BenchmarkTool {
    async fn tool_execute(
        &mut self,
        _ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        _args: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let mut message = ChatMessage::new("tool".to_string(), "benchmark result".to_string());
        message.tool_call_id = tool_call_id.clone();
        Ok((false, vec![ContextEnum::ChatMessage(message)]))
    }

    pub(crate) fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: self.name.clone(),
            experimental: false,
            allow_parallel: true,
            description: format!("Deterministic local {} tool", self.name),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            output_schema: None,
            annotations: None,
            display_name: self.name.clone(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: String::new(),
            },
        }
    }

    async fn match_against_confirm_deny(
        &self,
        _ccx: Arc<AMutex<AtCommandsContext>>,
        _args: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<MatchConfirmDeny, String> {
        Ok(MatchConfirmDeny {
            result: MatchConfirmDenyResult::PASS,
            command: self.name.clone(),
            rule: "benchmark fixture".to_string(),
        })
    }
}

pub(crate) fn deterministic_tool_factory(tool_count: usize) -> FixtureToolFactory {
    Arc::new(move || {
        (0..tool_count)
            .map(|index| {
                Box::new(BenchmarkTool {
                    name: format!("benchmark_tool_{index}"),
                }) as Box<dyn Tool + Send>
            })
            .collect()
    })
}
