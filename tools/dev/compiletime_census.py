"""Compile-time oriented census (NOT a maintainability census).

The question this answers: "how much does the compiler actually have to chew",
which is decided by **crate boundaries and dependency edges**, not by file/module counts.

Reports:
  1. per-crate production lines (a crate = one rustc invocation = one unit of incremental work)
  2. per-crate direct dependency count (each dep = extra metadata to load + codegen surface)
  3. per-crate fan-in: how many other crates depend on it (fan-in X means editing X rebuilds its dependents)
  4. engine `refact-lsp` own prod lines (this is the unit you pay for on EVERY edit inside it)
"""
import os
import re
from collections import defaultdict

ENGINE = os.environ.get("REFACT_ENGINE") or os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "refact-agent", "engine")
)
CRATES = os.path.join(ENGINE, "crates")
SRC = os.path.join(ENGINE, "src")

INLINE_TESTS = re.compile(r"^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]", re.M)


def read(p):
    try:
        with open(p, "r", encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def prod_lines(text):
    m = INLINE_TESTS.search(text)
    body = text[: m.start()] if m else text
    return body.count("\n") + 1


def walk_rs(base):
    out = []
    for root, dirs, files in os.walk(base):
        dirs[:] = [d for d in dirs if d != "target"]
        for n in files:
            if n.endswith(".rs"):
                out.append(os.path.join(root, n))
    return out


def manifest_info(path):
    txt = read(path)
    dep = re.search(r"^\[dependencies\]\s*$(.*?)(?=^\[|\Z)", txt, re.M | re.S)
    deps = []
    if dep:
        for line in dep.group(1).splitlines():
            m = re.match(r"^([A-Za-z0-9_\-]+)\s*=", line)
            if m:
                deps.append(m.group(1))
    return deps


# ------------------------------------------------------------------- crates
crate_rows = {}
for name in sorted(os.listdir(CRATES)):
    cdir = os.path.join(CRATES, name)
    if not os.path.isdir(cdir):
        continue
    files = walk_rs(cdir)
    if not files:
        continue
    prod = 0
    tests = 0
    for f in files:
        txt = read(f)
        p = prod_lines(txt)
        prod += p
        tests += (txt.count("\n") + 1) - p
    deps = manifest_info(os.path.join(cdir, "Cargo.toml"))
    crate_rows[name] = {
        "files": len(files),
        "prod": prod,
        "test": tests,
        "deps": deps,
        "refact_deps": [d for d in deps if d.startswith("refact-")],
    }

# engine itself
engine_files = walk_rs(SRC)
engine_prod = 0
engine_test = 0
for f in engine_files:
    txt = read(f)
    p = prod_lines(txt)
    engine_prod += p
    engine_test += (txt.count("\n") + 1) - p
engine_deps = manifest_info(os.path.join(ENGINE, "Cargo.toml"))

# fan-in
fan_in = defaultdict(int)
for name, row in crate_rows.items():
    for d in row["refact_deps"]:
        fan_in[d] += 1
for d in engine_deps:
    if d.startswith("refact-"):
        fan_in[d] += 1

print("=" * 108)
print("CRATE = UNIT OF INCREMENTAL WORK.  'prod' = lines the compiler must typecheck+codegen.")
print("=" * 108)
print(f"{'crate':<30}{'files':>6}{'prod':>9}{'test':>9}{'deps':>6}{'refact':>7}{'fan-in':>7}")
print("-" * 108)
print(
    f"{'refact-lsp (ENGINE)':<30}{len(engine_files):>6}{engine_prod:>9}{engine_test:>9}"
    f"{len(engine_deps):>6}{len([d for d in engine_deps if d.startswith('refact-')]):>7}{'-':>7}"
)
for name, row in sorted(crate_rows.items(), key=lambda kv: -kv[1]["prod"]):
    print(
        f"{name:<30}{row['files']:>6}{row['prod']:>9}{row['test']:>9}"
        f"{len(row['deps']):>6}{len(row['refact_deps']):>7}{fan_in.get(name, 0):>7}"
    )
print("-" * 108)
print(f"{'TOTAL':<30}{len(engine_files) + sum(r['files'] for r in crate_rows.values()):>6}"
      f"{engine_prod + sum(r['prod'] for r in crate_rows.values()):>9}"
      f"{engine_test + sum(r['test'] for r in crate_rows.values()):>9}")

print()
print("=" * 108)
print("WHAT AN ENGINE EDIT COSTS TODAY")
print("=" * 108)
print(f"refact-lsp own production lines : {engine_prod:,}")
print(f"refact-lsp direct dependencies  : {len(engine_deps)} ({len([d for d in engine_deps if d.startswith('refact-')])} refact-*)")
print()
print("An edit anywhere inside refact-lsp/src re-typechecks ALL of these lines in ONE rustc")
print("invocation, then re-links. File count and module count inside the crate do NOT change that.")
print()
print("Top 15 crates by production size (the candidates whose *extraction from refact-lsp* pays off):")
for name, row in sorted(crate_rows.items(), key=lambda kv: -kv[1]["prod"])[:15]:
    print(f"  {name:<30}{row['prod']:>8} prod   fan-in {fan_in.get(name, 0)}")
