pub use refact_runtime_api::memories_runtime::{
    delete_document_from_disk, delete_document_from_disk_for_ctx, deprecate_document,
    evict_stale_index_path, evict_stale_index_path_for_ctx, find_duplicate_memory_for_ctx,
    get_all_knowledge_dirs, get_first_knowledge_dir_for_ctx, get_global_knowledge_dir,
    load_memories_by_tags, load_memories_by_tags_from_index, memory_plane_roots_for_ctx,
    memories_add, memories_add_dedup, memories_add_dedup_for_ctx, memories_add_for_ctx,
    memories_search_for_app, memories_upsert_by_signal_key,
    memories_upsert_by_signal_key_for_ctx, path_contains_component, rewrite_memory_document,
    rewrite_memory_document_for_ctx, touch_memory_observation,
    touch_memory_observation_for_ctx,
};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::memories::format::{
        compute_content_hash_hex, create_frontmatter, extract_fallback_tags,
        normalize_memory_tags, normalize_preference_text_for_dedupe, preference_statement_is_safe,
    };
    use crate::file_filter::KNOWLEDGE_FOLDER_NAME;
    use refact_core::knowledge_frontmatter::KnowledgeFrontmatter;
    use refact_knowledge_graph::memo::MemoRecord;
    use walkdir::WalkDir;

    const PREFERENCE_STATEMENT_MAX_CHARS: usize = 240;
    const PREFERENCE_EVIDENCE_MAX_CHARS: usize = 600;

    fn redact_and_cap_preference_text(text: &str, max_chars: usize) -> String {
        let redacted = refact_core::string_utils::redact_sensitive(text);
        let collapsed = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
        refact_core::string_utils::safe_truncate(&collapsed, max_chars)
            .trim()
            .to_string()
    }

    fn preference_matches_existing(existing: &MemoRecord, normalized_statement: &str) -> bool {
        let mut candidates = vec![existing.content.as_str()];
        if let Some(title) = existing.title.as_deref() {
            candidates.push(title);
        }
        candidates.into_iter().any(|text| {
            let normalized = normalize_preference_text_for_dedupe(text);
            let normalized_without_heading = normalized.strip_prefix("# ").unwrap_or(&normalized);
            normalized == normalized_statement
                || normalized_without_heading == normalized_statement
                || normalized.contains(normalized_statement)
                || normalized_statement.contains(&normalized)
                || normalized_statement.contains(normalized_without_heading)
        })
    }

    fn preference_file_already_exists(knowledge_dirs: &[PathBuf], normalized_statement: &str) -> bool {
        knowledge_dirs.iter().any(|knowledge_dir| {
            if !knowledge_dir.exists() {
                return false;
            }
            WalkDir::new(knowledge_dir)
                .into_iter()
                .filter_map(|entry| entry.ok())
                .any(|entry| {
                    let path = entry.path();
                    if !path.is_file() || path_contains_component(path, "archive") {
                        return false;
                    }
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if ext != "md" && ext != "mdx" {
                        return false;
                    }
                    let Ok(text) = std::fs::read_to_string(path) else {
                        return false;
                    };
                    let (frontmatter, content_start) = KnowledgeFrontmatter::parse(&text);
                    if frontmatter.is_archived()
                        || frontmatter.is_deprecated()
                        || !frontmatter
                            .tags
                            .iter()
                            .any(|tag| tag.to_lowercase().contains("preference"))
                    {
                        return false;
                    }
                    let content = text[content_start..].trim();
                    let record = MemoRecord {
                        memid: path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default()
                            .to_string(),
                        tags: frontmatter.tags,
                        content: content.to_string(),
                        file_path: Some(path.to_path_buf()),
                        line_range: None,
                        title: frontmatter.title,
                        created: frontmatter.created,
                        kind: frontmatter.kind,
                        score: None,
                    };
                    preference_matches_existing(&record, normalized_statement)
                })
        })
    }

    async fn memories_add_preference_if_new(
        gcx: std::sync::Arc<crate::global_context::GlobalContext>,
        statement: &str,
        evidence: &str,
        confidence: f32,
    ) -> Result<Option<PathBuf>, String> {
        if !preference_statement_is_safe(statement, confidence) {
            return Ok(None);
        }

        let statement = redact_and_cap_preference_text(statement, PREFERENCE_STATEMENT_MAX_CHARS);
        let evidence = redact_and_cap_preference_text(evidence, PREFERENCE_EVIDENCE_MAX_CHARS);
        let normalized_statement = normalize_preference_text_for_dedupe(&statement);
        if normalized_statement.is_empty() {
            return Ok(None);
        }

        let existing = load_memories_by_tags(&gcx, &["preference"], 200).await?;
        if existing
            .iter()
            .any(|memory| preference_matches_existing(memory, &normalized_statement))
        {
            return Ok(None);
        }

        let knowledge_dirs = get_all_knowledge_dirs(&gcx).await;
        if preference_file_already_exists(&knowledge_dirs, &normalized_statement) {
            return Ok(None);
        }

        let tags = vec![
            "preference".to_string(),
            "buddy".to_string(),
            "behavior_learner".to_string(),
        ];
        let empty = Vec::<String>::new();
        let mut frontmatter =
            create_frontmatter(Some(&statement), &tags, &empty, &empty, "preference");
        frontmatter.summary = Some(statement.clone());
        if !evidence.is_empty() {
            frontmatter.description = Some(evidence.clone());
        }
        frontmatter.content_hash = Some(compute_content_hash_hex(&statement));
        frontmatter.source_tool = Some("buddy_behavior_learner".to_string());

        let content = if evidence.is_empty() {
            format!("# {}\n\nConfidence: {:.2}", statement, confidence)
        } else {
            format!(
                "# {}\n\nEvidence: {}\n\nConfidence: {:.2}",
                statement, evidence, confidence
            )
        };

        memories_add(&gcx, &frontmatter, &content).await.map(Some)
    }

    #[test]
    fn preference_statement_validation_rejects_low_confidence_vague_and_sensitive() {
        assert!(preference_statement_is_safe(
            "I prefer concise answers with bullet lists",
            0.90
        ));
        assert!(!preference_statement_is_safe(
            "I prefer concise answers with bullet lists",
            0.80
        ));
        assert!(!preference_statement_is_safe("remember this", 0.95));
        assert!(!preference_statement_is_safe("I prefer token=secret", 0.95));
    }

    #[test]
    fn normalized_preference_dedupe_matches_case_and_punctuation() {
        let first = normalize_preference_text_for_dedupe("I prefer concise answers, with bullets!");
        let second = normalize_preference_text_for_dedupe("i prefer concise answers with bullets");

        assert_eq!(first, second);
    }

    #[test]
    fn memory_tags_are_namespaced_and_normalized() {
        let tags = normalize_memory_tags(
            &[
                "Rust".to_string(),
                "behavior_learner".to_string(),
                " Playwright MCP ".to_string(),
                "component:Buddy Scheduler".to_string(),
                "Rust".to_string(),
            ],
            16,
        );

        assert_eq!(
            tags,
            vec![
                "language:rust".to_string(),
                "workflow:behavior-learner".to_string(),
                "tool:playwright-mcp".to_string(),
                "component:buddy-scheduler".to_string(),
            ]
        );
    }

    #[test]
    fn fallback_tags_extract_precise_kg_linking_signals() {
        let tags = extract_fallback_tags(
            "Buddy autonomous background chat should reuse subchat and save_trajectory_as. Run cargo check and git status.",
            &[
                "refact-agent/engine/src/buddy/jobs.rs".to_string(),
                "refact-agent/engine/src/subchat.rs".to_string(),
            ],
            &["save_trajectory_as".to_string()],
        );

        assert!(tags.contains(&"domain:buddy".to_string()));
        assert!(tags.contains(&"workflow:background-chat".to_string()));
        assert!(tags.contains(&"component:subchat".to_string()));
        assert!(tags.contains(&"component:trajectory-persistence".to_string()));
        assert!(tags.contains(&"component:buddy".to_string()));
        assert!(tags.contains(&"language:rust".to_string()));
        assert!(tags.contains(&"verification:cargo-check".to_string()));
        assert!(tags.contains(&"verification:git-sanity".to_string()));
        assert!(tags.contains(&"entity:save_trajectory_as".to_string()));
    }

    fn dedup_test_frontmatter(title: &str) -> KnowledgeFrontmatter {
        let tags = vec!["buddy".to_string(), "diagnostics".to_string()];
        let empty = Vec::<String>::new();
        create_frontmatter(Some(title), &tags, &empty, &empty, "insight")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_add_dedup_touches_exact_normalized_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        let first = memories_add_dedup(
            &gcx,
            &dedup_test_frontmatter("Signal insight"),
            "Signal hash fd0347ef seen 43 times at 2026-07-01T10:00:00Z in chat generation",
        )
        .await
        .unwrap();
        assert!(first.created());

        let second = memories_add_dedup(
            &gcx,
            &dedup_test_frontmatter("Signal insight repeat"),
            "Signal hash ab99cd11 seen 57 times at 2026-07-02T08:30:00Z in chat generation",
        )
        .await
        .unwrap();

        assert!(!second.created());
        assert_eq!(second.path(), first.path());
        let text = tokio::fs::read_to_string(second.path()).await.unwrap();
        let (frontmatter, _) = KnowledgeFrontmatter::parse(&text);
        assert!(frontmatter.occurrences >= 2);
        assert!(frontmatter.last_observed.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_add_dedup_touches_near_duplicate_with_shared_tag() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        let first = memories_add_dedup(
            &gcx,
            &dedup_test_frontmatter("Caps drift"),
            "Pattern observed: repeated llm_error diagnostics clustered at chat generation, \
             mostly caps-registry drift where the default model id is no longer served by \
             the server. Fix is in user config, update default models.",
        )
        .await
        .unwrap();
        assert!(first.created());

        let second = memories_add_dedup(
            &gcx,
            &dedup_test_frontmatter("Caps drift again"),
            "Pattern observed: repeated llm_error diagnostics clustered at chat generation, \
             mostly caps-registry drift where the default model id is no longer served by \
             the server. Fix is in user config, update the default models slot.",
        )
        .await
        .unwrap();

        assert!(!second.created());
        assert_eq!(second.path(), first.path());

        let distinct = memories_add_dedup(
            &gcx,
            &dedup_test_frontmatter("Voice cache"),
            "Voice rendering caches the pulse one-liner for five minutes which replays \
             identical lines when the pulse is quiet, needs a said-recently ring.",
        )
        .await
        .unwrap();
        assert!(distinct.created());
        assert_ne!(distinct.path(), first.path());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_add_dedup_recovers_from_stale_index_hint() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }
        let content =
            "Stale hint recovery memo body with enough distinctive tokens to index cleanly.";

        let first = memories_add_dedup(&gcx, &dedup_test_frontmatter("Stale"), content)
            .await
            .unwrap();
        assert!(first.created());
        tokio::fs::remove_file(first.path()).await.unwrap();

        let second = memories_add_dedup(&gcx, &dedup_test_frontmatter("Stale"), content)
            .await
            .unwrap();
        assert!(second.created());
        assert!(tokio::fs::try_exists(second.path()).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_upsert_signal_key_survives_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        let first = memories_upsert_by_signal_key(
            &gcx,
            "diag:signal:stale00",
            &dedup_test_frontmatter("Stale signal"),
            "Original signal body that will disappear from disk before the next upsert.",
        )
        .await
        .unwrap();
        assert!(first.created());
        tokio::fs::remove_file(first.path()).await.unwrap();

        let second = memories_upsert_by_signal_key(
            &gcx,
            "diag:signal:stale00",
            &dedup_test_frontmatter("Stale signal refreshed"),
            "Replacement body recreated after the original file vanished from disk.",
        )
        .await
        .unwrap();
        assert!(second.created());
        let text = tokio::fs::read_to_string(second.path()).await.unwrap();
        assert!(text.contains("signal_key: \"diag:signal:stale00\""));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_upsert_by_signal_key_rewrites_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        let first = memories_upsert_by_signal_key(
            &gcx,
            "diag:signal:fd0347ef",
            &dedup_test_frontmatter("Detective finding"),
            "First investigation body with initial conclusions about the failure cluster.",
        )
        .await
        .unwrap();
        assert!(first.created());

        let second = memories_upsert_by_signal_key(
            &gcx,
            "diag:signal:fd0347ef",
            &dedup_test_frontmatter("Detective finding refreshed"),
            "Updated investigation body superseding stale conclusions with fresh evidence.",
        )
        .await
        .unwrap();

        assert!(!second.created());
        assert_eq!(second.path(), first.path());
        let text = tokio::fs::read_to_string(second.path()).await.unwrap();
        let (frontmatter, content_start) = KnowledgeFrontmatter::parse(&text);
        assert_eq!(frontmatter.occurrences, 2);
        assert_eq!(
            frontmatter.signal_key.as_deref(),
            Some("diag:signal:fd0347ef")
        );
        assert!(text[content_start..].contains("Updated investigation body"));
        assert!(!text[content_start..].contains("First investigation body"));

        let files = std::fs::read_dir(dir.path().join(KNOWLEDGE_FOLDER_NAME))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
            .count();
        assert_eq!(files, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memories_add_preference_if_new_dedupes_existing_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        let first = memories_add_preference_if_new(
            gcx.clone(),
            "I prefer concise answers with bullet lists",
            "User-authored snippet",
            0.91,
        )
        .await
        .unwrap();
        let duplicate = memories_add_preference_if_new(
            gcx,
            "i prefer concise answers with bullet lists.",
            "Repeated user-authored snippet",
            0.95,
        )
        .await
        .unwrap();

        assert!(first.is_some());
        assert!(duplicate.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deprecate_document_marks_inactive_without_deleting_file() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("memory.md");
        let mut frontmatter = create_frontmatter(
            Some("Memory"),
            &["old".to_string()],
            &Vec::new(),
            &Vec::new(),
            "domain",
        );
        frontmatter.id = Some("old-doc".to_string());
        let body = "Original body";
        tokio::fs::write(&path, format!("{}\n\n{}", frontmatter.to_yaml(), body))
            .await
            .unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() = vec![dir.path().to_path_buf()];
        }

        deprecate_document(
            &gcx,
            &path,
            Some("new-doc"),
            "superseded by canonical memory",
        )
        .await
        .unwrap();

        assert!(path.exists());
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let (updated, content_start) = KnowledgeFrontmatter::parse(&text);
        assert_eq!(updated.status.as_deref(), Some("deprecated"));
        assert_eq!(updated.superseded_by.as_deref(), Some("new-doc"));
        assert_eq!(text[content_start..].trim(), body);
        let found = load_memories_by_tags(&gcx, &["old"], 10).await.unwrap();
        assert!(found.is_empty());
    }

}
