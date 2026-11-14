pub use refact_runtime_api::knowledge_index::{
    build_knowledge_index, build_knowledge_index_for_ctx, build_knowledge_index_for_ctx_impl,
    extract_task_id_from_path, format_related_memories_section, knowledge_dir_candidates,
    knowledge_index_watcher_background_task, knowledge_watch_roots, refresh_knowledge_index_path,
    KnowledgeBuilder, KnowledgeCard, KnowledgeIndex, KnowledgeSearchFilters, KnowledgeSearchHit,
};

use async_trait::async_trait;

pub struct EngineKnowledgeBuilder;

#[async_trait]
impl KnowledgeBuilder for EngineKnowledgeBuilder {
    async fn build_knowledge_index_for_ctx(
        &self,
        ctx: &crate::chat::ctx::ChatCtx,
    ) -> KnowledgeIndex {
        build_knowledge_index_for_ctx_impl(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_filter::KNOWLEDGE_FOLDER_NAME;

    #[test]
    fn extract_task_id_rejects_non_refact_tasks_path() {
        assert_eq!(
            extract_task_id_from_path(&crate::test_paths::abs("repo/tasks/examples/x.md")),
            None
        );
    }

    #[test]
    fn extract_task_id_accepts_refact_tasks_path() {
        assert_eq!(
            extract_task_id_from_path(&crate::test_paths::abs(
                "workspace/.refact/tasks/T-1/memories/x.md",
            )),
            Some("T-1".to_string())
        );
    }

    #[tokio::test]
    async fn build_index_skips_archived_and_deprecated_memories() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();

        let archived_path = knowledge_dir.join("archived.md");
        let deprecated_path = knowledge_dir.join("deprecated.md");
        let active_path = knowledge_dir.join("active.md");

        tokio::fs::write(
            &archived_path,
            "---\nstatus: archived\ntags: [old]\n---\n\nArchived memory",
        )
        .await
        .unwrap();
        tokio::fs::write(
            &deprecated_path,
            "---\nstatus: deprecated\ntags: [old]\n---\n\nDeprecated memory",
        )
        .await
        .unwrap();
        tokio::fs::write(
            &active_path,
            "---\nstatus: active\ntags: [new]\n---\n\nActive memory",
        )
        .await
        .unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        let index = build_knowledge_index(gcx).await;

        assert!(archived_path.exists());
        assert!(deprecated_path.exists());
        assert!(active_path.exists());
        assert_eq!(index.related_for_tags(&vec!["new".to_string()], 5).len(), 1);
    }

    #[tokio::test]
    async fn refresh_knowledge_index_path_tracks_write_archive_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let dir_root = dunce::simplified(&std::fs::canonicalize(dir.path()).unwrap()).to_path_buf();
        let knowledge_dir = dir_root.join(KNOWLEDGE_FOLDER_NAME);
        let path = knowledge_dir.join("note.md");
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir_root.clone()];

        tokio::fs::write(
            &path,
            "---\ntitle: Café\ntags: [unicode]\nstatus: active\n---\n\nRésumé needle",
        )
        .await
        .unwrap();
        assert!(refresh_knowledge_index_path(gcx.clone(), &path, false).await);
        {
            let index = gcx.knowledge_index.lock().await;
            assert_eq!(
                index
                    .search("résumé", &KnowledgeSearchFilters::default(), 1)
                    .len(),
                1
            );
        }

        tokio::fs::write(
            &path,
            "---\ntitle: Café\ntags: [unicode]\nstatus: archived\n---\n\nRésumé needle",
        )
        .await
        .unwrap();
        refresh_knowledge_index_path(gcx.clone(), &path, false).await;
        assert!(gcx
            .knowledge_index
            .lock()
            .await
            .search("résumé", &KnowledgeSearchFilters::default(), 1)
            .is_empty());

        tokio::fs::remove_file(&path).await.unwrap();
        assert!(refresh_knowledge_index_path(gcx.clone(), &path, true).await);
        assert!(gcx
            .knowledge_index
            .lock()
            .await
            .card_for_path(&path)
            .is_none());
    }

    #[tokio::test]
    async fn knowledge_watch_roots_match_knowledge_dirs_only() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        let roots = knowledge_watch_roots(gcx.clone()).await;
        let expected = knowledge_dir_candidates(gcx.clone()).await;

        assert_eq!(roots, expected);
        assert!(!roots.contains(&dir.path().to_path_buf()));
        for root in roots {
            assert!(root.exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refresh_knowledge_index_path_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        let outside = dir.path().join("outside.md");
        let link = knowledge_dir.join("link.md");
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        tokio::fs::write(&outside, "---\ntags: [private]\n---\n\nprivate needle")
            .await
            .unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        refresh_knowledge_index_path(gcx.clone(), &link, false).await;
        assert!(gcx
            .knowledge_index
            .lock()
            .await
            .card_for_path(&link)
            .is_none());
    }

    #[tokio::test]
    async fn build_index_picks_up_task_memories_and_documents() {
        let dir = tempfile::tempdir().unwrap();
        let task_dir = dir.path().join(".refact/tasks/task-1");
        let memories_dir = task_dir.join("memories");
        let documents_dir = task_dir.join("documents");
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();
        tokio::fs::create_dir_all(&documents_dir).await.unwrap();
        tokio::fs::write(
            memories_dir.join("decision.md"),
            "---\ntitle: Routing\ntask_id: task-1\nkind: decision\nnamespace: card:T-22\ntags: [routing]\n---\n\nUse text search for task memory.",
        )
        .await
        .unwrap();
        tokio::fs::write(
            documents_dir.join("spec.md"),
            "---\nname: Main Spec\nslug: main-spec\nkind: spec\ncreated_at: now\nupdated_at: now\nauthor_role: planner\npinned: true\nversion: 1\nrelevant_cards: [T-22]\n---\n\nDocument body token.",
        )
        .await
        .unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        let index = build_knowledge_index(gcx).await;
        let filters = KnowledgeSearchFilters {
            scope: Some("task".to_string()),
            task_id: Some("task-1".to_string()),
            ..Default::default()
        };

        assert_eq!(index.search("routing", &filters, 10).len(), 1);
        assert_eq!(index.search("main-spec", &filters, 10).len(), 1);
        assert_eq!(index.search("body", &filters, 10).len(), 1);
    }

    #[tokio::test]
    async fn existing_knowledge_still_found_after_task_extension() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        tokio::fs::write(
            knowledge_dir.join("active.md"),
            "---\ntitle: Knowledge Card\ntags: [stable]\n---\n\nEvergreen content",
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(dir.path().join(".refact/tasks/task-1/memories"))
            .await
            .unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        let index = build_knowledge_index(gcx).await;
        assert_eq!(
            index.related_for_tags(&vec!["stable".to_string()], 5).len(),
            1
        );
        assert_eq!(
            index
                .search("evergreen", &KnowledgeSearchFilters::default(), 5)
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn build_knowledge_index_uses_spawn_blocking_for_walk() {
        let dir = tempfile::tempdir().unwrap();
        let memories_dir = dir.path().join(".refact/tasks/T-1/memories");
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();
        for idx in 0..200 {
            tokio::fs::write(
                memories_dir.join(format!("memory-{idx}.md")),
                format!(
                    "---\ntitle: Memory {idx}\nkind: finding\n---\n\nlarge synthetic tree token {idx}"
                ),
            )
            .await
            .unwrap();
        }

        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        let index = build_knowledge_index(gcx).await;
        let hits = index.search(
            "synthetic",
            &KnowledgeSearchFilters {
                scope: Some("task".to_string()),
                task_id: Some("T-1".to_string()),
                ..Default::default()
            },
            250,
        );

        assert_eq!(hits.len(), 200);
    }

    #[tokio::test]
    async fn task_index_excludes_archived_superseded_history_and_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let memories_dir = dir.path().join(".refact/tasks/task-1/memories");
        tokio::fs::create_dir_all(memories_dir.join(".history"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(memories_dir.join("archived"))
            .await
            .unwrap();
        tokio::fs::write(
            memories_dir.join("active.md"),
            "---\ntitle: Active\ntask_id: task-1\nkind: finding\n---\n\nneedle active",
        )
        .await
        .unwrap();
        tokio::fs::write(
            memories_dir.join("superseded.md"),
            "---\nstatus: superseded\ntags: [needle]\n---\n\nneedle superseded",
        )
        .await
        .unwrap();
        tokio::fs::write(memories_dir.join("archived/old.md"), "needle archived")
            .await
            .unwrap();
        tokio::fs::write(memories_dir.join(".history/old.md"), "needle history")
            .await
            .unwrap();
        tokio::fs::write(memories_dir.join("draft.md.tmp"), "needle tmp")
            .await
            .unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        let index = build_knowledge_index(gcx).await;
        let filters = KnowledgeSearchFilters {
            scope: Some("task".to_string()),
            task_id: Some("task-1".to_string()),
            ..Default::default()
        };

        let hits = index.search("needle", &filters, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].card.title, "Active");
    }

    #[tokio::test]
    async fn knowledge_index_skips_archived_dir_memories() {
        let dir = tempfile::tempdir().unwrap();
        let memories_dir = dir.path().join(".refact/tasks/T-1/memories");
        let active_path = memories_dir.join("active.md");
        let archived_path = memories_dir.join("archived/old.md");
        tokio::fs::create_dir_all(memories_dir.join("archived"))
            .await
            .unwrap();
        tokio::fs::write(
            &active_path,
            "---\ntitle: Active\ntask_id: T-1\nkind: finding\n---\n\nactive body",
        )
        .await
        .unwrap();
        tokio::fs::write(
            &archived_path,
            "---\ntitle: Old\ntask_id: T-1\nkind: finding\n---\n\nold body",
        )
        .await
        .unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        let indexed_paths = build_knowledge_index(gcx)
            .await
            .all_cards()
            .into_iter()
            .map(|card| card.file_path)
            .collect::<Vec<_>>();

        let indexed_paths = indexed_paths
            .into_iter()
            .map(crate::files_correction::canonicalize_normalized_path)
            .collect::<Vec<_>>();
        assert!(
            indexed_paths.contains(&crate::files_correction::canonicalize_normalized_path(
                active_path
            ))
        );
        assert!(
            !indexed_paths.contains(&crate::files_correction::canonicalize_normalized_path(
                archived_path
            ))
        );
    }

    #[tokio::test]
    async fn knowledge_index_skips_status_archived_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let memories_dir = dir.path().join(".refact/tasks/T-1/memories");
        let active_path = memories_dir.join("active.md");
        let old_path = memories_dir.join("old.md");
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();
        tokio::fs::write(
            &active_path,
            "---\ntitle: Active\ntask_id: T-1\nkind: finding\n---\n\nactive body",
        )
        .await
        .unwrap();
        tokio::fs::write(
            &old_path,
            "---\ntitle: Old\ntask_id: T-1\nkind: finding\nstatus: archived\n---\n\nold body",
        )
        .await
        .unwrap();

        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        let indexed_paths = build_knowledge_index(gcx)
            .await
            .all_cards()
            .into_iter()
            .map(|card| card.file_path)
            .collect::<Vec<_>>();

        let indexed_paths = indexed_paths
            .into_iter()
            .map(crate::files_correction::canonicalize_normalized_path)
            .collect::<Vec<_>>();
        assert!(
            indexed_paths.contains(&crate::files_correction::canonicalize_normalized_path(
                active_path
            ))
        );
        assert!(
            !indexed_paths.contains(&crate::files_correction::canonicalize_normalized_path(
                old_path
            ))
        );
    }

    #[tokio::test]
    async fn indexes_workspace_under_absolute_archive_parent() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("archive/project");
        let knowledge_dir = workspace.join(KNOWLEDGE_FOLDER_NAME);
        let memories_dir = workspace.join(".refact/tasks/task-1/memories");
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        tokio::fs::create_dir_all(&memories_dir).await.unwrap();
        tokio::fs::write(
            knowledge_dir.join("knowledge.md"),
            "---\ntitle: Absolute Archive Knowledge\ntags: [absolute-archive]\n---\n\nknowledge needle",
        )
        .await
        .unwrap();
        tokio::fs::write(
            memories_dir.join("memory.md"),
            "---\ntitle: Absolute Archive Memory\ntask_id: task-1\nkind: finding\n---\n\nmemory needle",
        )
        .await
        .unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace];

        let index = build_knowledge_index(gcx).await;

        assert_eq!(
            index
                .search("knowledge", &KnowledgeSearchFilters::default(), 10)
                .len(),
            1
        );
        assert_eq!(
            index
                .search(
                    "memory",
                    &KnowledgeSearchFilters {
                        scope: Some("task".to_string()),
                        task_id: Some("task-1".to_string()),
                        ..Default::default()
                    },
                    10,
                )
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn task_index_skips_relative_archive_and_history_document_roots() {
        let dir = tempfile::tempdir().unwrap();
        let task_dir = dir.path().join(".refact/tasks/task-1");
        let memories_dir = task_dir.join("memories");
        let documents_dir = task_dir.join("documents");
        tokio::fs::create_dir_all(memories_dir.join("archived"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(documents_dir.join(".history"))
            .await
            .unwrap();
        tokio::fs::write(
            memories_dir.join("active.md"),
            "---\ntitle: Active\ntask_id: task-1\nkind: finding\n---\n\nneedle active",
        )
        .await
        .unwrap();
        tokio::fs::write(
            memories_dir.join("archived/old.md"),
            "---\ntitle: Archived\ntask_id: task-1\nkind: finding\n---\n\nneedle archived",
        )
        .await
        .unwrap();
        tokio::fs::write(
            documents_dir.join(".history/old.md"),
            "---\ntitle: History\nkind: spec\n---\n\nneedle history",
        )
        .await
        .unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];

        let index = build_knowledge_index(gcx).await;
        let hits = index.search(
            "needle",
            &KnowledgeSearchFilters {
                scope: Some("task".to_string()),
                task_id: Some("task-1".to_string()),
                ..Default::default()
            },
            10,
        );

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].card.title, "Active");
    }
}
