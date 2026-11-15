"""AA-4 verification: read-only assertions about the http harvest.

    python tools/dev/aa4-check.py

Checks (C1..C9):
  C1  every path mirror exists in refact-http for the AA-4 harvested modules
  C2  the harvested crate files contain no engine-resident `crate::` path in
      their production body (`#[cfg(test)]` regions ignored)
  C3  the engine side of each harvested module is a glob shim
      (`pub use refact_http::…::*;`)
  C4  every `#[cfg(test)]` block of the pre-move engine file is preserved
      verbatim inside the engine shim (line-for-line containment)
  C5  refact-http/Cargo.toml does not mention `refact-lsp`
  C6  no crate under crates/ depends on refact-lsp
  C7  refact-http does not depend on an engine-only crate that is absent from
      the workspace members list
  C8  every `tasks` handler the engine router aggregator imports still exists,
      `pub`, in refact-http
  C9  no crate-to-crate cycle: nothing refact-http depends on reaches
      refact-http
"""
import os
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "http" / "routers" / "v1"
CRATE = ENG / "crates" / "refact-http" / "src" / "routers" / "v1"

HARVESTED = ["ast", "caps", "tasks"]

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\(test\)[^\n]*\]")
ENGINE_PATH = re.compile(r"\bcrate::[A-Za-z_][A-Za-z0-9_]*")

fails = []
checks = 0


def check(cond, label):
    global checks
    checks += 1
    if not cond:
        fails.append(label)


def read(p):
    return p.read_text(encoding="utf-8")


def prod_body(text):
    """Strip every `#[cfg(test)]` item (module declaration or braced module)."""
    while True:
        m = CFG_TEST.search(text)
        if m is None:
            return text
        rest = text[m.end():].lstrip()
        if rest.startswith("mod ") or rest.startswith("pub mod "):
            semi = text.find(";", m.end())
            opening = text.find("{", m.end())
            if semi != -1 and (opening == -1 or semi < opening):
                text = text[:m.start()] + text[semi + 1:]
                continue
            if opening == -1:
                return text[:m.start()]
            depth, i = 0, opening
            while i < len(text):
                if text[i] == "{":
                    depth += 1
                elif text[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            text = text[:m.start()] + text[i + 1:]
            continue
        semi = text.find(";", m.end())
        if semi == -1:
            return text[:m.start()]
        text = text[:m.start()] + text[semi + 1:]


for name in HARVESTED:
    crate_file = CRATE / (name + ".rs")
    engine_file = SRC / (name + ".rs")
    check(crate_file.is_file(), "C1 missing crate file %s" % crate_file)
    check(engine_file.is_file(), "C1 missing engine file %s" % engine_file)
    if not crate_file.is_file() or not engine_file.is_file():
        continue
    crate_text = read(crate_file)
    engine_text = read(engine_file)
    body = prod_body(crate_text)
    leftovers = sorted(set(ENGINE_PATH.findall(body)))
    check(
        not leftovers,
        "C2 %s prod body still references engine paths: %s" % (name, leftovers),
    )
    check(
        engine_text.startswith("pub use refact_http::routers::v1::%s::*;" % name),
        "C3 %s engine side is not a glob shim" % name,
    )
    tests = crate_text[len(body):]
    for line in tests.splitlines():
        if line.strip():
            check(line in engine_text, "C4 %s engine shim lost test line %r" % (name, line.strip()))

cargo = read(ENG / "crates" / "refact-http" / "Cargo.toml")
check("refact-lsp" not in cargo, "C5 refact-http/Cargo.toml mentions refact-lsp")

for crate_toml in (ENG / "crates").glob("*/Cargo.toml"):
    text = read(crate_toml)
    check(
        "refact-lsp" not in text,
        "C6 %s depends on refact-lsp" % crate_toml.parent.name,
    )

members = read(ENG / "Cargo.toml")
for dep in re.findall(r"^(refact-[a-z0-9-]+)\s*=", cargo, re.M):
    check('"%s"' % dep in members or dep in members, "C7 refact-http dep %s not a workspace member" % dep)

# C8: every tasks handler the engine router aggregates still exists, `pub`, in the crate.
router_agg = read(SRC.parent / "v1.rs")
imported = set()
for m in re.finditer(r"use crate::http::routers::v1::tasks::\{(.*?)\};", router_agg, re.S):
    for name in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", m.group(1)):
        imported.add(name)
check(imported, "C8 no tasks import block found in the engine router aggregator")
tasks_crate = read(CRATE / "tasks.rs")
for name in sorted(imported):
    check(
        re.search(
            r"(?m)^\s*pub(?:\([^)]*\))?\s+(?:async\s+)?fn\s+%s\b" % re.escape(name), tasks_crate
        )
        is not None,
        "C8 tasks handler %s missing from refact-http" % name,
    )

# C9: no crate cycle -- nothing refact-http depends on may reach refact-http.
deps = {}
for crate_toml in (ENG / "crates").glob("*/Cargo.toml"):
    own = crate_toml.parent.name
    deps[own] = set(re.findall(r"^(refact-[a-z0-9-]+)\s*=", read(crate_toml), re.M)) | {own}


def reaches(start, goal):
    seen, stack = set(), list(deps.get(start, ()))
    while stack:
        node = stack.pop()
        if node == goal:
            return True
        if node in seen:
            continue
        seen.add(node)
        stack.extend(deps.get(node, ()))
    return False


for dep in sorted(set(re.findall(r"^(refact-[a-z0-9-]+)\s*=", cargo, re.M))):
    check(not reaches(dep, "refact-http"), "C9 cycle: %s reaches refact-http" % dep)

# C10: zero-behaviour-change proof -- reverse the import retargets on the crate's
# production body and compare it with the pre-move engine body from git.
REVERSED = {}


def add_reverse(old, new):
    REVERSED[new] = old


for old, new in [
    ("use crate::app_state::AppState;", "use refact_runtime_api::AppState;"),
    ("use crate::chat::ctx::ChatCtx;", "use refact_runtime_api::chat_ctx::ChatCtx;"),
    ("use crate::custom_error::ScratchError;", "use refact_core::custom_error::ScratchError;"),
    (
        "use crate::files_correction::get_unscoped_project_dirs_for_ctx;",
        "use refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx;",
    ),
    (
        "use crate::worktrees::service::WorktreeService;",
        "use refact_worktrees::service::WorktreeService;",
    ),
    (
        "use crate::tasks::comments::{self, CreateCardComment};",
        "use refact_runtime_api::task_comments::create_card_comment_for_ctx;\n"
        "use refact_tasks::comments::CreateCardComment;",
    ),
    (
        "use crate::tasks::types::{BoardCard, StatusUpdate, TaskBoard, TaskMeta, TaskStatus, TeamMember, TrajectoryInfo};",
        "use refact_tasks::types::{\n"
        "    BoardCard, StatusUpdate, TaskBoard, TaskMeta, TaskStatus, TeamMember, TrajectoryInfo,\n"
        "};",
    ),
    (
        "use crate::chat::trajectories::TrajectoryEvent;",
        "use refact_chat_history::trajectory_event::TrajectoryEvent;",
    ),
    (
        "use crate::tasks::events::{TaskEvent, TaskEventEnvelope};",
        "use refact_tasks::events::{TaskEvent, TaskEventEnvelope};",
    ),
    ("use crate::tasks::storage;", "use refact_runtime_api::task_storage_runtime as storage;"),
    ("use crate::tools::tool_task_documents::{", "use refact_tools::tools::tool_task_documents::{"),
    ("use crate::tools::tool_task_memory::{", "use refact_tools::tools::tool_task_memory::{"),
    (
        "crate::tasks::events::enrich_task_with_session_state_for_services(",
        "refact_runtime_api::task_storage::enrich_task_with_session_state_for_services(",
    ),
    ("comments::create_card_comment_for_ctx(", "create_card_comment_for_ctx("),
    (
        "comments::add_comment_to_board(",
        "refact_tasks::comments::add_comment_to_board(",
    ),
    (
        "crate::tasks::events::emit_task_event_for_services(",
        "refact_tasks::events_emit::emit_task_event_for_services(",
    ),
    (
        "crate::tasks::events::emit_task_updated_via(",
        "refact_runtime_api::task_storage::emit_task_updated_via(",
    ),
    ("crate::buddy::actor::buddy_apply(", "refact_runtime_api::buddy_runtime::buddy_apply("),
    (
        "crate::buddy::actor::make_runtime_event(",
        "refact_buddy_core::actor::make_runtime_event(",
    ),
    ("crate::buddy::actor::BuddyMutation", "refact_buddy_core::actor::BuddyMutation"),
    ("crate::buddy::types::BuddyActivity", "refact_buddy_core::types::BuddyActivity"),
    ("crate::chat::SessionsMap", "refact_chat_api::SessionsMap"),
    ("crate::chat::types::ChatSession", "refact_chat_api::types::ChatSession"),
    ("crate::chat::trajectory_index::", "refact_chat_history::trajectory_index::"),
    (
        "crate::chat::trajectories::save_initial_task_chat_trajectory(",
        "refact_runtime_api::trajectory_runtime::save_initial_task_chat_trajectory(",
    ),
    (
        "crate::chat::trajectories::validate_trajectory_id(",
        "refact_chat_history::trajectory_paths::validate_trajectory_id(",
    ),
    (
        "crate::chat::trajectories::find_trajectory_path(",
        "refact_runtime_api::trajectory_runtime::find_trajectory_path(",
    ),
    (
        "crate::chat::trajectories::save_trajectory_snapshot(",
        "refact_runtime_api::trajectory_runtime::save_trajectory_snapshot(",
    ),
    (
        "crate::tools::tool_task_memory::parse_rfc3339_utc",
        "refact_tools::tools::tool_task_memory::parse_rfc3339_utc",
    ),
]:
    add_reverse(old, new)

import subprocess

before = subprocess.run(
    ["git", "show", "HEAD~1:refact-agent/engine/src/http/routers/v1/tasks.rs"],
    cwd=str(ROOT),
    capture_output=True,
    text=True,
).stdout
before_prod = prod_body(before[: before.index("#[cfg(test)]")]).rstrip() + "\n"
after_prod = prod_body(read(CRATE / "tasks.rs")).rstrip() + "\n"
restored = after_prod
for new, old in REVERSED.items():
    restored = restored.replace(new, old)
check(
    restored == before_prod,
    "C10 tasks.rs production body changed beyond the retarget (expected byte-identical "
    "after reversing the import rewrites)",
)

print("AA-4 checks: %d run, %d failed" % (checks, len(fails)))
for f in fails:
    print("  FAIL %s" % f)
sys.exit(1 if fails else 0)
