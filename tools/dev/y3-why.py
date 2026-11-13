"""Explain why an engine file is blocked: print every `crate::` ref and its origin.

Loads census_module.py (which resolves the whole engine's export graph) and then
reports, per requested file, each `crate::` reference (grouped `use` statements
expanded) together with how census would resolve it.

Usage: python y3-why.py http/routers/v1/exec.rs [more files...]
"""
import contextlib
import importlib.util
import io
import os
import re
import sys

CENSUS = os.path.join(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
    "refact-agent",
    "engine",
    "target",
    "census_module.py",
)

if not os.path.exists(CENSUS):
    raise SystemExit(
        f"{CENSUS} is missing; it is an untracked analysis helper. Copy "
        "census_module.py into engine/target/ (with ENGINE pointing at this "
        "checkout) before running this script."
    )

spec = importlib.util.spec_from_file_location("census_module", CENSUS)
census = importlib.util.module_from_spec(spec)
_argv = sys.argv
sys.argv = ["census_module.py", "http"]
with contextlib.redirect_stdout(io.StringIO()):
    spec.loader.exec_module(census)
sys.argv = _argv

USE_STMT = re.compile(r"\buse[ \t]+([^;]+);", re.S)


def refs(body):
    out = set()
    for m in USE_STMT.finditer(body):
        parts = []
        census.expand("", m.group(1), parts)
        for full in parts:
            if full.startswith("crate::"):
                out.add(full[len("crate::"):])
    for m in census.USE_PATH.finditer(re.sub(USE_STMT, "", body)):
        out.add(m.group(1))
    return sorted(out)


def main():
    show_all = False
    targets = []
    for arg in sys.argv[1:]:
        if arg == "--all":
            show_all = True
        else:
            targets.append(arg)
    for target in targets:
        rel = target.replace("/", "::").removesuffix(".rs")
        info = census.info.get(rel)
        state = "movable" if rel in census.movable else "blocked"
        print(f"##### {rel} ({state})")
        if not info:
            print("  <not found>")
            continue
        body = census.strip_comments(census.prod_part(census.read(census.files[rel])))
        for ref in refs(body):
            if ref.startswith("http") and ref not in census.info:
                continue
            kind, val = census.origin_of(ref)
            if kind == "crate" and not show_all:
                continue
            print(f"  {kind:8s} {ref}  ->  {val}")


main()
