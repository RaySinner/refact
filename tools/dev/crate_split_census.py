"""Reverse-dependency census for a crate: which top-level modules can leave it
without creating a crate -> crate cycle.

A module M can become part of a NEW crate iff nothing else in the source crate
references M (otherwise the source crate would need NEW, and NEW needs the source
crate -> cycle). Modules with inbound edges stay until their inbound callers move.

Usage:
    python tools/dev/crate_split_census.py [min_lines] [crate_dir]

    crate_dir defaults to refact-agent/engine/crates/refact-runtime-api
    (resolved relative to this script, so it works from any worktree).

Reports, per top-level module >= min_lines:
    module | lines | inbound from | outbound needs
and then the zero-inbound set with the total, plus mutual clusters (a <-> b),
which cannot be split apart without a trait seam.
"""
import os
import re
import sys
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))

MIN_LINES = int(sys.argv[1]) if len(sys.argv) > 1 else 200
CRATE = (
    sys.argv[2]
    if len(sys.argv) > 2
    else os.path.join(REPO, "refact-agent", "engine", "crates", "refact-runtime-api")
)
SRC = os.path.join(CRATE, "src")

USE_RE = re.compile(r"\buse\s+crate::([a-zA-Z0-9_]+)")
INLINE_RE = re.compile(r"\bcrate::([a-zA-Z0-9_]+)::")


def top_module(relpath):
    head = relpath.split(os.sep)[0]
    return head[:-3] if head.endswith(".rs") else head


def main():
    if not os.path.isdir(SRC):
        print("no such source dir: %s" % SRC)
        return 2

    files = []
    for dp, _, fs in os.walk(SRC):
        for f in sorted(fs):
            if not f.endswith(".rs"):
                continue
            p = os.path.join(dp, f)
            rel = os.path.relpath(p, SRC)
            text = open(p, encoding="utf-8", errors="ignore").read()
            deps = set(USE_RE.findall(text)) | set(INLINE_RE.findall(text))
            files.append((rel, top_module(rel), len(text.splitlines()), deps))

    sizes = defaultdict(int)
    for _, mod, n, _ in files:
        sizes[mod] += n

    inbound = defaultdict(set)
    outbound = defaultdict(set)
    for _rel, mod, _n, deps in files:
        for d in deps:
            if d == mod:
                continue
            outbound[mod].add(d)
            inbound[d].add(mod)

    rows = sorted(sizes.items(), key=lambda kv: -kv[1])

    print("crate: %s" % CRATE)
    print("=== modules >= %d lines, with inbound edges ===" % MIN_LINES)
    print("%-32s %7s  %s" % ("module", "lines", "inbound from"))
    print("-" * 110)
    for mod, n in rows:
        if n < MIN_LINES:
            continue
        ins = sorted(inbound.get(mod, ()))
        print("%-32s %7d  %s" % (mod, n, ", ".join(ins) if ins else "-- NONE --"))

    print()
    print("=== zero-inbound modules (candidates for a new crate) ===")
    total = 0
    for mod, n in rows:
        if n < MIN_LINES or inbound.get(mod):
            continue
        print("%-32s %7d  needs: %s" % (mod, n, ", ".join(sorted(outbound.get(mod, ())))))
        total += n
    print("-" * 110)
    print("total zero-inbound lines >= %d: %d" % (MIN_LINES, total))

    print()
    print("=== mutual clusters (pairs referencing each other) among >= %d ===" % MIN_LINES)
    big = sorted({m for m, n in rows if n >= MIN_LINES})
    for i, a in enumerate(big):
        for b in big[i + 1:]:
            if b in outbound.get(a, ()) and a in outbound.get(b, ()):
                print("  %s <-> %s  (%d + %d lines)" % (a, b, sizes[a], sizes[b]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
