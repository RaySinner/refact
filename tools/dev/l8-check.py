"""L-8 verification harness: static proof that the K8 move is behaviour-preserving.

No build is attempted here (the project build guard refuses compilation from agent
worktrees). Every assertion is a filesystem/text check with no shell separators.

Run: python tools/dev/l8-check.py
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

MOVED = ["tool_get_goal", "tool_set_goal", "tool_update_goal", "tool_goal_pursuit_controls",
         "tool_get_plan", "tool_set_plan", "tool_update_plan", "tool_search_trajectories",
         "tool_compress_chat"]

# already shims -> design_tools (engine-resident K12 group), left alone
ALREADY_SHIM = ["tool_visual_diff", "tool_ui_probe", "tool_contrast_audit",
                "tool_image_region", "tool_mark_elements"]

UNTOUCHED = ["mod", "design_tools", "tool_validate_goal", "tools_list", "tool_process",
             "tool_shell", "shell_gate"]

TEST_COUNTS = {
    "tool_get_goal": 5, "tool_set_goal": 5, "tool_update_goal": 8,
    "tool_goal_pursuit_controls": 5, "tool_get_plan": 4, "tool_set_plan": 12,
    "tool_update_plan": 13, "tool_search_trajectories": 2, "tool_compress_chat": 33,
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


# 1. crate modules exist and carry real production code
for mod in MOVED:
    p = DST / f"{mod}.rs"
    body = read(p) if p.exists() else ""
    check(f"crate file {mod}.rs exists and is non-trivial", p.exists() and len(body) > 1000,
          f"{len(body)} bytes")

# 2. no stray engine paths in the crate
for mod in MOVED:
    body = read(DST / f"{mod}.rs")
    stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)
                    if not m.startswith("crate::tools::")})
    check(f"crate {mod}.rs has no engine-only crate:: refs", not stray, str(stray))

# 3. engine files are shims that keep their tests
for mod in MOVED:
    body = read(SRC / f"{mod}.rs")
    shim = f"pub use refact_tools::tools::{mod}::*;"
    check(f"engine {mod}.rs starts with the re-export shim", body.startswith(shim),
          body.splitlines()[0] if body else "")
    got = len(re.findall(r"#\[(?:tokio::)?test[\(\]]", body))
    check(f"engine {mod}.rs keeps {TEST_COUNTS[mod]} tests", got == TEST_COUNTS[mod],
          f"got {got}")
    check(f"engine {mod}.rs keeps its mod tests block",
          "#[cfg(test)]\nmod tests {" in body or "#[cfg(test)]\nfn " in body)

# 4. K13 stubs are still the design_tools facades
for mod in ALREADY_SHIM:
    body = read(SRC / f"{mod}.rs").strip()
    struct = "".join(part.capitalize() for part in mod[len("tool_"):].split("_"))
    check(f"{mod}.rs is still a design_tools facade",
          body == f"pub use super::design_tools::Tool{struct};", body)

# 5. crate root module table
tools_rs = read(CRATE / "src" / "tools.rs")
for mod in MOVED:
    check(f"crate tools.rs declares pub mod {mod}", f"pub mod {mod};" in tools_rs)

# 6. crate manifest: deps for the moved code, and never refact-lsp
crate_toml = read(CRATE / "Cargo.toml")
for dep in ['dunce = "1.0.5"', 'hex = "0.4.3"', 'sha2 = "0.10.8"']:
    check(f"crate manifest declares {dep.split(' =')[0]}", dep in crate_toml)
check("crate manifest never mentions refact-lsp", "refact-lsp" not in crate_toml)

# 7. Cargo.lock keeps the crate entry consistent
lock = read(ENG / "Cargo.lock")
tools_block = lock.split('name = "refact-tools"')[1].split("[[package]]")[0]
for dep in ["dunce", "hex", "sha2"]:
    check(f"Cargo.lock refact-tools block depends on {dep}", f' "{dep}",' in tools_block)
    check(f"Cargo.lock has a {dep} package entry", f'name = "{dep}"' in lock)
check("Cargo.lock refact-tools block still has no refact-lsp", "refact-lsp" not in tools_block)
check("engine [dependencies] keeps refact-tools",
      'refact-tools = { path = "crates/refact-tools" }' in read(ENG / "Cargo.toml"))

# 8. moved symbols are reachable through the crate
check("crate defines ToolGetGoal", "pub struct ToolGetGoal" in read(DST / "tool_get_goal.rs"))
check("crate defines ToolSetPlan", "pub struct ToolSetPlan" in read(DST / "tool_set_plan.rs"))
check("crate defines ToolCompressChatApply",
      "pub struct ToolCompressChatApply" in read(DST / "tool_compress_chat.rs"))
for symbol in ["queue_goal_side_effect", "read_plan_from_path", "hist_search_preview_notice",
               "compress_chat_apply_head_output", "build_maximal_full_sweep_request",
               "insert_current_compression_report", "require_pursuable_goal"]:
    found = any(symbol in read(DST / f"{m}.rs") for m in MOVED)
    check(f"crate still defines {symbol}", found)

# 9. no behaviour-affecting edit to the moved bodies
set_goal = read(DST / "tool_set_goal.rs")
check("set_goal keeps its tool name and schema",
      '"set_goal"' in set_goal and '"required": ["content"]' in set_goal)
check("set_goal keeps the duplicate-goal error",
      "goal already exists; use update_goal" in set_goal)
set_plan = read(DST / "tool_set_plan.rs")
check("set_plan keeps its tool name and duplicate error",
      '"set_plan"' in set_plan and "a plan already exists; use update_plan to change it" in set_plan)
check("get_goal keeps its tool name and empty schema",
      '"get_goal"' in read(DST / "tool_get_goal.rs")
      and '"properties": {}' in read(DST / "tool_get_goal.rs"))
check("update_goal keeps its truncated tool-result shape",
      '"original_chars"' in read(DST / "tool_update_goal.rs"))
compress = read(DST / "tool_compress_chat.rs")
check("compress_chat keeps both tool names",
      '"compress_chat_probe"' in compress and '"compress_chat_apply"' in compress)
check("compress_chat keeps the deterministic recovery sweep constant",
      "RECOVERY_PLAIN_TEXT_FLOOR" in compress)

# 10. constructor surface still resolves in the engine
callers = read(SRC / "tools_list.rs")
for mod, ctor in [("tool_get_goal", "ToolGetGoal::new"), ("tool_set_goal", "ToolSetGoal {"),
                  ("tool_update_goal", "ToolUpdateGoal {"), ("tool_get_plan", "ToolGetPlan::new"),
                  ("tool_set_plan", "ToolSetPlan {"), ("tool_update_plan", "ToolUpdatePlan {"),
                  ("tool_search_trajectories", "ToolSearchTrajectories {"),
                  ("tool_goal_pursuit_controls", "ToolPauseGoal {"),
                  ("tool_compress_chat", "ToolCompressChatApply {")]:
    check(f"tools_list.rs still constructs {ctor} via crate::tools::{mod}",
          f"crate::tools::{mod}::" in callers and ctor.split(" ")[0] in callers)

# 11. the kept engine tests still compile against the engine's own helpers
for mod, marker in [("tool_get_goal", "crate::global_context::tests::make_test_gcx"),
                    ("tool_set_plan", "crate::tools::tools_list::get_tools_for_mode"),
                    ("tool_search_trajectories", "crate::tools::settings_guard::SettingsGuard"),
                    ("tool_compress_chat", "refact_chat_history::trajectory_ops::")]:
    check(f"engine {mod}.rs tests still use {marker}", marker in read(SRC / f"{mod}.rs"))

# 12. nothing that must stay was disturbed
dirty = git("status", "--porcelain").splitlines()
touched = {line.strip().split(None, 1)[1].replace("\\", "/")
           for line in dirty if line.strip().split(None, 1)[1:]}
allowed_prefixes = (
    "refact-agent/engine/src/tools/",
    "refact-agent/engine/crates/refact-tools/",
    "refact-agent/engine/Cargo.lock",
    "tools/dev/l8-check.py",
)
extra = [t for t in touched if not t.startswith(allowed_prefixes)]
check("no unrelated file touched", not extra, str(extra))
for mod in UNTOUCHED:
    name = "mod" if mod == "mod" else mod
    check(f"src/tools/{name}.rs untouched",
          f"refact-agent/engine/src/tools/{name}.rs" not in touched)

failed = [c for c in checks if not c[1]]
print(f"\n{len(checks) - len(failed)}/{len(checks)} checks passed")
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
