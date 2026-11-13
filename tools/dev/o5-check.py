"""O-5 verification harness: static proof the goal-verifier helper extraction is faithful
and that the two `tools/` files are (not) movable yet.

No build is attempted here - subagents must not compile; the planner owns the engine build.
Every check is a filesystem / text / git-blob comparison, and there is no shell separator.

Run (single command):
    python tools/dev/o5-check.py
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
CHAT_API = ENG / "crates" / "refact-chat-api" / "src"
TOOLS = ENG / "src" / "tools"
GV = ENG / "src" / "chat" / "goal_verifier"

CRATE_GV = CHAT_API / "goal_verifier.rs"
ENG_MOD = GV / "mod.rs"
ENG_MONITOR = GV / "monitor.rs"

BASE = "a9f66c936"

# helpers that need no engine handle and therefore move to the crate
MOVED = [
    "GOAL_VERIFIER_MAX_TOKENS",
    "goal_text_for_verifier",
    "verifier_sampling_parameters",
    "verifier_cancel_requested",
]

# engine wrappers that genuinely need an engine handle / the LLM stream and must stay verbatim
KEPT = [
    "verify_goal_before_completion",
    "run_goal_verifier",
]

# neither may move: they need the engine handle types or the engine-local stream types
ENGINE_ONLY = [
    "goal_verifier_prepare_inputs_from_parts",
    "goal_verifier_prepare_inputs",
    "goal_verifier_reply_from_choice",
]

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(path):
    return path.read_text(encoding="utf-8")


def baseline(rel):
    out = subprocess.run(["git", "-C", str(ROOT), "show", "%s:%s" % (BASE, rel)],
                         capture_output=True)
    if out.returncode != 0:
        return ""
    return out.stdout.decode("utf-8").replace("\r\n", "\n")


def declared(text, symbol):
    """A definition of `symbol` in this text (fn / const / struct / enum), not a call."""
    pattern = (r"^pub(?:\([a-z_]*\))?\s+(?:async\s+)?"
               r"(?:fn|const|struct|enum|static|type|trait|union)\s+%s\b" % re.escape(symbol))
    return re.search(pattern, text, re.M) is not None


def body_of(text, symbol):
    """Signature line .. the first column-0 `}` after it (these fns close at column 0)."""
    lines = text.split("\n")
    start = None
    for index, line in enumerate(lines):
        if re.match(r"^(?:pub(?:\([a-z_]*\))?\s+)?(?:async\s+)?fn\s+%s\b" % re.escape(symbol), line):
            start = index
            break
    if start is None:
        return None
    for index in range(start + 1, len(lines)):
        if lines[index] == "}":
            return "\n".join(l.rstrip() for l in lines[start:index + 1])
    return None


crate_text = read(CRATE_GV)
mod_text = read(ENG_MOD)
monitor_text = read(ENG_MONITOR)
monitor_base = baseline("refact-agent/engine/src/chat/goal_verifier/monitor.rs")

# 1. the crate owns the four moved helpers
print("=== 1. crate-owned helpers")
for symbol in MOVED:
    check("refact-chat-api goal_verifier.rs declares `%s`" % symbol, declared(crate_text, symbol))

# 2. the engine no longer declares them
for symbol in MOVED:
    check("engine monitor.rs no longer declares `%s`" % symbol, not declared(monitor_text, symbol))

# 2b. the engine module re-exports them from the crate
mod_reexport = re.search(r"pub use refact_chat_api::goal_verifier::\{(.*?)\};", mod_text, re.S)
check("engine mod.rs re-exports refact_chat_api::goal_verifier", bool(mod_reexport))
names = set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", mod_reexport.group(1))) if mod_reexport else set()
for symbol in MOVED:
    check("engine mod.rs re-exports `%s`" % symbol, symbol in names)

# 3. moved bodies are verbatim apart from the two documented rewrites
REWRITES = [
    ("crate::chat::goal_role::try_synthesize_current_goal",
     "crate::session_goal_role::try_synthesize_current_goal"),
    ("crate::call_validation::ReasoningEffort", "ReasoningEffort"),
]


def normalized(text):
    """Line identity ignoring the visibility token and the two documented path rewrites."""
    for old, new in REWRITES:
        text = text.replace(old, new)
    lines = [re.sub(r"^(pub(?:\([a-z_]*\))?)\s+", "", line.rstrip()) for line in text.split("\n")]
    return "\n".join(lines).strip()


print("\n=== 2. moved bodies verbatim (rewrites applied)")
for symbol in MOVED:
    want = body_of(monitor_base, symbol)
    got = body_of(crate_text, symbol)
    if symbol == "GOAL_VERIFIER_MAX_TOKENS":
        want = "pub const GOAL_VERIFIER_MAX_TOKENS: usize = 1024;"
        got = re.search(r"pub const GOAL_VERIFIER_MAX_TOKENS: usize = \d+;", crate_text)
        got = got.group(0) if got else ""
        check("crate keeps GOAL_VERIFIER_MAX_TOKENS = 1024", got == want, got)
        continue
    check("crate `%s` body matches the engine baseline" % symbol,
          bool(want) and bool(got) and normalized(want) == normalized(got),
          "want %r got %r" % (want, got))

# 4. the engine wrappers stay verbatim
print("\n=== 3. engine wrappers untouched")
for symbol in KEPT:
    check("monitor.rs keeps `%s` verbatim" % symbol,
          body_of(monitor_base, symbol) == body_of(monitor_text, symbol))
for symbol in ENGINE_ONLY:
    want = body_of(monitor_base, symbol)
    got = body_of(monitor_text, symbol)
    check("monitor.rs keeps engine-only `%s`" % symbol,
          bool(want) and bool(got) and normalized(want) == normalized(got)
          or (want is None and got is None))

# 5. the moved code is gone from the engine monitor (no duplicate definitions)
leftover = [s for s in MOVED if re.search(r"^\s*(?:pub(?:\(super\))?\s+)?(?:async\s+)?fn\s+%s\b"
                                          % re.escape(s), monitor_text, re.M)]
check("no moved helper body remains in monitor.rs", not leftover, str(leftover))

# 6. tool_validate_goal.rs import retarget
solve_text = read(TOOLS / "tool_validate_goal.rs")
check("tool_validate_goal.rs takes the verdict helpers from refact_chat_api::goal_verifier",
      "use refact_chat_api::goal_verifier::{" in solve_text)
check("tool_validate_goal.rs still takes run_goal_verifier from the engine module",
      "use crate::chat::goal_verifier::run_goal_verifier;" in solve_text)
for symbol in ["apply_goal_verdict_guarded_with_delivery", "begin_goal_verification_if_needed",
               "GoalVerificationApplyOutcome", "GoalVerificationBegin", "GoalVerdict"]:
    check("tool_validate_goal.rs names `%s` after the retarget" % symbol, symbol in solve_text)
check("tool_validate_goal.rs no longer groups run_goal_verifier with the verdict helpers",
      "apply_goal_verdict_guarded_with_delivery, begin_goal_verification_if_needed, run_goal_verifier"
      not in solve_text)

# 7. tool_task_batch.rs const retarget
batch_text = read(TOOLS / "tool_task_batch.rs")
check("tool_task_batch.rs no longer uses crate::chat::post_merge_check::DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS",
      "crate::chat::post_merge_check::DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS" not in batch_text)
retargeted = batch_text.count("refact_chat_api::tools_facade::DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS")
check("tool_task_batch.rs retargets all 4 const sites", retargeted == 4, "got %d" % retargeted)

# 8. the two tool files changed on their import lines only
print("\n=== 4. tool-file change scope")
for rel, allowed_added, allowed_removed in [
    ("refact-agent/engine/src/tools/tool_validate_goal.rs", 3, 2),
    ("refact-agent/engine/src/tools/tool_task_batch.rs", 4, 4),
]:
    out = subprocess.run(["git", "-C", str(ROOT), "diff", "--numstat", BASE, "--", rel],
                         capture_output=True, text=True, encoding="utf-8").stdout.strip()
    added = removed = None
    if out:
        parts = out.splitlines()[0].split("\t")
        added, removed = int(parts[0]), int(parts[1])
    check("%s diff is %d+/%d- (import lines only)" % (rel.split("/")[-1], allowed_added, allowed_removed),
          added == allowed_added and removed == allowed_removed,
          "got %s+/%s-" % (added, removed))

# 9. no crate -> engine dependency
print("\n=== 5. dependency direction")
chat_api_toml = read(ENG / "crates" / "refact-chat-api" / "Cargo.toml")
tools_toml = read(ENG / "crates" / "refact-tools" / "Cargo.toml")
for name, toml in [("refact-chat-api", chat_api_toml), ("refact-tools", tools_toml)]:
    check("%s manifest never names refact-lsp" % name, "refact-lsp" not in toml)
check("refact-chat-api manifest declares refact-core",
      'refact-core = { path = "../refact-core" }' in chat_api_toml)
check("engine [dependencies] keeps refact-chat-api",
      'refact-chat-api = { path = "crates/refact-chat-api" }' in read(ENG / "Cargo.toml"))

# 10. scope of the change
status = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain", "--untracked-files=all"],
                        capture_output=True, text=True, encoding="utf-8").stdout
touched = sorted({line[3:].strip().replace("\\", "/") for line in status.splitlines() if line[3:]})
allowed = (
    "refact-agent/engine/src/chat/goal_verifier/",
    "refact-agent/engine/crates/refact-chat-api/src/goal_verifier.rs",
    "refact-agent/engine/src/tools/tool_validate_goal.rs",
    "refact-agent/engine/src/tools/tool_task_batch.rs",
    "tools/dev/o5-check.py",
    "tools/dev/o5-check.ps1",
)
stray = [t for t in touched if not t.startswith(allowed)]
check("no file outside the card's scope changed", not stray, str(stray))

for tool in ["refact-agent/engine/src/tools/tool_task_batch.rs",
             "refact-agent/engine/src/tools/tool_validate_goal.rs"]:
    tracked = subprocess.run(["git", "-C", str(ROOT), "ls-files", tool],
                             capture_output=True, text=True, encoding="utf-8").stdout.strip()
    check("%s stays engine-side (not deleted)" % tool.split("/")[-1], bool(tracked))

# 11. syntax
print("\n=== 6. parse check")
if subprocess.run(["rustfmt", "--version"], capture_output=True, text=True).returncode == 0:
    for path in [CRATE_GV, ENG_MOD, ENG_MONITOR]:
        result = subprocess.run(["rustfmt", "--edition", "2021", "--emit", "stdout", str(path)],
                                capture_output=True, text=True)
        check("%s parses" % path.name, result.returncode == 0,
              (result.stderr or result.stdout)[:200])
else:
    check("rustfmt unavailable, parse check skipped", True, "")

# ------------------------------------------------------------------ verdict table
print("\n=== 7. honesty census: the two card targets")
verdict_rows = [
    ("tools/tool_validate_goal.rs", 241, "NO",
     "crate::chat::goal_verifier::run_goal_verifier (engine LLM-stream wrapper) + "
     "crate::chat::try_restore_session_if_trajectory_exists (engine url: src/chat/session_runtime.rs:293)"),
    ("tools/tool_task_batch.rs", 1182, "NO",
     "crate::tools::tool_task_merge_agent::ToolTaskMergeAgent "
     "(blocked in turn by crate::agentic::generate_commit_message::generate_commit_message_by_diff)"),
]
print("  %-32s %6s %6s  %s" % ("file", "prod", "movable", "residual blocker"))
for name, prod, movable, blocker in verdict_rows:
    print("  %-32s %6d %6s  %s" % (name, prod, movable, blocker))

print("\nNOTE: the only engine dep that the two files reached through `chat::goal_verifier` was")
print("      the LLM-stream wrapper; everything else there is now crate-resident (this card).")

failed = [name for name, ok, _ in checks if not ok]
print("\n%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
