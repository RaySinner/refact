#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeadCodeIndexState {
    pub queued: usize,
    pub dirty_paths: i64,
    pub pending_refs: i64,
    pub cross_file_edges: i64,
    pub cross_file_ready: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeadCodeEntry {
    pub name: String,
    pub path: String,
    pub line: usize,
    pub reason: String,
    pub confidence: f64,
    pub git_recency: String,
    pub incoming_edges: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeadCodeReport {
    pub entries: Vec<DeadCodeEntry>,
    #[serde(skip)]
    pub total_candidates: usize,
    #[serde(default)]
    pub analysis_total_found: usize,
    #[serde(default)]
    pub analysis_truncated: bool,
    pub index_state: DeadCodeIndexState,
    pub partial: bool,
    pub warning: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeadCodeGitFacts {
    pub last_touched_days: Option<u64>,
    pub file_churn: u32,
    pub recent_churn_days: Option<u64>,
}
pub async fn dead_code_report(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    limit: usize,
    path_filter: Option<&str>,
    min_confidence: f64,
) -> Result<DeadCodeReport, String> {
    let repo_dir = project_dir(gcx.clone()).await;
    dead_code_report_with_roots(
        gcx,
        limit,
        path_filter,
        min_confidence,
        repo_dir.as_deref(),
        repo_dir.as_deref(),
        None,
    )
    .await
}

async fn dead_code_report_with_roots(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    limit: usize,
    path_filter: Option<&str>,
    min_confidence: f64,
    git_root: Option<&Path>,
    indexed_root: Option<&Path>,
    analytics_scope_root: Option<&Path>,
) -> Result<DeadCodeReport, String> {
    let service = gcx
        .codegraph
        .lock()
        .await
        .clone()
        .ok_or_else(|| "codegraph is not available".to_string())?;
    let readiness = service.index_readiness().await?;
    let index_state = dead_code_index_state(&readiness);
    let mut warning = dead_code_partial_warning(&index_state);
    let dead = match analytics_scope_root {
        Some(root) => {
            let cached = service.cached_graph_analytics().await?;
            let paths = scoped_graph_paths(&cached.data, root);
            service
                .scoped_graph_analytics(&paths)
                .await?
                .dead_code
                .clone()
        }
        None => service.dead_code().await?,
    };
    let intel = match git_root {
        Some(dir) => cached_mine_history_async(dir, DEAD_CODE_HISTORY_COMMITS)
            .await
            .ok(),
        None => None,
    };
    let analysis_total_found = dead.total_found;
    let analysis_truncated = dead.truncated;
    let analysis_notice = dead.truncation_notice();
    let history_notice = intel
        .as_ref()
        .and_then(|intel| intel.history_truncation().notice);
    let now_ts = current_unix_ts();
    let facts = {
        let dead = dead.symbols.clone();
        let git_root = git_root.map(Path::to_path_buf);
        let indexed_root = indexed_root.map(Path::to_path_buf);
        let intel = intel.clone();
        tokio::task::spawn_blocking(move || {
            dead_code_git_facts_by_node(
                &dead,
                git_root.as_deref(),
                indexed_root.as_deref(),
                intel.as_ref(),
                now_ts,
            )
        })
        .await
        .map_err(|e| format!("dead code git facts join: {e}"))?
    };
    let min_confidence = min_confidence.clamp(0.0, 1.0);
    let limit = limit.clamp(1, DEAD_CODE_MAX_LIMIT);
    let mut entries = enrich_dead_symbols_with_facts(dead.symbols, &facts)
        .into_iter()
        .filter(|entry| entry.confidence >= min_confidence)
        .filter(|entry| {
            path_filter.is_none_or(|filter| dead_code_path_matches(&entry.path, filter))
        })
        .collect::<Vec<_>>();
    let total_candidates = entries.len();
    entries.truncate(limit);
    for notice in [analysis_notice, history_notice].into_iter().flatten() {
        warning = Some(match warning {
            Some(existing) => format!("{existing}; {notice}"),
            None => notice,
        });
    }
    Ok(DeadCodeReport {
        entries,
        total_candidates,
        analysis_total_found,
        analysis_truncated,
        index_state,
        partial: !readiness.cross_file_ready,
        warning,
    })
}

pub fn dead_code_index_state(
    readiness: &refact_codegraph::IndexReadiness,
) -> DeadCodeIndexState {
    DeadCodeIndexState {
        queued: readiness.queued,
        dirty_paths: readiness.dirty_paths,
        pending_refs: readiness.pending_refs,
        cross_file_edges: readiness.cross_file_edges,
        cross_file_ready: readiness.cross_file_ready,
    }
}

pub fn dead_code_partial_warning(state: &DeadCodeIndexState) -> Option<String> {
    (!state.cross_file_ready).then(|| {
        format!(
            "⚠ index still building ({} files queued, {} dirty paths, {} pending refs) — dead-code may be over-reported",
            state.queued, state.dirty_paths, state.pending_refs
        )
    })
}

pub fn enrich_dead_symbols_with_facts(
    dead: Vec<refact_codegraph::dead_code::DeadSymbol>,
    facts_by_node: &HashMap<i64, DeadCodeGitFacts>,
) -> Vec<DeadCodeEntry> {
    let mut entries = dead
        .into_iter()
        .map(|symbol| {
            let facts = facts_by_node
                .get(&symbol.node_id)
                .cloned()
                .unwrap_or_default();
            DeadCodeEntry {
                name: symbol.name,
                path: symbol.path,
                line: symbol.line,
                reason: symbol.reason,
                confidence: dead_code_confidence(symbol.incoming_edges, &facts),
                git_recency: dead_code_git_recency(&facts),
                incoming_edges: symbol.incoming_edges,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.name.cmp(&b.name))
    });
    entries
}

fn dead_code_confidence(incoming_edges: usize, facts: &DeadCodeGitFacts) -> f64 {
    let mut confidence: f64 = 0.4;
    if facts
        .last_touched_days
        .is_some_and(|age_days| age_days > 365)
        && facts.file_churn == 0
    {
        confidence += 0.3;
    }
    if incoming_edges == 0 {
        confidence += 0.1;
    }
    if facts
        .recent_churn_days
        .is_some_and(|age_days| age_days <= 90)
    {
        confidence -= 0.2;
    }
    confidence.clamp(0.1, 0.9)
}

fn dead_code_git_recency(facts: &DeadCodeGitFacts) -> String {
    match facts.last_touched_days {
        Some(days) => format!(
            "last touched {}d ago; churn {} in mined window",
            days, facts.file_churn
        ),
        None if facts.file_churn == 0 => "no git touches in mined window".to_string(),
        None => format!("churn {} in mined window", facts.file_churn),
    }
}

fn dead_code_git_facts_by_node(
    dead: &[refact_codegraph::dead_code::DeadSymbol],
    repo_root: Option<&Path>,
    indexed_root: Option<&Path>,
    intel: Option<&refact_git_intel::GitIntel>,
    now_ts: i64,
) -> HashMap<i64, DeadCodeGitFacts> {
    let last_touch_by_path = intel.map(|intel| {
        let mut map: HashMap<String, i64> = HashMap::new();
        for commit in &intel.commit_records {
            if commit.ts <= 0 {
                continue;
            }
            for (path, _, _) in &commit.files {
                let normalized = refact_git_intel::paths::normalize_separators(path);
                let entry = map.entry(normalized).or_insert(commit.ts);
                if commit.ts > *entry {
                    *entry = commit.ts;
                }
            }
        }
        map
    });
    let mut facts_by_node = HashMap::new();
    let mut ranges_by_path: HashMap<String, Vec<(String, usize, usize)>> = HashMap::new();
    for symbol in dead {
        let key = dead_code_git_key(&symbol.path, indexed_root.or(repo_root));
        let facts = DeadCodeGitFacts {
            last_touched_days: None,
            file_churn: intel
                .and_then(|intel| intel.file_churn.get(&key).copied())
                .unwrap_or(0),
            recent_churn_days: last_touch_by_path
                .as_ref()
                .and_then(|map| map.get(&key))
                .and_then(|ts| days_since_ts(now_ts, *ts)),
        };
        facts_by_node.insert(symbol.node_id, facts);
        if repo_root.is_some() {
            ranges_by_path.entry(key).or_default().push((
                symbol.node_id.to_string(),
                symbol.line.max(1),
                symbol.line.max(1),
            ));
        }
    }
    let Some(repo_root) = repo_root else {
        return facts_by_node;
    };
    for (path, ranges) in ranges_by_path {
        for range in cached_blame_range_facts(repo_root, &path, &ranges) {
            let Ok(node_id) = range.name.parse::<i64>() else {
                continue;
            };
            if let Some(facts) = facts_by_node.get_mut(&node_id) {
                facts.last_touched_days = days_since_ts(now_ts, range.last_modified_ts);
            }
        }
    }
    facts_by_node
}

fn dead_code_git_key(path: &str, repo_root: Option<&Path>) -> String {
    match repo_root {
        Some(root) => {
            refact_git_intel::paths::repo_relative_or_basename(path, &root.to_string_lossy())
        }
        None => refact_git_intel::paths::normalize_separators(path),
    }
}

fn days_since_ts(now_ts: i64, ts: i64) -> Option<u64> {
    if ts <= 0 {
        return None;
    }
    Some(now_ts.saturating_sub(ts).max(0) as u64 / 86_400)
}

fn dead_code_path_matches(path: &str, filter: &str) -> bool {
    let normalized = path.replace('\\', "/");
    if normalized == filter || normalized.ends_with(filter) {
        return true;
    }
    let basename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    basename == filter
}

pub struct ToolDeadCode {
    pub config_path: String,
}
#[derive(Serialize)]
pub struct DeadCodeToolResponse {
    #[serde(flatten)]
    pub report: DeadCodeReport,
    pub shown: usize,
    pub total_candidates: usize,
}
#[async_trait]
impl Tool for ToolDeadCode {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let limit = optional_usize_arg(args, "limit", DEAD_CODE_DEFAULT_LIMIT)?
            .clamp(1, DEAD_CODE_MAX_LIMIT);
        let min_confidence = optional_f64_arg(args, "min_confidence", 0.5)?.clamp(0.0, 1.0);
        let requested_path = optional_string_arg(args, "path")?;
        let path = match requested_path.as_ref() {
            Some(path) => Some(resolve_codegraph_filter_path(ccx.clone(), path).await?),
            None => None,
        };
        let roots = codegraph_project_roots(ccx.clone()).await?;
        let gcx = ccx.lock().await.app.gcx.clone();
        let report = dead_code_report_with_roots(
            gcx.clone(),
            limit,
            path.as_deref(),
            min_confidence,
            roots.as_ref().map(|roots| roots.git_root.as_path()),
            roots.as_ref().map(|roots| roots.indexed_root.as_path()),
            analytics_scope_root(roots.as_ref()),
        )
        .await?;
        let shown = report.entries.len();
        let total_candidates = report.total_candidates;
        let mut summary = format!(
            "Dead code candidates: showing {shown} of {total_candidates} matching candidates"
        );
        if report.analysis_truncated {
            summary.push_str(&format!(
                "; analysis truncated: showing {} of {} detected candidates, raise \
                 codegraph_dead_code_max_results",
                total_candidates, report.analysis_total_found
            ));
        }
        Ok((
            false,
            match requested_path {
                Some(requested_path) => {
                    refact_runtime_api::privacy::load_privacy_if_needed(gcx.clone()).await;
                    tool_message_with_records(
                        &gcx,
                        tool_call_id,
                        ToolJson::new(
                            "dead_code",
                            summary,
                            DeadCodeToolResponse {
                                report,
                                shown,
                                total_candidates,
                            },
                        )
                        .to_text(),
                        [PathBuf::from(requested_path)],
                    )?
                }
                None => tool_message(
                    tool_call_id,
                    ToolJson::new(
                        "dead_code",
                        summary,
                        DeadCodeToolResponse {
                            report,
                            shown,
                            total_candidates,
                        },
                    )
                    .to_text(),
                ),
            },
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "dead_code".to_string(),
            display_name: "Dead Code".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Dead-code candidates from cached CodeGraph reachability: excludes override methods, build scripts, and shell setup/install entry points; adds git recency/churn confidence, path/min-confidence filters, and readiness warnings while the index is partial.".to_string(),
            input_schema: json_schema_from_params(
                &[
                    ("limit", "integer", "Maximum number of candidates to return. Defaults to 50, max 500."),
                    (
                        "path",
                        "string",
                        "Optional workspace-relative, absolute current-project, active-worktree, or basename filter; accepts the same path forms as cat.",
                    ),
                    ("min_confidence", "number", "Minimum confidence threshold. Defaults to 0.5."),
                ],
                &[],
            ),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        codegraph_dependency()
    }
}
