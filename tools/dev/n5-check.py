"""N-5 verification harness: static proof that the tool_codegraph split is faithful.

No cargo run here - compilation is the planner's gate (agent worktrees do not build).
Every check is a filesystem / text comparison, and the original single file is read
back from git history, so the check cannot be fooled by a stale copy on disk.

Run (single command, no shell separators):
    python tools/dev/n5-check.py
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENGINE = ROOT / "refact-agent" / "engine"
TOOLS = ENGINE / "src" / "tools"
OLD = TOOLS / "tool_codegraph.rs"
NEW = TOOLS / "tool_codegraph"
REL_OLD = "refact-agent/engine/src/tools/tool_codegraph.rs"

MODULES = ["tools", "health", "graph", "git_risk", "pr_blast", "clones",
           "dead_code", "code_map", "code_why"]

# Everything the single file exposed at `pub` / `pub(crate)` before the split.
ORIGINAL_SURFACE = [
    "CloneAnalysis", "DeadCodeEntry", "DeadCodeGitFacts", "DeadCodeIndexState",
    "DeadCodeReport", "GitRiskAssembly", "GitRiskFileAssembly", "HealthAnalysisContext",
    "HealthFileAnalysis", "HealthFinding", "HealthFunctionSummary", "HealthGraphSnapshot",
    "HealthImpactContributor", "PrBlastIndexState", "RecentCommitRiskSummary",
    "SuggestedReviewer", "ToolCodeDuplication", "ToolCodeHealth", "ToolCodeMap",
    "ToolCodeWhy", "ToolCodegraphOverview", "ToolDeadCode", "ToolGitRisk", "ToolPrBlast",
    "ToolSecurityScan", "analyze_health_file_shared", "build_git_meta",
    "build_git_risk_assembly", "build_health_snapshot", "cached_cross_file_clones",
    "cached_mine_history", "cached_mine_history_async", "co_change_count",
    "dead_code_index_state", "dead_code_partial_warning", "dead_code_report",
    "enrich_dead_symbols_with_facts", "enrich_health_findings",
    "execution_flow_truncation_notice", "git_churn_by_file", "git_risk_response",
    "health_graph_snapshot", "health_paths_refer_to_same_file",
    "health_stored_path_for_request", "load_health_snapshots", "persist_health_snapshots",
    "pr_blast_index_state", "pr_blast_is_bot_author", "pr_blast_partial_warning",
    "pr_blast_suggested_reviewers", "project_dir", "push_health_snapshot",
    "top_git_function_fact_paths",
]

# The subset `src/http/routers/v1/code_intel.rs` consumes through the module path.
CODE_INTEL_SYMBOLS = [
    "CloneAnalysis", "GitRiskAssembly", "HealthAnalysisContext", "HealthFileAnalysis",
    "HealthGraphSnapshot", "PrBlastIndexState", "analyze_health_file_shared",
    "build_git_risk_assembly", "build_health_snapshot", "cached_cross_file_clones",
    "cached_mine_history_async", "co_change_count", "dead_code_report",
    "health_graph_snapshot", "health_paths_refer_to_same_file", "load_health_snapshots",
    "persist_health_snapshots", "pr_blast_index_state", "pr_blast_partial_warning",
    "pr_blast_suggested_reviewers", "project_dir", "push_health_snapshot",
    "top_git_function_fact_paths",
]

MAX_PROD_LINES = 1500
MIN_FILES = 6

DECL_RE = (r"^pub(?:\(crate\))?\s+(?:async\s+)?"
           r"(?:fn|struct|enum|const|static|type|trait|union)\s+%s\b")
USE_RE = r"^pub(?:\(crate\))?\s+use [a-z_:]+::%s;$"
ANY_DECL_RE = (r"^(?:pub(?:\(crate\)|\(super\))?\s+)?(?:async\s+)?"
               r"(?:fn|struct|enum|const|static|type|trait|union)\s+([A-Za-z0-9_]+)")

GENERATED = [
    r"^#\[allow\(unused_imports\)\]$",
    r"^pub\(crate\) use self::[a-z_]+::\*;$",
    r"^mod (tools|health|graph|git_risk|pr_blast|clones|dead_code|code_map|code_why);$",
    r"^use super::\*;$",
]

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(path):
    return path.read_text(encoding="utf-8")


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args],
                          capture_output=True, text=True, encoding="utf-8")


def original_file_text():
    """The single-file module: the newest blob that still had content at that path."""
    revs = git("log", "--format=%H", "--", REL_OLD).stdout.split()
    revs += ["refs/remotes/origin/main", "HEAD"]
    for rev in revs:
        result = git("show", "%s:%s" % (rev, REL_OLD))
        if result.returncode == 0 and result.stdout.strip():
            return result.stdout, rev
    return "", None


# ---------------------------------------------------------------- 1. shape
check("tool_codegraph.rs is gone", not OLD.exists())
check("tool_codegraph/ exists", NEW.is_dir())
files = sorted(p.name for p in NEW.glob("*.rs"))
check("tool_codegraph/ has >= %d files" % MIN_FILES, len(files) >= MIN_FILES, str(files))
check("tool_codegraph/ has mod.rs", "mod.rs" in files)
check("no non-.rs file in tool_codegraph/",
      all(p.suffix == ".rs" for p in NEW.iterdir()), str([p.name for p in NEW.iterdir()]))

old_text, old_rev = original_file_text()
check("original file readable from git history", bool(old_text.strip()),
      "no non-empty blob for %s" % REL_OLD)
orig_lines = old_text.splitlines()

TESTS_DECL = orig_lines.index("mod tests {")
TESTS_TEXT = orig_lines[TESTS_DECL - 1:]
PROD_LINES_ORIG = TESTS_DECL - 1

# ------------------------------------------------------- 2. size per file
mod_rs = read(NEW / "mod.rs")
mod_lines = mod_rs.splitlines()
check("mod.rs keeps the `#[cfg(test)] mod tests {` block verbatim",
      "\n".join(TESTS_TEXT) in mod_rs)

counts = {}
for name in files:
    body = read(NEW / name).splitlines()
    prod = len(body) - len(TESTS_TEXT) - 1 if name == "mod.rs" else len(body)
    counts[name] = (len(body), prod)
    check("%s is under %d prod lines" % (name, MAX_PROD_LINES), prod < MAX_PROD_LINES,
          "%d prod lines" % prod)


# ------------------------------------------------ 3. line-for-line coverage
def normalize(lines):
    """Drop generated scaffolding and the narrowing visibility token."""
    out = []
    for line in lines:
        if not line.strip():
            continue
        if any(re.match(pattern, line) for pattern in GENERATED):
            continue
        out.append(re.sub(r"^(\s*)pub\(super\) ", r"\1", line.rstrip()))
    return out


split_lines = []
for name in files:
    split_lines += read(NEW / name).splitlines()

before = normalize(orig_lines)
after = normalize(split_lines)
check("no line lost from the original file", len(before) == len(after),
      "orig %d vs split %d" % (len(before), len(after)))
check("no line duplicated, dropped or reordered", sorted(before) == sorted(after))

# Within each destination file the original relative order is preserved: the split only
# partitions the original top-level items, it never reorders them.
orig_order = [match.group(1) for match in
              re.finditer(ANY_DECL_RE, "\n".join(orig_lines[:PROD_LINES_ORIG]), re.M)]
split_order = []
for name in files:
    body = read(NEW / name)
    if name == "mod.rs":
        body = body[:body.index("#[cfg(test)]\nmod tests {")]
    split_order += [match.group(1) for match in re.finditer(ANY_DECL_RE, body, re.M)]
undecided = sorted(set(split_order) - set(orig_order))
check("every split module declares only names from the original file", not undecided,
      str(undecided))
lost = sorted({n for n in orig_order if n not in split_order})
check("every original top-level item is present in the split", not lost, str(lost))
# ---------------------------------------------- 4. module wiring and globs
for module in MODULES:
    check("mod.rs declares `mod %s;`" % module, ("mod %s;" % module) in mod_lines)
    check("mod.rs re-exports `%s`" % module, ("pub(crate) use self::%s::*;" % module) in mod_rs)
check("mod.rs declares exactly the %d children" % len(MODULES),
      len(re.findall(r"^mod [a-z_]+;$", mod_rs, re.M)) == len(MODULES))
for name in files:
    if name == "mod.rs":
        continue
    head = read(NEW / name).splitlines()[:2]
    check("%s starts with the super glob" % name,
          head == ["#[allow(unused_imports)]", "use super::*;"], repr(head))

# ------------------------------------- 5. outward surface of the module path
surface_text = "\n".join(read(NEW / name) for name in files)
for symbol in ORIGINAL_SURFACE:
    declared = re.search(DECL_RE % re.escape(symbol), surface_text, re.M) or re.search(
        USE_RE % re.escape(symbol), surface_text, re.M)
    check("`%s` is still pub/pub(crate)" % symbol, bool(declared))
missing_code_intel = [
    symbol for symbol in CODE_INTEL_SYMBOLS
    if not (re.search(DECL_RE % re.escape(symbol), surface_text, re.M)
            or re.search(USE_RE % re.escape(symbol), surface_text, re.M))
]
check("all %d code_intel.rs symbols are covered" % len(CODE_INTEL_SYMBOLS),
      not missing_code_intel, str(missing_code_intel))

# --------------------------------------------- 6. no new public export tier
check("no `pub use self::` tier (module is pub(crate), so the glob is exact)",
      "pub use self::" not in mod_rs)

# --------------------------------- 6b. the globbed children must not collide
declared_by_child = {}
for name in files:
    if name == "mod.rs":
        continue
    for match in re.finditer(ANY_DECL_RE, read(NEW / name), re.M):
        declared_by_child.setdefault(match.group(1), set()).add(name)
collisions = {n: sorted(where) for n, where in declared_by_child.items() if len(where) > 1}
check("no name is declared by two children (glob ambiguity)", not collisions, str(collisions))

mod_names = set(re.findall(ANY_DECL_RE, mod_rs, re.M))
shadowed = sorted(mod_names & set(declared_by_child))
check("mod.rs does not shadow a globbed child item", not shadowed, str(shadowed))

# ------------------------------- 6c. no module reads another module's private item
# A child-owned item declared without a visibility token is visible inside that child
# only. Any reference to it from a sibling would be E0603/E0601 at compile time, so the
# widened `pub(super)` set must cover every cross-module use.
mod_prod_text = mod_rs[:mod_rs.index("#[cfg(test)]\nmod tests {")]
child_bodies = {name: read(NEW / name) for name in files if name != "mod.rs"}
private_child_items = {}
for name, body in child_bodies.items():
    for match in re.finditer(ANY_DECL_RE, body, re.M):
        if match.group(0).lstrip().startswith("pub"):
            continue
        private_child_items.setdefault(match.group(1), set()).add(name)

dangling = []
for item, owners in private_child_items.items():
    pattern = re.compile(r"\b" + re.escape(item) + r"\b")
    readers = [name for name, body in child_bodies.items() if name not in owners
               and pattern.search(body)]
    if pattern.search(mod_prod_text):
        readers.append("mod.rs")
    if readers:
        dangling.append((item, sorted(owners), readers))
check("no module references another module's private item", not dangling, str(dangling))

mod_private_items = {match.group(1) for match in re.finditer(ANY_DECL_RE, mod_prod_text, re.M)
                     if not match.group(0).lstrip().startswith("pub")}
check("mod.rs private items are reachable (children + tests are descendants)",
      bool(mod_private_items), "%d mod.rs private items" % len(mod_private_items))

# ------------------------------------------------- 7. scope of the change
status = git("status", "--porcelain", "--untracked-files=all").stdout
touched = sorted({line[3:].strip().replace("\\", "/") for line in status.splitlines()
                  if line[3:] and not line[3:].strip().startswith(".")})
allowed = (
    "refact-agent/engine/src/tools/tool_codegraph",
    "tools/dev/n5-check.py",
    "tools/dev/n5-split.py",
)
stray = [t for t in touched if not t.startswith(allowed)]
check("no file outside tool_codegraph/ and this harness changed", not stray, str(stray))
tracked = git("ls-files", REL_OLD).stdout.strip()
check("the old single file is deleted", not tracked and not OLD.exists(), tracked)

# --------------------------------------------------------- 8. syntax check
if subprocess.run(["rustfmt", "--version"], capture_output=True, text=True).returncode == 0:
    for name in files:
        result = subprocess.run(["rustfmt", "--edition", "2021", "--emit", "stdout",
                                 str(NEW / name)], capture_output=True, text=True)
        check("%s parses" % name, result.returncode == 0,
              (result.stderr or result.stdout)[:200])
else:
    check("rustfmt unavailable, parse check skipped", True, "")

# ------------------------------------------------------------------ report
print("\n=== per-file line counts")
total = 0
for name in files:
    body, prod = counts[name]
    total += body
    print("  %-12s %5d lines (%5d prod)" % (name, body, prod))
print("  TOTAL %d lines; the original single file had %d prod + %d test lines = %d"
      % (total, PROD_LINES_ORIG, len(TESTS_TEXT), len(orig_lines)))
print("  re-exported surface: %d symbols (%d of them consumed by code_intel.rs)"
      % (len(ORIGINAL_SURFACE), len(CODE_INTEL_SYMBOLS)))

failed = [name for name, ok, _ in checks if not ok]
print("\n%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
