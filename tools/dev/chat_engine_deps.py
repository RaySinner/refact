"""Trustworthy engine-resident-dependency report for an engine module tree.

`tools/dev/chat_movable_census.py` decides "blocked" from a per-file edge set that
is built by scanning the body with `REF_RE`, which only matches paths that start
with `crate::`/`super::`/`self::`. That produces two systematic false positives:

1. `use crate::foo::{a, b};` -- the regex stops at `crate::foo` (the `{` breaks the
   `::ident` chain), so the *module* is recorded instead of the imported symbols.
   A file whose only engine-looking dep is a brace import of a pure crate-shim
   module is reported blocked although every symbol it imports comes from a crate.
2. `crate::git::FileChangeStatus::ADDED` -- the last segment (`ADDED`, an enum
   variant) is looked up as an export of `git` and, not being there, the whole
   path is classified engine. Any associated-path / variant path is thus engine.

`uses` (the brace-expanded import list the census already computes for its
export chain) is never turned into an edge.

This script recomputes the edge set for every file with those two defects fixed
(brace imports expanded per symbol, and the first *known* symbol among the
trailing segments used instead of the last), and prints per blocked file:

* the still-in-tree chat dependencies (closure partners),
* genuine engine dependencies with the source line,
* the non-chat blocker modules ranked by how many chat files they block.

`use super::*;` (invisible to `REF_RE`) is treated as an edge to the parent
module, which is what it means.
"""
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
CENSUS = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")
TARGET = sys.argv[1] if len(sys.argv) > 1 else "chat"

spec = importlib.util.spec_from_file_location("census", CENSUS)
census = importlib.util.module_from_spec(spec)
sys.argv = ["census", TARGET]
spec.loader.exec_module(census)

mods = census.mods
exports = census.exports
wild = census.wild
test_only = census.test_only
files = census.files
is_island = census.is_island
CRATE_RE = census.CRATE_RE
REF_RE = census.REF_RE

BODY = {m: census.code(census.prod(census.read(mods[m]))) for m in mods if m not in test_only}
SUPER_GLOB = re.compile(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+((?:super\s*::\s*)+)\s*\*\s*;")


def symbol_of(tmod, rest):
    """Pick the first trailing segment that is a real export of `tmod`."""
    for seg in rest:
        if seg in exports.get(tmod, {}):
            return seg
    return None


def norm_segments(rest):
    """`use x::{self}` / `x::Y as Z` produce synthetic segments the resolver must
    ignore rather than treat as symbols."""
    out = []
    for seg in rest:
        seg = seg.split(" as ")[0].strip()
        if seg and seg != "self":
            out.append(seg)
    return out


def edges(mod):
    """(crates, in_tree, engine) as sets of module paths / symbol paths."""
    body = BODY[mod]
    crates, tree, engine = set(), set(), set()

    def classify(path):
        target = census.longest_module(census.absolute(mod, path))
        tmod, rest = target
        rest = norm_segments(rest)
        if tmod is None or tmod == mod:
            return
        if not rest or rest[-1] == "*":
            if tmod in wild:
                crates.update(wild[tmod])
            elif tmod in files:
                tree.add(tmod)
            else:
                engine.add(tmod)
            return
        sym = symbol_of(tmod, rest) or rest[0]
        kind, value = census.symbol_origin(tmod, sym)
        if kind == "crate":
            crates.add(value)
        elif value in files and value != mod:
            tree.add(value)
        else:
            engine.add(value)

    use_paths = [re.sub(r"\s+", "", full) for _p, full in census.raw_uses.get(mod, [])]
    # 1. Every expanded `use` path, symbol by symbol (the census only used these
    #    for its export chain, never as edges).
    for is_pub, full in census.raw_uses.get(mod, []):
        if CRATE_RE.match(full):
            crates.add(full.split("::")[0])
        else:
            classify(full)
    # 2. `use super::*;` -- the parent's whole surface.
    for m in SUPER_GLOB.finditer(body):
        up = m.group(1).count("super")
        parts = mod.split("::")
        parent = "::".join(parts[: max(1, len(parts) - up)])
        if parent in files:
            tree.add(parent)
        else:
            engine.add(parent)
    # 3. Inline qualified paths. `REF_RE` stops at `{`, so a braced import leaks
    #    its head (`crate::foo`) as if the whole module were a dependency; those
    #    prefixes are dropped because step 1 already expanded them per symbol.
    for ref in set(REF_RE.findall(body)):
        ref = re.sub(r"\s+", "", ref)
        if any(p == ref or p.startswith(ref + "::") for p in use_paths):
            continue
        classify(ref)
    return crates, tree, engine


info = {}
for mod in files:
    if mod in test_only:
        info[mod] = {"lines": 0, "crates": set(), "tree": set(), "engine": set(), "test_only": True}
        continue
    body = BODY[mod]
    crates, tree, engine = edges(mod)
    info[mod] = {
        "lines": len([l for l in body.splitlines() if l.strip()]),
        "crates": crates,
        "tree": tree,
        "engine": engine,
        "test_only": False,
    }

facade = {
    m
    for m in files
    if m not in test_only
    and not census.PUB_DEF.findall(BODY[m])
    and info[m]["crates"]
    and not info[m]["engine"]
    and not info[m]["tree"]
}
movable = set(facade) | {m for m in files if info[m]["test_only"]}
while True:
    nxt = {
        m
        for m in files
        if m not in movable and not info[m]["engine"] and info[m]["tree"] <= movable
    }
    if not nxt:
        break
    movable |= nxt

blocked = sorted(files - movable, key=lambda m: -info[m]["lines"])
candidates = sorted(
    (m for m in movable - facade if not info[m]["test_only"] and not is_island(m)),
    key=lambda m: -info[m]["lines"],
)

lines = []
lines.append(f"### {TARGET} (brace-import + variant-path corrected)")
lines.append(
    f"prod lines: {sum(i['lines'] for i in info.values())}  "
    f"shim: {len(facade)}  MOVABLE: {len(candidates)}"
    f" / {sum(info[m]['lines'] for m in candidates)}  "
    f"BLOCKED: {len(blocked)} / {sum(info[m]['lines'] for m in blocked)}"
)
lines.append("")
lines.append("### MOVABLE after correction (module-declaration files and super-glob files removed)")
for m in candidates:
    defs = census.PUB_DEF.findall(BODY[m])
    sup = len(SUPER_GLOB.findall(BODY[m]))
    note = ""
    if not defs:
        note = "  NOT-MOVABLE(no pub defs: module-declaration file)"
    if sup and not info[m]["tree"] and not info[m]["engine"]:
        note = "  NOT-MOVABLE(use super::*: parent edges invisible to census)"
    lines.append(
        f"  {info[m]['lines']:6}  {m:44} crates={','.join(sorted(info[m]['crates']))}{note}"
    )
lines.append("")
lines.append("### BLOCKED / ISLANDS: genuine non-chat engine deps (brace imports expanded)")
for m in blocked:
    if info[m]["test_only"]:
        continue
    tag = "ISLAND" if is_island(m) else "BLOCKED"
    engine = sorted(info[m]["engine"])
    tree = sorted(info[m]["tree"])
    lines.append(f"  {info[m]['lines']:6}  {m:44} {tag}")
    lines.append(f"          in-tree: {', '.join(tree) or '-'}")
    lines.append(f"          engine : {', '.join(engine) or '-'}")
lines.append("")
lines.append("### NON-CHAT BLOCKER MODULES ranked by blocked chat lines")
count = {}
for m in blocked:
    if info[m]["test_only"]:
        continue
    for e in info[m]["engine"]:
        top = e.split("::")[0]
        count.setdefault(top, [0, 0, set()])
        count[top][0] += info[m]["lines"]
        count[top][1] += 1
        count[top][2].add(m)
for top, (size, nfiles, who) in sorted(count.items(), key=lambda kv: -kv[1][0]):
    lines.append(f"  {size:6}  {top:28} blocks {nfiles:2} files: {', '.join(sorted(who)[:4])}")

dest = os.path.join(census.ENGINE, "target", "z1_engine_deps.txt")
with open(dest, "w", encoding="utf-8") as fh:
    fh.write("\n".join(lines) + "\n")
print("\n".join(lines))
