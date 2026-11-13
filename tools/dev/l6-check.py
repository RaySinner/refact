"""L-6 verification harness: static proof that the K3a read/search move is
behaviour-preserving.

Every assertion is a filesystem/text check with no shell separators.

Run: python tools/dev/l6-check.py
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
CRATE = ENG / "crates" / "refact-tools"
DST = CRATE / "src" / "tools"

MOVED = [
    "tool_cat",
    "tool_tree",
    "tool_glob",
    "tool_regex_search",
    "tool_search",
    "tool_ast_definition",
    "native_enrichment",
]
ALREADY_SHIM = ["scope_utils"]
ENGINE_PATHS = {
    "tool_cat": ["refact_core::chat_types::MultimodalElement"],
    "tool_regex_search": ["refact_runtime_api::global_context::"],
    "tool_search": ["refact_runtime_api::global_context::"],
    "tool_ast_definition": ["refact_codegraph::CodeGraphService", "refact_runtime_api::global_context::"],
    "native_enrichment": ["refact_runtime_api::global_context::"],
}
TEST_MODS = {
    "tool_cat": ("mod tests {", 13),
    "tool_tree": ("mod privacy_and_bounds_tests {", 1),
    "tool_glob": ("mod tests {", 9),
    "tool_regex_search": ("mod tests {", 9),
    "tool_search": ("mod tests {", 6),
    "tool_ast_definition": ("mod tests {", 10),
    "native_enrichment": ("mod tests {", 2),
}
NAMED = {m: "%s.rs" % m for m in MOVED}

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(p):
    return p.read_text(encoding="utf-8")


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True).stdout.strip()


# 1. crate files exist and carry the real production body
for mod in MOVED:
    p = DST / f"{mod}.rs"
    body = read(p) if p.exists() else ""
    check(f"crate {mod}.rs exists and is non-trivial", p.exists() and len(body) > 200, f"{len(body)} bytes")

# 2. no crate:: reference outside crate::tools:: in the crate (no engine edge)
for mod in MOVED:
    body = read(DST / f"{mod}.rs")
    stray = sorted({m for m in re.findall(r"crate::[\w:]*", body) if not m.startswith("crate::tools::")})
    check(f"crate {mod}.rs has no engine-only crate:: refs", not stray, str(stray))

# 3. the engine-path rewrites landed
for mod, needles in ENGINE_PATHS.items():
    body = read(DST / f"{mod}.rs")
    check(f"crate {mod}.rs rewrites crate::global_context/codegraph/scratchpads",
          all(n in body for n in needles), str([n for n in needles if n not in body]))

# 4. engine files are shims that keep their original tests verbatim
for mod in MOVED:
    body = read(SRC / f"{mod}.rs")
    check(f"engine {mod}.rs starts with the re-export shim",
          body.startswith(f"pub use refact_tools::tools::{mod}::*;"),
          body.splitlines()[0] if body else "")
    got = len(re.findall(r"#\[(?:tokio::)?test[\(\]]", body))
    mod_head, want = TEST_MODS[mod]
    check(f"engine {mod}.rs keeps {want} tests", got == want, f"got {got}")
    check(f"engine {mod}.rs keeps its {mod_head}", f"#[cfg(test)]\n{mod_head}" in body)

# 5. the already-shim member was not disturbed
su = read(SRC / "scope_utils.rs")
check("scope_utils.rs is still the 2-line runtime-api shim",
      su.startswith("pub use refact_runtime_api::scope_utils::*;") and su.count("#[cfg(test)]") == 1,
      su.splitlines()[0])

# 6. crate module table
tools_rs = read(CRATE / "src" / "tools.rs")
for mod in MOVED:
    check(f"crate tools.rs declares pub mod {mod}", f"pub mod {mod};" in tools_rs)

# 7. crate manifest
crate_toml = read(CRATE / "Cargo.toml")
check("crate manifest declares itertools", 'itertools = "0.14.0"' in crate_toml)
check("crate manifest declares resvg", 'resvg = "0.44.0"' in crate_toml)
check("crate manifest never mentions refact-lsp", "refact-lsp" not in crate_toml)

# 8. lock wiring (no re-resolution churn)
lock = read(ENG / "Cargo.lock")
tools_block = lock.split('name = "refact-tools"')[1].split("[[package]]")[0]
check("Cargo.lock refact-tools block depends on itertools 0.14.0", ' "itertools 0.14.0",' in tools_block)
check("Cargo.lock refact-tools block depends on resvg", ' "resvg",' in tools_block)
check("Cargo.lock refact-tools block still has no refact-lsp", "refact-lsp" not in tools_block)

# 9. widened symbols the staying tests reach
for mod, symbol in [("tool_glob", "pub struct GlobOutcome"), ("tool_glob", "pub fn run_glob"),
                    ("tool_glob", "pub fn parse_max_results"), ("tool_glob", "pub const CAP_MAX_RESULTS"),
                    ("tool_regex_search", "pub async fn search_files_with_regex"),
                    ("tool_regex_search", "pub const MAX_SEARCH_FILE_BYTES"),
                    ("tool_search", "pub struct FallbackOutcome"),
                    ("tool_search", "pub fn semantic_no_match_result"),
                    ("tool_search", "pub fn parse_usize_arg"),
                    ("tool_ast_definition", "pub static IDENTIFIER_RE"),
                    ("tool_ast_definition", "pub fn related_memory_entities"),
                    ("tool_ast_definition", "pub async fn symbol_def_via_codegraph"),
                    ("native_enrichment", "pub struct NativeReferences"),
                    ("native_enrichment", "pub fn workspace_roots"),
                    ("tool_cat", "pub fn expansion_cap_notice"),
                    ("tool_cat", "pub fn clamp_cat_line_range")]:
    check(f"crate {mod} exposes {symbol}", symbol in read(DST / f"{mod}.rs"))

# 10. constructor surface: engine call sites still name the same paths, each a shim
named = set()
for path in SRC.glob("*.rs"):
    for m in re.findall(r"crate::tools::(\w+)::", read(path)):
        named.add(m)
surface = sorted(n for n in named if n in MOVED)
check("engine call sites still name the moved modules", len(surface) == len(MOVED), str(surface))
for mod in MOVED:
    check(f"crate::tools::{mod}:: still resolves via a shim",
          read(SRC / f"{mod}.rs").startswith(f"pub use refact_tools::tools::{mod}::*;"))
check("tools_list.rs still constructs all six K3a tools",
      all(f"crate::tools::{m}::{s}" in read(SRC / "tools_list.rs")
          for m, s in [("tool_ast_definition", "ToolAstDefinition"), ("tool_tree", "ToolTree"),
                       ("tool_cat", "ToolCat"), ("tool_regex_search", "ToolRegexSearch"),
                       ("tool_glob", "ToolGlob"), ("tool_search", "ToolSearch")]))

# 11. nothing outside the move was touched (compare the change set of the
#     delivered commits against the pre-move base, not just the dirty tree)
BASE = "620f9c2ee"
names = git("diff", "--name-only", f"{BASE}..HEAD").splitlines()
touched = {n.replace("\\", "/") for n in names if n.strip()}
allowed_prefixes = (
    "refact-agent/engine/src/tools/",
    "refact-agent/engine/crates/refact-tools/",
    "refact-agent/engine/Cargo.lock",
    "tools/dev/l6-",
)
extra = [t for t in touched if not t.startswith(allowed_prefixes)]
check("no unrelated file touched", not extra, str(extra))
for keep in ["mod.rs", "tools_list.rs", "tests_schema.rs", "tool_codegraph.rs", "tool_knowledge.rs",
             "subagent_phases.rs", "ctx.rs", "scope_utils.rs", "settings_guard.rs", "tool_cmdline.rs"]:
    check(f"src/tools/{keep} untouched", f"refact-agent/engine/src/tools/{keep}" not in touched)

failed = [c for c in checks if not c[1]]
print(f"\n{len(checks) - len(failed)}/{len(checks)} checks passed")
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
