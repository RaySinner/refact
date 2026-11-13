"""O-4 verification harness: static proof the buddy-issue move is behaviour-preserving.

No build is attempted here (the project build guard refuses compilation from agent
worktrees, and the parent retains compilation). Every assertion is a filesystem /
text check, and the verbatim half recomputes the expected crate files and engine
shims from the mover's own rules.

Run: python tools/dev/o4-check.py
"""
import difflib
import importlib.util
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location(
    "o4_move", pathlib.Path(__file__).resolve().parent / "o4-move.py")
mover = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mover)

ENG = mover.ENG
SRC = mover.SRC
DST = mover.DST
CRATE = mover.CRATE

MOVED = [crate_rel for _rel, crate_rel in mover.MOVED]
ENGINE_SHIM = {"tool_buddy_create_issue.rs": "tool_buddy_create_issue::",
               "buddy/integrations.rs": "buddy::integrations::"}

# `tools_list.rs:640` is the only external consumer, and it must keep reading the
# same engine path; the engine path must now resolve through the crate.
TOOLS_LIST_SITES = [
    "crate::tools::tool_buddy_create_issue::ToolBuddyCreateIssue {",
    "crate::tools::buddy::integrations::ToolBuddyOpenIssue {",
]

# public surface that the rest of the engine / other crates may reference
PUBLIC_SURFACE = {
    "tool_buddy_create_issue.rs": [
        "pub struct ToolBuddyCreateIssue {", "pub config_path: String,",
        'name: "buddy_create_issue".to_string()',
        'required": ["title", "body", "confidence"]',
    ],
    "buddy/integrations.rs": [
        "pub struct ToolBuddyOpenIssue {", "pub config_path: String,",
        'name: "buddy_open_issue".to_string()',
        'required": ["title", "body"]',
        "fn prepare_forwarded_args(",
    ],
}

# prompt/schema literals and the tool-name strings an LLM consumes
SCHEMA_MARKERS = [
    '"buddy_create_issue"',
    '"Buddy Create Issue"',
    '"buddy_open_issue"',
    '"Buddy Open Issue"',
    '"confidence"',
    '"diagnostic_index"',
    '"collected_at"',
    '"source_file"',
    '"tool_name"',
    "additionalProperties",
]

# the moved inline test names, in source order
TEST_NAMES = {
    "tool_buddy_create_issue.rs": [],
    "buddy/integrations.rs": [
        "buddy_open_issue_redacts_secrets_in_body",
        "buddy_open_issue_caps_title_at_120_chars",
        "buddy_open_issue_rejects_invalid_provider",
        "buddy_open_issue_caps_labels_count_and_length",
        "buddy_open_issue_passes_valid_args_to_runner",
        "buddy_open_issue_rejects_unknown_arguments",
        "buddy_open_issue_rejects_non_string_label",
        "buddy_open_issue_rejects_non_array_labels",
        "buddy_open_issue_forwards_only_allowlisted_keys",
        "buddy_open_issue_accepts_valid_input",
        "buddy_open_issue_forwards_finding_fields",
        "buddy_open_issue_forwards_diagnostic_reference",
        "buddy_open_issue_redacts_forwarded_error",
        "buddy_open_issue_skips_blank_finding_fields",
    ],
}

checks = []


def check(name, ok, detail=""):
    checks.append((name, bool(ok), detail))
    print("%-6s %s%s" % ("ok" if ok else "FAIL", name, ("  -- " + detail) if detail else ""))


def read(path):
    return pathlib.Path(path).read_text(encoding="utf-8")


def base_text(rel):
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show",
         "%s:refact-agent/engine/src/tools/%s" % (mover.BASE, rel)],
        capture_output=True, check=True,
    ).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def expected_crate_body(rel):
    body = base_text(rel)
    for old, new in mover.REWRITES:
        body = body.replace(old, new)
    return body.rstrip("\n") + "\n"


def shim_entry(crate_rel):
    return "pub use refact_tools::tools::%s::*;" % crate_rel[:-3].replace("/", "::")


# 1. crate files exist and carry real production code
for crate_rel in MOVED:
    p = DST / crate_rel
    body = read(p) if p.exists() else ""
    check("crate file %s exists and is non-trivial" % crate_rel,
          p.exists() and len(body) > 200, "%d bytes" % len(body))

# 2. crate files are pre-move text + ONLY the documented rewrites
for rel, crate_rel in mover.MOVED:
    want = expected_crate_body(rel)
    got = read(DST / crate_rel)
    ok = want.rstrip("\n") == got.rstrip("\n")
    check("crate %s is pre-move verbatim + documented rewrites" % crate_rel, ok)
    if not ok:
        for line in list(difflib.unified_diff(
                want.split("\n"), got.split("\n"), "expected", "got", lineterm=""))[:24]:
            print("      %s" % line)

# 3. no engine-only `crate::` path survived inside the crate
for crate_rel in MOVED:
    body = read(DST / crate_rel)
    stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)
                    if not m.startswith("crate::tools::")})
    check("crate %s has no engine-only crate:: refs" % crate_rel, not stray, str(stray))

# 4. the engine module is now a pure one-line glob shim
for rel, crate_rel in mover.MOVED:
    text = read(SRC / rel)
    check("engine %s is a single-line glob shim" % rel,
          text.strip() == shim_entry(crate_rel), repr(text[:80]))
    check("engine %s shim re-exports a crate module the mover declared" % rel,
          crate_rel in MOVED)

# 5. the crate module tree declares every moved module
tools_rs = read(DST.parent / "tools.rs")
check("crate tools.rs declares `pub mod buddy;`", "pub mod buddy;" in tools_rs)
check("crate tools.rs declares `pub mod tool_buddy_create_issue;`",
      "pub mod tool_buddy_create_issue;" in tools_rs)
check("crate tools/buddy/mod.rs declares `pub mod integrations;`",
      "pub mod integrations;" in read(DST / "buddy" / "mod.rs"))
check("crate tools.rs keeps its module list sorted",
      [l for l in tools_rs.split("\n") if l.startswith("pub mod ")]
      == sorted(l for l in tools_rs.split("\n") if l.startswith("pub mod ")))

# 6. public surface survived the move
for crate_rel, expects in PUBLIC_SURFACE.items():
    body = read(DST / crate_rel)
    for item in expects:
        check("crate %s keeps `%s`" % (crate_rel, item), item in body)

# 7. schema/tool-name literals survived byte-for-byte
for rel, crate_rel in mover.MOVED:
    body = read(DST / crate_rel)
    src = base_text(rel)
    for marker in SCHEMA_MARKERS:
        if marker in src:
            check("crate %s keeps literal %s" % (crate_rel, marker), marker in body)

# 8. the moved inline tests are present, by name, in the crate (file-by-file,
#    driven by what the pre-move file actually had)
for rel, crate_rel in mover.MOVED:
    src = base_text(rel)
    body = read(DST / crate_rel)
    has_tests = "#[cfg(test)]" in src and "mod tests {" in src
    check("crate %s test module matches the pre-move file" % crate_rel,
          ("#[cfg(test)]" in body and "mod tests {" in body) == has_tests)
    if not has_tests:
        continue
    for name in TEST_NAMES.get(crate_rel, []):
        check("crate %s keeps test fn %s" % (crate_rel, name),
              re.search(r"fn %s\(\)" % re.escape(name), body) is not None)
    base_fns = sorted(re.findall(r"fn (\w+)\(\)", src))
    body_fns = sorted(re.findall(r"fn (\w+)\(\)", body))
    check("crate %s keeps every pre-move fn verbatim" % crate_rel, base_fns == body_fns,
          "%d vs %d" % (len(base_fns), len(body_fns)))

# 9. no engine test body was left behind, and nothing outside the pair moved
for rel, _crate_rel in mover.MOVED:
    text = read(SRC / rel)
    check("engine %s keeps no test module" % rel, "mod tests" not in text)
    check("engine %s keeps no production fn" % rel, "fn " not in text)

# 10. `tools_list.rs` still constructs both tools through the same engine paths
tools_list = read(SRC / "tools_list.rs")
for site in TOOLS_LIST_SITES:
    check("tools_list.rs still contains `%s`" % site, site in tools_list)
check("tools_list.rs is untouched by this card",
      tools_list == base_text("tools_list.rs"))

# 11. nothing else in the repo reads the old engine internals
for name in ["prepare_forwarded_args", "truncate_with_suffix", "capped_redacted", "valid_provider"]:
    hits = subprocess.run(
        ["git", "-C", str(ROOT), "grep", "-n", name, "--", "refact-agent/engine/src"],
        capture_output=True,
    ).stdout.decode("utf-8").strip().splitlines()
    check("no engine source still names `%s`" % name, not hits, "; ".join(hits[:3]))

# 12. only the expected files are touched
status = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain"],
                        capture_output=True, check=True).stdout.decode("utf-8")
touched = sorted(l[3:].strip().replace("\\", "/") for l in status.splitlines() if l.strip())
allowed = {
    "refact-agent/engine/src/tools/buddy/integrations.rs",
    "refact-agent/engine/src/tools/tool_buddy_create_issue.rs",
    "refact-agent/engine/crates/refact-tools/src/tools.rs",
    "refact-agent/engine/crates/refact-tools/src/tools/buddy/",
    "refact-agent/engine/crates/refact-tools/src/tools/tool_buddy_create_issue.rs",
    "tools/dev/o4-move.py",
    "tools/dev/o4-check.py",
}
extra = [t for t in touched
         if t not in allowed and not any(t.startswith(a) for a in allowed if a.endswith("/"))]
check("no unrelated file touched", not extra, str(extra))

# 13. the crate has no dependency on the engine (the crate->engine hazard)
cargo = read(CRATE / "Cargo.toml")
check("refact-tools Cargo.toml has no `refact-lsp` dependency",
      "refact-lsp" not in cargo)
for crate_rel in MOVED:
    body = read(DST / crate_rel)
    check("crate %s names no refact_lsp path" % crate_rel, "refact_lsp" not in body)

failed = [c for c in checks if not c[1]]
print("\n%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
