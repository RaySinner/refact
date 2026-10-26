use super::*;

use std::collections::HashMap;

impl WorktreeService {
    pub async fn open_worktree(&self, id: &str) -> Result<OpenWorktreeResponse, String> {
        let view = self.get_worktree(id).await?;
        Ok(OpenWorktreeResponse {
            id: view.meta.id,
            path: view.meta.root,
            branch: view.meta.branch,
            can_open_folder: view.status.path_exists && view.status.is_git_worktree,
        })
    }

    pub async fn inspect_worktrees(&self) -> Result<WorktreeInventory, String> {
        self.inspect_worktrees_with_min_age(24).await
    }

    pub async fn inspect_worktrees_with_min_age(
        &self,
        min_age_hours: u64,
    ) -> Result<WorktreeInventory, String> {
        let registry = self.load_registry_unlocked().await?;
        let service = self.clone();
        tokio::task::spawn_blocking(move || {
            service.inspect_worktrees_from_registry(&registry, min_age_hours)
        })
        .await
        .map_err(|e| format!("Worktree inspection task failed: {}", e))?
    }

    pub async fn cleanup_worktrees_dry_run(
        &self,
        request: WorktreeCleanupRequest,
    ) -> Result<WorktreeCleanupPlan, String> {
        validate_cleanup_request(&request)?;
        let registry = self.load_registry_unlocked().await?;
        let service = self.clone();
        let min_age_hours = request.min_age_hours;
        let inventory = tokio::task::spawn_blocking(move || {
            service.inspect_worktrees_from_registry(&registry, min_age_hours)
        })
        .await
        .map_err(|e| format!("Worktree cleanup inspection task failed: {}", e))??;
        Ok(self.cleanup_plan_from_inventory(&inventory, request))
    }

    pub async fn cleanup_worktrees(
        &self,
        request: WorktreeCleanupRequest,
    ) -> Result<WorktreeCleanupResult, String> {
        let service = self.clone();
        tokio::spawn(async move {
            validate_cleanup_request(&request)?;
            let _guard = registry_write_lock().lock().await;
            let registry = service.load_registry_unlocked().await?;
            let blocking_service = service.clone();
            let (result, updated_registry) = tokio::task::spawn_blocking(move || {
                let mut registry = registry;
                let mut registry_changed = false;
                let result = blocking_service.cleanup_worktrees_inner_blocking(
                    request,
                    &mut registry,
                    &mut registry_changed,
                )?;
                Ok::<_, String>((result, registry_changed.then_some(registry)))
            })
            .await
            .map_err(|e| format!("Worktree cleanup blocking task failed: {}", e))??;
            if let Some(registry) = updated_registry {
                service.save_registry_unlocked(&registry).await?;
            }
            Ok(result)
        })
        .await
        .map_err(|e| format!("Worktree cleanup task failed: {}", e))?
    }

    fn cleanup_worktrees_inner_blocking(
        &self,
        request: WorktreeCleanupRequest,
        registry: &mut WorktreeRegistry,
        registry_changed: &mut bool,
    ) -> Result<WorktreeCleanupResult, String> {
        let inventory = self.inspect_worktrees_from_registry(&registry, request.min_age_hours)?;
        let plan = self.cleanup_plan_from_inventory(&inventory, request.clone());
        let mut deleted = Vec::new();
        let mut skipped = plan.skipped.clone();
        let warnings = Vec::new();

        for target in plan.candidates {
            if let Some(index) = registry
                .records
                .iter()
                .position(|record| record.meta.id == target.id)
            {
                let record = registry.records[index].clone();
                let removal = self.cleanup_registered_worktree_blocking(
                    registry,
                    index,
                    &record,
                    target.delete_branch,
                )?;
                *registry_changed = true;
                deleted.push(WorktreeCleanupDeleted {
                    id: target.id,
                    root: target.root,
                    branch: target.branch,
                    worktree_deleted: removal.worktree_deleted,
                    branch_deleted: removal.branch_deleted,
                    registry_deleted: removal.registry_deleted,
                    stale_path: removal.stale_path,
                    warnings: removal.warnings,
                });
            } else {
                let stale_path = !target.root.exists();
                let mut item_warnings =
                    git::remove_worktree_path(&self.source_workspace_root, &target.root);
                let mut branch_deleted = false;
                if target.delete_branch {
                    if let Some(branch) = target.branch.as_deref() {
                        match git::delete_branch(&self.source_workspace_root, branch) {
                            Ok(deleted) => branch_deleted = deleted,
                            Err(e) => item_warnings.push(e),
                        }
                    }
                }
                deleted.push(WorktreeCleanupDeleted {
                    id: target.id,
                    root: target.root.clone(),
                    branch: target.branch.clone(),
                    worktree_deleted: !target.root.exists(),
                    branch_deleted,
                    registry_deleted: false,
                    stale_path,
                    warnings: item_warnings,
                });
            }
        }

        skipped.sort_by(|a, b| a.id.cmp(&b.id));
        deleted.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(WorktreeCleanupResult {
            generated_at: Utc::now().to_rfc3339(),
            request,
            deleted,
            skipped,
            warnings,
        })
    }

    fn inspect_worktrees_from_registry(
        &self,
        registry: &WorktreeRegistry,
        min_age_hours: u64,
    ) -> Result<WorktreeInventory, String> {
        let mut worktrees = Vec::new();
        let mut registered_roots = HashSet::new();
        let registry_dir = self.registry_dir();

        for record in &registry.records {
            let root_key = normalize_lexical(&record.meta.root)?;
            registered_roots.insert(root_key);
            worktrees.push(self.inspect_registered_record(record, min_age_hours));
        }

        let mut discovered_roots = HashSet::new();
        for entry in git::list_git_worktrees(&self.source_workspace_root) {
            let root_key = normalize_lexical(&entry.root)?;
            if root_key == self.source_workspace_root || registered_roots.contains(&root_key) {
                continue;
            }
            if !root_key.starts_with(&registry_dir) || !discovered_roots.insert(root_key.clone()) {
                continue;
            }
            worktrees.push(self.inspect_discovered_root(
                worktree_id_from_path(&root_key),
                root_key,
                entry.branch,
                entry.head,
                true,
                min_age_hours,
            ));
        }

        if registry_dir.exists() {
            if let Ok(read_dir) = std::fs::read_dir(&registry_dir) {
                for entry in read_dir.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let root_key = normalize_lexical(&path)?;
                    if registered_roots.contains(&root_key) || discovered_roots.contains(&root_key)
                    {
                        continue;
                    }
                    if !discovered_roots.insert(root_key.clone()) {
                        continue;
                    }
                    worktrees.push(self.inspect_discovered_root(
                        worktree_id_from_path(&root_key),
                        root_key,
                        None,
                        None,
                        true,
                        min_age_hours,
                    ));
                }
            }
        }

        worktrees.sort_by(|a, b| a.id.cmp(&b.id));
        let summary = summarize_inventory(&worktrees);
        let cleanup_candidates = worktrees
            .iter()
            .filter(|item| item.cleanup_candidate)
            .map(|item| item.id.clone())
            .collect();
        Ok(WorktreeInventory {
            project_hash: self.project_hash.clone(),
            source_workspace_root: self.source_workspace_root.clone(),
            generated_at: Utc::now().to_rfc3339(),
            summary,
            worktrees,
            cleanup_candidates,
        })
    }

    fn inspect_registered_record(
        &self,
        record: &WorktreeRegistryRecord,
        min_age_hours: u64,
    ) -> WorktreeInspection {
        let age_hours = age_hours_since(
            record
                .last_seen_at
                .as_deref()
                .unwrap_or(record.updated_at.as_str()),
        );
        let last_used_at = record
            .last_seen_at
            .clone()
            .or_else(|| Some(record.updated_at.clone()));
        if let Err(error) = validate_registered_record_checkout(record) {
            return self.inspect_invalid_registered_record(
                record,
                age_hours,
                last_used_at,
                min_age_hours,
                error,
            );
        }
        let mut item = self.inspect_root(
            record.meta.id.clone(),
            "registered".to_string(),
            record.meta.root.clone(),
            record.meta.branch.clone(),
            record.meta.base_branch.clone(),
            record.meta.base_commit.clone(),
            record.references.clone(),
            age_hours,
            last_used_at,
            false,
            false,
            min_age_hours,
        );
        item.attached_chat_ids = attached_chat_ids(&item.references);
        item.attached_task_ids = attached_task_ids(&item.references);
        item
    }

    fn inspect_invalid_registered_record(
        &self,
        record: &WorktreeRegistryRecord,
        age_hours: Option<u64>,
        last_used_at: Option<String>,
        min_age_hours: u64,
        error: String,
    ) -> WorktreeInspection {
        let references = record.references.clone();
        let mut item = WorktreeInspection {
            id: record.meta.id.clone(),
            source: "registered".to_string(),
            root: record.meta.root.clone(),
            branch: record.meta.branch.clone(),
            base_branch: record.meta.base_branch.clone(),
            base_commit: record.meta.base_commit.clone(),
            status: WorktreeStatus {
                path_exists: record.meta.root.exists(),
                is_git_worktree: false,
                dirty: false,
                conflicted: false,
                staged_count: 0,
                unstaged_count: 0,
                untracked_count: 0,
                branch: None,
                head_commit: None,
                error: Some(format!("checkout validation failed: {}", error)),
            },
            reference_count: references.len(),
            shared: references.len() > 1,
            references,
            stale: true,
            conflicted: false,
            changed_files: 0,
            committed_files: 0,
            staged_files: 0,
            unstaged_files: 0,
            untracked_files: 0,
            additions: 0,
            deletions: 0,
            cleanup_candidate: false,
            cleanup_blockers: Vec::new(),
            disk_usage_bytes: None,
            age_hours,
            last_used_at,
            branch_merged: None,
            registry_missing: false,
            cache_dir_missing_from_registry: false,
            attached_chat_ids: Vec::new(),
            attached_task_ids: Vec::new(),
        };
        let request = WorktreeCleanupRequest {
            ids: vec![item.id.clone()],
            min_age_hours,
            ..WorktreeCleanupRequest::default()
        };
        item.cleanup_blockers = cleanup_blockers_for_item(&item, &request);
        item.attached_chat_ids = attached_chat_ids(&item.references);
        item.attached_task_ids = attached_task_ids(&item.references);
        item
    }

    fn inspect_discovered_root(
        &self,
        id: String,
        root: PathBuf,
        branch: Option<String>,
        _head: Option<String>,
        cache_dir_missing_from_registry: bool,
        min_age_hours: u64,
    ) -> WorktreeInspection {
        let age_hours = filesystem_age_hours(&root);
        let mut item = self.inspect_root(
            id,
            "discovered".to_string(),
            root,
            branch,
            None,
            None,
            Vec::new(),
            age_hours,
            None,
            true,
            cache_dir_missing_from_registry,
            min_age_hours,
        );
        item.attached_chat_ids = attached_chat_ids(&item.references);
        item.attached_task_ids = attached_task_ids(&item.references);
        item
    }

    fn inspect_root(
        &self,
        id: String,
        source: String,
        root: PathBuf,
        branch: Option<String>,
        base_branch: Option<String>,
        base_commit: Option<String>,
        references: Vec<WorktreeReference>,
        age_hours: Option<u64>,
        last_used_at: Option<String>,
        registry_missing: bool,
        cache_dir_missing_from_registry: bool,
        min_age_hours: u64,
    ) -> WorktreeInspection {
        let mut status = git::status_for_path(&root);
        if let Some(branch) = branch.as_deref() {
            if status.branch.is_none() {
                status.branch = Some(branch.to_string());
            }
        }
        let mut diff_stats = None;
        let mut diff_error = None;
        if status.path_exists && status.is_git_worktree {
            if base_commit.is_none() && base_branch.is_none() {
                diff_error = Some("base metadata is missing".to_string());
            } else {
                match git::diff_for_path(&root, base_commit.as_deref(), base_branch.as_deref(), 1) {
                    Ok(diff) => diff_stats = Some(diff.stats),
                    Err(e) => diff_error = Some(e),
                }
            }
        }
        if let Some(error) = diff_error {
            status.error = Some(format!("diff failed: {}", error));
        }
        let conflicted = status.conflicted
            || (status.path_exists
                && status.is_git_worktree
                && !git::conflict_files_for_path(&root).is_empty());
        status.conflicted = conflicted;
        let shared = references.len() > 1;
        let stale = !status.path_exists || !status.is_git_worktree;
        let changed_files = diff_stats
            .as_ref()
            .map(|stats| stats.files_changed)
            .unwrap_or_else(|| {
                status.staged_count + status.unstaged_count + status.untracked_count
            });
        let committed_files = diff_stats
            .as_ref()
            .map(|stats| stats.committed_files)
            .unwrap_or(0);
        let staged_files = diff_stats
            .as_ref()
            .map(|stats| stats.staged_files)
            .unwrap_or(status.staged_count);
        let unstaged_files = diff_stats
            .as_ref()
            .map(|stats| stats.unstaged_files)
            .unwrap_or(status.unstaged_count);
        let untracked_files = diff_stats
            .as_ref()
            .map(|stats| stats.untracked_files)
            .unwrap_or(status.untracked_count);
        let additions = diff_stats
            .as_ref()
            .map(|stats| stats.additions)
            .unwrap_or(0);
        let deletions = diff_stats
            .as_ref()
            .map(|stats| stats.deletions)
            .unwrap_or(0);
        let branch_merged = branch.as_deref().and_then(|branch| {
            base_branch
                .as_deref()
                .map(|base| git::branch_merged_into(&self.source_workspace_root, branch, base))
        });
        let mut item = WorktreeInspection {
            id,
            source,
            root: root.clone(),
            branch,
            base_branch,
            base_commit,
            status,
            reference_count: references.len(),
            references,
            shared,
            stale,
            conflicted,
            changed_files,
            committed_files,
            staged_files,
            unstaged_files,
            untracked_files,
            additions,
            deletions,
            cleanup_candidate: false,
            cleanup_blockers: Vec::new(),
            disk_usage_bytes: cheap_disk_usage(&root),
            age_hours,
            last_used_at,
            branch_merged,
            registry_missing,
            cache_dir_missing_from_registry,
            attached_chat_ids: Vec::new(),
            attached_task_ids: Vec::new(),
        };
        item.reference_count = item.references.len();
        item.shared = item.reference_count > 1;
        let request = WorktreeCleanupRequest {
            ids: vec![item.id.clone()],
            min_age_hours,
            ..WorktreeCleanupRequest::default()
        };
        item.cleanup_blockers = cleanup_blockers_for_item(&item, &request);
        item.cleanup_candidate = item.cleanup_blockers.is_empty();
        item
    }

    fn cleanup_plan_from_inventory(
        &self,
        inventory: &WorktreeInventory,
        request: WorktreeCleanupRequest,
    ) -> WorktreeCleanupPlan {
        let by_id: HashMap<&str, &WorktreeInspection> = inventory
            .worktrees
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect();
        let mut candidates = Vec::new();
        let mut skipped = Vec::new();
        let mut seen = HashSet::new();
        for id in &request.ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some(item) = by_id.get(id.as_str()) else {
                skipped.push(WorktreeCleanupSkipped {
                    id: id.clone(),
                    root: None,
                    reason: "not_found".to_string(),
                    details: vec!["worktree id was not found in registry or discovery".to_string()],
                });
                continue;
            };
            let blockers = cleanup_blockers_for_item(item, &request);
            if blockers.is_empty() {
                candidates.push(WorktreeCleanupTarget {
                    id: item.id.clone(),
                    root: item.root.clone(),
                    branch: item.branch.clone(),
                    shared: item.shared,
                    stale: item.stale,
                    changed_files: item.changed_files,
                    additions: item.additions,
                    deletions: item.deletions,
                    delete_branch: request.delete_branches,
                    references: item.references.clone(),
                    disk_usage_bytes: item.disk_usage_bytes,
                });
            } else {
                skipped.push(WorktreeCleanupSkipped {
                    id: item.id.clone(),
                    root: Some(item.root.clone()),
                    reason: blockers
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "blocked".to_string()),
                    details: blockers,
                });
            }
        }
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        skipped.sort_by(|a, b| a.id.cmp(&b.id));
        WorktreeCleanupPlan {
            generated_at: Utc::now().to_rfc3339(),
            request,
            candidates,
            skipped,
        }
    }

    fn cleanup_registered_worktree_blocking(
        &self,
        registry: &mut WorktreeRegistry,
        index: usize,
        record: &WorktreeRegistryRecord,
        delete_branch: bool,
    ) -> Result<WorktreeRemovalResult, String> {
        validate_registered_record_checkout(record)?;
        let stale_path = !record.meta.root.exists();
        let mut warnings = git::remove_worktree(
            &record.meta.source_workspace_root,
            &record.meta.id,
            &record.meta.root,
        );
        let worktree_deleted = !record.meta.root.exists();
        let mut branch_deleted = false;
        if delete_branch {
            if let Some(branch) = record.meta.branch.as_deref() {
                match git::delete_branch(&record.meta.source_workspace_root, branch) {
                    Ok(deleted) => branch_deleted = deleted,
                    Err(e) => warnings.push(e),
                }
            }
        }
        let registry_deleted = worktree_deleted;
        if registry_deleted {
            registry.records.remove(index);
        } else if let Some(record) = registry.records.get_mut(index) {
            record.last_known_status = Some(git::status_for_path(&record.meta.root));
            record.updated_at = Utc::now().to_rfc3339();
        }
        Ok(WorktreeRemovalResult {
            worktree_deleted,
            branch_deleted,
            registry_deleted,
            stale_path,
            warnings,
        })
    }
}
