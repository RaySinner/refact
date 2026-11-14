#[allow(unused_imports)]
use super::*;

pub fn execution_flow_truncation_notice(
    flows: &[refact_codegraph::communities::ExecFlow],
) -> Option<String> {
    let truncated = flows.iter().filter(|flow| flow.truncated).count();
    if truncated == 0 {
        return None;
    }
    let node_cap = flows
        .iter()
        .filter(|flow| flow.truncated)
        .map(|flow| flow.node_cap)
        .min()
        .unwrap_or_default();
    Some(format!(
        "execution flow truncated: {truncated} of {} flows stopped at the {node_cap}-node cap, \
         raise codegraph_exec_flow_max_nodes",
        flows.len()
    ))
}

pub struct ToolCodegraphOverview {
    pub config_path: String,
}
#[derive(Serialize)]
pub struct OverviewToolResponse {
    #[serde(flatten)]
    pub overview: OverviewResponse,
    pub partial: bool,
    pub warning: Option<String>,
    pub communities: Vec<OverviewCommunity>,
    pub execution_flows: Vec<OverviewExecutionFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_flow_truncation: Option<String>,
    pub dead_code: Vec<OverviewDeadSymbol>,
    pub entry_points: Vec<String>,
    pub api_contract_files: Vec<String>,
}
#[async_trait]
impl Tool for ToolCodegraphOverview {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        _args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let gcx = ccx.lock().await.app.gcx.clone();
        let roots = codegraph_project_roots(ccx.clone()).await?;
        let service = gcx
            .codegraph
            .lock()
            .await
            .clone()
            .ok_or_else(|| "codegraph is not available".to_string())?;
        let readiness = service.index_readiness().await?;
        let index_state = pr_blast_index_state(&readiness);
        let cached = service.cached_graph_analytics().await?;
        let view = match analytics_scope_root(roots.as_ref()) {
            Some(root) => {
                let paths = scoped_graph_paths(&cached.data, root);
                service.scoped_graph_analytics(&paths).await?
            }
            None => cached,
        };
        let graph = view.data.clone();
        let analytics = &view.analytics;
        let overview = analytics.overview.truncated(15);
        let mut warning = pr_blast_partial_warning(&index_state);
        let mut communities = view.communities.clone();
        communities.sort_by(|a, b| b.members.len().cmp(&a.members.len()));
        let community_count = communities.len();
        let communities = communities
            .into_iter()
            .take(8)
            .map(|community| OverviewCommunity {
                label: community.label,
                member_count: community.members.len(),
                cohesion: community.cohesion,
            })
            .collect();
        let flows =
            refact_codegraph::communities::execution_flows_from_data(&graph, 5).unwrap_or_default();
        let execution_flow_truncation = execution_flow_truncation_notice(&flows);
        let execution_flows = flows
            .into_iter()
            .map(|flow| OverviewExecutionFlow {
                entry: flow.entry,
                reaches: flow.reached,
                depth: flow.depth,
                truncated: flow.truncated,
                node_cap: flow.node_cap,
            })
            .collect();
        let dead_code_truncation = view.dead_code.truncation_notice();
        let mut dead = view.dead_code.symbols.clone();
        dead.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        let dead_code_count = dead.len();
        let dead_code = dead
            .into_iter()
            .take(15)
            .map(|symbol| OverviewDeadSymbol {
                name: symbol.name,
                path: symbol.path,
                reason: symbol.reason,
                confidence: symbol.confidence,
            })
            .collect();
        let all_files = service
            .all_files_with_text()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|(path, _)| {
                analytics_scope_root(roots.as_ref())
                    .is_none_or(|root| code_intel_path_allowed(root, path))
            })
            .collect::<Vec<_>>();
        let centrality = analytics.file_centrality.truncated(100);
        let pr: HashMap<String, f64> = centrality.top_pagerank.iter().cloned().collect();
        let bt: HashMap<String, f64> = centrality.top_betweenness.iter().cloned().collect();
        let candidates: Vec<(String, f64, f64)> = all_files
            .iter()
            .map(|(p, _)| {
                let pagerank = pr.get(p).copied().unwrap_or(0.0);
                let betweenness = bt.get(p).copied().unwrap_or(0.0);
                (p.clone(), pagerank, betweenness)
            })
            .collect();
        let stems = refact_codewiki::entry_points::default_conventional_stems();
        let entry_points = refact_codewiki::entry_points::rank_entry_points(&candidates, &stems)
            .into_iter()
            .take(10)
            .collect();
        let mut api_files: Vec<String> = all_files
            .iter()
            .filter(|(p, t)| {
                let lang = refact_codegraph::lang_from_path(p);
                if !matches!(lang, "python" | "py" | "csharp" | "cs") {
                    return false;
                }
                refact_codewiki::api_contract::detect_api_contract(
                    &refact_codewiki::parsed_file::build_parsed_file(lang, t),
                )
            })
            .map(|(p, _)| p.clone())
            .collect();
        api_files.sort();
        api_files.truncate(15);
        for notice in [execution_flow_truncation.clone(), dead_code_truncation]
            .into_iter()
            .flatten()
        {
            warning = Some(match warning {
                Some(existing) => format!("{existing}; {notice}"),
                None => notice,
            });
        }
        let data = OverviewToolResponse {
            overview: OverviewResponse {
                counts: CodeIntelCounts {
                    nodes: overview.node_count as i64,
                    edges: overview.edge_count as i64,
                    files: all_files.len() as i64,
                },
                index_state,
                scc_count: overview.scc_count,
                largest_scc: overview.largest_scc,
                component_count: overview.component_count,
                top_pagerank: overview
                    .top_pagerank
                    .into_iter()
                    .map(|entry| ScoreEntry {
                        symbol: entry.symbol,
                        path: entry.path,
                        score: entry.score,
                    })
                    .collect(),
                top_betweenness: overview
                    .top_betweenness
                    .into_iter()
                    .filter(|entry| entry.score > 0.0)
                    .map(|entry| ScoreEntry {
                        symbol: entry.symbol,
                        path: entry.path,
                        score: entry.score,
                    })
                    .collect(),
                file_centrality: FileCentralityResponse {
                    top_pagerank: centrality
                        .top_pagerank
                        .into_iter()
                        .map(|(path, score)| FileScoreEntry { path, score })
                        .collect(),
                    top_betweenness: centrality
                        .top_betweenness
                        .into_iter()
                        .map(|(path, score)| FileScoreEntry { path, score })
                        .collect(),
                },
                community_count,
                dead_code_count,
            },
            partial: !readiness.cross_file_ready,
            warning,
            communities,
            execution_flows,
            execution_flow_truncation,
            dead_code,
            entry_points,
            api_contract_files: api_files,
        };
        let mut summary = format!(
            "Code graph: {} nodes, {} edges, {} components",
            data.overview.counts.nodes, data.overview.counts.edges, data.overview.component_count
        );
        if let Some(notice) = data.warning.as_ref() {
            summary.push_str(&format!("; {notice}"));
        }
        Ok((
            false,
            tool_message(
                tool_call_id,
                ToolJson::new("codegraph_overview", summary, data).to_text(),
            ),
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "codegraph_overview".to_string(),
            display_name: "Code Graph Overview".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Project-wide CodeGraph overview from the generation-keyed analytics cache: readiness warning, node/edge counts, SCCs, PageRank/betweenness symbols with paths, communities, entry points, API-contract files, and likely dead code.".to_string(),
            input_schema: json_schema_from_params(&[], &[]),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        codegraph_dependency()
    }
}
pub struct ToolSecurityScan {
    pub config_path: String,
}
#[async_trait]
impl Tool for ToolSecurityScan {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let requested_file_path = string_arg(args, "file_path")?;
        let resolved_file = resolve_codegraph_file(ccx.clone(), &requested_file_path).await?;
        let file_path = resolved_file.indexed_path;
        let gcx = ccx.lock().await.app.gcx.clone();
        refact_runtime_api::privacy::load_privacy_if_needed(gcx.clone()).await;
        let record_path = resolved_file.read_path.clone();
        let text = refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk(
            gcx.clone(),
            &resolved_file.read_path,
        )
        .await?
        .to_string();
        let service = gcx
            .codegraph
            .lock()
            .await
            .clone()
            .ok_or_else(|| "codegraph is not available".to_string())?;
        let lang = refact_codegraph::lang_from_path(&file_path);
        let findings = service.security_scan(&file_path, lang, &text).await?;
        if findings.is_empty() {
            let data = SecurityScanResponse {
                path: requested_file_path.clone(),
                lang: lang.to_string(),
                finding_count: 0,
                counts: BTreeMap::new(),
                findings: Vec::new(),
                omitted: 0,
            };
            return Ok((
                false,
                tool_message_with_records(
                    &gcx,
                    tool_call_id,
                    ToolJson::new(
                        "security_scan",
                        format!(
                    "Security scan for `{requested_file_path}` found no findings (lang: {lang})"
                ),
                        data,
                    )
                    .to_text(),
                    [record_path.clone()],
                )?,
            ));
        }

        let mut counts: HashMap<&'static str, usize> = HashMap::new();
        for finding in &findings {
            *counts.entry(severity_label(finding.severity)).or_insert(0) += 1;
        }
        let finding_count = findings.len();
        let omitted = finding_count.saturating_sub(50);
        let counts = counts
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        let data = SecurityScanResponse {
            path: requested_file_path.clone(),
            lang: lang.to_string(),
            finding_count,
            counts,
            findings: findings.into_iter().take(50).collect(),
            omitted,
        };
        let summary =
            format!("Security scan for `{requested_file_path}` found {finding_count} findings");
        Ok((
            false,
            tool_message_with_records(
                &gcx,
                tool_call_id,
                ToolJson::new("security_scan", summary, data).to_text(),
                [record_path],
            )?,
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "security_scan".to_string(),
            display_name: "Security Scan".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Security scan for one file using CodeGraph security heuristics: deduped findings for hardcoded secrets, dynamic SQL and command execution, dangerous eval/deserialization, TLS verification disabled, weak crypto, and insecure randomness.".to_string(),
            input_schema: json_schema_from_params(
                &[(
                    "file_path",
                    "string",
                    "File to scan; accepts the same workspace-relative, absolute current-project, and active-worktree path forms as cat.",
                )],
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
fn severity_label(severity: refact_codegraph::security_scan::Severity) -> &'static str {
    match severity {
        refact_codegraph::security_scan::Severity::Low => "Low",
        refact_codegraph::security_scan::Severity::Medium => "Medium",
        refact_codegraph::security_scan::Severity::High => "High",
        refact_codegraph::security_scan::Severity::Critical => "Critical",
    }
}
