"""O-3 verification harness: static proof that the agentic/agents/chat blockers are cleared.

Complements the real build (`tools/dev/engine-build.ps1`). Every assertion is a
filesystem/text check with no shell separators.

Run: python tools/dev/o3-check.py
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src"
CRATE = ENG / "crates"

checks = []


def check(name, ok, detail=""):
    checks.append((name, bool(ok), detail))


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def prod_lines(text):
    """Count lines outside `#[cfg(test)]`-attributed items (production mass)."""
    lines = text.splitlines()
    count = 0
    index = 0
    while index < len(lines):
        line = lines[index]
        if line.startswith("#[cfg(test)]"):
            index += 1
            pending = True
            depth = 0
            while index < len(lines) and pending:
                body = lines[index]
                depth += body.count("{") - body.count("}")
                if depth > 0:
                    pending = False
                elif body.strip().endswith(";"):
                    pending = False
                index += 1
            while index < len(lines) and depth > 0:
                depth += lines[index].count("{") - lines[index].count("}")
                index += 1
            continue
        count += 1
        index += 1
    return count


RUNTIME_API = CRATE / "refact-runtime-api" / "src"

# --- 1. crate-side modules exist and hold the moved bodies -------------------
trajectory_prefix = RUNTIME_API / "trajectory_prefix.rs"
check("crate module refact-runtime-api/src/trajectory_prefix.rs exists", trajectory_prefix.exists())
prefix_src = read(trajectory_prefix) if trajectory_prefix.exists() else ""
for symbol in [
    "pub fn ensure_frozen_prefix",
    "pub fn first_system_prompt",
    "pub fn frozen_prefix_is_complete",
    "pub fn new_frozen_request_prefix",
    "pub fn repair_transition_frozen_prefix",
]:
    check(f"trajectory_prefix defines {symbol}", symbol in prefix_src)

session_runtime = RUNTIME_API / "session_runtime.rs"
check("crate module refact-runtime-api/src/session_runtime.rs exists", session_runtime.exists())
session_src = read(session_runtime) if session_runtime.exists() else ""
check(
    "session_runtime defines get_or_create_session_with_trajectory",
    "pub async fn get_or_create_session_with_trajectory" in session_src,
)
check(
    "session_runtime defines try_restore_session_if_trajectory_exists",
    "pub async fn try_restore_session_if_trajectory_exists" in session_src,
)

commit_runtime = RUNTIME_API / "commit_message_runtime.rs"
check("crate module refact-runtime-api/src/commit_message_runtime.rs exists", commit_runtime.exists())
commit_src = read(commit_runtime) if commit_runtime.exists() else ""
check(
    "commit_message_runtime defines generate_commit_message_by_diff",
    "pub async fn generate_commit_message_by_diff" in commit_src,
)
check(
    "commit_message_runtime defines generate_commit_message_by_diff_with_abort",
    "pub async fn generate_commit_message_by_diff_with_abort" in commit_src,
)

lib_src = read(RUNTIME_API / "lib.rs")
for module in ["session_runtime", "trajectory_prefix", "commit_message_runtime"]:
    check(f"refact-runtime-api lib.rs declares `pub mod {module}`", f"pub mod {module};" in lib_src)

# --- 2. engine files became shims / re-exports ------------------------------
engine_session = read(SRC / "chat" / "session_runtime.rs")
check(
    "engine chat/session_runtime.rs re-exports the crate session runtime",
    "pub use refact_runtime_api::session_runtime::{" in engine_session
    and "get_or_create_session_with_trajectory" in engine_session,
)
check(
    "engine chat/session_runtime.rs no longer defines the moved fns",
    "pub async fn get_or_create_session_with_trajectory" not in engine_session
    and "pub async fn try_restore_session_if_trajectory_exists" not in engine_session,
)

engine_reload = read(SRC / "chat" / "trajectories" / "reload.rs")
check(
    "engine trajectories/reload.rs re-exports the crate prefix helpers",
    "pub use refact_runtime_api::trajectory_prefix::{" in engine_reload,
)
for gone in [
    "fn normalize_system_prompt",
    "fn frozen_prefix_has_system_prompt",
    "fn frozen_prefix_has_tools",
    "fn is_mode_transition_or_handoff",
    "pub fn ensure_frozen_prefix",
    "pub fn first_system_prompt",
    "pub fn repair_transition_frozen_prefix",
    "pub fn new_frozen_request_prefix",
    "fn legacy_incomplete_prefix_from_messages",
]:
    check(f"engine trajectories/reload.rs no longer defines {gone}", gone not in engine_reload)

engine_commit = read(SRC / "agentic" / "generate_commit_message.rs")
check(
    "engine agentic/generate_commit_message.rs forwards to the crate runtime",
    "refact_runtime_api::commit_message_runtime::generate_commit_message_by_diff" in engine_commit,
)
check(
    "engine agentic/generate_commit_message.rs keeps the public wrapper",
    "pub async fn generate_commit_message_by_diff" in engine_commit,
)
check(
    "engine agentic/generate_commit_message.rs keeps _generate_commit_message_for_projects",
    "pub async fn _generate_commit_message_for_projects" in engine_commit,
)

# --- 3. BgAgentKind is crate-owned ------------------------------------------
agents_types = read(CRATE / "refact-agents-core" / "src" / "types.rs")
check(
    "refact-agents-core owns BgAgentKind",
    "pub enum BgAgentKind" in agents_types,
)
agentic_check = read(SRC / "tools" / "tool_subagent.rs")
check(
    "tool_subagent.rs imports BgAgentKind from refact-agents-core",
    "use refact_agents_core::types::BgAgentKind;" in agentic_check,
)
check(
    "tool_subagent.rs no longer imports crate::agents::types::BgAgentKind",
    "use crate::agents::types::BgAgentKind;" not in agentic_check,
)

# --- 4. the four tools files no longer reach the moved engine modules -------
tool_files = [
    "tool_task_merge_agent.rs",
    "tool_worktree_merge.rs",
    "tool_subagent.rs",
    "tool_cron_create.rs",
    "tool_validate_goal.rs",
]
for name in tool_files:
    text = read(SRC / "tools" / name)
    check(f"tools/{name} does not import crate::agentic::generate_commit_message",
          "crate::agentic::generate_commit_message" not in text)
    check(f"tools/{name} does not call crate::chat::get_or_create_session_with_trajectory",
          "crate::chat::get_or_create_session_with_trajectory" not in text)
    check(f"tools/{name} does not call crate::chat::try_restore_session_if_trajectory_exists",
          "crate::chat::try_restore_session_if_trajectory_exists" not in text)

merge_agent = read(SRC / "tools" / "tool_task_merge_agent.rs")
check(
    "tool_task_merge_agent.rs uses refact_runtime_api::commit_message_runtime",
    merge_agent.count("refact_runtime_api::commit_message_runtime::generate_commit_message_by_diff") == 2,
)
worktree_merge = read(SRC / "tools" / "tool_worktree_merge.rs")
check(
    "tool_worktree_merge.rs uses refact_runtime_api::commit_message_runtime",
    "refact_runtime_api::commit_message_runtime::generate_commit_message_by_diff" in worktree_merge,
)
cron_create = read(SRC / "tools" / "tool_cron_create.rs")
check(
    "tool_cron_create.rs uses refact_runtime_api::session_runtime",
    "refact_runtime_api::session_runtime::get_or_create_session_with_trajectory" in cron_create,
)
validate_goal = read(SRC / "tools" / "tool_validate_goal.rs")
check(
    "tool_validate_goal.rs uses refact_runtime_api::session_runtime",
    "refact_runtime_api::session_runtime::try_restore_session_if_trajectory_exists" in validate_goal,
)

# --- 5. no cycle: crates must not depend on the engine ----------------------
crate_roots = [p for p in CRATE.iterdir() if p.is_dir() and (p / "Cargo.toml").exists()]
cycles = []
for crate in crate_roots:
    toml = read(crate / "Cargo.toml")
    if re.search(r'^\s*(refact-lsp|refact_agent_engine)\s*=', toml, re.M):
        cycles.append(crate.name)
engine_toml = read(ENG / "Cargo.toml")
check(
    "refact-runtime-api is an engine dependency",
    "refact-runtime-api" in engine_toml,
)
check("no engine crate depends back on refact-lsp", not cycles, ",".join(cycles))

# --- 6. report the unblocked tool files and their production sizes ----------
UNBLOCKED = [
    ("tool_task_merge_agent.rs", 1719),
    ("tool_worktree_merge.rs", 283),
    ("tool_subagent.rs", 656),
    ("tool_cron_create.rs", 636),
]
sizes = []
for name, expected in UNBLOCKED:
    text = read(SRC / "tools" / name)
    sizes.append((name, prod_lines(text), expected))
    check(f"tools/{name} still exists (group cards move it, not this one)", len(text) > 0)

failed = [c for c in checks if not c[1]]
for name, ok, detail in checks:
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail and not ok else ""))

print("")
print("unblocked tools files (production lines, census baseline in parens):")
for name, actual, expected in sizes:
    print(f"  {name}: {actual} (census {expected})")

print("")
print(f"{len(checks) - len(failed)}/{len(checks)} checks passed")
sys.exit(1 if failed else 0)
