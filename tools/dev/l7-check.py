"""L-7 verification harness: static proof that the K5a move is behaviour-preserving.

Mirrors tools/dev/l2-check.py: filesystem/text assertions only, no shell separators.
Run: python tools/dev/l7-check.py
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
    "tool_task_board",
    "tool_card_comment",
    "tool_task_mark_card",
    "tool_task_create_room",
    "tool_task_init",
    "tool_task_done",
    "tool_tasks",
    "tool_task_verify_card",
]
WITH_TESTS = {
    "tool_task_board": 7,
    "tool_card_comment": 8,
    "tool_task_mark_card": 2,
    "tool_task_create_room": 17,
    "tool_task_verify_card": 5,
}

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

# 3. the engine-side types are reached through their owning crates, not through the engine
board = read(DST / "tool_task_board.rs")
check("crate tool_task_board.rs targets refact_runtime_api types",
      "refact_runtime_api::AppState" not in board and "crate::app_state" not in board)
room = read(DST / "tool_task_create_room.rs")
check("crate tool_task_create_room.rs uses refact_runtime_api::AppState",
      "use refact_runtime_api::AppState;" in room)
check("crate tool_task_create_room.rs uses refact_runtime_api ToolCtx",
      "refact_runtime_api::tool_ctx::ToolCtx" in room)

# 4. engine files are shims; the five split ones keep their tests verbatim
for mod in MOVED:
    body = read(SRC / f"{mod}.rs")
    shim = f"pub use refact_tools::tools::{mod}::*;"
    check(f"engine {mod}.rs starts with the re-export shim", body.startswith(shim),
          body.splitlines()[0] if body else "")

for mod, want in WITH_TESTS.items():
    body = read(SRC / f"{mod}.rs")
    got = len(re.findall(r"#\[(?:tokio::)?test[\(\]]", body))
    check(f"engine {mod}.rs keeps {want} tests", got == want, f"got {got}")
    check(f"engine {mod}.rs keeps its mod tests block", "#[cfg(test)]\nmod tests {" in body)

for mod in ["tool_task_init", "tool_task_done", "tool_tasks"]:
    body = read(SRC / f"{mod}.rs")
    check(f"engine {mod}.rs is shim only", body.strip() == f"pub use refact_tools::tools::{mod}::*;",
          body.strip()[:80])

# 5. every private item the staying tests reach was widened to pub in the crate
WIDENED = {
    "tool_task_board": ["pub fn parse_filter(", "pub fn card_matches_filter(", "pub fn render_board_summary(",
                        "pub fn render_card_details(", "pub fn render_board_tree(", "pub fn render_board_mermaid(",
                        "pub fn render_ready_cards(", "pub enum BoardVerbosity {", "pub struct BoardFilter {"],
    "tool_card_comment": ["pub fn format_comments("],
    "tool_task_create_room": ["pub fn unknown_config_name_error(", "pub fn parse_agents(", "pub fn roster_summary("],
}
for mod, symbols in WIDENED.items():
    body = read(DST / f"{mod}.rs")
    for symbol in symbols:
        check(f"crate {mod}.rs widens {symbol.rstrip('(').rstrip('{')}", symbol in body)

# 6. crate root module table
tools_rs = read(CRATE / "src" / "tools.rs")
for mod in MOVED:
    check(f"crate tools.rs declares pub mod {mod}", f"pub mod {mod};" in tools_rs)

# 7. crate manifest stays engine-free
crate_toml = read(CRATE / "Cargo.toml")
check("crate manifest never mentions refact-lsp", "refact-lsp" not in crate_toml)

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

# 9. tools_list.rs constructor sites are untouched and unchanged
tools_list = read(SRC / "tools_list.rs")
for ctor in ["ToolTaskBoardGet::new()", "ToolTaskBoardCreateCard::new()", "ToolTaskBoardUpdateCard::new()",
             "ToolTaskBoardMoveCard::new()", "ToolTaskBoardDeleteCard::new()", "ToolTaskReadyCards::new()",
             "ToolTaskCreateRoom::new()", "ToolCardCommentAdd::new()", "ToolCardCommentList::new()",
             "ToolTaskMarkCardDone::new()", "ToolTaskMarkCardFailed::new()", "ToolTaskVerifyCard::new()",
             "ToolTaskInit::new()"]:
    check(f"tools_list.rs still constructs {ctor}", ctor in tools_list)
for ctor in ["crate::tools::tool_task_done::ToolTaskDone {", "crate::tools::tool_tasks::ToolTasksSet {"]:
    check(f"tools_list.rs still constructs {ctor}", ctor in tools_list)

# 10. nothing that must stay was disturbed
dirty = git("status", "--porcelain").splitlines()
touched = {line.strip().split(None, 1)[1].replace("\\", "/") for line in dirty if line.strip().split(None, 1)[1:]}
allowed_prefixes = (
    "refact-agent/engine/src/tools/",
    "refact-agent/engine/crates/refact-tools/",
    "tools/dev/l7-check.py",
)
extra = [t for t in touched if not t.startswith(allowed_prefixes)]
check("no unrelated file touched", not extra, str(extra))
check("tools/mod.rs untouched", "refact-agent/engine/src/tools/mod.rs" not in touched)
check("engine Cargo.toml untouched", "refact-agent/engine/Cargo.toml" not in touched)
check("engine Cargo.lock untouched", "refact-agent/engine/Cargo.lock" not in touched)
check("tools_list.rs untouched", "refact-agent/engine/src/tools/tools_list.rs" not in touched)
check("tool_task_batch.rs untouched", "refact-agent/engine/src/tools/tool_task_batch.rs" not in touched)
check("task_tool_helpers.rs untouched", "refact-agent/engine/src/tools/task_tool_helpers.rs" not in touched)

failed = [c for c in checks if not c[1]]
print(f"\n{len(checks) - len(failed)}/{len(checks)} checks passed")
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
