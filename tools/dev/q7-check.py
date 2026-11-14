"""Q-7 verification harness: static proof that the three cross-cutting seams are settled.

No build is attempted - `~/.cargo/refact-build-guard.exe` refuses compilation from
agent worktrees; the planner owns the engine build. Every check is a filesystem /
text / git-blob comparison, and the harness command has no shell separator:

    python tools/dev/q7-check.py
"""
import pathlib
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src"
AGENTS_CORE = ENG / "crates" / "refact-agents-core" / "src"
RUNTIME_API = ENG / "crates" / "refact-runtime-api" / "src"

BASE = "ac177abe7"

checks = []


def check(name, ok, detail=""):
    checks.append((name, bool(ok), detail))
    print("%-6s %s%s" % ("ok" if ok else "FAIL", name, ("  -- " + detail) if detail else ""))


def read(path):
    return pathlib.Path(path).read_text(encoding="utf-8").replace("\r\n", "\n")


def base_text(rel):
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:refact-agent/engine/%s" % (BASE, rel)],
        capture_output=True,
        check=True,
    ).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def prod_region(text):
    marker = text.find("#[cfg(test)]")
    return text if marker == -1 else text[:marker]


def test_region(text):
    marker = text.find("#[cfg(test)]")
    return "" if marker == -1 else text[marker:]


def literals(text):
    return sorted(set(re.findall(r'"((?:[^"\\]|\\.)*)"', text)))


# ------------------------------------------------------------------ 1. mode_transition
print("=== 1. agentic::mode_transition::reconstruct_context")
mt_engine = read(SRC / "agentic" / "mode_transition.rs")
mt_runtime = read(RUNTIME_API / "mode_transition_runtime.rs")
check(
    "engine agentic/mode_transition.rs is a pure shim (no production fn)",
    not re.search(r"(?m)^(pub )?(async )?fn ", prod_region(mt_engine)),
)
check(
    "engine shim re-exports the crate body symbols",
    "pub use refact_agentic::mode_transition::{" in mt_engine,
)
check(
    "engine shim routes reconstruct_context through refact_runtime_api",
    "pub use refact_runtime_api::mode_transition_runtime::{" in mt_engine
    and "reconstruct_context," in mt_engine,
)
check(
    "refact-runtime-api owns reconstruct_context",
    "pub async fn reconstruct_context(" in mt_runtime,
)
check(
    "refact-runtime-api owns analyze_mode_transition and assemble_new_chat",
    "pub async fn analyze_mode_transition(" in mt_runtime
    and "pub async fn assemble_new_chat<" in mt_runtime,
)
base_mt = base_text("src/agentic/mode_transition.rs")
check(
    "engine mode_transition inline tests unchanged since the base commit",
    test_region(base_mt).rstrip("\n") == test_region(mt_engine).rstrip("\n"),
)

# ------------------------------------------------------------------ 2. memory_lifecycle seam
print("\n=== 2. buddy::memory_lifecycle::apply_memory_lifecycle_op_status")
ml = read(SRC / "buddy" / "memory_lifecycle.rs")
mlr = read(RUNTIME_API / "memory_lifecycle_runtime.rs")
check(
    "MemoryLifecycleRuntime gains apply_memory_lifecycle_op_status_for_ctx",
    "async fn apply_memory_lifecycle_op_status_for_ctx(" in mlr,
)
check(
    "crate exposes the matching free fn routing through ctx.memory_lifecycle",
    "pub async fn apply_memory_lifecycle_op_status_for_ctx(" in mlr
    and "ctx.memory_lifecycle" in mlr,
)
check(
    "engine impl of MemoryLifecycleRuntime delegates the new method",
    "impl refact_runtime_api::memory_lifecycle_runtime::MemoryLifecycleRuntime" in ml
    and ml.count("async fn apply_memory_lifecycle_op_status_for_ctx(") == 2,
)
check(
    "engine wrapper apply_memory_lifecycle_op_status routes through the seam",
    "refact_runtime_api::memory_lifecycle_runtime::apply_memory_lifecycle_op_status_for_ctx("
    in ml,
)
check(
    "apply_memory_lifecycle_op_status keeps its AppState -> MemoryLifecycleOp signature",
    re.search(
        r"pub async fn apply_memory_lifecycle_op_status\(\s*app: AppState,\s*op: &MemoryLifecycleOp,\s*\) -> MemoryLifecycleOp \{",
        ml,
    )
    is not None,
)
base_ml = base_text("src/buddy/memory_lifecycle.rs")
check(
    "the private _for_ctx body is byte-identical to the base commit",
    re.search(
        r"(?m)^async fn apply_memory_lifecycle_op_status_for_ctx\(.*?\n\}\n",
        prod_region(base_ml),
        re.S,
    ).group(0)
    == re.search(
        r"(?m)^async fn apply_memory_lifecycle_op_status_for_ctx\(.*?\n\}\n",
        prod_region(ml),
        re.S,
    ).group(0),
)
check(
    "every pre-existing production fn of memory_lifecycle.rs is still present",
    all(
        (("fn %s(" % name) in prod_region(ml) or ("fn %s<" % name) in prod_region(ml))
        for name in re.findall(r"(?m)^(?:pub )?(?:async )?fn ([A-Za-z_][A-Za-z0-9_]*)", prod_region(base_ml))
    ),
)
check(
    "consumers keep the engine path (buddy/storage.rs + inline tests)",
    "use super::memory_lifecycle::{apply_memory_lifecycle_op_status, MemoryOpsState};"
    in read(SRC / "buddy" / "storage.rs")
    and "apply_memory_lifecycle_op_status(gcx" in read(SRC / "buddy" / "storage.rs"),
)
check(
    "EngineMemoryLifecycleRuntime is the installed ChatCtx impl",
    "crate::buddy::memory_lifecycle::EngineMemoryLifecycleRuntime" in read(SRC / "app_state.rs"),
)

# ------------------------------------------------------------------ 3. agents monitor move
print("\n=== 3. agents::monitor -> refact-agents-core")
crate_monitor = read(AGENTS_CORE / "monitor.rs")
engine_monitor = read(SRC / "agents" / "monitor.rs")
check("crate agents-core/monitor.rs exists", "pub mod monitor;" in read(AGENTS_CORE / "lib.rs"))
check(
    "crate owns run_background_agent_monitor and monitor_once",
    "pub async fn run_background_agent_monitor<H: AgentHost>(" in crate_monitor
    and "pub async fn monitor_once<H: AgentHost>(" in crate_monitor,
)
check(
    "engine agents/monitor.rs is a re-export shim with no production fn",
    "pub use refact_agents_core::monitor::{monitor_once, run_background_agent_monitor};"
    in engine_monitor
    and not re.search(r"(?m)^(pub )?(async )?fn ", prod_region(engine_monitor)),
)
check(
    "crate monitor has no engine-only crate:: ref",
    not re.search(r"\bcrate::(?!host|push|spawn|types)\w+", crate_monitor),
)
base_mon = base_text("src/agents/monitor.rs")
check(
    "engine shim test module keeps `use super::*;` as its first prelude line",
    test_region(engine_monitor).splitlines()[1:3]
    == ["mod tests {", "    use super::*;"],
)
check(
    "moved inline test bodies are byte-identical in the engine shim",
    test_region(base_mon)[test_region(base_mon).index("#[tokio::test]"):].rstrip("\n")
    == test_region(engine_monitor)[test_region(engine_monitor).index("#[tokio::test]"):].rstrip("\n"),
)
check(
    "every moved string literal is preserved verbatim",
    literals(prod_region(base_mon)) == literals(prod_region(crate_monitor)),
    "base=%r crate=%r" % (literals(prod_region(base_mon)), literals(prod_region(crate_monitor))),
)
for const in ["MONITOR_INTERVAL: Duration = Duration::from_secs(60)",
              "STUCK_AFTER: TimeDelta = TimeDelta::minutes(20)"]:
    check("crate monitor keeps `%s`" % const, const in crate_monitor)
check(
    "background_tasks.rs still calls the same engine path",
    crate_monitor.count("20+ minutes") == 1
    and "crate::agents::monitor::run_background_agent_monitor(" in read(SRC / "background_tasks.rs"),
)

# ------------------------------------------------------------------ 4. dependency direction
print("\n=== 4. dependency direction (no cycle)")
for crate in ["refact-agents-core", "refact-runtime-api"]:
    toml = read(ENG / "crates" / crate / "Cargo.toml")
    check("%s manifest never names refact-lsp" % crate, "refact-lsp" not in toml)
check(
    "refact-agents-core keeps no dependency on refact-runtime-api",
    "refact-runtime-api" not in read(ENG / "crates" / "refact-agents-core" / "Cargo.toml"),
)
check(
    "refact-runtime-api depends on refact-agents-core (one-way edge preserved)",
    "refact-agents-core = { path = " in read(ENG / "crates" / "refact-runtime-api" / "Cargo.toml"),
)
check(
    "refact-agents-core declares tracing + tokio macros for the monitor",
    "tracing.workspace = true" in read(ENG / "crates" / "refact-agents-core" / "Cargo.toml")
    and 'features = ["macros", "sync", "time"]' in read(ENG / "crates" / "refact-agents-core" / "Cargo.toml"),
)

# ------------------------------------------------------------------ 5. scope
print("\n=== 5. change scope")
status = subprocess.run(
    ["git", "-C", str(ROOT), "diff", "--name-only", BASE],
    capture_output=True,
    text=True,
    encoding="utf-8",
).stdout
untracked = subprocess.run(
    ["git", "-C", str(ROOT), "ls-files", "--others", "--exclude-standard"],
    capture_output=True,
    text=True,
    encoding="utf-8",
).stdout
touched = sorted(
    {line.strip().replace("\\", "/") for line in (status + untracked).splitlines() if line.strip()}
)
allowed = (
    "refact-agent/engine/src/agents/monitor.rs",
    "refact-agent/engine/src/buddy/memory_lifecycle.rs",
    "refact-agent/engine/crates/refact-agents-core/",
    "refact-agent/engine/crates/refact-runtime-api/src/memory_lifecycle_runtime.rs",
    "refact-agent/engine/Cargo.lock",
    "tools/dev/q7-check.py",
)
stray = [t for t in touched if not t.startswith(allowed)]
check("no file outside the card's scope changed", not stray, str(stray))

# ------------------------------------------------------------------ 6. parse check
print("\n=== 6. parse check")
rustfmt = subprocess.run(["rustfmt", "--version"], capture_output=True, text=True)
if rustfmt.returncode == 0:
    for path in [
        SRC / "agents" / "monitor.rs",
        SRC / "buddy" / "memory_lifecycle.rs",
        SRC / "agentic" / "mode_transition.rs",
        AGENTS_CORE / "monitor.rs",
        AGENTS_CORE / "lib.rs",
        RUNTIME_API / "memory_lifecycle_runtime.rs",
    ]:
        result = subprocess.run(
            ["rustfmt", "--edition", "2021", "--emit", "stdout", str(path)],
            capture_output=True,
            text=True,
        )
        check("%s parses" % path.name, result.returncode == 0, (result.stderr or result.stdout)[:200])
else:
    check("rustfmt unavailable, parse check skipped", True, "")

# ------------------------------------------------------------------ 7. live census for agents
print("\n=== 7. live census: agents::monitor is no longer blocked")
built = subprocess.run(
    [sys.executable, str(ROOT / "tools" / "dev" / "chat_movable_census.py"), "agents"],
    cwd=str(ROOT),
    capture_output=True,
    text=True,
    encoding="utf-8",
)
print((built.stdout or built.stderr).rstrip())
census = read(ENG / "target" / "agents_movable_census.txt")
left = census.split("### LEFT / ISLANDS", 1)[-1]
check("census runs", built.returncode == 0, (built.stderr or "")[:200])
check("agents::monitor no longer appears as blocked", "agents::monitor" not in left)
check(
    "agents::spawn is the only remaining agents blocker (subchat island)",
    "agents::spawn" in left and left.count("blocker=") == 1,
)

# ------------------------------------------------------------------ verdict table
print("\n=== 8. decision table (deliverable summary)")
rows = [
    ("agentic::mode_transition::reconstruct_context",
     "already settled by K-2; engine file is a pure shim",
     "report only"),
    ("buddy::memory_lifecycle::apply_memory_lifecycle_op_status",
     "trait seam on MemoryLifecycleRuntime + crate free fn",
     "1 trait method + 1 free fn in refact-runtime-api; engine wrapper delegates"),
    ("agents::monitor (run_background_agent_monitor / monitor_once)",
     "generic AgentHost body moved verbatim to refact-agents-core::monitor",
     "engine file is a re-export shim; inline tests stay in the engine"),
    ("agents::spawn (918 lines)",
     "LEFT BEHIND - the body still reaches the engine subchat island "
     "(resolve_subchat_config_with_parent / run_subchat) and GlobalContext worktrees",
     "own card, as the master plan predicts"),
]
for symbol, why, note in rows:
    print("  * %s\n      -> %s\n      -> %s" % (symbol, why, note))

failed = [c for c in checks if not c[1]]
print("\n%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
