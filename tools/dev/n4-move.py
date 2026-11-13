"""N-4 mover: K5b + K3b + part of K11 -> refact-tools.

Moves 10 of the 12 card files. `tool_cron_create.rs` and `tool_cron_update.rs`
stay in the engine: cron_create calls
`crate::chat::get_or_create_session_with_trajectory` (engine
`src/chat/session_runtime.rs:123`, not crate-resident) and cron_update imports
cron_create's `human_schedule_for_trigger`.

Idempotent: reads every source file from the pristine pre-move commit, so
re-running reproduces the exact same output. Run from the repo root:
    python tools/dev/n4-move.py

Read-only verification: tools/dev/n4-check.py
"""
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"
BASE = "bca526164"

MOVED = [
    "tool_task_documents",
    "tool_task_memory",
    "subagent_phases",
    "tool_add_workspace_folder",
    "tool_create_knowledge",
    "tool_knowledge",
    "tool_activate_skill",
    "tool_config_subagent",
    "tool_cron_list",
    "tool_cron_delete",
]

LEFT = {
    "tool_cron_create": "crate::chat::get_or_create_session_with_trajectory (engine src/chat/session_runtime.rs:123)",
    "tool_cron_update": "use crate::tools::tool_cron_create::human_schedule_for_trigger",
}

# engine path -> owning-crate path. Everything else under `crate::tools::*`
# already resolves inside refact-tools (tools_description, tool_helpers,
# native_enrichment, task_tool_helpers).
REWRITES = [
    ("crate::app_state::ChatServices", "refact_runtime_api::ChatServices"),
    ("crate::app_state::AppState", "refact_runtime_api::AppState"),
    ("crate::global_context::", "refact_runtime_api::global_context::"),
    ("crate::ext::config_dirs::", "refact_ext::config_dirs::"),
    ("crate::ext::skills_context::", "refact_ext::skills_context::"),
    ("crate::ext::skills::", "refact_ext::skills::"),
]

# In the crate the engine's *test* build must still see these items, so
# `#[cfg(test)]` becomes `cfg(any(test, feature = "test-util"))` (the engine
# enables the feature in [dev-dependencies]).
TEST_CFG = '#[cfg(any(test, feature = "test-util"))]'

USE_RE = re.compile(r"(?m)^use [^;]*;")
STRUCT_RE = re.compile(r"^\s*(pub(?:\([a-z_]*\))?\s+)?(struct|enum)\s+(\w+)")
IMPL_RE = re.compile(r"^\s*impl\b|^\s*impl<")
FN_HDR_RE = re.compile(r"^\s*(?:pub(?:\([a-z_]*\))?\s+)?(?:async\s+)?fn\b")
TYPE_HDR_RE = re.compile(r"^\s*(?:pub(?:\([a-z_]*\))?\s+)?(?:struct|enum|union)\b")


def source_text(mod):
    out = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:refact-agent/engine/src/tools/%s.rs" % (BASE, mod)],
        capture_output=True, check=True,
    ).stdout.decode("utf-8")
    return out.replace("\r\n", "\n")


def split_tests(lines):
    for i in range(len(lines) - 1):
        if lines[i].strip() == "#[cfg(test)]" and re.match(r"^(pub )?mod tests\b", lines[i + 1].strip()):
            return lines[:i], lines[i:]
    return lines, []


def names_in(text, name):
    return re.search(r"(?<![\w])%s(?![\w])" % re.escape(name), text) is not None


def strip_strings(s):
    """Drop string/char literals so brace counting is not fooled by `{` in text."""
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c == '"':
            i += 1
            while i < len(s) and s[i] != '"':
                i += 2 if s[i] == "\\" else 1
            i += 1
        elif c == "'" and i + 2 < len(s) and s[i + 2] == "'":
            i += 3
        else:
            out.append(c)
            i += 1
    return "".join(out)


def scopes(lines):
    """Per-line nesting context: which frames are open at that line's start."""
    depth, frames, out = 0, [], []
    for raw in lines:
        out.append(tuple(f[0] for f in frames))
        s = strip_strings(raw)
        t = s.strip()
        if IMPL_RE.match(t):
            frames.append(("impl-trait" if " for " in t else "impl-inherent", depth + 1))
        elif FN_HDR_RE.match(t) and t.endswith("{"):
            frames.append(("fn", depth + 1))
        elif TYPE_HDR_RE.match(t):
            frames.append(("type", depth + 1))
        depth += s.count("{") - s.count("}")
        while frames and depth < frames[-1][1]:
            frames.pop()
    return out


def struct_body(lines, i):
    """(kind, name, body_first, body_last_exclusive) for a struct/enum declaration."""
    m = STRUCT_RE.match(lines[i])
    if not m:
        return None
    kind, name = m.group(2), m.group(3)
    j = i
    while j < len(lines) and "{" not in strip_strings(lines[j]):
        if lines[j].rstrip().endswith(";"):
            return (kind, name, None, None)  # tuple / unit struct
        j += 1
    if j >= len(lines):
        return (kind, name, None, None)
    depth = 0
    for k in range(j, len(lines)):
        depth += strip_strings(lines[k]).count("{") - strip_strings(lines[k]).count("}")
        if depth == 0:
            return (kind, name, j + 1, k)
    return (kind, name, None, None)


def frame_kind(ctx):
    """Where the line sits: 'trait-impl', 'inherent-impl', 'nested' or 'module'."""
    if "impl-trait" in ctx:
        return "trait-impl"
    if "impl-inherent" in ctx:
        return "inherent-impl"
    if "fn" in ctx:
        return "nested"
    return "module"


def is_module_scope(ctx):
    """True for lines at module level or directly inside a top-level type body."""
    return not any(f in ("fn", "impl-inherent", "impl-trait") for f in ctx)


def widen(lines, test_text, log, mod):
    """Widen the prod surface the engine side still touches.

    The engine shim re-exports the module with `pub use …::*`, and the engine's
    `#[cfg(test)] mod tests` lives in the *engine* crate, so everything those
    tests (or another engine module) can reach must be `pub` — not `pub(crate)`:

      * every module-level type is `pub` (a private type in a `pub fn` signature
        is `private_interfaces`, and the tests also name the types);
      * every field of a module-level struct is `pub` (tests read them);
      * a module-level or inherent-impl `fn`/`const`/`static` becomes `pub` when
        the tests name it or when it was already `pub(crate)` and is therefore
        called from the engine.

    Widening more than strictly needed is invisible: the module itself is not
    re-exported from `refact_tools::lib`, so `pub` here means `pub` inside
    `refact_tools::tools::<mod>` only.

    Trait-impl items are never touched (adding `pub` there is E0449).
    """
    ctxs = scopes(lines)

    for i, line in enumerate(lines):
        where = frame_kind(ctxs[i])
        if where in ("trait-impl", "nested"):
            continue
        if "type" in ctxs[i]:
            continue

        tm = re.match(
            r"^(\s*)(pub(?:\([a-z_]*\))?\s+)?(struct|enum|union|type)\s+(\w+)", line)
        if tm:
            if where != "module" or "type" in ctxs[i]:
                continue
            consumed = len(tm.group(1)) + len(tm.group(2) or "") + len(tm.group(3))
            if (tm.group(2) or "").strip() != "pub":
                log.append("pub type %s (was %s)" % (tm.group(4), (tm.group(2) or "priv").strip()))
            lines[i] = tm.group(1) + "pub " + tm.group(3) + line[consumed:]
            continue

        fm = re.match(r"^(\s*)(pub(?:\([a-z_]*\))?\s+)?(?:async\s+)?(fn|const|static)\s+(\w+)", line)
        if fm:
            vis, kind, name = (fm.group(2) or "").strip(), fm.group(3), fm.group(4)
            if vis == "pub":
                continue
            if vis == "pub(crate)" or names_in(test_text, name):
                keep = line[len(fm.group(1)) + len(fm.group(2) or ""):]
                lines[i] = fm.group(1) + "pub " + keep
                log.append("pub %s %s (was %s)" % (kind, name, vis or "priv"))

    # struct fields (module-level structs only; tuple structs are skipped)
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
            s = lines[j]
            fm = re.match(r"^(\s*)(pub(?:\([a-z_]*\))?\s+)?([A-Za-z_]\w*)\s*:", s)
            if not fm or (fm.group(2) or "").strip() == "pub":
                continue
            lines[j] = fm.group(1) + "pub " + s[len(fm.group(1)) + len(fm.group(2) or ""):]
            log.append("pub-field %s %s.%s (was %s)"
                       % (mod, name, fm.group(3), (fm.group(2) or "priv").strip()))
    return lines


def crate_body(mod, prod, test_text, log):
    # drop `#[cfg(test)] use crate::global_context::GlobalContext;` (marker and
    # import) — the test module that needs it stays behind in the engine shim.
    out, pending = [], False
    for line in prod:
        if pending:
            pending = False
            if line.strip() == "use crate::global_context::GlobalContext;":
                out.pop()  # drop the dangling `#[cfg(test)]` marker too
                log.append("dropped engine GlobalContext import (%s)" % mod)
                continue
        if line.strip() == "#[cfg(test)]":
            pending = True
        out.append(line)
    lines = widen(out, test_text, log, mod)
    text = "\n".join(lines)
    for old, new in REWRITES:
        text = text.replace(old, new)
    text = text.replace("#[cfg(test)]", TEST_CFG)
    return text.rstrip("\n") + "\n"


def shim(mod, prod, test):
    """Glob re-export + cfg(test) prelude (pre-move imports verbatim) + the tests."""
    prelude = []
    for statement in USE_RE.findall("\n".join(prod)):
        if statement.startswith("pub("):
            continue
        if re.match(r"^use crate::tools::%s::" % re.escape(mod), statement):
            continue
        if statement not in prelude:
            prelude.append(statement)
    parts = ["pub use refact_tools::tools::%s::*;\n" % mod]
    if prelude:
        parts.append("\n".join("#[cfg(test)]\n#[allow(unused_imports)]\n" + s for s in prelude) + "\n")
    parts.append("\n".join(test).rstrip("\n"))
    return "\n".join(parts) + "\n"


def main():
    log = []
    for mod in MOVED:
        lines = source_text(mod).split("\n")
        prod, test = split_tests(lines)
        test_text = "\n".join(test)
        body = crate_body(mod, prod, test_text, log)
        stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)})
        (DST / (mod + ".rs")).write_text(body, encoding="utf-8", newline="\n")
        (SRC / (mod + ".rs")).write_text(shim(mod, prod, test), encoding="utf-8", newline="\n")
        log.append("%-26s prod=%4d test=%4d  stray crate:: %s"
                   % (mod, len(prod), len(test), stray or "-"))
    tools_rs = DST.parent / "tools.rs"
    mods = sorted({f.stem for f in DST.glob("*.rs")} | {d.name for d in DST.iterdir() if d.is_dir()})
    out = ["//! Tool implementations moved out of the engine (see task doc `refact-tools-split`).",
           ""]
    for name in mods:
        if name == "settings_guard":
            out.append('#[cfg(feature = "test-util")]')
        out.append("pub mod %s;" % name)
    tools_rs.write_text("\n".join(out) + "\n", encoding="utf-8", newline="\n")
    print("\n".join(log))
    print("\ncrate modules: %d" % len(mods))
    print("LEFT in engine: %s" % ", ".join("%s (%s)" % (k, v) for k, v in sorted(LEFT.items())))


if __name__ == "__main__":
    main()
