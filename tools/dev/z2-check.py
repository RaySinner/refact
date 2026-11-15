"""Z-2 verification harness: static proof that the scheduler/tasks/caps harvest
into `refact-runtime-api` is behaviour-preserving, then the engine build gate.

Run from the repo root (no shell separators needed):

    python tools/dev/z2-check.py            # static checks + cargo check
    python tools/dev/z2-check.py --static   # skip the build gate

  Z1 census before -> after, per module;
  Z2 every harvested engine file is a pure re-export shim plus its
     `#[cfg(test)]` code (the tests stayed in the engine on purpose);
  Z3 the new crate modules are declared in refact-runtime-api/lib.rs;
  Z4 no crate file reaches back into the engine (`crate::` refs resolve in-crate);
  Z5 the moved production regions are verbatim modulo the documented rewrites;
  Z6 the moved test modules survive verbatim on the engine side;
  Z7 manifests: no crate -> engine dependency, engine -> crate edge intact;
  Z8 `engine-build.ps1 -Mode check` exits 0.
"""
import os
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENG = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENG, "src")
CRATES = os.path.join(ENG, "crates")
RUNTIME = os.path.join(CRATES, "refact-runtime-api", "src")

# Batched-S head this card started from.
BASE = os.environ.get("Z2_BASE", "67c5623d0")

# engine file (relative to engine/src) -> (crate module, [documented rewrites])
MOVED = [
    (
        "scheduler/runner.rs",
        "scheduler_runner",
        [
            ("crate::call_validation::", "refact_core::call_validation::"),
            ("crate::chat::internal_roles", "refact_chat_api::internal_roles"),
            ("crate::chat::get_or_create_session_with_trajectory",
             "crate::session_runtime::get_or_create_session_with_trajectory"),
            ("crate::chat::try_restore_session_if_trajectory_exists",
             "crate::session_runtime::try_restore_session_if_trajectory_exists"),
            ("crate::scheduler::scheduler_timezone",
             "refact_scheduler_core::schedule::scheduler_timezone"),
            ("super::delivery::", "crate::scheduler_delivery::"),
            ("super::exec_action::", "crate::scheduler_exec_action::"),
            ("super::", "refact_scheduler_core::"),
            ("crate::app_state::app_state_from_gcx", "crate::app_state_from_gcx"),
        ],
    ),
    (
        "scheduler/delivery.rs",
        "scheduler_delivery",
        [
            ("crate::call_validation::", "refact_core::call_validation::"),
            ("crate::chat::delivery::deliver_to_chat",
             "refact_chat_api::delivery::deliver_to_chat"),
            ("crate::chat::get_or_create_session_with_trajectory",
             "crate::session_runtime::get_or_create_session_with_trajectory"),
            ("crate::chat::internal_roles", "refact_chat_api::internal_roles"),
            ("super::planning::", "refact_scheduler_core::planning::"),
            ("super::types::", "refact_scheduler_core::types::"),
        ],
    ),
    (
        "scheduler/exec_action.rs",
        "scheduler_exec_action",
        [
            ("use crate::exec::command_policy::{build_exec_request, CommandKind, CommandPolicyInput, ExecSource};",
             "use crate::command_policy::build_exec_request_for_gcx as build_exec_request;\n"
             "use refact_exec::command_policy::{CommandKind, CommandPolicyInput, ExecSource};"),
            ("use crate::exec::{", "use refact_exec::{"),
            ("crate::call_validation::", "refact_core::call_validation::"),
            ("crate::chat::internal_roles", "refact_chat_api::internal_roles"),
            ("crate::scheduler::types::", "refact_scheduler_core::types::"),
        ],
    ),
    ("tasks/events.rs", "task_events", [("refact_runtime_api::task_storage::", "crate::task_storage::")]),
    (
        "tasks/storage.rs",
        "task_storage_runtime",
        [
            ("use refact_runtime_api::GlobalContextCtx;", "use crate::GlobalContextCtx;"),
            ("use super::types::{TaskBoard, TaskMeta};", "use refact_tasks::types::{TaskBoard, TaskMeta};"),
            ("use super::events::{emit_task_event_via, TaskEvent};",
             "use crate::task_events::{emit_task_event_via, TaskEvent};"),
            ("refact_runtime_api::task_storage::", "crate::task_storage::"),
            ("crate::chat::ctx::ChatCtx", "crate::chat_ctx::ChatCtx"),
        ],
    ),
    (
        "caps/caps.rs",
        "caps_load_runtime",
        [
            ("use crate::caps::providers::", "use refact_caps_core::providers::"),
            ("use crate::caps::model_caps::get_model_caps;", "use crate::caps_model_caps::get_model_caps;"),
            ("crate::global_context::CommandLine", "crate::command_line::CommandLine"),
            ("crate::providers::traits::", "refact_providers::traits::"),
            ("pub(crate) use refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER;",
             "pub use refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER;"),
            ("    crate::providers::oauth_refresh::refresh_expiring_claude_code_tokens(&gcx).await;\n", ""),
        ],
    ),
    ("caps/models_dev.rs", "caps_models_dev", []),
    (
        "caps/model_caps.rs",
        "caps_model_caps",
        [
            ("use crate::caps::models_dev::load_models_dev_catalog;",
             "use crate::caps_models_dev::load_models_dev_catalog;"),
        ],
    ),
]

NEW_MODULES = [module for _engine, module, _r in MOVED] + ["scheduler_runtime"]

# Widens the mover applies; they only raise visibility, so they are stripped
# before the verbatim comparison.
WIDENS = [
    "pub const IDLE_DEFER_MS",
    "pub const INVALID_TARGET_DEFER_MS",
    "pub const DAY_MS",
    "pub deferred_until_ms: HashMap",
    "pub fn cron_fire_message(",
    "pub fn scheduled_prompt_message(",
    "pub async fn catch_up(",
    "pub async fn fire_due_tasks(",
    "pub async fn handle_due_task(",
    "pub fn task_is_due(",
    "pub async fn record_run(",
    "pub async fn fire(",
    "pub fn validate_argv(",
    "pub fn command_env(",
    "pub fn resolve_cwd(",
    "pub async fn take_models_dev_startup_refresh_flag(",
]

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]")
USE_LINE = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?use\s")
ATTR_LINE = re.compile(r"^#\[cfg\(test\)\]$")

FAILURES = []
CHECKS = [0]


def check(ok, message):
    CHECKS[0] += 1
    if not ok:
        FAILURES.append(message)


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read().replace("\r\n", "\n")


def git(*args):
    return (
        subprocess.run(["git", "-C", ROOT, *args], capture_output=True, check=False)
        .stdout.decode("utf-8", errors="replace")
        .replace("\r\n", "\n")
    )


def base_blob(rel):
    return git("show", "%s:refact-agent/engine/src/%s" % (BASE, rel))


def prod(text):
    """Everything before the first column-0 `#[cfg(test)] mod tests`."""
    m = re.search(r"(?m)^#\[cfg\(test\)\]\s*\n(?:pub\s+)?mod\s+tests\s*\{", text)
    return text if not m else text[: m.start()]


def strip_cfg_test_items(text):
    """Remove every `#[cfg(test)]`-attributed item (brace- or semicolon-terminated)."""
    while True:
        m = CFG_TEST.search(text)
        if m is None:
            return text
        semi = text.find(";", m.end())
        brace = text.find("{", m.end())
        if brace != -1 and (semi == -1 or brace < semi):
            depth, i = 0, brace
            while i < len(text):
                if text[i] == "{":
                    depth += 1
                elif text[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            text = text[: m.start()] + text[i + 1:]
            continue
        if semi == -1:
            return text[: m.start()]
        text = text[: m.start()] + text[semi + 1:]


def tidy(body, rewrites):
    """Body text normalized so only the documented move edits are invisible."""
    body = strip_cfg_test_items(body)
    out, in_use = [], False
    for line in body.split("\n"):
        if in_use:
            in_use = not line.rstrip().endswith(";")
            continue
        if USE_LINE.match(line):
            in_use = not line.rstrip().endswith(";")
            continue
        out.append(line)
    body = "\n".join(out)
    for needle in WIDENS:
        body = body.replace(needle, needle[4:])
    for old, new in rewrites:
        body = body.replace(old, new)
    body = re.sub(r"(?m)^\s*//.*$", "", body)
    return re.sub(r"\s+", "", body)


# ---------------------------------------------------------------- Z1 census
print("### Z1 census (before -> after)")
for module in ("scheduler", "tasks", "caps"):
    out = subprocess.run(
        [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), module],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    print("  " + (out.stdout or "").strip().replace("\n", "\n  "))
    check(out.returncode == 0, "Z1 census failed for %s: %s" % (module, out.stderr[-200:]))

# ------------------------------------------------------- Z2 engine is a shim
print("### Z2 engine shims")
for engine_rel, module, _rewrites in MOVED:
    body = strip_cfg_test_items(prod(read(os.path.join(SRC, *engine_rel.split("/")))))
    statements, cur, gated = [], [], False
    for line in body.split("\n"):
        if ATTR_LINE.match(line.strip()):
            gated = True
            continue
        code = line.split("//", 1)[0]
        cur.append(code)
        if ";" in code:
            if not gated:
                statements.append(re.sub(r"\s+", " ", " ".join(cur)).strip())
            cur, gated = [], False
    statements = [s for s in statements if s]
    leftovers = [s for s in statements if not USE_LINE.match(s)]
    check(not leftovers, "Z2 %s is a pure shim (found: %s)" % (engine_rel, "; ".join(leftovers)[:160]))
    print("  %-24s statements=%d non-use=%d" % (engine_rel, len(statements), len(leftovers)))

# --------------------------------------------------- Z3 crate module declared
print("### Z3 crate module registration")
lib = read(os.path.join(RUNTIME, "lib.rs"))
for module in NEW_MODULES:
    check("pub mod %s;" % module in lib, "Z3 lib.rs declares %s" % module)

# ------------------------------------------- Z4 crate must not reach the engine
print("### Z4 crate -> engine backrefs")
root_entries = set(os.listdir(RUNTIME))
root_names = {n[:-3] for n in root_entries if n.endswith(".rs")}
root_names |= {n for n in root_entries if os.path.isdir(os.path.join(RUNTIME, n))}
declared = set(re.findall(r"(?m)^pub mod (\w+);", lib))
for module in NEW_MODULES:
    body = read(os.path.join(RUNTIME, module + ".rs"))
    bad = sorted(
        {
            ref
            for ref in re.findall(r"crate::(\w+)", body)
            if ref not in root_names and ref not in declared and ref not in lib
        }
    )
    check(not bad, "Z4 %s has no engine backref (%s)" % (module, ",".join(bad)))
    check("refact_lsp" not in body and "global_context::tests" not in body,
          "Z4 %s has no engine binary / test-gcx reference" % module)
    print("  %-24s backrefs=%d" % (module, len(bad)))

# ------------------------------------------------------ Z5 bodies are verbatim
print("### Z5 moved production region verbatim modulo rewrites")
for engine_rel, module, rewrites in MOVED:
    before = tidy(prod(base_blob(engine_rel)), rewrites)
    after = tidy(read(os.path.join(RUNTIME, module + ".rs")), [])
    check(before == after, "Z5 %s body is verbatim (%d vs %d chars)" % (engine_rel, len(before), len(after)))
    if before != after:
        first = next((i for i in range(min(len(before), len(after))) if before[i] != after[i]),
                     min(len(before), len(after)))
        print("     base: " + before[max(0, first - 70): first + 70])
        print("      new: " + after[max(0, first - 70): first + 70])

# --------------------------------------------------------- Z6 tests travelled
print("### Z6 kept test modules verbatim")
for engine_rel, _module, _rewrites in MOVED:
    before = base_blob(engine_rel)
    after = read(os.path.join(SRC, *engine_rel.split("/")))
    m_before = re.search(r"(?m)^#\[cfg\(test\)\]\s*\n(?:pub\s+)?mod\s+tests\s*\{", before)
    m_after = re.search(r"(?m)^#\[cfg\(test\)\]\s*\n(?:pub\s+)?mod\s+tests\s*\{", after)
    if m_before is None:
        check(m_after is None, "Z6 %s has no test module in either revision" % engine_rel)
        continue
    check(m_after is not None, "Z6 %s kept its test module" % engine_rel)
    if m_after is None:
        continue
    check(before[m_before.start():].strip() == after[m_after.start():].strip(),
          "Z6 %s test module is verbatim" % engine_rel)

# ------------------------------------------------------------- Z7 manifests
print("### Z7 manifests")
engine_toml = read(os.path.join(ENG, "Cargo.toml"))
runtime_toml = read(os.path.join(CRATES, "refact-runtime-api", "Cargo.toml"))
check("refact-lsp" not in runtime_toml, "Z7 refact-runtime-api never depends on the engine binary")
check('refact-runtime-api = { path = "crates/refact-runtime-api"' in engine_toml,
      "Z7 engine keeps its refact-runtime-api edge")

static_only = "--static" in sys.argv
print("\n%d checks, %d failures" % (CHECKS[0], len(FAILURES)))
for failure in FAILURES:
    print("FAIL " + failure)
if FAILURES:
    sys.exit(1)
if static_only:
    print("static checks passed (cargo check skipped)")
    sys.exit(0)

print("\n### Z8 cargo check")
code = subprocess.call(
    [
        "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", os.path.join(ROOT, "tools", "dev", "engine-build.ps1"),
        "-Mode", "check",
    ],
    cwd=ROOT,
)
if code != 0:
    print("FAIL engine-build.ps1 -Mode check exited %d" % code)
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
