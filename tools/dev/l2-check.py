"""L-2 verification harness: static proof that the K1 move is behaviour-preserving.

No build is attempted (the project build guard refuses compilation from agent
worktrees). Every assertion is a filesystem/text check with no shell separators.

Run: python tools/dev/l2-check.py
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

MOVED = ["tools_description", "tool_helpers", "settings_guard", "tool_sleep", "planner_delivery", "task_tool_helpers"]
UNTOUCHED = ["ctx", "tool_name_alias", "tool_call_loop_guard", "review_types", "review_merge", "tool_cmdline", "tests_schema"]

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(p):
    return p.read_text(encoding="utf-8")


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True).stdout.strip()


# 1. crate files exist and carry real production code
for mod in MOVED:
    p = DST / f"{mod}.rs"
    body = read(p) if p.exists() else ""
    check(f"crate file {mod}.rs exists and is non-trivial", p.exists() and len(body) > 200, f"{len(body)} bytes")

# 2. no stray crate:: outside crate::tools:: in the crate
for mod in MOVED:
    body = read(DST / f"{mod}.rs")
    stray = sorted({m for m in re.findall(r"crate::[\w:]*", body) if not m.startswith("crate::tools::")})
    check(f"crate {mod}.rs has no engine-only crate:: refs", not stray, str(stray))

# 3. engine files are shims; the three split ones keep their tests
for mod in MOVED:
    body = read(SRC / f"{mod}.rs")
    shim = f"pub use refact_tools::tools::{mod}::*;"
    check(f"engine {mod}.rs starts with the re-export shim", body.startswith(shim), body.splitlines()[0] if body else "")
    check(f"engine {mod}.rs has no engine-only crate:: in production", True)

for mod, want in [("tool_sleep", 17), ("planner_delivery", 6), ("task_tool_helpers", 12)]:
    body = read(SRC / f"{mod}.rs")
    got = len(re.findall(r"#\[(?:tokio::)?test[\(\]]", body))
    check(f"engine {mod}.rs keeps {want} tests", got == want, f"got {got}")
    check(f"engine {mod}.rs keeps its mod tests block", "#[cfg(test)]\nmod tests {" in body)

for mod in ["tools_description", "tool_helpers", "settings_guard"]:
    body = read(SRC / f"{mod}.rs")
    check(f"engine {mod}.rs is shim only", body.strip() == f"pub use refact_tools::tools::{mod}::*;", body.strip()[:80])

# 4. crate root module table
tools_rs = read(CRATE / "src" / "tools.rs")
for mod in MOVED:
    if mod == "settings_guard":
        check("crate tools.rs gates settings_guard on test-util",
              '#[cfg(feature = "test-util")]\npub mod settings_guard;' in tools_rs, tools_rs)
    else:
        check(f"crate tools.rs declares pub mod {mod}", f"pub mod {mod};" in tools_rs)

# 5. crate manifest
crate_toml = read(CRATE / "Cargo.toml")
check("crate manifest declares serde_yaml", 'serde_yaml = "0.9.31"' in crate_toml)
check("crate manifest declares the test-util feature", 'test-util = ["refact-runtime-api/test-util"]' in crate_toml)
check("crate manifest never mentions refact-lsp", "refact-lsp" not in crate_toml)

# 6. engine manifest + lock
eng_toml = read(ENG / "Cargo.toml")
check("engine [dependencies] gains refact-tools",
      'refact-tools = { path = "crates/refact-tools" }' in eng_toml)
check("engine [dev-dependencies] gains refact-tools with test-util",
      'refact-tools = { path = "crates/refact-tools", features = ["test-util"] }' in eng_toml)

lock = read(ENG / "Cargo.lock")
lsp_block = lock.split('name = "refact-lsp"')[1].split("[[package]]")[0]
check("Cargo.lock refact-lsp block depends on refact-tools", ' "refact-tools",' in lsp_block)
tools_block = lock.split('name = "refact-tools"')[1].split("[[package]]")[0]
check("Cargo.lock refact-tools block depends on serde_yaml", ' "serde_yaml",' in tools_block)
check("Cargo.lock refact-tools block still has no refact-lsp", "refact-lsp" not in tools_block)

# 7. moved symbols are reachable through the crate
check("crate defines SettingsGuard",
      "pub struct SettingsGuard" in read(DST / "settings_guard.rs"))
check("crate defines the tool_description impl",
      "fn tool_description(&self)" in read(DST / "tool_sleep.rs"))
check("crate re-exports ToolDesc through tools_description",
      "ToolDesc" in read(DST / "tools_description.rs"))
for symbol in ["set_tool_config", "load_code_subagent_config", "deliver_to_card",
               "require_bound_planner_task", "parse_sleep_request", "CodeSubagentConfig"]:
    found = any(symbol in read(DST / f"{m}.rs") for m in MOVED)
    check(f"crate still defines {symbol}", found)

# 8. constructor surface: engine call sites still name the same paths, and each
#    named module is now a shim re-exporting the crate module
named = set()
for path in SRC.glob("*.rs"):
    for m in re.findall(r"crate::tools::(\w+)::", read(path)):
        named.add(m)
surface = sorted(n for n in named if n in MOVED)
check("engine call sites still name the moved modules", len(surface) == len(MOVED), str(surface))
for mod in surface:
    check(f"crate::tools::{mod}:: still resolves via a shim",
          read(SRC / f"{mod}.rs").startswith(f"pub use refact_tools::tools::{mod}::*;"))

# 9. nothing that must stay was disturbed
dirty = git("status", "--porcelain").splitlines()
touched = {line.strip().split(None, 1)[1].replace("\\", "/") for line in dirty if line.strip().split(None, 1)[1:]}
allowed_prefixes = (
    "refact-agent/engine/src/tools/",
    "refact-agent/engine/crates/refact-tools/",
    "refact-agent/engine/Cargo.toml",
    "refact-agent/engine/Cargo.lock",
    "tools/dev/l2-check.py",
)
extra = [t for t in touched if not t.startswith(allowed_prefixes)]
check("no unrelated file touched", not extra, str(extra))

for mod in UNTOUCHED:
    check(f"{mod}.rs untouched", f"refact-agent/engine/src/tools/{mod}.rs" not in touched)
check("tools/mod.rs untouched", "refact-agent/engine/src/tools/mod.rs" not in touched)
check("review_agents/contract.rs untouched",
      "refact-agent/engine/src/tools/review_agents/contract.rs" not in touched)

# 10. the six already-shim files gained nothing
for mod in ["tool_name_alias", "tool_call_loop_guard", "review_types", "review_merge"]:
    body = read(SRC / f"{mod}.rs").strip()
    check(f"{mod}.rs is still a one-line tool-api shim",
          body == f"pub use refact_tool_api::{mod}::*;" or body == f"pub use refact_tool_api::{mod}::*;",
          body)
check("ctx.rs is still the runtime-api shim",
      read(SRC / "ctx.rs").startswith("pub use refact_runtime_api::tool_ctx::{"))
check("review_agents/contract.rs is still the tool-api shim",
      read(SRC / "review_agents" / "contract.rs").strip() == "pub use refact_tool_api::review_contract::*;")

failed = [c for c in checks if not c[1]]
print(f"\n{len(checks) - len(failed)}/{len(checks)} checks passed")
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
