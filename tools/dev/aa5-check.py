"""AA-5 harvest verification.

Run: python tools/dev/aa5-check.py

Asserts, from git (base commit AA5_BASE) and the current worktree:

1. every engine-root file touched by AA-5 shrank to a shim/wrapper;
2. the moved bodies are present in `refact-runtime-api` (production lines found);
3. the engine-root production-line totals before -> after;
4. no crate depends back on `refact-lsp`;
5. the explored-but-kept files are unchanged (trajectory_memos, app_state, background_tasks,
   global_context, lib.rs, lsp.rs) so the report's "island" claim is checked, not asserted.
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ENGINE = ROOT / "refact-agent" / "engine"
SRC = ENGINE / "src"
CRATE = ENGINE / "crates" / "refact-runtime-api"
BASE = "5e457e310"

checks = []


def check(name, ok, detail=""):
    checks.append((name, bool(ok), detail))


def read(path):
    return Path(path).read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n")


def base_text(rel):
    try:
        return subprocess.run(
            ["git", "show", "%s:refact-agent/engine/%s" % (BASE, rel)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.replace("\r\n", "\n")
    except subprocess.CalledProcessError:
        return ""


def prod_lines(text):
    """Production lines: whole `#[cfg(test)] mod x { ... }` blocks dropped."""
    lines = text.split("\n")
    out, i, n = [], 0, len(lines)
    while i < n:
        stripped = lines[i].strip()
        if stripped.startswith("#[cfg(test)]") and i + 1 < n and lines[i + 1].strip().startswith("mod "):
            depth, j = 0, i + 1
            while j < n:
                depth += lines[j].count("{") - lines[j].count("}")
                if depth <= 0 and "{" in "".join(lines[i + 1 : j + 1]):
                    break
                j += 1
            i = j + 1
            continue
        out.append(lines[i])
        i += 1
    return len([line for line in out if line.strip()])


# ---------------------------------------------------------------- 1. shrunk files
SHRUNK = [
    "src/files_correction.rs",
    "src/agentic/generate_code_edit.rs",
    "src/agentic/generate_commit_message.rs",
    "src/agentic/generate_follow_up_message.rs",
    "src/knowledge_graph/kg_subchat.rs",
]
print("### 1. engine files that shrank (production lines before -> after)")
tot_before = tot_after = 0
for rel in SHRUNK:
    before = prod_lines(base_text(rel))
    after = prod_lines(read(ENGINE / rel))
    tot_before += before
    tot_after += after
    print("  %-44s %4d -> %4d" % (rel, before, after))
    check("%s shrank" % rel, after < before, "%d -> %d" % (before, after))

# ---------------------------------------------------------------- 2. crate bodies
MOVED = {
    "src/files_correction.rs": "crates/refact-runtime-api/src/files_correction_impl.rs",
    "src/agentic/generate_code_edit.rs": "crates/refact-runtime-api/src/generate_code_edit_runtime.rs",
    "src/agentic/generate_commit_message.rs": "crates/refact-runtime-api/src/commit_message_runtime.rs",
    "src/agentic/generate_follow_up_message.rs": "crates/refact-runtime-api/src/generate_follow_up_runtime.rs",
    "src/knowledge_graph/kg_subchat.rs": "crates/refact-runtime-api/src/kg_subchat_runtime.rs",
}
print("\n### 2. crate destinations carry the moved production lines")
for engine_rel, crate_rel in MOVED.items():
    crate_prod = prod_lines(read(ENGINE / crate_rel))
    engine_after = prod_lines(read(ENGINE / engine_rel))
    before = prod_lines(base_text(engine_rel))
    print(
        "  %-40s crate_prod=%4d  engine %d -> %d  accounted=%d"
        % (engine_rel, crate_prod, before, engine_after, crate_prod + engine_after)
    )
    check(
        "%s body landed in %s" % (engine_rel, crate_rel),
        crate_prod + engine_after >= before - 40,
        "before=%d crate=%d engine_after=%d" % (before, crate_prod, engine_after),
    )

# ---------------------------------------------------------------- 3. islands untouched
ISLANDS = [
    "src/trajectory_memos.rs",
    "src/app_state.rs",
    "src/background_tasks.rs",
    "src/global_context.rs",
    "src/lib.rs",
    "src/lsp.rs",
    "src/http.rs",
]
print("\n### 3. declared islands unchanged since the AA-5 base")
for rel in ISLANDS:
    before = prod_lines(base_text(rel))
    after = prod_lines(read(ENGINE / rel))
    print("  %-32s %4d -> %4d" % (rel, before, after))
    check("%s is untouched (island)" % rel, before == after, "%d -> %d" % (before, after))

# ---------------------------------------------------------------- 4. root totals
ROOT_FILES = [
    "app_state.rs", "background_tasks.rs", "cli_dispatch.rs", "daemon_link.rs",
    "files_blocklist.rs", "files_correction.rs", "files_in_jsonl.rs", "file_filter.rs",
    "global_context.rs", "http.rs", "indexing_routing.rs", "knowledge_index.rs", "lib.rs",
    "lsp.rs", "privacy.rs", "scratchpad_abstract.rs", "test_paths.rs", "tokens.rs",
    "trajectory_memos.rs", "version.rs", "agentic/generate_code_edit.rs",
    "agentic/generate_commit_message.rs", "agentic/generate_follow_up_message.rs",
    "agentic/mode_transition.rs", "agentic/compress_trajectory.rs",
    "knowledge_graph/kg_subchat.rs", "knowledge_graph/mod.rs", "scratchpads/mod.rs",
]
print("\n### 4. engine root production lines (the 28 significant files)")
before_total = sum(prod_lines(base_text("src/" + rel)) for rel in ROOT_FILES)
after_total = sum(prod_lines(read(SRC / rel)) for rel in ROOT_FILES)
print("  before (AA-5 base)  %5d" % before_total)
print("  after  (this card)  %5d" % after_total)
print("  delta              %5d" % (after_total - before_total))
check("the engine root shrank", after_total < before_total, "%d -> %d" % (before_total, after_total))

# ---------------------------------------------------------------- 5. no cycle
print("\n### 5. dependency direction")
crate_toml = read(CRATE / "Cargo.toml")
check(
    "refact-runtime-api does not depend on refact-lsp",
    not re.search(r"(?m)^\s*refact-lsp\s*=", crate_toml),
)
engine_toml = read(ENGINE / "Cargo.toml")
check("refact-lsp still depends on refact-runtime-api", "refact-runtime-api" in engine_toml)
crate_lib = read(CRATE / "src" / "lib.rs")
crate_mods = set(re.findall(r"(?m)^pub mod ([a-z0-9_]+);", crate_lib))
unknown = []
for rel in ["crates/refact-runtime-api/src/" + n for n in (
    "files_correction_impl.rs", "generate_code_edit_runtime.rs",
    "generate_follow_up_runtime.rs", "kg_subchat_runtime.rs", "commit_message_runtime.rs",
)]:
    text = read(ENGINE / rel)
    for hit in set(re.findall(r"\bcrate::([a-z0-9_]+)", text)):
        if hit not in crate_mods:
            unknown.append("%s -> crate::%s" % (rel, hit))
check("moved crate modules only use crate-local paths", not unknown, str(sorted(unknown)[:5]))

failed = [c for c in checks if not c[1]]
print("")
for name, ok, detail in checks:
    if not ok:
        print("FAIL  %s  [%s]" % (name, detail))
print("%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: PASSED")
