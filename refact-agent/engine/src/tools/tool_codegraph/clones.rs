#[allow(unused_imports)]
use super::*;

pub(crate) struct CloneAnalysis {
    pub(crate) generation: u64,
    pub(crate) clones: Vec<refact_codehealth::duplication::CrossFileClonePair>,
    pub(crate) duplication_pct: f64,
    pub(crate) files: usize,
    pub(crate) text_by_path: HashMap<String, (String, String)>,
    pub(crate) tokens_by_path: HashMap<String, usize>,
    pub(crate) duplicated_tokens_by_path: HashMap<String, usize>,
}
#[derive(Default)]
struct CrossFileCloneCache {
    analyses: HashMap<usize, (Weak<refact_codegraph::CodeGraphService>, Arc<CloneAnalysis>)>,
}
fn clone_analysis_cache() -> &'static AMutex<CrossFileCloneCache> {
    static CACHE: OnceLock<AMutex<CrossFileCloneCache>> = OnceLock::new();
    CACHE.get_or_init(|| AMutex::new(CrossFileCloneCache::default()))
}

pub(crate) async fn cached_cross_file_clones(
    gcx: Arc<crate::global_context::GlobalContext>,
) -> Result<Arc<CloneAnalysis>, String> {
    let service = gcx
        .codegraph
        .lock()
        .await
        .clone()
        .ok_or_else(|| "codegraph is not available".to_string())?;
    let key = Arc::as_ptr(&service) as usize;
    {
        let generation = service.graph_generation();
        if let Some(analysis) = cached_clone_analysis(&service, key, generation).await {
            return Ok(analysis);
        }

        let files_text = service.all_files_with_text().await?;
        let rebuilt = tokio::task::spawn_blocking(move || {
            let triples = files_text
                .iter()
                .map(|(path, text)| {
                    (
                        path.clone(),
                        refact_codegraph::lang_from_path(path).to_string(),
                        text.clone(),
                    )
                })
                .collect::<Vec<_>>();
            let analysis = refact_codehealth::duplication::cross_file_analysis(&triples);
            let files = files_text.len();
            let text_by_path = files_text
                .into_iter()
                .map(|(path, text)| {
                    let lang = refact_codegraph::lang_from_path(&path).to_string();
                    (path, (lang, text))
                })
                .collect::<HashMap<_, _>>();
            Arc::new(CloneAnalysis {
                generation,
                clones: analysis.clones,
                duplication_pct: analysis.duplication_pct,
                files,
                text_by_path,
                tokens_by_path: analysis.tokens_by_path,
                duplicated_tokens_by_path: analysis.duplicated_tokens_by_path,
            })
        })
        .await
        .map_err(|e| format!("cross-file clone analysis join: {e}"))?;

        let mut cache = clone_analysis_cache().lock().await;
        cache
            .analyses
            .retain(|_, (weak, _)| weak.upgrade().is_some());
        let current_generation = service.graph_generation();
        if let Some((weak, cached)) = cache.analyses.get(&key) {
            if weak
                .upgrade()
                .as_ref()
                .is_some_and(|cached_service| Arc::ptr_eq(cached_service, &service))
                && cached.generation == current_generation
            {
                return Ok(cached.clone());
            }
        }
        if current_generation == generation {
            cache
                .analyses
                .insert(key, (Arc::downgrade(&service), rebuilt.clone()));
        }
        // The graph may keep advancing during active indexing; a stale-but-fresh
        // analysis is still valid for this request. Returning it (uncached when
        // the generation moved) avoids livelocking/recomputing until quiescence.
        return Ok(rebuilt);
    }
}

async fn cached_clone_analysis(
    service: &Arc<refact_codegraph::CodeGraphService>,
    key: usize,
    generation: u64,
) -> Option<Arc<CloneAnalysis>> {
    let cache = clone_analysis_cache().lock().await;
    cache.analyses.get(&key).and_then(|(weak, analysis)| {
        weak.upgrade().and_then(|cached_service| {
            (Arc::ptr_eq(&cached_service, service) && analysis.generation == generation)
                .then(|| analysis.clone())
        })
    })
}

pub struct ToolCodeDuplication {
    pub config_path: String,
}
#[async_trait]
impl Tool for ToolCodeDuplication {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        _args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let gcx = ccx.lock().await.app.gcx.clone();
        let roots = codegraph_project_roots(ccx.clone()).await?;
        let global_analysis = cached_cross_file_clones(gcx.clone()).await?;
        let analysis = match analytics_scope_root(roots.as_ref()) {
            Some(root) => Some(scoped_clone_analysis(&global_analysis, root).await?),
            None => None,
        };
        let analysis = analysis.as_ref().unwrap_or(global_analysis.as_ref());
        if analysis.clones.is_empty() {
            let data = DuplicationResponse {
                aggregate: DuplicationAggregateResponse {
                    file_count: analysis.files,
                    clone_pair_count: 0,
                    duplication_pct: analysis.duplication_pct,
                    duplication_percent: analysis.duplication_pct * 100.0,
                },
                clones: Vec::new(),
                dry_violations: Vec::new(),
                test_smells: Vec::new(),
            };
            return Ok((
                false,
                tool_message(
                    tool_call_id,
                    ToolJson::new("code_duplication", "No cross-file clones detected", data)
                        .to_text(),
                ),
            ));
        }
        let dup_pct = analysis.duplication_pct * 100.0;
        let project_root = roots.as_ref().map(|roots| roots.indexed_root.as_path());
        let intel = match roots.as_ref().map(|roots| roots.git_root.as_path()) {
            Some(dir) => cached_mine_history_async(dir, 1000).await.ok(),
            None => None,
        };
        let co_change =
            |a: &str, b: &str| -> u32 { co_change_count(intel.as_ref(), a, b, project_root) };
        let mut per_file: HashMap<
            String,
            Vec<&refact_codehealth::duplication::CrossFileClonePair>,
        > = HashMap::new();
        for c in &analysis.clones {
            per_file.entry(c.file_a.clone()).or_default().push(c);
            per_file.entry(c.file_b.clone()).or_default().push(c);
        }
        let mut dry_lines = Vec::new();
        let mut smell_lines = Vec::new();
        for (path, file_clones) in &per_file {
            let dry_clones: Vec<refact_codehealth::dry::DryClonePair> = file_clones
                .iter()
                .map(|c| refact_codehealth::dry::DryClonePair {
                    file_a: c.file_a.clone(),
                    a_start_line: c.a_start_line,
                    a_line_count: c.a_end_line.saturating_sub(c.a_start_line) + 1,
                    file_b: c.file_b.clone(),
                    b_start_line: c.b_start_line,
                    b_line_count: c.b_end_line.saturating_sub(c.b_start_line) + 1,
                    co_change_count: co_change(&c.file_a, &c.file_b),
                })
                .collect();
            let file_dup_tokens = analysis
                .duplicated_tokens_by_path
                .get(path)
                .copied()
                .unwrap_or(0);
            let file_total = analysis
                .tokens_by_path
                .get(path)
                .copied()
                .unwrap_or(0)
                .max(1);
            let file_dup_pct = (file_dup_tokens as f64 / file_total as f64).min(1.0);
            let dry_input = refact_codehealth::dry::DryInput {
                file_path: path.clone(),
                duplication_pct: file_dup_pct,
                clones: dry_clones,
            };
            for f in refact_codehealth::dry::dry_violation(&dry_input) {
                dry_lines.push(DuplicationFindingResponse {
                    path: path.clone(),
                    biomarker: f.biomarker,
                    category: f.category,
                    dimension: f.dimension,
                    severity: f.severity,
                    line: f.line,
                    detail: f.detail,
                });
            }
            if refact_git_intel::paths::is_test_path(path) {
                if let Some((lang, text)) = analysis.text_by_path.get(path) {
                    let assertion_blocks =
                        refact_codehealth::assertions::assertion_blocks(lang, text);
                    let ts_clones: Vec<refact_codehealth::test_smells::ClonePair> = file_clones
                        .iter()
                        .map(|c| refact_codehealth::test_smells::ClonePair {
                            file_a: c.file_a.clone(),
                            a_start_line: c.a_start_line,
                            a_end_line: c.a_end_line,
                            file_b: c.file_b.clone(),
                            b_start_line: c.b_start_line,
                            b_end_line: c.b_end_line,
                        })
                        .collect();
                    let ts_input = refact_codehealth::test_smells::TestSmellInput {
                        file_path: path.clone(),
                        is_test_file: true,
                        assertion_blocks,
                        clones: ts_clones,
                    };
                    for f in refact_codehealth::test_smells::test_smell_biomarkers(&ts_input) {
                        smell_lines.push(DuplicationFindingResponse {
                            path: path.clone(),
                            biomarker: f.biomarker,
                            category: f.category,
                            dimension: f.dimension,
                            severity: f.severity,
                            line: f.line,
                            detail: f.detail,
                        });
                    }
                }
            }
        }
        dry_lines.truncate(15);
        smell_lines.truncate(15);
        let clones = analysis
            .clones
            .iter()
            .take(15)
            .map(|clone| DuplicationCloneResponse {
                path_a: clone.file_a.clone(),
                path_b: clone.file_b.clone(),
                line_a: clone.line_a,
                line_b: clone.line_b,
                a_start_line: clone.a_start_line,
                a_end_line: clone.a_end_line,
                b_start_line: clone.b_start_line,
                b_end_line: clone.b_end_line,
                lines: clone.a_end_line.saturating_sub(clone.a_start_line) + 1,
                token_len: clone.token_len,
                co_change: co_change(&clone.file_a, &clone.file_b),
            })
            .collect();
        let data = DuplicationResponse {
            aggregate: DuplicationAggregateResponse {
                file_count: analysis.files,
                clone_pair_count: analysis.clones.len(),
                duplication_pct: analysis.duplication_pct,
                duplication_percent: dup_pct,
            },
            clones,
            dry_violations: dry_lines,
            test_smells: smell_lines,
        };
        let summary = format!(
            "Cross-file duplication: {:.1}% of tokens, {} clone pairs",
            dup_pct,
            analysis.clones.len()
        );
        Ok((
            false,
            tool_message(
                tool_call_id,
                ToolJson::new("code_duplication", summary, data).to_text(),
            ),
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "code_duplication".to_string(),
            display_name: "Code Duplication".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Project-wide cross-file duplication from a graph-generation cache: token clone pairs and duplication percentage, git co-change counts joined to clone paths, co-change-weighted DRY findings, and assertion/clone test smells.".to_string(),
            input_schema: json_schema_from_params(&[], &[]),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        codegraph_dependency()
    }
}
