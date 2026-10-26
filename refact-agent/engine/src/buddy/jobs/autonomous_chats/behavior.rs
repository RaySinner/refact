use super::*;

pub(super) const MAX_BEHAVIOR_TRAJECTORY_BYTES: u64 = 1_024 * 1_024;
pub(super) const MAX_BEHAVIOR_TRAJECTORY_META_BYTES: u64 = 32 * 1024;
pub(super) const MAX_BEHAVIOR_TRAJECTORY_SCAN_FILES: usize = 5_000;
pub(super) const MAX_BEHAVIOR_TRAJECTORY_SCAN_ENTRIES: usize = 5_000;
pub(super) const MAX_BEHAVIOR_TASK_DIR_SCAN_ENTRIES: usize = 1_000;
pub(super) const BEHAVIOR_COOLDOWN_SECS: u64 = 4 * 60 * 60;
pub(super) const MAX_TRAJECTORY_SNIPPETS: usize = 12;
pub(super) const MAX_BEHAVIOR_PREFERENCE_WRITES: usize = 2;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrajectoryUserSnippet {
    pub(super) trajectory_id: String,
    pub(super) title: String,
    pub(super) mode: String,
    pub(super) updated_at: String,
    pub(super) text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PreferenceCandidate {
    pub(super) statement: String,
    pub(super) evidence: String,
    pub(super) confidence: f32,
}

pub(super) fn first_sentence(text: &str) -> String {
    let trimmed = text.trim();
    let mut end = trimmed.len();
    for marker in ['.', '!', '?', '\n'] {
        if let Some(idx) = trimmed.find(marker) {
            end = end.min(idx + marker.len_utf8());
        }
    }
    crate::llm::safe_truncate(trimmed[..end].trim(), 240).to_string()
}

pub(super) fn explicit_preference_confidence(text: &str) -> Option<f32> {
    let lower = text.to_lowercase();
    let strong = [
        "i prefer",
        "i'd prefer",
        "i would prefer",
        "always ",
        "never ",
        "please always",
    ];
    if strong.iter().any(|cue| lower.contains(cue)) {
        return Some(0.90);
    }
    let medium = [
        "please use",
        "please keep",
        "please avoid",
        "do not ",
        "don't ",
    ];
    if medium.iter().any(|cue| lower.contains(cue)) {
        return Some(0.86);
    }
    None
}

pub(super) fn preference_like(text: &str) -> bool {
    explicit_preference_confidence(text).is_some()
}

pub(super) fn behavior_candidate_status(confidence: f32) -> MemoryCandidateStatus {
    if confidence >= 0.95 {
        MemoryCandidateStatus::Approved
    } else {
        MemoryCandidateStatus::Proposed
    }
}

pub(super) fn behavior_preference_candidates(
    snippets: &[TrajectoryUserSnippet],
) -> Vec<PreferenceCandidate> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for snippet in snippets {
        let Some(confidence) = explicit_preference_confidence(&snippet.text) else {
            continue;
        };
        let statement = redact_and_cap_text(&first_sentence(&snippet.text), 240);
        if !crate::memories::preference_statement_is_safe(&statement, confidence) {
            continue;
        }
        let normalized = crate::memories::normalize_preference_text_for_dedupe(&statement);
        if !seen.insert(normalized) {
            continue;
        }
        let evidence = redact_and_cap_text(
            &format!(
                "User-authored snippet in {} ({})",
                snippet.trajectory_id, snippet.title
            ),
            240,
        );
        candidates.push(PreferenceCandidate {
            statement,
            evidence,
            confidence,
        });
    }
    candidates
}

pub(super) async fn enqueue_behavior_preferences(
    gcx: AppState,
    candidates: &[PreferenceCandidate],
) -> usize {
    let Some(project_root) = crate::files_correction::get_project_dirs(gcx.gcx.clone())
        .await
        .into_iter()
        .next()
    else {
        return 0;
    };
    let mut queued = 0;
    for candidate in candidates {
        if queued >= MAX_BEHAVIOR_PREFERENCE_WRITES {
            break;
        }
        let content = if candidate.evidence.trim().is_empty() {
            format!(
                "# {}\n\nConfidence: {:.2}",
                candidate.statement, candidate.confidence
            )
        } else {
            format!(
                "# {}\n\nEvidence: {}\n\nConfidence: {:.2}",
                candidate.statement, candidate.evidence, candidate.confidence
            )
        };
        let memory_candidate = MemoryCandidate {
            candidate_id: format!(
                "memcand_behavior_{}",
                signal_hash([candidate.statement.as_str(), candidate.evidence.as_str()])
            ),
            source: MemorySource::BehaviorLearner,
            title: candidate.statement.clone(),
            content,
            tags: vec![
                "preference".to_string(),
                "buddy".to_string(),
                "behavior_learner".to_string(),
            ],
            kind: "preference".to_string(),
            source_id: Some(format!(
                "behavior_learner:{}",
                crate::memories::normalize_preference_text_for_dedupe(&candidate.statement)
            )),
            confidence: candidate.confidence,
            status: behavior_candidate_status(candidate.confidence),
            ..Default::default()
        };
        let op = memory_candidate.into_create_memory_op(
            format!(
                "memop_behavior_{}",
                signal_hash([candidate.statement.as_str()])
            ),
            candidate.evidence.clone(),
            Utc::now().to_rfc3339(),
        );
        match crate::buddy::storage::enqueue_memory_op(&project_root, op).await {
            Ok(_) => queued += 1,
            Err(err) => tracing::warn!("buddy behavior learner preference enqueue failed: {}", err),
        }
    }
    queued
}

pub(super) fn extract_text_from_trajectory_content(content: &serde_json::Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    if let Some(items) = content.as_array() {
        let parts = items
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|v| v.as_str()) == Some("image_url")
                    || item
                        .get("m_type")
                        .and_then(|v| v.as_str())
                        .map(|kind| kind.starts_with("image/"))
                        .unwrap_or(false)
                {
                    return Some("[image]".to_string());
                }
                item.get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("m_content").and_then(|v| v.as_str()))
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return Some(parts.join("\n\n"));
        }
    }
    None
}

pub(super) async fn read_bounded_trajectory_json(
    path: &Path,
    max_bytes: u64,
) -> Option<serde_json::Value> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    if metadata.len() > max_bytes {
        return None;
    }
    let content = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str::<serde_json::Value>(&content).ok()
}

#[derive(Debug, Clone)]
pub(super) struct BehaviorTrajectoryMeta {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) mode: String,
    pub(super) updated_at: String,
    pub(super) path: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct BehaviorTrajectoryCandidate {
    pub(super) modified_key: u64,
    pub(super) path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct BehaviorTrajectoryScanStats {
    pub(super) visited_entries: usize,
    pub(super) matching_files_considered: usize,
    pub(super) raw_entries_examined: usize,
}

pub(super) fn parse_json_string_at(content: &str, start: usize) -> Option<(String, usize)> {
    if content.as_bytes().get(start) != Some(&b'"') {
        return None;
    }
    let mut escaped = false;
    for (offset, ch) in content[start + 1..].char_indices() {
        let idx = start + 1 + offset;
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            let raw = &content[start..=idx];
            let parsed = serde_json::from_str::<String>(raw).ok()?;
            return Some((parsed, idx + 1));
        }
    }
    None
}

pub(super) fn skip_json_ws(content: &str, mut idx: usize) -> usize {
    let bytes = content.as_bytes();
    while bytes
        .get(idx)
        .map(|byte| byte.is_ascii_whitespace())
        .unwrap_or(false)
    {
        idx += 1;
    }
    idx
}

pub(super) fn skip_json_value(content: &str, idx: usize) -> Option<usize> {
    let mut idx = skip_json_ws(content, idx);
    let bytes = content.as_bytes();
    match bytes.get(idx).copied()? {
        b'"' => parse_json_string_at(content, idx).map(|(_, next)| next),
        b'{' | b'[' => {
            let mut depth = 0usize;
            while idx < bytes.len() {
                match bytes[idx] {
                    b'"' => idx = parse_json_string_at(content, idx)?.1,
                    b'{' | b'[' => {
                        depth += 1;
                        idx += 1;
                    }
                    b'}' | b']' => {
                        depth = depth.checked_sub(1)?;
                        idx += 1;
                        if depth == 0 {
                            return Some(idx);
                        }
                    }
                    _ => idx += 1,
                }
            }
            None
        }
        _ => {
            while idx < bytes.len() && !matches!(bytes[idx], b',' | b'}') {
                idx += 1;
            }
            Some(idx)
        }
    }
}

pub(super) fn top_level_json_strings(content: &str, fields: &[&str]) -> HashMap<String, String> {
    let bytes = content.as_bytes();
    let mut idx = skip_json_ws(content, 0);
    let mut values = HashMap::new();
    if bytes.get(idx) != Some(&b'{') {
        return values;
    }
    idx += 1;
    while idx < bytes.len() {
        idx = skip_json_ws(content, idx);
        match bytes.get(idx).copied() {
            Some(b'}') | None => break,
            Some(b',') => {
                idx += 1;
                continue;
            }
            Some(b'"') => {}
            _ => break,
        }
        let Some((key, next)) = parse_json_string_at(content, idx) else {
            break;
        };
        idx = skip_json_ws(content, next);
        if bytes.get(idx) != Some(&b':') {
            break;
        }
        idx = skip_json_ws(content, idx + 1);
        if fields.contains(&key.as_str()) && bytes.get(idx) == Some(&b'"') {
            let Some((value, next)) = parse_json_string_at(content, idx) else {
                break;
            };
            values.entry(key).or_insert(value);
            idx = next;
        } else {
            let Some(next) = skip_json_value(content, idx) else {
                break;
            };
            idx = next;
        }
    }
    values
}

pub(super) fn top_level_json_string(content: &str, fields: &[&str]) -> Option<String> {
    let values = top_level_json_strings(content, fields);
    fields.iter().find_map(|field| values.get(*field).cloned())
}

pub(super) fn parse_behavior_trajectory_meta(
    content: &str,
    path: PathBuf,
) -> Option<BehaviorTrajectoryMeta> {
    let fields = top_level_json_strings(
        content,
        &[
            "id",
            "chat_id",
            "title",
            "mode",
            "updated_at",
            "last_message_at",
            "created_at",
            "link_type",
        ],
    );
    if fields
        .get("link_type")
        .is_some_and(|link_type| link_type.starts_with("internal:"))
    {
        return None;
    }
    let id = ["id", "chat_id"]
        .iter()
        .find_map(|field| fields.get(*field).cloned())?;
    let updated_at = ["updated_at", "last_message_at", "created_at"]
        .iter()
        .find_map(|field| fields.get(*field).cloned())?;
    Some(BehaviorTrajectoryMeta {
        id,
        title: fields.get("title").cloned().unwrap_or_default(),
        mode: fields.get("mode").cloned().unwrap_or_default(),
        updated_at,
        path,
    })
}

pub(super) fn behavior_trajectory_is_internal(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let file_len = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let mut prefix = String::new();
    if (&mut file)
        .take(MAX_BEHAVIOR_TRAJECTORY_META_BYTES)
        .read_to_string(&mut prefix)
        .is_err()
    {
        return false;
    }
    let tail_start = file_len.saturating_sub(MAX_BEHAVIOR_TRAJECTORY_META_BYTES);
    if file.seek(SeekFrom::Start(tail_start)).is_err() {
        return false;
    }
    let mut tail = String::new();
    if file.read_to_string(&mut tail).is_err() {
        return false;
    }
    [prefix.as_str(), tail.as_str()]
        .iter()
        .find_map(|content| top_level_json_string(content, &["link_type"]))
        .is_some_and(|link_type| link_type.starts_with("internal:"))
}

#[cfg(test)]
pub(super) fn collect_behavior_trajectory_metas_from_dir(
    dir: &Path,
    metas: &mut Vec<BehaviorTrajectoryMeta>,
    seen: &mut HashSet<String>,
) {
    let candidates = collect_behavior_trajectory_candidates_from_dirs(
        std::slice::from_ref(&dir.to_path_buf()),
        MAX_BEHAVIOR_TRAJECTORY_SCAN_FILES,
    );
    collect_behavior_trajectory_metas_from_candidates(candidates, metas, seen);
}

pub(super) fn system_time_key(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(super) fn push_behavior_trajectory_candidate(
    heap: &mut BinaryHeap<Reverse<BehaviorTrajectoryCandidate>>,
    candidate: BehaviorTrajectoryCandidate,
    max_files: usize,
) {
    if max_files == 0 {
        return;
    }
    if heap.len() < max_files {
        heap.push(Reverse(candidate));
        return;
    }
    let should_replace = heap
        .peek()
        .map(|oldest| candidate > oldest.0)
        .unwrap_or(false);
    if should_replace {
        heap.pop();
        heap.push(Reverse(candidate));
    }
}

pub(super) fn collect_behavior_trajectory_candidates_from_dir(
    dir: &Path,
    heap: &mut BinaryHeap<Reverse<BehaviorTrajectoryCandidate>>,
    max_files: usize,
    max_visited_entries: usize,
    stats: &mut BehaviorTrajectoryScanStats,
) {
    let max_raw_entries = max_visited_entries.saturating_mul(4);
    if max_visited_entries == 0 || stats.raw_entries_examined >= max_raw_entries {
        return;
    }
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        if stats.visited_entries >= max_visited_entries
            || stats.raw_entries_examined >= max_raw_entries
        {
            return;
        }
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        let remaining = max_raw_entries.saturating_sub(stats.raw_entries_examined);
        let mut entries = Vec::new();
        for entry in read_dir.take(remaining) {
            stats.raw_entries_examined += 1;
            let Ok(entry) = entry else {
                continue;
            };
            entries.push(entry);
        }
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                dirs.push(path);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if metadata.len() > MAX_BEHAVIOR_TRAJECTORY_BYTES {
                continue;
            }
            // Filter before applying max_files so recent internal traces cannot crowd
            // older user-visible chats out of the behavior learner's candidate set.
            if behavior_trajectory_is_internal(&path) {
                continue;
            }
            if stats.visited_entries >= max_visited_entries {
                return;
            }
            stats.matching_files_considered += 1;
            stats.visited_entries += 1;
            let modified_key = metadata.modified().map(system_time_key).unwrap_or_default();
            push_behavior_trajectory_candidate(
                heap,
                BehaviorTrajectoryCandidate { modified_key, path },
                max_files,
            );
        }
    }
}

pub(super) fn collect_behavior_trajectory_candidates_from_dirs(
    dirs: &[PathBuf],
    max_files: usize,
) -> Vec<BehaviorTrajectoryCandidate> {
    collect_behavior_trajectory_candidates_from_dirs_with_stats(
        dirs,
        max_files,
        MAX_BEHAVIOR_TRAJECTORY_SCAN_ENTRIES,
    )
    .0
}

pub(super) fn collect_behavior_trajectory_candidates_from_dirs_with_stats(
    dirs: &[PathBuf],
    max_files: usize,
    max_visited_entries: usize,
) -> (
    Vec<BehaviorTrajectoryCandidate>,
    BehaviorTrajectoryScanStats,
) {
    let mut heap = BinaryHeap::new();
    let mut stats = BehaviorTrajectoryScanStats::default();
    for dir in dirs {
        if stats.visited_entries >= max_visited_entries {
            break;
        }
        let Ok(metadata) = std::fs::symlink_metadata(dir) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        collect_behavior_trajectory_candidates_from_dir(
            dir,
            &mut heap,
            max_files,
            max_visited_entries,
            &mut stats,
        );
    }
    let mut candidates = heap
        .into_iter()
        .map(|Reverse(candidate)| candidate)
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        b.modified_key
            .cmp(&a.modified_key)
            .then_with(|| a.path.cmp(&b.path))
    });
    (candidates, stats)
}

pub(super) fn parse_behavior_trajectory_meta_from_path(
    path: &Path,
) -> Option<BehaviorTrajectoryMeta> {
    let Ok(file) = std::fs::File::open(path) else {
        return None;
    };
    let mut content = String::new();
    let mut reader = std::io::Read::take(file, MAX_BEHAVIOR_TRAJECTORY_META_BYTES);
    if std::io::Read::read_to_string(&mut reader, &mut content).is_err() {
        return None;
    }
    parse_behavior_trajectory_meta(&content, path.to_path_buf()).or_else(|| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| parse_behavior_trajectory_meta(&content, path.to_path_buf()))
    })
}

pub(super) fn collect_behavior_trajectory_metas_from_candidates(
    candidates: Vec<BehaviorTrajectoryCandidate>,
    metas: &mut Vec<BehaviorTrajectoryMeta>,
    seen: &mut HashSet<String>,
) {
    for candidate in candidates {
        let Some(meta) = parse_behavior_trajectory_meta_from_path(&candidate.path) else {
            continue;
        };
        if seen.insert(meta.id.clone()) {
            metas.push(meta);
        }
    }
}

pub(super) fn collect_behavior_task_trajectory_dirs(
    tasks_dirs: &[PathBuf],
    max_visited_entries: usize,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut visited_entries = 0usize;
    for tasks_dir in tasks_dirs {
        if visited_entries >= max_visited_entries {
            break;
        }
        let Ok(tasks_metadata) = std::fs::symlink_metadata(tasks_dir) else {
            continue;
        };
        if tasks_metadata.file_type().is_symlink() || !tasks_metadata.is_dir() {
            continue;
        }
        let Ok(read_dir) = std::fs::read_dir(tasks_dir) else {
            continue;
        };
        let remaining = max_visited_entries.saturating_sub(visited_entries);
        let mut task_entries = Vec::new();
        for task_entry in read_dir.take(remaining) {
            visited_entries += 1;
            let Ok(task_entry) = task_entry else {
                continue;
            };
            task_entries.push(task_entry);
        }
        task_entries.sort_by_key(|entry| entry.path());
        for task_entry in task_entries {
            let task_dir = task_entry.path();
            let Ok(task_metadata) = std::fs::symlink_metadata(&task_dir) else {
                continue;
            };
            if task_metadata.file_type().is_symlink() || !task_metadata.is_dir() {
                continue;
            }
            for role in ["planner", "agents"] {
                let role_dir = task_dir.join("trajectories").join(role);
                let Ok(role_metadata) = std::fs::symlink_metadata(&role_dir) else {
                    continue;
                };
                if !role_metadata.file_type().is_symlink() && role_metadata.is_dir() {
                    dirs.push(role_dir);
                }
            }
        }
    }
    dirs
}

pub(super) async fn collect_behavior_trajectory_metas(
    gcx: AppState,
) -> Vec<BehaviorTrajectoryMeta> {
    let mut dirs = crate::chat::trajectories::get_all_trajectories_dirs(gcx.gcx.clone()).await;
    let tasks_dirs = crate::tasks::storage::get_all_tasks_dirs(gcx.gcx.clone()).await;
    tokio::task::spawn_blocking(move || {
        dirs.extend(collect_behavior_task_trajectory_dirs(
            &tasks_dirs,
            MAX_BEHAVIOR_TASK_DIR_SCAN_ENTRIES,
        ));
        let mut metas = Vec::new();
        let mut seen = HashSet::new();
        let candidates = collect_behavior_trajectory_candidates_from_dirs(
            &dirs,
            MAX_BEHAVIOR_TRAJECTORY_SCAN_FILES,
        );
        collect_behavior_trajectory_metas_from_candidates(candidates, &mut metas, &mut seen);
        metas.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        metas
    })
    .await
    .unwrap_or_else(|err| {
        tracing::warn!("buddy behavior trajectory scan task failed: {err}");
        Vec::new()
    })
}

pub(super) async fn collect_recent_user_snippets(
    gcx: AppState,
    max_snippets: usize,
) -> Vec<TrajectoryUserSnippet> {
    let metas = collect_behavior_trajectory_metas(gcx).await;
    let mut snippets = Vec::new();
    for meta in metas.into_iter().take(30) {
        let Some(value) =
            read_bounded_trajectory_json(&meta.path, MAX_BEHAVIOR_TRAJECTORY_BYTES).await
        else {
            continue;
        };
        let Some(messages) = value.get("messages").and_then(|v| v.as_array()) else {
            continue;
        };
        for msg in messages.iter().rev() {
            if msg.get("role").and_then(|role| role.as_str()) != Some("user") {
                continue;
            }
            let Some(text) = msg
                .get("content")
                .and_then(extract_text_from_trajectory_content)
            else {
                continue;
            };
            let text = redact_and_cap_text(&text, 300);
            if text.trim().is_empty() {
                continue;
            }
            snippets.push(TrajectoryUserSnippet {
                trajectory_id: meta.id.clone(),
                title: redact_and_cap_text(&meta.title, 120),
                mode: meta.mode.clone(),
                updated_at: meta.updated_at.clone(),
                text,
            });
            if snippets.len() >= max_snippets {
                return snippets;
            }
        }
    }
    snippets
}

pub(super) fn behavior_evidence_from_snippets(
    snippets: &[TrajectoryUserSnippet],
) -> Option<AutonomousEvidence> {
    let preference_like_count = snippets
        .iter()
        .filter(|snippet| preference_like(&snippet.text))
        .count();
    if snippets.len() < 4 && preference_like_count < 2 {
        return None;
    }
    let mut lines = vec![format!(
        "recent_user_snippets={} preference_like_snippets={}",
        snippets.len(),
        preference_like_count
    )];
    for snippet in snippets.iter().take(MAX_TRAJECTORY_SNIPPETS) {
        lines.push(format!(
            "trajectory={} title={} mode={} updated={} user_snippet={}",
            redact_and_cap_text(&snippet.trajectory_id, 80),
            snippet.title,
            redact_and_cap_text(&snippet.mode, 80),
            redact_and_cap_text(&snippet.updated_at, 80),
            snippet.text
        ));
    }
    Some(AutonomousEvidence {
        prompt: "Learn stable user behavior and preference signals from recent user-authored snippets only. Produce a concise report and identify only high-confidence preferences that are directly supported by the snippets.".to_string(),
        evidence: lines.join("\n"),
    })
}

pub(super) async fn behavior_learner_evidence(
    gcx: AppState,
) -> Option<(AutonomousEvidence, Vec<PreferenceCandidate>)> {
    let snippets = collect_recent_user_snippets(gcx, MAX_TRAJECTORY_SNIPPETS).await;
    let evidence = behavior_evidence_from_snippets(&snippets)?;
    let candidates = behavior_preference_candidates(&snippets);
    Some((evidence, candidates))
}

pub(super) fn habit_fact_relevant(kind: BuddyFactKind) -> bool {
    matches!(
        kind,
        BuddyFactKind::TaskStuck
            | BuddyFactKind::TaskAbandoned
            | BuddyFactKind::TrajectoryClutter
            | BuddyFactKind::ChatRetryStreak
            | BuddyFactKind::DiagnosticCluster
            | BuddyFactKind::FrontendErrorBurst
    )
}

pub(super) fn habit_evidence(ctx: &BuddyJobContext) -> Option<AutonomousEvidence> {
    let mut lines = vec![format!(
        "task_pulse current_total={} recent_stuck_alerts_1h={} recent_abandoned_alerts_24h={} trajectory_total={} untitled={} oldest_age_days={} diagnostics_last_hour={} top_error_types={}",
        ctx.pulse.tasks.total,
        ctx.pulse.tasks.recent_stuck_alert_count_1h(),
        ctx.pulse.tasks.recent_abandoned_alert_count_24h(),
        ctx.pulse.trajectories.total,
        ctx.pulse.trajectories.untitled,
        ctx.pulse.trajectories.oldest_age_days,
        ctx.pulse.diagnostics.last_hour,
        ctx.pulse.diagnostics.top_error_types.join(", ")
    )];
    let mut task_statuses = ctx.pulse.tasks.by_status.iter().collect::<Vec<_>>();
    task_statuses.sort_by(|a, b| a.0.cmp(b.0));
    for (status, count) in task_statuses {
        lines.push(format!(
            "task_status {}={}",
            redact_and_cap_text(status, 80),
            count
        ));
    }
    for fact in stable_facts(
        ctx.facts
            .iter()
            .filter(|fact| habit_fact_relevant(fact.kind)),
    )
    .into_iter()
    .take(MAX_FACT_EVIDENCE_ITEMS)
    {
        let mut fact_lines = vec![format!(
            "fact kind={} key={} source={} confidence={:.2}",
            fact_kind_name(fact.kind),
            redact_and_cap_text(&fact.key, 160),
            fact.source,
            fact.confidence
        )];
        if let Some(count) = fact.payload.get("count").and_then(|v| v.as_u64()) {
            fact_lines.push(format!("count={}", count));
        }
        if let Some(summary) = fact.payload.get("summary") {
            push_payload_string(&mut fact_lines, "summary", summary);
        }
        if let Some(pattern) = fact.payload.get("pattern") {
            push_payload_string(&mut fact_lines, "pattern", pattern);
        }
        lines.push(fact_lines.join("; "));
    }
    let mut diag_counts: HashMap<String, usize> = HashMap::new();
    for diag in &ctx.recent_diagnostics {
        *diag_counts.entry(diag.error_type.clone()).or_default() += 1;
    }
    let mut diag_counts = diag_counts.into_iter().collect::<Vec<_>>();
    diag_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (error_type, count) in diag_counts.into_iter().take(5) {
        lines.push(format!(
            "diagnostic_pattern type={} count={}",
            redact_and_cap_text(&error_type, 120),
            count
        ));
    }
    let repeated_diagnostics = ctx.recent_diagnostics.len() >= 5;
    let task_pattern = ctx.pulse.tasks.recent_stuck_alert_count_1h() > 0
        || ctx.pulse.tasks.recent_abandoned_alert_count_24h() > 0;
    let trajectory_pattern =
        ctx.pulse.trajectories.untitled >= 5 || ctx.pulse.trajectories.oldest_age_days >= 30;
    let fact_pattern = ctx.facts.iter().any(|fact| habit_fact_relevant(fact.kind));
    if !(repeated_diagnostics || task_pattern || trajectory_pattern || fact_pattern) {
        return None;
    }
    Some(AutonomousEvidence {
        prompt: "Look for repeated workflow habits or friction patterns from metadata only. Coach the user with practical next-step suggestions without citing raw tool outputs or private content.".to_string(),
        evidence: lines.join("\n"),
    })
}

pub(super) fn bucket_rate(part: usize, total: usize) -> String {
    if total == 0 {
        return "0%".to_string();
    }
    let rate = part as f64 * 100.0 / total as f64;
    if rate == 0.0 {
        "0%".to_string()
    } else if rate < 10.0 {
        "under_10%".to_string()
    } else if rate < 25.0 {
        "10-24%".to_string()
    } else if rate < 50.0 {
        "25-49%".to_string()
    } else if rate < 75.0 {
        "50-74%".to_string()
    } else {
        "75%+".to_string()
    }
}

pub(super) fn bucket_count(value: usize) -> String {
    match value {
        0 => "0".to_string(),
        1..=4 => "1-4".to_string(),
        5..=9 => "5-9".to_string(),
        10..=24 => "10-24".to_string(),
        25..=49 => "25-49".to_string(),
        50..=99 => "50-99".to_string(),
        100..=249 => "100-249".to_string(),
        250..=499 => "250-499".to_string(),
        _ => "500+".to_string(),
    }
}

pub(super) fn bucket_tokens(value: usize) -> String {
    match value {
        0 => "0".to_string(),
        1..=9_999 => "1-9k".to_string(),
        10_000..=49_999 => "10k-49k".to_string(),
        50_000..=99_999 => "50k-99k".to_string(),
        100_000..=249_999 => "100k-249k".to_string(),
        250_000..=499_999 => "250k-499k".to_string(),
        500_000..=999_999 => "500k-999k".to_string(),
        _ => "1m+".to_string(),
    }
}

pub(super) fn bucket_cost(value: f64) -> String {
    if value <= 0.0 {
        "0".to_string()
    } else if value < 0.10 {
        "under_0.10".to_string()
    } else if value < 0.50 {
        "0.10-0.49".to_string()
    } else if value < 1.00 {
        "0.50-0.99".to_string()
    } else if value < 5.00 {
        "1-4.99".to_string()
    } else if value < 10.00 {
        "5-9.99".to_string()
    } else {
        "10+".to_string()
    }
}

pub(super) fn bucket_duration_ms(value: u64) -> String {
    match value {
        0..=4_999 => "under_5s".to_string(),
        5_000..=9_999 => "5s-9s".to_string(),
        10_000..=19_999 => "10s-19s".to_string(),
        20_000..=39_999 => "20s-39s".to_string(),
        40_000..=59_999 => "40s-59s".to_string(),
        _ => "60s+".to_string(),
    }
}

#[async_trait::async_trait]
impl BuddyJob for BuddyBehaviorLearnerJob {
    fn id(&self) -> &str {
        BEHAVIOR_LEARNER_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        BEHAVIOR_COOLDOWN_SECS
    }

    fn priority(&self) -> u32 {
        22
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        ctx.pulse.trajectories.total >= 4
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let Some((evidence, candidates)) = behavior_learner_evidence(gcx.clone()).await else {
            return BuddyJobResult::default();
        };
        let spec = build_spec(self.id(), evidence);
        if same_signal_or_failed_backoff(&ctx, &spec.signal_hash, self.cooldown_seconds()) {
            return preserve_last_result(&ctx);
        }
        let mut result =
            execute_autonomous_spec(gcx.clone(), &ctx, spec, self.cooldown_seconds()).await;
        if result.last_result.is_some() {
            let queued = enqueue_behavior_preferences(gcx, &candidates).await;
            if queued > 0 {
                if let Some(activity) = result.activity.as_mut() {
                    activity.description = format!(
                        "{} Queued {} high-confidence preference candidate{} for review.",
                        activity.description,
                        queued,
                        if queued == 1 { "" } else { "s" }
                    );
                }
            }
        }
        result
    }
}

#[async_trait::async_trait]
impl BuddyJob for BuddyUserHabitCoachJob {
    fn id(&self) -> &str {
        USER_HABIT_COACH_WORKFLOW_ID
    }

    fn cooldown_seconds(&self) -> u64 {
        MEMORY_COOLDOWN_SECS
    }

    fn priority(&self) -> u32 {
        23
    }

    async fn should_run(&self, _gcx: AppState, ctx: &BuddyJobContext) -> bool {
        let Some(evidence) = habit_evidence(ctx) else {
            return false;
        };
        !same_signal_or_failed_backoff(
            ctx,
            &build_spec(self.id(), evidence).signal_hash,
            self.cooldown_seconds(),
        )
    }

    async fn execute(&self, gcx: AppState, ctx: BuddyJobContext) -> BuddyJobResult {
        let Some(evidence) = habit_evidence(&ctx) else {
            return BuddyJobResult::default();
        };
        execute_autonomous_spec(
            gcx,
            &ctx,
            build_spec(self.id(), evidence),
            self.cooldown_seconds(),
        )
        .await
    }
}
