"""N-4 verification harness: static proof the K5b/K3b/K11 move is behaviour-preserving.

No build is attempted here (subagents must not compile; the planner owns the
engine build). Every assertion is a filesystem/text check with no shell separator.

Run: python tools/dev/n4-check.py
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
BASE = "bca526164"

MOVED = [
    "tool_task_documents",
    "tool_task_memory",
    "subagent_phases",
    "tool_add_workspace_folder",
    "tool_create_knowledge",
    "tool_knowledge",
    "tool_activate_skill",
    "tool_config_subagent",
    "tool_cron_list",
    "tool_cron_delete",
]

# of the K11 group, these two stay in the engine (cron_create needs the
# engine-only `chat::get_or_create_session_with_trajectory`; cron_update imports
# cron_create). They must be untouched.
LEFT_IN_ENGINE = ["tool_cron_create", "tool_cron_update"]

TESTS = {
    "tool_task_documents": 16,
    "tool_task_memory": 25,
    "subagent_phases": 4,
    "tool_activate_skill": 16,
    "tool_knowledge": 1,
    "tool_cron_list": 7,
    "tool_cron_delete": 3,
}

# items/fields the engine-side staying tests read: widened to `pub` in the crate
WIDENED = {
    "tool_task_documents": [
        "pub struct DocumentFrontmatter {", "pub struct TaskDocument {", "pub struct HistoryItem {",
        "pub const HISTORY_CAP", "pub fn validate_slug(", "pub fn document_path(",
        "pub fn deleted_dir(", "pub async fn create_document_at(", "pub async fn next_available_slug_at(",
        "pub async fn list_documents_at(", "pub fn format_doc_list(",
        "pub async fn documents_dir_for_task_for_ctx(", "pub struct CreateDocumentRequest {",
        "pub slug: String,", "pub content: String,",
    ],
    "tool_task_memory": [
        "pub struct TaskMemoryFrontmatter {", "pub struct TaskMemoryInboxEntry {",
        "pub const MEMORIES_DIR", "pub const MAX_DUPLICATE_COMPARISONS",
        "pub fn parse_memory_file(", "pub fn render_memory_file(",
        "pub fn resolve_memory_namespace(", "pub async fn read_memory_inbox_cursor(",
        "pub async fn load_task_memory_inbox_entries(", "pub fn new_memories_since(",
        "pub fn stale_memory_candidates(", "pub fn duplicate_memory_pairs(",
        "pub fn render_memory_inbox(", "pub async fn mark_memory_superseded(",
        "pub fn parse_rfc3339_utc(", "pub async fn load_task_memories(",
        "pub title: String,", "pub supersedes: Option<String>,",
    ],
    "subagent_phases": [
        "pub async fn resolve_gathered_file_path(", "pub fn get_last_assistant_content(",
        "pub fn resolve_gathered_file_path_with_scope(",
    ],
    "tool_knowledge": ["pub fn add_knowledge_references("],
    "tool_activate_skill": [
        "pub struct ActivatedSkill {", "pub fn build_cd_instruction_content(",
        "pub async fn activate_skill_inner(", "pub body: String,",
    ],
    "tool_cron_list": [
        "pub struct ToolCronList {", "pub fn with_stores(", "pub fn set_job_isolated(",
        "pub test_session_store: Option<Arc<dyn CronStore>>,",
    ],
    "tool_cron_delete": [
        "pub struct ToolCronDelete {", "pub fn with_stores(",
        "pub test_session_store: Option<Arc<dyn CronStore>>,",
    ],
    "tool_add_workspace_folder": ["pub struct ToolAddWorkspaceFolder {"],
    "tool_create_knowledge": ["pub struct ToolCreateKnowledge {"],
    "tool_config_subagent": ["pub struct ToolConfigSubagent {"],
}

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(p):
    return p.read_text(encoding="utf-8")


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args],
                          capture_output=True, text=True).stdout.strip()


def pristine(mod):
    out = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:refact-agent/engine/src/tools/%s.rs" % (BASE, mod)],
        capture_output=True, check=True).stdout.decode("utf-8")
    return out.replace("\r\n", "\n")


def split_tests(text):
    lines = text.split("\n")
    for i in range(len(lines) - 1):
        if lines[i].strip() == "#[cfg(test)]" and re.match(r"^(pub )?mod tests\b", lines[i + 1].strip()):
            return lines[:i], lines[i:]
    return lines, []


# 1. crate files exist and carry real production code
for mod in MOVED:
    p = DST / f"{mod}.rs"
    body = read(p) if p.exists() else ""
    check(f"crate file {mod}.rs exists and is non-trivial", p.exists() and len(body) > 200,
          f"{len(body)} bytes")

# 2. no engine-only `crate::` path survives in the crate
for mod in MOVED:
    body = read(DST / f"{mod}.rs")
    stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)
                    if not m.startswith("crate::tools::")})
    check(f"crate {mod}.rs has no engine-only crate:: refs", not stray, str(stray))

# 3. engine-resident symbols now come from owning crates
activate = read(DST / "tool_activate_skill.rs")
for sym in ["refact_ext::config_dirs::ExtDirs", "refact_ext::skills::load_skill_full",
            "refact_ext::skills::validate_skill_id", "refact_ext::skills_context::expand_skill_includes"]:
    check(f"crate tool_activate_skill.rs uses {sym}", sym in activate)
docs = read(DST / "tool_task_documents.rs")
check("crate tool_task_documents.rs uses refact_runtime_api::AppState",
      "refact_runtime_api::AppState" in docs)
check("crate tool_task_documents.rs uses refact_runtime_api::ChatServices",
      "refact_runtime_api::ChatServices" in docs)
phases = read(DST / "subagent_phases.rs")
check("crate subagent_phases.rs uses refact_runtime_api::global_context::GlobalContext",
      "refact_runtime_api::global_context::GlobalContext" in phases)
check("crate subagent_phases.rs uses the crate-local tool_helpers",
      "crate::tools::tool_helpers::" in phases)

# 4. engine files are shims; the ones with tests keep them verbatim
for mod in MOVED:
    body = read(SRC / f"{mod}.rs")
    check(f"engine {mod}.rs starts with the re-export shim",
          body.startswith(f"pub use refact_tools::tools::{mod}::*;"),
          body.splitlines()[0] if body else "")
    _, test = split_tests(body)
    if mod in TESTS:
        check(f"engine {mod}.rs keeps its mod tests block",
              any(l.strip() == "mod tests {" for l in test))
        got = len(re.findall(r"#\[(?:tokio::)?test[\(\]]", body))
        check(f"engine {mod}.rs keeps {TESTS[mod]} tests", got == TESTS[mod], f"got {got}")
        want = split_tests(pristine(mod))[1]
        check(f"engine {mod}.rs test module is verbatim", test == want,
              "test region differs from the pre-move file")
        # a glob re-export does not put trait impls' methods in scope, so the
        # shim prelude has to re-import every trait the tests call methods on.
        # (The prelude may use multi-line `use a::{ b, c };` blocks.)
        prelude = "\n".join(split_tests(body)[0])
        for method, trait in [("tool_execute", "Tool"), ("tool_description", "Tool"),
                              ("tool_ctx", "GlobalContextCtx"), ("chat_ctx", "GlobalContextCtx"),
                              ("app_state", "GlobalContextCtx")]:
            if re.search(r"\.%s\(" % method, "\n".join(test)):
                check(f"engine {mod}.rs prelude imports {trait} (needed by .{method}())",
                      trait in prelude, prelude[:120])
    else:
        check(f"engine {mod}.rs is shim only (no tests)", not test,
              f"{len(test)} trailing lines")

# 5. every item/field the staying tests reach was widened to pub in the crate
for mod, symbols in WIDENED.items():
    body = read(DST / f"{mod}.rs")
    for symbol in symbols:
        check(f"crate {mod}.rs widens `{symbol[:-1].strip()}`", symbol in body)

# 6. production bodies are verbatim apart from the documented edits
REWRITES = [
    ("crate::app_state::ChatServices", "refact_runtime_api::ChatServices"),
    ("crate::app_state::AppState", "refact_runtime_api::AppState"),
    ("crate::global_context::", "refact_runtime_api::global_context::"),
    ("crate::ext::config_dirs::", "refact_ext::config_dirs::"),
    ("crate::ext::skills_context::", "refact_ext::skills_context::"),
    ("crate::ext::skills::", "refact_ext::skills::"),
]
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import importlib.util
from collections import Counter

_spec = importlib.util.spec_from_file_location(
    "n4_move", pathlib.Path(__file__).resolve().parent / "n4-move.py")
mover = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mover)


def behaviour_key(line):
    """Line identity ignoring visibility, cfg gating and surrounding whitespace."""
    s = line.strip()
    if s.startswith("#[cfg(") or s.startswith('//!') or s.startswith("//"):
        return None
    s = re.sub(r"^pub(?:\([a-z_]*\))?\s+", "", s)
    for old, new in REWRITES:
        s = s.replace(old, new)
    return s


ENGINE_ONLY_IMPORTS = {
    "use crate::global_context::GlobalContext;",
    "use refact_runtime_api::global_context::GlobalContext;",
}

for mod in MOVED:
    prod, _ = split_tests(pristine(mod))
    want = Counter(k for k in (behaviour_key(l) for l in prod) if k is not None)
    for drop in ENGINE_ONLY_IMPORTS:
        want.pop(drop, None)
    body = read(DST / f"{mod}.rs")
    got = Counter(k for k in (behaviour_key(l) for l in body.split("\n")) if k is not None)
    for drop in ENGINE_ONLY_IMPORTS:
        got.pop(drop, None)
    only_want = {k: v for k, v in (want - got).items() if v}
    only_got = {k: v for k, v in (got - want).items() if v}
    check(f"crate {mod}.rs keeps every production line (visibility/cfg-insensitive)",
          not only_want, f"missing {list(only_want)[:4]}")
    check(f"crate {mod}.rs adds no production line", not only_got, f"added {list(only_got)[:4]}")

for mod in MOVED:
    prod, test = split_tests(pristine(mod))
    expected = mover.crate_body(mod, prod, "\n".join(test), [])
    got = read(DST / f"{mod}.rs")
    check(f"crate {mod}.rs == mover output (prod + documented edits)",
          expected.rstrip("\n") == got.rstrip("\n"))
    expected_shim = mover.shim(mod, prod, test)
    got_shim = read(SRC / f"{mod}.rs")
    check(f"engine {mod}.rs == mover output (shim + tests)",
          expected_shim.rstrip("\n") == got_shim.rstrip("\n"))

# 7. crate root module table
tools_rs = read(CRATE / "src" / "tools.rs")
for mod in MOVED:
    check(f"crate tools.rs declares pub mod {mod}", f"pub mod {mod};" in tools_rs)

# 8. crate manifest: new deps declared, never refact-lsp
crate_toml = read(CRATE / "Cargo.toml")
check("crate manifest declares refact-ext", 'refact-ext = { path = "../refact-ext" }' in crate_toml)
check("crate manifest declares chrono-tz", 'chrono-tz = "0.10"' in crate_toml)
check("crate manifest never mentions refact-lsp", "refact-lsp" not in crate_toml)

# 8b. Cargo.lock: still CRLF, and the refact-tools dep list carries the new deps
lock_raw = (ENG / "Cargo.lock").read_bytes()
check("Cargo.lock stays pure CRLF",
      lock_raw.count(b"\r\n") == lock_raw.count(b"\n"),
      "CRLF=%d LF=%d" % (lock_raw.count(b"\r\n"), lock_raw.count(b"\n")))
lock = lock_raw.decode("utf-8").replace("\r\n", "\n")
lock_block = lock.split('name = "refact-tools"')[1].split("[[package]]")[0]
lock_deps = re.findall(r"(?m)^ \"([^\"]+)\",$", lock_block)
for dep in ["chrono-tz", "refact-ext"]:
    check(f"Cargo.lock refact-tools block depends on {dep}", dep in lock_deps)
check("Cargo.lock refact-tools block has no refact-lsp", "refact-lsp" not in lock_deps)
for dep in ["chrono-tz", "refact-ext"]:
    check(f'Cargo.lock has a "{dep}" package entry', 'name = "%s"\n' % dep in lock)
check("engine [dependencies] keeps refact-tools",
      'refact-tools = { path = "crates/refact-tools" }' in read(ENG / "Cargo.toml"))

# 9. constructor surface: engine call sites still name the same paths,
#    and each named module is a shim re-exporting the crate module
named = set()
for path in SRC.glob("*.rs"):
    body = read(path)
    for m in re.findall(r"crate::tools::(\w+)::", body):
        named.add(m)
    for m in re.findall(r"(?m)^use super::(\w+)::", body):
        named.add(m)
surface = sorted(n for n in named if n in MOVED)
check("engine call sites still name every moved module", surface == sorted(MOVED),
      f"missing {sorted(set(MOVED) - set(surface))}")
for mod in MOVED:
    check(f"crate::tools::{mod}:: still resolves via a shim",
          read(SRC / f"{mod}.rs").startswith(f"pub use refact_tools::tools::{mod}::*;"))

# 9b. every constructor tools_list.rs reaches for is defined in the crate
tools_list = read(SRC / "tools_list.rs")
for mod, ctor in [
    ("tool_task_documents", "ToolDocList"), ("tool_task_documents", "ToolDocGet"),
    ("tool_task_documents", "ToolDocCreate"), ("tool_task_documents", "ToolDocUpdate"),
    ("tool_task_documents", "ToolDocAppend"), ("tool_task_documents", "ToolDocDelete"),
    ("tool_task_documents", "ToolDocPin"), ("tool_task_documents", "ToolDocHistory"),
    ("tool_task_memory", "ToolTaskMemorySave"), ("tool_task_memory", "ToolTaskMemoriesGet"),
    ("tool_task_memory", "ToolTaskMemorySearch"), ("tool_task_memory", "ToolTaskMemoryPin"),
    ("tool_task_memory", "ToolTaskMemoryArchive"), ("tool_task_memory", "ToolTaskMemoryUnarchive"),
    ("tool_task_memory", "ToolTaskMemoryInbox"), ("tool_task_memory", "ToolTaskMemoryTriageDone"),
    ("tool_knowledge", "ToolGetKnowledge"), ("tool_create_knowledge", "ToolCreateKnowledge"),
    ("tool_add_workspace_folder", "ToolAddWorkspaceFolder"),
    ("tool_activate_skill", "ToolActivateSkill"), ("tool_activate_skill", "ToolDeactivateSkill"),
    ("tool_config_subagent", "ToolConfigSubagent"),
    ("tool_cron_list", "ToolCronList"), ("tool_cron_delete", "ToolCronDelete"),
]:
    used = f"crate::tools::{mod}::{ctor}" in tools_list or (
        ctor in tools_list and f"use super::{mod}::" in tools_list)
    check(f"tools_list.rs still constructs {ctor}", used)
    check(f"crate {mod}.rs defines pub struct {ctor}",
          f"pub struct {ctor}" in read(DST / f"{mod}.rs"))

# 10. the two K11 files that cannot move are untouched
dirty = git("status", "--porcelain").splitlines()
touched = {line.strip().split(None, 1)[1].replace("\\", "/")
           for line in dirty if line.strip().split(None, 1)[1:]}
for mod in LEFT_IN_ENGINE:
    check(f"src/tools/{mod}.rs untouched (stays in the engine)",
          f"refact-agent/engine/src/tools/{mod}.rs" not in touched)
for mod in ["mod", "tools_list", "tool_shell", "shell_gate"]:
    check(f"src/tools/{mod}.rs untouched",
          f"refact-agent/engine/src/tools/{mod}.rs" not in touched)
check("engine Cargo.toml untouched", "refact-agent/engine/Cargo.toml" not in touched)

allowed_prefixes = (
    "refact-agent/engine/src/tools/",
    "refact-agent/engine/crates/refact-tools/",
    "tools/dev/n4-move.py",
    "tools/dev/n4-check.py",
    "tools/dev/n4-audit.py",
    "tools/dev/n4-widen-spec.py",
    "tools/dev/n4-lock-check.py",
    "refact-agent/engine/Cargo.lock",
)
extra = [t for t in sorted(touched) if not t.startswith(allowed_prefixes)]
check("no unrelated file touched", not extra, str(extra))

failed = [c for c in checks if not c[1]]
print(f"\n{len(checks) - len(failed)}/{len(checks)} checks passed")
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
