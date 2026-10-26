use super::*;

impl WorktreeService {
    pub async fn merge_worktree(
        &self,
        id: &str,
        request: MergeWorktreeRequest,
    ) -> Result<MergeWorktreeResponse, String> {
        let service = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            validate_worktree_id(&id)?;
            if let Some(target_branch) = request.target_branch.as_deref() {
                validate_branch_name(target_branch)?;
            }
            let _merge_guard = worktree_merge_lock().lock().await;
            let _registry_guard = registry_write_lock().lock().await;
            let registry = service.load_registry_unlocked().await?;
            let blocking_service = service.clone();
            let blocking_id = id.clone();
            let (response, updated_registry) = tokio::task::spawn_blocking(move || {
                let mut registry = registry;
                let mut registry_changed = false;
                let response = blocking_service.merge_worktree_inner_blocking(
                    &blocking_id,
                    request,
                    &mut registry,
                    &mut registry_changed,
                )?;
                Ok::<_, String>((response, registry_changed.then_some(registry)))
            })
            .await
            .map_err(|e| format!("Worktree merge blocking task failed: {}", e))??;
            if let Some(registry) = updated_registry {
                service.save_registry_unlocked(&registry).await?;
            }
            Ok(response)
        })
        .await
        .map_err(|e| format!("Worktree merge task failed: {}", e))?
    }

    fn merge_worktree_inner_blocking(
        &self,
        id: &str,
        request: MergeWorktreeRequest,
        registry: &mut WorktreeRegistry,
        registry_changed: &mut bool,
    ) -> Result<MergeWorktreeResponse, String> {
        let index = registry
            .records
            .iter()
            .position(|record| record.meta.id == id)
            .ok_or_else(|| format!("Worktree '{}' not found", id))?;
        let record = registry.records[index].clone();
        let source_branch = record
            .meta
            .branch
            .clone()
            .ok_or_else(|| format!("Worktree '{}' has no source branch", id))?;
        validate_registered_record_checkout(&record)?;
        validate_branch_name(&source_branch)?;
        let target_branch = request
            .target_branch
            .clone()
            .or_else(|| record.meta.base_branch.clone())
            .ok_or_else(|| format!("Worktree '{}' has no base branch", id))?;
        validate_branch_name(&target_branch)?;
        if !git::branch_exists(&record.meta.source_workspace_root, &source_branch)? {
            return Err(format!("Source branch '{}' not found", source_branch));
        }
        if !git::branch_exists(&record.meta.source_workspace_root, &target_branch)? {
            return Err(format!("Target branch '{}' not found", target_branch));
        }
        git::ensure_no_merge_in_progress(&record.meta.source_workspace_root, "Target workspace")?;
        git::ensure_dirty_worktree_on_branch(
            &record.meta.source_workspace_root,
            "Target workspace",
            &target_branch,
        )?;
        let commit_message = request
            .commit_message
            .clone()
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| fallback_merge_message(&record, &source_branch));
        let mut committed_uncommitted = None;
        if git::has_porcelain_changes(&record.meta.root)? {
            if !request.include_uncommitted {
                return Err(format!(
                    "Worktree '{}' has uncommitted changes; set include_uncommitted=true to auto-commit them first",
                    id
                ));
            }
            committed_uncommitted = git::commit_all(&record.meta.root, &commit_message)?;
        }
        git::ensure_clean_worktree(&record.meta.root, "Source worktree")?;
        let ahead = git::commits_ahead(
            &record.meta.source_workspace_root,
            &target_branch,
            &source_branch,
        )?;
        let affected_references = record.references.clone();
        let affected_reference_count = affected_references.len();
        if ahead == 0 {
            let cleanup = if request.delete_after_merge && source_branch != target_branch {
                let removal =
                    self.cleanup_registered_worktree_blocking(registry, index, &record, true)?;
                *registry_changed = true;
                Some(removal)
            } else {
                None
            };
            return Ok(MergeWorktreeResponse {
                id: id.to_string(),
                status: "nothing_to_merge".to_string(),
                merged: false,
                strategy: request.strategy.as_str().to_string(),
                source_branch,
                target_branch,
                committed_uncommitted,
                merge_commit: None,
                cleanup,
                conflict: None,
                affected_references,
                affected_reference_count,
                warnings: Vec::new(),
            });
        }
        let preflight = git::preflight_merge_conflicts(
            &record.meta.source_workspace_root,
            &target_branch,
            &source_branch,
            request.strategy.as_str(),
        )?;
        let preflight_warnings = preflight.warnings;
        let conflicts = preflight.conflicts;
        if !conflicts.is_empty() {
            return Ok(MergeWorktreeResponse {
                id: id.to_string(),
                status: "conflict".to_string(),
                merged: false,
                strategy: request.strategy.as_str().to_string(),
                source_branch,
                target_branch,
                committed_uncommitted,
                merge_commit: None,
                cleanup: None,
                conflict: Some(conflict_state(conflicts, true, false)),
                affected_references,
                affected_reference_count,
                warnings: preflight_warnings,
            });
        }
        let target_stash = git::stash_dirty_worktree(
            &record.meta.source_workspace_root,
            "Target workspace",
            Some(&target_branch),
        )?;
        if let Err(e) = git::checkout_branch(&record.meta.source_workspace_root, &target_branch) {
            let mut cleanup_warnings = Vec::new();
            if let Some(stash) = target_stash.as_ref() {
                restore_target_stash(
                    &record.meta.source_workspace_root,
                    stash,
                    &mut cleanup_warnings,
                );
            }
            return Err(format_merge_failure(
                &format!("Failed to checkout target branch '{}'", target_branch),
                &e,
                cleanup_warnings,
            ));
        }
        let target_pre_merge_head = match git::head_rev(&record.meta.source_workspace_root) {
            Ok(head) => head,
            Err(e) => {
                let mut cleanup_warnings = Vec::new();
                if let Some(stash) = target_stash.as_ref() {
                    restore_target_stash(
                        &record.meta.source_workspace_root,
                        stash,
                        &mut cleanup_warnings,
                    );
                }
                return Err(format_merge_failure(
                    "Failed to read target workspace HEAD after checkout",
                    &e,
                    cleanup_warnings,
                ));
            }
        };
        let merge_result = if request.strategy.as_str() == "squash" {
            git::run_git(
                &record.meta.source_workspace_root,
                &["merge", "--squash", &source_branch],
            )
        } else {
            git::run_git_with_refact_author(
                &record.meta.source_workspace_root,
                &[
                    "merge",
                    "--no-ff",
                    "--no-gpg-sign",
                    &source_branch,
                    "-m",
                    &commit_message,
                ],
            )
        };
        if let Err(e) = merge_result {
            let conflict_files = git::conflict_files_for_path(&record.meta.source_workspace_root);
            let merge_abort_warnings =
                git::cleanup_failed_merge(&record.meta.source_workspace_root);
            let merge_abort_ok = merge_abort_warnings.is_empty();
            let mut cleanup_warnings = merge_abort_warnings;
            let stash_restored = if let Some(stash) = target_stash.as_ref() {
                restore_target_stash(
                    &record.meta.source_workspace_root,
                    stash,
                    &mut cleanup_warnings,
                )
            } else {
                true
            };
            if !conflict_files.is_empty() {
                let cleanup_ok = merge_abort_ok && stash_restored;
                let conflict = post_cleanup_conflict_state(
                    &record.meta.source_workspace_root,
                    conflict_files,
                    cleanup_ok,
                );
                return Ok(MergeWorktreeResponse {
                    id: id.to_string(),
                    status: "conflict".to_string(),
                    merged: false,
                    strategy: request.strategy.as_str().to_string(),
                    source_branch,
                    target_branch,
                    committed_uncommitted,
                    merge_commit: None,
                    cleanup: None,
                    conflict: Some(conflict),
                    affected_references,
                    affected_reference_count,
                    warnings: cleanup_warnings,
                });
            }
            return Err(format_merge_failure("Merge failed", &e, cleanup_warnings));
        }
        if request.strategy.as_str() == "squash" {
            if let Err(e) = git::run_git_with_refact_author(
                &record.meta.source_workspace_root,
                &["commit", "-m", &commit_message, "--no-gpg-sign"],
            ) {
                let mut cleanup_warnings =
                    git::cleanup_failed_merge(&record.meta.source_workspace_root);
                if let Some(stash) = target_stash.as_ref() {
                    restore_target_stash(
                        &record.meta.source_workspace_root,
                        stash,
                        &mut cleanup_warnings,
                    );
                }
                return Err(format_merge_failure(
                    "Failed to commit squash merge",
                    &e,
                    cleanup_warnings,
                ));
            }
        }
        let mut warnings = preflight_warnings;
        let merge_commit = match git::head_rev(&record.meta.source_workspace_root) {
            Ok(head) => Some(head),
            Err(e) => {
                warnings.push(format!("Failed to read merge commit: {}", e));
                None
            }
        };
        if let Some(stash) = target_stash.as_ref() {
            match git::apply_and_drop_stash(&record.meta.source_workspace_root, stash) {
                Ok(stash_warnings) => warnings.extend(stash_warnings),
                Err(e) => {
                    let conflict_files =
                        git::conflict_files_for_path(&record.meta.source_workspace_root);
                    let mut cleanup_warnings =
                        git::reset_hard(&record.meta.source_workspace_root, &target_pre_merge_head);
                    let reset_ok = cleanup_warnings.is_empty();
                    let stash_restored = restore_target_stash(
                        &record.meta.source_workspace_root,
                        stash,
                        &mut cleanup_warnings,
                    );
                    let restored_conflicts =
                        git::conflict_files_for_path(&record.meta.source_workspace_root);
                    cleanup_warnings.insert(
                        0,
                        format!(
                            "Failed to reapply target workspace changes after merge; merge was rolled back: {}",
                            e
                        ),
                    );
                    let cleanup_ok = reset_ok && stash_restored;
                    let conflict = post_cleanup_conflict_state(
                        &record.meta.source_workspace_root,
                        if conflict_files.is_empty() {
                            restored_conflicts
                        } else {
                            conflict_files
                        },
                        cleanup_ok,
                    );
                    return Ok(MergeWorktreeResponse {
                        id: id.to_string(),
                        status: "conflict".to_string(),
                        merged: false,
                        strategy: request.strategy.as_str().to_string(),
                        source_branch,
                        target_branch,
                        committed_uncommitted,
                        merge_commit: None,
                        cleanup: None,
                        conflict: Some(conflict),
                        affected_references,
                        affected_reference_count,
                        warnings: cleanup_warnings,
                    });
                }
            }
        }
        let cleanup = if request.delete_after_merge && source_branch != target_branch {
            let removal =
                self.cleanup_registered_worktree_blocking(registry, index, &record, true)?;
            *registry_changed = true;
            Some(removal)
        } else {
            registry.records[index].last_known_status =
                Some(git::status_for_path(&record.meta.root));
            registry.records[index].updated_at = Utc::now().to_rfc3339();
            *registry_changed = true;
            None
        };
        Ok(MergeWorktreeResponse {
            id: id.to_string(),
            status: "merged".to_string(),
            merged: true,
            strategy: request.strategy.as_str().to_string(),
            source_branch,
            target_branch,
            committed_uncommitted,
            merge_commit,
            cleanup,
            conflict: None,
            affected_references,
            affected_reference_count,
            warnings,
        })
    }
}
