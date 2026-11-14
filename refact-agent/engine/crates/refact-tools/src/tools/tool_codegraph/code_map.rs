#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeMapNode {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub visibility: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CodeMapFile {
    pub parsed: refact_codewiki::ParsedFile,
    pub text: String,
    pub is_hotspot: bool,
    pub kg_bonus: f64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeMapLink {
    pub target_path: String,
    pub labels: Vec<String>,
    pub count: usize,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CodeMapRenderedPage {
    pub page: refact_codewiki::AllocatedPage,
    pub content: String,
}
#[derive(Serialize)]
pub struct CodeMapResponse {
    pub files_count: usize,
    pub page_count: usize,
    pub link_count: usize,
    pub query: Option<String>,
    pub index_state: Option<PrBlastIndexState>,
    pub partial: bool,
    pub warning: Option<String>,
    pub top_files: Vec<CodeMapFileScore>,
    pub backlink_hubs: Vec<CodeMapBacklinkHub>,
    pub pages: Vec<CodeMapPage>,
    pub markdown: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeMapFormat {
    Markdown,
    ClaudeMd,
}

impl CodeMapFormat {
    pub fn parse(value: Option<&Value>) -> Result<Self, String> {
        match value {
            None | Some(Value::Null) => Ok(Self::Markdown),
            Some(Value::String(value)) if value == "markdown" => Ok(Self::Markdown),
            Some(Value::String(value)) if value == "claude_md" => Ok(Self::ClaudeMd),
            Some(Value::String(value)) => Err(format!(
                "argument `format` must be `markdown` or `claude_md`, got `{value}`"
            )),
            Some(value) => Err(format!(
                "argument `format` must be `markdown` or `claude_md`: {value:?}"
            )),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeMapArgs {
    pub query: Option<String>,
    pub format: CodeMapFormat,
    pub budget_tokens: usize,
}
fn code_map_args(args: &HashMap<String, Value>) -> Result<CodeMapArgs, String> {
    Ok(CodeMapArgs {
        query: optional_string_arg(args, "query")?,
        format: CodeMapFormat::parse(args.get("format"))?,
        budget_tokens: optional_usize_arg(args, "budget", CODE_MAP_DEFAULT_BUDGET_TOKENS)?.max(1),
    })
}

fn code_map_node_kind(kind: &str) -> String {
    match kind {
        "typealias" => "type_alias".to_string(),
        "route" => "route".to_string(),
        value => value.to_string(),
    }
}

fn code_map_node_visibility(data: Option<&str>) -> Option<String> {
    let data = data?;
    let value = serde_json::from_str::<Value>(data).ok()?;
    let visibility = value.get("visibility")?.as_str()?.trim();
    (!visibility.is_empty()).then(|| visibility.to_string())
}

pub fn code_map_nodes_from_records(
    records: Vec<(i64, String, String, String, Option<String>)>,
) -> Vec<CodeMapNode> {
    let mut nodes = records
        .into_iter()
        .map(|(id, kind, name, path, data)| CodeMapNode {
            id,
            kind: code_map_node_kind(&kind),
            name,
            path,
            visibility: code_map_node_visibility(data.as_deref()),
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    nodes
}

pub fn code_map_symbols_by_path(
    nodes: &[CodeMapNode],
) -> HashMap<String, Vec<refact_codewiki::SymbolInfo>> {
    let mut symbols = HashMap::<String, Vec<refact_codewiki::SymbolInfo>>::new();
    for node in nodes {
        if node.kind == "route" || node.kind == "unknown" {
            continue;
        }
        symbols
            .entry(node.path.clone())
            .or_default()
            .push(refact_codewiki::SymbolInfo {
                kind: node.kind.clone(),
                visibility: node.visibility.clone().unwrap_or_default(),
            });
    }
    for values in symbols.values_mut() {
        values.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.visibility.cmp(&right.visibility))
        });
    }
    symbols
}

fn normalize_file_scores(values: &HashMap<String, f64>) -> HashMap<String, f64> {
    let max = values
        .values()
        .copied()
        .filter(|value| value.is_finite())
        .fold(0.0_f64, f64::max);
    values
        .iter()
        .map(|(path, score)| {
            let normalized = if max > 0.0 && score.is_finite() {
                (score / max).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (path.clone(), normalized)
        })
        .collect()
}

fn code_map_is_hotspot(
    path: &str,
    intel: Option<&refact_git_intel::GitIntel>,
    repo_root: Option<&Path>,
) -> bool {
    let Some(intel) = intel else {
        return false;
    };
    let key = match repo_root {
        Some(root) => {
            refact_git_intel::paths::repo_relative_or_basename(path, &root.to_string_lossy())
        }
        None => refact_git_intel::paths::normalize_separators(path),
    };
    intel.churn_percentile(&key) >= 0.9
}

fn code_map_infra_path(path: &str) -> bool {
    let normalized = refact_git_intel::paths::normalize_separators(path).to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    matches!(
        name,
        "dockerfile"
            | "docker-compose.yml"
            | "docker-compose.yaml"
            | "makefile"
            | "gnumakefile"
            | "package.json"
            | "package-lock.json"
            | "cargo.toml"
            | "cargo.lock"
            | "pyproject.toml"
            | "requirements.txt"
            | "build.gradle"
            | "build.gradle.kts"
            | "settings.gradle"
            | "settings.gradle.kts"
            | "pom.xml"
    ) || normalized.starts_with(".github/workflows/")
        || normalized.contains("/.github/workflows/")
}

fn top_level_module(path: &str) -> Option<String> {
    path.split('/')
        .find(|segment| !segment.is_empty())
        .map(str::to_string)
}

fn second_level_module(path: &str) -> Option<String> {
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .take(2)
        .collect::<Vec<_>>();
    (segments.len() == 2).then(|| segments.join("/"))
}

fn module_selections(files: &[CodeMapFile]) -> Vec<refact_codewiki::ModuleSelection> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        let path = file.parsed.file_info.path.as_str();
        if let Some(module) = top_level_module(path) {
            grouped.entry(module).or_default().push(path.to_string());
        }
        if let Some(module) = second_level_module(path) {
            grouped.entry(module).or_default().push(path.to_string());
        }
    }
    grouped
        .into_iter()
        .filter_map(|(id, mut paths)| {
            paths.sort();
            paths.dedup();
            if paths.len() < CODE_MAP_MIN_MODULE_FILES {
                return None;
            }
            Some(refact_codewiki::ModuleSelection {
                id,
                size: paths.len() as u64,
                cohesion: 0.5,
                paths,
            })
        })
        .collect()
}

fn scc_path_groups(
    nodes: &[CodeMapNode],
    edges: &[refact_codegraph::analytics::GraphEdge],
) -> Vec<Vec<String>> {
    let mut graph = DiGraph::<i64, ()>::new();
    let mut index_by_id = HashMap::<i64, NodeIndex>::new();
    for node in nodes {
        index_by_id
            .entry(node.id)
            .or_insert_with(|| graph.add_node(node.id));
    }
    for (src, dst, kind) in edges {
        if !CODE_MAP_LINK_EDGE_KINDS.contains(&kind.as_str()) {
            continue;
        }
        let (Some(src_idx), Some(dst_idx)) = (index_by_id.get(src), index_by_id.get(dst)) else {
            continue;
        };
        graph.add_edge(*src_idx, *dst_idx, ());
    }
    let path_by_id: HashMap<i64, &str> = nodes
        .iter()
        .map(|node| (node.id, node.path.as_str()))
        .collect();
    let mut out = Vec::new();
    for component in tarjan_scc(&graph) {
        if component.len() <= 1 {
            continue;
        }
        let mut paths = component
            .into_iter()
            .filter_map(|idx| path_by_id.get(&graph[idx]).map(|path| (*path).to_string()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if paths.len() <= 1 {
            continue;
        }
        paths.sort();
        out.push(paths);
    }
    out.sort();
    out
}

pub fn build_code_map_files(
    files_text: &[(String, String)],
    nodes: &[CodeMapNode],
    centrality: &refact_codegraph::analytics::FileCentrality,
    intel: Option<&refact_git_intel::GitIntel>,
    repo_root: Option<&Path>,
) -> Vec<CodeMapFile> {
    let pr: HashMap<String, f64> = centrality.top_pagerank.iter().cloned().collect();
    let normalized_pr = normalize_file_scores(&pr);
    let symbols_by_path = code_map_symbols_by_path(nodes);
    let mut out = files_text
        .iter()
        .map(|(path, text)| {
            let parsed = refact_codewiki::ParsedFile {
                file_info: refact_codewiki::FileInfo {
                    path: path.clone(),
                    is_entry_point: refact_codewiki::entry_points::is_conventional_entry(path),
                    is_test: refact_git_intel::paths::is_test_path(path),
                    size_bytes: text.len() as u64,
                },
                symbols: symbols_by_path.get(path).cloned().unwrap_or_default(),
            };
            CodeMapFile {
                parsed,
                text: text.clone(),
                is_hotspot: code_map_is_hotspot(path, intel, repo_root),
                kg_bonus: normalized_pr.get(path).copied().unwrap_or(0.0),
            }
        })
        .collect::<Vec<_>>();
    out.sort_by(|left, right| left.parsed.file_info.path.cmp(&right.parsed.file_info.path));
    out
}

pub fn code_map_selection_input(
    files: Vec<CodeMapFile>,
    nodes: &[CodeMapNode],
    edges: &[refact_codegraph::analytics::GraphEdge],
    centrality: &refact_codegraph::analytics::FileCentrality,
) -> refact_codewiki::SelectionInput {
    let pr: HashMap<String, f64> = centrality.top_pagerank.iter().cloned().collect();
    let bt: HashMap<String, f64> = centrality.top_betweenness.iter().cloned().collect();
    let api_contract_paths = files
        .iter()
        .filter_map(|file| {
            let path = file.parsed.file_info.path.as_str();
            let lang = refact_codegraph::lang_from_path(path);
            let parsed = refact_codewiki::parsed_file::build_parsed_file(lang, &file.text);
            refact_codewiki::api_contract::detect_api_contract(&parsed).then(|| path.to_string())
        })
        .collect();
    let infra_paths = files
        .iter()
        .filter_map(|file| {
            code_map_infra_path(&file.parsed.file_info.path)
                .then(|| file.parsed.file_info.path.clone())
        })
        .collect();
    let modules = module_selections(&files);
    let selections = files
        .into_iter()
        .map(|file| {
            let path = file.parsed.file_info.path.clone();
            refact_codewiki::FileSelection {
                parsed: file.parsed,
                pagerank: pr.get(&path).copied().unwrap_or(0.0),
                betweenness: bt.get(&path).copied().unwrap_or(0.0),
                is_hotspot: file.is_hotspot,
                kg_bonus: file.kg_bonus,
            }
        })
        .collect();
    refact_codewiki::SelectionInput {
        files: selections,
        modules,
        sccs: scc_path_groups(nodes, edges),
        api_contract_paths,
        infra_paths,
        min_module_size: CODE_MAP_MIN_MODULE_SIZE,
    }
}

fn page_title(page: &refact_codewiki::PageCandidate) -> String {
    page.id
        .split_once(':')
        .map(|(_, title)| title)
        .unwrap_or(page.id.as_str())
        .to_string()
}

fn page_kind_label(kind: refact_codewiki::PageKind) -> &'static str {
    match kind {
        refact_codewiki::PageKind::File => "file",
        refact_codewiki::PageKind::Module => "module",
        refact_codewiki::PageKind::Scc => "scc",
        refact_codewiki::PageKind::ApiContract => "api-contract",
        refact_codewiki::PageKind::Infra => "infra",
    }
}

fn page_paths(page: &refact_codewiki::PageCandidate) -> BTreeSet<String> {
    page.paths.iter().cloned().collect()
}

fn code_map_links_for_page(
    page: &refact_codewiki::PageCandidate,
    nodes: &[CodeMapNode],
    edges: &[refact_codegraph::analytics::GraphEdge],
) -> Vec<CodeMapLink> {
    let page_paths = page_paths(page);
    let path_by_id: HashMap<i64, &str> = nodes
        .iter()
        .map(|node| (node.id, node.path.as_str()))
        .collect();
    let mut links = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for (src, dst, kind) in edges {
        if !CODE_MAP_LINK_EDGE_KINDS.contains(&kind.as_str()) {
            continue;
        }
        let (Some(src_path), Some(dst_path)) = (path_by_id.get(src), path_by_id.get(dst)) else {
            continue;
        };
        if src_path == dst_path {
            continue;
        }
        let target = if page_paths.contains(*src_path) && !page_paths.contains(*dst_path) {
            Some(*dst_path)
        } else if page_paths.contains(*dst_path) && !page_paths.contains(*src_path) {
            Some(*src_path)
        } else {
            None
        };
        if let Some(target) = target {
            *links
                .entry(target.to_string())
                .or_default()
                .entry(kind.clone())
                .or_default() += 1;
        }
    }
    let mut out = links
        .into_iter()
        .map(|(target_path, labels)| {
            let count = labels.values().sum();
            CodeMapLink {
                target_path,
                labels: labels.into_keys().collect(),
                count,
            }
        })
        .collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.target_path.cmp(&right.target_path))
    });
    out.truncate(CODE_MAP_LINK_LIMIT);
    out
}

fn symbol_kind_counts(
    page: &refact_codewiki::PageCandidate,
    nodes: &[CodeMapNode],
) -> Vec<(String, usize)> {
    let page_paths = page_paths(page);
    let mut counts = BTreeMap::<String, usize>::new();
    for node in nodes {
        if !page_paths.contains(&node.path) || node.kind == "route" || node.kind == "unknown" {
            continue;
        }
        *counts.entry(node.kind.clone()).or_default() += 1;
    }
    counts.into_iter().collect()
}

fn symbol_visibility_counts(
    page: &refact_codewiki::PageCandidate,
    nodes: &[CodeMapNode],
) -> Vec<(String, usize)> {
    let page_paths = page_paths(page);
    let mut counts = BTreeMap::<String, usize>::new();
    for node in nodes {
        if !page_paths.contains(&node.path) {
            continue;
        }
        if let Some(visibility) = &node.visibility {
            *counts.entry(visibility.clone()).or_default() += 1;
        }
    }
    counts.into_iter().collect()
}

fn trim_to_token_allowance(mut content: String, token_allowance: usize) -> String {
    if refact_codewiki::token_budget::estimate_tokens(&content) <= token_allowance {
        return content;
    }
    if token_allowance == 0 {
        return String::new();
    }
    let suffix = "...";
    let suffix_tokens = refact_codewiki::token_budget::estimate_tokens(suffix).max(1);
    if token_allowance <= suffix_tokens {
        return suffix.to_string();
    }
    let max_chars = token_allowance
        .saturating_sub(suffix_tokens)
        .saturating_mul(4);
    if content.chars().count() > max_chars {
        content = content.chars().take(max_chars).collect();
    }
    let target_tokens = token_allowance.saturating_sub(suffix_tokens);
    while refact_codewiki::token_budget::estimate_tokens(&content) > target_tokens
        && !content.is_empty()
    {
        content.pop();
    }
    content.push_str(suffix);
    content
}

fn page_signal_labels(
    page: &refact_codewiki::PageCandidate,
    input: &refact_codewiki::SelectionInput,
) -> Vec<String> {
    let page_paths = page_paths(page);
    let mut labels = BTreeSet::new();
    for file in &input.files {
        if !page_paths.contains(&file.parsed.file_info.path) {
            continue;
        }
        if file.parsed.file_info.is_entry_point {
            labels.insert("entry_point".to_string());
        }
        if file.parsed.file_info.is_test {
            labels.insert("test".to_string());
        }
        if file.is_hotspot {
            labels.insert("hotspot".to_string());
        }
        if file.kg_bonus > 0.0 {
            labels.insert(format!("kg_prior={:.2}", file.kg_bonus));
        }
    }
    labels.into_iter().collect()
}

fn render_code_map_page_markdown(
    allocation: &refact_codewiki::AllocatedPage,
    input: &refact_codewiki::SelectionInput,
    nodes: &[CodeMapNode],
    edges: &[refact_codegraph::analytics::GraphEdge],
) -> String {
    let page = &allocation.page;
    let mut content = String::new();
    content.push_str(&format!(
        "### {} — {}\n",
        page_title(page),
        page_kind_label(page.kind)
    ));
    content.push_str(&format!("Score: {:.3}\n", page.score));
    content.push_str(&format!("Paths: {}\n", page.paths.join(", ")));
    let signals = page_signal_labels(page, input);
    if !signals.is_empty() {
        content.push_str(&format!("Signals: {}\n", signals.join(", ")));
    }
    let symbol_counts = symbol_kind_counts(page, nodes);
    if !symbol_counts.is_empty() {
        content.push_str("Symbols:");
        for (kind, count) in symbol_counts {
            content.push_str(&format!(" {}={}", kind, count));
        }
        content.push('\n');
    }
    let visibility_counts = symbol_visibility_counts(page, nodes);
    if !visibility_counts.is_empty() {
        content.push_str("Visibility:");
        for (visibility, count) in visibility_counts {
            content.push_str(&format!(" {}={}", visibility, count));
        }
        content.push('\n');
    }
    let links = code_map_links_for_page(page, nodes, edges);
    if !links.is_empty() {
        content.push_str("Links:\n");
        for link in links {
            content.push_str(&format!(
                "- {} ({}, {} edges)\n",
                link.target_path,
                link.labels.join("/"),
                link.count
            ));
        }
    }
    trim_to_token_allowance(content, allocation.token_allowance)
}

fn code_map_backlink_hubs(
    rendered_pages: &[CodeMapRenderedPage],
    nodes: &[CodeMapNode],
    edges: &[refact_codegraph::analytics::GraphEdge],
) -> Vec<(String, usize)> {
    let mut counts = BTreeMap::<String, usize>::new();
    for rendered in rendered_pages {
        for link in code_map_links_for_page(&rendered.page.page, nodes, edges) {
            *counts.entry(link.target_path).or_default() += link.count;
        }
    }
    let mut hubs = counts.into_iter().collect::<Vec<_>>();
    hubs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    hubs
}

fn code_map_page_prior(pages: &[refact_codewiki::PageCandidate]) -> HashMap<String, f64> {
    let max_score = pages
        .iter()
        .map(|page| page.score)
        .filter(|score| score.is_finite())
        .fold(0.0_f64, f64::max);
    pages
        .iter()
        .map(|page| {
            let prior = if max_score > 0.0 && page.score.is_finite() {
                page.score / max_score
            } else {
                0.0
            };
            (page.id.clone(), prior.clamp(0.0, 1.0))
        })
        .collect()
}

fn choose_code_map_pages(
    mut pages: Vec<refact_codewiki::PageCandidate>,
    query: Option<&str>,
) -> Vec<refact_codewiki::PageCandidate> {
    if let Some(query) = query {
        let prior = code_map_page_prior(&pages);
        let mut hits = refact_codewiki::search_hybrid(query, &pages, &prior);
        if hits.is_empty() {
            pages.sort_by(|left, right| {
                right
                    .score
                    .total_cmp(&left.score)
                    .then_with(|| left.id.cmp(&right.id))
            });
            pages.truncate(CODE_MAP_QUERY_LIMIT);
            return pages;
        }
        return hits
            .drain(..)
            .into_iter()
            .take(CODE_MAP_QUERY_LIMIT)
            .map(|hit| hit.page.clone())
            .collect();
    }
    pages.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.id.cmp(&right.id))
    });
    pages.truncate(CODE_MAP_MAX_PAGES_WITHOUT_QUERY);
    pages
}

fn render_code_map_claude_md(
    rendered_pages: &[CodeMapRenderedPage],
    index_state: Option<&PrBlastIndexState>,
) -> String {
    let pages = rendered_pages
        .iter()
        .map(|rendered| rendered.page.page.clone())
        .collect::<Vec<_>>();
    let mut out = String::new();
    if let Some(state) = index_state {
        if let Some(warning) = pr_blast_partial_warning(state) {
            out.push_str(&warning);
            out.push('\n');
        }
    }
    out.push_str(&refact_codewiki::render_claude_md(&pages));
    out
}

pub fn assemble_code_map_response(
    files_text: Vec<(String, String)>,
    node_records: Vec<(i64, String, String, String, Option<String>)>,
    edges: Vec<refact_codegraph::analytics::GraphEdge>,
    centrality: refact_codegraph::analytics::FileCentrality,
    index_state: Option<PrBlastIndexState>,
    intel: Option<&refact_git_intel::GitIntel>,
    repo_root: Option<&Path>,
    args: &CodeMapArgs,
) -> CodeMapResponse {
    if files_text.is_empty() {
        let warning = index_state.as_ref().and_then(pr_blast_partial_warning);
        return CodeMapResponse {
            files_count: 0,
            page_count: 0,
            link_count: 0,
            query: args.query.clone(),
            partial: index_state
                .as_ref()
                .is_some_and(|state| !state.cross_file_ready),
            index_state,
            warning,
            top_files: Vec::new(),
            backlink_hubs: Vec::new(),
            pages: Vec::new(),
            markdown: None,
        };
    }
    let nodes = code_map_nodes_from_records(node_records);
    let files = build_code_map_files(&files_text, &nodes, &centrality, intel, repo_root);
    let input = code_map_selection_input(files, &nodes, &edges, &centrality);
    let selected_pages =
        choose_code_map_pages(refact_codewiki::select_pages(&input), args.query.as_deref());
    let allocations = refact_codewiki::allocate(&selected_pages, args.budget_tokens);
    let rendered_pages = allocations
        .into_iter()
        .map(|page| {
            let content = render_code_map_page_markdown(&page, &input, &nodes, &edges);
            CodeMapRenderedPage { page, content }
        })
        .collect::<Vec<_>>();
    let link_count = rendered_pages
        .iter()
        .map(|page| code_map_links_for_page(&page.page.page, &nodes, &edges).len())
        .sum();
    let top_files = rendered_pages
        .iter()
        .filter(|rendered| rendered.page.page.kind == refact_codewiki::PageKind::File)
        .take(12)
        .map(|rendered| CodeMapFileScore {
            path: rendered
                .page
                .page
                .paths
                .first()
                .cloned()
                .unwrap_or_else(|| rendered.page.page.id.clone()),
            score: rendered.page.page.score,
        })
        .collect();
    let backlink_hubs = code_map_backlink_hubs(&rendered_pages, &nodes, &edges)
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .take(12)
        .map(|(path, count)| CodeMapBacklinkHub { path, count })
        .collect();
    let pages = rendered_pages
        .iter()
        .map(|rendered| {
            let page = &rendered.page.page;
            CodeMapPage {
                title: page_title(page),
                kind: page_kind_label(page.kind).to_string(),
                score: page.score,
                paths: page.paths.clone(),
                signals: page_signal_labels(page, &input),
                symbols: symbol_kind_counts(page, &nodes).into_iter().collect(),
                visibility: symbol_visibility_counts(page, &nodes).into_iter().collect(),
                links: code_map_links_for_page(page, &nodes, &edges)
                    .into_iter()
                    .map(|link| CodeMapPageLink {
                        target_path: link.target_path,
                        labels: link.labels,
                        count: link.count,
                    })
                    .collect(),
                content: rendered.content.clone(),
            }
        })
        .collect::<Vec<_>>();
    let markdown = matches!(args.format, CodeMapFormat::ClaudeMd).then(|| {
        trim_to_token_allowance(
            render_code_map_claude_md(&rendered_pages, index_state.as_ref()),
            args.budget_tokens,
        )
    });
    let warning = index_state.as_ref().and_then(pr_blast_partial_warning);
    CodeMapResponse {
        files_count: files_text.len(),
        page_count: pages.len(),
        link_count,
        query: args.query.clone(),
        partial: index_state
            .as_ref()
            .is_some_and(|state| !state.cross_file_ready),
        index_state,
        warning,
        top_files,
        backlink_hubs,
        pages,
        markdown,
    }
}

pub struct ToolCodeMap {
    pub config_path: String,
}
#[async_trait]
impl Tool for ToolCodeMap {
    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let args = code_map_args(args)?;
        let gcx = ccx.lock().await.app.gcx.clone();
        let service = gcx
            .codegraph
            .lock()
            .await
            .clone()
            .ok_or_else(|| "codegraph is not available".to_string())?;
        let files_text = service.all_files_with_text().await?;
        let cached = service.cached_graph_analytics().await?;
        let node_records = service.graph_node_records().await?;
        let readiness = service.index_readiness().await?;
        let roots = codegraph_project_roots(ccx.clone()).await?;
        let (files_text, node_records, edges, centrality) =
            match analytics_scope_root(roots.as_ref()) {
                Some(root) => {
                    let paths = scoped_graph_paths(&cached.data, root);
                    let scoped = service.scoped_graph_analytics(&paths).await?;
                    scoped_code_map_inputs(
                        root,
                        files_text,
                        node_records,
                        scoped.data.edges.clone(),
                        scoped.analytics.file_centrality.truncated(5000),
                    )
                }
                None => (
                    files_text,
                    node_records,
                    cached.data.edges.clone(),
                    cached.analytics.file_centrality.truncated(5000),
                ),
            };
        let intel = match roots.as_ref().map(|roots| roots.git_root.as_path()) {
            Some(dir) => cached_mine_history_async(dir, 1000).await.ok(),
            None => None,
        };
        let data = assemble_code_map_response(
            files_text,
            node_records,
            edges,
            centrality,
            Some(pr_blast_index_state(&readiness)),
            intel.as_ref(),
            roots.as_ref().map(|roots| roots.indexed_root.as_path()),
            &args,
        );
        let summary = if data.files_count == 0 {
            "Code graph has no indexed files (index empty/building)".to_string()
        } else {
            format!(
                "Code map: {} indexed files, {} pages, {} cross-file links",
                data.files_count, data.page_count, data.link_count
            )
        };
        Ok((
            false,
            tool_message(
                tool_call_id,
                ToolJson::new("code_map", summary, data).to_text(),
            ),
        ))
    }

    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "code_map".to_string(),
            display_name: "Code Map".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: true,
            description: "Documentation-worthy code map from CodeGraph and git signals: file centrality, churn hotspots, real symbol kinds and parsed visibility when present, file/module/SCC/API/infra pages, edge-derived links and backlink hubs, readiness warnings, optional hybrid query filtering, token budget trimming, and markdown or claude_md output.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional search query; when supplied, hybrid search returns only the most relevant selected pages."
                    },
                    "format": {
                        "type": "string",
                        "enum": ["markdown", "claude_md"],
                        "description": "Output format. Defaults to markdown."
                    },
                    "budget": {
                        "type": "integer",
                        "description": "Approximate token budget for selected page output. Defaults to 12000."
                    }
                }
            }),
            output_schema: None,
            annotations: None,
        }
    }

    fn tool_depends_on(&self) -> Vec<String> {
        codegraph_dependency()
    }
}
