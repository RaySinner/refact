"""AL-3: source-derived crate/engine edge resolution for an engine module tree.

The census resolves an import by taking the longest *module prefix* that is a
real file and then looking only the final segment up in that module's export
table. Five shapes lose or invent their target:

* a nested module re-export -- `stats/mod.rs` has `pub use refact_stats::reader;`
  and callers write `crate::stats::reader::aggregate_summary`. The longest module
  is `stats`, `aggregate_summary` is not bound there, so the edge is reported as
  `engine:stats::aggregate_summary` although the path is pure crate.
* a plain `pub mod child;` -- `crate::chat::ctx::ChatCtx` walks `chat` -> `ctx`
  -> `ChatCtx`, and `ctx` re-exports `refact_runtime_api::chat_ctx::ChatCtx`.
  The census has no binding for a `mod` declaration, so it stops at `chat`.
* `crate::x::y` where `lib.rs` re-exports the crate root
  (`pub use refact_core::custom_error;`): the census never checks `lib.rs`.
* `use super::*` / `super::X`: the census has no model for relative paths, so a
  file whose imports arrive through its parent's prelude (the `providers/http/*`
  prelude, `buddy/jobs/autonomous_chats/*`, the `buddy/tests/*` helpers) looks
  dependency-free.
* a private `use crate::providers::traits::ProviderTrait;`: the export table is
  built from `pub use` only, so the edge is reported engine-resident although
  `providers::traits` is `pub use refact_providers::traits;`.

This script re-derives the edges from the source text: each module's `use`
statements and `mod` declarations form a `name -> path` table (private ones too,
because a child may import its ancestors' private items -- exactly what
`use super::*` does), a path is walked one segment at a time through the table of
the module being entered, `super::`/`self::` resolve against the entering
module's absolute path, `use super::*` is expanded for the names the body
mentions, and `crate::<root>` heads consult `lib.rs`.

Usage:  python refact-agent/engine/target/al3_edges.py <tree> [tree ...]
Read-only. No Rust compilation.
"""
import importlib.util
import os
import re
import sys

def _repo_root():
    """Walk up until the census script is found, so this file works from both
    `tools/dev/` and `refact-agent/engine/target/` (where the census output goes)."""
    here = os.path.dirname(os.path.abspath(__file__))
    while True:
        if os.path.isfile(
            os.path.join(here, "tools", "dev", "chat_movable_census.py")
        ):
            return here
        parent = os.path.dirname(here)
        if parent == here:
            raise RuntimeError("repo root not found")
        here = parent


ROOT = _repo_root()
CENSUS = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")

CRATE_RE = re.compile(r"^refact_[a-z0-9_]+$")
SUPER_STAR = re.compile(r"\buse\s+super\s*::\s*\*")
OWN_USE_RE = re.compile(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?use\b[^;]*;")
MOD_DECL_RE = re.compile(
    r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)

_READ = {}
_BIND = {}

GROUPS = (
    "MOVABLE now",
    "BLOCKED on another island (that island needs its own card first)",
    "LEFT (blocked on engine code)",
)


def load(tree):
    saved = list(sys.argv)
    sys.argv = [CENSUS, tree]
    spec = importlib.util.spec_from_file_location("cmc_%s" % tree, CENSUS)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    sys.argv = saved
    return module


def source(c, mod):
    if mod not in _READ:
        _READ[mod] = c.code(c.prod(c.read(c.mods[mod])))
    return _READ[mod]


def own_bindings(c, mod):
    """name -> path for one module: its `use` bindings plus its child `mod`s."""
    table = {}
    for _is_pub, full in c.raw_uses.get(mod, []):
        if full.endswith("::*"):
            continue
        table[full.split("::")[-1]] = full
    for child in MOD_DECL_RE.findall(source(c, mod)):
        candidate = mod + "::" + child
        if candidate in c.mods:
            table[child] = candidate
    return table


def module_bindings(c, mod):
    """`own_bindings` plus the `use super::*` inheritance chain."""
    if mod in _BIND:
        return _BIND[mod]
    table = dict(own_bindings(c, mod))
    if SUPER_STAR.search(source(c, mod)):
        body = OWN_USE_RE.sub("", source(c, mod))
        mentioned = set(re.findall(r"\b[A-Za-z_][A-Za-z0-9_]*\b", body))
        parent = mod.rsplit("::", 1)[0] if "::" in mod else None
        seen = set()
        while parent and parent in c.mods and parent not in seen:
            seen.add(parent)
            for name, full in own_bindings(c, parent).items():
                if name not in table and name in mentioned:
                    table[name] = full
            if not SUPER_STAR.search(source(c, parent)):
                break
            parent = parent.rsplit("::", 1)[0] if "::" in parent else None
    _BIND[mod] = table
    return table


def ascend(mod, levels):
    parts = mod.split("::") if mod else []
    for _ in range(levels):
        if parts:
            parts = parts[:-1]
    return "::".join(parts)


def root_binding(c, name):
    """Resolve a `crate::<name>` head through `lib.rs`'s own bindings."""
    target = own_bindings(c, "lib").get(name)
    if target is None:
        return None
    tseg = [s for s in target.split("::") if s]
    joined = "::".join(tseg)
    if tseg and CRATE_RE.match(tseg[0]):
        return ("crate", tseg[0])
    if joined in c.mods:
        return ("module", joined)
    if target in c.mods:
        return ("module", target)
    return ("external", target)


def resolve_path(c, path, here):
    """('crate', name) | ('engine', engine path) | ('external', path)."""
    seg = [s for s in path.split("::") if s]
    if not seg:
        return ("external", path)
    head = seg[0]
    if CRATE_RE.match(head):
        return ("crate", head)
    if head == "crate":
        if len(seg) < 2:
            return ("engine", path)
        if seg[1] in c.mods:
            cur, i = seg[1], 2
        else:
            rooted = root_binding(c, seg[1])
            if rooted is None:
                return ("external", "::".join(seg))
            if rooted[0] == "crate":
                return ("crate", rooted[1])
            if rooted[0] == "external":
                return ("external", rooted[1])
            cur, i = rooted[1], 2
    elif head in ("super", "self"):
        base = here
        j = 0
        while j < len(seg) and seg[j] in ("super", "self"):
            if seg[j] == "super":
                if base == "":
                    return ("external", path)
                base = ascend(base, 1)
            j += 1
        if j >= len(seg):
            return ("engine", base or "::".join(seg))
        cur, i = (seg[j], j + 1) if base == "" else (base, j)
    elif head in c.mods:
        cur, i = head, 1
    else:
        return ("external", "::".join(seg))
    while cur in c.mods:
        if i >= len(seg):
            return ("engine", cur)
        child = cur + "::" + seg[i]
        if child in c.mods and child not in _BIND.get(cur, {}).values():
            cur, i = child, i + 1
            continue
        target = module_bindings(c, cur).get(seg[i])
        if target is None:
            if cur in c.wild:
                return ("crate", sorted(c.wild[cur])[0])
            return ("engine", "::".join(seg))
        tseg = [s for s in target.split("::") if s]
        joined = "::".join(tseg)
        if CRATE_RE.match(tseg[0]):
            return ("crate", tseg[0])
        if tseg[0] == "crate" and len(tseg) > 1:
            cur = tseg[1]
        elif tseg[0] == "super":
            base = cur
            j = 0
            while j < len(tseg) and tseg[j] == "super":
                base = ascend(base, 1)
                j += 1
            if j >= len(tseg):
                return ("engine", target)
            cur = base if base else tseg[j]
        elif tseg[0] == "self":
            cur = tseg[1] if len(tseg) > 1 else cur
        elif joined in c.mods:
            cur = joined
        elif (cur + "::" + tseg[0]) in c.mods:
            # A bare `pub use runner::X;` inside `subchat` names `subchat::runner`.
            cur = cur + "::" + tseg[0]
        elif tseg[0] in c.mods:
            cur = tseg[0]
        else:
            return ("external", target)
        i += 1
    return ("engine", "::".join(seg))


def edges(c, mod):
    seed = module_bindings(c, mod)
    paths = set(seed.values())
    for ref in c.REF_RE.findall(source(c, mod)):
        paths.add(re.sub(r"\s+", "", ref))
    # A path that is a strict prefix of another path in the same file is the
    # module itself, not a reference to it (`crate::buddy::types` occurs only
    # inside `crate::buddy::types::BuddyFact`); the longer path carries the edge.
    paths = {
        p for p in paths if not any(o != p and o.startswith(p + "::") for o in paths)
    }
    crates, engine = set(), set()
    for path in paths:
        kind, value = resolve_path(c, path, mod)
        if kind == "crate":
            crates.add(value)
        elif kind == "engine" and value != mod:
            engine.add(value)
    return crates, engine


def closure(c, start):
    seen, stack = set(), [start]
    while stack:
        for dep in c.CRATE_DEPS.get(stack.pop(), ()):
            if dep not in seen:
                seen.add(dep)
                stack.append(dep)
    return seen


def cfg_test_modules(c):
    """Every module inside a `#[cfg(test)] mod X;`, transitively.

    The census marks only the direct child (`buddy::tests`) as test-only, so the
    files that child declares (`buddy::tests::chat_reactions_and_pulse`) are
    analysed as production mass even though they can never leave the engine.
    """
    testish = set(c.test_only)
    changed = True
    while changed:
        changed = False
        for mod in c.mods:
            if mod in testish:
                continue
            parts = mod.split("::")
            if any("::".join(parts[:k]) in testish for k in range(1, len(parts))):
                testish.add(mod)
                changed = True
    return testish


def main():
    for tree in sys.argv[1:]:
        _BIND.clear()
        _READ.clear()
        c = load(tree)
        testish = cfg_test_modules(c)
        print("=" * 78)
        print("### %s" % tree)
        for label in GROUPS:
            mods = sorted(c.classes.get(label, []), key=lambda m: -c.info[m]["lines"])
            if not mods:
                continue
            print("-- %s" % label)
            for mod in mods:
                if mod in testish:
                    continue
                crates, engine = edges(c, mod)
                print(
                    "  %6d  %-48s crates={%s}"
                    % (c.info[mod]["lines"], mod, ",".join(sorted(crates)))
                )
                for one in sorted(engine):
                    print("           engine: %s" % one)


if __name__ == "__main__":
    main()
