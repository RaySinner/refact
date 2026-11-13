"""O-4 mover: `tools/buddy/integrations.rs` + `tools/tool_buddy_create_issue.rs` -> refact-tools.

Both files are `Tool` impls and form one mutual pair: `ToolBuddyOpenIssue`
(`tools/buddy/integrations.rs:21`) builds `ToolBuddyCreateIssue` for its
`runner()`, and `tools_list.rs:640` needs `ToolBuddyOpenIssue` at construction
time. They therefore move together (spec rule 1).

They move *whole* -- production code and their inline `#[cfg(test)] mod tests`
-- because the tests read only module-local items (`prepare_forwarded_args`,
`TITLE_MAX_CHARS`, `TRUNCATED_SUFFIX`, `serde_json`) and nothing outside the two
files references them. The engine side becomes a pure one-line glob shim.

Only one rewrite is needed: `crate::buddy::actor::redact_sensitive` is an engine
module that re-exports `refact_buddy_core::actor`, so it becomes the crate path.
`crate::tools::tools_description::*` and `crate::tools::tool_buddy_create_issue::*`
keep their text: the crate has the same `tools/` tree, so those paths resolve to
`refact_tools::tools::...` inside the crate exactly as they did in the engine.

Idempotent: every source file is read from the pristine pre-move commit, so
re-running reproduces the same output. Run from the repo root:

    python tools/dev/o4-move.py

Read-only proof: tools/dev/o4-check.py
"""
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"
CRATE = ENG / "crates" / "refact-tools"

BASE = "a9f66c9366124b6c6655beba6347be4f04c9d5f2"

# (engine-relative path under src/tools, crate-relative path under src/tools)
MOVED = [
    ("tool_buddy_create_issue.rs", "tool_buddy_create_issue.rs"),
    ("buddy/integrations.rs", "buddy/integrations.rs"),
]

REWRITES = [
    ("crate::buddy::actor::redact_sensitive", "refact_buddy_core::actor::redact_sensitive"),
]

CRATE_TOOLS_HEADER = "﻿//! Tool implementations moved out of the engine (see task doc `refact-tools-split`)."
CRATE_BUDDY_MOD = "pub mod integrations;\n"


def source_text(rel):
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show",
         "%s:refact-agent/engine/src/tools/%s" % (BASE, rel)],
        capture_output=True, check=True,
    ).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def crate_module_path(crate_rel):
    return crate_rel[:-3].replace("/", "::")


def shim_path(crate_rel):
    return ("pub use refact_tools::tools::%s::*;\n" % crate_module_path(crate_rel))


def insert_line(target, entry):
    """Add a `pub mod ...;` / directory line to a module file, sorted, if missing."""
    text = target.read_text(encoding="utf-8") if target.exists() else ""
    lines = text.split("\n")
    if entry in lines:
        return False
    header = [l for l in lines if l.lstrip("\ufeff").startswith("//!")]
    rest = [l for l in lines if l.strip() and not l.lstrip("\ufeff").startswith("//!")]
    at = 0
    while at < len(rest) and rest[at] < entry:
        at += 1
    out = header + [""] + rest[:at] + [entry] + rest[at:]
    target.write_text("\n".join(out).rstrip("\n") + "\n", encoding="utf-8", newline="\n")
    return True


def declare_module(rel):
    """Declare every ancestor of `rel` in the crate module tree."""
    names = crate_module_path(rel).split("::")
    changed = []
    for depth in range(1, len(names) + 1):
        parent = DST.joinpath(*names[: depth - 1])
        parent.mkdir(parents=True, exist_ok=True)
        target = (parent / "mod.rs").resolve() if depth > 1 else (DST / "../tools.rs").resolve()
        if insert_line(target, "pub mod %s;" % names[depth - 1]):
            changed.append("::".join(names[:depth]))
    return changed


def main():
    for rel, crate_rel in MOVED:
        body = source_text(rel)
        for old, new in REWRITES:
            body = body.replace(old, new)
        stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)
                        if not m.startswith("crate::tools::")})
        if stray:
            raise SystemExit("STRAY engine-only crate:: path in %s: %s" % (rel, stray))

        out = DST / crate_rel
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(body.rstrip("\n") + "\n", encoding="utf-8", newline="\n")

        (SRC / rel).write_text(shim_path(crate_rel), encoding="utf-8", newline="\n")
        print("%-32s crate %4d lines | engine shim 1 line"
              % (crate_rel, len(body.split("\n"))))

    (DST / "buddy" / "mod.rs").write_text(CRATE_BUDDY_MOD, encoding="utf-8", newline="\n")
    for _rel, crate_rel in MOVED:
        for declared in declare_module(crate_rel):
            print("declared pub mod %s" % declared)

    if not CRATE_TOOLS_HEADER.strip() in (DST.parent / "tools.rs").read_text(encoding="utf-8"):
        raise SystemExit("crate tools.rs header changed unexpectedly")


if __name__ == "__main__":
    main()
