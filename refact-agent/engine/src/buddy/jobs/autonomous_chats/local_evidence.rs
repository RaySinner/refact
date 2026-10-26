use super::*;

pub(super) const MAX_DIAGNOSTIC_EVIDENCE: usize = 20;
pub(super) const MAX_SECURITY_FINDINGS: usize = 20;
pub(super) const MAX_PATH_EVIDENCE: usize = 40;
pub(super) const MAX_MANIFEST_WALK_FILES: usize = 5_000;
pub(super) const MAX_UNTRACKED_CONTENT_BYTES: u64 = 64 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiagnosticEvidence {
    pub(super) repeated_error_type: Option<String>,
    pub(super) repeated_count: usize,
    pub(super) high_or_critical_count: usize,
    pub(super) summaries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityFinding {
    pub path: String,
    pub source: String,
    pub kind: String,
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DependencyManifestEvidence {
    pub changed_manifests: Vec<PathStatus>,
    pub manifest_counts: BTreeMap<String, usize>,
    pub total_manifest_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocsEvidence {
    pub changed_code_paths: Vec<PathStatus>,
    pub changed_doc_paths: Vec<PathStatus>,
    pub has_readme: bool,
    pub has_agents: bool,
    pub docs_file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArchitectureEvidence {
    pub changed_file_count: usize,
    pub additions: usize,
    pub deletions: usize,
    pub path_groups: Vec<PathGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStatus {
    pub path: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathGroup {
    pub group: String,
    pub count: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct LocalGitEvidence {
    pub(super) changed_paths: Vec<PathStatus>,
    pub(super) additions: usize,
    pub(super) deletions: usize,
    pub(super) security_findings: Vec<SecurityFinding>,
}

#[derive(Debug, Clone)]
pub(super) struct SecurityScanContent {
    pub(super) source: &'static str,
    pub(super) content: String,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct AutonomousJobDefinition {
    pub(super) meta: &'static AutonomousWorkflowMeta,
    pub(super) cooldown_seconds: u64,
    pub(super) scheduler_priority: u32,
    pub(super) prompt: &'static str,
}

pub(super) fn diagnostic_evidence(ctx: &BuddyJobContext) -> Option<DiagnosticEvidence> {
    let recent: Vec<&DiagnosticContext> = ctx.recent_diagnostics.iter().rev().take(50).collect();
    if recent.is_empty() {
        return None;
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut high_or_critical_count = 0;
    for diag in &recent {
        *counts.entry(diag.error_type.clone()).or_default() += 1;
        if matches!(
            diag.severity,
            DiagnosticSeverity::High | DiagnosticSeverity::Critical
        ) {
            high_or_critical_count += 1;
        }
    }

    let repeated = counts
        .into_iter()
        .filter(|(_, count)| *count >= 3)
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)));
    if repeated.is_none() && high_or_critical_count == 0 {
        return None;
    }

    let summaries = recent
        .into_iter()
        .take(MAX_DIAGNOSTIC_EVIDENCE)
        .map(format_diagnostic_summary)
        .collect();
    Some(DiagnosticEvidence {
        repeated_error_type: repeated.as_ref().map(|(error_type, _)| error_type.clone()),
        repeated_count: repeated.map(|(_, count)| count).unwrap_or(0),
        high_or_critical_count,
        summaries,
    })
}

pub(super) fn format_diagnostic_summary(diag: &DiagnosticContext) -> String {
    let source = diag
        .source_file
        .as_deref()
        .or(diag.tool_name.as_deref())
        .unwrap_or("unknown");
    let preview = preview_text(&diag.error_message, 240);
    let user_error = refact_core::retry_policy::user_error_info(
        refact_core::retry_policy::classify_user_error(&diag.error_message),
    );
    // NOTE: collected_at (timestamp) is intentionally excluded so the same recurring cluster
    // renders a stable summary and does not re-trigger the investigation every occurrence.
    format!(
        "type={} severity={:?} category={:?} recoverable={} source={} preview={}",
        clean_evidence_value(&diag.error_type),
        diag.severity,
        user_error.category,
        user_error.is_retryable,
        clean_evidence_value(source),
        preview
    )
}

/// Stable signal for a diagnostic cluster: error type + bucketed counts + timestamp-free
/// per-summary identities. Excludes volatile timestamps and exact counts so repeated
/// occurrences of the same cluster hash identically (fixes endless re-triggering).
pub(super) fn stable_diagnostic_signal(evidence: &DiagnosticEvidence) -> String {
    fn bucket(n: usize) -> usize {
        // Round down to the nearest power of two so slow count growth does not change identity.
        if n == 0 {
            0
        } else {
            1usize << (usize::BITS - 1 - n.leading_zeros())
        }
    }
    let mut parts = vec![
        format!(
            "type={}",
            evidence.repeated_error_type.as_deref().unwrap_or("none")
        ),
        format!("count_bucket={}", bucket(evidence.repeated_count)),
        format!(
            "highcrit_bucket={}",
            bucket(evidence.high_or_critical_count)
        ),
    ];
    let mut summaries: Vec<String> = evidence
        .summaries
        .iter()
        .map(|summary| refact_buddy_core::memory_dedup::normalize_memory_text(summary))
        .filter(|summary| !summary.is_empty())
        .collect();
    summaries.sort();
    summaries.dedup();
    parts.extend(summaries);
    parts.join("|")
}

pub(super) fn render_diagnostic_evidence(evidence: &DiagnosticEvidence) -> String {
    let mut lines = vec![
        "Diagnostic signal:".to_string(),
        format!(
            "- repeated_error_type: {}",
            evidence.repeated_error_type.as_deref().unwrap_or("none")
        ),
        format!("- repeated_count: {}", evidence.repeated_count),
        format!(
            "- high_or_critical_count: {}",
            evidence.high_or_critical_count
        ),
        "- summaries:".to_string(),
    ];
    for summary in &evidence.summaries {
        lines.push(format!("  - {summary}"));
    }
    lines.join("\n")
}

pub(super) fn setup_evidence(project_root: &Path) -> Option<String> {
    let has_agents = project_root.join("AGENTS.md").exists();
    let has_refact = project_root.join(".refact").exists();
    let has_refact_knowledge = project_root.join(".refact").join("knowledge").exists();
    let has_refact_tasks = project_root.join(".refact").join("tasks").exists();
    let has_readme = has_root_file_case_insensitive(project_root, "readme");
    let has_git = git2::Repository::discover(project_root).is_ok();
    if has_agents && has_refact && has_refact_knowledge && has_readme {
        return None;
    }

    Some(
        [
            "Local setup checklist:".to_string(),
            format!("- AGENTS.md present: {has_agents}"),
            format!("- README present: {has_readme}"),
            format!("- .refact directory present: {has_refact}"),
            format!("- .refact/knowledge present: {has_refact_knowledge}"),
            format!("- .refact/tasks present: {has_refact_tasks}"),
            format!("- git workspace detected: {has_git}"),
        ]
        .join("\n"),
    )
}

pub(super) fn has_root_file_case_insensitive(project_root: &Path, stem: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(project_root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|name| name.to_ascii_lowercase().starts_with(stem))
            .unwrap_or(false)
    })
}

pub(super) fn collect_local_git_evidence(
    project_root: &Path,
    scan_security: bool,
) -> Option<LocalGitEvidence> {
    track_local_git_evidence_scan(project_root);
    let repo = git2::Repository::discover(project_root).ok()?;
    let repo_root = repo.workdir()?.to_path_buf();
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .show(git2::StatusShow::IndexAndWorkdir);
    let statuses = repo.statuses(Some(&mut opts)).ok()?;
    let mut changed_paths = Vec::new();
    let mut seen = HashSet::new();
    let mut security_findings = Vec::new();
    let mut seen_security_findings = HashSet::new();
    let mut status_entries = statuses
        .iter()
        .filter_map(|entry| {
            let status = entry.status();
            if status.is_empty() {
                return None;
            }
            entry.path().map(|path| (path.to_string(), status))
        })
        .collect::<Vec<_>>();
    status_entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (path, status) in status_entries {
        if seen.insert(path.clone()) {
            changed_paths.push(PathStatus {
                path: path.clone(),
                status: git_status_label(status).to_string(),
            });
        }
        if scan_security && security_findings.len() < MAX_SECURITY_FINDINGS {
            for scan_content in git_security_scan_contents(&repo, &repo_root, &path, status) {
                push_security_findings(
                    &mut security_findings,
                    &mut seen_security_findings,
                    &path,
                    scan_content.source,
                    &scan_content.content,
                );
                if security_findings.len() >= MAX_SECURITY_FINDINGS {
                    break;
                }
            }
        }
    }

    let (additions, deletions) = diff_stats(&repo).unwrap_or((0, 0));
    changed_paths.sort_by(|a, b| a.path.cmp(&b.path));
    changed_paths.truncate(MAX_PATH_EVIDENCE);
    Some(LocalGitEvidence {
        changed_paths,
        additions,
        deletions,
        security_findings,
    })
}

#[cfg(test)]
pub(super) type LocalGitEvidenceScanTracker = HashMap<PathBuf, usize>;

#[cfg(test)]
pub(super) static LOCAL_GIT_EVIDENCE_SCAN_TRACKER: OnceLock<
    std::sync::Mutex<LocalGitEvidenceScanTracker>,
> = OnceLock::new();

#[cfg(test)]
pub(super) fn local_git_evidence_scan_tracker(
) -> &'static std::sync::Mutex<LocalGitEvidenceScanTracker> {
    LOCAL_GIT_EVIDENCE_SCAN_TRACKER.get_or_init(Default::default)
}

#[cfg(test)]
pub(super) fn normalize_local_git_evidence_scan_root(project_root: &Path) -> PathBuf {
    project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf())
}

#[cfg(test)]
pub(super) fn track_local_git_evidence_scan(project_root: &Path) {
    let root = normalize_local_git_evidence_scan_root(project_root);
    let mut tracker = local_git_evidence_scan_tracker().lock().unwrap();
    *tracker.entry(root).or_default() += 1;
}

#[cfg(not(test))]
pub(super) fn track_local_git_evidence_scan(_project_root: &Path) {}

#[cfg(test)]
pub(super) fn reset_local_git_evidence_scan_count(root: &Path) {
    let root = normalize_local_git_evidence_scan_root(root);
    local_git_evidence_scan_tracker()
        .lock()
        .unwrap()
        .insert(root, 0);
}

#[cfg(test)]
pub(super) fn local_git_evidence_scan_count(root: &Path) -> usize {
    let root = normalize_local_git_evidence_scan_root(root);
    local_git_evidence_scan_tracker()
        .lock()
        .unwrap()
        .get(&root)
        .copied()
        .unwrap_or_default()
}

pub(super) fn git_status_label(status: git2::Status) -> &'static str {
    if status.is_conflicted() {
        "conflicted"
    } else if status.intersects(git2::Status::INDEX_NEW | git2::Status::WT_NEW) {
        "added"
    } else if status.intersects(git2::Status::INDEX_DELETED | git2::Status::WT_DELETED) {
        "deleted"
    } else if status.intersects(git2::Status::INDEX_RENAMED | git2::Status::WT_RENAMED) {
        "renamed"
    } else if status.intersects(git2::Status::INDEX_TYPECHANGE | git2::Status::WT_TYPECHANGE) {
        "typechanged"
    } else {
        "modified"
    }
}

pub(super) fn should_scan_index_status(status: git2::Status) -> bool {
    status.intersects(
        git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED
            | git2::Status::INDEX_RENAMED
            | git2::Status::INDEX_TYPECHANGE,
    )
}

pub(super) fn should_scan_worktree_status(status: git2::Status) -> bool {
    status.intersects(
        git2::Status::WT_NEW
            | git2::Status::WT_MODIFIED
            | git2::Status::WT_RENAMED
            | git2::Status::WT_TYPECHANGE,
    )
}

pub(super) fn git_security_scan_contents(
    repo: &git2::Repository,
    repo_root: &Path,
    path: &str,
    status: git2::Status,
) -> Vec<SecurityScanContent> {
    let mut contents = Vec::new();
    if should_scan_index_status(status) {
        if let Ok(Some(content)) = git_index_content(repo, path) {
            contents.push(SecurityScanContent {
                source: "index",
                content,
            });
        }
    }

    if should_scan_worktree_status(status) {
        if let Ok(Some(content)) = git_workdir_content(repo_root, path) {
            contents.push(SecurityScanContent {
                source: "worktree",
                content,
            });
        }
    }

    contents
}

pub(super) fn git_workdir_content(repo_root: &Path, path: &str) -> Result<Option<String>, String> {
    let workdir_path = repo_root.join(path);
    read_file_to_string_capped(&workdir_path, MAX_UNTRACKED_CONTENT_BYTES)
}

pub(super) fn git_index_content(
    repo: &git2::Repository,
    path: &str,
) -> Result<Option<String>, String> {
    let index = repo.index().map_err(|e| e.to_string())?;
    let Some(entry) = index.get_path(Path::new(path), 0) else {
        return Ok(None);
    };
    let blob = repo.find_blob(entry.id).map_err(|e| e.to_string())?;
    if blob.size() > MAX_UNTRACKED_CONTENT_BYTES as usize {
        return Ok(None);
    }
    let Ok(content) = std::str::from_utf8(blob.content()) else {
        return Ok(None);
    };
    if content.contains('\0') {
        return Ok(None);
    }
    Ok(Some(content.to_string()))
}

pub(super) fn read_file_to_string_capped(
    path: &Path,
    max_bytes: u64,
) -> Result<Option<String>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Ok(None);
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut content = String::new();
    let mut reader = std::io::Read::take(file, max_bytes);
    std::io::Read::read_to_string(&mut reader, &mut content).map_err(|e| e.to_string())?;
    if content.contains('\0') {
        return Ok(None);
    }
    Ok(Some(content))
}

pub(super) fn diff_stats(repo: &git2::Repository) -> Option<(usize, usize)> {
    let root = repo.workdir()?;
    let staged_numstat =
        crate::worktrees::git::run_git_lossy(root, &["diff", "--cached", "--numstat"]);
    let unstaged_numstat = crate::worktrees::git::run_git_lossy(root, &["diff", "--numstat"]);
    let untracked_numstat =
        crate::worktrees::git::run_git_lossy(root, &["ls-files", "--others", "--exclude-standard"]);
    let mut additions = 0usize;
    let mut deletions = 0usize;
    accumulate_numstat(&staged_numstat, &mut additions, &mut deletions);
    accumulate_numstat(&unstaged_numstat, &mut additions, &mut deletions);
    let mut untracked_paths = untracked_numstat
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    untracked_paths.sort_unstable();
    for rel in untracked_paths.into_iter().take(MAX_PATH_EVIDENCE) {
        let path = root.join(rel);
        if let Some(content) = read_untracked_diffstat_content(&path) {
            additions = additions.saturating_add(content.lines().count());
        }
    }
    Some((additions, deletions))
}

pub(super) fn read_untracked_diffstat_content(path: &Path) -> Option<String> {
    read_file_to_string_capped(path, MAX_UNTRACKED_CONTENT_BYTES)
        .ok()
        .flatten()
}

pub(super) fn accumulate_numstat(output: &str, additions: &mut usize, deletions: &mut usize) {
    for line in output.lines() {
        let mut parts = line.split('\t');
        if let Some(value) = parts.next().and_then(|part| part.parse::<usize>().ok()) {
            *additions = additions.saturating_add(value);
        }
        if let Some(value) = parts.next().and_then(|part| part.parse::<usize>().ok()) {
            *deletions = deletions.saturating_add(value);
        }
    }
}

pub(super) fn scan_security_findings(
    path: &str,
    content: &str,
    limit: usize,
) -> Vec<SecurityFinding> {
    scan_security_findings_from_source(path, "worktree", content, limit)
}

pub(super) fn scan_security_findings_from_source(
    path: &str,
    source: &str,
    content: &str,
    limit: usize,
) -> Vec<SecurityFinding> {
    if limit == 0 {
        return Vec::new();
    }

    // Collect (kind, start, end) for every match across all patterns.
    // We then suppress `assigned_secret` matches that overlap with a more
    // specific pattern (github_token, gitlab_token, openai_key, bearer_token,
    // authorization_header) so generic catches don't duplicate or override
    // precise detections.
    let mut raw: Vec<(&'static str, usize, usize)> = Vec::new();
    for (kind, regex) in secret_patterns().iter() {
        for m in regex.find_iter(content) {
            raw.push((*kind, m.start(), m.end()));
        }
    }

    let mut suppressed_ranges: Vec<(usize, usize)> = Vec::new();
    for (kind, start, end) in &raw {
        if *kind != ASSIGNED_SECRET_KIND {
            suppressed_ranges.push((*start, *end));
        }
    }

    let mut findings = Vec::new();
    for (kind, start, end) in raw {
        if kind == ASSIGNED_SECRET_KIND
            && suppressed_ranges
                .iter()
                .any(|(s, e)| start < *e && end > *s)
        {
            continue;
        }
        if findings.len() >= limit {
            break;
        }
        findings.push(SecurityFinding {
            path: path.to_string(),
            source: source.to_string(),
            kind: kind.to_string(),
            preview: security_preview(content, start, end),
        });
    }
    findings
}

pub(super) const ASSIGNED_SECRET_KIND: &str = "assigned_secret";

pub(super) fn push_security_findings(
    findings: &mut Vec<SecurityFinding>,
    seen: &mut HashSet<(String, String, String, String)>,
    path: &str,
    source: &str,
    content: &str,
) {
    for finding in scan_security_findings_from_source(path, source, content, MAX_SECURITY_FINDINGS)
    {
        let key = (
            finding.path.clone(),
            finding.source.clone(),
            finding.kind.clone(),
            finding.preview.clone(),
        );
        if seen.insert(key) {
            findings.push(finding);
            if findings.len() >= MAX_SECURITY_FINDINGS {
                break;
            }
        }
    }
}

pub(super) fn secret_patterns() -> &'static [(&'static str, Regex)] {
    static PATTERNS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            (
                "bearer_token",
                Regex::new(r#"(?i)Bearer\s+[^\s"',]+"#).unwrap(),
            ),
            (
                "openai_key",
                Regex::new(r#"\bsk-[A-Za-z0-9]{8,}\b"#).unwrap(),
            ),
            (
                "github_token",
                Regex::new(r#"(?i)\bghp_[A-Za-z0-9]{10,}\b"#).unwrap(),
            ),
            (
                "gitlab_token",
                Regex::new(r#"(?i)\bglpat-[A-Za-z0-9_-]{10,}\b"#).unwrap(),
            ),
            (
                "assigned_secret",
                Regex::new(
                    r#"(?i)["']?\b(api[_-]?key|apikey|(?:access|refresh|id|client)[_-]?token|client[_-]?secret|private[_-]?key|password|secret|token)\b["']?\s*[:=]\s*["']?[^"'\s,;]{12,}"#,
                )
                .unwrap(),
            ),
            (
                "authorization_header",
                Regex::new(r#"(?i)["']?\bAuthorization\b["']?\s*[:=]\s*["']?[^"'\n,;]+"#).unwrap(),
            ),
        ]
    })
}

pub(super) fn security_preview(content: &str, start: usize, end: usize) -> String {
    let line_start = content[..start].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    let line_end = content[end..]
        .find('\n')
        .map(|idx| end + idx)
        .unwrap_or(content.len());
    let line = &content[line_start..line_end];
    let secret_start = start.saturating_sub(line_start).min(line.len());
    let secret_end = end.saturating_sub(line_start).min(line.len());
    let mut masked = String::with_capacity(line.len());
    masked.push_str(&line[..secret_start]);
    masked.push_str("[REDACTED_SECRET]");
    masked.push_str(&line[secret_end..]);
    preview_text(&masked, 200)
}

pub(super) fn diagnostic_security_findings(
    diagnostics: &[DiagnosticContext],
) -> Vec<SecurityFinding> {
    diagnostics
        .iter()
        .rev()
        .take(MAX_DIAGNOSTIC_EVIDENCE)
        .flat_map(|diag| {
            let source = diag
                .source_file
                .as_deref()
                .or(diag.tool_name.as_deref())
                .unwrap_or("diagnostic");
            scan_security_findings(source, &diag.error_message, MAX_SECURITY_FINDINGS)
        })
        .take(MAX_SECURITY_FINDINGS)
        .collect()
}

pub(super) fn render_security_evidence(findings: &[SecurityFinding]) -> String {
    let mut lines = vec!["Redacted local security findings:".to_string()];
    for finding in findings.iter().take(MAX_SECURITY_FINDINGS) {
        let include_source = finding.source != "worktree"
            || findings
                .iter()
                .any(|other| other.path == finding.path && other.source != finding.source);
        if include_source {
            lines.push(format!(
                "- path={} source={} kind={} preview={}",
                clean_evidence_value(&finding.path),
                clean_evidence_value(&finding.source),
                clean_evidence_value(&finding.kind),
                finding.preview
            ));
        } else {
            lines.push(format!(
                "- path={} kind={} preview={}",
                clean_evidence_value(&finding.path),
                clean_evidence_value(&finding.kind),
                finding.preview
            ));
        }
    }
    lines.join("\n")
}

pub(super) fn dependency_manifest_evidence(
    git: Option<&LocalGitEvidence>,
    project_root: &Path,
) -> DependencyManifestEvidence {
    let changed_manifests = git
        .map(|evidence| {
            evidence
                .changed_paths
                .iter()
                .filter(|path| classify_dependency_manifest(&path.path).is_some())
                .take(MAX_PATH_EVIDENCE)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let manifest_counts = count_dependency_manifests(project_root);
    let total_manifest_count = manifest_counts.values().copied().sum();
    DependencyManifestEvidence {
        changed_manifests,
        manifest_counts,
        total_manifest_count,
    }
}

pub(super) fn dependency_manifest_trigger(evidence: &DependencyManifestEvidence) -> bool {
    !evidence.changed_manifests.is_empty() || evidence.total_manifest_count >= 12
}

pub(super) fn render_dependency_evidence(evidence: &DependencyManifestEvidence) -> String {
    let mut lines = vec![
        "Local dependency manifest evidence:".to_string(),
        format!("- total_manifest_count: {}", evidence.total_manifest_count),
        "- manifest_counts:".to_string(),
    ];
    for (kind, count) in &evidence.manifest_counts {
        lines.push(format!("  - {kind}: {count}"));
    }
    lines.push("- changed_manifests:".to_string());
    for path in &evidence.changed_manifests {
        lines.push(format!(
            "  - {} ({})",
            clean_evidence_value(&path.path),
            clean_evidence_value(&path.status)
        ));
    }
    lines.join("\n")
}

pub(super) fn classify_dependency_manifest(path: &str) -> Option<&'static str> {
    let normalized = path.replace('\\', "/");
    let file = normalized.rsplit('/').next()?.to_ascii_lowercase();
    match file.as_str() {
        "package.json" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb" => {
            Some("javascript")
        }
        "cargo.toml" | "cargo.lock" => Some("rust"),
        "pyproject.toml" | "poetry.lock" | "pdm.lock" | "requirements.txt" | "pipfile"
        | "pipfile.lock" => Some("python"),
        "go.mod" | "go.sum" => Some("go"),
        "pom.xml" | "build.gradle" | "build.gradle.kts" | "gradle.lockfile" => Some("jvm"),
        "gemfile" | "gemfile.lock" => Some("ruby"),
        "composer.json" | "composer.lock" => Some("php"),
        "mix.exs" | "mix.lock" => Some("elixir"),
        "packages.config" | "paket.dependencies" | "paket.lock" => Some("dotnet"),
        _ => None,
    }
}

pub(super) fn count_dependency_manifests(project_root: &Path) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let mut stack = vec![project_root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if scanned >= MAX_MANIFEST_WALK_FILES {
                return counts;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if should_skip_dir(&name) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            scanned += 1;
            let Ok(rel) = path.strip_prefix(project_root) else {
                continue;
            };
            if let Some(kind) = classify_dependency_manifest(&rel.to_string_lossy()) {
                *counts.entry(kind.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
}

pub(super) fn docs_evidence(git: Option<&LocalGitEvidence>, project_root: &Path) -> DocsEvidence {
    let changed_paths = git
        .map(|evidence| evidence.changed_paths.as_slice())
        .unwrap_or_default();
    let changed_code_paths = changed_paths
        .iter()
        .filter(|path| is_code_path(&path.path))
        .take(MAX_PATH_EVIDENCE)
        .cloned()
        .collect();
    let changed_doc_paths = changed_paths
        .iter()
        .filter(|path| is_docs_path(&path.path))
        .take(MAX_PATH_EVIDENCE)
        .cloned()
        .collect();
    DocsEvidence {
        changed_code_paths,
        changed_doc_paths,
        has_readme: has_root_file_case_insensitive(project_root, "readme"),
        has_agents: project_root.join("AGENTS.md").exists(),
        docs_file_count: count_docs_files(project_root),
    }
}

pub(super) fn docs_trigger(evidence: &DocsEvidence) -> bool {
    (!evidence.changed_code_paths.is_empty() && evidence.changed_doc_paths.is_empty())
        || !evidence.has_readme
        || !evidence.has_agents
        || evidence.docs_file_count == 0
}

pub(super) fn render_docs_evidence(evidence: &DocsEvidence) -> String {
    let mut lines = vec![
        "Docs signal:".to_string(),
        format!("- has_readme: {}", evidence.has_readme),
        format!("- has_agents: {}", evidence.has_agents),
        format!("- docs_file_count: {}", evidence.docs_file_count),
        format!(
            "- changed_code_path_count: {}",
            evidence.changed_code_paths.len()
        ),
        format!(
            "- changed_doc_path_count: {}",
            evidence.changed_doc_paths.len()
        ),
        "- changed_code_paths:".to_string(),
    ];
    for path in &evidence.changed_code_paths {
        lines.push(format!(
            "  - {} ({})",
            clean_evidence_value(&path.path),
            clean_evidence_value(&path.status)
        ));
    }
    lines.push("- changed_doc_paths:".to_string());
    for path in &evidence.changed_doc_paths {
        lines.push(format!(
            "  - {} ({})",
            clean_evidence_value(&path.path),
            clean_evidence_value(&path.status)
        ));
    }
    lines.join("\n")
}

pub(super) fn is_docs_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let file = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    normalized.starts_with("docs/")
        || normalized.contains("/docs/")
        || file.starts_with("readme")
        || file.starts_with("changelog")
        || file == "agents.md"
        || file.ends_with(".md")
        || file.ends_with(".mdx")
        || file.ends_with(".rst")
        || file.ends_with(".adoc")
}

pub(super) fn is_code_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if is_docs_path(&normalized) || normalized.starts_with(".refact/") {
        return false;
    }
    let Some(file) = normalized.rsplit('/').next() else {
        return false;
    };
    if classify_dependency_manifest(file).is_some() {
        return false;
    }
    matches!(
        file.rsplit('.').next().unwrap_or_default(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "cpp"
            | "cc"
            | "c"
            | "h"
            | "hpp"
            | "cs"
            | "php"
            | "rb"
            | "swift"
            | "scala"
            | "sql"
            | "yaml"
            | "yml"
            | "toml"
    )
}

pub(super) fn count_docs_files(project_root: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![project_root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if scanned >= MAX_MANIFEST_WALK_FILES {
                return count;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if should_skip_dir(&name) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            scanned += 1;
            let Ok(rel) = path.strip_prefix(project_root) else {
                continue;
            };
            if is_docs_path(&rel.to_string_lossy()) {
                count += 1;
            }
        }
    }
    count
}

pub(super) fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | "vendor" | "dist" | "build" | ".next" | ".cache"
    )
}

pub(super) fn architecture_evidence(git: &LocalGitEvidence) -> ArchitectureEvidence {
    let relevant_paths: Vec<PathStatus> = git
        .changed_paths
        .iter()
        .filter(|path| is_code_path(&path.path) || is_docs_path(&path.path))
        .cloned()
        .collect();
    let path_groups = architecture_groups(&relevant_paths);
    ArchitectureEvidence {
        changed_file_count: relevant_paths.len(),
        additions: git.additions,
        deletions: git.deletions,
        path_groups,
    }
}

pub(super) fn architecture_trigger(evidence: &ArchitectureEvidence) -> bool {
    let total_lines = evidence.additions + evidence.deletions;
    let largest_group = evidence
        .path_groups
        .iter()
        .map(|group| group.count)
        .max()
        .unwrap_or(0);
    largest_group >= 8
        || evidence.path_groups.len() >= 5 && evidence.changed_file_count >= 12
        || total_lines >= 700
        || evidence.changed_file_count >= 25
}

pub(super) fn architecture_groups(paths: &[PathStatus]) -> Vec<PathGroup> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for path in paths {
        let group = architecture_group_for_path(&path.path);
        *counts.entry(group).or_insert(0) += 1;
    }
    let mut groups: Vec<PathGroup> = counts
        .into_iter()
        .map(|(group, count)| PathGroup { group, count })
        .collect();
    groups.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.group.cmp(&b.group)));
    groups.truncate(10);
    groups
}

pub(super) fn architecture_group_for_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut parts = normalized.split('/').filter(|part| !part.is_empty());
    match (parts.next(), parts.next()) {
        (Some("src"), Some(next)) => format!("src/{next}"),
        (Some("refact-agent"), Some(next)) => format!("refact-agent/{next}"),
        (Some("extra"), Some(next)) => format!("extra/{next}"),
        (Some("crates"), Some(next)) => format!("crates/{next}"),
        (Some(first), _) => first.to_string(),
        _ => "root".to_string(),
    }
}

pub(super) fn render_architecture_evidence(evidence: &ArchitectureEvidence) -> String {
    let mut lines = vec![
        "Architecture drift signal:".to_string(),
        format!("- changed_file_count: {}", evidence.changed_file_count),
        format!("- additions: {}", evidence.additions),
        format!("- deletions: {}", evidence.deletions),
        "- path_groups:".to_string(),
    ];
    for group in &evidence.path_groups {
        lines.push(format!(
            "  - {}: {} files",
            clean_evidence_value(&group.group),
            group.count
        ));
    }
    lines.join("\n")
}
