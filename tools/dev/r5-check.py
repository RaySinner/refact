"""R-5 verification harness: the `src/app_state.rs` split is mechanical.

The card's own gate (`tools/dev/engine-build.ps1 -Mode check`, exit 0) proves the
result compiles; this harness proves *what* was left behind is unchanged. Every check
is a filesystem / git-blob comparison, and the command has no shell separator:

    python tools/dev/r5-check.py

Checks
------
1. line accounting: base prod/test split, new parent prod size, child sizes;
2. every base line survives exactly once across parent + children, modulo only the
   declared `pub(super)` widening and the generated `mod`/glob block;
3. untouched surfaces: the parent's `pub use` re-export lines and its
   `#[cfg(test)] mod tests` blob are byte-identical to the base;
4. children are leaves: header + a verbatim slice, no nested module, no
   `crate::app_state::` back-reference;
5. no duplicate top-level type/const definition across the whole module tree;
6. the intended islands are still defined in the parent and nowhere else;
7. the census is re-run and printed (informational - submodule granularity changes
   its per-file verdicts, see the card report).
"""
import pathlib
import re
import subprocess
import sys
from collections import Counter

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENGINE = ROOT / "refact-agent" / "engine"
SRC = ENGINE / "src"
PARENT = SRC / "app_state.rs"
CHILD_DIR = SRC / "app_state"
BASE = "60c7da99f"
BASE_REL = "refact-agent/engine/src/app_state.rs"

CHILDREN = [
    "facade",
    "launchers",
    "prompt_and_tools",
    "providers",
    "sinks",
    "tool_pool",
    "tool_registry",
    "tool_registry_trait",
    "trajectory_store",
]

ISLANDS = [
    "pub struct EngineContextFactory;",
    "impl ContextFactory for EngineContextFactory {",
    "struct EngineCapsLoader;",
    "impl CapsLoader for EngineCapsLoader {",
    "pub struct EngineExecPolicyProvider {",
    "impl ExecPolicyProvider for EngineExecPolicyProvider {",
    "pub struct EngineFilesProvider {",
    "impl FilesProvider for EngineFilesProvider {",
]

TEST_START = "#[cfg(test)]\nmod tests {"
DEF_RE = re.compile(
    r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?"
    r"(?:struct|enum|union|type|trait|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)"
)

checks = []


def check(name, ok, detail=""):
    checks.append((name, bool(ok), detail))
    print("%-5s %s%s" % ("ok" if ok else "FAIL", name, ("  -- " + detail) if detail else ""))


def read(path):
    return pathlib.Path(path).read_text(encoding="utf-8").replace("\r\n", "\n")


def base_text():
    return subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:%s" % (BASE, BASE_REL)],
        capture_output=True,
        check=True,
    ).stdout.decode("utf-8").replace("\r\n", "\n")


def lines(text):
    """Line list without the trailing-empty artifact of a final newline."""
    if text.endswith("\n"):
        text = text[:-1]
    return text.split("\n")


WIDENED = re.compile(r"^(?P<indent>\s*)pub\(super\)\s+(?P<rest>.*)$")


def normalize(line):
    match = WIDENED.match(line)
    return match.group("indent") + match.group("rest") if match else line


def generated_block():
    lines = ["mod %s;" % name for name in CHILDREN] + [""]
    for name in CHILDREN:
        lines += ["#[allow(unused_imports)]", "pub use self::%s::*;" % name]
    return lines


def strip_test_module(text):
    index = text.index(TEST_START)
    return text[:index]


def definitions(text):
    found = Counter()
    for line in text.split("\n"):
        match = DEF_RE.match(line)
        if match:
            found[match.group(1)] += 1
    return found


def main():
    base = base_text()
    base_lines = lines(base)
    base_prod = strip_test_module(base)
    base_prod_lines = lines(base_prod)

    parent = read(PARENT)
    parent_lines = lines(parent)
    parent_prod = strip_test_module(parent)
    parent_prod_lines = lines(parent_prod)
    children = {name: read(CHILD_DIR / (name + ".rs")) for name in CHILDREN}

    print("### 1. line accounting")
    print("  base:   %d lines total / %d prod / %d test" % (
        len(base_lines), len(base_prod_lines), len(base_lines) - len(base_prod_lines)))
    print("  parent: %d lines total / %d prod / %d test" % (
        len(parent_lines), len(parent_prod_lines), len(parent_lines) - len(parent_prod_lines)))
    print("  children: " + ", ".join(
        "%s=%d" % (n, len(lines(children[n]))) for n in CHILDREN))
    check("parent prod part is under the 800-line god-file threshold",
          len(parent_prod_lines) < 800, "%d prod lines" % len(parent_prod_lines))
    check("every child is under 800 lines",
          all(len(lines(children[n])) < 800 for n in CHILDREN),
          str({n: len(lines(children[n])) for n in CHILDREN if len(lines(children[n])) >= 800}))

    # ---- 2. multiset equality of content ---------------------------------
    block = generated_block()
    parent_body = list(parent_lines)
    index = None
    for i in range(len(parent_body) - len(block) + 1):
        if parent_body[i:i + len(block)] == block:
            index = i
            break
    check("parent contains the generated mod/glob block", index is not None)
    if index is not None:
        del parent_body[index:index + len(block)]

    rebuilt = list(parent_body)
    header_ok = []
    for name in CHILDREN:
        body = lines(children[name])
        header_ok.append(body[:3] == ["#[allow(unused_imports)]", "use super::*;", ""])
        rebuilt.extend(body[3:])

    rebuilt = [line for line in rebuilt]
    if rebuilt and rebuilt[-1] == "":
        rebuilt.pop()

    expected = Counter(normalize(line) for line in base_lines)
    actual = Counter(normalize(line) for line in rebuilt)
    missing = sorted(line for line in expected if expected[line] > actual[line])
    extra = sorted(line for line in actual if actual[line] > expected[line])
    check("no base line lost", not missing, "%d lost, e.g. %r" % (len(missing), missing[:3]))
    check("no line duplicated or invented", not extra, "%d extra, e.g. %r" % (len(extra), extra[:3]))
    check("all %d children start with the child header" % len(CHILDREN), all(header_ok))
    check("every `pub(super)` widening is inside a child",
          not any("pub(super)" in line for line in parent_body),
          str([line for line in parent_body if "pub(super)" in line][:3]))

    # ---- 3. untouched surfaces -------------------------------------------
    base_reexports = [line for line in base_prod_lines if line.startswith("pub use ")]
    parent_reexports = [
        line for line in parent_prod_lines
        if line.startswith("pub use ") and "pub use self::" not in line
    ]
    check("every base re-export line survives verbatim in the parent",
          base_reexports == parent_reexports,
          "%d vs %d" % (len(base_reexports), len(parent_reexports)))

    base_test = base[base.index(TEST_START):]
    check("test module is byte-identical and lives in the parent", base_test in parent)
    check("parent keeps exactly one test module", parent.count(TEST_START) == 1)
    for name in CHILDREN:
        check("%s.rs carries no test module" % name, TEST_START not in children[name])

    # ---- 4. children are leaves ------------------------------------------
    for name in CHILDREN:
        body = children[name]
        check("%s.rs never reaches crate::app_state::" % name, "crate::app_state::" not in body)
        check("%s.rs declares no nested module" % name,
              not re.search(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[a-z_]", body))

    # ---- 5. no duplicate definitions across the tree ----------------------
    owners = {}
    for name, text in [("parent", parent_prod)] + [(n, children[n]) for n in CHILDREN]:
        for item in definitions(text):
            owners.setdefault(item, []).append(name)
    duplicates = {item: where for item, where in owners.items() if len(where) > 1}
    check("no top-level type/const is defined twice in the module tree",
          not duplicates, str(duplicates))
    print("  top-level definitions across the tree: %d" % len(owners))

    # ---- 6. islands -------------------------------------------------------
    parent_stripped = [line.strip() for line in parent_lines]
    for island in ISLANDS:
        check("island stays in the parent: %s" % island.strip(), island in parent_stripped)
    for name in CHILDREN:
        moved = [line.strip() for line in children[name].split("\n")]
        clash = [line for line in ISLANDS if line in moved]
        check("%s.rs does not hold a parent island" % name, not clash, str(clash))

    # ---- 7. census (informational) ---------------------------------------
    print("\n### 7. census (informational)")
    out = subprocess.run(
        [sys.executable, str(ROOT / "tools" / "dev" / "chat_movable_census.py"), "app_state"],
        cwd=str(ROOT), capture_output=True, text=True,
    )
    print(out.stdout.strip())
    check("census ran", out.returncode == 0, out.stderr.strip()[-200:])

    failed = [name for name, ok, _ in checks if not ok]
    print()
    print("%d/%d checks passed" % (len(checks) - len(failed), len(checks)))
    if failed:
        print("RESULT: FAILED")
        for name in failed:
            print("  - " + name)
        return 1
    print("RESULT: ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
