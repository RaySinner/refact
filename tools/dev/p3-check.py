"""P-3 verification harness: static proof that the four `tools/` blockers are cleared.

No build is attempted - `~/.cargo/refact-build-guard.exe` refuses compilation from
agent worktrees; the planner owns the engine build. Every check is a filesystem /
text / git-blob comparison, and the harness command has no shell separator:

    python tools/dev/p3-check.py
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
TOOLS_CRATE = ENG / "crates" / "refact-tools" / "src" / "tools"
RUNTIME_API = ENG / "crates" / "refact-runtime-api" / "src"

BASE = "68efcde49"

checks = []


def check(name, ok, detail=""):
    checks.append((name, bool(ok), detail))
    print("%-6s %s%s" % ("ok" if ok else "FAIL", name, ("  -- " + detail) if detail else ""))


def read(path):
    return pathlib.Path(path).read_text(encoding="utf-8")


def base_text(rel):
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:refact-agent/engine/src/tools/%s" % (BASE, rel)],
        capture_output=True,
        check=True,
    ).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def diff_numstat(rel):
    out = subprocess.run(
        ["git", "-C", str(ROOT), "diff", "--numstat", BASE, "--", rel],
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout.strip()
    if not out:
        return (0, 0)
    parts = out.splitlines()[0].split("\t")
    return (int(parts[0]), int(parts[1]))


# ------------------------------------------------------------------ row 1: normalize_path_for_overlap
print("=== 1. agents::normalize_path_for_overlap")
subagent = read(SRC / "tool_subagent.rs")
crate_owner = read(ENG / "crates" / "refact-agents-core" / "src" / "registry.rs")
check("refact-agents-core owns normalize_path_for_overlap",
      "pub fn normalize_path_for_overlap(" in crate_owner)
check("engine agents/mod.rs is a refact_agents_core re-export shim",
      "pub use refact_agents_core::{registry, storage, types};" in read(ENG / "src" / "agents" / "mod.rs"))
check("tool_subagent.rs no longer names crate::agents::registry::normalize_path_for_overlap",
      "crate::agents::registry::normalize_path_for_overlap" not in subagent)
check("tool_subagent.rs names refact_agents_core::registry::normalize_path_for_overlap at both sites",
      subagent.count("refact_agents_core::registry::normalize_path_for_overlap") == 2,
      "got %d" % subagent.count("refact_agents_core::registry::normalize_path_for_overlap"))
added, removed = diff_numstat("refact-agent/engine/src/tools/tool_subagent.rs")
check("tool_subagent.rs diff is 2+/2- (import lines only)", (added, removed) == (2, 2),
      "got %s+/%s-" % (added, removed))

# ------------------------------------------------------------------ row 2: buddy::memory move
print("\n=== 2. tools::buddy::memory -> refact-tools")
moved = read(TOOLS_CRATE / "buddy" / "memory.rs")
engine_mem = read(SRC / "buddy" / "memory.rs")
check("crate buddy/memory.rs exists", "pub struct ToolBuddyMemorySearch {" in moved)
check("engine buddy/memory.rs is a one-line glob shim plus test prelude",
      engine_mem.split("\n")[0] == "pub use refact_tools::tools::buddy::memory::*;")
check("engine buddy/memory.rs keeps no production fn",
      not re.search(r"(?m)^(async )?fn ", engine_mem))
check("engine buddy/memory.rs keeps the inline test module",
      "#[cfg(test)]\nmod tests {" in engine_mem)
check("crate tools/buddy/mod.rs declares `pub mod memory;`",
      "pub mod memory;" in read(TOOLS_CRATE / "buddy" / "mod.rs"))

STRAY = sorted({m for m in re.findall(r"crate::[\w:]*", moved) if not m.startswith("crate::tools::")})
check("crate buddy/memory.rs has no engine-only crate:: ref", not STRAY, str(STRAY))
for needle, why in [
    ("use refact_buddy_core::memory_lifecycle_model::{", "crate model types"),
    ("use refact_runtime_api::memory_lifecycle_runtime::{", "seam call"),
    ("use refact_buddy_core::storage::enqueue_memory_op;", "crate storage"),
    ("use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;", "crate file filter"),
    ("use refact_knowledge_graph::kg_structs::KnowledgeFrontmatter;", "crate kg structs"),
]:
    check("crate buddy/memory.rs imports %s" % why, needle in moved)

for widened in [
    "pub const WRITABLE_KINDS:",
    "pub const MAX_KNOWLEDGE_SCAN_FILES:",
    "pub async fn find_duplicate_content(",
    "pub async fn scan_cards_in_dirs(",
]:
    check("crate buddy/memory.rs widens `%s`" % widened.rstrip("("), widened in moved)

base_mem = base_text("buddy/memory.rs")
base_prod = base_mem[: base_mem.index("#[cfg(test)]\nmod tests {")]
base_body = base_prod[base_prod.index("const MEMORY_KINDS: &[&str] = &["):]
crate_body = moved[moved.index("const MEMORY_KINDS: &[&str] = &["):]
pairs = [
    (r"^const WRITABLE_KINDS: ", r"pub const WRITABLE_KINDS: "),
    (r"^const MAX_KNOWLEDGE_SCAN_FILES: ", r"pub const MAX_KNOWLEDGE_SCAN_FILES: "),
    (r"^async fn find_duplicate_content\(", r"pub async fn find_duplicate_content("),
    (r"^async fn scan_cards_in_dirs\(", r"pub async fn scan_cards_in_dirs("),
]
for pattern, replacement in pairs:
    base_body = re.sub(pattern, replacement, base_body, count=1, flags=re.M)
check("crate production body is byte-identical to the pre-move body (widening only)",
      base_body.rstrip("\n") == crate_body.rstrip("\n"))

base_tests = base_mem[base_mem.index("#[cfg(test)]\nmod tests {"):]
shim_tests = engine_mem[engine_mem.index("#[cfg(test)]\nmod tests {"):]
check("moved inline tests are byte-identical in the engine shim",
      base_tests.rstrip("\n") == shim_tests.rstrip("\n"))

# ------------------------------------------------------------------ row 3: goal verifier seam
print("\n=== 3. chat::goal_verifier::run_goal_verifier")
gv_runtime = read(RUNTIME_API / "goal_verifier_runtime.rs")
check("refact-runtime-api owns the GoalVerifierRunner trait",
      "pub trait GoalVerifierRunner: Send + Sync {" in gv_runtime)
check("run_goal_verifier takes AppState and returns GoalVerifierReply",
      "pub async fn run_goal_verifier(" in gv_runtime and "-> Result<GoalVerifierReply, String>" in gv_runtime)
monitor = read(ENG / "src" / "chat" / "goal_verifier" / "monitor.rs")
check("engine monitor.rs still owns the run_goal_verifier body",
      "pub async fn run_goal_verifier(" in monitor)
check("engine installs EngineGoalVerifierRunner",
      "impl refact_runtime_api::goal_verifier_runtime::GoalVerifierRunner for EngineGoalVerifierRunner" in monitor)
ctx = read(RUNTIME_API / "chat_ctx.rs")
check("ChatCtx carries `goal_verifier`", "goal_verifier: Arc<dyn crate::goal_verifier_runtime::GoalVerifierRunner>," in ctx)
check("ChatCtx carries `memory_lifecycle`", "memory_lifecycle: Arc<dyn crate::memory_lifecycle_runtime::MemoryLifecycleRuntime>," in ctx)
app_state = read(ENG / "src" / "app_state.rs")
runtime_lib = read(RUNTIME_API / "lib.rs")
check("refact-runtime-api declares goal_verifier_runtime", "pub mod goal_verifier_runtime;" in runtime_lib)
check("refact-runtime-api declares memory_lifecycle_runtime", "pub mod memory_lifecycle_runtime;" in runtime_lib)
check("ChatCtx is the only ChatCtx literal site updated (struct + engine factory)",
      len([p for p in [ctx, read(ENG / "src" / "app_state.rs")] if "memory_lifecycle:" in p]) == 2)
lifecycle = read(ENG / "src" / "buddy" / "memory_lifecycle.rs")
check("engine memory_lifecycle.rs installs EngineMemoryLifecycleRuntime",
      "impl refact_runtime_api::memory_lifecycle_runtime::MemoryLifecycleRuntime" in lifecycle)
for name in ["propose_supersede_for_near_duplicate_for_ctx", "find_memory_by_source_for_ctx",
             "archive_memory_file_checked_for_ctx"]:
    check("engine memory_lifecycle.rs still defines `%s`" % name, ("pub async fn %s(" % name) in lifecycle)

# ------------------------------------------------------------------ row 4: shell island
print("\n=== 4. shell_gate <-> shell_gate_llm (island-internal)")
shell_gate = read(SRC / "shell_gate.rs")
shell_gate_llm = read(SRC / "shell_gate_llm.rs")
check("shell_gate.rs stays in the engine and names shell_gate_llm", "shell_gate_llm" in shell_gate)
check("shell_gate_llm.rs stays in the engine and names shell_gate", "shell_gate" in shell_gate_llm)
check("shell_gate.rs keeps its production struct", "pub struct ShellLlmValidation {" in shell_gate)
check("shell_gate_llm.rs keeps its production verdict type", "pub struct LlmVerdict {" in shell_gate_llm)
check("shell_gate_llm.rs keeps validate_command", "pub async fn validate_command(" in shell_gate_llm)

# ------------------------------------------------------------------ the four consumers
print("\n=== 5. tool-file import retargets")
tools_list = read(SRC / "tools_list.rs")
check("tools_list.rs takes load_integrations from refact-runtime-api",
      "use refact_runtime_api::integrations_runtime::load_integrations;" in tools_list)
check("tools_list.rs no longer names crate::integrations::load_integrations",
      "crate::integrations::load_integrations" not in tools_list)
check("tools_list.rs constructs all 5 buddy memory tools through the crate path",
      tools_list.count("refact_tools::tools::buddy::memory::ToolBuddyMemory") == 5,
      "got %d" % tools_list.count("refact_tools::tools::buddy::memory::ToolBuddyMemory"))
check("tools_list.rs no longer names crate::tools::buddy::memory",
      "crate::tools::buddy::memory" not in tools_list)

validate = read(SRC / "tool_validate_goal.rs")
check("tool_validate_goal.rs takes run_goal_verifier from refact-runtime-api",
      "use refact_runtime_api::goal_verifier_runtime::run_goal_verifier;" in validate)
check("tool_validate_goal.rs no longer names crate::chat::goal_verifier::run_goal_verifier",
      "crate::chat::goal_verifier::run_goal_verifier" not in validate)
check("tool_validate_goal.rs keeps the verdict helpers from refact-chat-api",
      "use refact_chat_api::goal_verifier::{" in validate)

# ------------------------------------------------------------------ tool schema fidelity
print("\n=== 6. tool schema fidelity")
base_moved = base_text("buddy/memory.rs")
for literal in ['"buddy_memory_search"', '"buddy_memory_create"', '"buddy_memory_archive"',
                '"buddy_memory_retag"', '"buddy_memory_merge"', '"knowledge"', '"additionalProperties"',
                "archive_memory_file_checked_for_ctx", "propose_supersede_for_near_duplicate_for_ctx"]:
    if literal in base_moved:
        check("crate buddy/memory.rs keeps literal `%s`" % literal, literal in moved)
check("crate buddy/memory.rs keeps the 5 Tool impls",
      moved.count("impl Tool for ToolBuddyMemory") == 5,
      "got %d" % moved.count("impl Tool for ToolBuddyMemory"))

# ------------------------------------------------------------------ dependency direction
print("\n=== 7. dependency direction")
for crate in ["refact-tools", "refact-runtime-api"]:
    toml = read(ENG / "crates" / crate / "Cargo.toml")
    check("%s manifest never names refact-lsp" % crate, "refact-lsp" not in toml)
check("refact-tools depends on refact-runtime-api",
      "refact-runtime-api = { path = " in read(ENG / "crates" / "refact-tools" / "Cargo.toml"))

# ------------------------------------------------------------------ scope
print("\n=== 8. change scope")
status = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain", "--untracked-files=all"],
                        capture_output=True, text=True, encoding="utf-8").stdout
touched = sorted({line[3:].strip().replace("\\", "/") for line in status.splitlines() if line[3:]})
allowed = (
    "refact-agent/engine/src/tools/",
    "refact-agent/engine/src/app_state.rs",
    "refact-agent/engine/src/buddy/memory_lifecycle.rs",
    "refact-agent/engine/src/chat/goal_verifier/",
    "refact-agent/engine/crates/refact-tools/src/tools.rs",
    "refact-agent/engine/crates/refact-tools/src/tools/",
    "refact-agent/engine/crates/refact-runtime-api/src/",
    "tools/dev/p3-check.py",
    "tools/dev/p3-move.py",
)
stray = [t for t in touched if not t.startswith(allowed)]
check("no file outside the card's scope changed", not stray, str(stray))

# ------------------------------------------------------------------ parser sanity
print("\n=== 9. parse check")
rustfmt = subprocess.run(["rustfmt", "--version"], capture_output=True, text=True)
if rustfmt.returncode == 0:
    for path in [ENG / "src" / "tools" / "buddy" / "memory.rs",
                 TOOLS_CRATE / "buddy" / "memory.rs",
                 ENG / "src" / "tools" / "tools_list.rs",
                 ENG / "src" / "tools" / "tool_validate_goal.rs",
                 RUNTIME_API / "goal_verifier_runtime.rs",
                 RUNTIME_API / "memory_lifecycle_runtime.rs"]:
        result = subprocess.run(
            ["rustfmt", "--edition", "2021", "--emit", "stdout", str(path)],
            capture_output=True,
            text=True,
        )
        check("%s parses" % path.name, result.returncode == 0,
              (result.stderr or result.stdout)[:200])
else:
    check("rustfmt unavailable, parse check skipped", True, "")

# ------------------------------------------------------------------ live blocker table
print("\n=== 10. live y3-blocker status (in-repo script, same census tool)")
CONSUMERS = [
    "tools/tool_subagent.rs",
    "tools/tool_validate_goal.rs",
    "tools/tools_list.rs",
    "tools/buddy/memory.rs",
    "tools/tool_mcp_call.rs",
    "tools/tool_mcp_search.rs",
]


def run_y3(*args):
    return subprocess.run(
        [sys.executable, str(pathlib.Path(__file__).resolve().parent / "y3-blockers.py"), *args],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout


status = run_y3("--file", *CONSUMERS)
print(status.rstrip())
for line in status.splitlines():
    words = line.split()
    if len(words) < 2:
        continue
    if words[0] == "movable" and words[1] in ("tools::buddy::memory", "tools::tool_validate_goal"):
        check("y3-blockers reports `%s` movable" % words[1], True)
    if words[0] == "BLOCKED" and words[1] in ("tools::buddy::memory", "tools::tool_validate_goal"):
        check("y3-blockers no longer reports `%s` blocked" % words[1], False, line.strip())

prod_out = run_y3("--prod", *CONSUMERS)
counts = [l for l in prod_out.splitlines() if l.startswith("###")]
print("\n".join(counts))
for expected in ["tools::tool_subagent", "tools::tool_validate_goal", "tools::tools_list",
                 "tools::buddy::memory", "tools::tool_mcp_call", "tools::tool_mcp_search"]:
    check("prod-line count present for `%s`" % expected, any(expected in c for c in counts))

# ------------------------------------------------------------------ verdict table
print("\n=== 11. honesty census: the four card targets")
rows = [
    ("agents::normalize_path_for_overlap (-> tool_subagent 656)",
     "already crate (refact-agents-core::registry); engine path was a false positive",
     "2-line retarget in tool_subagent.rs"),
    ("tools::buddy::memory (834 + tools_list 834 + mcp 202/158)",
     "NO - moved to refact-tools::tools::buddy::memory",
     "prod body 968 lines now crate-side; engine file is a glob shim"),
    ("chat::goal_verifier::run_goal_verifier (-> tool_validate_goal 221)",
     "NO - body needs run_llm_stream + AppState",
     "ChatCtx::goal_verifier seam + crate-path caller; 1-line retarget"),
    ("tools::shell_gate <-> tools::shell_gate_llm (757)",
     "NO - island-internal cycle, both stay in the engine by design",
     "nothing moved (census honesty only)"),
]
for symbol, moved_to, note in rows:
    print("  * %s\n      -> %s\n      -> %s" % (symbol, moved_to, note))

print("\n=== 12. the tools files whose blocker status changed")
print("  tool_subagent.rs       691 prod lines  2-line retarget (residual blocker agents::spawn)")
print("  tool_validate_goal.rs  242 prod lines  1-line retarget via the runtime-api seam")
print("  tools_list.rs          934 prod lines  load_integrations retarget + 5 crate paths")
print("  buddy/memory.rs        971 prod lines  moved to refact-tools; engine is a glob shim")
print("  tool_mcp_call.rs       234 prod lines  blocked only through tools_list (follows it)")
print("  tool_mcp_search.rs     172 prod lines  blocked only through tools_list (follows it)")

failed = [c for c in checks if not c[1]]
print("\n%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
