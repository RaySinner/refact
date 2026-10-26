use super::*;

impl WorktreeService {
    pub fn new(cache_dir: PathBuf, source_workspace_root: PathBuf) -> Result<Self, String> {
        let cache_dir = normalize_existing_or_parent(&cache_dir);
        let source_workspace_root = canonicalize_existing_dir(&source_workspace_root)?;
        let worktrees_base = worktrees_base_dir(&cache_dir)?;
        Ok(Self::from_normalized_paths(source_workspace_root, worktrees_base))
    }

    pub async fn new_async(
        cache_dir: PathBuf,
        source_workspace_root: PathBuf,
    ) -> Result<Self, String> {
        let cache_dir = normalize_existing_or_parent_async(&cache_dir).await;
        let source_workspace_root = canonicalize_existing_dir_async(&source_workspace_root).await?;
        let worktrees_base = worktrees_base_dir(&cache_dir)?;
        Ok(Self::from_normalized_paths(source_workspace_root, worktrees_base))
    }

    fn from_normalized_paths(
        source_workspace_root: PathBuf,
        worktrees_base: PathBuf,
    ) -> Self {
        let project_hash = project_hash_for_path(&source_workspace_root);
        Self {
            worktrees_base,
            source_workspace_root,
            project_hash,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_worktree_root_override(
        cache_dir: PathBuf,
        source_workspace_root: PathBuf,
        override_root: Option<&str>,
    ) -> Result<Self, String> {
        let cache_dir = normalize_existing_or_parent(&cache_dir);
        let source_workspace_root = canonicalize_existing_dir(&source_workspace_root)?;
        let worktrees_base = worktrees_base_dir_impl(override_root, &cache_dir)?;
        Ok(Self::from_normalized_paths(source_workspace_root, worktrees_base))
    }

    pub fn source_workspace_root(&self) -> &Path {
        &self.source_workspace_root
    }

    pub fn project_hash(&self) -> &str {
        &self.project_hash
    }

    pub fn registry_dir(&self) -> PathBuf {
        self.worktrees_base.join(&self.project_hash)
    }

    pub fn worktrees_base(&self) -> &Path {
        &self.worktrees_base
    }

    pub fn registry_path(&self) -> PathBuf {
        self.registry_dir().join("index.json")
    }

    pub fn worktree_path_for_id(&self, id: &str) -> Result<PathBuf, String> {
        validate_worktree_id(id)?;
        let path = self.registry_dir().join(id);
        if !path.starts_with(self.registry_dir()) {
            return Err("Resolved worktree path escapes registry directory".to_string());
        }
        Ok(path)
    }

    pub async fn load_registry(&self) -> Result<WorktreeRegistry, String> {
        self.load_registry_unlocked().await
    }

    pub async fn save_registry(&self, registry: &WorktreeRegistry) -> Result<(), String> {
        let _guard = registry_write_lock().lock().await;
        self.validate_registry(registry).await?;
        self.save_registry_unlocked(registry).await
    }

    async fn load_registry_pruning_missing_records(&self) -> Result<WorktreeRegistry, String> {
        let _guard = registry_write_lock().lock().await;
        let mut registry = self.load_registry_unlocked().await?;
        let source_root = self.source_workspace_root.clone();
        let git_roots = tokio::task::spawn_blocking(move || {
            git::prune_worktrees(&source_root)?;
            Ok::<HashSet<PathBuf>, String>(
                git::list_git_worktrees(&source_root)
                    .into_iter()
                    .filter_map(|entry| normalized_path_key(&entry.root).ok())
                    .collect(),
            )
        })
        .await
        .map_err(|error| format!("Git worktree reconciliation task failed: {error}"))??;
        let before = registry.records.len();
        let mut retained = Vec::with_capacity(registry.records.len());
        for record in registry.records.drain(..) {
            let exists = tokio::fs::try_exists(&record.meta.root)
                .await
                .unwrap_or(true);
            let registered_in_git = normalized_path_key(&record.meta.root)
                .map(|root| git_roots.contains(&root))
                .unwrap_or(true);
            if exists || registered_in_git {
                retained.push(record);
            }
        }
        registry.records = retained;
        if registry.records.len() != before {
            let _ = self.save_registry_unlocked(&registry).await;
        }
        Ok(registry)
    }

    pub async fn list_worktrees(&self) -> Result<WorktreeListResponse, String> {
        let registry = self.load_registry_pruning_missing_records().await?;
        let mut worktrees = Vec::with_capacity(registry.records.len());
        for record in &registry.records {
            self.validate_registered_record_checkout_record(record)
                .await?;
            worktrees.push(self.record_view(record).await?);
        }
        worktrees.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let source_root = self.source_workspace_root.clone();
        let (source_current_branch, source_branches) =
            tokio::task::spawn_blocking(move || match git::discover_repo(&source_root) {
                Ok(repo) => (git::current_branch(&repo), git::local_branches(&repo)),
                Err(_) => (None, Vec::new()),
            })
            .await
            .map_err(|e| format!("Worktree source inspection task failed: {}", e))?;
        Ok(WorktreeListResponse {
            project_hash: self.project_hash.clone(),
            source_workspace_root: self.source_workspace_root.clone(),
            source_current_branch,
            source_branches,
            worktrees,
        })
    }

    pub async fn get_worktree(&self, id: &str) -> Result<WorktreeRecordView, String> {
        validate_worktree_id(id)?;
        let registry = self.load_registry_unlocked().await?;
        let record = registry
            .records
            .iter()
            .find(|record| record.meta.id == id)
            .ok_or_else(|| format!("Worktree '{}' not found", id))?;
        self.validate_registered_record_checkout_record(record)
            .await?;
        self.record_view(record).await
    }

    pub async fn validate_registered_worktree_checkout(&self, id: &str) -> Result<(), String> {
        validate_worktree_id(id)?;
        let registry = self.load_registry_unlocked().await?;
        let record = registry
            .records
            .iter()
            .find(|record| record.meta.id == id)
            .ok_or_else(|| format!("Worktree '{}' not found", id))?;
        self.validate_registered_record_checkout_record(record)
            .await
    }
}

impl WorktreeService {
    pub(super) async fn record_view(
        &self,
        record: &WorktreeRegistryRecord,
    ) -> Result<WorktreeRecordView, String> {
        let root = record.meta.root.clone();
        let status = tokio::task::spawn_blocking(move || git::status_for_path(&root))
            .await
            .map_err(|e| format!("Worktree status task failed: {}", e))?;
        self.record_view_with_status(record, status).await
    }

    pub(super) async fn validate_registered_record_checkout_record(
        &self,
        record: &WorktreeRegistryRecord,
    ) -> Result<(), String> {
        let record = record.clone();
        tokio::task::spawn_blocking(move || validate_registered_record_checkout(&record))
            .await
            .map_err(|e| format!("Worktree validation task failed: {}", e))?
    }

    pub(super) async fn validate_registered_record_diff_record(
        &self,
        record: &WorktreeRegistryRecord,
    ) -> Result<(), String> {
        let record = record.clone();
        tokio::task::spawn_blocking(move || validate_registered_record_diff(&record))
            .await
            .map_err(|e| format!("Worktree validation task failed: {}", e))?
    }

    pub(super) async fn record_view_with_status(
        &self,
        record: &WorktreeRegistryRecord,
        status: WorktreeStatus,
    ) -> Result<WorktreeRecordView, String> {
        validate_worktree_id(&record.meta.id)?;
        validate_registry_root_async(&self.registry_dir(), &record.meta.id, &record.meta.root)
            .await?;
        let mut seen = HashSet::new();
        let references = record
            .references
            .iter()
            .filter(|reference| reference.has_identity())
            .filter(|reference| seen.insert(reference_key(reference)))
            .cloned()
            .collect::<Vec<_>>();
        let reference_count = references.len();
        Ok(WorktreeRecordView {
            meta: record.meta.clone(),
            created_at: record.created_at.clone(),
            updated_at: record.updated_at.clone(),
            last_seen_at: record.last_seen_at.clone(),
            references,
            reference_count,
            status,
        })
    }

    pub(super) async fn load_registry_unlocked(&self) -> Result<WorktreeRegistry, String> {
        let path = self.registry_path();
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(WorktreeRegistry {
                schema_version: 1,
                source_workspace_root: self.source_workspace_root.clone(),
                project_hash: self.project_hash.clone(),
                records: Vec::new(),
            });
        }
        let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
            format!(
                "Failed to read worktree registry '{}': {}",
                path.display(),
                e
            )
        })?;
        let parse_path = path.clone();
        let registry: WorktreeRegistry = tokio::task::spawn_blocking(move || {
            serde_json::from_str(&content).map_err(|e| {
                format!(
                    "Failed to parse worktree registry '{}': {}",
                    parse_path.display(),
                    e
                )
            })
        })
        .await
        .map_err(|e| format!("Worktree registry parse task failed: {}", e))??;
        self.validate_registry(&registry).await?;
        Ok(registry)
    }

    pub(super) async fn save_registry_unlocked(&self, registry: &WorktreeRegistry) -> Result<(), String> {
        self.validate_registry(registry).await?;
        let dir = self.registry_dir();
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("Failed to create worktree registry dir: {}", e))?;
        let path = self.registry_path();
        let tmp_path = path.with_extension("json.tmp");
        let registry = registry.clone();
        let content = tokio::task::spawn_blocking(move || {
            serde_json::to_string_pretty(&registry)
                .map_err(|e| format!("Failed to serialize worktree registry: {}", e))
        })
        .await
        .map_err(|e| format!("Worktree registry serialization task failed: {}", e))??;
        tokio::fs::write(&tmp_path, content)
            .await
            .map_err(|e| format!("Failed to write worktree registry temp file: {}", e))?;
        #[cfg(windows)]
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| format!("Failed to remove existing worktree registry: {}", e))?;
        }
        tokio::fs::rename(&tmp_path, &path)
            .await
            .map_err(|e| format!("Failed to replace worktree registry: {}", e))
    }

    async fn validate_registry(&self, registry: &WorktreeRegistry) -> Result<(), String> {
        if registry.project_hash != self.project_hash {
            return Err("Worktree registry project hash mismatch".to_string());
        }
        let registry_root = normalized_path_key_async(&registry.source_workspace_root).await?;
        if registry_root != normalized_path_key_async(&self.source_workspace_root).await? {
            return Err("Worktree registry source root mismatch".to_string());
        }
        for record in &registry.records {
            validate_worktree_id(&record.meta.id)?;
            validate_kind(&record.meta.kind)?;
            if let Some(branch) = record.meta.branch.as_deref() {
                validate_branch_name(branch)?;
            }
            if let Some(base_branch) = record.meta.base_branch.as_deref() {
                validate_branch_name(base_branch)?;
            }
            validate_registry_root_async(&self.registry_dir(), &record.meta.id, &record.meta.root)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn next_worktree_id(&self, registry: &WorktreeRegistry) -> Result<String, String> {
        for _ in 0..16 {
            let id = Uuid::new_v4().to_string();
            if registry.records.iter().all(|record| record.meta.id != id)
                && !tokio::fs::try_exists(self.worktree_path_for_id(&id)?)
                    .await
                    .unwrap_or(false)
            {
                return Ok(id);
            }
        }
        Err("Failed to allocate unique worktree id".to_string())
    }
}
