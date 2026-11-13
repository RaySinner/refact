"""Y-3 verification harness: five http v1 routers moved into `refact-http`.

The build guard refuses cargo, so this proves the move statically:

  R1  each engine file is a one-line glob shim into `refact-http`
  R2  each module is declared on both sides (crate `routers/v1/mod.rs`, engine `v1.rs`)
  R3  the moved body contains no `crate::` reference and keeps its inline tests 1:1
  R4  every external crate root used is a declared `refact-http` dependency and is
      listed in the `refact-http` Cargo.lock entry
  R5  dropping `use` statements, the moved file is byte-identical to the original on
      all other lines, except the documented `write_file_atomic` visibility widening
  R6  balanced delimiters / no merge markers
  R7  no other engine file changed, so no route path, SSE event name or JSON payload
      can have changed

Usage: python tools/dev/y3-check.py
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = "refact-agent/engine"
V1 = ENGINE + "/src/http/routers/v1"
CRATE_V1 = ENGINE + "/crates/refact-http/src/routers/v1"

FILES = [
    "ext_management",
    "plugins",
    "buddy_frontend_error",
    "mcp_interactions",
    "gui_help_handlers",
]

STD = {"std", "core", "alloc", "crate", "self", "super", "test", "tests", "main"}

FAILURES = []
CHECKS = [0]


def check(ok, label):
    CHECKS[0] += 1
    if not ok:
        FAILURES.append(label)


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as fh:
        return fh.read()


def git(*args):
    return subprocess.run(
        ["git", "-C", ROOT, *args], capture_output=True, text=True, check=True
    ).stdout


def without_uses(text):
    """Drop every `use ...;` statement, including multi-line grouped ones."""
    kept, skipping = [], False
    for line in text.splitlines():
        stripped = line.strip()
        if skipping:
            if ";" in stripped:
                skipping = False
            continue
        if re.match(r"^(pub(\([^)]*\))?\s+)?use\s", stripped):
            if ";" not in stripped:
                skipping = True
            continue
        kept.append(line.rstrip())
    return [line for line in kept if line.strip()]


def balance(text):
    stack = []
    pairs = {")": "(", "]": "[", "}": "{"}
    i = 0
    while i < len(text):
        ch = text[i]
        if text[i : i + 2] == "//":
            i = text.find("\n", i)
            if i == -1:
                break
            continue
        if ch == '"':
            i += 1
            while i < len(text) and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
            i += 1
            continue
        if ch == "'":
            m = re.match(r"'(\\.|[^\\'])'", text[i:])
            if m:
                i += m.end()
                continue
        if ch in "([{":
            stack.append(ch)
        elif ch in ")]}":
            if not stack or stack.pop() != pairs[ch]:
                return False
        i += 1
    return not stack


def declared_deps():
    deps = set()
    for line in read(ENGINE + "/crates/refact-http/Cargo.toml").splitlines():
        m = re.match(r"^([A-Za-z0-9_-]+)\s*=", line)
        if m:
            deps.add(m.group(1).replace("-", "_"))
    return deps


def used_roots(text):
    found = set()
    for m in re.finditer(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([^;]+);", text, re.S
    ):
        head = (
            re.sub(r"\s+", " ", m.group(1)).split("{")[0].rstrip(" :").split("::")[0].strip()
        )
        if re.match(r"^[a-z_][a-z0-9_]*$", head):
            found.add(head)
    body = re.sub(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+[^;]+;", "", text
    )
    found |= set(re.findall(r"(?<![:\w])([a-z_][a-z0-9_]*)::(?=[A-Za-z_])", body))
    return {f for f in found - STD if f != "refact_http"}


def main():
    deps = declared_deps()
    lock = read(ENGINE + "/Cargo.lock")
    start = lock.find('name = "refact-http"')
    lock_block = lock[start : lock.find("[[package]]", start)]
    lock_names = {
        re.sub(r" .*$", "", entry).replace("-", "_")
        for entry in re.findall(r'^ "([^"]+)",$', lock_block, re.M)
    }
    crate_v1_mod = read(CRATE_V1 + "/mod.rs")
    engine_v1 = read(V1 + ".rs")

    # Walk back from HEAD to the last commit where the engine file was not yet a
    # shim; that commit holds the pre-move source. Y3_BASE overrides it.
    base = os.environ.get("Y3_BASE")
    if not base:
        rev = "HEAD"
        probe = f"{V1}/{FILES[0]}.rs"
        while True:
            content = git("show", f"{rev}:{probe}")
            if content.strip() != f"pub use refact_http::routers::v1::{FILES[0]}::*;":
                break
            rev = git("rev-parse", f"{rev}^").strip()
        base = rev
    print(f"# base commit: {base}")

    for name in FILES:
        engine_text = read(f"{V1}/{name}.rs")
        crate_text = read(f"{CRATE_V1}/{name}.rs")
        original = git("show", f"{base}:{V1}/{name}.rs")

        check(
            engine_text.strip() == f"pub use refact_http::routers::v1::{name}::*;",
            f"R1 {name}: engine file is a one-line glob shim",
        )
        check(f"pub mod {name};" in crate_v1_mod, f"R2 {name}: declared in crate v1/mod.rs")
        check(
            re.search(rf"(?m)^(?:pub )?mod {name};", engine_v1) is not None,
            f"R2 {name}: still declared in engine v1.rs",
        )
        check("crate::" not in crate_text, f"R3 {name}: no crate:: reference left")
        check(
            crate_text.count("#[cfg(test)]") == original.count("#[cfg(test)]"),
            f"R3 {name}: inline tests moved 1:1",
        )
        for root in used_roots(crate_text):
            check(root in deps, f"R4 {name}: `{root}` declared in refact-http Cargo.toml")
            check(root in lock_names, f"R4 {name}: `{root}` in the lock entry")

        before = without_uses(original)
        after = without_uses(crate_text)
        after = [
            line.replace("pub async fn write_file_atomic(", "PLACEHOLDER")
            for line in after
        ]
        before = [
            line.replace("pub(crate) async fn write_file_atomic(", "PLACEHOLDER")
            for line in before
        ]
        check(
            before == after,
            f"R5 {name}: body identical to HEAD outside the import block",
        )
        if before != after:
            for i, (a, b) in enumerate(zip(before, after)):
                if a != b:
                    print(f"   {name} first diff at body line {i}: {a!r} != {b!r}")
                    break
        check(balance(crate_text), f"R6 {name}: balanced delimiters")
        check(
            "<<<<<<<" not in crate_text and ">>>>>>>" not in crate_text,
            f"R6 {name}: no merge markers",
        )

    dirty = git("diff", "--name-only", base, "--", ENGINE).split()
    allowed = (
        {f"{V1}/{n}.rs" for n in FILES}
        | {f"{CRATE_V1}/{n}.rs" for n in FILES}
        | {
            ENGINE + "/Cargo.lock",
            ENGINE + "/crates/refact-http/Cargo.toml",
            ENGINE + "/crates/refact-http/src/routers/v1/mod.rs",
        }
    )
    unexpected = sorted(set(dirty) - allowed)
    check(not unexpected, f"R7 no unrelated engine change (got {unexpected})")

    print(f"{CHECKS[0]} checks, {len(FAILURES)} failures")
    for failure in FAILURES:
        print(f"FAIL {failure}")
    return 1 if FAILURES else 0


sys.exit(main())
