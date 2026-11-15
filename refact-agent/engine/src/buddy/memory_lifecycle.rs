pub use refact_runtime_api::buddy_memory_lifecycle::*;
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use chrono::{DateTime, Utc};

    use crate::app_state::AppState;
    use crate::file_filter::KNOWLEDGE_FOLDER_NAME;
    use crate::git::operations::{
        GitCoChangePair, GitCommitClassification, GitCommitSummary, GitFileChangeStatus,
        GitHistoryReport, GitHotspot,
    };
    use crate::knowledge_graph::kg_structs::KnowledgeFrontmatter;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn test_op(op_id: &str, evidence: &str, status: MemoryOpStatus) -> MemoryLifecycleOp {
        let mut op = MemoryLifecycleOp::pending(
            op_id,
            MemorySource::MemoryGarden,
            MemoryOpType::CreateMemory,
            strings(&[".refact/knowledge/item.md"]),
            evidence,
            0.91,
            "2026-05-02T00:00:00Z",
        );
        op.status = status;
        op
    }

    fn auto_apply_test_op(
        op_type: MemoryOpType,
        confidence: f32,
        evidence: &str,
    ) -> MemoryLifecycleOp {
        let mut op = MemoryLifecycleOp::pending(
            "op-auto",
            MemorySource::MemoryGarden,
            op_type,
            strings(&[".refact/knowledge/item.md"]),
            evidence,
            confidence,
            "2026-05-02T00:00:00Z",
        );
        op.requires_approval = false;
        op.payload.canonical = Some(MemoryCreatePayload {
            content: "Canonical merged body with plenty of alphabetic text.".to_string(),
            ..Default::default()
        });
        op
    }

    #[test]
    fn exact_duplicate_merge_op_is_auto_apply_eligible() {
        let op = auto_apply_test_op(
            MemoryOpType::MergeArchive,
            MEMORY_OP_AUTO_APPLY_MIN_CONFIDENCE,
            &format!(
                "{}: canonical=a, superseded=b",
                MEMORY_OP_EXACT_DUPLICATE_REASON
            ),
        );

        assert!(memory_op_auto_apply_eligible(&op));
    }

    #[test]
    fn auto_apply_eligibility_rejects_lower_confidence() {
        let op = auto_apply_test_op(
            MemoryOpType::MergeArchive,
            MEMORY_OP_AUTO_APPLY_MIN_CONFIDENCE - 0.01,
            &format!(
                "{}: canonical=a, superseded=b",
                MEMORY_OP_EXACT_DUPLICATE_REASON
            ),
        );

        assert!(!memory_op_auto_apply_eligible(&op));
    }

    #[test]
    fn auto_apply_eligibility_rejects_non_merge_archive() {
        let op = auto_apply_test_op(
            MemoryOpType::ArchiveCandidate,
            MEMORY_OP_AUTO_APPLY_MIN_CONFIDENCE,
            &format!(
                "{}: canonical=a, superseded=b",
                MEMORY_OP_EXACT_DUPLICATE_REASON
            ),
        );

        assert!(!memory_op_auto_apply_eligible(&op));
    }

    #[test]
    fn auto_apply_eligibility_rejects_different_evidence_prefix() {
        let op = auto_apply_test_op(
            MemoryOpType::MergeArchive,
            MEMORY_OP_AUTO_APPLY_MIN_CONFIDENCE,
            "near duplicate: canonical=a, superseded=b",
        );

        assert!(!memory_op_auto_apply_eligible(&op));
    }

    #[test]
    fn auto_apply_eligibility_rejects_degenerate_canonical_body() {
        let mut op = auto_apply_test_op(
            MemoryOpType::MergeArchive,
            MEMORY_OP_AUTO_APPLY_MIN_CONFIDENCE,
            &format!(
                "{}: canonical=a, superseded=b",
                MEMORY_OP_EXACT_DUPLICATE_REASON
            ),
        );
        op.payload.canonical = Some(MemoryCreatePayload {
            content: "247".to_string(),
            ..Default::default()
        });

        assert!(!memory_op_auto_apply_eligible(&op));

        op.payload.canonical = None;
        assert!(!memory_op_auto_apply_eligible(&op));
    }

    fn legacy_test_op(op_id: &str, evidence: &str, status: MemoryOpStatus) -> MemoryLifecycleOp {
        let mut op = MemoryLifecycleOp::default();
        op.op_id = op_id.to_string();
        op.source = MemorySource::MemoryGarden;
        op.op_type = MemoryOpType::CreateMemory;
        op.target_paths = strings(&[".refact/knowledge/item.md"]);
        op.evidence = evidence.to_string();
        op.confidence = 0.91;
        op.requires_approval = false;
        op.status = status;
        op.created_at = "2026-05-02T00:00:00Z".to_string();
        op
    }

    fn explicit_key_test_op(op_id: &str, key: &str, status: MemoryOpStatus) -> MemoryLifecycleOp {
        let mut op = test_op(op_id, key, status);
        op.idempotency_key = key.to_string();
        op
    }

    fn assert_no_raw_secret(text: &str) {
        assert!(!text.contains("password=secret"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("ghp_AbCdEfGhIj1234567890"));
    }

    async fn test_gcx_with_workspace(dir: &Path) -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *crate::app_state::app_state_from_gcx(gcx.clone())
                .await
                .workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.to_path_buf()];
        }
        crate::app_state::app_state_from_gcx(gcx).await
    }

    fn frontmatter_and_body(text: &str) -> (KnowledgeFrontmatter, String) {
        let (frontmatter, content_start) = KnowledgeFrontmatter::parse(text);
        (frontmatter, text[content_start..].trim().to_string())
    }

    async fn write_memory_file(path: &Path, frontmatter: KnowledgeFrontmatter, body: &str) {
        tokio::fs::write(path, format!("{}\n\n{}", frontmatter.to_yaml(), body))
            .await
            .unwrap();
    }

    fn active_frontmatter(title: &str, tags: &[&str]) -> KnowledgeFrontmatter {
        KnowledgeFrontmatter {
            id: Some(title.to_string()),
            title: Some(title.to_string()),
            status: Some("active".to_string()),
            tags: strings(tags),
            created: Some("2026-05-02".to_string()),
            updated: Some("2026-05-02".to_string()),
            kind: Some("domain".to_string()),
            ..Default::default()
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn snapshot(
        id: &str,
        title: &str,
        content: &str,
        tags: &[&str],
        files: &[&str],
        status: &str,
        source_class: MemorySourceClass,
    ) -> MemoryDocSnapshot {
        MemoryDocSnapshot {
            id: id.to_string(),
            path: format!("/tmp/{id}.md"),
            title: title.to_string(),
            content: content.to_string(),
            tags: strings(tags),
            filenames: strings(files),
            status: status.to_string(),
            kind: "domain".to_string(),
            source_class: Some(source_class),
            source_confidence: Some(0.8),
            content_hash: compute_content_hash(content),
            created_at: Some("2026-04-01T00:00:00Z".to_string()),
            review_after: Some("2026-08-01".to_string()),
            ..Default::default()
        }
        .normalized()
    }

    fn op_types(ops: &[MemoryLifecycleOp]) -> Vec<MemoryOpType> {
        ops.iter().map(|op| op.op_type).collect()
    }

    fn git_commit(
        oid: &str,
        message: &str,
        classifications: Vec<GitCommitClassification>,
        changes: Vec<crate::git::operations::GitCommitFileChange>,
    ) -> GitCommitSummary {
        GitCommitSummary {
            oid: oid.to_string(),
            short_oid: oid.chars().take(12).collect(),
            time: fixed_now(),
            parent_oids: Vec::new(),
            message: message.to_string(),
            classifications,
            changes,
            file_cap_hit: false,
        }
    }

    fn git_change(path: &str) -> crate::git::operations::GitCommitFileChange {
        crate::git::operations::GitCommitFileChange {
            path: path.to_string(),
            old_path: None,
            status: GitFileChangeStatus::Modified,
            additions: 1,
            deletions: 1,
            binary: false,
        }
    }

    fn git_hotspot(
        path: &str,
        edit_count: usize,
        additions: usize,
        deletions: usize,
        score: u64,
        latest_commit: &str,
    ) -> GitHotspot {
        GitHotspot {
            path: path.to_string(),
            edit_count,
            additions,
            deletions,
            score,
            latest_commit: latest_commit.to_string(),
        }
    }

    fn git_cochange(path_a: &str, path_b: &str, count: usize, commits: &[&str]) -> GitCoChangePair {
        GitCoChangePair {
            path_a: path_a.to_string(),
            path_b: path_b.to_string(),
            count,
            commits: strings(commits),
        }
    }

    #[test]
    fn usefulness_score_monotonicity_prefers_pinned_active_and_proposed_over_stale_duplicate() {
        let now = fixed_now();
        let base = MemoryScoreInput {
            created_at: Some("2026-04-01T00:00:00Z".to_string()),
            source_confidence: Some(0.9),
            tag_overlap: 2,
            ..Default::default()
        };
        let pinned = MemoryScoreInput {
            status: "pinned".to_string(),
            source_class: MemorySourceClass::UserAuthored,
            ..base.clone()
        };
        let active = MemoryScoreInput {
            status: "active".to_string(),
            source_class: MemorySourceClass::AutoGenerated,
            ..base.clone()
        };
        let proposed = MemoryScoreInput {
            status: "proposed".to_string(),
            source_class: MemorySourceClass::AutoGenerated,
            ..base.clone()
        };
        let stale_duplicate = MemoryScoreInput {
            status: "proposed".to_string(),
            source_class: MemorySourceClass::AutoGenerated,
            source_confidence: Some(0.4),
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
            review_after: Some("2024-06-01".to_string()),
            dismissed_count: 4,
            duplicate_penalty: 0.35,
            ..MemoryScoreInput::default()
        };

        let pinned_score = score_memory_usefulness(&pinned, now).score;
        let active_score = score_memory_usefulness(&active, now).score;
        let proposed_score = score_memory_usefulness(&proposed, now).score;
        let stale_duplicate_score = score_memory_usefulness(&stale_duplicate, now).score;

        assert!(pinned_score > active_score);
        assert!(active_score > proposed_score);
        assert!(proposed_score > stale_duplicate_score);
    }

    #[test]
    fn detects_duplicate_by_exact_content_hash() {
        let first = snapshot(
            "a",
            "Memory A",
            "Same body",
            &["buddy"],
            &["src/lib.rs"],
            "active",
            MemorySourceClass::AutoGenerated,
        );
        let mut second = snapshot(
            "b",
            "Memory B",
            "Different spelling",
            &["buddy"],
            &["src/lib.rs"],
            "proposed",
            MemorySourceClass::AutoGenerated,
        );
        second.content_hash = first.content_hash.clone();

        let ops = detect_memory_lifecycle_ops(&[second, first], fixed_now());

        assert!(ops.iter().any(|op| {
            op.op_type == MemoryOpType::MergeArchive
                && op.evidence.contains("exact content_hash duplicate")
        }));
    }

    #[test]
    fn detects_duplicate_by_normalized_title_tags_and_files() {
        let first = snapshot(
            "a",
            "Use Cargo Check",
            "First body",
            &["rust", "buddy"],
            &["refact-agent/engine/src/lib.rs"],
            "active",
            MemorySourceClass::AutoGenerated,
        );
        let second = snapshot(
            "b",
            "use cargo-check",
            "Second body",
            &["buddy"],
            &["refact-agent/engine/src/lib.rs"],
            "proposed",
            MemorySourceClass::AutoGenerated,
        );

        let ops = detect_memory_lifecycle_ops(&[second, first], fixed_now());

        assert!(ops.iter().any(|op| {
            op.op_type == MemoryOpType::MergeArchive
                && op
                    .evidence
                    .contains("same normalized title with overlapping tags/files/entities")
        }));
    }

    #[test]
    fn conflict_precedence_pinned_user_memory_beats_auto_generated_contradiction() {
        let pinned = snapshot(
            "pinned",
            "Use pnpm",
            "User says use pnpm",
            &["package"],
            &[],
            "pinned",
            MemorySourceClass::UserAuthored,
        );
        let auto = snapshot(
            "auto",
            "Avoid pnpm",
            "Generated old advice",
            &["package"],
            &[],
            "proposed",
            MemorySourceClass::AutoGenerated,
        );

        let ops = detect_memory_lifecycle_ops(&[auto, pinned], fixed_now());
        let conflict = ops
            .iter()
            .find(|op| op.evidence.contains("conflict candidate"))
            .expect("conflict op");

        assert_eq!(conflict.op_type, MemoryOpType::MarkReviewNeeded);
        assert_eq!(conflict.target_paths, strings(&["/tmp/auto.md"]));
        assert!(conflict
            .evidence
            .contains("pinned takes precedence over auto"));
    }

    #[test]
    fn usage_metadata_update_tracks_use_injection_and_dismissal() {
        let now = fixed_now();
        let mut frontmatter = active_frontmatter("memory", &["buddy"]);

        assert!(record_memory_usage_metadata(
            &mut frontmatter,
            now,
            true,
            false
        ));
        assert_eq!(frontmatter.use_count, 1);
        assert_eq!(
            frontmatter.last_used_at.as_deref(),
            Some(now.to_rfc3339().as_str())
        );
        assert_eq!(
            frontmatter.last_injected_at.as_deref(),
            Some(now.to_rfc3339().as_str())
        );
        assert!(record_memory_usage_metadata(
            &mut frontmatter,
            now,
            false,
            true
        ));
        assert_eq!(frontmatter.dismissed_count, 1);
        assert_eq!(frontmatter.use_count, 1);
    }

    #[test]
    fn usage_metadata_update_reports_same_timestamp_use_count_change() {
        let now = fixed_now();
        let mut frontmatter = active_frontmatter("memory", &["buddy"]);

        assert!(record_memory_usage_metadata(
            &mut frontmatter,
            now,
            false,
            false
        ));
        assert!(record_memory_usage_metadata(
            &mut frontmatter,
            now,
            false,
            false
        ));

        assert_eq!(frontmatter.use_count, 2);
        assert_eq!(
            frontmatter.last_used_at.as_deref(),
            Some(now.to_rfc3339().as_str())
        );
    }

    #[test]
    fn candidate_output_order_is_deterministic() {
        let a = snapshot(
            "b",
            "Use Y",
            "Body b",
            &["tag"],
            &["file.rs"],
            "proposed",
            MemorySourceClass::AutoGenerated,
        );
        let b = snapshot(
            "a",
            "Use Y",
            "Body a",
            &["tag"],
            &["file.rs"],
            "active",
            MemorySourceClass::AutoGenerated,
        );
        let c = snapshot(
            "c",
            "Avoid Z",
            "Body c",
            &["tag"],
            &[],
            "proposed",
            MemorySourceClass::AutoGenerated,
        );
        let d = snapshot(
            "d",
            "Use Z",
            "Body d",
            &["tag"],
            &[],
            "active",
            MemorySourceClass::AutoGenerated,
        );

        let first =
            detect_memory_lifecycle_ops(&[a.clone(), b.clone(), c.clone(), d.clone()], fixed_now());
        let second = detect_memory_lifecycle_ops(&[d, c, b, a], fixed_now());

        assert_eq!(op_types(&first), op_types(&second));
        assert_eq!(
            first
                .iter()
                .map(|op| op.idempotency_key.clone())
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|op| op.idempotency_key.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn git_bugfix_commit_creates_lesson_candidate_with_source_sha() {
        let commit = git_commit(
            "abcdef1234567890abcdef1234567890abcdef12",
            "fix parser bug because newline crash",
            vec![
                GitCommitClassification::Bugfix,
                GitCommitClassification::Rationale,
            ],
            vec![git_change("src/parser.rs")],
        );
        let report = GitHistoryReport {
            commits: vec![commit.clone()],
            cochanges: Vec::new(),
            hotspots: Vec::new(),
            commit_cap_hit: false,
        };

        let ops = detect_git_memory_ops(&report, &[], fixed_now());
        let lesson = ops
            .iter()
            .find(|op| {
                op.op_type == MemoryOpType::CreateMemory
                    && op
                        .payload
                        .canonical
                        .as_ref()
                        .map(|payload| payload.kind.as_str() == "lesson")
                        .unwrap_or(false)
            })
            .expect("lesson op");
        let payload = lesson.payload.canonical.as_ref().unwrap();

        assert_eq!(lesson.source, MemorySource::Git);
        assert_eq!(payload.source_commit.as_deref(), Some(commit.oid.as_str()));
        assert_eq!(payload.status.as_deref(), Some("proposed"));
        assert!(payload.content.contains(&commit.short_oid));
    }

    #[test]
    fn git_hotspot_identity_ignores_changed_metrics_and_evidence() {
        let first = git_hotspot_create_op(
            &git_hotspot("src/hot.rs", 3, 10, 5, 100, "aaaaaaaaaaaa"),
            fixed_now(),
        );
        let second = git_hotspot_create_op(
            &git_hotspot("src//hot.rs", 9, 120, 40, 400, "bbbbbbbbbbbb"),
            fixed_now(),
        );

        assert_eq!(first.op_id, second.op_id);
        assert_eq!(first.idempotency_key, second.idempotency_key);
        assert_ne!(first.evidence, second.evidence);
    }

    #[test]
    fn git_cochange_identity_ignores_changed_count_commit_list_and_evidence() {
        let first = git_cochange_create_op(
            &git_cochange("src/a.rs", "src/b.rs", 3, &["aaaaaaaaaaaa", "bbbbbbbbbbbb"]),
            fixed_now(),
        );
        let second = git_cochange_create_op(
            &git_cochange("src//b.rs", "src/a.rs", 9, &["cccccccccccc"]),
            fixed_now(),
        );

        assert_eq!(first.op_id, second.op_id);
        assert_eq!(first.idempotency_key, second.idempotency_key);
        assert_ne!(first.evidence, second.evidence);
    }

    #[test]
    fn git_commit_candidate_idempotency_stays_distinct_by_sha() {
        let first = git_commit(
            "abcdef1234567890abcdef1234567890abcdef12",
            "fix parser bug because newline crash",
            vec![GitCommitClassification::Bugfix],
            vec![git_change("src/parser.rs")],
        );
        let second = git_commit(
            "1234567890abcdef1234567890abcdef12345678",
            "fix parser bug because newline crash",
            vec![GitCommitClassification::Bugfix],
            vec![git_change("src/parser.rs")],
        );
        let first_op = git_commit_memory_create_op(&first, "lesson", fixed_now()).unwrap();
        let second_op = git_commit_memory_create_op(&second, "lesson", fixed_now()).unwrap();

        assert_ne!(first_op.op_id, second_op.op_id);
        assert_ne!(first_op.idempotency_key, second_op.idempotency_key);
    }

    #[test]
    fn git_revert_commit_marks_source_commit_memory_stale() {
        let source = "1234567890abcdef1234567890abcdef12345678";
        let report = GitHistoryReport {
            commits: vec![git_commit(
                "abcdef1234567890abcdef1234567890abcdef12",
                &format!("Revert \"add risky lesson\" This reverts commit {source}."),
                vec![GitCommitClassification::Revert],
                vec![git_change("src/risky.rs")],
            )],
            cochanges: Vec::new(),
            hotspots: Vec::new(),
            commit_cap_hit: false,
        };
        let mut doc = snapshot(
            "git-doc",
            "Risky lesson",
            "Remember risky code",
            &["git"],
            &["src/risky.rs"],
            "proposed",
            MemorySourceClass::AutoGenerated,
        );
        doc.source_commit = Some(source.to_string());

        let ops = detect_git_memory_ops(&report, &[doc], fixed_now());
        let stale = ops
            .iter()
            .find(|op| op.op_type == MemoryOpType::MarkStale)
            .expect("stale op");

        assert_eq!(stale.source, MemorySource::Git);
        assert_eq!(stale.target_paths, strings(&["/tmp/git-doc.md"]));
        assert!(stale.evidence.contains(source));
        assert!(!stale.requires_approval);
    }

    #[test]
    fn serde_roundtrip_every_source_op_and_status_variant() {
        let sources = [
            MemorySource::Buddy,
            MemorySource::Trajectory,
            MemorySource::Git,
            MemorySource::Manual,
            MemorySource::BehaviorLearner,
            MemorySource::MemoryGarden,
            MemorySource::KnowledgeConflictResolver,
        ];
        for source in sources {
            let json = serde_json::to_string(&source).unwrap();
            assert_eq!(serde_json::from_str::<MemorySource>(&json).unwrap(), source);
        }

        let op_types = [
            MemoryOpType::CreateMemory,
            MemoryOpType::UpdateMemory,
            MemoryOpType::Retag,
            MemoryOpType::RepairLinks,
            MemoryOpType::Refresh,
            MemoryOpType::ArchiveCandidate,
            MemoryOpType::Archive,
            MemoryOpType::MergeArchive,
            MemoryOpType::DeleteCandidate,
            MemoryOpType::PromoteDigest,
            MemoryOpType::MarkReviewNeeded,
            MemoryOpType::MarkStale,
        ];
        for op_type in op_types {
            let json = serde_json::to_string(&op_type).unwrap();
            assert_eq!(
                serde_json::from_str::<MemoryOpType>(&json).unwrap(),
                op_type
            );
        }

        let statuses = [
            MemoryOpStatus::Pending,
            MemoryOpStatus::Approved,
            MemoryOpStatus::Applied,
            MemoryOpStatus::Rejected,
            MemoryOpStatus::Failed,
            MemoryOpStatus::Skipped,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(
                serde_json::from_str::<MemoryOpStatus>(&json).unwrap(),
                status
            );
        }

        let op = MemoryLifecycleOp::pending(
            "op-1",
            MemorySource::MemoryGarden,
            MemoryOpType::Retag,
            strings(&["src//lib.rs", "src/lib.rs"]),
            "Memory tags were stale",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(
            serde_json::from_str::<MemoryLifecycleOp>(&json).unwrap(),
            op
        );
    }

    #[test]
    fn idempotency_key_is_stable_for_tag_and_path_order() {
        let first = MemoryOpIdempotencyInput {
            source: MemorySource::Trajectory,
            op_type: MemoryOpType::CreateMemory,
            target_paths: strings(&["src//buddy/memory_lifecycle.rs", "README.md"]),
            tags: strings(&[" Buddy ", "Memory", "buddy"]),
            kind: Some(" Research Note ".to_string()),
            source_id: Some(" trajectory-1 ".to_string()),
            title: Some("  Useful discovery  ".to_string()),
            content: Some("Line one\r\nLine two\n".to_string()),
            evidence: Some(" observed in trajectory ".to_string()),
        };
        let second = MemoryOpIdempotencyInput {
            source: MemorySource::Trajectory,
            op_type: MemoryOpType::CreateMemory,
            target_paths: strings(&["README.md", "src/buddy/memory_lifecycle.rs"]),
            tags: strings(&["memory", "buddy"]),
            kind: Some("research_note".to_string()),
            source_id: Some("trajectory-1".to_string()),
            title: Some("Useful discovery".to_string()),
            content: Some("Line one\nLine two".to_string()),
            evidence: Some("observed in trajectory".to_string()),
        };

        assert_eq!(
            compute_idempotency_key(&first),
            compute_idempotency_key(&second)
        );
    }

    #[test]
    fn path_normalization_handles_unix_relative_and_windows_inputs() {
        assert_eq!(
            normalize_path("/tmp//project/./src/lib.rs"),
            Some("/tmp/project/src/lib.rs".to_string())
        );
        assert_eq!(
            normalize_path(" ./relative//path/ "),
            Some("relative/path".to_string())
        );
        assert_eq!(
            normalize_path("../outside.md"),
            Some("../outside.md".to_string())
        );
        assert_eq!(
            normalize_path("src\\buddy//memory_lifecycle.rs"),
            Some("src/buddy/memory_lifecycle.rs".to_string())
        );
        assert_eq!(
            normalize_path("c:\\Users\\Ada\\project\\file.md"),
            Some("C:/Users/Ada/project/file.md".to_string())
        );
        assert_eq!(
            normalize_paths(&strings(&["b//c", "a\\d", "b/c", ""])),
            strings(&["a/d", "b/c"])
        );
    }

    #[test]
    fn memory_doc_path_accepts_md_and_mdx_only() {
        assert!(is_memory_doc_path(Path::new("memory.md")));
        assert!(is_memory_doc_path(Path::new("memory.mdx")));
        assert!(!is_memory_doc_path(Path::new("memory.txt")));
        assert!(validate_memory_extension(Path::new("memory.mdx")).is_ok());
    }

    #[test]
    fn lifecycle_status_parser_covers_canonical_statuses_and_aliases() {
        let cases = [
            ("proposed", Some("proposed")),
            ("active", Some("active")),
            ("pinned", Some("pinned")),
            ("archived", Some("archived")),
            ("deprecated", Some("deprecated")),
            ("review", Some("proposed")),
            ("review-needed", Some("proposed")),
            ("review__needed", Some("proposed")),
            ("needs review", Some("proposed")),
            ("needs   review", Some("proposed")),
            ("needs\treview", Some("proposed")),
            ("needs_review", Some("proposed")),
            ("stale", Some("deprecated")),
            ("obsolete", Some("deprecated")),
            ("inactive", Some("archived")),
            ("archive", Some("archived")),
            ("", None),
            ("unknown", None),
        ];

        for (input, expected) in cases {
            assert_eq!(
                parse_memory_lifecycle_status(input),
                expected.map(str::to_string)
            );
        }
    }

    #[test]
    fn lifecycle_status_normalizer_delegates_and_defaults_unknown_to_active() {
        assert_eq!(normalize_memory_status(Some("needs-review")), "proposed");
        assert_eq!(normalize_memory_status(Some("obsolete")), "deprecated");
        assert_eq!(normalize_memory_status(Some("inactive")), "archived");
        assert_eq!(normalize_memory_status(Some("")), "active");
        assert_eq!(normalize_memory_status(Some("unknown")), "active");
        assert_eq!(normalize_memory_status(None), "active");
    }

    #[test]
    fn tag_normalization_trims_lowercases_sorts_and_dedupes() {
        assert_eq!(
            normalize_tags(&strings(&[" Buddy ", "memory", "", "MEMORY", "alpha"])),
            strings(&["alpha", "buddy", "memory"])
        );
    }

    #[test]
    fn default_approval_policy_allows_all_ops() {
        assert!(!default_requires_approval(MemoryOpType::Archive, 0.99));
        assert!(!default_requires_approval(
            MemoryOpType::ArchiveCandidate,
            0.99
        ));
        assert!(!default_requires_approval(MemoryOpType::MergeArchive, 0.99));
        assert!(!default_requires_approval(
            MemoryOpType::DeleteCandidate,
            0.99
        ));

        assert!(!default_requires_approval(MemoryOpType::CreateMemory, 0.90));
        assert!(!default_requires_approval(MemoryOpType::Retag, 0.90));
        assert!(!default_requires_approval(MemoryOpType::RepairLinks, 0.90));
        assert!(!default_requires_approval(MemoryOpType::CreateMemory, 0.70));
        assert!(!default_requires_approval(MemoryOpType::UpdateMemory, 0.95));
    }

    #[test]
    fn review_after_policy_varies_by_kind_source_and_status() {
        let manual_code = default_review_after_days(
            "code",
            MemorySource::Manual,
            MemoryCandidateStatus::Promoted,
        );
        let manual_research = default_review_after_days(
            "research",
            MemorySource::Manual,
            MemoryCandidateStatus::Promoted,
        );
        let manual_task = default_review_after_days(
            "task_report",
            MemorySource::Manual,
            MemoryCandidateStatus::Promoted,
        );
        let proposed_auto_code = default_review_after_days(
            "code",
            MemorySource::BehaviorLearner,
            MemoryCandidateStatus::Proposed,
        );

        assert!(manual_code > manual_research);
        assert!(manual_research > manual_task);
        assert!(proposed_auto_code < manual_code);
        assert_eq!(proposed_auto_code, 30);
        assert_eq!(
            default_review_after_date(
                chrono::NaiveDate::from_ymd_opt(2026, 5, 2).unwrap(),
                "task_report",
                MemorySource::Manual,
                MemoryCandidateStatus::Promoted,
            ),
            "2026-06-01"
        );
    }

    #[test]
    fn memory_ops_state_preserves_first_seen_order() {
        let first = test_op("op-1", "first", MemoryOpStatus::Pending);
        let second = test_op("op-2", "second", MemoryOpStatus::Approved);
        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first.clone() },
            MemoryOpsRecord::Op { op: second.clone() },
        ]);

        assert_eq!(state.ops, vec![first.normalized(), second.normalized()]);
        assert_eq!(state.pending_count, 1);
        assert_eq!(state.approved_count, 1);
    }

    #[test]
    fn memory_ops_state_duplicate_idempotency_key_uses_latest_slot() {
        let first = test_op("op-1", "same", MemoryOpStatus::Pending);
        let mut second = test_op("op-2", "same", MemoryOpStatus::Applied);
        second.idempotency_key = first.idempotency_key.clone();

        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first },
            MemoryOpsRecord::Op { op: second.clone() },
        ]);

        assert_eq!(state.ops.len(), 1);
        assert_eq!(state.ops[0].op_id, "op-2");
        assert_eq!(state.ops[0].status, MemoryOpStatus::Applied);
        assert_eq!(state.applied_count, 1);
    }

    #[test]
    fn memory_ops_state_same_idempotency_key_with_different_op_id_is_duplicate() {
        let first = explicit_key_test_op("op-1", "semantic-key", MemoryOpStatus::Pending);
        let second = explicit_key_test_op("op-2", "semantic-key", MemoryOpStatus::Applied);

        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first },
            MemoryOpsRecord::Op { op: second.clone() },
        ]);

        assert_eq!(state.ops, vec![second.normalized()]);
        assert_eq!(state.applied_count, 1);
    }

    #[test]
    fn memory_ops_state_missing_incoming_idempotency_key_uses_op_id_fallback() {
        let first = legacy_test_op("op-legacy", "first", MemoryOpStatus::Pending);
        let second = legacy_test_op("op-legacy", "second", MemoryOpStatus::Applied);
        let expected = second.clone().normalized();

        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first },
            MemoryOpsRecord::Op { op: second },
        ]);

        assert_eq!(state.ops, vec![expected]);
        assert_eq!(state.applied_count, 1);
    }

    #[test]
    fn memory_ops_state_different_idempotency_keys_with_same_op_id_are_not_duplicates() {
        let first = explicit_key_test_op("op-collide", "semantic-key-a", MemoryOpStatus::Applied);
        let second = explicit_key_test_op("op-collide", "semantic-key-b", MemoryOpStatus::Pending);

        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first.clone() },
            MemoryOpsRecord::Op { op: second.clone() },
        ]);

        assert_eq!(state.ops, vec![first.normalized(), second.normalized()]);
        assert_eq!(state.applied_count, 1);
        assert_eq!(state.pending_count, 1);
    }

    #[test]
    fn memory_ops_state_existing_finalized_old_key_does_not_suppress_new_key() {
        for status in [MemoryOpStatus::Applied, MemoryOpStatus::Rejected] {
            let first = explicit_key_test_op("op-collide", "old-key", status);
            let second = explicit_key_test_op("op-collide", "new-key", MemoryOpStatus::Pending);

            let state = MemoryOpsState::from_records(vec![
                MemoryOpsRecord::Op { op: first.clone() },
                MemoryOpsRecord::Op { op: second.clone() },
            ]);

            assert_eq!(state.ops, vec![first.normalized(), second.normalized()]);
        }
    }

    #[test]
    fn memory_ops_state_duplicate_pending_does_not_reopen_finalized_or_approved() {
        let statuses = [
            MemoryOpStatus::Applied,
            MemoryOpStatus::Rejected,
            MemoryOpStatus::Skipped,
            MemoryOpStatus::Failed,
            MemoryOpStatus::Approved,
        ];
        for status in statuses {
            let first = test_op(
                &format!("op-{}-first", status.as_str()),
                status.as_str(),
                status,
            );
            let mut pending = test_op(
                &format!("op-{}-pending", status.as_str()),
                "new pending",
                MemoryOpStatus::Pending,
            );
            pending.idempotency_key = first.idempotency_key.clone();

            let state = MemoryOpsState::from_records(vec![
                MemoryOpsRecord::Op { op: first.clone() },
                MemoryOpsRecord::Op { op: pending },
            ]);
            let compacted = MemoryOpsState::from_records(state.canonical_records());

            assert_eq!(state.ops, vec![first.normalized()]);
            assert_eq!(compacted.ops, state.ops);
        }
    }

    #[test]
    fn memory_ops_state_duplicate_pending_replaces_pending() {
        let first = test_op("op-pending-first", "same", MemoryOpStatus::Pending);
        let mut second = test_op("op-pending-second", "new pending", MemoryOpStatus::Pending);
        second.idempotency_key = first.idempotency_key.clone();

        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first },
            MemoryOpsRecord::Op { op: second.clone() },
        ]);

        assert_eq!(state.ops, vec![second.normalized()]);
        assert_eq!(state.pending_count, 1);
    }

    #[test]
    fn memory_ops_state_pending_duplicate_replaced_by_applied() {
        let first = test_op("op-pending-first", "same", MemoryOpStatus::Pending);
        let mut second = test_op("op-applied-second", "applied", MemoryOpStatus::Applied);
        second.idempotency_key = first.idempotency_key.clone();

        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first },
            MemoryOpsRecord::Op { op: second.clone() },
        ]);

        assert_eq!(state.ops, vec![second.normalized()]);
        assert_eq!(state.applied_count, 1);
    }

    #[test]
    fn memory_ops_state_approved_duplicate_replaced_by_finalized() {
        let first = test_op("op-approved-first", "same", MemoryOpStatus::Approved);
        let mut second = test_op("op-rejected-second", "rejected", MemoryOpStatus::Rejected);
        second.idempotency_key = first.idempotency_key.clone();

        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first },
            MemoryOpsRecord::Op { op: second.clone() },
        ]);

        assert_eq!(state.ops, vec![second.normalized()]);
        assert_eq!(state.rejected_count, 1);
    }

    #[test]
    fn memory_ops_state_compaction_records_latest_per_op_and_key() {
        let first = test_op("op-1", "first", MemoryOpStatus::Pending);
        let mut second = first.clone();
        second.status = MemoryOpStatus::Failed;
        second.error = Some("write failed".to_string());
        let third = test_op("op-2", "second", MemoryOpStatus::Applied);

        let state = MemoryOpsState::from_records(vec![
            MemoryOpsRecord::Op { op: first },
            MemoryOpsRecord::Op { op: second.clone() },
            MemoryOpsRecord::Op { op: third.clone() },
        ]);
        let compacted = MemoryOpsState::from_records(state.canonical_records());

        assert_eq!(compacted.ops, vec![second.normalized(), third.normalized()]);
        assert_eq!(compacted.failed_count, 1);
        assert_eq!(compacted.applied_count, 1);
    }

    #[test]
    fn lifecycle_op_normalizes_evidence_redacts_and_caps() {
        let raw = format!(
            "token ghp_AbCdEfGhIj1234567890 password=secret {}",
            "x".repeat(MEMORY_OP_EVIDENCE_MAX_CHARS * 2)
        );
        let op = MemoryLifecycleOp::pending(
            "op-evidence",
            MemorySource::MemoryGarden,
            MemoryOpType::CreateMemory,
            Vec::new(),
            raw.clone(),
            0.91,
            "2026-05-02T00:00:00Z",
        );

        assert_no_raw_secret(&op.evidence);
        assert!(op.evidence.len() <= MEMORY_OP_EVIDENCE_MAX_CHARS);

        let mut stale = op.clone();
        stale.evidence = raw;
        let normalized = stale.normalized();

        assert_no_raw_secret(&normalized.evidence);
        assert!(normalized.evidence.len() <= MEMORY_OP_EVIDENCE_MAX_CHARS);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_memory_op_writes_frontmatter_body_with_normalized_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let mut op = MemoryLifecycleOp::pending(
            "op-create",
            MemorySource::BehaviorLearner,
            MemoryOpType::CreateMemory,
            Vec::new(),
            "evidence",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        op.payload = MemoryLifecyclePayload {
            title: Some(" Useful Memory ".to_string()),
            content: Some("# Useful Memory\n\nBody".to_string()),
            tags: Some(strings(&[" Buddy ", "MEMORY", "buddy"])),
            kind: Some("Preference".to_string()),
            filenames: Some(strings(&["src//lib.rs"])),
            related_files: Some(strings(&["src/main.rs"])),
            ..Default::default()
        };

        let outcome = apply_memory_lifecycle_op(gcx, &op).await.unwrap();

        assert_eq!(outcome.status, MemoryOpStatus::Applied);
        assert_eq!(outcome.applied_paths.len(), 1);
        let text = tokio::fs::read_to_string(&outcome.applied_paths[0])
            .await
            .unwrap();
        let (frontmatter, body) = frontmatter_and_body(&text);
        assert_eq!(frontmatter.title.as_deref(), Some("Useful Memory"));
        assert_eq!(frontmatter.tags, strings(&["buddy", "memory"]));
        assert_eq!(frontmatter.kind.as_deref(), Some("preference"));
        assert_eq!(frontmatter.status.as_deref(), Some("active"));
        assert_eq!(frontmatter.filenames, strings(&["src/lib.rs"]));
        assert_eq!(frontmatter.related_files, strings(&["src/main.rs"]));
        assert_eq!(body, "# Useful Memory\n\nBody");
    }

    #[test]
    fn memory_candidate_create_op_normalizes_proposed_metadata() {
        let candidate = MemoryCandidate {
            candidate_id: " candidate-1 ".to_string(),
            source: MemorySource::Trajectory,
            title: " Useful Lesson ".to_string(),
            content: "Body password=secret".to_string(),
            tags: strings(&["Trajectory", "LESSON", "trajectory"]),
            kind: "Decision".to_string(),
            filenames: strings(&["src//lib.rs"]),
            related_files: strings(&["src/main.rs"]),
            source_id: Some(" trajectory-1:0-2 ".to_string()),
            source_message_range: Some(" 0-2 ".to_string()),
            confidence: 0.72,
            status: MemoryCandidateStatus::Proposed,
            ..Default::default()
        };

        let op = candidate.into_create_memory_op(
            "op-candidate",
            "evidence password=secret",
            "2026-05-02T00:00:00Z",
        );

        assert_eq!(op.source, MemorySource::Trajectory);
        assert_eq!(op.op_type, MemoryOpType::CreateMemory);
        assert_eq!(op.status, MemoryOpStatus::Pending);
        assert!(!op.requires_approval);
        assert_eq!(op.payload.title.as_deref(), Some("Useful Lesson"));
        assert_eq!(op.payload.kind.as_deref(), Some("decision"));
        assert_eq!(op.payload.review_after.as_deref(), Some("2026-06-01"));
        assert_eq!(op.payload.source_id.as_deref(), Some("trajectory-1:0-2"));
        assert_eq!(op.payload.source_message_range.as_deref(), Some("0-2"));
        assert_eq!(op.payload.filenames.unwrap(), strings(&["src/lib.rs"]));
        assert_eq!(op.payload.related_files.unwrap(), strings(&["src/main.rs"]));
        assert!(op.payload.tags.unwrap().contains(&"memory".to_string()));
        assert!(!op.payload.content.unwrap().contains("secret"));
        assert_no_raw_secret(&op.evidence);
        assert!(op.evidence.len() <= MEMORY_OP_EVIDENCE_MAX_CHARS);
    }

    #[cfg_attr(windows, ignore = "needs symlink privileges Windows does not grant by default")]
    #[tokio::test(flavor = "multi_thread")]
    async fn symlinked_knowledge_root_scan_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let real_root = dir.path().join("real_knowledge");
        tokio::fs::create_dir_all(&real_root).await.unwrap();
        write_memory_file(
            &real_root.join("memory.md"),
            active_frontmatter("memory", &["buddy"]),
            "Body",
        )
        .await;
        let symlink_root = dir.path().join("symlink_knowledge");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_root, &symlink_root).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_root, &symlink_root).unwrap();

        let docs = load_memory_doc_snapshots_from_knowledge_dirs(&[symlink_root]).await;

        assert!(docs.is_empty());
    }

    #[cfg_attr(windows, ignore = "needs symlink privileges Windows does not grant by default")]
    #[tokio::test(flavor = "multi_thread")]
    async fn popped_symlink_directory_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let scan_root = dir.path().join("scan_root");
        let real_root = dir.path().join("real_root");
        tokio::fs::create_dir_all(&scan_root).await.unwrap();
        tokio::fs::create_dir_all(&real_root).await.unwrap();
        write_memory_file(
            &real_root.join("memory.md"),
            active_frontmatter("memory", &["buddy"]),
            "Body",
        )
        .await;
        let symlink_dir = scan_root.join("linked_dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_root, &symlink_dir).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_root, &symlink_dir).unwrap();

        let docs = load_memory_doc_snapshots_from_knowledge_dirs(&[scan_root]).await;

        assert!(docs.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn autonomous_create_memory_defaults_to_proposed_without_approval() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let mut op = MemoryLifecycleOp::pending(
            "op-create-proposed",
            MemorySource::MemoryGarden,
            MemoryOpType::CreateMemory,
            Vec::new(),
            "Unapproved autonomous evidence",
            0.50,
            "2026-05-02T00:00:00Z",
        );
        op.requires_approval = false;
        op.payload.content = Some("Autonomous body".to_string());

        let outcome = apply_memory_lifecycle_op(gcx, &op).await.unwrap();
        let text = tokio::fs::read_to_string(&outcome.applied_paths[0])
            .await
            .unwrap();
        let (frontmatter, _) = frontmatter_and_body(&text);

        assert_eq!(frontmatter.status.as_deref(), Some("proposed"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retag_and_repair_links_preserve_body_and_parseable_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("memory.md");
        let mut frontmatter = active_frontmatter("memory", &["old"]);
        frontmatter.filenames = strings(&["old.rs"]);
        frontmatter.related_files = strings(&["old-related.rs"]);
        frontmatter.links = strings(&["old-link"]);
        write_memory_file(&path, frontmatter, "# Heading\n\nOriginal body").await;

        let mut retag = MemoryLifecycleOp::pending(
            "op-retag",
            MemorySource::MemoryGarden,
            MemoryOpType::Retag,
            vec![path.to_string_lossy().to_string()],
            "retag",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        retag.payload.tags = Some(strings(&["New", "buddy"]));
        apply_memory_lifecycle_op(gcx.clone(), &retag)
            .await
            .unwrap();

        let mut repair = MemoryLifecycleOp::pending(
            "op-repair",
            MemorySource::MemoryGarden,
            MemoryOpType::RepairLinks,
            vec![path.to_string_lossy().to_string()],
            "repair",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        repair.payload.filenames = Some(strings(&["src/lib.rs"]));
        repair.payload.related_files = Some(strings(&["src/main.rs"]));
        repair.payload.links = Some(strings(&["new-link"]));
        apply_memory_lifecycle_op(gcx, &repair).await.unwrap();

        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let (frontmatter, body) = frontmatter_and_body(&text);
        assert_eq!(frontmatter.tags, strings(&["buddy", "new"]));
        assert_eq!(frontmatter.filenames, strings(&["src/lib.rs"]));
        assert_eq!(frontmatter.related_files, strings(&["src/main.rs"]));
        assert_eq!(frontmatter.links, strings(&["new-link"]));
        assert_eq!(body, "# Heading\n\nOriginal body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn archive_op_preserves_content_and_makes_doc_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("memory.md");
        write_memory_file(
            &path,
            active_frontmatter("memory", &["old"]),
            "Archive body",
        )
        .await;

        let mut op = MemoryLifecycleOp::pending(
            "op-archive",
            MemorySource::MemoryGarden,
            MemoryOpType::Archive,
            vec![path.to_string_lossy().to_string()],
            "archive",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        op.status = MemoryOpStatus::Approved;

        apply_memory_lifecycle_op(gcx.clone(), &op).await.unwrap();

        assert!(path.exists());
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let (frontmatter, body) = frontmatter_and_body(&text);
        assert_eq!(frontmatter.status.as_deref(), Some("archived"));
        assert_eq!(body, "Archive body");
        let kg = crate::knowledge_graph::build_knowledge_graph(gcx.gcx.clone()).await;
        assert!(kg.active_docs().all(|doc| doc.path != path));
        let found = crate::memories::load_memories_by_tags(&gcx.gcx, &["old"], 10)
            .await
            .unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn merge_archive_preserves_existing_observation_metadata_and_extra() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("canonical.md");
        let mut frontmatter = active_frontmatter("canonical", &["old"]);
        frontmatter.signal_key = Some("signal-1".to_string());
        frontmatter.occurrences = 7;
        frontmatter.last_observed = Some("2026-05-01T00:00:00Z".to_string());
        frontmatter.extra.insert(
            "unknown_key".to_string(),
            serde_yaml::Value::String("unknown-value".to_string()),
        );
        write_memory_file(&path, frontmatter, "Old body").await;

        let mut op = MemoryLifecycleOp::pending(
            "op-merge-preserve",
            MemorySource::MemoryGarden,
            MemoryOpType::MergeArchive,
            vec![path.to_string_lossy().to_string()],
            "merge",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        op.status = MemoryOpStatus::Approved;
        op.payload.canonical = Some(MemoryCreatePayload {
            title: Some("Merged canonical".to_string()),
            content: "Merged body".to_string(),
            tags: strings(&["new"]),
            kind: "domain".to_string(),
            ..Default::default()
        });

        apply_memory_lifecycle_op(gcx, &op).await.unwrap();

        let (frontmatter, body) =
            frontmatter_and_body(&tokio::fs::read_to_string(&path).await.unwrap());
        assert_eq!(frontmatter.title.as_deref(), Some("Merged canonical"));
        assert_eq!(frontmatter.tags, strings(&["new"]));
        assert_eq!(frontmatter.signal_key.as_deref(), Some("signal-1"));
        assert_eq!(frontmatter.occurrences, 7);
        assert_eq!(
            frontmatter.last_observed.as_deref(),
            Some("2026-05-01T00:00:00Z")
        );
        assert_eq!(
            frontmatter.extra.get("unknown_key"),
            Some(&serde_yaml::Value::String("unknown-value".to_string()))
        );
        assert_eq!(body, "Merged body");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn review_status_reactivation_clears_archive_metadata() {
        for status in ["active", "proposed", "pinned"] {
            let dir = tempfile::tempdir().unwrap();
            let gcx = test_gcx_with_workspace(dir.path()).await;
            let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
            tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
            let path = knowledge_dir.join(format!("reactivate-{status}.md"));
            let mut frontmatter = active_frontmatter(status, &["old"]);
            frontmatter.status = Some("deprecated".to_string());
            frontmatter.deprecated_at = Some("2026-05-01".to_string());
            frontmatter.superseded_by = Some("canonical".to_string());
            write_memory_file(&path, frontmatter, "Reactivation body").await;

            let mut op = MemoryLifecycleOp::pending(
                format!("op-reactivate-{status}"),
                MemorySource::MemoryGarden,
                MemoryOpType::MarkReviewNeeded,
                vec![path.to_string_lossy().to_string()],
                "reactivate",
                0.91,
                "2026-05-02T00:00:00Z",
            );
            op.status = MemoryOpStatus::Approved;
            op.payload.review_after = Some("2026-05-03".to_string());

            apply_review_status(&gcx.chat_ctx(), &op, status)
                .await
                .unwrap();

            let (frontmatter, body) =
                frontmatter_and_body(&tokio::fs::read_to_string(&path).await.unwrap());
            assert_eq!(frontmatter.status.as_deref(), Some(status));
            assert_eq!(frontmatter.deprecated_at, None);
            assert_eq!(frontmatter.superseded_by, None);
            assert_eq!(body, "Reactivation body");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn review_and_stale_ops_persist_canonical_statuses() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let review_path = knowledge_dir.join("review.md");
        let stale_path = knowledge_dir.join("stale.md");
        write_memory_file(
            &review_path,
            active_frontmatter("review", &["old"]),
            "Review body",
        )
        .await;
        write_memory_file(
            &stale_path,
            active_frontmatter("stale", &["old"]),
            "Stale body",
        )
        .await;

        let mut review = MemoryLifecycleOp::pending(
            "op-review",
            MemorySource::MemoryGarden,
            MemoryOpType::MarkReviewNeeded,
            vec![review_path.to_string_lossy().to_string()],
            "review",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        review.status = MemoryOpStatus::Approved;
        review.payload.review_after = Some("2026-05-03".to_string());
        apply_memory_lifecycle_op(gcx.clone(), &review)
            .await
            .unwrap();

        let mut stale = MemoryLifecycleOp::pending(
            "op-stale",
            MemorySource::MemoryGarden,
            MemoryOpType::MarkStale,
            vec![stale_path.to_string_lossy().to_string()],
            "stale",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        stale.status = MemoryOpStatus::Approved;
        stale.payload.review_after = Some("2026-05-04".to_string());
        apply_memory_lifecycle_op(gcx, &stale).await.unwrap();

        let (review_frontmatter, review_body) =
            frontmatter_and_body(&tokio::fs::read_to_string(&review_path).await.unwrap());
        assert_eq!(review_frontmatter.status.as_deref(), Some("active"));
        assert_eq!(review_frontmatter.review_needed, Some(true));
        assert_eq!(
            review_frontmatter.review_after.as_deref(),
            Some("2026-05-03")
        );
        assert_eq!(review_frontmatter.deprecated_at, None);
        assert_eq!(review_body, "Review body");

        let (stale_frontmatter, stale_body) =
            frontmatter_and_body(&tokio::fs::read_to_string(&stale_path).await.unwrap());
        assert_eq!(stale_frontmatter.status.as_deref(), Some("deprecated"));
        assert_eq!(
            stale_frontmatter.review_after.as_deref(),
            Some("2026-05-04")
        );
        assert!(stale_frontmatter.deprecated_at.is_some());
        assert_eq!(stale_body, "Stale body");
    }

    #[cfg_attr(windows, ignore = "needs symlink privileges Windows does not grant by default")]
    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_path_traversal_and_symlink_escape_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let outside = dir.path().join("outside.md");
        write_memory_file(&outside, active_frontmatter("outside", &["old"]), "Outside").await;
        let link = knowledge_dir.join("link.md");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, &link).unwrap();

        let mut traversal = MemoryLifecycleOp::pending(
            "op-traversal",
            MemorySource::MemoryGarden,
            MemoryOpType::Retag,
            strings(&["../outside.md"]),
            "retag",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        traversal.payload.tags = Some(strings(&["new"]));
        let traversal_err = apply_memory_lifecycle_op(gcx.clone(), &traversal)
            .await
            .unwrap_err();
        assert!(traversal_err.contains(".."));

        let mut symlink = MemoryLifecycleOp::pending(
            "op-symlink",
            MemorySource::MemoryGarden,
            MemoryOpType::Retag,
            vec![link.to_string_lossy().to_string()],
            "retag",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        symlink.payload.tags = Some(strings(&["new"]));
        let symlink_err = apply_memory_lifecycle_op(gcx, &symlink).await.unwrap_err();
        assert!(symlink_err.contains("symlink"));

        let text = tokio::fs::read_to_string(&outside).await.unwrap();
        let (frontmatter, body) = frontmatter_and_body(&text);
        assert_eq!(frontmatter.tags, strings(&["old"]));
        assert_eq!(body, "Outside");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apply_status_preserves_finalized_ops_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        for status in [
            MemoryOpStatus::Applied,
            MemoryOpStatus::Rejected,
            MemoryOpStatus::Skipped,
            MemoryOpStatus::Failed,
        ] {
            let mut op = test_op(&format!("op-{}", status.as_str()), status.as_str(), status);
            op.error = Some(format!("{} original", status.as_str()));
            op.applied_at = Some("2026-05-02T00:01:00Z".to_string());

            let updated = apply_memory_lifecycle_op_status(gcx.clone(), &op).await;

            assert_eq!(updated, op.normalized());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_apply_op_is_direct_noop() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let mut op = MemoryLifecycleOp::pending(
            "op-failed",
            MemorySource::BehaviorLearner,
            MemoryOpType::CreateMemory,
            Vec::new(),
            "evidence",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        op.status = MemoryOpStatus::Failed;
        op.error = Some("original failure".to_string());
        op.payload.content = Some("Should not be written".to_string());

        let outcome = apply_memory_lifecycle_op(gcx, &op).await.unwrap();

        assert_eq!(outcome.status, MemoryOpStatus::Failed);
        assert_eq!(outcome.message.as_deref(), Some("original failure"));
        assert!(outcome.applied_paths.is_empty());
        assert!(dir.path().join(KNOWLEDGE_FOLDER_NAME).read_dir().is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_approval_required_op_status_applies() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let old_path = knowledge_dir.join("old.md");
        write_memory_file(&old_path, active_frontmatter("old", &["old"]), "Old body").await;

        let mut op = MemoryLifecycleOp::pending(
            "op-pending-merge",
            MemorySource::MemoryGarden,
            MemoryOpType::MergeArchive,
            vec![old_path.to_string_lossy().to_string()],
            "merge",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        op.payload.canonical = Some(MemoryCreatePayload {
            title: Some("Canonical".to_string()),
            content: "Canonical body".to_string(),
            tags: strings(&["canonical"]),
            kind: "domain".to_string(),
            ..Default::default()
        });

        let updated = apply_memory_lifecycle_op_status(gcx, &op).await;

        assert_eq!(updated.status, MemoryOpStatus::Applied);
        assert_eq!(updated.error, None);
        assert!(updated.applied_at.is_some());
        let text = tokio::fs::read_to_string(&old_path).await.unwrap();
        let (frontmatter, body) = frontmatter_and_body(&text);
        assert_eq!(frontmatter.status.as_deref(), Some("active"));
        assert_eq!(frontmatter.title.as_deref(), Some("Canonical"));
        assert!(body.contains("Canonical body"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_apply_leaves_original_file_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let path = knowledge_dir.join("memory.md");
        write_memory_file(&path, active_frontmatter("memory", &["old"]), "Body").await;
        let before = tokio::fs::read_to_string(&path).await.unwrap();

        let op = MemoryLifecycleOp::pending(
            "op-fail",
            MemorySource::MemoryGarden,
            MemoryOpType::Retag,
            vec![path.to_string_lossy().to_string()],
            "retag",
            0.91,
            "2026-05-02T00:00:00Z",
        );

        let err = apply_memory_lifecycle_op(gcx, &op).await.unwrap_err();
        assert!(err.contains("missing tags"));
        let after = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(after, before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replay_of_applied_op_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let gcx = test_gcx_with_workspace(dir.path()).await;
        let mut op = MemoryLifecycleOp::pending(
            "op-applied",
            MemorySource::BehaviorLearner,
            MemoryOpType::CreateMemory,
            Vec::new(),
            "evidence",
            0.91,
            "2026-05-02T00:00:00Z",
        );
        op.status = MemoryOpStatus::Applied;
        op.payload.content = Some("Should not be written".to_string());

        let outcome = apply_memory_lifecycle_op(gcx, &op).await.unwrap();

        assert_eq!(outcome.status, MemoryOpStatus::Applied);
        assert!(dir.path().join(KNOWLEDGE_FOLDER_NAME).read_dir().is_err());
    }
}

#[cfg(test)]
mod buddy_memory_tools_checked_tests {
    use super::*;
    use crate::file_filter::KNOWLEDGE_FOLDER_NAME;

    #[tokio::test]
    async fn archive_memory_file_checked_rejects_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        let knowledge_dir = dir.path().join(KNOWLEDGE_FOLDER_NAME);
        tokio::fs::create_dir_all(&knowledge_dir).await.unwrap();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        {
            *crate::app_state::app_state_from_gcx(gcx.clone())
                .await
                .workspace
                .documents_state
                .workspace_folders
                .lock()
                .unwrap() = vec![dir.path().to_path_buf()];
        }
        let gcx = crate::app_state::app_state_from_gcx(gcx).await;
        let path = knowledge_dir
            .join("..")
            .join("knowledge")
            .join("missing.md");
        let err = archive_memory_file_checked_for_ctx(&gcx.chat_ctx(), &path, None)
            .await
            .unwrap_err();
        assert!(err.contains(".."));
    }
}
