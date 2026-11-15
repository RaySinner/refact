"""AB-4 in-repo verification gate.

    python tools/dev/ab4-check.py

Read-only, deterministic, and it does not compile Rust (the compile gate is
`tools/dev/engine-build.ps1 -Mode check`). Two halves:

  A. the `tools` destination-crate cycle is broken
     C1  the census reports 0 files / 0 lines in the
         `BLOCKED on a destination-crate cycle` class for `tools`
     C2  no crate's dependency closure reaches back into itself; the only crates
         whose closure still contains `refact-tools` are the binary (`refact-lsp`)
         and `refact-http`, which consumes the destination
     C3  the browser layer that closed the cycle lives in `refact-runtime-api`,
         with 1-line `pub use` shims left behind in `refact-http`

  B. the two `tools` files really moved, with no behaviour change
     C4  both crate files exist and their production bodies carry no
         engine-resident `crate::` path
     C5  the engine side of each moved module is a glob shim, and the leaf shims
         for the five design tools point at the crate
     C6  zero-behaviour-change proof: applying the documented forward rewrite to
         the pre-move engine file reproduces the crate file, modulo the two
         `make_test_gcx` tests that stay in the engine and the blank lines at the
         seams where they were cut
     C7  every `#[cfg(test)]` line of the pre-move engine file still exists, EITHER
         in the crate file or in the engine shim
     C8  no crate under `crates/` depends on `refact-lsp`
"""
import os
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
CRATE_TOOLS = ENG / "crates" / "refact-tools" / "src" / "tools"
CRATE_HTTP = ENG / "crates" / "refact-http"
CRATE_RAPI = ENG / "crates" / "refact-runtime-api"
CENSUS = ROOT / "tools" / "dev" / "chat_movable_census.py"
BASE = "3af711082"

LEAF_SHIMS = {
    "tool_contrast_audit": "ToolContrastAudit",
    "tool_image_region": "ToolImageRegion",
    "tool_mark_elements": "ToolMarkElements",
    "tool_ui_probe": "ToolUiProbe",
    "tool_visual_diff": "ToolVisualDiff",
}

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\(test\)[^\n]*\]")
ENGINE_PATH = re.compile(r"\bcrate::[A-Za-z_][A-Za-z0-9_]*")

# The only `crate::` path a moved tool may keep is its own sibling module inside
# the destination crate.
ALLOWED_ENGINE_PATH = "crate::tools"

# Documented rewrites applied when a `tools` file moves into `refact-tools`.
REWRITES = [
    ("crate::integrations::browser_controller", "refact_runtime_api::integrations::browser_controller"),
    ("crate::integrations::browser_runtime", "refact_runtime_api::integrations::browser_runtime"),
    ("crate::integrations::browser_models", "refact_integrations::browser_models"),
    ("crate::integrations::browser_actions", "refact_integrations::browser_actions"),
    ("crate::app_state", "refact_runtime_api::app_state"),
    ("crate::global_context", "refact_runtime_api::global_context"),
    ("crate::scratchpads::multimodality", "refact_scratchpads::multimodality"),
    (
        "crate::at_commands::at_commands::at_commands_new_from_app",
        "refact_runtime_api::at_commands_new_from_app",
    ),
    ("use crate::http::routers::v1::browser_settings;", "use refact_browser::settings as browser_settings;"),
    ("\nfn design_artifacts_dir(", "\npub fn design_artifacts_dir("),
    ("\nasync fn resolve_image_source(", "\npub async fn resolve_image_source("),
    ('const ARTIFACT_PATH_PREFIX: &str = "artifact://";', 'pub const ARTIFACT_PATH_PREFIX: &str = "artifact://";'),
]

# The two tests that keep `make_test_gcx` and therefore stay in the engine shim.
KEPT_TESTS = {
    "design_tools": [(2360, 2392), (2631, 2674)],
    "tool_chrome": [],
}

fails = []
checks = 0


def check(cond, label):
    global checks
    checks += 1
    if not cond:
        fails.append(label)


def read(path):
    return pathlib.Path(path).read_text(encoding="utf-8")


def git_show(rel):
    return subprocess.run(
        ["git", "show", "%s:%s" % (BASE, rel)],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout


def strip_cfg_test(text):
    """Drop every `#[cfg(test)] mod …` item, brace-balanced, ignoring literals."""
    while True:
        m = CFG_TEST.search(text)
        if m is None:
            return text
        rest = text[m.end():].lstrip()
        if not (rest.startswith("mod ") or rest.startswith("pub mod ")):
            return text[: m.start()]
        open_brace = text.find("{", m.end())
        semi = text.find(";", m.end())
        if open_brace == -1 or (semi != -1 and semi < open_brace):
            text = text[: m.start()] + text[semi + 1:]
            continue
        depth, i, in_str, in_char = 0, open_brace, False, False
        while i < len(text):
            ch = text[i]
            if in_str or in_char:
                if ch == "\\":
                    i += 2
                    continue
                if (in_str and ch == '"') or (in_char and ch == "'"):
                    in_str = in_char = False
            elif ch == '"':
                in_str = True
            elif ch == "'":
                in_char = True
            elif ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if i >= len(text):
            return text[: m.start()]
        text = text[: m.start()] + text[i + 1:]


def collapse_blank_runs(text):
    out = []
    for line in text.splitlines():
        if not line.strip() and out and not out[-1].strip():
            continue
        out.append(line)
    return "\n".join(out).strip("\n") + "\n"


def forward(text):
    for old, new in REWRITES:
        text = text.replace(old, new)
    return text


def cut_kept_tests(text, spans):
    lines = text.splitlines(keepends=True)
    keep = [True] * len(lines)
    for a, b in spans:
        for i in range(a - 1, b):
            keep[i] = False
    return "".join(l for l, k in zip(lines, keep) if k)


# ------------------------------------------------------------------ A1 census
out = subprocess.run(
    [sys.executable, str(CENSUS), "tools"],
    cwd=str(ROOT),
    capture_output=True,
    text=True,
    encoding="utf-8",
)
check(out.returncode == 0, "census tools exits 0 (%s)" % out.stderr[-200:])
report = ENG / "target" / "tools_movable_census.txt"
check(report.is_file(), "census wrote %s" % report)
body = read(report) if report.is_file() else ""
m = re.search(
    r"(?m)^BLOCKED on a destination-crate cycle \(dest=refact-tools\): (\d+) files / (\d+) lines$",
    body,
)
check(m is not None, "census prints a `BLOCKED on a destination-crate cycle` header")
if m:
    check(
        (m.group(1), m.group(2)) == ("0", "0"),
        "cycle class is empty, got %s files / %s lines" % (m.group(1), m.group(2)),
    )

# ------------------------------------------------------------ A2 crate graph
deps = {}
for toml in (ENG / "crates").glob("*/Cargo.toml"):
    deps[toml.parent.name] = set(re.findall(r"(?m)^\s*(refact-[a-z0-9-]+)\s*=", read(toml)))
deps["refact-lsp"] = set(re.findall(r"(?m)^\s*(refact-[a-z0-9-]+)\s*=", read(ENG / "Cargo.toml")))


def closure(start):
    seen, stack = set(), [start]
    while stack:
        for dep in deps.get(stack.pop(), ()):
            if dep not in seen:
                seen.add(dep)
                stack.append(dep)
    return seen


reaching = sorted(c for c in deps if "refact-tools" in closure(c))
check(
    reaching == ["refact-http", "refact-lsp"],
    "only refact-http + refact-lsp reach refact-tools, got %s" % reaching,
)
check(
    "refact-tools" not in closure("refact-runtime-api"),
    "refact-runtime-api must not reach refact-tools (it now hosts the browser layer)",
)
for crate in sorted(deps):
    check(crate not in closure(crate), "closure of %s does not contain itself" % crate)

# ------------------------------------------------ A3 the browser layer re-home
for name in ("browser_runtime", "browser_controller"):
    rapi = CRATE_RAPI / "src/integrations" / (name + ".rs")
    http = CRATE_HTTP / "src/integrations" / (name + ".rs")
    check(rapi.is_file(), "refact-runtime-api hosts %s" % name)
    check(
        read(http).strip() == "pub use refact_runtime_api::integrations::%s::*;" % name,
        "refact-http/%s.rs is a 1-line shim" % name,
    )
    if rapi.is_file():
        text = read(rapi)
        check("refact_runtime_api::" not in text, "runtime-api/%s.rs has no self-referential path" % name)
        check(
            "crate::routers::v1::browser_settings" not in text,
            "runtime-api/%s.rs imports no http module" % name,
        )
rapi_mod = read(CRATE_RAPI / "src/integrations/mod.rs")
check(
    "pub mod browser_runtime;" in rapi_mod and "pub mod browser_controller;" in rapi_mod,
    "refact-runtime-api declares both browser modules",
)
browser_runtime_text = read(CRATE_RAPI / "src/integrations/browser_runtime.rs")
for cross_path in (
    '"/../refact-http/src/routers/v1/v1_browser.rs"',
    '"/../refact-tools/src/tools/tool_chrome.rs"',
):
    check(
        cross_path in browser_runtime_text,
        "moved browser_runtime.rs rewrites include_str! to %s" % cross_path,
    )
check(
    'env!("CARGO_MANIFEST_DIR")' in browser_runtime_text,
    "the cross-crate include_str! is manifest-relative",
)

# Zero-behaviour-change proof for the re-home: the documented rewrite of the
# pre-move refact-http file reproduces the new refact-runtime-api file exactly.
BROWSER_REWRITES = [
    ("refact_runtime_api::", "crate::"),
    ("use crate::routers::v1::browser_settings;", "use refact_browser::settings as browser_settings;"),
    (
        'include_str!("../routers/v1/v1_browser.rs")',
        'include_str!(concat!(\n            env!("CARGO_MANIFEST_DIR"),\n'
        '            "/../refact-http/src/routers/v1/v1_browser.rs"\n        ))',
    ),
    (
        'include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../src/tools/tool_chrome.rs"))',
        'include_str!(concat!(\n            env!("CARGO_MANIFEST_DIR"),\n'
        '            "/../refact-tools/src/tools/tool_chrome.rs"\n        ))',
    ),
]
for name in ("browser_runtime", "browser_controller"):
    before = git_show("refact-agent/engine/crates/refact-http/src/integrations/%s.rs" % name)
    check(bool(before), "git show %s returned the pre-move refact-http file" % name)
    rewritten = before
    for old, new in BROWSER_REWRITES:
        rewritten = rewritten.replace(old, new)
    check(
        rewritten == read(CRATE_RAPI / "src/integrations" / (name + ".rs")),
        "refact-runtime-api/%s.rs is the pre-move refact-http file after the documented rewrite" % name,
    )

# -------------------------------------------------- B4..B7 the two tool moves
for name, spans in KEPT_TESTS.items():
    before = git_show("refact-agent/engine/src/tools/%s.rs" % name)
    check(bool(before), "git show %s returned the pre-move file" % name)
    check("#[cfg(test)]" in before, "%s pre-move file has a #[cfg(test)] block" % name)

    crate_file = CRATE_TOOLS / (name + ".rs")
    engine_file = SRC / (name + ".rs")
    check(crate_file.is_file(), "crates/refact-tools/.../tools/%s.rs exists" % name)
    check(
        read(engine_file).startswith("pub use refact_tools::tools::%s::*;" % name),
        "engine %s.rs is a glob shim" % name,
    )
    if not (crate_file.is_file() and engine_file.is_file()):
        continue

    after = read(crate_file)
    body_after = strip_cfg_test(after)
    leftovers = sorted(p for p in set(ENGINE_PATH.findall(body_after)) if p != ALLOWED_ENGINE_PATH)
    check(not leftovers, "%s production body still references engine paths: %s" % (name, leftovers))

    # C6: forward rewrite of the pre-move production body must reproduce the
    # crate body, ignoring the seams of the tests that stayed in the engine.
    expected = forward(cut_kept_tests(before, spans))
    check(
        collapse_blank_runs(expected) == collapse_blank_runs(after),
        "%s crate body equals the forward rewrite of the pre-move file" % name,
    )

    # C7: no test line lost; the kept spans must live in the engine shim.
    engine_text = read(engine_file)
    lost = []
    before_tests = before[len(strip_cfg_test(before)):]
    for line in before_tests.splitlines():
        if not line.strip():
            continue
        candidates = {line, line.strip(), forward(line), forward(line).strip()}
        if any(c in engine_text or c in after for c in candidates):
            continue
        lost.append(line.strip())
    check(not lost, "%s lost no test line: %s" % (name, lost[:5]))

    for a, b in spans:
        chunk = "\n".join(before.splitlines()[a - 1:b])
        for line in chunk.splitlines():
            if not line.strip():
                continue
            if line in engine_text or line.strip() in engine_text:
                continue
            check(False, "%s kept-test line missing from the engine shim: %s" % (name, line.strip()[:70]))
            break

engine_design = read(SRC / "design_tools.rs")
check("make_test_gcx" in engine_design, "the make_test_gcx tests stayed in the engine shim")
check(
    "image_region_accepts_artifact_paths_and_bare_artifact_names" in engine_design
    and "visual_diff_confirms_only_explicit_baseline_updates" in engine_design,
    "both engine-resident design tests are present",
)

for leaf, item in sorted(LEAF_SHIMS.items()):
    text = read(SRC / (leaf + ".rs")).strip()
    check(
        text == "pub use refact_tools::tools::design_tools::%s;" % item,
        "%s.rs points at the crate (%s)" % (leaf, text[:60]),
    )

check(
    "pub mod tool_chrome;" in read(ENG / "crates/refact-tools/src/tools.rs")
    and "pub mod design_tools;" in read(ENG / "crates/refact-tools/src/tools.rs"),
    "refact-tools declares both moved modules",
)

# ------------------------------------------------------------- B8 no engine dep
for toml in (ENG / "crates").glob("*/Cargo.toml"):
    check("refact-lsp" not in read(toml), "%s depends on refact-lsp" % toml.parent.name)

print("AB-4 checks: %d run, %d failed" % (checks, len(fails)))
for f in fails:
    print("  FAIL %s" % f)
sys.exit(1 if fails else 0)
