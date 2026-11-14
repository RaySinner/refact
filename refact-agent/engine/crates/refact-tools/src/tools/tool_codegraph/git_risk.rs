#[allow(unused_imports)]
use super::*;

pub struct GitRiskFileAssembly {
    pub meta: refact_codehealth::git_biomarkers::GitMeta,
    pub findings: Vec<refact_codehealth::biomarkers::Finding>,
    pub temporal_score: f64,
    pub churn_risk: f64,
}
pub struct GitRiskAssembly {
    pub files: Vec<GitRiskFileAssembly>,
    pub recent_commit_risks: Vec<RecentCommitRiskSummary>,
}
pub fn build_git_meta(
    intel: &refact_git_intel::GitIntel,
    repo_root: &Path,
    path_repo_relative: &str,
    graph: Option<&HealthGraphSnapshot>,
    function_ranges: Option<&[(String, usize, usize)]>,
) -> refact_codehealth::git_biomarkers::GitMeta {
    let now_ts = current_unix_ts();
    let entropy_map = intel.change_entropy();
    let (added_90d, deleted_90d) = intel.lines_in_window(path_repo_relative, now_ts, 90);
    let (primary_owner_name, primary_owner_commit_pct) = intel.primary_owner(path_repo_relative);
    let (recent_owner_name, recent_owner_commit_pct) =
        intel.recent_owner(path_repo_relative, now_ts, 90);
    let functions = match function_ranges {
        Some(ranges) => build_function_git_facts(repo_root, path_repo_relative, ranges, now_ts),
        None => Vec::new(),
    };
    let repo_function_mod_p80 = function_mod_p80(&functions);
    let import_edges = graph
        .and_then(|graph| graph.import_index.as_ref())
        .map(|index| index.import_edges(path_repo_relative))
        .unwrap_or_default();

    refact_codehealth::git_biomarkers::GitMeta {
        file_path: path_repo_relative.to_string(),
        change_entropy: entropy_map.get(path_repo_relative).copied().unwrap_or(0.0),
        change_entropy_pct: intel.change_entropy_pct(path_repo_relative),
        commit_count_90d: intel.commit_count_in_window(path_repo_relative, now_ts, 90),
        commit_count_total: intel
            .file_churn
            .get(path_repo_relative)
            .copied()
            .unwrap_or(0),
        is_hotspot: intel.is_hotspot_file(path_repo_relative, GIT_META_FUNCTION_FACT_TOP_K),
        is_stable: file_is_stable(intel, path_repo_relative, now_ts),
        churn_percentile: intel.churn_percentile(path_repo_relative),
        lines_added_90d: added_90d,
        lines_deleted_90d: deleted_90d,
        nloc: nloc_for_file(repo_root, path_repo_relative),
        contributor_count: intel
            .file_authors
            .get(path_repo_relative)
            .map(|authors| authors.len().min(u32::MAX as usize) as u32)
            .unwrap_or(0),
        primary_owner_commit_pct,
        primary_owner_name,
        recent_owner_name,
        recent_owner_commit_pct,
        bus_factor: intel.bus_factor(path_repo_relative) as u32,
        prior_defect_count: intel.prior_defects(path_repo_relative),
        repo_active_contributors_90d: Some(intel.active_contributors_in_window(now_ts, 90)),
        repo_function_mod_p80,
        co_change_partners: intel
            .co_change_partners(path_repo_relative, 1)
            .into_iter()
            .map(
                |(path, count)| refact_codehealth::git_biomarkers::CoChangePartner {
                    path,
                    co_change_count: count as f64,
                },
            )
            .collect(),
        top_authors: intel
            .ownership(path_repo_relative)
            .into_iter()
            .map(|owner| (owner.author, owner.commits))
            .collect(),
        functions,
        repo_commit_counts: git_churn_by_file(intel),
        import_edges,
    }
}

pub async fn build_git_risk_assembly(
    intel: &refact_git_intel::GitIntel,
    git_root: &Path,
    analytics_scope_root: Option<&Path>,
    service: Option<&Arc<refact_codegraph::CodeGraphService>>,
    limit: usize,
    filter: Option<&str>,
) -> GitRiskAssembly {
    build_git_risk_assembly_with_roots(
        intel,
        git_root,
        git_root,
        analytics_scope_root,
        service,
        limit,
        filter,
    )
    .await
}

async fn build_git_risk_assembly_with_roots(
    intel: &refact_git_intel::GitIntel,
    git_root: &Path,
    indexed_root: &Path,
    analytics_scope_root: Option<&Path>,
    service: Option<&Arc<refact_codegraph::CodeGraphService>>,
    limit: usize,
    filter: Option<&str>,
) -> GitRiskAssembly {
    let graph = health_graph_snapshot_with_scope(service, Some(indexed_root), analytics_scope_root)
        .await
        .map(Arc::new);
    let intel_owned = intel.clone();
    let repo_root = git_root.to_path_buf();
    let filter = filter.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        build_git_risk_assembly_blocking(
            &intel_owned,
            &repo_root,
            graph.as_deref(),
            limit,
            filter.as_deref(),
        )
    })
    .await
    .unwrap_or_else(|_| GitRiskAssembly {
        files: Vec::new(),
        recent_commit_risks: Vec::new(),
    })
}

fn build_git_risk_assembly_blocking(
    intel: &refact_git_intel::GitIntel,
    repo_root: &Path,
    graph: Option<&HealthGraphSnapshot>,
    limit: usize,
    filter: Option<&str>,
) -> GitRiskAssembly {
    let now_ts = current_unix_ts();
    let all_hotspots = intel.hotspots(intel.file_churn.len().max(1));
    let mut selected = all_hotspots
        .into_iter()
        .filter(|hotspot| {
            filter.is_none_or(|filter| path_matches_git_filter(&hotspot.path, filter))
        })
        .take(limit)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        if let Some(filter) = filter {
            selected = intel
                .file_churn
                .keys()
                .filter(|path| path_matches_git_filter(path, filter))
                .take(limit)
                .map(|path| refact_git_intel::Hotspot {
                    path: path.clone(),
                    churn: intel.file_churn.get(path).copied().unwrap_or(0),
                })
                .collect();
        }
    }
    let temporal_map: HashMap<String, f64> = intel
        .temporal_hotspots(now_ts, intel.file_churn.len().max(1))
        .into_iter()
        .collect();
    let mut files = Vec::new();
    for (index, hotspot) in selected.iter().enumerate() {
        let ranges = if index < GIT_META_FUNCTION_FACT_TOP_K {
            function_ranges_from_disk(repo_root, &hotspot.path)
        } else {
            Vec::new()
        };
        let mut meta = build_git_meta(
            intel,
            repo_root,
            &hotspot.path,
            graph,
            (!ranges.is_empty()).then_some(ranges.as_slice()),
        );
        meta.is_hotspot = true;
        let findings = refact_codehealth::git_biomarkers::git_biomarkers(&meta);
        files.push(GitRiskFileAssembly {
            temporal_score: temporal_map.get(&hotspot.path).copied().unwrap_or(0.0),
            churn_risk: intel.churn_risk(&hotspot.path),
            meta,
            findings,
        });
    }
    let p80 = function_mod_p80(
        &files
            .iter()
            .flat_map(|file| file.meta.functions.iter().cloned())
            .collect::<Vec<_>>(),
    );
    if p80.is_some() {
        for file in &mut files {
            file.meta.repo_function_mod_p80 = p80;
            file.findings = refact_codehealth::git_biomarkers::git_biomarkers(&file.meta);
        }
    }

    GitRiskAssembly {
        files,
        recent_commit_risks: recent_commit_risk_summaries(intel),
    }
}

fn file_is_stable(intel: &refact_git_intel::GitIntel, file: &str, now_ts: i64) -> bool {
    let last_ts = intel
        .commit_records
        .iter()
        .filter(|commit| commit.files.iter().any(|(path, _, _)| path == file))
        .map(|commit| commit.ts)
        .max();
    last_ts.is_some_and(|ts| now_ts.saturating_sub(ts) / 86_400 > GIT_STABLE_AGE_DAYS)
}

fn nloc_for_file(repo_root: &Path, path_repo_relative: &str) -> u32 {
    std::fs::read_to_string(repo_root.join(path_repo_relative))
        .map(|text| {
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .count()
                .min(u32::MAX as usize) as u32
        })
        .unwrap_or(0)
}

pub fn function_ranges_from_disk(
    repo_root: &Path,
    path_repo_relative: &str,
) -> Vec<(String, usize, usize)> {
    let Ok(text) = std::fs::read_to_string(repo_root.join(path_repo_relative)) else {
        return Vec::new();
    };
    let lang = refact_codegraph::lang_from_path(path_repo_relative);
    refact_codehealth::analyze(lang, &text)
        .functions
        .into_iter()
        .map(|function| {
            let line2 = function
                .line1
                .saturating_add(function.loc as usize)
                .saturating_sub(1)
                .max(function.line1);
            (function.name, function.line1, line2)
        })
        .collect()
}

fn build_function_git_facts(
    repo_root: &Path,
    path_repo_relative: &str,
    ranges: &[(String, usize, usize)],
    now_ts: i64,
) -> Vec<refact_codehealth::git_biomarkers::FunctionGitFacts> {
    let range_facts = cached_blame_range_facts(repo_root, path_repo_relative, ranges);
    let health_by_range = function_health_by_range(repo_root, path_repo_relative);
    range_facts
        .into_iter()
        .map(|fact| {
            let health = health_by_range
                .get(&(fact.name.clone(), fact.line1))
                .or_else(|| health_by_range.get(&(fact.name.clone(), 0)));
            refact_codehealth::git_biomarkers::FunctionGitFacts {
                name: fact.name,
                median_age_days: age_days_from_ts(now_ts, fact.last_modified_ts),
                recent_mod_count: fact.author_count,
                modification_count: fact.commit_count,
                ccn: health.map(|(ccn, _)| *ccn).unwrap_or(0),
                max_nesting: health.map(|(_, nesting)| *nesting).unwrap_or(0),
            }
        })
        .collect()
}

fn function_health_by_range(
    repo_root: &Path,
    path_repo_relative: &str,
) -> HashMap<(String, usize), (u32, u32)> {
    let Ok(text) = std::fs::read_to_string(repo_root.join(path_repo_relative)) else {
        return HashMap::new();
    };
    let lang = refact_codegraph::lang_from_path(path_repo_relative);
    let mut out = HashMap::new();
    for function in refact_codehealth::analyze(lang, &text).functions {
        out.insert(
            (function.name.clone(), function.line1),
            (function.complexity, function.nesting),
        );
        out.entry((function.name, 0))
            .or_insert((function.complexity, function.nesting));
    }
    out
}

fn age_days_from_ts(now_ts: i64, ts: i64) -> u32 {
    if ts <= 0 {
        return 0;
    }
    let days = now_ts.saturating_sub(ts).max(0) / 86_400;
    days.min(u32::MAX as i64) as u32
}

fn function_mod_p80(
    functions: &[refact_codehealth::git_biomarkers::FunctionGitFacts],
) -> Option<u32> {
    if functions.is_empty() {
        return None;
    }
    let mut counts = functions
        .iter()
        .map(|function| function.modification_count)
        .collect::<Vec<_>>();
    counts.sort_unstable();
    let index = ((counts.len() as f64 * 0.8).ceil() as usize).saturating_sub(1);
    counts.get(index).copied()
}

pub struct HealthImportIndex {
    pub indexed_root: String,
    pub distinct_paths: Vec<String>,
    pub import_dsts_by_src_path: HashMap<String, BTreeSet<String>>,
}
impl HealthImportIndex {
    pub fn build(cached: &refact_codegraph::CachedGraphAnalytics, repo_root: &Path) -> Self {
        let repo_root = repo_root.to_string_lossy();
        let mut path_by_id = HashMap::new();
        let mut distinct_paths = BTreeSet::new();
        for (id, _name, path) in &cached.data.nodes {
            path_by_id.insert(*id, path.as_str());
            distinct_paths.insert(path.clone());
        }

        let mut import_dsts_by_src_path: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (src, dst, kind) in &cached.data.edges {
            if kind != "calls" && kind != "imports" {
                continue;
            }
            let (Some(src_path), Some(dst_path)) = (path_by_id.get(src), path_by_id.get(dst))
            else {
                continue;
            };
            if refact_git_intel::paths::paths_refer_to_same_file(src_path, dst_path, &repo_root) {
                continue;
            }
            import_dsts_by_src_path
                .entry((*src_path).to_string())
                .or_default()
                .insert(refact_git_intel::paths::repo_relative_or_basename(
                    dst_path, &repo_root,
                ));
        }

        Self {
            indexed_root: repo_root.to_string(),
            distinct_paths: distinct_paths.into_iter().collect(),
            import_dsts_by_src_path,
        }
    }

    pub fn import_edges(&self, path_repo_relative: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        for path in &self.distinct_paths {
            if refact_git_intel::paths::paths_refer_to_same_file(
                path,
                path_repo_relative,
                &self.indexed_root,
            ) {
                if let Some(dsts) = self.import_dsts_by_src_path.get(path) {
                    out.extend(dsts.iter().cloned());
                }
            }
        }
        out
    }
}
fn path_matches_git_filter(path: &str, filter: &str) -> bool {
    let normalized = refact_git_intel::paths::normalize_separators(path);
    normalized == filter
        || normalized.ends_with(filter)
        || normalized.rsplit('/').next() == Some(filter)
}

pub fn recent_commit_risk_summaries(
    intel: &refact_git_intel::GitIntel,
) -> Vec<RecentCommitRiskSummary> {
    intel
        .recent_commit_risks(GIT_RISK_RECENT_COMMITS)
        .into_iter()
        .take(GIT_RISK_RECENT_COMMIT_LIMIT)
        .map(|risk| RecentCommitRiskSummary {
            sha: truncate_chars(&risk.commit_id, 12),
            summary: truncate_chars(&risk.summary, 80),
            risk: risk.risk,
            top_factor_names: commit_risk_factor_names(&risk.inputs),
        })
        .collect()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn commit_risk_factor_names(inputs: &refact_git_intel::change_risk::RiskInputs) -> Vec<String> {
    let mut factors = [
        ("lines_added", inputs.la),
        ("lines_deleted", inputs.ld),
        ("files", inputs.nf),
        ("directories", inputs.nd),
        ("subsystems", inputs.ns),
        ("entropy", inputs.entropy * 100.0),
        ("low_experience", 100.0 / (inputs.exp + 1.0)),
    ];
    factors.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    factors
        .into_iter()
        .filter(|(_, score)| *score > 0.0)
        .take(3)
        .map(|(name, _)| name.to_string())
        .collect()
}

pub fn git_risk_response(
    intel: &refact_git_intel::GitIntel,
    assembly: &GitRiskAssembly,
) -> GitRiskResponse {
    let hotspots = assembly
        .files
        .iter()
        .map(|file| GitHotspotResponse {
            path: file.meta.file_path.clone(),
            churn: file.meta.commit_count_total,
            risk: file.churn_risk,
            churn_risk: file.churn_risk,
            churn_percentile: file.meta.churn_percentile,
            temporal_score: file.temporal_score,
            change_entropy: file.meta.change_entropy,
            change_entropy_pct: file.meta.change_entropy_pct,
            bus_factor: file.meta.bus_factor as usize,
            ownership_risk: intel.ownership_risk(&file.meta.file_path),
            knowledge_loss: intel.knowledge_loss(&file.meta.file_path),
        })
        .collect();
    let ownership = assembly
        .files
        .iter()
        .map(|file| {
            let owners = intel.ownership(&file.meta.file_path);
            GitOwnershipResponse {
                path: file.meta.file_path.clone(),
                top_owner: owners
                    .first()
                    .map(|owner| owner.author.clone())
                    .unwrap_or_default(),
                top_owner_share: owners.first().map(|owner| owner.share).unwrap_or(0.0),
                bus_factor: file.meta.bus_factor as usize,
                owner_count: owners.len(),
                ownership_risk: intel.ownership_risk(&file.meta.file_path),
                knowledge_loss: intel.knowledge_loss(&file.meta.file_path),
                owners: owners
                    .into_iter()
                    .map(|owner| GitOwnerResponse {
                        author: owner.author,
                        commits: owner.commits,
                        share: owner.share,
                    })
                    .collect(),
            }
        })
        .collect();
    let co_change = intel
        .co_change_pairs(2)
        .into_iter()
        .take(10)
        .map(|((a, b), count)| GitCoChangeResponse {
            path_a: a,
            path_b: b,
            count,
        })
        .collect();
    let coupling = refact_git_intel::coupling::build_coupling_graph(intel, 8).edges;
    let hotspot_paths: Vec<String> = assembly
        .files
        .iter()
        .map(|file| file.meta.file_path.clone())
        .collect();
    let reviewers = refact_git_intel::coupling::reviewer_suggestions(intel, &hotspot_paths, 5)
        .into_iter()
        .map(|(author, score)| GitReviewerResponse { author, score })
        .collect();
    let findings = assembly
        .files
        .iter()
        .flat_map(|file| {
            file.findings.iter().map(|finding| GitFindingResponse {
                path: file.meta.file_path.clone(),
                biomarker: finding.biomarker.clone(),
                category: finding.category.clone(),
                dimension: finding.dimension,
                severity: finding.severity,
                line: finding.line,
                detail: finding.detail.clone(),
            })
        })
        .take(20)
        .collect();
    GitRiskResponse {
        commits_analyzed: intel.commits_analyzed,
        agent_authored_pct: intel.agent_authored_pct(),
        history_truncation: intel.history_truncation(),
        hotspots,
        ownership,
        co_change,
        coupling,
        reviewers,
        findings,
        recent_commit_risks: assembly.recent_commit_risks.clone(),
    }
}

pub struct ToolGitRisk {
    pub config_path: String,
}
#[async_trait]
impl Tool for ToolGitRisk {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        _args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let roots = codegraph_project_roots(ccx.clone())
            .await?
            .ok_or_else(|| "no project directory available".to_string())?;
        let gcx = ccx.lock().await.app.gcx.clone();
        let intel = cached_mine_history_async(&roots.git_root, 1000).await?;
        if intel.hotspots(1).is_empty() {
            let data = git_risk_response(
                &intel,
                &GitRiskAssembly {
                    files: Vec::new(),
                    recent_commit_risks: Vec::new(),
                },
            );
            return Ok((
                false,
                tool_message(
                    tool_call_id,
                    ToolJson::new("git_risk", "No git history found", data).to_text(),
                ),
            ));
        }
        let service = gcx.codegraph.lock().await.clone();
        let assembly = build_git_risk_assembly_with_roots(
            &intel,
            &roots.git_root,
            &roots.indexed_root,
            analytics_scope_root(Some(&roots)),
            service.as_ref(),
            15,
            None,
        )
        .await;
        let response = git_risk_response(&intel, &assembly);
        let mut summary = format!(
            "Git risk over {} commits: {} hotspots",
            intel.commits_analyzed,
            response.hotspots.len()
        );
        if let Some(notice) = response.history_truncation.notice.as_ref() {
            summary.push_str(&format!("; {notice}"));
        }
        Ok((
            false,
            tool_message(
                tool_call_id,
                ToolJson::new("git_risk", summary, response).to_text(),
            ),
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "git_risk".to_string(),
            display_name: "Git Risk".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Git intelligence for the project: mined history summary, churn/temporal hotspots, ownership and bus-factor risk, co-change/coupling pairs, reviewer ownership hints, recent commit risk factors, and git-biomarker findings with function-level facts for the top hotspot files.".to_string(),
            input_schema: json_schema_from_params(&[], &[]),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        codegraph_dependency()
    }
}
