#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthFunctionSummary {
    pub name: String,
    pub line1: usize,
    pub complexity: u32,
    pub nesting: u32,
    pub loc: u32,
    pub maintainability: f64,
    pub maintainability_index: f64,
}
#[derive(Debug, Clone)]
pub struct HealthFileAnalysis {
    pub path: String,
    pub lang: String,
    pub grade: char,
    pub maintainability_index: f64,
    pub max_complexity: u32,
    pub avg_maintainability: f64,
    pub function_count: usize,
    pub duplication_pct: f64,
    pub dry_violation: bool,
    pub defect_score: f64,
    pub maintainability_score: f64,
    pub performance_score: f64,
    pub biomarker_count: usize,
    pub refactoring_count: usize,
    pub functions: Vec<HealthFunctionSummary>,
    pub findings: Vec<HealthFinding>,
    pub health_impact: Vec<HealthImpactContributor>,
    pub refactorings: Vec<refact_codehealth::refactoring::RefactoringSuggestion>,
    pub cache_hit: bool,
}
#[derive(Clone, Copy)]
pub struct HealthAnalysisContext<'a> {
    pub repo_root: Option<&'a Path>,
    pub intel: Option<&'a refact_git_intel::GitIntel>,
    pub graph: Option<&'a HealthGraphSnapshot>,
    pub coverage: Option<&'a refact_codehealth::coverage::CoverageReport>,
    pub trend_findings: &'a [refact_codehealth::biomarkers::Finding],
    pub git_function_fact_paths: &'a HashSet<String>,
}
#[derive(Debug, Clone)]
pub struct HealthFileCore {
    pub path: String,
    pub lang: String,
    pub functions: Vec<HealthFunctionSummary>,
    pub max_complexity: u32,
    pub avg_maintainability: f64,
    pub duplication_pct: f64,
    pub dry_violation: bool,
    pub findings: Vec<HealthFinding>,
    pub refactorings: Vec<refact_codehealth::refactoring::RefactoringSuggestion>,
}
#[derive(Default)]
pub struct HealthAnalysisCache {
    pub files: HashMap<HealthCacheKey, HealthCacheEntry>,
    pub lru: VecDeque<HealthCacheKey>,
}
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct HealthCacheKey {
    pub path: String,
    pub graph_service_identity: Option<usize>,
    pub graph_generation: Option<u64>,
    pub graph_scope_signature: String,
}
#[derive(Clone)]
pub struct HealthCacheEntry {
    pub graph_service: Option<Weak<refact_codegraph::CodeGraphService>>,
    pub graph_service_identity: Option<usize>,
    pub graph_generation: Option<u64>,
    pub graph_scope_signature: String,
    pub content_hash: String,
    pub git_generation: Option<String>,
    pub coverage_signature: Option<String>,
    pub trend_signature: String,
    pub core: HealthFileCore,
}
pub fn health_analysis_cache() -> &'static StdMutex<HealthAnalysisCache> {
    static CACHE: OnceLock<StdMutex<HealthAnalysisCache>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(HealthAnalysisCache::default()))
}

pub fn git_churn_by_file(intel: &refact_git_intel::GitIntel) -> HashMap<String, u32> {
    intel.file_churn.clone()
}

pub fn top_git_function_fact_paths(
    intel: Option<&refact_git_intel::GitIntel>,
) -> HashSet<String> {
    intel
        .map(|intel| {
            intel
                .hotspots(GIT_META_FUNCTION_FACT_TOP_K)
                .into_iter()
                .map(|hotspot| hotspot.path)
                .collect()
        })
        .unwrap_or_default()
}

pub async fn load_health_snapshots(
    service: &refact_codegraph::CodeGraphService,
) -> Vec<refact_codehealth::trends::HealthSnapshot> {
    service
        .meta_get(HEALTH_SNAPSHOT_META_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub async fn persist_health_snapshots(
    service: &refact_codegraph::CodeGraphService,
    snapshots: &[refact_codehealth::trends::HealthSnapshot],
) -> Result<(), String> {
    let value = serde_json::to_string(snapshots)
        .map_err(|err| format!("health snapshot serialize: {err}"))?;
    service.meta_set(HEALTH_SNAPSHOT_META_KEY, &value).await
}

pub fn build_health_snapshot(
    files: &[HealthFileAnalysis],
) -> refact_codehealth::trends::HealthSnapshot {
    let per_file = files
        .iter()
        .map(|file| (file.path.clone(), file.defect_score))
        .collect::<HashMap<_, _>>();
    let aggregate = if files.is_empty() {
        10.0
    } else {
        files.iter().map(|file| file.defect_score).sum::<f64>() / files.len() as f64
    };
    refact_codehealth::trends::HealthSnapshot {
        ts: current_unix_ts(),
        per_file,
        aggregate,
    }
}

pub fn push_health_snapshot(
    history: &mut Vec<refact_codehealth::trends::HealthSnapshot>,
    snapshot: refact_codehealth::trends::HealthSnapshot,
) {
    refact_codehealth::trends::push_snapshot(history, snapshot, HEALTH_SNAPSHOT_LIMIT);
}

pub fn analyze_health_file_shared(
    path: String,
    text: String,
    ctx: &HealthAnalysisContext<'_>,
) -> Result<HealthFileAnalysis, String> {
    analyze_health_file_shared_for_path(path, text, None, ctx)
}

fn analyze_health_file_shared_for_path(
    path: String,
    text: String,
    coverage_path: Option<&Path>,
    ctx: &HealthAnalysisContext<'_>,
) -> Result<HealthFileAnalysis, String> {
    let lang = refact_codegraph::lang_from_path(&path).to_string();
    let content_hash = health_content_hash(&text, &lang);
    let git_generation = health_git_generation(ctx.intel);
    let coverage_signature =
        health_coverage_signature(ctx.coverage, &path, coverage_path, ctx.repo_root);
    let trend_signature = health_trend_signature(ctx.trend_findings, &path, ctx.repo_root);
    let graph_generation = ctx.graph.map(|graph| graph.graph_generation);
    let graph_service_identity = ctx.graph.map(|graph| graph.service_identity);
    let graph_scope_signature = ctx
        .graph
        .map(|graph| graph.scope_signature.clone())
        .unwrap_or_else(|| "no-graph".to_string());
    if let Some(core) = health_cache_lookup(
        &path,
        ctx.graph.map(|graph| &graph.service),
        graph_service_identity,
        graph_generation,
        &graph_scope_signature,
        &content_hash,
        &git_generation,
        &coverage_signature,
        &trend_signature,
    ) {
        return Ok(health_analysis_from_core(core, true));
    }

    let core = compute_health_core(path, text, lang, coverage_path, ctx)?;
    health_cache_store(
        &core.path,
        ctx.graph.map(|graph| graph.service.clone()),
        graph_service_identity,
        graph_generation,
        graph_scope_signature,
        content_hash,
        git_generation,
        coverage_signature,
        trend_signature,
        core.clone(),
    );
    Ok(health_analysis_from_core(core, false))
}

fn health_content_hash(text: &str, lang: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(lang.as_bytes());
    hasher.update([0xff]);
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn hash_string(value: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_ref());
    hex::encode(hasher.finalize())
}

fn health_git_generation(intel: Option<&refact_git_intel::GitIntel>) -> Option<String> {
    intel.map(|intel| {
        if let Some(last) = &intel.last_commit_id {
            return last.clone();
        }
        let mut parts = vec![format!("commits={}", intel.commits_analyzed)];
        let mut churn = intel.file_churn.iter().collect::<Vec<_>>();
        churn.sort_by(|a, b| a.0.cmp(b.0));
        for (path, count) in churn {
            parts.push(format!("churn:{path}:{count}"));
        }
        let mut fixes = intel.fix_commit_counts.iter().collect::<Vec<_>>();
        fixes.sort_by(|a, b| a.0.cmp(b.0));
        for (path, count) in fixes {
            parts.push(format!("fix:{path}:{count}"));
        }
        hash_string(parts.join("\n"))
    })
}

fn health_cache_lookup(
    path: &str,
    graph_service: Option<&Arc<refact_codegraph::CodeGraphService>>,
    graph_service_identity: Option<usize>,
    graph_generation: Option<u64>,
    graph_scope_signature: &str,
    content_hash: &str,
    git_generation: &Option<String>,
    coverage_signature: &Option<String>,
    trend_signature: &str,
) -> Option<HealthFileCore> {
    let mut cache = health_analysis_cache().lock().unwrap();
    let key = HealthCacheKey {
        path: path.to_string(),
        graph_service_identity,
        graph_generation,
        graph_scope_signature: graph_scope_signature.to_string(),
    };
    let core = cache
        .files
        .get(&key)
        .filter(|entry| {
            let same_service = match (graph_service, entry.graph_service.as_ref()) {
                (Some(service), Some(weak)) => weak
                    .upgrade()
                    .is_some_and(|cached| Arc::ptr_eq(&cached, service)),
                (None, None) => true,
                _ => false,
            };
            same_service
                && entry.graph_service_identity == graph_service_identity
                && entry.graph_generation == graph_generation
                && entry.graph_scope_signature == graph_scope_signature
                && entry.content_hash == content_hash
                && &entry.git_generation == git_generation
                && &entry.coverage_signature == coverage_signature
                && entry.trend_signature == trend_signature
        })
        .map(|entry| entry.core.clone());
    if core.is_some() {
        cache.lru.retain(|cached_key| cached_key != &key);
        cache.lru.push_back(key);
    }
    core
}

fn health_cache_store(
    path: &str,
    graph_service: Option<Arc<refact_codegraph::CodeGraphService>>,
    graph_service_identity: Option<usize>,
    graph_generation: Option<u64>,
    graph_scope_signature: String,
    content_hash: String,
    git_generation: Option<String>,
    coverage_signature: Option<String>,
    trend_signature: String,
    core: HealthFileCore,
) {
    let key = HealthCacheKey {
        path: path.to_string(),
        graph_service_identity,
        graph_generation,
        graph_scope_signature: graph_scope_signature.clone(),
    };
    let mut cache = health_analysis_cache().lock().unwrap();
    cache.files.retain(|cached_key, entry| {
        entry
            .graph_service
            .as_ref()
            .is_none_or(|weak| weak.upgrade().is_some())
            && !(graph_service_identity.is_some()
                && cached_key.graph_service_identity == graph_service_identity
                && cached_key.graph_generation != graph_generation)
    });
    let live_keys = cache.files.keys().cloned().collect::<HashSet<_>>();
    cache
        .lru
        .retain(|cached_key| live_keys.contains(cached_key));
    cache.lru.retain(|cached_key| cached_key != &key);
    cache.files.insert(
        key.clone(),
        HealthCacheEntry {
            graph_service: graph_service.as_ref().map(Arc::downgrade),
            graph_service_identity,
            graph_generation,
            graph_scope_signature,
            content_hash,
            git_generation,
            coverage_signature,
            trend_signature,
            core,
        },
    );
    cache.lru.push_back(key);
    while cache.files.len() > HEALTH_ANALYSIS_CACHE_CAPACITY {
        if let Some(oldest) = cache.lru.pop_front() {
            cache.files.remove(&oldest);
        } else {
            break;
        }
    }
}

fn compute_health_core(
    path: String,
    text: String,
    lang: String,
    coverage_path: Option<&Path>,
    ctx: &HealthAnalysisContext<'_>,
) -> Result<HealthFileCore, String> {
    let health = refact_codehealth::analyze(&lang, &text);
    let duplication_pct = refact_codehealth::duplication::duplication_pct(&lang, &text);
    let mut findings = refact_codehealth::biomarkers::detect_biomarkers(&lang, &text);
    findings.extend(refact_codehealth::perf::detect_perf(&lang, &text));
    let dry_findings = health_dry_findings(&path, &lang, &text, duplication_pct);
    let dry_violation = !dry_findings.is_empty();
    findings.extend(dry_findings);
    findings.extend(health_git_findings(&path, ctx, &health)?);
    findings.extend(health_coverage_findings(
        &path,
        coverage_path,
        ctx.coverage,
        ctx.repo_root,
        ctx.intel,
        health.max_complexity,
    ));
    findings.extend(health_trend_findings_for_path(
        ctx.trend_findings,
        &path,
        ctx.repo_root,
    ));
    sort_health_findings(&mut findings);
    let findings = enrich_health_findings(ctx.graph, &path, &health.functions, findings);

    let mut functions = health.functions.clone();
    functions.sort_by(|a, b| {
        b.complexity
            .cmp(&a.complexity)
            .then_with(|| b.loc.cmp(&a.loc))
            .then_with(|| a.line1.cmp(&b.line1))
            .then_with(|| a.name.cmp(&b.name))
    });
    let functions = functions
        .into_iter()
        .map(|function| HealthFunctionSummary {
            name: function.name,
            line1: function.line1,
            complexity: function.complexity,
            nesting: function.nesting,
            loc: function.loc,
            maintainability: function.maintainability,
            maintainability_index: function.maintainability,
        })
        .collect();
    let mut refactorings = refact_codehealth::refactoring::rank(
        refact_codehealth::refactoring::suggest_refactorings(&lang, &text),
    );
    refactorings.sort_by(|a, b| {
        b.impact
            .total_cmp(&a.impact)
            .then_with(|| a.line.cmp(&b.line))
    });

    Ok(HealthFileCore {
        path,
        lang,
        functions,
        max_complexity: health.max_complexity,
        avg_maintainability: health.avg_maintainability,
        duplication_pct,
        dry_violation,
        findings,
        refactorings,
    })
}

fn health_analysis_from_core(core: HealthFileCore, cache_hit: bool) -> HealthFileAnalysis {
    let plain_findings = core
        .findings
        .iter()
        .map(|finding| finding.finding.clone())
        .collect::<Vec<_>>();
    let (score, impacts) = refact_codehealth::scoring::score_file_with_impacts(&plain_findings);
    let mut health_impact = impacts
        .into_iter()
        .filter_map(|impact| {
            core.findings
                .get(impact.index)
                .map(|finding| HealthImpactContributor {
                    biomarker: finding.finding.biomarker.clone(),
                    category: finding.finding.category.clone(),
                    dimension: finding.finding.dimension,
                    severity: finding.finding.severity,
                    line: finding.finding.line,
                    detail: finding.finding.detail.clone(),
                    deduction: impact.deduction,
                    capped: impact.capped,
                })
        })
        .collect::<Vec<_>>();
    health_impact.sort_by(|a, b| {
        b.deduction
            .total_cmp(&a.deduction)
            .then_with(|| a.biomarker.cmp(&b.biomarker))
            .then_with(|| a.line.cmp(&b.line))
    });
    health_impact.truncate(5);
    let function_count = core.functions.len();
    let biomarker_count = core.findings.len();
    let refactoring_count = core.refactorings.len();

    HealthFileAnalysis {
        path: core.path,
        lang: core.lang,
        grade: refact_codehealth::scoring::grade(score.defect),
        maintainability_index: core.avg_maintainability,
        max_complexity: core.max_complexity,
        avg_maintainability: core.avg_maintainability,
        function_count,
        duplication_pct: core.duplication_pct,
        dry_violation: core.dry_violation,
        defect_score: score.defect,
        maintainability_score: score.maintainability,
        performance_score: score.performance,
        biomarker_count,
        refactoring_count,
        functions: core.functions,
        findings: core.findings,
        health_impact,
        refactorings: core.refactorings,
        cache_hit,
    }
}

pub fn health_dry_findings(
    path: &str,
    lang: &str,
    text: &str,
    duplication_pct: f64,
) -> Vec<refact_codehealth::biomarkers::Finding> {
    let clones = refact_codehealth::duplication::detect_clones(lang, text)
        .into_iter()
        .map(|clone| refact_codehealth::dry::DryClonePair {
            file_a: path.to_string(),
            a_start_line: clone.line_a,
            a_line_count: clone
                .a_end_line
                .saturating_sub(clone.line_a)
                .saturating_add(1),
            file_b: path.to_string(),
            b_start_line: clone.line_b,
            b_line_count: clone
                .b_end_line
                .saturating_sub(clone.line_b)
                .saturating_add(1),
            co_change_count: 0,
        })
        .collect();
    let input = refact_codehealth::dry::DryInput {
        file_path: path.to_string(),
        duplication_pct,
        clones,
    };
    refact_codehealth::dry::dry_violation(&input)
}

fn health_git_findings(
    path: &str,
    ctx: &HealthAnalysisContext<'_>,
    health: &refact_codehealth::FileHealth,
) -> Result<Vec<refact_codehealth::biomarkers::Finding>, String> {
    let (Some(intel), Some(repo_root)) = (ctx.intel, ctx.repo_root) else {
        return Ok(Vec::new());
    };
    let path_key = ctx
        .graph
        .and_then(|graph| graph.file_nodes(path).first().copied())
        .and_then(|(_, _, stored_path)| {
            ctx.graph
                .and_then(|graph| graph.import_index.as_ref())
                .and_then(|index| {
                    code_intel_relative_path(Path::new(&index.indexed_root), stored_path)
                })
                .map(|path| path.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| {
            refact_git_intel::paths::repo_relative_or_basename(path, &repo_root.to_string_lossy())
        });
    let ranges = if ctx.git_function_fact_paths.contains(&path_key) {
        health
            .functions
            .iter()
            .map(|function| {
                let line2 = function
                    .line1
                    .saturating_add(function.loc as usize)
                    .saturating_sub(1)
                    .max(function.line1);
                (function.name.clone(), function.line1, line2)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let meta = build_git_meta(
        intel,
        repo_root,
        &path_key,
        ctx.graph,
        (!ranges.is_empty()).then_some(ranges.as_slice()),
    );
    Ok(refact_codehealth::git_biomarkers::git_biomarkers(&meta))
}

fn health_coverage_findings(
    path: &str,
    coverage_path: Option<&Path>,
    coverage: Option<&refact_codehealth::coverage::CoverageReport>,
    repo_root: Option<&Path>,
    intel: Option<&refact_git_intel::GitIntel>,
    max_complexity: u32,
) -> Vec<refact_codehealth::biomarkers::Finding> {
    let Some(coverage) = coverage else {
        return Vec::new();
    };
    let Some(file) = coverage_file_for_path(coverage, path, coverage_path, repo_root) else {
        return Vec::new();
    };
    let mut complexity_by_file = HashMap::new();
    complexity_by_file.insert(file.path.clone(), max_complexity);
    let mut churn_by_file = HashMap::new();
    if let Some(churn) = health_churn_for_path(intel, &file.path, repo_root) {
        churn_by_file.insert(file.path.clone(), churn);
    }
    let report = refact_codehealth::coverage::CoverageReport {
        format: coverage.format.clone(),
        files: vec![file],
    };
    refact_codehealth::coverage_biomarkers::coverage_biomarkers(
        &report,
        &complexity_by_file,
        &churn_by_file,
    )
}

pub fn coverage_file_for_path(
    coverage: &refact_codehealth::coverage::CoverageReport,
    indexed_path: &str,
    coverage_path: Option<&Path>,
    repo_root: Option<&Path>,
) -> Option<refact_codehealth::coverage::FileCoverage> {
    let worktree_path = coverage_path.map(|path| path.to_string_lossy().to_string());
    let project_relative = coverage_path.and_then(|path| {
        repo_root
            .and_then(|root| path.strip_prefix(root).ok())
            .map(|path| path.to_string_lossy().to_string())
    });
    let targets = [
        Some(indexed_path.to_string()),
        worktree_path,
        project_relative,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let matches = coverage
        .files
        .iter()
        .filter(|file| {
            targets
                .iter()
                .any(|target| coverage_paths_match_strongly(&file.path, target, repo_root))
        })
        .cloned()
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        return matches.into_iter().next();
    }
    if !matches.is_empty() {
        return None;
    }

    let requested_basenames = targets
        .iter()
        .map(|path| graph_path_basename(path))
        .collect::<HashSet<_>>();
    let fallback = coverage
        .files
        .iter()
        .filter(|file| requested_basenames.contains(&graph_path_basename(&file.path)))
        .collect::<Vec<_>>();
    (fallback.len() == 1).then(|| fallback[0].clone())
}

fn coverage_paths_match_strongly(a: &str, b: &str, repo_root: Option<&Path>) -> bool {
    let normalized_a = normalize_health_path(a);
    let normalized_b = normalize_health_path(b);
    if normalized_a == normalized_b {
        return true;
    }
    let Some(root) = repo_root else {
        return false;
    };
    let root = root.to_string_lossy();
    let relative_a = refact_git_intel::paths::repo_relative(&normalized_a, &root)
        .map(refact_git_intel::paths::normalize_separators);
    let relative_b = refact_git_intel::paths::repo_relative(&normalized_b, &root)
        .map(refact_git_intel::paths::normalize_separators);
    relative_a.as_deref() == Some(normalized_b.as_str())
        || relative_b.as_deref() == Some(normalized_a.as_str())
        || relative_a.is_some() && relative_a == relative_b
}

fn normalize_health_path(path: &str) -> String {
    let mut normalized = refact_git_intel::paths::normalize_separators(path);
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    normalized
}

fn health_coverage_signature(
    coverage: Option<&refact_codehealth::coverage::CoverageReport>,
    path: &str,
    coverage_path: Option<&Path>,
    repo_root: Option<&Path>,
) -> Option<String> {
    coverage.map(|coverage| {
        let file = coverage_file_for_path(coverage, path, coverage_path, repo_root);
        serde_json::to_string(&(coverage.format.as_str(), file))
            .map(hash_string)
            .unwrap_or_else(|_| "coverage-unserializable".to_string())
    })
}

fn health_churn_for_path(
    intel: Option<&refact_git_intel::GitIntel>,
    path: &str,
    repo_root: Option<&Path>,
) -> Option<u32> {
    let intel = intel?;
    intel.file_churn.get(path).copied().or_else(|| {
        intel
            .file_churn
            .iter()
            .find(|(candidate, _)| health_paths_refer_to_same_file(candidate, path, repo_root))
            .map(|(_, churn)| *churn)
    })
}

fn health_trend_findings_for_path(
    findings: &[refact_codehealth::biomarkers::Finding],
    path: &str,
    repo_root: Option<&Path>,
) -> Vec<refact_codehealth::biomarkers::Finding> {
    findings
        .iter()
        .filter(|finding| {
            health_trend_path(&finding.detail).is_some_and(|trend_path| {
                health_paths_refer_to_same_file(trend_path, path, repo_root)
            })
        })
        .cloned()
        .collect()
}

fn health_trend_signature(
    findings: &[refact_codehealth::biomarkers::Finding],
    path: &str,
    repo_root: Option<&Path>,
) -> String {
    let relevant = health_trend_findings_for_path(findings, path, repo_root);
    if relevant.is_empty() {
        return String::new();
    }
    serde_json::to_string(&relevant)
        .map(hash_string)
        .unwrap_or_else(|_| "trend-unserializable".to_string())
}

fn health_trend_path(detail: &str) -> Option<&str> {
    detail.split_once(':').map(|(path, _)| path.trim())
}

pub fn health_paths_refer_to_same_file(a: &str, b: &str, repo_root: Option<&Path>) -> bool {
    match repo_root {
        Some(repo_root) => {
            refact_git_intel::paths::paths_refer_to_same_file(a, b, &repo_root.to_string_lossy())
        }
        None => {
            refact_git_intel::paths::normalize_separators(a)
                == refact_git_intel::paths::normalize_separators(b)
        }
    }
}

pub fn health_stored_path_for_request(
    paths: impl IntoIterator<Item = String>,
    requested: &str,
    repo_root: Option<&Path>,
) -> Option<String> {
    let matches = paths
        .into_iter()
        .filter(|path| health_paths_refer_to_same_file(path, requested, repo_root))
        .collect::<BTreeSet<_>>();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

fn sort_health_findings(findings: &mut [refact_codehealth::biomarkers::Finding]) {
    findings.sort_by(|a, b| {
        health_severity_rank(a.severity)
            .cmp(&health_severity_rank(b.severity))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.biomarker.cmp(&b.biomarker))
            .then_with(|| a.detail.cmp(&b.detail))
    });
}

fn health_severity_rank(severity: refact_codehealth::biomarkers::Severity) -> u8 {
    match severity {
        refact_codehealth::biomarkers::Severity::Critical => 0,
        refact_codehealth::biomarkers::Severity::High => 1,
        refact_codehealth::biomarkers::Severity::Medium => 2,
        refact_codehealth::biomarkers::Severity::Low => 3,
    }
}

#[cfg(any(test, feature = "test-util"))]
pub fn reset_health_cache_for_tests() {
    *health_analysis_cache().lock().unwrap() = HealthAnalysisCache::default();
}

pub struct ToolCodeHealth {
    pub config_path: String,
}
#[derive(Serialize)]
pub struct HealthToolResponse {
    #[serde(flatten)]
    pub response: HealthResponse,
    pub file_category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_role: Option<String>,
    pub call_graph: Vec<CallGraphEdge>,
    pub coverage: Option<CoverageSummary>,
    pub warm_cache: bool,
}
#[async_trait]
impl Tool for ToolCodeHealth {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let requested_file_path = string_arg(args, "file_path")?;
        let resolved_file = resolve_codegraph_file(ccx.clone(), &requested_file_path).await?;
        let file_path = resolved_file.indexed_path;
        let coverage_path = resolved_file.read_path.clone();
        let gcx = ccx.lock().await.app.gcx.clone();
        refact_runtime_api::privacy::load_privacy_if_needed(gcx.clone()).await;
        let mut record_paths = vec![resolved_file.read_path.clone()];
        let text = refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk(
            gcx.clone(),
            &resolved_file.read_path,
        )
        .await?
        .to_string();
        let coverage = match args.get("coverage_file") {
            Some(Value::String(cov_path)) if !cov_path.trim().is_empty() => {
                let resolved_coverage = resolve_codegraph_file(ccx.clone(), cov_path).await?;
                record_paths.push(resolved_coverage.read_path.clone());
                let rope = refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk(
                    gcx.clone(),
                    &resolved_coverage.read_path,
                )
                .await?;
                refact_codehealth::coverage::detect_and_parse(&rope.to_string())
            }
            _ => None,
        };
        let service = gcx.codegraph.lock().await.clone();
        let roots = codegraph_project_roots(ccx.clone()).await?;
        let git_root = roots.as_ref().map(|roots| roots.git_root.clone());
        let indexed_root = roots.as_ref().map(|roots| roots.indexed_root.clone());
        let intel = match roots.as_ref().map(|roots| roots.git_root.as_path()) {
            Some(dir) => cached_mine_history_async(dir, GIT_HISTORY_MAX_COMMITS)
                .await
                .ok()
                .map(Arc::new),
            None => None,
        };
        let snapshots = match service.as_deref() {
            Some(service) => load_health_snapshots(service).await,
            None => Vec::new(),
        };
        let trend_findings = refact_codehealth::trends::evaluate_trends(&snapshots);
        let git_function_fact_paths = top_git_function_fact_paths(intel.as_deref());
        let graph = health_graph_snapshot_with_scope(
            service.as_ref(),
            indexed_root.as_deref(),
            analytics_scope_root(roots.as_ref()),
        )
        .await
        .map(Arc::new);
        let coverage = coverage.map(Arc::new);
        let analysis = {
            let file_path = file_path.clone();
            let coverage_path = coverage_path.clone();
            let repo_root = git_root.clone();
            let intel = intel.clone();
            let graph = graph.clone();
            let coverage = coverage.clone();
            tokio::task::spawn_blocking(move || {
                let ctx = HealthAnalysisContext {
                    repo_root: repo_root.as_deref(),
                    intel: intel.as_deref(),
                    graph: graph.as_deref(),
                    coverage: coverage.as_deref(),
                    trend_findings: &trend_findings,
                    git_function_fact_paths: &git_function_fact_paths,
                };
                analyze_health_file_shared_for_path(file_path, text, Some(&coverage_path), &ctx)
            })
            .await
            .map_err(|e| format!("health analysis join: {e}"))??
        };
        if analysis.functions.is_empty() {
            let index_state = match service.as_ref() {
                Some(service) => pr_blast_index_state(&service.index_readiness().await?),
                None => PrBlastIndexState {
                    queued: 0,
                    cross_file_edges: 0,
                    cross_file_ready: false,
                },
            };
            let data = HealthToolResponse {
                response: HealthResponse {
                    index_state,
                    aggregate: HealthAggregateResponse {
                        file_count: 1,
                        function_count: 0,
                        avg_score: 0.0,
                        grade: analysis.grade,
                        max_complexity: analysis.max_complexity,
                        avg_maintainability: analysis.avg_maintainability,
                        avg_maintainability_index: analysis.maintainability_index,
                        avg_maintainability_signal: analysis.maintainability_score,
                        avg_duplication_pct: analysis.duplication_pct,
                        biomarker_count: analysis.biomarker_count,
                        refactoring_count: analysis.refactoring_count,
                    },
                    files: Vec::new(),
                },
                file_category: refact_codewiki::well_known::file_category(
                    &file_path,
                    &analysis.lang,
                    false,
                )
                .to_string(),
                file_role: refact_codewiki::well_known::well_known_role(&file_path)
                    .map(|role| role.to_string()),
                call_graph: Vec::new(),
                coverage: None,
                warm_cache: analysis.cache_hit,
            };
            return Ok((
                false,
                tool_message_with_records(
                    &gcx,
                    tool_call_id,
                    ToolJson::new(
                        "code_health",
                        format!(
                            "No functions analyzed in `{requested_file_path}` (lang: {})",
                            analysis.lang
                        ),
                        data,
                    )
                    .to_text(),
                    record_paths.clone(),
                )?,
            ));
        }

        let file_category =
            refact_codewiki::well_known::file_category(&file_path, &analysis.lang, false)
                .to_string();
        let file_role =
            refact_codewiki::well_known::well_known_role(&file_path).map(|role| role.to_string());
        let call_graph = graph
            .as_deref()
            .map(|graph| health_call_graph(graph, &file_path, indexed_root.as_deref()))
            .unwrap_or_default();
        let mut coverage_summary = None;
        if let Some(report) = coverage.as_deref() {
            if let Some(file) = coverage_file_for_path(
                report,
                &file_path,
                Some(&coverage_path),
                git_root.as_deref(),
            ) {
                let single = refact_codehealth::coverage::CoverageReport {
                    format: report.format.clone(),
                    files: vec![file],
                };
                let (line_pct, branch_pct, below_50) =
                    refact_codehealth::coverage_biomarkers::coverage_summary(&single);
                coverage_summary = Some(CoverageSummary {
                    label: single.format.clone(),
                    line_pct,
                    branch_pct,
                    files_below_50: below_50,
                });
            }
        }
        let index_state = match service.as_ref() {
            Some(service) => service
                .index_readiness()
                .await
                .ok()
                .map(|r| pr_blast_index_state(&r)),
            None => None,
        }
        .unwrap_or(PrBlastIndexState {
            queued: 0,
            cross_file_edges: 0,
            cross_file_ready: false,
        });
        let functions = analysis
            .functions
            .iter()
            .take(30)
            .map(|function| HealthFunctionResponse {
                name: function.name.clone(),
                line1: function.line1,
                complexity: function.complexity,
                nesting: function.nesting,
                loc: function.loc,
                maintainability: function.maintainability,
                maintainability_index: function.maintainability_index,
            })
            .collect();
        let file = HealthFileResponse {
            path: file_path.clone(),
            lang: analysis.lang.clone(),
            score: analysis.defect_score,
            grade: analysis.grade,
            complexity: analysis.max_complexity,
            maintainability: analysis.avg_maintainability,
            maintainability_index: analysis.maintainability_index,
            maintainability_signal: analysis.maintainability_score,
            max_complexity: analysis.max_complexity,
            avg_maintainability: analysis.avg_maintainability,
            function_count: analysis.function_count,
            duplication_pct: analysis.duplication_pct,
            dry_violation: analysis.dry_violation,
            defect_score: analysis.defect_score,
            maintainability_score: analysis.maintainability_score,
            performance_score: analysis.performance_score,
            biomarker_count: analysis.biomarker_count,
            refactoring_count: analysis.refactoring_count,
            functions,
            findings: analysis.findings.clone(),
            health_impact: analysis.health_impact.clone(),
            cache_hit: analysis.cache_hit,
            refactorings: analysis.refactorings.clone(),
        };
        let aggregate = HealthAggregateResponse {
            file_count: 1,
            function_count: analysis.function_count,
            avg_score: analysis.defect_score,
            grade: analysis.grade,
            max_complexity: analysis.max_complexity,
            avg_maintainability: analysis.avg_maintainability,
            avg_maintainability_index: analysis.maintainability_index,
            avg_maintainability_signal: analysis.maintainability_score,
            avg_duplication_pct: analysis.duplication_pct,
            biomarker_count: analysis.biomarker_count,
            refactoring_count: analysis.refactoring_count,
        };
        let summary = format!(
            "Code health for `{requested_file_path}`: MI {:.1}, max complexity {}",
            analysis.maintainability_index, analysis.max_complexity
        );
        let data = HealthToolResponse {
            response: HealthResponse {
                index_state,
                aggregate,
                files: vec![file],
            },
            file_category,
            file_role,
            call_graph,
            coverage: coverage_summary,
            warm_cache: analysis.cache_hit,
        };
        Ok((
            false,
            tool_message_with_records(
                &gcx,
                tool_call_id,
                ToolJson::new("code_health", summary, data).to_text(),
                record_paths,
            )?,
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "code_health".to_string(),
            display_name: "Code Health".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Deterministic per-file health: function complexity/nesting/LOC/MI, duplication, structural/git/coverage/trend/performance findings, hot-path and fan-in graph enrichment, cached unchanged-file analysis, 1-10 defect/maintainability/performance scores with A-F grade, health-impact contributors, and refactoring targets.".to_string(),
            input_schema: json_schema_from_params(
                &[
                    (
                        "file_path",
                        "string",
                        "File to analyze; accepts the same workspace-relative, absolute current-project, and active-worktree path forms as cat.",
                    ),
                    (
                        "coverage_file",
                        "string",
                        "Optional LCOV/Cobertura/Clover report; accepts the same path forms as cat.",
                    ),
                ],
                &["file_path"],
            ),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        codegraph_dependency()
    }
}
