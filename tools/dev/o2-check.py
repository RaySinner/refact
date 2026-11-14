"""O-2 verification harness: the `privacy::records::Ask` blocker is gone from the shell island.

The build guard refuses cargo in agent worktrees, so this proves the change statically:

  A  no `privacy::records::Ask` / `privacy::records::ShellReadDecision` anywhere in the engine
  B  both island tool files import `ShellReadDecision` from `refact_privacy::model_context`
  C  both match arms use the bare `ShellReadDecision::{Pass,Ask}` variants
  D  dropping `use` statements, the two files are byte-identical to HEAD outside the
     documented path rewrite, so no shell-permission decision or prompt text can have changed
  E  only the two island tool files changed (plus this harness)
  F  the residual island blockers are reported and no longer mention the privacy shim
  G  balanced delimiters / no merge markers

Usage: python tools/dev/o2-check.py
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = "refact-agent/engine"
TOOLS = ENGINE + "/src/tools"
TARGETS = [
    TOOLS + "/tool_process.rs",
    TOOLS + "/tool_shell.rs",
]
ISLAND = [
    "tools/tool_process.rs",
    "tools/tool_shell.rs",
    "tools/shell_gate.rs",
    "tools/shell_gate_llm.rs",
]

CHECKS = [0]
FAILURES = []


def check(ok, label, detail=""):
    CHECKS[0] += 1
    suffix = (" -- " + str(detail)) if (detail and not ok) else ""
    print(("PASS " if ok else "FAIL ") + label + suffix)
    if not ok:
        FAILURES.append(label)


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as fh:
        return fh.read()


def git(*args):
    return subprocess.run(
        ["git", "-C", ROOT, *args], capture_output=True, text=True, encoding="utf-8"
    )


def walk_engine():
    for dirpath, dirs, files in os.walk(os.path.join(ROOT, ENGINE, "src")):
        dirs[:] = [d for d in dirs if d != "target"]
        for name in files:
            if name.endswith(".rs"):
                yield os.path.join(dirpath, name)


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


STALE = re.compile(r"privacy::records::(?:ShellReadDecision|Ask|Pass)")


def main():
    # ---------------------------------------------------------------- A
    offenders = []
    for path in walk_engine():
        text = read(os.path.relpath(path, ROOT))
        for number, line in enumerate(text.splitlines(), 1):
            if STALE.search(line):
                offenders.append(
                    "%s:%d %s" % (os.path.relpath(path, ROOT).replace("\\", "/"), number, line.strip())
                )
    check(not offenders, "A no `privacy::records::Ask` reference left in the engine", str(offenders))
    check(
        not git("grep", "-n", "privacy::records::Ask", "--", TOOLS).stdout.strip(),
        "A `git grep privacy::records::Ask src/tools` returns nothing",
    )

    for path in TARGETS:
        text = read(path)
        # ------------------------------------------------------------ B
        check(
            re.search(
                r"(?m)^use refact_privacy::model_context::ShellReadDecision;$", text
            )
            is not None,
            "B %s imports ShellReadDecision from refact_privacy::model_context"
            % os.path.basename(path),
        )
        # ------------------------------------------------------------ C
        check(
            "ShellReadDecision::Pass => Ok(())," in text
            and "ShellReadDecision::Ask => Ok(())," in text,
            "C %s uses the bare Pass/Ask variants" % os.path.basename(path),
        )
        # ------------------------------------------------------------ D
        original = git("show", "HEAD:%s" % path).stdout
        before = [
            line.replace("crate::privacy::records::ShellReadDecision", "ShellReadDecision")
            for line in without_uses(original)
        ]
        after = without_uses(text)
        check(
            before == after,
            "D %s body identical to HEAD outside the import block" % os.path.basename(path),
        )
        if before != after:
            for i, (a, b) in enumerate(zip(before, after)):
                if a != b:
                    print("   first diff at body line %d: %r != %r" % (i, a, b))
                    break
        # ------------------------------------------------------------ G
        check(balance(text), "G %s balanced delimiters" % os.path.basename(path))
        check(
            "<<<<<<<" not in text and ">>>>>>>" not in text,
            "G %s no merge markers" % os.path.basename(path),
        )

    # ---------------------------------------------------------------- E
    status = git("status", "--porcelain", "--untracked-files=all").stdout
    touched = sorted(
        {
            line[3:].strip().replace("\\", "/")
            for line in status.splitlines()
            if line[3:] and not line[3:].strip().startswith(".refact")
        }
    )
    allowed = set(TARGETS) | {"tools/dev/o2-check.py"}
    stray = [t for t in touched if t not in allowed]
    check(not stray, "E no file outside the two island tools changed", str(stray))

    # ---------------------------------------------------------------- F
    result = subprocess.run(
        [sys.executable, os.path.join(ROOT, "tools", "dev", "y3-blockers.py"), "--file", *ISLAND],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    blockers = result.stdout.strip()
    print("\n=== residual island blockers (y3-blockers.py)")
    print(blockers)
    check(
        blockers.count("BLOCKED") == len(ISLAND),
        "F blocker report produced",
        blockers,
    )
    check(
        "privacy::records" not in blockers,
        "F privacy shim is no longer a blocker",
        blockers,
    )
    check(
        all("shell_gate" in line.split("<-")[-1] for line in blockers.splitlines()),
        "F residual blocker is the shell island itself",
        blockers,
    )

    print("\n%d/%d checks passed" % (CHECKS[0] - len(FAILURES), CHECKS[0]))
    if FAILURES:
        for failure in FAILURES:
            print("FAIL %s" % failure)
        print("RESULT: FAILED")
        return 1
    print("RESULT: ALL CHECKS PASSED")
    return 0


sys.exit(main())
