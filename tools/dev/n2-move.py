"""One-off mover for N-2 (K7a agent I/O + K7b planner Q&A/pulse) -> refact-tools.

Idempotent: reads every source file from the pre-move commit, so re-running it
reproduces the exact same output. Run from the repo root:

    python tools/dev/n2-move.py

The read-only proof that the result is faithful lives in tools/dev/n2-check.py.
"""
import pathlib
import re
import subprocess

ENG = pathlib.Path(__file__).resolve().parents[2] / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"

BASE = "bca526164"

MODS = [
    "tool_agent_chat_summary",
    "tool_agent_diff",
    "tool_agent_interact",
    "tool_agent_lifecycle",
    "tool_agent_registry",
    "tool_agent_steer",
    "tool_ask_questions",
    "tool_agent_planner_qna",
    "tool_agent_pulse",
]

# engine path -> crate path (production bodies)
REWRITES = [
    ("crate::llm::params::CacheControl", "refact_core::chat_types::CacheControl"),
    ("crate::app_state::AppState", "refact_runtime_api::AppState"),
    (
        "crate::global_context::GlobalContext",
        "refact_runtime_api::global_context::GlobalContext",
    ),
    ("crate::agents::types::", "refact_agents_core::types::"),
    ("crate::agents::registry::", "refact_agents_core::registry::"),
    ("crate::agents::spawn::", "refact_agents_core::spawn::"),
    ("crate::agents::push::", "refact_agents_core::push::"),
    ("crate::agents::delivery::", "refact_agents_core::delivery::"),
    ("crate::chat::deliver_to_chat", "refact_chat_api::delivery::deliver_to_chat"),
    (
        "crate::chat::task_agent_monitor::notify_planner_blocking_question",
        "refact_chat_api::agent_notifications::notify_planner_blocking_question",
    ),
]

# private declarations the staying engine tests reach -> widened to pub
WIDEN = {
    "tool_agent_chat_summary": ["fn render_summary_response"],
    "tool_agent_diff": [
        "const GIT_DIFF_TIMEOUT",
        "enum AgentDiffMode",
        "struct DiffBase",
        "fn resolve_base",
        "fn run_with_timeout",
        "fn git_output_cap_notice",
        "fn run_git",
        "fn render_agent_diff",
    ],
    "tool_agent_interact": ["fn short_id"],
    "tool_agent_lifecycle": ["struct PushReport", "fn push_if_card_current_and_live"],
    "tool_agent_registry": ["fn render_registry"],
    "tool_agent_steer": ["enum AgentSteerPriority", "fn format_message"],
    "tool_agent_planner_qna": [
        "struct UnansweredQuestion",
        "fn qna_truncation_notice",
        "fn make_question_id",
        "fn collect_unanswered_questions",
        "fn format_unanswered_questions",
    ],
    "tool_agent_pulse": [],
    "tool_ask_questions": [],
}

# structs whose fields the engine-side tests read
PUB_FIELD_STRUCTS = {
    "tool_agent_diff": ["DiffBase"],
    "tool_agent_lifecycle": ["PushReport"],
}

# `#[cfg(test)]`-gated imports that sit in the production region but only the
# engine-side test module needs; they move verbatim into the shim prelude.
MOVED_CFG_TEST_USES = {
    "tool_agent_interact": ["use crate::agents::registry::InboxMessage;"],
    "tool_agent_lifecycle": ["use std::process::Command;"],
}

TRAIT_METHOD_HINTS = {
    "tool_execute": "Tool",
    "tool_description": "Tool",
    "tool_depends_on": "Tool",
    "tool_ctx": "GlobalContextCtx",
    "chat_ctx": "GlobalContextCtx",
    "app_state": "GlobalContextCtx",
}


def source_text(mod):
    """Read the pristine pre-move file so the script is idempotent."""
    return subprocess.run(
        ["git", "-C", str(ENG), "show", "%s:refact-agent/engine/src/tools/%s.rs" % (BASE, mod)],
        capture_output=True, check=True,
    ).stdout.decode("utf-8")


def split_test(lines):
    for i in range(len(lines) - 1):
        if lines[i].strip() == "#[cfg(test)]" and lines[i + 1].strip().startswith("mod tests"):
            return lines[:i], lines[i:]
    return lines, []


CTX_CALL = re.compile(r"\.(tool_ctx|chat_ctx|app_state)\(([^)]*)\)")


def needs_global_context_ctx(text):
    """True when a body calls a composite-context builder with `GlobalContext`.

    `GlobalContextCtx` is only implemented for `GlobalContext`, and its methods
    take the context as an argument. `AppState` has an inherent zero-argument
    `chat_ctx()`, so a call that passes nothing does not need the trait.
    """
    return any(args.strip() for _, args in CTX_CALL.findall(text))


def extract_cfg_test_uses(prod, mod, log):
    """Pull `#[cfg(test)]` + `use ...;` pairs out of the production region."""
    out = []
    skip = set()
    want = MOVED_CFG_TEST_USES.get(mod, [])
    for i in range(len(prod) - 1):
        if prod[i].strip() == "#[cfg(test)]" and prod[i + 1].strip() in want:
            out.append(prod[i + 1].strip())
            skip.add(i)
            skip.add(i + 1)
            log.append("moved cfg(test) use out of prod: %s %s" % (mod, prod[i + 1].strip()))
    kept = [line for i, line in enumerate(prod) if i not in skip]
    missing = [u for u in want if u not in out]
    if missing:
        raise SystemExit("cfg(test) use not found in %s: %s" % (mod, missing))
    return kept, out


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
    return {trait for method, trait in TRAIT_METHOD_HINTS.items()
            if re.search(r"\.%s\(" % method, test_text)}


def widen(lines, mod, log):
    end = len(lines)
    for i in range(end):
        s = lines[i].strip()
        for p in WIDEN.get(mod, []):
            kind, name = p.split(" ", 1)
            if re.match(r"^(?:async )?%s %s\b" % (kind, re.escape(name)), s):
                indent = lines[i][: len(lines[i]) - len(lines[i].lstrip())]
                lines[i] = indent + "pub " + lines[i][len(indent):]
                log.append("widen %s L%d %s" % (mod, i + 1, s))
                break
    for name in PUB_FIELD_STRUCTS.get(mod, []):
        start = None
        for i in range(end):
            if re.match(r"^(pub )?struct %s\b" % name, lines[i].strip()):
                start = i
                break
        if start is None:
            raise SystemExit("struct %s not found in %s" % (name, mod))
        for j in range(start + 1, end):
            s = lines[j].strip()
            if s.startswith("}"):
                break
            if s and not s.startswith("pub ") and not s.startswith("//"):
                lines[j] = re.sub(r"^(\s*)", r"\1pub ", lines[j])
                log.append("pub-field %s L%d %s" % (mod, j + 1, s))


def crate_body(mod, prod, log):
    lines, _ = extract_cfg_test_uses(prod, mod, log)
    lines = list(lines)
    widen(lines, mod, log)
    text = "\n".join(lines)
    for old, new in REWRITES:
        text = text.replace(old, new)
    # `GlobalContextCtx` is what makes `gcx.chat_ctx(..)` resolve. On an `AppState`
    # receiver the same call is an inherent method, so a body whose only calls go
    # through `app` has no use for the trait: the import is dropped and the
    # engine-side test module gets it back through the shim prelude.
    if not needs_global_context_ctx(text):
        text = text.replace("use refact_runtime_api::GlobalContextCtx;\n", "")
    return text


def test_prelude(mod, prod, test):
    test_text = "\n".join(test)
    already = set()
    for statement in nested_use_statements(test_text):
        already |= imported_names(statement)
    needed = trait_names(test_text)
    for statement in use_statements("\n".join(prod)):
        if statement.startswith("pub(") or re.match(r"(?m)^use crate::tools::%s::" % mod, statement):
            continue
        for name in imported_names(statement):
            if name in already:
                continue
            if names_an_identifier(test_text, name):
                needed.add(name)
    pre = []
    bound = set()
    for statement in MOVED_CFG_TEST_USES.get(mod, []) + use_statements("\n".join(prod)):
        if statement.startswith("pub(") or re.match(r"(?m)^use crate::tools::%s::" % mod, statement):
            continue
        for line in precise_imports(statement, needed):
            names = imported_names(line)
            if names and names <= bound:
                continue
            if line not in pre:
                pre.append(line)
                bound |= names
    return pre


def main():
    log = []
    for mod in MODS:
        lines = source_text(mod).replace("\r\n", "\n").split("\n")
        prod, test = split_test(lines)

        text = crate_body(mod, prod, log)
        stray = sorted({m for m in re.findall(r"crate::[\w:]*", text)
                        if not m.startswith("crate::tools::")})
        if stray:
            print("STRAY in %s: %s" % (mod, stray))
        (DST / (mod + ".rs")).write_text(text.rstrip("\n") + "\n", encoding="utf-8")

        pre = test_prelude(mod, prod, test)
        shim = "pub use refact_tools::tools::%s::*;\n" % mod
        if pre:
            shim += "\n" + "\n".join("#[cfg(test)]\n" + line for line in pre) + "\n"
        if test:
            shim += "\n" + "\n".join(test).rstrip("\n") + "\n"
        (SRC / (mod + ".rs")).write_text(shim, encoding="utf-8")
        log.append("%s: prod %d -> crate; shim prelude %d; test lines %d"
                   % (mod, len(prod), len(pre), len(test)))

    print("\n".join(log))


if __name__ == "__main__":
    main()
