#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionKey {
    pub name: String,
    pub line1: usize,
    pub line2: usize,
}
#[derive(Default)]
pub struct HealthGraphContext {
    pub fan_in: HashMap<FunctionKey, usize>,
    pub hot_path: HashSet<FunctionKey>,
}
pub struct HealthGraphSnapshot {
    pub cached: Arc<refact_codegraph::CachedGraphAnalytics>,
    pub service: Arc<refact_codegraph::CodeGraphService>,
    pub service_identity: usize,
    pub graph_generation: u64,
    pub scope_signature: String,
    pub node_indices_by_basename: HashMap<String, Vec<usize>>,
    pub call_sources_by_dst: HashMap<i64, Vec<i64>>,
    pub hot_node_ids: BTreeSet<i64>,
    pub import_index: Option<HealthImportIndex>,
}
impl HealthGraphSnapshot {
    pub fn build(
        cached: Arc<refact_codegraph::CachedGraphAnalytics>,
        service: Arc<refact_codegraph::CodeGraphService>,
        indexed_root: Option<&Path>,
        analytics_scope_root: Option<&Path>,
    ) -> Self {
        let mut node_indices_by_basename: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, (_id, _name, path)) in cached.data.nodes.iter().enumerate() {
            node_indices_by_basename
                .entry(graph_path_basename(path))
                .or_default()
                .push(index);
        }

        let mut call_sources_by_dst: HashMap<i64, Vec<i64>> = HashMap::new();
        for (src, dst, kind) in &cached.data.edges {
            if kind == "calls" {
                call_sources_by_dst.entry(*dst).or_default().push(*src);
            }
        }
        for sources in call_sources_by_dst.values_mut() {
            sources.sort_unstable();
            sources.dedup();
        }

        let hot_node_ids = hot_path_node_ids(&cached.data, 3);
        let import_index = indexed_root.map(|root| HealthImportIndex::build(&cached, root));
        let graph_generation = cached.generation;
        let service_identity = Arc::as_ptr(&service) as usize;
        let scope_signature = health_graph_scope_signature(indexed_root, analytics_scope_root);

        Self {
            cached,
            service,
            service_identity,
            graph_generation,
            scope_signature,
            node_indices_by_basename,
            call_sources_by_dst,
            hot_node_ids,
            import_index,
        }
    }

    pub fn file_nodes(&self, file_path: &str) -> Vec<&refact_codegraph::analytics::GraphNode> {
        self.node_indices_by_basename
            .get(&graph_path_basename(file_path))
            .map(|indices| {
                indices
                    .iter()
                    .map(|index| &self.cached.data.nodes[*index])
                    .filter(|(_, _, path)| graph_path_matches(path, file_path))
                    .collect()
            })
            .unwrap_or_default()
    }
}
pub fn graph_path_basename(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit('/')
        .next()
        .unwrap_or(normalized.as_str())
        .to_string()
}

pub async fn health_graph_snapshot(
    service: Option<&Arc<refact_codegraph::CodeGraphService>>,
    indexed_root: Option<&Path>,
) -> Option<HealthGraphSnapshot> {
    health_graph_snapshot_with_scope(service, indexed_root, None).await
}

pub async fn health_graph_snapshot_with_scope(
    service: Option<&Arc<refact_codegraph::CodeGraphService>>,
    indexed_root: Option<&Path>,
    analytics_scope_root: Option<&Path>,
) -> Option<HealthGraphSnapshot> {
    let service = service?;
    for _ in 0..HEALTH_GRAPH_SNAPSHOT_MAX_ATTEMPTS {
        let full = service.cached_graph_analytics().await.ok()?;
        let cached = match analytics_scope_root {
            Some(root) => {
                let paths = scoped_graph_paths(&full.data, root);
                let scoped = service.scoped_graph_analytics(&paths).await.ok()?;
                if scoped.generation != full.generation {
                    continue;
                }
                scoped
            }
            None => full,
        };
        if cached.generation != service.graph_generation() {
            continue;
        }
        return Some(HealthGraphSnapshot::build(
            cached,
            service.clone(),
            indexed_root,
            analytics_scope_root,
        ));
    }
    None
}

fn health_graph_scope_signature(
    indexed_root: Option<&Path>,
    analytics_scope_root: Option<&Path>,
) -> String {
    let indexed = indexed_root
        .map(|root| root.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| "<none>".to_string());
    match analytics_scope_root {
        None => format!("unscoped:indexed={indexed}"),
        Some(root) => format!(
            "scoped:{}:indexed={indexed}",
            root.to_string_lossy().replace('\\', "/")
        ),
    }
}

pub fn health_call_graph(
    snapshot: &HealthGraphSnapshot,
    file_path: &str,
    indexed_root: Option<&Path>,
) -> Vec<CallGraphEdge> {
    let cached = &snapshot.cached;
    let graph = refact_codewiki::graph_intelligence::CodeGraph {
        nodes: cached
            .data
            .nodes
            .iter()
            .map(
                |(id, _name, path)| refact_codewiki::graph_intelligence::GraphNode {
                    id: id.to_string(),
                    node_type: "symbol".to_string(),
                    file_path: path.clone(),
                },
            )
            .collect(),
        edges: cached
            .data
            .edges
            .iter()
            .map(
                |(src, dst, kind)| refact_codewiki::graph_intelligence::GraphEdge {
                    src: src.to_string(),
                    dst: dst.to_string(),
                    edge_type: kind.clone(),
                },
            )
            .collect(),
    };
    let Some(stored_path) = health_stored_path_for_request(
        cached.data.nodes.iter().map(|(_, _, path)| path.clone()),
        file_path,
        indexed_root,
    ) else {
        return Vec::new();
    };
    refact_codewiki::graph_intelligence::extract_call_graph(&stored_path, &graph)
        .into_iter()
        .take(12)
        .map(|call| CallGraphEdge {
            caller: call.caller,
            callee: call.callee,
        })
        .collect()
}

pub fn enrich_health_findings(
    snapshot: Option<&HealthGraphSnapshot>,
    file_path: &str,
    functions: &[refact_codehealth::FunctionHealth],
    findings: Vec<refact_codehealth::biomarkers::Finding>,
) -> Vec<HealthFinding> {
    let context = snapshot.map(|snapshot| health_graph_context(snapshot, file_path, functions));
    findings
        .into_iter()
        .map(|finding| enrich_health_finding(finding, functions, context.as_ref()))
        .collect()
}

fn enrich_health_finding(
    mut finding: refact_codehealth::biomarkers::Finding,
    functions: &[refact_codehealth::FunctionHealth],
    context: Option<&HealthGraphContext>,
) -> HealthFinding {
    let mut hot_path = false;
    if let (Some(context), Some(function)) = (context, containing_function(functions, finding.line))
    {
        let key = function_key(function);
        if finding.biomarker == "brain_method" {
            if let Some(fan_in) = context.fan_in.get(&key).copied() {
                if fan_in == 0 {
                    finding.severity = downgrade_health_severity(finding.severity);
                    finding.detail.push_str(" (low fan-in)");
                } else if fan_in >= 5 {
                    finding
                        .detail
                        .push_str(&format!(" (called from {fan_in} sites)"));
                }
            }
        }
        if finding.dimension == refact_codehealth::biomarkers::Dimension::Performance {
            hot_path = context.hot_path.contains(&key);
        }
    }
    HealthFinding { finding, hot_path }
}

pub fn health_graph_context(
    snapshot: &HealthGraphSnapshot,
    file_path: &str,
    functions: &[refact_codehealth::FunctionHealth],
) -> HealthGraphContext {
    let file_nodes = snapshot.file_nodes(file_path);
    let mut node_ids_by_key = HashMap::<FunctionKey, BTreeSet<i64>>::new();
    for function in functions {
        let key = function_key(function);
        for (id, name, _path) in &file_nodes {
            if graph_name_matches(name, &function.name) {
                node_ids_by_key.entry(key.clone()).or_default().insert(*id);
            }
        }
    }

    let mut fan_in = HashMap::new();
    let mut hot_path = HashSet::new();
    for (key, ids) in &node_ids_by_key {
        let mut sources = BTreeSet::new();
        for id in ids {
            if let Some(node_sources) = snapshot.call_sources_by_dst.get(id) {
                sources.extend(node_sources.iter().copied());
            }
        }
        fan_in.insert(key.clone(), sources.len());
        if ids.iter().any(|id| snapshot.hot_node_ids.contains(id)) {
            hot_path.insert(key.clone());
        }
    }

    HealthGraphContext { fan_in, hot_path }
}

fn hot_path_node_ids(
    graph: &refact_codegraph::analytics::GraphData,
    max_depth: usize,
) -> BTreeSet<i64> {
    let mut out = HashMap::<i64, Vec<i64>>::new();
    let mut roots = BTreeSet::new();
    for (src, dst, kind) in &graph.edges {
        if !matches!(kind.as_str(), "calls" | "route_handler") {
            continue;
        }
        out.entry(*src).or_default().push(*dst);
        if kind == "route_handler" {
            roots.insert(*src);
        }
    }
    for (id, name, _) in &graph.nodes {
        if graph_entry_name(name) {
            roots.insert(*id);
        }
    }

    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    for root in roots {
        if seen.insert(root) {
            queue.push_back((root, 0));
        }
    }
    while let Some((id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(targets) = out.get(&id) {
            for target in targets {
                if seen.insert(*target) {
                    queue.push_back((*target, depth + 1));
                }
            }
        }
    }
    seen
}

fn containing_function(
    functions: &[refact_codehealth::FunctionHealth],
    line: usize,
) -> Option<&refact_codehealth::FunctionHealth> {
    functions
        .iter()
        .filter(|function| {
            let line2 = function_line2(function);
            function.line1 <= line && line <= line2
        })
        .max_by_key(|function| function.line1)
}

pub fn function_key(function: &refact_codehealth::FunctionHealth) -> FunctionKey {
    FunctionKey {
        name: function.name.clone(),
        line1: function.line1,
        line2: function_line2(function),
    }
}

pub fn function_line2(function: &refact_codehealth::FunctionHealth) -> usize {
    function
        .line1
        .saturating_add(function.loc as usize)
        .saturating_sub(1)
        .max(function.line1)
}

pub fn downgrade_health_severity(
    severity: refact_codehealth::biomarkers::Severity,
) -> refact_codehealth::biomarkers::Severity {
    match severity {
        refact_codehealth::biomarkers::Severity::Critical => {
            refact_codehealth::biomarkers::Severity::High
        }
        refact_codehealth::biomarkers::Severity::High => {
            refact_codehealth::biomarkers::Severity::Medium
        }
        refact_codehealth::biomarkers::Severity::Medium => {
            refact_codehealth::biomarkers::Severity::Low
        }
        refact_codehealth::biomarkers::Severity::Low => {
            refact_codehealth::biomarkers::Severity::Low
        }
    }
}

fn graph_path_matches(graph_path: &str, requested_path: &str) -> bool {
    // Exact or suffix path matches only. A bare-basename fallback is deliberately
    // NOT allowed here: enrichment (fan-in gates, hot-path flags) must never
    // borrow signals from a same-named file in another directory.
    let graph_path = graph_path.replace('\\', "/");
    let requested_path = requested_path.replace('\\', "/");
    graph_path == requested_path
        || graph_path.ends_with(&format!("/{requested_path}"))
        || requested_path.ends_with(&format!("/{graph_path}"))
}

fn graph_name_matches(graph_name: &str, function_name: &str) -> bool {
    graph_name == function_name
        || graph_name.ends_with(&format!("::{function_name}"))
        || graph_name.ends_with(&format!(".{function_name}"))
}

fn graph_entry_name(name: &str) -> bool {
    name == "main" || name.ends_with("::main") || name.ends_with(".main")
}
