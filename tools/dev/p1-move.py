"""P-1 mover: harvest the movable `tools/` body into refact-tools.

Moves every file the honest census lists as movable *except* `tool_chrome.rs` and
`design_tools.rs`: their production code calls the engine-resident browser runtime
helpers (`register_browser_runtime`, `remove_browser_runtime`,
`get_browser_profile_dir`, `find_runtime_for_chat`, `select_runtime_for_chat`,
`RuntimeOwner`, `ChatRuntimeLookup` in `src/integrations/browser_runtime.rs`), so
moving them would create a crate -> engine edge. They stay in the engine.

Pattern (proven on 60+ files): the production body moves verbatim, `crate::` paths
are rewritten to the owning crate, module-level items are widened where an engine
consumer reaches them, the file's inline `#[cfg(test)] mod tests { ... }` block
stays in the engine behind a one-line glob shim, and the file's own `use` lines are
re-imported there as a `#[cfg(test)]` prelude.

Only the brace-matched test *module* is treated as test code: several movers carry
production code below the test module, and truncating at the first `#[cfg(test)]`
(what the census warns about) silently drops it.

Idempotent: every moved body is read from the pre-move commit via `git show`, so
re-running reproduces the same output.

Run from the repo root:  python tools/dev/p1-move.py
Read-only proof:         python tools/dev/p1-check.py
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"
BASE = "68efcde49"

# engine-relative path under src/tools -> crate-relative path under src/tools
MOVED = [
    ("tool_review.rs", "tool_review.rs"),
    ("tool_design_system.rs", "tool_design_system.rs"),
    ("tool_spawn_ab.rs", "tool_spawn_ab.rs"),
    ("tool_handoff_to_mode.rs", "tool_handoff_to_mode.rs"),
    ("tool_background_agents.rs", "tool_background_agents.rs"),
    ("tool_task_restart_agent.rs", "tool_task_restart_agent.rs"),
    ("tool_task_overview.rs", "tool_task_overview.rs"),
    ("tool_process_subscribe.rs", "tool_process_subscribe.rs"),
    ("tool_cron_create.rs", "tool_cron_create.rs"),
    ("tool_buddy_get_logs.rs", "tool_buddy_get_logs.rs"),
    ("tool_task_broadcast.rs", "tool_task_broadcast.rs"),
    ("tool_buddy_say.rs", "tool_buddy_say.rs"),
    ("tool_buddy_get_context.rs", "tool_buddy_get_context.rs"),
    ("tool_worktree_merge.rs", "tool_worktree_merge.rs"),
    ("tool_cron_update.rs", "tool_cron_update.rs"),
    ("tool_trajectory_context.rs", "tool_trajectory_context.rs"),
    ("tool_clean_background_processes.rs", "tool_clean_background_processes.rs"),
    ("tool_buddy_launch_investigation.rs", "tool_buddy_launch_investigation.rs"),
    ("tool_web.rs", "tool_web.rs"),
    ("tool_buddy_create_draft.rs", "tool_buddy_create_draft.rs"),
    ("tool_web_search.rs", "tool_web_search.rs"),
    ("tool_buddy_open_view.rs", "tool_buddy_open_view.rs"),
    ("tool_buddy_open_setup_flow.rs", "tool_buddy_open_setup_flow.rs"),
    ("tool_set_trajectory_label.rs", "tool_set_trajectory_label.rs"),
    ("buddy/user_prefs.rs", "buddy/user_prefs.rs"),
    ("buddy/refact_engine.rs", "buddy/refact_engine.rs"),
    ("buddy/surface.rs", "buddy/surface.rs"),
    ("review_agents/mod.rs", "review_agents/mod.rs"),
    ("review_agents/runner.rs", "review_agents/runner.rs"),
    ("tool_codegraph/mod.rs", "tool_codegraph/mod.rs"),
    ("tool_codegraph/health.rs", "tool_codegraph/health.rs"),
    ("tool_codegraph/code_map.rs", "tool_codegraph/code_map.rs"),
    ("tool_codegraph/code_why.rs", "tool_codegraph/code_why.rs"),
    ("tool_codegraph/git_risk.rs", "tool_codegraph/git_risk.rs"),
    ("tool_codegraph/dead_code.rs", "tool_codegraph/dead_code.rs"),
    ("tool_codegraph/tools.rs", "tool_codegraph/tools.rs"),
    ("tool_codegraph/graph.rs", "tool_codegraph/graph.rs"),
    ("tool_codegraph/clones.rs", "tool_codegraph/clones.rs"),
    ("tool_codegraph/pr_blast.rs", "tool_codegraph/pr_blast.rs"),
]

# Files the card leaves in the engine, with the reason the census mis-scored them.
LEFT = {
    "tool_chrome.rs": "calls engine src/integrations/browser_runtime.rs helpers",
    "design_tools.rs": "calls engine src/integrations/browser_runtime.rs helpers",
    "buddy/memory.rs": "imports engine src/buddy/memory_lifecycle.rs helpers",
}

# Ordered longest/most specific first.
REWRITES = [
    ("crate::tools::ctx::", "refact_runtime_api::tool_ctx::"),
    ("crate::tools::review_types::", "refact_tool_api::review_types::"),
    ("crate::tools::review_merge::", "refact_tool_api::review_merge::"),
    ("crate::tools::scope_utils::", "refact_runtime_api::scope_utils::"),
    ("crate::buddy::actor::resolve_buddy_chat_model",
     "refact_runtime_api::buddy_actor_runtime::resolve_buddy_chat_model"),
    ("crate::buddy::actor::", "refact_buddy_core::actor::"),
    ("crate::buddy::runtime_queue::", "refact_buddy_core::runtime_queue::"),
    ("crate::buddy::settings::", "refact_buddy_core::settings::"),
    ("crate::buddy::state::", "refact_buddy_core::state::"),
    ("crate::buddy::storage::", "refact_buddy_core::storage::"),
    ("crate::buddy::types::", "refact_buddy_core::types::"),
    ("crate::agents::delivery::", "refact_agents_core::delivery::"),
    ("crate::agents::push::", "refact_agents_core::push::"),
    ("crate::agents::spawn::", "refact_agents_core::spawn::"),
    ("crate::agents::registry::", "refact_agents_core::registry::"),
    ("crate::agents::tool_agent::ToolAgentHandle",
     "refact_runtime_api::tool_agent::ToolAgentHandle"),
    ("crate::agents::types", "refact_agents_core::types"),
    ("crate::app_state::PathServices", "refact_runtime_api::services::PathServices"),
    ("crate::app_state::WorkspaceServices", "refact_runtime_api::services::WorkspaceServices"),
    ("crate::app_state::BuddyServices", "refact_runtime_api::services::BuddyServices"),
    ("crate::app_state::", "refact_runtime_api::"),
    ("crate::at_commands::at_commands::at_commands_new_from_app",
     "refact_runtime_api::at_commands_new_from_app"),
    ("crate::at_commands::at_commands::at_commands_new_with_abort",
     "refact_runtime_api::at_commands_new_with_abort"),
    ("crate::at_commands::at_web::", "refact_at_web::web_fetch::"),
    ("crate::at_commands::at_web_search::", "refact_at_web::web_search::"),
    ("crate::chat::delivery::deliver_to_chat", "refact_chat_api::delivery::deliver_to_chat"),
    ("crate::chat::history_limit::", "refact_chat_history::history_limit::"),
    ("crate::file_filter::KNOWLEDGE_FOLDER_NAME",
     "refact_files::file_filter::KNOWLEDGE_FOLDER_NAME"),
    ("crate::global_context::", "refact_runtime_api::global_context::"),
    ("crate::knowledge_graph::kg_structs::", "refact_knowledge_graph::kg_structs::"),
    ("crate::scratchpads::multimodality::", "refact_core::chat_types::"),
    ("crate::subchat::SubchatConfig", "refact_chat_api::SubchatConfig"),
    ("crate::llm::params::CacheControl", "refact_core::chat_types::CacheControl"),
]

TEST_CFG = '#[cfg(any(test, feature = "test-util"))]'

# The move brings new direct dependencies into refact-tools.
CARGO_DEPS = [
    'petgraph = "0.6"',
    'dirs = "5.0"',
    'shell-words = "1.1.0"',
    'refact-agentic = { path = "../refact-agentic" }',
    'refact-at-web = { path = "../refact-at-web" }',
    'refact-code-intel-api = { path = "../refact-code-intel-api" }',
    'refact-codewiki = { path = "../refact-codewiki" }',
]

# The tool_codegraph subtree is consumed by the engine http router, tools_list and
# tests_schema, so its tight visibilities widen to `pub`.
BLANKET_PUB_SUBTREES = ("tool_codegraph/",)
BLANKET_PUB_RULES = [("pub(super) ", "pub "), ("pub(crate) ", "pub ")]

# Directory-root modules whose children the engine shim must keep declaring, so
# the engine-side child shims (which hold the engine test modules) stay reachable.
ENGINE_SHIM_CHILDREN = {
    "review_agents/mod.rs": [
        "pub mod config;",
        "pub mod contract;",
        "pub mod prompts;",
        "pub mod runner;",
        "pub mod stages;",
    ],
}

# The crate needs review_agents::contract (the engine's is a refact-tool-api re-export).
CRATE_FACADES = {
    "review_agents/contract.rs": "pub use refact_tool_api::review_contract::*;\n",
}

# Children of a moved directory root: unreachable from the engine once the root is
# a shim, so they must not linger.
ENGINE_DELETED = [
    "tool_codegraph/health.rs",
    "tool_codegraph/code_map.rs",
    "tool_codegraph/code_why.rs",
    "tool_codegraph/git_risk.rs",
    "tool_codegraph/dead_code.rs",
    "tool_codegraph/tools.rs",
    "tool_codegraph/graph.rs",
    "tool_codegraph/clones.rs",
    "tool_codegraph/pr_blast.rs",
]

USE_RE = re.compile(r"(?m)^use [^;]*;")
STRUCT_RE = re.compile(r"^\s*(pub(?:\([a-z_]*\))?\s+)?(struct|enum)\s+(\w+)")
IMPL_RE = re.compile(r"^\s*impl\b|^\s*impl<")
TRAIT_RE = re.compile(r"^\s*(?:pub(?:\([a-z_]*\))?\s+)?(?:unsafe\s+)?trait\b")
FN_HDR_RE = re.compile(r"^\s*(?:pub(?:\([a-z_]*\))?\s+)?(?:async\s+)?fn\b")
TYPE_HDR_RE = re.compile(r"^\s*(?:pub(?:\([a-z_]*\))?\s+)?(?:struct|enum|union)\b")
CFG_TEST_RE = re.compile(r"^#\[cfg\((?:test|any\(test[^)]*\))\)\]$")
TEST_MOD_RE = re.compile(r"^(?:pub )?mod ([A-Za-z_]\w*)\s*\{")


def git_text(rel):
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:refact-agent/engine/src/tools/%s" % (BASE, rel)],
        capture_output=True, check=True,
    ).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def read(path):
    return path.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n")


def module_path(rel):
    p = rel[:-3]
    if p.endswith("/mod"):
        p = p[:-4]
    return p.replace("/", "::")


def module_name(crate_rel):
    p = crate_rel[:-3]
    if p.endswith("/mod"):
        p = p[: -len("/mod")]
    return p.split("/")[0]


def names_in(text, name):
    return re.search(r"(?<![\w])%s(?![\w])" % re.escape(name), text) is not None


RAW_OPEN_RE = re.compile(r'(?:^|[^\w])r(#*)"')


def mask_text(text):
    """Blank out every string literal so brace counting sees structure only.

    These tools embed JSON fixtures in raw strings (`r#"{"a": 1}"#`) that span
    whole lines; a line-local scanner cannot see the closing quote on a later
    line and then counts the fixture's braces as Rust structure. Newlines are
    preserved so the result still splits into the same lines.
    """
    out, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        m = None
        if c == "r" and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            m = re.compile(r'r(#*)"').match(text, i)
        if m:
            hashes = m.group(1)
            end = text.find('"' + hashes, m.end())
            end = n if end == -1 else end + len(hashes) + 1
            out.append(re.sub(r"[^\n]", " ", text[i:end]))
            i = end
        elif text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end == -1 else end
            out.append(re.sub(r"[^\n]", " ", text[i:end]))
            i = end
        elif text.startswith("/*", i):
            end = text.find("*/", i)
            end = n if end == -1 else end + 2
            out.append(re.sub(r"[^\n]", " ", text[i:end]))
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j = min(j + 1, n)
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif c == "'":
            m = re.compile(r"'(?:\\.|[^'\\])'").match(text, i)
            if m:
                out.append(" " * len(m.group(0)))
                i = m.end()
            else:
                out.append(c)
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def mask_lines(lines):
    return mask_text("\n".join(lines)).split("\n")


def extract_test_blocks(lines):
    """Return (production_lines, test_lines).

    Only `#[cfg(test)] mod <name> { ... }` blocks move to the engine shim. The
    production code above *and below* such a block stays in the moved body.
    """
    masked = mask_lines(lines)
    prod, tests, i = [], [], 0
    while i < len(lines):
        if CFG_TEST_RE.match(lines[i].strip()) and i + 1 < len(lines):
            m = TEST_MOD_RE.match(lines[i + 1].strip())
            if m:
                depth, j = 0, i + 1
                while j < len(lines):
                    depth += masked[j].count("{") - masked[j].count("}")
                    if depth == 0:
                        break
                    j += 1
                tests.append("\n".join(lines[i: j + 1]))
                i = j + 1
                continue
        prod.append(lines[i])
        i += 1
    return prod, tests


def scopes(lines):
    masked = mask_lines(lines)
    depth, frames, out = 0, [], []
    for raw, s in zip(lines, masked):
        out.append(tuple(f[0] for f in frames))
        t = s.strip()
        if IMPL_RE.match(t):
            frames.append(("impl-trait" if " for " in t else "impl-inherent", depth + 1))
        elif TRAIT_RE.match(t):
            frames.append(("impl-trait", depth + 1))
        elif FN_HDR_RE.match(t) and t.endswith("{"):
            frames.append(("fn", depth + 1))
        elif TYPE_HDR_RE.match(t):
            frames.append(("type", depth + 1))
        depth += s.count("{") - s.count("}")
        while frames and depth < frames[-1][1]:
            frames.pop()
    return out


def struct_body(lines, i):
    m = STRUCT_RE.match(lines[i])
    if not m:
        return None
    kind, name = m.group(2), m.group(3)
    masked = mask_lines(lines)
    j = i
    while j < len(lines) and "{" not in masked[j]:
        if lines[j].rstrip().endswith(";"):
            return (kind, name, None, None)
        j += 1
    if j >= len(lines):
        return (kind, name, None, None)
    depth = 0
    for k in range(j, len(lines)):
        depth += masked[k].count("{") - masked[k].count("}")
        if depth == 0:
            return (kind, name, j + 1, k)
    return (kind, name, None, None)


def frame_kind(ctx):
    if "impl-trait" in ctx:
        return "trait-impl"
    if "impl-inherent" in ctx:
        return "inherent-impl"
    if "fn" in ctx:
        return "nested"
    return "module"


def is_module_scope(ctx):
    return not any(f in ("fn", "impl-inherent", "impl-trait") for f in ctx)


def widen(lines, reach_text, log, mod):
    """Widen exactly the surface the engine still reaches after the move.

    Module-level types and struct fields become `pub` because both the engine's
    kept test modules and other engine modules name them (a private type in a
    `pub fn` signature is `private_interfaces`). Module-level and inherent-impl
    `fn`/`const`/`static` items become `pub` when they were `pub(crate)`/
    `pub(super)` (the engine called them) or when the surviving engine code names
    them. Trait-impl and nested items are never touched.
    """
    ctxs = scopes(lines)
    for i, line in enumerate(lines):
        where = frame_kind(ctxs[i])
        if where in ("trait-impl", "nested") or "type" in ctxs[i]:
            continue
        tm = re.match(r"^(\s*)(pub(?:\([a-z_]*\))?\s+)?(struct|enum|union|type)\s+(\w+)", line)
        if tm:
            if where != "module":
                continue
            consumed = len(tm.group(1)) + len(tm.group(2) or "") + len(tm.group(3))
            if (tm.group(2) or "").strip() != "pub":
                log.append("type %s (%s)" % (tm.group(4), (tm.group(2) or "priv").strip()))
            lines[i] = tm.group(1) + "pub " + tm.group(3) + line[consumed:]
            continue
        fm = re.match(r"^(\s*)(pub(?:\([a-z_]*\))?\s+)?(?:async\s+)?(fn|const|static)\s+(\w+)", line)
        if fm:
            vis, kind, name = (fm.group(2) or "").strip(), fm.group(3), fm.group(4)
            if vis == "pub":
                continue
            if vis in ("pub(crate)", "pub(super)") or names_in(reach_text, name):
                lines[i] = fm.group(1) + "pub " + line[len(fm.group(1)) + len(fm.group(2) or ""):]
                log.append("%s %s (%s)" % (kind, name, vis or "priv"))
    for i, line in enumerate(lines):
        info = struct_body(lines, i)
        if not info:
            continue
        kind, name, b0, b1 = info
        if kind != "struct" or b0 is None:
            continue
        for j in range(b0, b1):
            if not is_module_scope(ctxs[j]):
                continue
            fm = re.match(r"^(\s*)(pub(?:\([a-z_]*\))?\s+)?([A-Za-z_]\w*)\s*:", lines[j])
            if not fm or (fm.group(2) or "").strip() == "pub":
                continue
            lines[j] = fm.group(1) + "pub " + lines[j][len(fm.group(1)) + len(fm.group(2) or ""):]
            log.append("field %s.%s" % (name, fm.group(3)))
    return lines


def crate_body(rel, prod, reach_text, log):
    out, pending = [], False
    for line in prod:
        if pending:
            pending = False
            if line.strip() == "use crate::global_context::GlobalContext;":
                out.pop()
                log.append("dropped engine GlobalContext import")
                continue
        if line.strip() == "#[cfg(test)]":
            pending = True
        out.append(line)
    lines = widen(out, reach_text, log, rel)
    text = "\n".join(lines)
    if rel.startswith(BLANKET_PUB_SUBTREES):
        for old, new in BLANKET_PUB_RULES:
            text = text.replace(old, new)
    for old, new in REWRITES:
        text = text.replace(old, new)
    text = text.replace("#[cfg(test)]", TEST_CFG)
    return text.rstrip("\n") + "\n"


def shim(rel, prod, tests):
    path = module_path(rel)
    prelude = []
    for statement in USE_RE.findall("\n".join(prod)):
        if statement.startswith("pub("):
            continue
        if re.match(r"^use crate::tools::%s::" % re.escape(path), statement):
            continue
        if statement not in prelude:
            prelude.append(statement)
    parts = ["pub use refact_tools::tools::%s::*;\n" % path]
    children = ENGINE_SHIM_CHILDREN.get(rel)
    if children:
        parts.append("\n".join(children) + "\n")
    if prelude:
        parts.append(
            "\n".join("#[cfg(test)]\n#[allow(unused_imports)]\n" + s for s in prelude) + "\n"
        )
    if tests:
        parts.append("\n\n".join(tests).rstrip("\n"))
    return "\n".join(p for p in parts if p != "") + "\n"


def engine_reach_text(pristine):
    """Engine source outside `src/tools` (never touched by this move) plus the
    inline test blocks the engine shims keep."""
    chunks = []
    base = ENG / "src"
    for root, dirs, files in base.walk():
        dirs[:] = [d for d in dirs if d not in ("target", ".git")]
        for f in files:
            if not f.endswith(".rs"):
                continue
            p = root / f
            rel = p.relative_to(base)
            if rel.parts and rel.parts[0] == "tools":
                continue
            chunks.append(read(p))
    for text in pristine.values():
        _prod, tests = extract_test_blocks(text.split("\n"))
        chunks.extend(tests)
    return "\n".join(chunks)


def patch_crate_tools_rs(added_names):
    path = DST.parent / "tools.rs"
    lines = read(path).split("\n")
    header = [l for l in lines if l.strip().startswith("//!")]
    entries, seen = [], set()
    for i, line in enumerate(lines):
        s = line.strip()
        attr = None
        if s.startswith("pub mod ") and s.endswith(";"):
            name = s[len("pub mod "):-1]
        elif s == '#[cfg(feature = "test-util")]':
            nxt = lines[i + 1].strip() if i + 1 < len(lines) else ""
            if not (nxt.startswith("pub mod ") and nxt.endswith(";")):
                continue
            name = nxt[len("pub mod "):-1]
            attr = s
        else:
            continue
        if name in seen:
            continue
        seen.add(name)
        entries.append((name, attr))
    missing = []
    for name in added_names:
        if name in seen:
            continue
        seen.add(name)
        entries.append((name, None))
        missing.append(name)
    entries.sort(key=lambda e: e[0])
    out = list(header) + [""]
    for name, attr in entries:
        if attr:
            out.append(attr)
        out.append("pub mod %s;" % name)
    path.write_text("\n".join(out).rstrip("\n") + "\n", encoding="utf-8", newline="\n")
    return missing


def patch_crate_mod_rs(rel_dir, modules, drop=()):
    path = DST / rel_dir / "mod.rs"
    existing = []
    if path.exists():
        for line in read(path).split("\n"):
            s = line.strip()
            if s.startswith("pub mod ") and s.endswith(";"):
                name = s[len("pub mod "):-1]
                if name not in drop:
                    existing.append(name)
    for m in modules:
        if m not in existing and m not in drop:
            existing.append(m)
    existing.sort()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join("pub mod %s;" % m for m in existing) + "\n", encoding="utf-8", newline="\n")


def patch_crate_cargo_toml():
    path = DST.parents[1] / "Cargo.toml"
    text = read(path)
    added = []
    marker = '\nrefact-codegraph = { path = "../refact-codegraph" }\n'
    for dep in CARGO_DEPS:
        crate_name = dep.split(" =")[0]
        if re.search(r"(?m)^%s\s*=" % re.escape(crate_name), text):
            continue
        text = text.replace(marker, marker + dep + "\n")
        added.append(crate_name)
    path.write_text(text, encoding="utf-8", newline="\n")
    return added


def main():
    log = []
    pristine = {rel: git_text(rel) for rel, _crate in MOVED}

    # Files this card must leave in the engine: restore the pristine body and drop
    # any crate copy an older revision of this mover created.
    for rel, _reason in LEFT.items():
        if (SRC / rel).exists():
            (SRC / rel).write_text(git_text(rel), encoding="utf-8", newline="\n")
            log.append("restored engine %s" % rel)
        crate_copy = DST / rel
        if crate_copy.exists():
            crate_copy.unlink()
            log.append("dropped crate %s" % rel)

    for engine_rel, crate_rel in MOVED:
        prod, tests = extract_test_blocks(pristine[engine_rel].split("\n"))
        body = crate_body(engine_rel, prod, "\n".join(tests), log)
        stray = sorted(
            {m for m in re.findall(r"crate::[\w:]+", body) if not m.startswith("crate::tools::")}
        )
        if stray:
            log.append("STRAY %-36s %s" % (crate_rel, stray))
        out = DST / crate_rel
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(body, encoding="utf-8", newline="\n")
        (SRC / engine_rel).write_text(shim(engine_rel, prod, tests), encoding="utf-8", newline="\n")
        log.append("%-40s prod=%5d test=%5d" % (crate_rel, len(prod), sum(len(t.split("\n")) for t in tests)))

    for crate_rel, text in CRATE_FACADES.items():
        p = DST / crate_rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8", newline="\n")
        log.append("crate facade %s" % crate_rel)

    for engine_rel in ENGINE_DELETED:
        p = SRC / engine_rel
        if p.exists():
            p.unlink()
            log.append("removed engine %s" % engine_rel)

    top_level = sorted({module_name(c) for _e, c in MOVED if "/" not in c})
    dirs = sorted({c.split("/")[0] for _e, c in MOVED if "/" in c})
    added = patch_crate_tools_rs(top_level + dirs)
    log.append("crate tools.rs added: %s" % (", ".join(added) or "nothing"))
    patch_crate_mod_rs("buddy", ["refact_engine", "surface", "user_prefs"], drop=("memory",))
    deps = patch_crate_cargo_toml()
    log.append("crate Cargo.toml added: %s" % (", ".join(deps) or "nothing"))

    print("\n".join(log))
    print("\nLEFT in engine: %s" % ", ".join("%s (%s)" % kv for kv in sorted(LEFT.items())))


if __name__ == "__main__":
    sys.exit(main())
