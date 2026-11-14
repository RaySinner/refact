"""Q-4 mover: harvest the three genuinely movable `http/routers/v1` modules
(`code_intel`, `background_agents`, `file_edit_tools`) into `refact-http`.

Idempotent: it rewrites the engine file into a glob shim plus the verbatim
`#[cfg(test)]` block with an explicit test prelude, and writes the crate file
from the pre-move body with a fixed, documented rewrite table.

Run from the repo root:  python tools/dev/q4-move.py
"""

import os
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "http" / "routers" / "v1"
DST = ENG / "crates" / "refact-http" / "src" / "routers" / "v1"

MODS = ["code_intel", "background_agents", "file_edit_tools"]

REWRITES = {
    "code_intel": [
        ("use crate::app_state::AppState;", "use refact_runtime_api::AppState;"),
        ("use crate::codegraph::code_intel_api::*;", "use refact_code_intel_api::*;"),
        (
            "use crate::custom_error::ScratchError;",
            "use refact_core::custom_error::ScratchError;",
        ),
        (
            "crate::files_in_workspace::get_file_text_from_memory_or_disk",
            "refact_runtime_api::files_in_workspace::get_file_text_from_memory_or_disk",
        ),
        ("crate::tools::tool_codegraph::", "refact_tools::tools::tool_codegraph::"),
        (
            "super::git_read::resolve_requested_root",
            "crate::routers::v1::git_read::resolve_requested_root",
        ),
    ],
    "background_agents": [
        (
            "use crate::agents::types::{BackgroundAgent, BackgroundAgentSummary};",
            "use refact_agents_core::types::{BackgroundAgent, BackgroundAgentSummary};",
        ),
        ("use crate::app_state::AppState;", "use refact_runtime_api::AppState;"),
        (
            "crate::agents::delivery::deliver_to_agent",
            "refact_agents_core::delivery::deliver_to_agent",
        ),
        (
            "crate::agents::spawn::emit_background_agent_update",
            "refact_agents_core::spawn::emit_background_agent_update",
        ),
        ("crate::chat::internal_roles::", "refact_chat_api::internal_roles::"),
        (
            "crate::tools::tool_agent_interact::records_for_root",
            "refact_tools::tools::tool_agent_interact::records_for_root",
        ),
    ],
    "file_edit_tools": [
        ("use crate::app_state::AppState;", "use refact_runtime_api::AppState;"),
        (
            "use crate::call_validation::DiffChunk;",
            "use refact_core::call_validation::DiffChunk;",
        ),
        (
            "use crate::custom_error::ScratchError;",
            "use refact_core::custom_error::ScratchError;",
        ),
        ("crate::tools::file_edit::", "refact_tools::tools::file_edit::"),
    ],
}

# Non-pub engine items the kept test module reaches through `super::`.
WIDEN = {
    "code_intel": ["fn select_graph_node_ids("],
    "background_agents": [],
    "file_edit_tools": [],
}

TRAIT_METHOD_HINTS = {
    "tool_execute": "Tool",
    "tool_description": "Tool",
    "tool_depends_on": "Tool",
    "tool_ctx": "GlobalContextCtx",
    "chat_ctx": "GlobalContextCtx",
    "app_state": "GlobalContextCtx",
}


def split_test(text):
    lines = text.replace("\r\n", "\n").split("\n")
    for i, line in enumerate(lines):
        if i > 5 and line.strip() == "#[cfg(test)]":
            return lines[:i], lines[i:]
    return lines, []


def use_statements(text):
    return re.findall(r"(?m)^use [^;]*;", text)


def nested_use_statements(text):
    return re.findall(r"(?m)^\s*use [^;]*;", text)


def imported_names(statement):
    m = re.match(r"(?s)^\s*use (.*);$", statement.strip())
    body = " ".join(m.group(1).split())
    names = set()
    if body.endswith("::*"):
        return names
    brace = re.search(r"\{([^}]*)\}$", body)
    if brace:
        prefix = body[: body.index("{")].rstrip(":").rstrip()
        for part in brace.group(1).split(","):
            part = part.strip()
            if not part:
                continue
            if part == "self":
                names.add(prefix.split("::")[-1].strip())
            else:
                names.add(part.split(" as ")[-1].strip())
    else:
        names.add(body.split(" as ")[-1].split("::")[-1].strip())
    return names


def precise_imports(statement, identifiers):
    m = re.match(r"(?s)^use (.*);$", statement)
    body = " ".join(m.group(1).split())
    if body.endswith("::*"):
        return []
    if " as " in body:
        alias = body.split(" as ")[-1].strip()
        return [statement] if alias in identifiers else []
    brace = re.search(r"\{([^}]*)\}$", body)
    if brace:
        prefix = body[: body.index("{")].rstrip(":").rstrip()
        out = []
        for part in brace.group(1).split(","):
            name = part.strip()
            if not name:
                continue
            if name == "self":
                leaf = prefix.split("::")[-1].strip()
                if leaf in identifiers:
                    out.append(statement)
            elif name in identifiers:
                out.append("use %s::%s;" % (prefix, name))
        return out
    name = body.split("::")[-1].strip()
    return [statement] if name in identifiers else []


def names_an_identifier(text, name):
    return re.search(r"(?<![\w])%s\b" % re.escape(name), text) is not None


def trait_names(test_text):
    return {
        trait
        for method, trait in TRAIT_METHOD_HINTS.items()
        if re.search(r"\.%s\(" % method, test_text)
    }


def test_prelude(prod, test):
    test_text = "\n".join(test)
    already = set()
    for statement in nested_use_statements(test_text):
        already |= imported_names(statement)
    needed = trait_names(test_text)
    prod_use = "\n".join(prod)
    for statement in use_statements(prod_use):
        for name in imported_names(statement):
            if name in already:
                continue
            if names_an_identifier(test_text, name):
                needed.add(name)
    pre = []
    for statement in use_statements(prod_use):
        for line in precise_imports(statement, needed):
            if line not in pre:
                pre.append(line)
    return pre


def main():
    log = []
    for mod in MODS:
        engine_path = SRC / (mod + ".rs")
        crate_path = DST / (mod + ".rs")
        raw = engine_path.read_text(encoding="utf-8")
        if raw.startswith("pub use refact_http::routers::v1::%s::*;" % mod):
            log.append("%s already moved, skipped" % mod)
            continue

        prod, test = split_test(raw)
        for i, line in enumerate(prod):
            for pattern in WIDEN[mod]:
                if line.strip().startswith(pattern):
                    prod[i] = "pub " + line
                    log.append("widen %s L%d" % (mod, i + 1))
        body = "\n".join(prod)
        for old, new in REWRITES[mod]:
            body = body.replace(old, new)

        stray = sorted(
            {m for m in re.findall(r"crate::[\w:]*", body) if not m.startswith("crate::routers::v1::")}
        )
        if stray:
            raise SystemExit("STRAY engine refs left in %s: %s" % (mod, stray))

        crate_path.write_text(body.rstrip("\n") + "\n", encoding="utf-8")

        pre = test_prelude(prod, test)
        shim = "pub use refact_http::routers::v1::%s::*;\n" % mod
        if pre:
            shim += "\n" + "\n".join("#[cfg(test)]\n" + line for line in pre) + "\n"
        if test:
            shim += "\n" + "\n".join(test).rstrip("\n") + "\n"
        engine_path.write_text(shim, encoding="utf-8")
        log.append(
            "%s: prod %d -> crate; test lines %d; prelude %d"
            % (mod, len(prod), len(test), len(pre))
        )
    print("\n".join(log))


if __name__ == "__main__":
    main()
