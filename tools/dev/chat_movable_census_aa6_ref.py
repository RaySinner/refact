"""Authoritative movability census for an engine module tree.

Usage:  python tools/dev/chat_movable_census.py [module]      (default: chat)

Written because `target/census_module.py` overstates chat movability by more than
an order of magnitude:

* it reads whole files, so `#[cfg(test)]` re-exports, test helpers and
  `tests_*.rs` bodies count as production edges (chat's `tests_*.rs` alone are
  ~10 000 lines);
* it counts already-shipped `pub use` shims as movable;
* it only resolves `crate::тАж` references, so `use super::{тАж}` / `use super::*` тАФ
  the dominant intra-directory edge of `chat/trajectories`, `chat/queue` and
  `chat/perf_harness` тАФ is invisible and those files look standalone.

This script:

* strips every `#[cfg(test)]` region before reading `use`/`pub use`/definitions,
  and treats `#[cfg(test)] mod foo;` children as test-only (they stay in the
  engine, so they never count as production mass);
* resolves every `use` statement to an absolute engine module, then each imported
  symbol through the engine `pub use` chain, accepting a `refact_*` origin only
  when the chain really lands on a crate (including glob re-exports such as
  `pub use refact_core::x::*`);
* computes the movability closure, so a module is movable only when every
  production edge lands on a crate or on another movable module;
* prints, per blocked file, the first engine-resident dependency.

Four artifact classes it used to miss (AA-6, HEAD 5e457e310):

1. **bare sibling paths** тАФ `REF_RE` matched only `crate::|super::|self::`, so
   `integrations/mod.rs` calling bare `mcp::integr_mcp_sse::IntegrationMCPSse`,
   `notifier::тАж::INTEGRATION_ID`, `integr_cmdline::ToolCmdline` produced *zero*
   edges and the file ranked as movable. Bare multi-segment paths (`a::b`) are
   now parsed too.
2. **private globs** тАФ `use super::*;` (the first line of every
   `providers/http/*.rs`) binds the file to the parent module's *private* import
   list, but produced no edge. A glob now contributes an edge to the globbed
   module and re-attributes every glob-supplied identifier the file actually
   uses (so an alias of a crate type stays a crate edge instead of being
   mistaken for an engine symbol, and a sibling `pub(crate) fn` becomes an
   engine edge). Names the file defines itself are never re-attributed.
3. **crate aliases through `pub use refact_x::y;`** тАФ `crate::providers::
   claude_code::ClaudeCodeProvider` stopped at the longest *file* module
   (`providers`) and then mislabelled the tail as an engine symbol, inventing
   blockers such as `providers::ProviderTrait`. Tails are now walked through the
   module's `pub use` map, and a tail that is neither an alias nor a submodule
   resolves to its owning module instead of an invented symbol path.
4. **declaration-only files** тАФ `PUB_DEF` matched `pub mod x;`, so a file that
   only declares submodules (`integrations/mcp/mod.rs`) ranked as movable payload
   and a pure `pub use` facade with decls (`providers/mod.rs`) escaped the
   facade class. `mod` is no longer a definition; a file whose production body is
   nothing but attributes, `use` lines and `mod x;` declarations carries no
   payload at all and is reported as a shim, exactly like a `pub use` facade.

Output: `refact-agent/engine/target/<module>_movable_census.txt`.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
# Overridable so `tools/dev/aa6-check.py` can run the same analysis over a
# synthetic fixture tree instead of only over the real engine sources.
SRC = os.environ.get("CENSUS_SRC_OVERRIDE") or os.path.join(ENGINE, "src")
OUT_DIR = os.environ.get("CENSUS_OUT_DIR") or os.path.join(ENGINE, "target")

TARGET = sys.argv[1] if len(sys.argv) > 1 else "chat"

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]")
TEST_MOD = re.compile(
    r"(?s)#\[cfg\((?:test|any\(test[^)]*)\)\]\s*(?:pub\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
PUB_DEF = re.compile(
    r"(?m)^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|type|const|static|union)[ \t]+([A-Za-z_][A-Za-z0-9_]*)"
)
USE_STMT = re.compile(r"\b(pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([^;]+);", re.S)
CRATE_RE = re.compile(r"^refact_[a-z0-9_]+\b")
# Any path: an optional `crate`/`super`/`self` head, then at least one `::seg`.
# The lookbehind stops a match from starting mid-path; the trailing optional `::`
# lets `a::b::{X, Y}` be read as the `a::b` path; the lookahead lets the match
# stop at a brace group but not inside a longer path.
REF_RE = re.compile(
    r"(?<![\w:])(?:(?:crate|super|self)\s*::\s*)?[A-Za-z_][A-Za-z0-9_]*"
    r"(?:\s*::\s*[A-Za-z_][A-Za-z0-9_]*)+(?:\s*::)?(?![\w:])"
)
DECL_RE = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;$")
ATTR_RE = re.compile(r"^#!?\[.*\]$")
IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
# Whole `use` statements, blanked before the inline-path scan: every `use` is
# already expanded item-by-item by USE_STMT/expand (brace groups included), and
# scanning it again reads a truncated brace head such as `crate::providers::{a, b}`
# as the path `crate::providers::` тАФ which is how `providers/mod.rs` used to look
# like a blocker of its own children.
ANY_USE = re.compile(r"\bpub(?:\([^)]*\))?\s+use\s[^;]*;|\buse\s[^;]*;", re.S)
# Names the file introduces itself; a glob import must never override them.
LOCAL_DEF = re.compile(
    r"\b(?:fn|struct|enum|trait|type|const|static|union|let|for)\s+([A-Za-z_][A-Za-z0-9_]*)"
)


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def prod(text):
    """Remove `#[cfg(test)]` items without discarding the production code around them.

    Cutting at the first `#[cfg(test)]` truncates a file whose test modules are
    declared near the top (`chat/queue/mod.rs` declares `#[cfg(test)] mod tests;`
    above the real imports) and then silently hides every production edge below.
    Here only the attributed item goes: a braced `mod x { тАж }` by brace matching,
    a `mod x;` declaration, or any other single item up to its `;`.
    """
    while True:
        m = CFG_TEST.search(text)
        if m is None:
            return text
        rest = text[m.end() :].lstrip()
        if rest.startswith("mod ") or rest.startswith("pub mod "):
            semi = text.find(";", m.end())
            open_brace = text.find("{", m.end())
            if semi != -1 and (open_brace == -1 or semi < open_brace):
                text = text[: m.start()] + text[semi + 1 :]
                continue
            if open_brace == -1:
                return text[: m.start()]
            depth, i = 0, open_brace
            while i < len(text):
                if text[i] == "{":
                    depth += 1
                elif text[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            text = text[: m.start()] + text[i + 1 :]
            continue
        semi = text.find(";", m.end())
        if semi == -1:
            return text[: m.start()]
        text = text[: m.start()] + text[semi + 1 :]


def code(text):
    out = []
    for line in text.splitlines():
        i = line.find("//")
        if i != -1 and line[:i].count('"') % 2 == 0:
            line = line[:i]
        out.append(line)
    return "\n".join(out)


def decl_only_body(body):
    """True when the production body is only attributes, `use` lines and `mod x;`."""
    rest = "\n".join(
        "" if DECL_RE.match(line.strip()) or ATTR_RE.match(line.strip()) else line
        for line in body.splitlines()
    )
    rest = ANY_USE.sub(" ", rest)
    return not rest.strip()


def inline_refs(body):
    """Paths written in code, with every `use` statement blanked first.

    `use` statements are expanded item-by-item by `USE_STMT`/`expand` (brace
    groups included), so scanning them again reads a truncated brace head such as
    `crate::providers::{a, b}` as the path `crate::providers::`. That is how
    `providers/mod.rs`, a pure `pub use` facade, looked like a blocker of every
    one of its own children.
    """
    return set(REF_RE.findall(ANY_USE.sub(" ", body)))


def split_top(body):
    parts, depth, cur = [], 0, []
    for ch in body:
        if ch in "{<([":
            depth += 1
        elif ch in "}>)]":
            depth -= 1
        if ch == "," and depth == 0:
            parts.append("".join(cur).strip())
            cur = []
            continue
        cur.append(ch)
    tail = "".join(cur).strip()
    if tail:
        parts.append(tail)
    return [p for p in parts if p]


def expand(prefix, body, out):
    body = re.sub(r"\s+", " ", body).strip()
    if body.startswith("{"):
        for piece in split_top(body.strip("{} ")):
            expand(prefix, piece, out)
        return
    m = re.match(r"^(.*?)\s*::\s*\{(.*)\}$", body)
    if m:
        head = m.group(1).strip()
        np = (prefix + "::" + head) if prefix else head
        for piece in split_top(m.group(2)):
            expand(np, piece, out)
        return
    out.append(re.sub(r"\s+", "", (prefix + "::" + body) if prefix else body))


mods, test_only = {}, set()
for root, dirs, files in os.walk(SRC):
    dirs[:] = [d for d in dirs if d != "target"]
    for name in files:
        if not name.endswith(".rs"):
            continue
        full = os.path.join(root, name)
        rel = os.path.relpath(full, SRC)[:-3].replace(os.sep, "::")
        if rel.endswith("::mod"):
            rel = rel[: -len("::mod")]
        mods[rel] = full
for mod, path in mods.items():
    for m in TEST_MOD.finditer(read(path)):
        test_only.add(mod + "::" + m.group(1))

exports, raw_uses, priv_uses, glob_targets, wild = {}, {}, {}, {}, {}
for mod, path in mods.items():
    if mod in test_only:
        exports[mod] = {}
        raw_uses[mod] = []
        priv_uses[mod] = {}
        glob_targets[mod] = []
        continue
    body = code(prod(read(path)))
    exports[mod] = {n: f"engine:{mod}" for n in set(PUB_DEF.findall(body))}
    uses = []
    priv, globs = {}, []
    for m in USE_STMT.finditer(body):
        out = []
        expand("", m.group(2), out)
        for full in out:
            uses.append((m.group(1) is not None, full))
            if full.endswith("::*"):
                globs.append(full[: -len("::*")])
            else:
                priv[full.split("::")[-1]] = full
    raw_uses[mod] = uses
    priv_uses[mod] = priv
    glob_targets[mod] = globs
    for is_pub, full in uses:
        if is_pub and CRATE_RE.match(full):
            parts = full.split("::")
            if len(parts) >= 2 and parts[-1] == "*":
                wild.setdefault(mod, set()).add(parts[0])

ROOT_CRATE_EXPORTS = {}
for m in re.finditer(
    r"pub use (refact_[a-z0-9_]+)::([A-Za-z_][A-Za-z0-9_]*)\s*;", read(os.path.join(SRC, "lib.rs"))
):
    ROOT_CRATE_EXPORTS[m.group(2)] = m.group(1)


def absolute(cur_mod, ref):
    """Resolve `crate::`/`super::`/`self::` (and edition-2018 uniform paths) to an
    absolute engine module path."""
    parts = [p.strip() for p in ref.split("::") if p.strip()]
    head = parts[0]
    if head == "crate":
        return "::".join(parts[1:])
    if head == "self":
        return cur_mod + ("::" + "::".join(parts[1:]) if len(parts) > 1 else "")
    if head == "super":
        base = cur_mod.split("::")
        rest = parts
        while rest and rest[0] == "super" and base:
            base = base[:-1]
            rest = rest[1:]
        return "::".join(base + rest)
    if CRATE_RE.match(head):
        return ref
    if (cur_mod + "::" + head) in mods:
        return cur_mod + "::" + ref
    if head in mods:
        return ref
    return ref


def longest_module(path):
    seg = path.split("::")
    for k in range(len(seg), 0, -1):
        cand = "::".join(seg[:k])
        if cand in mods:
            return cand, seg[k:]
    return None, seg


for _ in range(80):
    changed = False
    for mod, uses in raw_uses.items():
        for is_pub, full in uses:
            if not is_pub:
                continue
            if CRATE_RE.match(full):
                parts = full.split("::")[1:]
                if parts and parts[-1] != "*":
                    if exports[mod].setdefault(parts[-1], f"crate:{full.split('::')[0]}") is None:
                        changed = True
                continue
            tmod, rest = longest_module(absolute(mod, full))
            if tmod is None or tmod == mod or not rest:
                continue
            if rest[-1] == "*":
                for name, origin in exports.get(tmod, {}).items():
                    if name not in exports[mod]:
                        exports[mod][name] = origin
                        changed = True
                continue
            origin = exports.get(tmod, {}).get(rest[-1])
            if origin and rest[-1] not in exports[mod]:
                exports[mod][rest[-1]] = origin
                changed = True
    for mod, uses in raw_uses.items():
        for is_pub, full in uses:
            if not is_pub or CRATE_RE.match(full):
                continue
            tmod, rest = longest_module(absolute(mod, full))
            if tmod is None or tmod == mod or not rest or rest[-1] != "*":
                continue
            if tmod in wild:
                before = len(wild.get(mod, set()))
                wild.setdefault(mod, set()).update(wild[tmod])
                if len(wild[mod]) != before:
                    changed = True
    if not changed:
        break


def resolve_tail(tmod, segs):
    """Resolve `segs` inside module `tmod` through the module's `pub use` aliases.

    A single-segment tail is the classic symbol lookup. A multi-segment tail may
    name a submodule that the module re-exports from a crate тАФ the case that used
    to invent blockers such as `providers::ProviderTrait` for the real
    `refact_providers::traits::ProviderTrait`.
    """
    cur = tmod
    for i, seg in enumerate(segs):
        origin = exports.get(cur, {}).get(seg)
        if origin:
            kind, value = origin.split(":", 1)
            if kind == "crate":
                return ("crate", value)
            if i == len(segs) - 1:
                return ("engine", value)
            return resolve_tail(value, segs[i + 1 :])
        inner = cur + "::" + seg
        if inner in mods:
            cur = inner
            if i == len(segs) - 1:
                return ("engine", inner)
            continue
        if cur in wild:
            return ("crate", ",".join(sorted(wild[cur])))
        # `seg` is neither a re-export nor a submodule: an `impl` member, a trait
        # item, a macro-generated name. The census only needs the owning module,
        # so stop there instead of inventing `integrations::mcp::тАж::default`.
        return ("engine", cur)
    return ("engine", cur)


import_names, glob_names = {}, {}
for mod in mods:
    if mod in test_only:
        import_names[mod], glob_names[mod] = {}, {}
        continue
    names = {}
    for name, full in priv_uses.get(mod, {}).items():
        if CRATE_RE.match(full):
            names[name] = ("crate", full.split("::")[0])
            continue
        tmod, rest = longest_module(absolute(mod, full))
        if tmod is None or tmod == mod or not rest:
            continue
        names[name] = resolve_tail(tmod, rest)
    import_names[mod] = names
    glob_names[mod] = {}

for _ in range(80):
    changed = False
    for mod in mods:
        if mod in test_only:
            continue
        merged = dict(glob_names[mod])
        for target in glob_targets.get(mod, []):
            tmod, rest = longest_module(absolute(mod, target))
            if tmod is None or (rest and rest != ["*"]):
                continue
            for name, origin in exports.get(tmod, {}).items():
                merged.setdefault(name, tuple(origin.split(":", 1)))
            for name, origin in import_names.get(tmod, {}).items():
                merged.setdefault(name, origin)
            for name, origin in glob_names.get(tmod, {}).items():
                merged.setdefault(name, origin)
        if merged != glob_names[mod]:
            glob_names[mod] = merged
            changed = True
    if not changed:
        break

files = {m for m in mods if m == TARGET or m.startswith(TARGET + "::")}


info = {}
for mod in files:
    if mod in test_only:
        info[mod] = {"lines": 0, "crates": set(), "engine": set(), "deps": set(), "test_only": True}
        continue
    body = code(prod(read(mods[mod])))
    crates, engine, deps = set(), set(), set()
    local = set(LOCAL_DEF.findall(body))

    def add_edge(ref):
        target = absolute(mod, ref)
        tmod, rest = longest_module(target)
        if tmod is None or tmod == mod:
            return
        star = (not rest) or rest[-1] == "*"
        if star:
            # A bare module path or a glob binds the file to the module: the file
            # still needs that module to exist beside it.
            if tmod in files:
                deps.add(tmod)
            else:
                engine.add(tmod)
            if tmod in wild:
                crates.update(wild[tmod])
            return
        kind, value = resolve_tail(tmod, rest)
        if kind == "crate":
            crates.add(value)
            return
        if value == mod:
            return
        if value in files:
            deps.add(value)
        else:
            engine.add(value)

    # Direct crate imports (`use refact_core::тАж`) are crate edges.
    for _is_pub, full in raw_uses.get(mod, []):
        if CRATE_RE.match(full):
            crates.add(full.split("::")[0])
    # Non-crate `use` paths (engine or sibling) are edges too; globs are handled
    # by the `glob_targets` pass below, which also attributes the globbed names.
    for _is_pub, full in raw_uses.get(mod, []):
        if CRATE_RE.match(full) or full.endswith("::*"):
            continue
        add_edge(full)
    # Every path written in code binds the file to its module just as firmly as
    # an import does: `crate::chat::summarization::f(x)` in a body, and bare
    # sibling paths such as `mcp::integr_mcp_sse::IntegrationMCPSse`. `use`
    # statements are already handled above (and recorded as decls/globs), so
    # they are blanked out of this scan.
    for ref in inline_refs(body):
        add_edge(re.sub(r"\s+", "", ref))
    # A glob import (`use super::*;`) is an edge to the globbed module *and* a
    # name-level edge for every glob-supplied identifier the file really uses:
    # an alias of a crate type stays a crate edge, a sibling `pub(crate) fn`
    # becomes an engine edge. Names the file defines itself are never rewritten.
    for target in glob_targets.get(mod, []):
        add_edge(re.sub(r"\s+", "", target) + "::*")
    for name in set(IDENT_RE.findall(body)) - local:
        origin = glob_names.get(mod, {}).get(name)
        if not origin:
            continue
        kind, value = origin
        if kind == "crate":
            crates.add(value)
        elif value == mod:
            continue
        elif value in files:
            deps.add(value)
        else:
            engine.add(value)
    info[mod] = {
        "lines": len([l for l in body.splitlines() if l.strip()]),
        "crates": crates,
        "engine": engine,
        "deps": deps,
        "test_only": False,
        "decl_only": decl_only_body(body),
    }

decl_only = {mod for mod in files if not info[mod]["test_only"] and info[mod]["decl_only"]}
# A payload-free file is a shim: every production line is an attribute, a `use`
# or a `mod x;` declaration, so there is nothing to move. That covers both the
# classic `pub use refact_core::тАж` facade and the bare `mod`-declaration file,
# which used to be reported as movable payload (artifacts 1 and 4).
shims = decl_only | {m for m in files if info[m]["test_only"]}
movable = set(shims)
while True:
    nxt = {
        mod
        for mod in files
        if mod not in movable
        and not info[mod]["engine"]
        and info[mod]["deps"] <= movable
    }
    if not nxt:
        break
    movable |= nxt

# Documented islands: each needs its own card, so they are never reported as
# harvest candidates even when their current edges happen to be crate-only.
ISLANDS = (
    "chat::stream_core",
    "chat::tools",
    "chat::prompts",
    "chat::ctx",
    "chat::cache_guard",
    "chat::summarization",
    "chat::perf_harness",
    "chat::perf_diagnostics",
    "chat::verifier",
    "chat::goal_verifier",
    "chat::notifications",
    "chat::cloud_token_count",
)


def is_island(mod):
    return any(mod == island or mod.startswith(island + "::") for island in ISLANDS)


blocked = sorted(files - movable, key=lambda m: -info[m]["lines"])
islands = [m for m in files if is_island(m)]
candidates = sorted(
    (m for m in movable - shims if not info[m]["test_only"] and not is_island(m)),
    key=lambda m: -info[m]["lines"],
)
test_mass = sorted(
    (m for m in movable if m in shims),
    key=lambda m: -info[m]["lines"],
)
total = sum(i["lines"] for i in info.values())
lines = [
    f"### {TARGET}: {len(files)} files / {total} production lines",
    f"already-shim (facade / declaration-only / test-only): {len(shims)} files / "
    f"{sum(info[m]['lines'] for m in shims)} lines",
    f"MOVABLE now: {len(candidates)} files / {sum(info[m]['lines'] for m in candidates)} lines",
    f"LEFT (blocked): {len(blocked)} files / {sum(info[m]['lines'] for m in blocked)} lines",
    f"stays in engine (test-only modules + shims/decls): {len(test_mass)} files / "
    f"{sum(info[m]['lines'] for m in test_mass)} lines",
    "",
    "### MOVABLE (non-shim, non-test)",
]
for mod in candidates:
    lines.append(
        f"  {info[mod]['lines']:6}  {mod:56} crates={','.join(sorted(info[mod]['crates']))}"
    )
lines.append("")
lines.append("### DOCUMENTED ISLANDS (own card each, never harvested here)")
for mod in sorted(islands, key=lambda m: -info[m]["lines"]):
    engine = sorted(info[mod]["engine"] | info[mod]["deps"])
    lines.append(f"  {info[mod]['lines']:6}  {mod:56} blocker={engine[0] if engine else '-'}")
lines.append("")
lines.append("### LEFT / ISLANDS")
for mod in blocked:
    if is_island(mod):
        continue
    engine = sorted(info[mod]["engine"] | info[mod]["deps"])
    lines.append(f"  {info[mod]['lines']:6}  {mod:56} blocker={engine[0] if engine else '-'}")
    if len(engine) > 1:
        lines.append(f"          others: {', '.join(engine[1:6])}")

dest = os.path.join(OUT_DIR, f"{TARGET.split('::')[-1]}_movable_census.txt")
with open(dest, "w", encoding="utf-8") as fh:
    fh.write("\n".join(lines) + "\n")
print("\n".join(lines[:6]))
