"""Z-2 mover: harvest the movable `scheduler`, `tasks` and `caps` engine files
into `refact-runtime-api`, leaving at the old engine paths a `pub use` shim plus
the verbatim `#[cfg(test)]` items and test modules.

Pattern (established by l6-move.py / p1-move.py):
  * crate body  = production region verbatim + documented path rewrites
                  + documented private -> pub widens;
  * engine file = `pub use refact_runtime_api::<mod>::*;` + the
                  `#[cfg(test)]`-gated re-imports the kept test module needs
                  + the verbatim `#[cfg(test)]` items and test module.

The engine's `EngineSchedulerRuntime` / `EngineTaskStorageRuntime` are
implementations of runtime-api traits, so they travel with their module and are
re-exported by the shim.

Deterministic and idempotent (the source is read from the pre-move commit);
run from the repo root:

    python tools/dev/z2-move.py

Read-only proof of the result: tools/dev/z2-check.py
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENGINE, "src")
CRATE = os.path.join(ENGINE, "crates", "refact-runtime-api", "src")

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import importlib.util

_spec = importlib.util.spec_from_file_location(
    "l6_move", os.path.join(os.path.dirname(os.path.abspath(__file__)), "l6-move.py")
)
l6 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(l6)

# l6's proven helpers, reused verbatim.
nested_use_statements = l6.nested_use_statements
imported_names = l6.imported_names
precise_imports = l6.precise_imports
names_an_identifier = l6.names_an_identifier
trait_names = l6.trait_names

BASE = "67c5623d0"

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]")
TEST_MODULE = re.compile(r"(?m)^#\[cfg\(test\)\]\s*\n(?:pub\s+)?mod\s+tests\s*\{")

MODULES = [
    {
        "engine": "scheduler/runner.rs",
        "module": "scheduler_runner",
        "rewrites": [
            (
                "use crate::call_validation::ChatMessage;",
                "use refact_core::call_validation::ChatMessage;",
            ),
            (
                "use crate::chat::internal_roles::{event, EventSubkind};",
                "use refact_chat_api::internal_roles::{event, EventSubkind};",
            ),
            (
                "use crate::chat::get_or_create_session_with_trajectory;",
                "use crate::session_runtime::get_or_create_session_with_trajectory;",
            ),
            (
                "use crate::chat::try_restore_session_if_trajectory_exists;",
                "use crate::session_runtime::try_restore_session_if_trajectory_exists;",
            ),
            (
                "use crate::scheduler::scheduler_timezone;",
                "use refact_scheduler_core::schedule::scheduler_timezone;",
            ),
            ("use super::delivery::", "use crate::scheduler_delivery::"),
            ("use super::exec_action::", "use crate::scheduler_exec_action::"),
            ("use super::jitter::", "use refact_scheduler_core::jitter::"),
            ("use super::retry::", "use refact_scheduler_core::retry::"),
            ("use super::schedule::", "use refact_scheduler_core::schedule::"),
            ("use super::planning::", "use refact_scheduler_core::planning::"),
            ("use super::store::", "use refact_scheduler_core::store::"),
            ("use super::types::", "use refact_scheduler_core::types::"),
            ("crate::app_state::app_state_from_gcx", "crate::app_state_from_gcx"),
            ("crate::call_validation::", "refact_core::call_validation::"),
        ],
        "widen": [
            "const IDLE_DEFER_MS",
            "const INVALID_TARGET_DEFER_MS",
            "const DAY_MS",
            "deferred_until_ms: HashMap",
            "fn cron_fire_message(",
            "fn scheduled_prompt_message(",
            "async fn catch_up(",
            "async fn fire_due_tasks(",
            "async fn handle_due_task(",
            "fn task_is_due(",
            "async fn record_run(",
            "async fn fire(",
        ],
        # Only the kept engine test module constructs this; the crate body does not.
        "drop_imports": ["CronRunRecord"],
    },
    {
        "engine": "scheduler/delivery.rs",
        "module": "scheduler_delivery",
        "rewrites": [
            (
                "use crate::call_validation::{ChatContent, ChatMessage};",
                "use refact_core::call_validation::{ChatContent, ChatMessage};",
            ),
            (
                "crate::chat::delivery::deliver_to_chat",
                "refact_chat_api::delivery::deliver_to_chat",
            ),
            (
                "use crate::chat::get_or_create_session_with_trajectory;",
                "use crate::session_runtime::get_or_create_session_with_trajectory;",
            ),
            (
                "use crate::chat::internal_roles::{event, EventSubkind};",
                "use refact_chat_api::internal_roles::{event, EventSubkind};",
            ),
            (
                "super::planning::scheduled_thread_patch",
                "refact_scheduler_core::planning::scheduled_thread_patch",
            ),
            ("use super::types::", "use refact_scheduler_core::types::"),
        ],
        "widen": [],
    },
    {
        "engine": "scheduler/exec_action.rs",
        "module": "scheduler_exec_action",
        "rewrites": [
            (
                "use crate::exec::command_policy::{build_exec_request, CommandKind, CommandPolicyInput, ExecSource};",
                "use crate::command_policy::build_exec_request_for_gcx as build_exec_request;\n"
                "use refact_exec::command_policy::{CommandKind, CommandPolicyInput, ExecSource};",
            ),
            ("use crate::exec::{", "use refact_exec::{"),
            (
                "use crate::call_validation::{ChatContent, ChatMessage};",
                "use refact_core::call_validation::{ChatContent, ChatMessage};",
            ),
            (
                "use crate::chat::internal_roles::{event, EventSubkind};",
                "use refact_chat_api::internal_roles::{event, EventSubkind};",
            ),
            ("use crate::scheduler::types::", "use refact_scheduler_core::types::"),
            ("crate::scheduler::types::", "refact_scheduler_core::types::"),
        ],
        "widen": ["fn validate_argv(", "fn command_env(", "fn resolve_cwd("],
    },
    {
        "engine": "tasks/events.rs",
        "module": "task_events",
        "rewrites": [
            ("refact_runtime_api::task_storage::", "crate::task_storage::"),
        ],
        "widen": [],
    },
    {
        "engine": "tasks/storage.rs",
        "module": "task_storage_runtime",
        "rewrites": [
            ("use refact_runtime_api::GlobalContextCtx;", "use crate::GlobalContextCtx;"),
            (
                "use super::types::{TaskBoard, TaskMeta};",
                "use refact_tasks::types::{TaskBoard, TaskMeta};",
            ),
            (
                "use super::events::{emit_task_event_via, TaskEvent};",
                "use crate::task_events::{emit_task_event_via, TaskEvent};",
            ),
            ("refact_runtime_api::task_storage::", "crate::task_storage::"),
            ("crate::chat::ctx::ChatCtx", "crate::chat_ctx::ChatCtx"),
        ],
        "widen": [],
    },
    {
        "engine": "caps/caps.rs",
        "module": "caps_load_runtime",
        "rewrites": [
            ("use crate::caps::providers::", "use refact_caps_core::providers::"),
            (
                "use crate::caps::model_caps::get_model_caps;",
                "use crate::caps_model_caps::get_model_caps;",
            ),
            ("crate::global_context::CommandLine", "crate::command_line::CommandLine"),
            ("crate::providers::traits::", "refact_providers::traits::"),
            (
                "pub(crate) use refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER;",
                "pub use refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER;",
            ),
        ],
        "widen": ["async fn take_models_dev_startup_refresh_flag("],
        # `load_caps` loses its one engine-only statement; the engine wrapper in
        # `EngineCapsLoader::load_caps` runs it immediately before calling in.
        "drop_lines": [
            "    crate::providers::oauth_refresh::refresh_expiring_claude_code_tokens(&gcx).await;\n"
        ],
    },
    {
        "engine": "caps/models_dev.rs",
        "module": "caps_models_dev",
        "rewrites": [],
        "widen": [],
    },
    {
        "engine": "caps/model_caps.rs",
        "module": "caps_model_caps",
        "rewrites": [
            (
                "use crate::caps::models_dev::load_models_dev_catalog;",
                "use crate::caps_models_dev::load_models_dev_catalog;",
            ),
        ],
        "widen": [],
    },
]

SCHEDULER_MOD_OLD = (
    "pub use runner::{runner_change_notify, session_cron_store, CronRunner, spawn, spawn_if_enabled};"
)
SCHEDULER_MOD_NEW = (
    "pub use refact_runtime_api::scheduler_runner::{\n"
    "    runner_change_notify, session_cron_store, CronRunner, spawn, spawn_if_enabled,\n"
    "};\n"
    "pub use refact_runtime_api::scheduler_runtime::EngineSchedulerRuntime;"
)
SCHEDULER_RUNTIME_START = "pub struct EngineSchedulerRuntime;"

SCHEDULER_RUNTIME_BODY = """use std::sync::Arc;

use async_trait::async_trait;
use refact_scheduler_core::CronStore;

pub struct EngineSchedulerRuntime;

#[async_trait]
impl crate::scheduler::SchedulerRuntime for EngineSchedulerRuntime {
    async fn active_durable_cron_store_for_services(
        &self,
        paths: &crate::services::PathServices,
        workspace: &crate::services::WorkspaceServices,
    ) -> Result<Option<Arc<dyn CronStore>>, String> {
        crate::scheduler::cron_store_for_project(
            crate::files_correction::get_active_project_path_for_services(paths, workspace).await,
        )
    }
}
"""

LIB_MODULES = [
    "scheduler_runner",
    "scheduler_delivery",
    "scheduler_exec_action",
    "scheduler_runtime",
    "task_events",
    "task_storage_runtime",
    "caps_load_runtime",
    "caps_models_dev",
    "caps_model_caps",
]


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def write(path, text):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)


def source_text(rel):
    """The pristine pre-move engine file, so re-running the mover is idempotent."""
    out = subprocess.run(
        ["git", "-C", ENGINE, "show", "%s:refact-agent/engine/src/%s" % (BASE, rel)],
        capture_output=True,
        check=True,
    ).stdout.decode("utf-8")
    return out.replace("\r\n", "\n")


def item_end(text, attr_start):
    """End index (exclusive) of the item attributed by the `#[cfg(test)]` at `attr_start`.

    The item ends at its closing brace (a `mod`/`fn`/`impl` item) or at its
    terminating `;` (a `use`/`const`/`static` item), whichever comes first.
    Cutting at the first `;` alone would stop inside a function body.
    """
    m = CFG_TEST.search(text, attr_start)
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
                    return i + 1
            i += 1
        return len(text)
    return len(text) if semi == -1 else semi + 1


def extract_cfg_test_items(text):
    """`text` without its `#[cfg(test)]` items, plus those items verbatim."""
    items, cursor = [], 0
    while True:
        m = CFG_TEST.search(text, cursor)
        if m is None:
            return text, items
        end = item_end(text, m.start())
        items.append(text[m.start():end].rstrip("\n"))
        text = text[: m.start()] + text[end:]
        cursor = m.start()


def drop_import_names(body, names):
    """Remove names from the module's own `use` statements.

    Used for imports that only the kept engine test module needs: carrying them
    into the crate body would leave an unused import there.
    """
    lines = body.split("\n")
    out, in_use = [], False
    for line in lines:
        stripped = line.strip()
        if in_use:
            target = True
            if stripped.endswith(";"):
                in_use = False
        elif stripped.startswith("use ") or stripped.startswith("pub use "):
            target = True
            in_use = not stripped.endswith(";")
        else:
            target = False
        if target:
            for name in names:
                line = re.sub(r"\b%s\b\s*,\s*" % re.escape(name), "", line)
                line = re.sub(r",\s*\b%s\b" % re.escape(name), "", line)
        out.append(line)
    return "\n".join(out)


def widen(text, needles, log, module):
    lines = text.split("\n")
    for needle in needles:
        for i, line in enumerate(lines):
            stripped = line.strip()
            if stripped.startswith(needle) and not stripped.startswith("pub "):
                indent = line[: len(line) - len(line.lstrip())]
                lines[i] = indent + "pub " + stripped
                log.append("     widen %s L%d %s" % (module, i + 1, stripped[:56]))
                break
        else:
            raise SystemExit("widen needle not found in %s: %s" % (module, needle))
    return "\n".join(lines)


def test_prelude(prod, kept_text):
    """`#[cfg(test)]`-gated re-imports the shim's kept test code needs.

    The shim's glob carries only `pub` items, so every private `use` of the old
    production region that the kept test code mentions has to be re-imported
    behind `#[cfg(test)]`.
    """
    already = set()
    for statement in nested_use_statements(kept_text):
        already |= imported_names(statement)
    needed = trait_names(kept_text)
    for statement in l6.use_statements(prod):
        for name in imported_names(statement):
            if name in already:
                continue
            if names_an_identifier(kept_text, name):
                needed.add(name)
    pre = []
    for statement in l6.use_statements(prod):
        for line in precise_imports(statement, needed):
            if line not in pre:
                pre.append(line)
    return pre


def main():
    log = []
    for spec in MODULES:
        path = os.path.join(SRC, *spec["engine"].split("/"))
        raw = source_text(spec["engine"])
        m = TEST_MODULE.search(raw)
        if m is None:
            prod, test = raw, ""
        else:
            prod, test = raw[: m.start()], raw[m.start():]
        prod, gated = extract_cfg_test_items(prod)

        for needle in spec.get("drop_lines", []):
            if needle not in prod:
                raise SystemExit("drop-line miss in %s: %r" % (spec["engine"], needle))
            prod = prod.replace(needle, "")
        for old, _new in spec["rewrites"]:
            if old not in prod:
                raise SystemExit("rewrite miss in %s: %s" % (spec["engine"], old))
        body = prod
        for old, new in spec["rewrites"]:
            body = body.replace(old, new)
        body = drop_import_names(body, spec.get("drop_imports", []))
        body = widen(body, spec["widen"], log, spec["module"]).strip("\n")

        kept_text = "\n".join(gated + [test]) if gated else test
        pre = test_prelude(prod, kept_text)
        shim = ["pub use refact_runtime_api::%s::*;" % spec["module"], ""]
        for line in pre:
            shim.extend(["#[cfg(test)]", line])
        if pre:
            shim.append("")
        shim.extend(gated)
        if gated:
            shim.append("")
        shim.append(test.rstrip("\n"))
        shim_text = "\n".join(shim).rstrip("\n") + "\n"

        write(os.path.join(CRATE, spec["module"] + ".rs"), body + "\n")
        write(path, shim_text)
        log.append(
            "%-24s -> %-24s prod %4d -> crate | prelude %2d gated %d"
            % (
                spec["engine"],
                spec["module"] + ".rs",
                len([l for l in body.split("\n") if l.strip()]),
                len(pre),
                len(gated),
            )
        )
        for line in pre:
            log.append("     prelude: " + line)

    write(os.path.join(CRATE, "scheduler_runtime.rs"), SCHEDULER_RUNTIME_BODY)

    # `caps::load_caps` loses its single engine-only statement; the engine wrapper
    # runs it immediately before calling into the crate.
    app_state_path = os.path.join(SRC, "app_state.rs")
    app_state = read(app_state_path).replace("\r\n", "\n")
    oauth_call = (
        "        crate::providers::oauth_refresh::refresh_expiring_claude_code_tokens(&gcx).await;\n"
    )
    if oauth_call not in app_state:
        anchor = "        crate::caps::load_caps(cmdline, gcx).await\n"
        if anchor not in app_state:
            raise SystemExit("app_state.rs EngineCapsLoader::load_caps not found")
        app_state = app_state.replace(anchor, oauth_call + anchor, 1)
        write(app_state_path, app_state)
        log.append("app_state.rs EngineCapsLoader::load_caps runs the oauth pre-refresh")

    mod_path = os.path.join(SRC, "scheduler", "mod.rs")
    mod_text = read(mod_path).replace("\r\n", "\n")
    if SCHEDULER_MOD_OLD in mod_text:
        mod_text = mod_text.replace(SCHEDULER_MOD_OLD, SCHEDULER_MOD_NEW)
    runtime_start = mod_text.find(SCHEDULER_RUNTIME_START)
    if runtime_start != -1:
        mod_text = mod_text[:runtime_start].rstrip("\n") + "\n"
    write(mod_path, mod_text)
    log.append("scheduler/mod.rs repointed at the crate runner + runtime seam")

    lib_path = os.path.join(CRATE, "lib.rs")
    lib = read(lib_path)
    anchor = "pub mod scheduler;\n"
    if anchor not in lib:
        raise SystemExit("refact-runtime-api lib.rs has no `pub mod scheduler;`")
    additions = "".join(
        "pub mod %s;\n" % name for name in LIB_MODULES if "pub mod %s;\n" % name not in lib
    )
    if additions:
        lib = lib.replace(anchor, anchor + additions, 1)
        write(lib_path, lib)
    log.append("refact-runtime-api/lib.rs declares %d new modules" % len(LIB_MODULES))

    print("\n".join(log))


if __name__ == "__main__":
    main()
