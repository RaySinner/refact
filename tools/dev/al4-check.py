#!/usr/bin/env python3
"""AL-4 structural checks: chat::goal_verifier::monitor split + chat::tools whole move.

Layer A of the AL-4 verification (the compiler layer is owned by the project
build guard; see tools/dev/al4-verify.ps1). Every check is a structural fact
about the tree, so it runs anywhere without cargo.

Usage:  python tools/dev/al4-check.py
Exit:   0 = all checks pass, 1 = at least one check failed.
"""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATES = os.path.join(ENGINE, "crates")

ENGINE_TOOLS = os.path.join(ENGINE, "src", "chat", "tools.rs")
CRATE_TOOLS = os.path.join(CRATES, "refact-tools", "src", "chat_tools.rs")
CRATE_TOOLS_LIB = os.path.join(CRATES, "refact-tools", "src", "lib.rs")
CRATE_TOOLS_CARGO = os.path.join(CRATES, "refact-tools", "Cargo.toml")

ENGINE_MONITOR = os.path.join(ENGINE, "src", "chat", "goal_verifier", "monitor.rs")
ENGINE_GV_MOD = os.path.join(ENGINE, "src", "chat", "goal_verifier", "mod.rs")
CRATE_MONITOR = os.path.join(CRATES, "refact-runtime-api", "src", "goal_verifier_monitor.rs")
RUNTIME_API_LIB = os.path.join(CRATES, "refact-runtime-api", "src", "lib.rs")
RUNTIME_API_CARGO = os.path.join(CRATES, "refact-runtime-api", "Cargo.toml")

CENSUS_OUT = os.path.join(ENGINE, "target", "chat_movable_census.txt")

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))


def read(path):
    with open(path, encoding="utf-8") as fh:
        return fh.read()


def strip_code(text):
    text = re.sub(r'"(?:\\.|[^"\\])*"', '""', text)
    text = re.sub(r"(?m)//.*$", "", text)
    return re.sub(r"/\*.*?\*/", "", text, flags=re.S)


def prod_part(text):
    """Everything outside a `#[cfg(test)]` block, with each block's body removed."""
    out = []
    for line in text.splitlines():
        if re.match(r"^\s*#\[cfg\(test\)\]\s*$", line):
            break
        out.append(line)
    return "\n".join(out)


# ---------------------------------------------------------------- the shim

shim = read(ENGINE_TOOLS)
check(
    "engine src/chat/tools.rs is a glob shim",
    shim.lstrip().startswith("pub use refact_tools::chat_tools::*;"),
    shim.lstrip().splitlines()[0],
)
check(
    "engine shim keeps no production item definition",
    not re.search(
        r"(?m)^(pub\s+)?(async\s+)?(fn|struct|enum|impl|const|static)\s",
        prod_part(strip_code(shim)),
    ),
    "found a production item definition in the shim",
)
check(
    "engine shim keeps the cfg(test) modules",
    all(f"mod {name}" in shim for name in ("tests", "interruptible_wait_tests")),
    "make_test_gcx-dependent test modules must stay engine-side",
)
check(
    "make_test_gcx stays out of the crate",
    "make_test_gcx" in shim and "make_test_gcx" not in read(CRATE_TOOLS),
    "make_test_gcx must live only in the engine test surface",
)

# ------------------------------------------------------------- the crate body

crate = read(CRATE_TOOLS)
crate_prod = prod_part(strip_code(crate))
crate_text_prod = prod_part(crate)
for symbol in (
    "pub async fn process_tool_calls_once",
    "pub async fn execute_tools(",
    "pub async fn execute_tools_with_session",
    "pub async fn check_tools_confirmation",
    "pub async fn resolve_tool_call_aliases",
    "pub fn resolve_tool_call_aliases_with_catalog",
    "pub enum ToolStepOutcome",
):
    check(f"crate exports {symbol.split()[-1].split('(')[0]}", symbol in crate_text_prod, symbol)

check(
    "crate production body carries no engine path",
    not re.search(r"\b(?:crate|super)::", crate_prod),
    "; ".join(sorted(set(re.findall(r"\b(?:crate|super)::[A-Za-z_:]+", crate_prod))))[:300],
)

check(
    "refact-tools lib declares mod chat_tools",
    re.search(r"(?m)^pub mod chat_tools;", read(CRATE_TOOLS_LIB)) is not None,
    "missing module declaration",
)
check(
    "refact-tools does not depend on refact-lsp",
    "refact-lsp" not in read(CRATE_TOOLS_CARGO),
    "crate->engine edge",
)
check(
    "refact-tools depends on refact-llm",
    re.search(r"(?m)^refact-llm\s*=", read(CRATE_TOOLS_CARGO)) is not None,
    "chat_tools.rs uses refact_llm::adapters::claude_code_compat",
)
lock = os.path.join(ENGINE, "Cargo.lock")
lock_block = ""
for block in read(lock).split("[[package]]"):
    if 'name = "refact-tools"' in block:
        lock_block = block
        break
check(
    "Cargo.lock records the refact-tools -> refact-llm edge",
    '"refact-llm",' in lock_block,
    "path dependency missing from the lockfile entry",
)

ENGINE_CARGO = os.path.join(ENGINE, "Cargo.toml")
check(
    "edge refact-tools -> refact-lsp is absent (crate never reaches the engine)",
    "refact-lsp" not in read(CRATE_TOOLS_CARGO),
    "crate->engine edge",
)
check(
    "edge refact-lsp -> refact-tools is present (engine owns the shim)",
    re.search(r"(?m)^refact-tools\s*=", read(ENGINE_CARGO)) is not None,
    "the engine must depend on the crate it shims",
)
check(
    "edge refact-runtime-api -> refact-lsp is absent",
    "refact-lsp" not in read(RUNTIME_API_CARGO),
    "crate->engine edge",
)
check(
    "refact-tools uses only refact_* crate paths",
    not re.search(r"\buse (crate|super)::", crate_prod),
    "engine-relative import survived the move",
)

# --------------------------------------------------------- goal_verifier split

monitor = read(CRATE_MONITOR)
check(
    "runtime-api owns verify_goal_before_completion",
    "pub async fn verify_goal_before_completion" in monitor,
    "not found",
)
check(
    "runtime-api monitor goes through the runner seam",
    "run_goal_verifier" in monitor and "crate::goal_verifier_runtime" in monitor,
    "the LLM call must dispatch through GoalVerifierRunner",
)
check(
    "runtime-api monitor carries no engine path",
    not re.search(
        r"\bcrate::(chat|llm|tools|caps|tokens|postprocessing|privacy|yaml_configs|ext|exec)\b",
        strip_code(monitor),
    ),
    "engine path in the crate copy",
)
check(
    "runtime-api declares mod goal_verifier_monitor",
    re.search(r"(?m)^pub mod goal_verifier_monitor;", read(RUNTIME_API_LIB)) is not None,
    "missing module declaration",
)
check(
    "runtime-api does not depend on refact-lsp",
    "refact-lsp" not in read(RUNTIME_API_CARGO),
    "crate->engine edge",
)

engine_monitor = read(ENGINE_MONITOR)
check(
    "engine monitor keeps the LLM body",
    "pub async fn run_goal_verifier" in engine_monitor
    and "EngineGoalVerifierRunner" in engine_monitor,
    "the prepare/stream body must stay in the engine",
)
check(
    "engine monitor no longer defines the gate",
    "pub async fn verify_goal_before_completion" not in engine_monitor,
    "the orchestration moved to refact-runtime-api",
)
gv_mod = read(ENGINE_GV_MOD)
check(
    "engine goal_verifier re-exports the gate from the crate",
    "pub use refact_runtime_api::goal_verifier_monitor::verify_goal_before_completion;" in gv_mod,
    "missing re-export",
)
check(
    "engine goal_verifier keeps its test-only prelude",
    all(
        needle in gv_mod
        for needle in ("use crate::chat::types::*;", "use crate::call_validation::")
    ),
    "tests.rs resolves types through super::*",
)

# ------------------------------------------------------------------- census

if os.path.exists(CENSUS_OUT):
    census = read(CENSUS_OUT)
    check(
        "census: chat::tools is a released shim",
        re.search(r"^\s*1\s+chat::tools\s+.*blocker=-", census, re.M) is not None,
        "chat::tools must be a 1-line facade",
    )
    check(
        "census: chat::goal_verifier::monitor is no longer island-blocked",
        re.search(
            r"chat::goal_verifier::monitor\s+.*blocker=(?:-|chat::(?:prepare|stream_core))",
            census,
        )
        is not None,
        "monitor must not gain a new engine or island blocker",
    )
    m = re.search(r"### chat: (\d+) files / (\d+) production lines", census)
    check("census covers the whole tree", m is not None, "class totals missing")
    if m:
        check(
            "census: chat shrank below the pre-move 23030 lines",
            int(m.group(2)) < 23030,
            f"current total {m.group(2)}",
        )
else:
    check("census output exists", False, CENSUS_OUT)

failed = [name for name, ok, _ in results if not ok]
for name, ok, detail in results:
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else "  <- " + detail))
print(f"\nAL-4 checks: {len(results)} run, {len(failed)} failed")
sys.exit(1 if failed else 0)
