use super::*;

impl WorktreeService {
    pub async fn validate_worktree_meta(
        &self,
        meta: &WorktreeMeta,
    ) -> Result<WorktreeMeta, String> {
        self.validate_worktree_meta_strict(meta).await
    }

    pub async fn validate_worktree_meta_strict(
        &self,
        meta: &WorktreeMeta,
    ) -> Result<WorktreeMeta, String> {
        validate_worktree_id(&meta.id)?;
        let meta_source = canonicalize_existing_dir_async(&meta.source_workspace_root).await?;
        if normalized_path_key_async(&meta_source).await?
            != normalized_path_key_async(&self.source_workspace_root).await?
        {
            return Err("Worktree source root does not match current workspace".to_string());
        }
        let meta_root = normalized_path_key_async(&meta.root).await?;
        let registry = self.load_registry_unlocked().await?;
        let record = registry
            .records
            .iter()
            .find(|record| record.meta.id == meta.id)
            .cloned()
            .ok_or_else(|| format!("Worktree '{}' is not registered", meta.id))?;
        let record_root = normalized_path_key_async(&record.meta.root).await?;
        if record_root != meta_root {
            return Err(format!(
                "Worktree '{}' root mismatch: '{}' != '{}'",
                meta.id,
                meta_root.display(),
                record_root.display()
            ));
        }
        let record_source =
            canonicalize_existing_dir_async(&record.meta.source_workspace_root).await?;
        if normalized_path_key_async(&record_source).await?
            != normalized_path_key_async(&meta_source).await?
        {
            return Err(format!("Worktree '{}' source root mismatch", meta.id));
        }
        let status_root = record.meta.root.clone();
        let status = tokio::task::spawn_blocking(move || git::status_for_path(&status_root))
            .await
            .map_err(|e| format!("Worktree status task failed: {}", e))?;
        if !status.path_exists {
            return Err(format!(
                "Worktree '{}' root '{}' is registered but no longer exists on disk",
                meta.id,
                record.meta.root.display()
            ));
        }
        if !status.is_git_worktree {
            return Err(format!(
                "Worktree '{}' root '{}' exists but is not a git worktree",
                meta.id,
                record.meta.root.display()
            ));
        }
        self.validate_registered_record_checkout_record(&record)
            .await?;
        Ok(record.meta.clone())
    }

    pub async fn validate_legacy_task_agent_worktree_meta(
        &self,
        meta: &WorktreeMeta,
    ) -> Result<WorktreeMeta, String> {
        validate_worktree_id(&meta.id)?;
        if meta.kind != "task_agent" {
            return Err("Legacy worktree metadata must be task_agent kind".to_string());
        }
        if meta.task_id.as_deref().unwrap_or_default().is_empty()
            || meta.card_id.as_deref().unwrap_or_default().is_empty()
            || meta.agent_id.as_deref().unwrap_or_default().is_empty()
        {
            return Err("Legacy task-agent worktree metadata is missing identity".to_string());
        }
        let meta_source = canonicalize_existing_dir_async(&meta.source_workspace_root).await?;
        if normalized_path_key_async(&meta_source).await?
            != normalized_path_key_async(&self.source_workspace_root).await?
        {
            return Err("Worktree source root does not match current workspace".to_string());
        }
        let meta_root = normalized_path_key_async(&meta.root).await?;
        if let Ok(validated) = self.validate_worktree_meta_strict(meta).await {
            return Ok(validated);
        }
        if normalized_path_key_async(&meta_source).await? == meta_root {
            return Err(format!(
                "Legacy worktree '{}' root equals the source workspace root '{}'; \
                 refusing to bind a task-agent session to the source workspace.",
                meta.id,
                meta_root.display()
            ));
        }
        let cache_root = normalized_path_key_async(&self.registry_dir()).await?;
        if !meta_root.starts_with(&cache_root) {
            return Err(format!(
                "Legacy worktree '{}' root '{}' is outside the Refact worktree cache '{}'; \
                 refusing to bind a task-agent session to it.",
                meta.id,
                meta_root.display(),
                cache_root.display()
            ));
        }
        let source_root = self.source_workspace_root.clone();
        let status_root = meta_root.clone();
        let expected_root = meta_root.clone();
        let (discovered, status) = tokio::task::spawn_blocking(move || {
            let discovered = git::list_git_worktrees(&source_root)
                .into_iter()
                .any(|entry| {
                    normalized_path_key(&entry.root)
                        .map(|root| root == expected_root)
                        .unwrap_or(false)
                });
            (discovered, git::status_for_path(&status_root))
        })
        .await
        .map_err(|e| format!("Legacy worktree validation task failed: {}", e))?;
        if discovered && status.path_exists && status.is_git_worktree {
            let validation_meta = meta.clone();
            tokio::task::spawn_blocking(move || validate_worktree_meta_checkout(&validation_meta))
                .await
                .map_err(|e| format!("Legacy worktree validation task failed: {}", e))??;
            return Ok(meta.clone());
        }
        Err(format!(
            "Worktree '{}' is not registered or discoverable for current workspace",
            meta.id
        ))
    }

    pub async fn diff_worktree(&self, id: &str) -> Result<WorktreeDiffResponse, String> {
        self.diff_worktree_with_limit(id, DEFAULT_MAX_PATCH_BYTES)
            .await
    }

    pub async fn diff_worktree_with_limit(
        &self,
        id: &str,
        max_patch_bytes: usize,
    ) -> Result<WorktreeDiffResponse, String> {
        validate_worktree_id(id)?;
        let registry = self.load_registry_unlocked().await?;
        let record = registry
            .records
            .iter()
            .find(|record| record.meta.id == id)
            .cloned()
            .ok_or_else(|| format!("Worktree '{}' not found", id))?;
        self.validate_registered_record_diff_record(&record).await?;
        let diff_root = record.meta.root.clone();
        let base_commit = record.meta.base_commit.clone();
        let base_branch = record.meta.base_branch.clone();
        let (diff, status, ahead, behind) = tokio::task::spawn_blocking(move || {
            let diff = git::diff_for_path(
                &diff_root,
                base_commit.as_deref(),
                base_branch.as_deref(),
                max_patch_bytes,
            )?;
            let status = git::status_for_path(&diff_root);
            let (ahead, behind) = git::base_relative_ahead_behind(
                &diff_root,
                base_commit.as_deref(),
                base_branch.as_deref(),
            )
            .map(|(ahead, behind)| (Some(ahead), Some(behind)))
            .unwrap_or((None, None));
            Ok::<_, String>((diff, status, ahead, behind))
        })
        .await
        .map_err(|e| format!("Worktree diff task failed: {}", e))??;
        Ok(WorktreeDiffResponse {
            id: id.to_string(),
            branch: record.meta.branch.clone(),
            base_branch: record.meta.base_branch.clone(),
            base_commit: record.meta.base_commit.clone(),
            ahead,
            behind,
            status,
            files: diff.files,
            stats: diff.stats,
            patch: diff.patch,
            patch_truncated: diff.patch_truncated,
            patch_shown_bytes: diff.patch_shown_bytes,
            patch_total_bytes: diff.patch_total_bytes,
        })
    }
}
