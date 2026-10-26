use super::*;

impl WorktreeService {
    pub async fn delete_worktree(
        &self,
        id: &str,
        delete_branch: bool,
        force_referenced: bool,
    ) -> Result<DeleteWorktreeResponse, String> {
        validate_worktree_id(id)?;
        let _guard = registry_write_lock().lock().await;
        let mut registry = self.load_registry_unlocked().await?;
        let index = registry
            .records
            .iter()
            .position(|record| record.meta.id == id)
            .ok_or_else(|| format!("Worktree '{}' not found", id))?;
        let record = registry.records[index].clone();
        if !force_referenced && !record.references.is_empty() {
            return Err(format!(
                "Worktree '{}' still has {} active reference(s) (kinds: {}); \
                 pass force_referenced=true to delete anyway.",
                record.meta.id,
                record.references.len(),
                record
                    .references
                    .iter()
                    .map(|reference| reference.kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let removal_source = record.meta.source_workspace_root.clone();
        let removal_id = record.meta.id.clone();
        let removal_root = record.meta.root.clone();
        let removal_branch = record.meta.branch.clone();
        let (stale_path, mut warnings, branch_result, still_present) =
            match tokio::task::spawn_blocking(move || {
                let stale_path = !removal_root.exists();
                let warnings = git::remove_worktree(&removal_source, &removal_id, &removal_root);
                let branch_result = if delete_branch {
                    removal_branch
                        .as_deref()
                        .map(|branch| git::delete_branch(&removal_source, branch))
                } else {
                    None
                };
                let still_present = removal_root.exists();
                (stale_path, warnings, branch_result, still_present)
            })
            .await
            {
                Ok(triple) => triple,
                Err(e) => {
                    return Err(format!(
                        "Worktree '{}' removal task failed: {}",
                        record.meta.id, e
                    ));
                }
            };
        if still_present {
            return Err(format!(
                "Failed to remove worktree directory '{}': {}",
                record.meta.root.display(),
                warnings.join("; ")
            ));
        }

        let mut branch_deleted = false;
        if let Some(result) = branch_result {
            match result {
                Ok(deleted) => branch_deleted = deleted,
                Err(e) => warnings.push(e),
            }
        }
        registry.records.remove(index);
        self.save_registry_unlocked(&registry).await?;
        let affected_reference_count = record.references.len();
        Ok(DeleteWorktreeResponse {
            deleted: true,
            branch_deleted,
            stale_path,
            affected_references: record.references,
            affected_reference_count,
            warnings,
        })
    }
}
