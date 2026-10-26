use super::*;

impl WorktreeService {
    pub async fn create_worktree(
        &self,
        request: CreateWorktreeRequest,
    ) -> Result<CreateWorktreeResponse, String> {
        let kind = validate_kind(request.kind.as_deref().unwrap_or("chat"))?;
        let branch = match request.branch.clone() {
            Some(branch) => {
                validate_branch_name(&branch)?;
                branch
            }
            None => default_branch_name(&kind, request.chat_id.as_deref()),
        };
        validate_branch_name(&branch)?;
        if let Some(base) = request.base_branch.as_deref() {
            validate_branch_name(base)?;
        }

        let _guard = registry_write_lock().lock().await;
        let mut registry = self.load_registry_unlocked().await?;
        let id = self.next_worktree_id(&registry).await?;
        let worktree_path = self.worktree_path_for_id(&id)?;
        if tokio::fs::try_exists(&worktree_path).await.unwrap_or(false) {
            return Err(format!(
                "Worktree path '{}' already exists",
                worktree_path.display()
            ));
        }
        let parent = worktree_path
            .parent()
            .ok_or_else(|| "Worktree path has no parent".to_string())?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create worktree parent: {}", e))?;

        let create_source = self.source_workspace_root.clone();
        let create_worktree_path = worktree_path.clone();
        let create_id = id.clone();
        let create_branch = branch.clone();
        let create_base_branch = request.base_branch.clone();
        let create_base_commit = request.base_commit.clone();
        let (created, status) = match tokio::task::spawn_blocking(move || {
            let created = git::create_worktree(
                &create_source,
                &create_worktree_path,
                &create_id,
                &create_branch,
                create_base_branch.as_deref(),
                create_base_commit.as_deref(),
            )?;
            let status = git::status_for_path(&create_worktree_path);
            Ok::<_, String>((created, status))
        })
        .await
        {
            Ok(result) => result?,
            Err(e) => return Err(format!("Worktree creation task failed: {}", e)),
        };
        let now = Utc::now().to_rfc3339();
        let reference = request_to_reference(&kind, &request);
        let references = reference.into_iter().collect::<Vec<_>>();
        let meta = WorktreeMeta {
            id: id.clone(),
            kind,
            root: normalized_path_key_async(&worktree_path).await?,
            source_workspace_root: self.source_workspace_root.clone(),
            repo_root: created.repo_root,
            branch: Some(branch.clone()),
            base_branch: created.base_branch,
            base_commit: Some(created.base_commit),
            task_id: request.task_id.clone(),
            card_id: request.card_id.clone(),
            agent_id: request.agent_id.clone(),
            enforce: true,
        };
        let record = WorktreeRegistryRecord {
            meta,
            created_at: now.clone(),
            updated_at: now,
            last_seen_at: Some(Utc::now().to_rfc3339()),
            references,
            last_known_status: Some(status.clone()),
        };
        registry.records.push(record.clone());
        if let Err(e) = self.save_registry_unlocked(&registry).await {
            let cleanup_source = self.source_workspace_root.clone();
            let cleanup_id = id.clone();
            let cleanup_path = worktree_path.clone();
            let cleanup_branch = branch.clone();
            let branch_was_created = created.branch_was_created;
            let warnings = tokio::task::spawn_blocking(move || {
                let mut warnings =
                    git::remove_worktree(&cleanup_source, &cleanup_id, &cleanup_path);
                if branch_was_created {
                    if let Err(branch_err) = git::delete_branch(&cleanup_source, &cleanup_branch) {
                        warnings.push(branch_err);
                    }
                }
                warnings
            })
            .await
            .unwrap_or_else(|join_error| vec![format!("Cleanup task failed: {}", join_error)]);
            return Err(format!(
                "Failed to save worktree registry: {}; cleanup warnings: {}",
                e,
                warnings.join("; ")
            ));
        }

        let mut warnings = Vec::new();
        if created.dirty_source {
            warnings.push(
                "Source checkout has uncommitted changes; worktree was created from committed HEAD/base only"
                    .to_string(),
            );
        }
        Ok(CreateWorktreeResponse {
            worktree: self.record_view_with_status(&record, status).await?,
            branch_was_created: created.branch_was_created,
            dirty_source_warning: created.dirty_source,
            warnings,
        })
    }

    pub async fn add_reference(
        &self,
        id: &str,
        reference: WorktreeReference,
    ) -> Result<WorktreeRecordView, String> {
        validate_worktree_id(id)?;
        if !reference.has_identity() {
            return Err("Worktree reference must include at least one id".to_string());
        }
        let _guard = registry_write_lock().lock().await;
        let mut registry = self.load_registry_unlocked().await?;
        let (record, changed) = {
            let record = registry
                .records
                .iter_mut()
                .find(|record| record.meta.id == id)
                .ok_or_else(|| format!("Worktree '{}' not found", id))?;
            let changed = !record.references.contains(&reference);
            if changed {
                record.references.push(reference);
                record.updated_at = Utc::now().to_rfc3339();
            }
            (record.clone(), changed)
        };
        if changed {
            self.save_registry_unlocked(&registry).await?;
        }
        drop(_guard);
        self.record_view(&record).await
    }

    pub async fn remove_reference(
        &self,
        id: &str,
        reference: &WorktreeReference,
    ) -> Result<WorktreeRecordView, String> {
        validate_worktree_id(id)?;
        if !reference.has_identity() {
            return Err("Worktree reference must include at least one id".to_string());
        }
        let _guard = registry_write_lock().lock().await;
        let mut registry = self.load_registry_unlocked().await?;
        let (record, changed) = {
            let record = registry
                .records
                .iter_mut()
                .find(|record| record.meta.id == id)
                .ok_or_else(|| format!("Worktree '{}' not found", id))?;
            let before = record.references.len();
            record.references.retain(|item| item != reference);
            let changed = record.references.len() != before;
            if changed {
                record.updated_at = Utc::now().to_rfc3339();
            }
            (record.clone(), changed)
        };
        if changed {
            self.save_registry_unlocked(&registry).await?;
        }
        drop(_guard);
        self.record_view(&record).await
    }
}
