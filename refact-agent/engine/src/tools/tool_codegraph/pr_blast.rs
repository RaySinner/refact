#[allow(unused_imports)]
use super::*;

pub(crate) fn pr_blast_index_state(
    readiness: &refact_codegraph::IndexReadiness,
) -> PrBlastIndexState {
    PrBlastIndexState {
        queued: readiness.queued,
        cross_file_edges: readiness.cross_file_edges,
        cross_file_ready: readiness.cross_file_ready,
    }
}

pub(crate) fn pr_blast_partial_warning(state: &PrBlastIndexState) -> Option<String> {
    (!state.cross_file_ready).then(|| {
        format!(
            "⚠ index still building ({} files queued, {} cross-file edges) — impact may be under-reported",
            state.queued, state.cross_file_edges
        )
    })
}

pub(crate) fn pr_blast_suggested_reviewers(
    report: &refact_codegraph::pr_blast::BlastReport,
    repo_root: Option<&Path>,
    intel: Option<&refact_git_intel::GitIntel>,
) -> Vec<SuggestedReviewer> {
    let (Some(repo_root), Some(intel)) = (repo_root, intel) else {
        return Vec::new();
    };
    let repo_root = repo_root.to_string_lossy();
    let mut paths = BTreeSet::new();
    for path in &report.changed_files {
        paths.insert(refact_git_intel::paths::repo_relative_or_basename(
            path, &repo_root,
        ));
    }
    for impact in report
        .directly_impacted
        .iter()
        .chain(report.transitively_impacted.iter())
    {
        paths.insert(refact_git_intel::paths::repo_relative_or_basename(
            &impact.path,
            &repo_root,
        ));
    }

    let mut scores: HashMap<String, f64> = HashMap::new();
    for path in paths {
        for owner in intel.ownership(&path) {
            if owner.share <= 0.0 || pr_blast_is_bot_author(&owner.author) {
                continue;
            }
            *scores.entry(owner.author).or_insert(0.0) += owner.share;
        }
    }

    let mut reviewers: Vec<SuggestedReviewer> = scores
        .into_iter()
        .map(|(author, score)| SuggestedReviewer { author, score })
        .collect();
    reviewers.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.author.cmp(&b.author))
    });
    reviewers.truncate(PR_BLAST_REVIEWER_LIMIT);
    reviewers
}

pub(crate) fn pr_blast_is_bot_author(author: &str) -> bool {
    let author = author.to_ascii_lowercase();
    PR_BLAST_BOT_AUTHOR_PATTERNS
        .iter()
        .any(|pattern| author.contains(pattern))
}

pub struct ToolPrBlast {
    pub config_path: String,
}
#[derive(Serialize)]
struct PrBlastToolResponse {
    #[serde(flatten)]
    response: PrBlastResponse,
    max_depth: usize,
}
#[async_trait]
impl Tool for ToolPrBlast {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let changed_files = string_list_arg(args, "changed_files")?;
        if changed_files.is_empty() {
            return Err("argument `changed_files` must not be empty".to_string());
        }
        let max_depth = optional_usize_arg(args, "max_depth", 3)?.clamp(1, 10);
        let mut resolved_changed_files = Vec::with_capacity(changed_files.len());
        for path in &changed_files {
            resolved_changed_files.push(resolve_codegraph_filter_path(ccx.clone(), path).await?);
        }
        let gcx = ccx.lock().await.app.gcx.clone();
        refact_runtime_api::privacy::load_privacy_if_needed(gcx.clone()).await;
        let service = gcx
            .codegraph
            .lock()
            .await
            .clone()
            .ok_or_else(|| "codegraph is not available".to_string())?;
        let readiness = service.index_readiness().await?;
        let index_state = pr_blast_index_state(&readiness);
        let warning = pr_blast_partial_warning(&index_state);
        let roots = codegraph_project_roots(ccx.clone()).await?;
        let report = match analytics_scope_root(roots.as_ref()) {
            Some(root) => {
                let cached = service.cached_graph_analytics().await?;
                let paths = scoped_graph_paths(&cached.data, root);
                service
                    .pr_blast_scoped(&resolved_changed_files, max_depth, &paths)
                    .await?
            }
            None => service.pr_blast(&resolved_changed_files, max_depth).await?,
        };
        let intel = match roots.as_ref().map(|roots| roots.git_root.as_path()) {
            Some(dir) => cached_mine_history_async(dir, 1000).await.ok(),
            None => None,
        };
        let reviewers = pr_blast_suggested_reviewers(
            &report,
            roots.as_ref().map(|roots| roots.indexed_root.as_path()),
            intel.as_ref(),
        );

        let changed_count = report.changed_files.len();
        let impacted_count = report.impacted_file_count;
        let data = PrBlastToolResponse {
            response: PrBlastResponse {
                changed_files: report.changed_files,
                directly_impacted: report.directly_impacted.into_iter().take(30).collect(),
                transitively_impacted: report.transitively_impacted.into_iter().take(30).collect(),
                impacted_file_count: report.impacted_file_count,
                risk_score: report.risk_score,
                suggested_reviewers: reviewers,
                index_state,
                partial: !readiness.cross_file_ready,
                warning,
            },
            max_depth,
        };
        let summary = format!(
            "PR blast radius for {changed_count} changed files: {impacted_count} impacted files"
        );
        Ok((
            false,
            tool_message_with_records(
                &gcx,
                tool_call_id,
                ToolJson::new("pr_blast", summary, data).to_text(),
                changed_files.into_iter().map(PathBuf::from),
            )?,
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "pr_blast".to_string(),
            display_name: "PR Blast Radius".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "PR blast-radius analysis for changed files: resolves indexed paths, walks reverse CodeGraph dependencies to max depth, reports direct/transitive impacted symbols, structural vs behavioral impact kind, impacted file count, risk score, git-ownership reviewer suggestions excluding bot authors, and readiness/partial-index warnings.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "changed_files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Changed files to analyze; each accepts the same workspace-relative, absolute current-project, active-worktree, or basename forms as cat. Deleted paths are allowed."
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum reverse dependency depth to walk. Defaults to 3, max 10."
                    }
                },
                "required": ["changed_files"]
            }),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        codegraph_dependency()
    }
}
