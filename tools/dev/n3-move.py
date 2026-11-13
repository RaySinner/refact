"""One-off mover for N-3 (K9 review subsystem) -> refact-tools.

Idempotent: reads every source file from the pre-move commit, so re-running it
reproduces the exact same output. Run from the repo root:

    python tools/dev/n3-move.py

The read-only proof that the result is faithful lives in tools/dev/n3-check.py.

Moved (5):  review_scope, review_evidence, review_agents/{config,prompts,stages}
Left  (3):  review_agents/mod.rs and review_agents/runner.rs (both consume
            crate::tools::subagent_phases::get_last_assistant_content) and
            tool_review.rs (consumes crate::tools::subagent_phases::
            resolve_gathered_file_path, which is pub(crate) in the engine).
Left  (3 shim): review_types.rs, review_merge.rs, review_agents/contract.rs --
            already re-exports of refact-tool-api, nothing to move.
"""
import pathlib
import re
import subprocess

ENG = pathlib.Path(__file__).resolve().parents[2] / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"
CRATE = ENG / "crates" / "refact-tools"

BASE = "bca526164a8d0b3715938aab9c3cc54b6ea11e63"

# (engine-relative path under src/tools, crate-relative path under src/tools)
MODS = [
    ("review_scope.rs", "review_scope.rs"),
    ("review_evidence.rs", "review_evidence.rs"),
    ("review_agents/config.rs", "review_agents/config.rs"),
    ("review_agents/prompts.rs", "review_agents/prompts.rs"),
    ("review_agents/stages.rs", "review_agents/stages.rs"),
]

# Every rewrite from an engine path to the crate-resident path it re-exports.
REWRITES = [
    ("crate::tools::review_types::", "refact_tool_api::review_types::"),
    ("crate::global_context::tests::make_test_gcx", "ENGINE_TEST_GCX"),
    ("crate::global_context::", "refact_runtime_api::global_context::"),
    ("crate::llm::params::CacheControl", "refact_core::chat_types::CacheControl"),
]

# The single grouped import that names the caps loader is split so the loader
# comes from its owning module and `GlobalContext` from global_context. Matched
# in its pre-rewrite form, so the split runs before REWRITES.
SPLIT_IMPORTS = [
    (
        "use crate::global_context::{try_load_caps_quickly_if_not_present, GlobalContext};",
        "use refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present;\n"
        "use refact_runtime_api::global_context::GlobalContext;",
    ),
]

# private -> pub, only where the engine's kept test module reaches the item
WIDEN = {
    "review_agents/prompts.rs": [
        "const FINDINGS_CONTRACT",
        "const VERDICTS_CONTRACT",
        "fn diff_char_cap",
        "fn truncate_diff",
        "fn scope_block",
    ],
    "review_agents/stages.rs": [
        "const STAGE_ORDER",
        "fn parse_stage",
    ],
}

# the crate's own review_agents facade declares only the three moved children
CRATE_AGENT_MOD = "pub mod config;\npub mod prompts;\npub mod stages;\n"

CRATE_MODULES = ["review_agents", "review_evidence", "review_scope"]


def source_text(rel):
    """Read the pristine pre-move file so the script is idempotent."""
    raw = subprocess.run(
        ["git", "-C", str(ENG), "show",
         "%s:refact-agent/engine/src/tools/%s" % (BASE, rel)],
        capture_output=True, check=True).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def split_test(lines):
    for i, line in enumerate(lines):
        if i > 5 and line.strip() == "#[cfg(test)]":
            return lines[:i], lines[i:]
    raise SystemExit("no #[cfg(test)] found")


def own_use_lines(prod):
    """Column-0 `use ...;` lines: the module's own, non-`pub` import block."""
    return [line for line in prod if re.match(r"^use [^;]*;$", line)]


def crate_body(rel, prod):
    lines = list(prod)
    for i, line in enumerate(lines):
        for prefix in WIDEN.get(rel, []):
            stripped = line.strip()
            if stripped.startswith(prefix) or stripped.startswith("async " + prefix):
                lines[i] = "pub " + line
                break
    text = "\n".join(lines)
    for old, new in SPLIT_IMPORTS:
        text = text.replace(old, new)
    for old, new in REWRITES:
        text = text.replace(old, new)
    assert "ENGINE_TEST_GCX" not in text, "test-only path leaked into " + rel
    return text


def shim_text(rel, prod, test):
    path = rel[:-3].replace("/", "::")
    text = "pub use refact_tools::tools::%s::*;\n\n" % path
    for line in own_use_lines(prod):
        text += "#[cfg(test)]\n#[allow(unused_imports)]\n" + line + "\n"
    text += "\n".join(test).rstrip("\n")
    return text


def patch_tools_rs(path):
    """Read-modify-write the crate module table: sorted, unique, idempotent."""
    lines = path.read_text(encoding="utf-8").split("\n")
    header = [line for line in lines if "//!" in line]
    entries, seen = [], set()
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("pub mod ") and stripped.endswith(";"):
            name = stripped[len("pub mod "):-1]
            attr = None
        elif stripped == '#[cfg(feature = "test-util")]':
            nxt = lines[i + 1].strip() if i + 1 < len(lines) else ""
            if not (nxt.startswith("pub mod ") and nxt.endswith(";")):
                continue
            name = nxt[len("pub mod "):-1]
            attr = stripped
        else:
            continue
        if name in seen:
            continue
        seen.add(name)
        entries.append((name, attr))
    missing = [m for m in CRATE_MODULES if m not in seen]
    for m in missing:
        entries.append((m, None))
    entries.sort(key=lambda entry: entry[0])

    out = list(header) + [""]
    for name, attr in entries:
        if attr:
            out.append(attr)
        out.append("pub mod %s;" % name)
    path.write_text("\n".join(out).rstrip("\n") + "\n", encoding="utf-8")
    return missing


def main():
    for rel, crate_rel in MODS:
        lines = source_text(rel).split("\n")
        prod, test = split_test(lines)

        out = DST / crate_rel
        out.parent.mkdir(parents=True, exist_ok=True)
        body = crate_body(rel, prod)
        stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)
                        if not m.startswith("crate::tools::")})
        if stray:
            print("STRAY %s: %s" % (rel, stray))
        out.write_text(body.rstrip("\n") + "\n", encoding="utf-8")

        (SRC / rel).write_text(shim_text(rel, prod, test) + "\n", encoding="utf-8")
        print("%-26s crate prod %d lines | engine shim test %d lines"
              % (rel, len(prod), len(test)))

    agent_mod = DST / "review_agents" / "mod.rs"
    agent_mod.parent.mkdir(parents=True, exist_ok=True)
    agent_mod.write_text(CRATE_AGENT_MOD, encoding="utf-8")

    added = patch_tools_rs(CRATE / "src" / "tools.rs")
    print("crate tools.rs: added %s" % (added or "nothing (already present)"))


if __name__ == "__main__":
    main()
