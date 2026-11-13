"""N-5 generator: split `refact-agent/engine/src/tools/tool_codegraph.rs` (5 702 production
lines in one file) into `refact-agent/engine/src/tools/tool_codegraph/*.rs`.

Run BEFORE deleting the original file (the script does the deletion itself):

    python tools/dev/n5-split.py --write

Layout
------
mod.rs       crate imports (the original first 28 lines, verbatim) + `mod <child>;` +
             `#[allow(unused_imports)] pub(crate) use self::<child>::*;` per child +
             the original module-level plumbing (caches, path/scope resolution, arg
             helpers, `refact_code_intel_api` re-exports, consts) + the verbatim
             `#[cfg(test)] mod tests { ... }` block
tools.rs     the Tool impls with no dedicated family (codegraph_overview, security_scan)
health.rs    code_health + per-file health analysis + the analysis cache
graph.rs     health-graph snapshot + fan-in / hot-path enrichment
git_risk.rs  git mining, git meta, git risk assembly, git_risk tool
pr_blast.rs  PR blast radius
clones.rs    cross-file clone analysis + code_duplication
dead_code.rs dead-code report + git facts
code_map.rs  code_map tool + page selection/rendering
code_why.rs  code_why tool + decision extraction/scoring

Visibility (verified against rustc with throwaway probes)
--------------------------------------------------------
Rust privacy is "visible in the defining module and its descendants", and `mod.rs` is
the parent, so:

* a private item that stays in `mod.rs` needs no change - the children and the tests
  module reach it through their `use super::*;`;
* a private item owned by a *child* that is referenced from another child or from the
  tests module becomes `pub(super)`: visible in the parent and all its descendants
  (= exactly the old single-module scope) but not from the rest of the crate;
* struct fields / inherent methods of such a type are widened the same way;
* everything that was `pub` / `pub(crate)` keeps its visibility, and the `pub(crate)`
  glob per child re-exports exactly that tier, so `crate::tools::tool_codegraph::X`
  resolves for every name that resolved before - `pub(super)` items do not leak.

The transformation is additive and order-preserving: it only inserts visibility tokens
and the generated module-list/glob scaffolding, never rewrites or reorders an item.
`tools/dev/n5-check.py` proves that line by line against the original file.
"""
import argparse
import pathlib
import re
import subprocess
import sys
from collections import defaultdict

ROOT = pathlib.Path(__file__).resolve().parents[2]
SRC = ROOT / "refact-agent/engine/src/tools/tool_codegraph.rs"
OUT = ROOT / "refact-agent/engine/src/tools/tool_codegraph"
REL_SRC = "refact-agent/engine/src/tools/tool_codegraph.rs"

HEADER_LAST = 28

MODULES = ["tools", "health", "graph", "git_risk", "pr_blast", "clones",
           "dead_code", "code_map", "code_why"]

ITEM_RE = re.compile(
    r"^(?:(?P<vis>pub\s*\([a-z:]*\)|pub)\s+)?(?:(?P<async>async)\s+)?"
    r"(?P<kind>fn|struct|enum|const|static|type|mod|trait|union|impl|use)\b")
FIELD_RE = re.compile(r"^(?P<indent>[ \t]+)(?P<name>[a-z_][A-Za-z0-9_]*)\s*:", re.M)
FN_RE = re.compile(r"^(?P<indent>[ \t]+)(?:(?:pub\s*\([a-z:]*\)|pub)\s+)?(?:async\s+)?"
                   r"fn\s+(?P<name>[a-z_][A-Za-z0-9_]*)", re.M)

def source_text():
    """The single-file module: from disk, or from the last commit that had it."""
    if SRC.exists():
        return SRC.read_text(encoding="utf-8")
    revs = subprocess.run(["git", "-C", str(ROOT), "log", "--format=%H", "--", REL_SRC],
                          capture_output=True, text=True, encoding="utf-8").stdout.split()
    for rev in revs:
        result = subprocess.run(["git", "-C", str(ROOT), "show", "%s:%s" % (rev, REL_SRC)],
                                capture_output=True, text=True, encoding="utf-8")
        if result.returncode == 0 and result.stdout.strip():
            return result.stdout
    print("cannot find the original single-file module on disk or in git history")
    sys.exit(1)


LINES = source_text().splitlines()

# The test module is the last top-level item. Anchor on its declaration line instead of
# deriving it from the item scan: its body contains a raw string whose column-0 contents
# look like top-level items to a line scanner.
TESTS_DECL = LINES.index("mod tests {")
assert LINES[TESTS_DECL - 1] == "#[cfg(test)]", LINES[TESTS_DECL - 1]
ATTR_LINE = TESTS_DECL - 1


def parse_items():
    raw_starts = [i for i, line in enumerate(LINES) if ITEM_RE.match(line)]
    folded = []
    for s in raw_starts:
        start = s
        while start - 1 >= 0 and (LINES[start - 1].startswith("#[")
                                  or LINES[start - 1].startswith("#!")
                                  or LINES[start - 1].startswith("///")
                                  or LINES[start - 1].startswith("//")):
            start -= 1
        folded.append(start)
    items = []
    for idx, s in enumerate(raw_starts):
        start = folded[idx]
        end = folded[idx + 1] - 1 if idx + 1 < len(raw_starts) else len(LINES) - 1
        head = LINES[s]
        m = ITEM_RE.match(head)
        kind = m.group("kind")
        vis = (m.group("vis") or "").replace(" ", "")
        vis = "pub(crate)" if vis.startswith("pub(") else vis
        nm = re.match(r"^(?:pub\s*\([a-z:]*\)\s+|pub\s+)?(?:async\s+)?(?:unsafe\s+)?"
                      r"(?:fn|struct|enum|const|static|type|mod|trait|union)\s+([A-Za-z0-9_]+)", head)
        if kind == "use":
            name = "USE@%d" % (s + 1)
        elif nm:
            name = nm.group(1)
        else:
            im = re.match(r"^(?:pub\s+)?(?:unsafe\s+)?(?:async\s+)?impl(?:<[^>]*>)?\s+"
                          r"(?:([A-Za-z0-9_:<>]+)\s+for\s+)?([A-Za-z0-9_:<>]+)", head)
            ty = (im.group(2) if im else head).replace(" ", "")
            name = "impl:%s|%s" % (ty, "trait" if (im and im.group(1)) else "inherent")
        items.append({"start": start, "end": end, "head": head, "kind": kind, "name": name,
                      "vis": vis, "text": "\n".join(LINES[start:end + 1])})
    return items


ASSIGN = {}


def assign(module, *names):
    for n in names:
        if n in ASSIGN and ASSIGN[n] != module:
            raise SystemExit("conflicting assignment: %s (%s vs %s)" % (n, ASSIGN[n], module))
        ASSIGN[n] = module


def build_assignment(items):
    prod = [it for it in items if it["end"] < ATTR_LINE]
    tests_item = next(it for it in items if it["start"] == ATTR_LINE)

    for it in prod:
        if it["name"].startswith("USE@"):
            assign("mod", it["name"])

    # Shared plumbing: caches, path/scope resolution, arg helpers, git mining.
    assign("mod", "GitMiningCache", "git_cache", "BlameFactsCache", "blame_facts_cache",
           "blame_ranges_hash", "cached_blame_range_facts", "tool_message",
           "tool_message_with_records", "codegraph_dependency", "string_arg",
           "string_list_arg", "optional_usize_arg", "optional_f64_arg", "optional_string_arg",
           "ResolvedCodegraphFile", "CodegraphProjectRoots", "resolve_codegraph_file",
           "resolve_codegraph_filter_path", "scoped_indexed_path",
           "scoped_codegraph_project_roots", "codegraph_project_roots", "analytics_scope_root",
           "code_intel_relative_path", "code_intel_path_allowed", "scoped_graph_paths",
           "scoped_graph_data", "scoped_dead_symbols", "scoped_clone_analysis",
           "scoped_code_map_inputs", "project_dir", "git_head_oid", "git_head_unchanged",
           "cached_mine_history", "cached_mine_history_async", "cached_mine_history_with",
           "co_change_count", "co_change_key", "current_unix_ts", "GitCacheKey")

    assign("tools", "ToolCodegraphOverview", "impl:ToolCodegraphOverview|trait",
           "OverviewToolResponse", "execution_flow_truncation_notice", "ToolSecurityScan",
           "impl:ToolSecurityScan|trait", "severity_label")
    assign("health", "HealthFunctionSummary", "HealthFileAnalysis", "HealthAnalysisContext",
           "HealthFileCore", "HealthAnalysisCache", "HealthCacheKey", "HealthCacheEntry",
           "health_analysis_cache", "hash_string", "reset_health_cache_for_tests",
           "git_churn_by_file", "top_git_function_fact_paths", "load_health_snapshots",
           "persist_health_snapshots", "build_health_snapshot", "push_health_snapshot",
           "analyze_health_file_shared", "analyze_health_file_shared_for_path",
           "health_content_hash", "health_git_generation", "health_cache_lookup",
           "health_cache_store", "compute_health_core", "health_analysis_from_core",
           "health_dry_findings", "health_git_findings", "health_coverage_findings",
           "coverage_file_for_path", "coverage_paths_match_strongly", "normalize_health_path",
           "health_coverage_signature", "health_churn_for_path", "health_trend_findings_for_path",
           "health_trend_signature", "health_trend_path", "health_paths_refer_to_same_file",
           "health_stored_path_for_request", "sort_health_findings", "health_severity_rank",
           "ToolCodeHealth", "impl:ToolCodeHealth|trait", "HealthToolResponse")
    assign("graph", "FunctionKey", "HealthGraphContext", "HealthGraphSnapshot",
           "impl:HealthGraphSnapshot|inherent", "graph_path_basename", "health_graph_snapshot",
           "health_graph_snapshot_with_scope", "health_graph_scope_signature",
           "health_call_graph", "enrich_health_findings", "enrich_health_finding",
           "health_graph_context", "hot_path_node_ids", "containing_function", "function_key",
           "function_line2", "downgrade_health_severity", "graph_path_matches",
           "graph_name_matches", "graph_entry_name")
    assign("git_risk", "build_git_meta", "build_git_risk_assembly",
           "build_git_risk_assembly_with_roots", "build_git_risk_assembly_blocking",
           "file_is_stable", "nloc_for_file", "function_ranges_from_disk",
           "build_function_git_facts", "function_health_by_range", "age_days_from_ts",
           "function_mod_p80", "HealthImportIndex", "impl:HealthImportIndex|inherent",
           "path_matches_git_filter", "recent_commit_risk_summaries", "truncate_chars",
           "commit_risk_factor_names", "git_risk_response", "GitRiskFileAssembly",
           "GitRiskAssembly", "ToolGitRisk", "impl:ToolGitRisk|trait")
    assign("pr_blast", "pr_blast_index_state", "pr_blast_partial_warning",
           "pr_blast_suggested_reviewers", "pr_blast_is_bot_author", "ToolPrBlast",
           "PrBlastToolResponse", "impl:ToolPrBlast|trait")
    assign("clones", "CloneAnalysis", "CrossFileCloneCache", "clone_analysis_cache",
           "cached_cross_file_clones", "cached_clone_analysis", "ToolCodeDuplication",
           "impl:ToolCodeDuplication|trait")
    assign("dead_code", "DeadCodeIndexState", "DeadCodeEntry", "DeadCodeReport",
           "DeadCodeGitFacts", "dead_code_report", "dead_code_report_with_roots",
           "dead_code_index_state", "dead_code_partial_warning",
           "enrich_dead_symbols_with_facts", "dead_code_confidence", "dead_code_git_recency",
           "dead_code_git_facts_by_node", "dead_code_git_key", "days_since_ts",
           "dead_code_path_matches", "ToolDeadCode", "DeadCodeToolResponse",
           "impl:ToolDeadCode|trait")
    assign("code_map", "CodeMapNode", "CodeMapFile", "CodeMapLink", "CodeMapRenderedPage",
           "CodeMapResponse", "CodeMapFormat", "impl:CodeMapFormat|inherent", "CodeMapArgs",
           "code_map_args", "code_map_node_kind", "code_map_node_visibility",
           "code_map_nodes_from_records", "code_map_symbols_by_path", "normalize_file_scores",
           "code_map_is_hotspot", "code_map_infra_path", "top_level_module",
           "second_level_module", "module_selections", "scc_path_groups",
           "build_code_map_files", "code_map_selection_input", "page_title", "page_kind_label",
           "page_paths", "code_map_links_for_page", "symbol_kind_counts",
           "symbol_visibility_counts", "trim_to_token_allowance", "page_signal_labels",
           "render_code_map_page_markdown", "code_map_backlink_hubs", "code_map_page_prior",
           "choose_code_map_pages", "render_code_map_claude_md", "assemble_code_map_response",
           "ToolCodeMap", "impl:ToolCodeMap|trait")
    assign("code_why", "ToolCodeWhy", "CODE_WHY_FILE_READ_ATTEMPTS", "CodeWhySource",
           "CodeWhyFileCandidate", "CodeWhySourceMeta", "CodeWhyCandidate", "CodeWhyDecision",
           "ScoredCodeWhyDecision", "code_why_priority", "short_oid", "path_ref",
           "merge_commit_has_body", "commit_prose_text", "git_code_why_sources",
           "glob_relative_files", "code_why_file_kinds", "code_why_file_candidates",
           "sort_code_why_source_meta", "select_code_why_file_candidates",
           "file_code_why_sources", "code_why_selected_candidate_count_before_reads",
           "sort_code_why_sources", "cap_code_why_sources", "assemble_code_why_sources",
           "code_why_candidates", "better_code_why_candidate", "representative_candidate",
           "corroborated_code_why_decisions", "source_rank_for_kind", "verification_for_status",
           "score_code_why_decision", "sort_scored_code_why_decisions", "provenance_tag",
           "related_decision_graph", "relation_label", "CodeWhyResponse", "code_why_response",
           "code_why_tokens", "code_why_statement_matches", "impl:ToolCodeWhy|trait")

    for it in prod:
        if it["name"] not in ASSIGN:
            if it["kind"] in ("const", "static", "type", "use"):
                assign("mod", it["name"])
            else:
                raise SystemExit("unassigned item: %s (%s)" % (it["name"], it["head"]))
    return prod, tests_item


def plan_widening(prod, tests_body):
    texts = defaultdict(list)
    for it in prod:
        texts[ASSIGN[it["name"]]].append(it["text"])
    texts = {m: "\n".join(v) for m, v in texts.items()}

    def used_from_another_module(name, module):
        pattern = re.compile(r"\b" + re.escape(name) + r"\b")
        others = any(pattern.search(text) for m, text in texts.items() if m != module)
        return others or bool(pattern.search(tests_body))

    item_widen = set()
    for it in prod:
        # mod.rs privates need nothing: the children and the tests module are its descendants.
        if ASSIGN[it["name"]] == "mod" or it["vis"] or it["kind"] in ("use", "impl", "mod"):
            continue
        if used_from_another_module(it["name"], ASSIGN[it["name"]]):
            item_widen.add(it["name"])

    crossing = set()
    for it in prod:
        if ASSIGN[it["name"]] != "mod" and it["kind"] in ("struct", "enum", "trait", "type",
                                                          "union") and \
                used_from_another_module(it["name"], ASSIGN[it["name"]]):
            crossing.add(it["name"])

    field_widen = set()
    method_widen = set()
    for it in prod:
        if ASSIGN[it["name"]] == "mod":
            continue
        module = ASSIGN[it["name"]]

        def member_used_elsewhere(member, kind):
            if kind == "field":
                pattern = re.compile(r"\.\s*" + re.escape(member) + r"\b(?!\s*\()")
            else:
                pattern = re.compile(r"\.\s*" + re.escape(member) + r"\s*\(")
            outside = any(pattern.search(text) for m, text in texts.items() if m != module)
            return outside or bool(pattern.search(tests_body))

        if it["kind"] == "struct":
            for m in FIELD_RE.finditer(it["text"]):
                line_start = it["text"].rfind("\n", 0, m.start()) + 1
                line = it["text"][line_start:it["text"].find("\n", line_start)]
                if re.match(r"^[ \t]+pub\b", line):
                    continue
                if it["name"] in crossing or member_used_elsewhere(m.group("name"), "field"):
                    field_widen.add((it["name"], m.group("name")))
        if it["kind"] == "impl" and it["name"].endswith("|inherent"):
            ty = it["name"][len("impl:"):-len("|inherent")]
            for m in FN_RE.finditer(it["text"]):
                line_start = it["text"].rfind("\n", 0, m.start()) + 1
                line = it["text"][line_start:it["text"].find("\n", line_start)]
                if re.match(r"^[ \t]+pub\b", line):
                    continue
                if ty in crossing or member_used_elsewhere(m.group("name"), "method"):
                    method_widen.add((ty, m.group("name")))
    return item_widen, field_widen, method_widen, crossing


def rewrite(it, item_widen, field_widen, method_widen):
    """Insert visibility tokens only; the original lines are otherwise untouched."""
    text = it["text"]
    if it["name"] in item_widen:
        lines = text.split("\n")
        head_idx = 0
        while lines[head_idx].startswith("#"):
            head_idx += 1
        assert lines[head_idx] == it["head"], (lines[head_idx], it["head"])
        lines[head_idx] = "pub(super) " + lines[head_idx]
        text = "\n".join(lines)
    if it["kind"] == "struct":
        out = []
        for line in text.splitlines():
            m = FIELD_RE.match(line)
            if m and (it["name"], m.group("name")) in field_widen:
                indent = len(m.group("indent"))
                line = line[:indent] + "pub(super) " + line[indent:]
            out.append(line)
        text = "\n".join(out)
    if it["kind"] == "impl":
        ty = it["name"][len("impl:"):].split("|")[0]
        out = []
        for line in text.splitlines():
            m = FN_RE.match(line)
            if m and (ty, m.group("name")) in method_widen:
                indent = len(m.group("indent"))
                line = line[:indent] + "pub(super) " + line[indent:]
            out.append(line)
        text = "\n".join(out)
    return text


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true",
                        help="write the split files and delete the original module")
    args = parser.parse_args()

    items = parse_items()
    prod, tests_item = build_assignment(items)
    tests_text = "\n".join(LINES[tests_item["start"]:])
    tests_body = "\n".join(LINES[TESTS_DECL + 1:-1])
    assert tests_body.splitlines()[0].strip() == "use super::*;"

    item_widen, field_widen, method_widen, crossing = plan_widening(prod, tests_body)

    bodies = defaultdict(list)
    for it in prod:
        if it["start"] < HEADER_LAST:      # crate imports: emitted verbatim in the header
            continue
        bodies[ASSIGN[it["name"]]].append(rewrite(it, item_widen, field_widen, method_widen))

    header = list(LINES[:HEADER_LAST])
    header.append("")
    for module in MODULES:
        header.append("mod %s;" % module)
    header.append("")
    for module in MODULES:
        header.append("#[allow(unused_imports)]")
        header.append("pub(crate) use self::%s::*;" % module)
    header.append("")

    files = {"mod": "\n".join(header) + "\n".join(bodies["mod"]).rstrip() + "\n\n"
                     + tests_text.rstrip() + "\n"}
    for module in MODULES:
        files[module] = "#[allow(unused_imports)]\nuse super::*;\n\n" \
                        + "\n".join(bodies[module]).rstrip() + "\n"

    print("widened private -> pub(super): %d items, %d fields, %d inherent methods"
          % (len(item_widen), len(field_widen), len(method_widen)))
    print("types crossing a module boundary: %d %s" % (len(crossing), sorted(crossing)))
    print("\nemitted line counts")
    total = 0
    for name in ["mod"] + MODULES:
        n = len(files[name].splitlines())
        total += n
        prod_n = n - len(tests_text.splitlines()) - 2 if name == "mod" else n
        flag = "  <-- OVER 1500 prod" if prod_n >= 1500 else ""
        print("  %-12s %5d lines (%5d prod)%s" % (name + ".rs", n, prod_n, flag))
    print("  TOTAL %d (original %d)" % (total, len(LINES)))

    if args.write:
        OUT.mkdir(parents=True, exist_ok=True)
        for name in ["mod"] + MODULES:
            (OUT / ("%s.rs" % name)).write_text(files[name], encoding="utf-8", newline="\n")
        if SRC.exists():
            SRC.unlink()
            print("wrote %s and removed %s" % (OUT, SRC))
        else:
            print("wrote %s (source already removed)" % OUT)


if __name__ == "__main__":
    main()
