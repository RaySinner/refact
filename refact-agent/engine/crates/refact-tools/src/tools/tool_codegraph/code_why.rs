#[allow(unused_imports)]
use super::*;

pub struct ToolCodeWhy {
    pub config_path: String,
}
#[cfg(any(test, feature = "test-util"))]
pub static CODE_WHY_FILE_READ_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeWhySource {
    pub kind: String,
    pub text: String,
    pub reference: String,
    pub priority: u8,
    pub order: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeWhyFileCandidate {
    pub relative: PathBuf,
    pub kind: &'static str,
    pub reference: String,
    pub priority: u8,
    pub order: usize,
}
#[derive(Debug, Clone)]
pub struct CodeWhySourceMeta {
    pub priority: u8,
    pub order: usize,
    pub reference: String,
    pub file: Option<CodeWhyFileCandidate>,
}
#[derive(Debug, Clone)]
pub struct CodeWhyCandidate {
    pub decision: refact_codewiki::Decision,
    pub source_rank: u8,
    pub source_ref: String,
}
#[derive(Debug, Clone)]
pub struct CodeWhyDecision {
    pub decision: refact_codewiki::Decision,
    pub source_rank: u8,
    pub source_ref: String,
}
#[derive(Debug, Clone)]
pub struct ScoredCodeWhyDecision {
    pub item: CodeWhyDecision,
    pub confidence: f64,
    pub verification: refact_codewiki::Verification,
}
pub fn code_why_priority(kind: &str) -> u8 {
    match kind {
        "adr" => 0,
        "changelog" => 1,
        "pr" => 2,
        _ => 3,
    }
}

fn short_oid(oid: Option<&String>, fallback: usize) -> String {
    oid.map(|oid| oid.chars().take(12).collect::<String>())
        .filter(|oid| !oid.is_empty())
        .unwrap_or_else(|| format!("commit:{fallback}"))
}

pub fn path_ref(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn merge_commit_has_body(message: &str) -> bool {
    let mut lines = message.lines();
    let first = lines.next().unwrap_or("").trim();
    if !(first.starts_with("Merge pull request") || first.starts_with("Merge branch")) {
        return false;
    }
    lines.any(|line| !line.trim().is_empty())
}

fn commit_prose_text(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn git_code_why_sources(intel: &refact_git_intel::GitIntel) -> Vec<CodeWhySource> {
    let mut sources = Vec::new();
    let mut order = 0usize;
    let significant: HashSet<usize> = intel
        .significant_commits(CODE_WHY_SIGNIFICANT_COMMITS)
        .into_iter()
        .collect();

    for (idx, commit) in intel.commit_records.iter().enumerate() {
        if merge_commit_has_body(&commit.message) {
            sources.push(CodeWhySource {
                kind: "pr".to_string(),
                text: commit.message.clone(),
                reference: short_oid(commit.oid.as_ref(), idx),
                priority: code_why_priority("pr"),
                order,
            });
            order += 1;
        }
        if !significant.contains(&idx) {
            continue;
        }
        if commit.message.trim().is_empty() {
            continue;
        }
        sources.push(CodeWhySource {
            kind: "commit".to_string(),
            text: commit_prose_text(&commit.message),
            reference: short_oid(commit.oid.as_ref(), idx),
            priority: code_why_priority("commit"),
            order,
        });
        order += 1;
    }

    sources
}

fn glob_relative_files(repo_path: &Path, pattern: &str) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let full_pattern = repo_path.join(pattern).to_string_lossy().to_string();
    let Ok(paths) = glob::glob(&full_pattern) else {
        return out;
    };
    for path in paths.flatten() {
        if !path.is_file() {
            continue;
        }
        if let Ok(relative) = path.strip_prefix(repo_path) {
            out.insert(relative.to_path_buf());
        }
    }
    out
}

pub fn code_why_file_kinds(repo_path: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut by_path: BTreeMap<PathBuf, &'static str> = BTreeMap::new();
    for pattern in [
        "docs/**/adr/**/*.md",
        "docs/adr*/*.md",
        "ADR-*.md",
        "docs/decisions/**",
    ] {
        for path in glob_relative_files(repo_path, pattern) {
            by_path.insert(path, "adr");
        }
    }
    for pattern in ["CHANGELOG*.md", "docs/CHANGELOG*.md"] {
        for path in glob_relative_files(repo_path, pattern) {
            by_path.entry(path).or_insert("changelog");
        }
    }
    by_path.into_iter().collect()
}

pub fn code_why_file_candidates(repo_path: &Path) -> Vec<CodeWhyFileCandidate> {
    code_why_file_kinds(repo_path)
        .into_iter()
        .enumerate()
        .map(|(index, (relative, kind))| CodeWhyFileCandidate {
            reference: path_ref(&relative),
            relative,
            kind,
            priority: code_why_priority(kind),
            order: usize::MAX / 2 + index,
        })
        .collect()
}

fn sort_code_why_source_meta(items: &mut [CodeWhySourceMeta]) {
    items.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.reference.cmp(&right.reference))
            .then_with(|| left.order.cmp(&right.order))
    });
}

pub fn select_code_why_file_candidates(
    candidates: Vec<CodeWhyFileCandidate>,
    git_sources: &[CodeWhySource],
    limit: usize,
) -> Vec<CodeWhyFileCandidate> {
    let mut slots = git_sources
        .iter()
        .map(|source| CodeWhySourceMeta {
            priority: source.priority,
            order: source.order,
            reference: source.reference.clone(),
            file: None,
        })
        .chain(candidates.into_iter().map(|candidate| CodeWhySourceMeta {
            priority: candidate.priority,
            order: candidate.order,
            reference: candidate.reference.clone(),
            file: Some(candidate),
        }))
        .collect::<Vec<_>>();
    sort_code_why_source_meta(&mut slots);
    slots
        .into_iter()
        .take(limit)
        .filter_map(|slot| slot.file)
        .collect()
}

async fn file_code_why_sources(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    repo_path: &Path,
    candidates: Vec<CodeWhyFileCandidate>,
) -> Vec<CodeWhySource> {
    let mut sources = Vec::new();
    for candidate in candidates {
        let absolute = repo_path.join(&candidate.relative);
        #[cfg(any(test, feature = "test-util"))]
        CODE_WHY_FILE_READ_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Ok(text) =
            refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk(gcx.clone(), &absolute)
                .await
        else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        sources.push(CodeWhySource {
            kind: candidate.kind.to_string(),
            text,
            reference: candidate.reference,
            priority: candidate.priority,
            order: candidate.order,
        });
    }
    sources
}

#[cfg(any(test, feature = "test-util"))]
pub fn code_why_selected_candidate_count_before_reads(
    repo_path: &Path,
    git_sources: &[CodeWhySource],
    limit: usize,
) -> usize {
    select_code_why_file_candidates(code_why_file_candidates(repo_path), git_sources, limit).len()
}

fn sort_code_why_sources(sources: &mut [CodeWhySource]) {
    sources.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.order.cmp(&right.order))
            .then_with(|| left.reference.cmp(&right.reference))
    });
}

pub fn cap_code_why_sources(mut sources: Vec<CodeWhySource>) -> Vec<CodeWhySource> {
    sort_code_why_sources(&mut sources);
    sources.truncate(CODE_WHY_SOURCE_LIMIT);
    sources
}

pub async fn assemble_code_why_sources(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    repo_path: &Path,
    intel: &refact_git_intel::GitIntel,
) -> Vec<CodeWhySource> {
    let mut sources = git_code_why_sources(intel);
    let file_candidates = select_code_why_file_candidates(
        code_why_file_candidates(repo_path),
        &sources,
        CODE_WHY_SOURCE_LIMIT,
    );
    sources.extend(file_code_why_sources(gcx, repo_path, file_candidates).await);
    cap_code_why_sources(sources)
}

fn code_why_candidates(sources: &[CodeWhySource]) -> Vec<CodeWhyCandidate> {
    let mut out = Vec::new();
    for source in sources {
        let decision_source = refact_codewiki::DecisionSource {
            kind: source.kind.clone(),
            text: source.text.clone(),
        };
        for extracted in refact_codewiki::sources::extract_all(&[decision_source]) {
            out.push(CodeWhyCandidate {
                decision: refact_codewiki::Decision {
                    statement: extracted.statement,
                    evidence: extracted.evidence,
                    source_kind: extracted.source_kind,
                    status: extracted.status,
                    provenance: extracted.provenance,
                    corroboration_count: 1,
                },
                source_rank: extracted.source_rank,
                source_ref: source.reference.clone(),
            });
        }
    }
    out
}

fn better_code_why_candidate(left: &CodeWhyCandidate, right: &CodeWhyCandidate) -> bool {
    left.source_rank > right.source_rank
        || (left.source_rank == right.source_rank && left.source_ref < right.source_ref)
}

fn representative_candidate<'a>(
    decision: &refact_codewiki::Decision,
    candidates: &'a [CodeWhyCandidate],
) -> Option<&'a CodeWhyCandidate> {
    let mut best = None;
    for candidate in candidates {
        if !refact_codewiki::semantic_match::is_duplicate_decision_text(
            &decision.statement,
            &candidate.decision.statement,
        ) {
            continue;
        }
        if best
            .map(|existing| better_code_why_candidate(candidate, existing))
            .unwrap_or(true)
        {
            best = Some(candidate);
        }
    }
    best
}

fn corroborated_code_why_decisions(candidates: &[CodeWhyCandidate]) -> Vec<CodeWhyDecision> {
    let mut decisions: Vec<refact_codewiki::Decision> = candidates
        .iter()
        .map(|candidate| candidate.decision.clone())
        .collect();
    refact_codewiki::corroborate(&mut decisions);
    decisions
        .into_iter()
        .map(|decision| {
            let (source_rank, source_ref) = representative_candidate(&decision, candidates)
                .map(|candidate| (candidate.source_rank, candidate.source_ref.clone()))
                .unwrap_or_else(|| {
                    (
                        source_rank_for_kind(&decision.source_kind),
                        decision.source_kind.clone(),
                    )
                });
            CodeWhyDecision {
                decision,
                source_rank,
                source_ref,
            }
        })
        .collect()
}

fn source_rank_for_kind(kind: &str) -> u8 {
    match kind {
        "adr" => refact_codewiki::sources::SOURCE_RANK_ADR,
        "pr" => refact_codewiki::sources::SOURCE_RANK_PR,
        "commit" => refact_codewiki::sources::SOURCE_RANK_COMMIT,
        "changelog" => refact_codewiki::sources::SOURCE_RANK_CHANGELOG,
        "code_comment" => refact_codewiki::sources::SOURCE_RANK_CODE_COMMENT,
        _ => refact_codewiki::sources::SOURCE_RANK_COMMENT,
    }
}

fn verification_for_status(
    status: refact_codewiki::DecisionStatus,
) -> refact_codewiki::Verification {
    match status {
        refact_codewiki::DecisionStatus::Verified => refact_codewiki::Verification::Exact,
        refact_codewiki::DecisionStatus::Fuzzy => refact_codewiki::Verification::Fuzzy,
        refact_codewiki::DecisionStatus::Unverified => refact_codewiki::Verification::Unverified,
    }
}

fn score_code_why_decision(decision: CodeWhyDecision) -> ScoredCodeWhyDecision {
    let verification = verification_for_status(decision.decision.status);
    let confidence = refact_codewiki::gate::compute_confidence(
        decision.source_rank,
        decision.decision.corroboration_count,
        (decision.decision.provenance, verification),
    );
    ScoredCodeWhyDecision {
        item: decision,
        confidence,
        verification,
    }
}

fn sort_scored_code_why_decisions(decisions: &mut [ScoredCodeWhyDecision]) {
    decisions.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| left.item.source_ref.cmp(&right.item.source_ref))
            .then_with(|| {
                left.item
                    .decision
                    .statement
                    .cmp(&right.item.decision.statement)
            })
    });
}

fn provenance_tag(
    provenance: refact_codewiki::Provenance,
    verification: refact_codewiki::Verification,
) -> &'static str {
    match (provenance, verification) {
        (refact_codewiki::Provenance::Verbatim, _) => "verbatim",
        (refact_codewiki::Provenance::Paraphrase, refact_codewiki::Verification::Unverified) => {
            "paraphrase-unverified"
        }
        (refact_codewiki::Provenance::Paraphrase, _) => "paraphrase-verified",
    }
}

fn related_decision_graph(
    matched: &[ScoredCodeWhyDecision],
    all: &[CodeWhyDecision],
) -> Option<refact_codewiki::DecisionGraph> {
    let mut included = BTreeSet::new();
    for matched_item in matched.iter().take(5) {
        let Some(self_idx) = all.iter().position(|candidate| {
            candidate.decision.statement == matched_item.item.decision.statement
                && candidate.source_ref == matched_item.item.source_ref
        }) else {
            continue;
        };
        let mut hits: Vec<refact_codewiki::semantic_match::SearchHit> = all
            .iter()
            .enumerate()
            .map(
                |(idx, candidate)| refact_codewiki::semantic_match::SearchHit {
                    page_id: refact_codewiki::semantic_match::decision_page_id(&idx.to_string()),
                    score: refact_codewiki::evolution::text_similarity(
                        &matched_item.item.decision.statement,
                        &candidate.decision.statement,
                    ),
                },
            )
            .collect();
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.page_id.cmp(&right.page_id))
        });
        let exclude_ids = HashSet::from([self_idx.to_string()]);
        let related = refact_codewiki::semantic_match::find_related_decisions(
            &hits,
            refact_codewiki::evolution::RELATED_TAU,
            refact_codewiki::semantic_match::DEFAULT_DEDUP_TAU,
            &exclude_ids,
            CODE_WHY_RELATED_PER_MATCH,
        );
        if related.is_empty() {
            continue;
        }
        included.insert(self_idx);
        for (id, _) in related {
            if let Ok(idx) = id.parse::<usize>() {
                included.insert(idx);
            }
        }
    }
    if included.len() < 2 {
        return None;
    }
    let decisions = included
        .into_iter()
        .filter_map(|idx| all.get(idx).map(|item| item.decision.clone()))
        .collect::<Vec<_>>();
    let graph = refact_codewiki::build_decision_graph(decisions);
    (!graph.edges.is_empty()).then_some(graph)
}

fn relation_label(relation: refact_codewiki::Relation) -> &'static str {
    match relation {
        refact_codewiki::Relation::Supersedes => "supersedes",
        refact_codewiki::Relation::Refines => "refines",
        refact_codewiki::Relation::RelatesTo => "relates",
        refact_codewiki::Relation::ConflictsWith => "conflicts",
    }
}

#[derive(Serialize)]
pub struct CodeWhyResponse {
    pub query: String,
    pub source_count: usize,
    pub commits_analyzed: u32,
    pub decisions: Vec<CodeWhyDecisionEntry>,
    pub related: Vec<CodeWhyRelation>,
}
pub fn code_why_response(
    query: &str,
    sources: &[CodeWhySource],
    commits_analyzed: u32,
) -> Option<CodeWhyResponse> {
    let candidates = code_why_candidates(sources);
    let decisions = corroborated_code_why_decisions(&candidates);
    let query_tokens = code_why_tokens(query);
    let mut all_scored: Vec<ScoredCodeWhyDecision> =
        decisions.into_iter().map(score_code_why_decision).collect();
    sort_scored_code_why_decisions(&mut all_scored);
    let mut matched: Vec<ScoredCodeWhyDecision> = all_scored
        .iter()
        .filter(|scored| code_why_statement_matches(&scored.item.decision.statement, &query_tokens))
        .cloned()
        .collect();
    matched.truncate(CODE_WHY_MATCH_LIMIT);
    if matched.is_empty() {
        return None;
    }

    let decisions =
        matched
            .iter()
            .map(|scored| {
                let d = &scored.item.decision;
                CodeWhyDecisionEntry {
                    kind: d.source_kind.clone(),
                    confidence: scored.confidence,
                    corroboration: d.corroboration_count as usize,
                    source_kind: d.source_kind.clone(),
                    source_ref: scored.item.source_ref.clone(),
                    summary: d.statement.clone(),
                    provenance_tags: vec![
                        provenance_tag(d.provenance, scored.verification).to_string()
                    ],
                }
            })
            .collect();
    let all_decisions: Vec<CodeWhyDecision> = all_scored
        .iter()
        .map(|scored| scored.item.clone())
        .collect();
    let mut related = Vec::new();
    if let Some(graph) = related_decision_graph(&matched, &all_decisions) {
        for (from, to, relation) in graph.edges.iter().take(12) {
            let Some(from_decision) = graph.decisions.get(*from) else {
                continue;
            };
            let Some(to_decision) = graph.decisions.get(*to) else {
                continue;
            };
            related.push(CodeWhyRelation {
                from: from_decision.statement.clone(),
                relation: relation_label(*relation).to_string(),
                to: to_decision.statement.clone(),
            });
        }
    }
    Some(CodeWhyResponse {
        query: query.to_string(),
        source_count: sources.len(),
        commits_analyzed,
        decisions,
        related,
    })
}

pub fn code_why_tokens(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

pub fn code_why_statement_matches(statement: &str, query_tokens: &BTreeSet<String>) -> bool {
    !query_tokens.is_empty() && query_tokens.is_subset(&code_why_tokens(statement))
}

#[async_trait]
impl Tool for ToolCodeWhy {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let query = string_arg(args, "query")?;
        let roots = codegraph_project_roots(ccx.clone())
            .await?
            .ok_or_else(|| "no project directory available".to_string())?;
        let gcx = ccx.lock().await.app.gcx.clone();
        let intel = cached_mine_history_async(&roots.git_root, CODE_WHY_HISTORY_COMMITS).await?;
        let sources = assemble_code_why_sources(gcx, &roots.git_root, &intel).await;
        let Some(response) = code_why_response(&query, &sources, intel.commits_analyzed) else {
            let response = CodeWhyResponse {
                query: query.clone(),
                source_count: sources.len(),
                commits_analyzed: intel.commits_analyzed,
                decisions: Vec::new(),
                related: Vec::new(),
            };
            return Ok((
                false,
                tool_message(
                    tool_call_id,
                    ToolJson::new(
                        "code_why",
                        format!("No matching decision records found for `{query}`"),
                        response,
                    )
                    .to_text(),
                ),
            ));
        };
        let summary = format!(
            "{} decisions matching `{query}` from {} sources",
            response.decisions.len(),
            response.source_count
        );
        Ok((
            false,
            tool_message(
                tool_call_id,
                ToolJson::new("code_why", summary, response).to_text(),
            ),
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "code_why".to_string(),
            display_name: "Code Why".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Explain why the code is the way it is: mines significant commit prose, merge PR bodies, ADRs, and changelogs for query-matching decisions, then reports source refs, confidence, corroboration, provenance tags, and related-decision links.".to_string(),
            input_schema: json_schema_from_params(
                &[("query", "string", "Topic or keyword to find decisions about.")],
                &["query"],
            ),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        codegraph_dependency()
    }
}
