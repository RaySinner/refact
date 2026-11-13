"""N-1 mover: K6a1 + K6a2 (agent spawn core + agent polling) -> refact-tools.

Idempotent: reads every source file from the pre-move commit, so re-running
reproduces the exact same output. Run from the repo root:

    python tools/dev/n1-move.py

The read-only proof lives in tools/dev/n1-check.ps1.

Established pattern (task doc `refact-tools-split`, L-6/L-7):
  * body moves verbatim, `crate::` refs that point outside `crate::tools::` are
    rewritten to the owning crate;
  * every item the engine still reaches has to become `pub`, because the engine
    shim is a cross-crate `pub use refact_tools::tools::<mod>::*;` glob and a
    glob only carries `pub` items;
  * the engine file becomes that glob plus the pre-move import block
    (`#[cfg(test)]`-gated) and the original test module, both verbatim.
"""
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"

BASE = "bca526164"

MODS = [
    "tool_task_spawn_agent",
    "tool_task_agent",
    "tool_task_agent_finish",
    "tool_task_check_agents",
    "tool_task_wait_for_agents",
]

REWRITES = [
    ("use crate::global_context::GlobalContext;",
     "use refact_runtime_api::global_context::GlobalContext;"),
    ("crate::chat::task_agent_monitor::notify_planner_agents_finished",
     "refact_chat_api::agent_notifications::notify_planner_agents_finished"),
]

TOP_ITEM = re.compile(r"^(pub\(crate\) )?(async fn|fn|const|static|struct|enum|type|union) ")
IMPL_HEAD = re.compile(r"^impl\b")
STRUCT_HEAD = re.compile(r"^(?:pub(?:\(crate\))? )?struct \w")
ENUM_HEAD = re.compile(r"^(?:pub(?:\(crate\))? )?enum \w")
FIELD = re.compile(r"^(\s+)(?:pub\(crate\) )?(\w+):")
METHOD = re.compile(r"^(\s+)(?:pub\(crate\) )?((?:async )?fn \w+|const \w+)")

# Methods the kept tests call through a trait; the identifier never shows up in
# the test text, so a textual scan cannot see that the trait has to be imported.
TRAIT_METHOD_HINTS = {
    "tool_execute": "Tool",
    "tool_description": "Tool",
    "tool_depends_on": "Tool",
    "tool_ctx": "GlobalContextCtx",
    "chat_ctx": "GlobalContextCtx",
    "app_state": "GlobalContextCtx",
}


def source_text(mod):
    return subprocess.run(
        ["git", "-C", str(ROOT), "show",
         "%s:refact-agent/engine/src/tools/%s.rs" % (BASE, mod)],
        capture_output=True, check=True).stdout.decode("utf-8")


def split_test(lines):
    for i, line in enumerate(lines):
        if i > 5 and line.strip() == "#[cfg(test)]":
            return lines[:i], lines[i:]
    raise SystemExit("no #[cfg(test)] found")


def widen(lines):
    """Force `pub` on everything the engine can still reach.

    Widening a whole module is deliberate: `pub` in a place nobody reads emits
    no warning, while missing `pub` on anything the engine references is a hard
    compile error, and a private type in a public signature is a `private_interfaces`
    warning. `impl <Trait> for` methods are left alone - a visibility qualifier
    is a syntax error there.
    """
    out = list(lines)
    ctx = None         # "struct" | "enum" | "inherent" | "trait" at depth 1
    depth = 0
    for i, line in enumerate(out):
        if depth == 0:
            if IMPL_HEAD.match(line) and line.rstrip().endswith("{"):
                ctx = "trait" if re.search(r"\bfor\b", line) else "inherent"
            elif STRUCT_HEAD.match(line) and line.rstrip().endswith("{"):
                ctx = "struct"
            elif ENUM_HEAD.match(line) and line.rstrip().endswith("{"):
                ctx = "enum"
            else:
                ctx = None
            if TOP_ITEM.match(line):
                out[i] = "pub " + re.sub(r"^pub\(crate\) ", "", line)
                line = out[i]
        elif depth == 1 and ctx == "struct":
            m = FIELD.match(line)
            if m:
                out[i] = "%spub %s" % (m.group(1), line[m.start(2):])
                line = out[i]
        elif depth == 1 and ctx == "inherent":
            m = METHOD.match(line)
            if m:
                out[i] = "%spub %s" % (m.group(1), line[m.start(2):])
                line = out[i]
        depth += line.count("{") - line.count("}")
        if depth == 0:
            ctx = None
    return out


def crate_body(prod):
    text = "\n".join(widen(prod))
    for old, new in REWRITES:
        text = text.replace(old, new)
    return text.rstrip("\n") + "\n"


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
        return [statement] if body.split(" as ")[-1].strip() in identifiers else []
    brace = re.search(r"\{([^}]*)\}$", body)
    if brace:
        prefix = body[: body.index("{")].rstrip(":").rstrip()
        out = []
        for part in brace.group(1).split(","):
            name = part.strip()
            if not name:
                continue
            if name == "self":
                if prefix.split("::")[-1].strip() in identifiers:
                    out.append(statement)
            elif name in identifiers:
                out.append("use %s::%s;" % (prefix, name))
        return out
    return [statement] if body.split("::")[-1].strip() in identifiers else []


def test_prelude(mod, prod, test):
    test_text = "\n".join(test)
    already = set()
    for statement in nested_use_statements(test_text):
        already |= imported_names(statement)
    needed = {trait for method, trait in TRAIT_METHOD_HINTS.items()
              if re.search(r"\.%s\(" % method, test_text)}
    for statement in use_statements("\n".join(prod)):
        if re.match(r"(?m)^use crate::tools::%s::" % mod, statement):
            continue
        for name in imported_names(statement):
            if name in already:
                continue
            if re.search(r"(?<![\w])%s\b" % re.escape(name), test_text):
                needed.add(name)
    pre = []
    for statement in use_statements("\n".join(prod)):
        if re.match(r"(?m)^use crate::tools::%s::" % mod, statement):
            continue
        for line in precise_imports(statement, needed):
            if line not in pre:
                pre.append(line)
    return pre


def shim(mod, prod, test):
    parts = ["pub use refact_tools::tools::%s::*;\n" % mod]
    pre = test_prelude(mod, prod, test)
    if pre:
        parts.append("\n" + "\n".join("#[cfg(test)]\n" + s for s in pre) + "\n")
    parts.append("\n" + "\n".join(test).rstrip("\n") + "\n")
    return "\n".join(parts)


def main():
    for mod in MODS:
        lines = source_text(mod).replace("\r\n", "\n").split("\n")
        prod, test = split_test(lines)

        body = crate_body(prod)
        stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)
                        if not m.startswith("crate::tools::")})
        if stray:
            raise SystemExit("STRAY crate:: refs in %s: %s" % (mod, stray))
        (DST / (mod + ".rs")).write_text(body, encoding="utf-8", newline="\n")

        (SRC / (mod + ".rs")).write_text(shim(mod, prod, test),
                                         encoding="utf-8", newline="\n")
        print("%s: prod %4d -> crate; test lines %4d" % (mod, len(prod), len(test)))


if __name__ == "__main__":
    main()
