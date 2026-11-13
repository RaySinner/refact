"""Authoritative movability census for an engine module tree.

Usage:  python tools/dev/chat_movable_census.py [module]      (default: chat)

Written because `target/census_module.py` overstates chat movability by more than
an order of magnitude:

* it reads whole files, so `#[cfg(test)]` re-exports, test helpers and
  `tests_*.rs` bodies count as production edges (chat's `tests_*.rs` alone are
  ~10 000 lines);
* it counts already-shipped `pub use` shims as movable;
* it only resolves `crate::…` references, so `use super::{…}` / `use super::*` —
  the dominant intra-directory edge of `chat/trajectories`, `chat/queue` and
  `chat/perf_harness` — is invisible and those files look standalone.

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

Output: `refact-agent/engine/target/<module>_movable_census.txt`.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENGINE, "src")

TARGET = sys.argv[1] if len(sys.argv) > 1 else "chat"

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]")
TEST_MOD = re.compile(
    r"(?s)#\[cfg\((?:test|any\(test[^)]*)\)\]\s*(?:pub\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
PUB_DEF = re.compile(
    r"(?m)^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|type|const|static|union|mod)[ \t]+([A-Za-z_][A-Za-z0-9_]*)"
)
USE_STMT = re.compile(r"\b(pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([^;]+);", re.S)
CRATE_RE = re.compile(r"^refact_[a-z0-9_]+\b")
REF_RE = re.compile(
    r"\b((?:crate|super|self)\s*::\s*[A-Za-z_][A-Za-z0-9_]*(?:\s*::\s*[A-Za-z0-9_]+)*)"
)


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def prod(text):
    """Remove `#[cfg(test)]` items without discarding the production code around them.

    Cutting at the first `#[cfg(test)]` truncates a file whose test modules are
    declared near the top (`chat/queue/mod.rs` declares `#[cfg(test)] mod tests;`
    above the real imports) and then silently hides every production edge below.
    Here only the attributed item goes: a braced `mod x { … }` by brace matching,
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

exports, raw_uses, wild = {}, {}, {}
for mod, path in mods.items():
    text = read(path)
    if mod in test_only:
        exports[mod] = {}
        raw_uses[mod] = []
        continue
    body = code(prod(text))
    exports[mod] = {n: f"engine:{mod}" for n in set(PUB_DEF.findall(body))}
    uses = []
    for m in USE_STMT.finditer(body):
        out = []
        expand("", m.group(2), out)
        for full in out:
            uses.append((m.group(1) is not None, full))
    raw_uses[mod] = uses
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

files = {m for m in mods if m == TARGET or m.startswith(TARGET + "::")}


def symbol_origin(tmod, symbol):
    origin = exports.get(tmod, {}).get(symbol)
    if origin:
        kind, value = origin.split(":", 1)
        return (kind, value)
    if tmod in wild:
        return ("crate", ",".join(sorted(wild[tmod])))
    if symbol is None:
        return ("engine", tmod)
    return ("engine", f"{tmod}::{symbol}")


info = {}
for mod in files:
    if mod in test_only:
        info[mod] = {"lines": 0, "crates": set(), "engine": set(), "deps": set(), "test_only": True}
        continue
    body = code(prod(read(mods[mod])))
    crates, engine, deps = set(), set(), set()

    def add_edge(ref):
        target = absolute(mod, ref)
        tmod, rest = longest_module(target)
        if tmod is None or tmod == mod:
            return
        star = (not rest) or rest[-1] == "*"
        if star and tmod not in wild:
            if tmod in files:
                deps.add(tmod)
            else:
                engine.add(tmod)
            return
        kind, value = symbol_origin(tmod, None if star else rest[-1])
        if kind == "crate":
            crates.add(value)
            return
        if value in files and value != mod:
            deps.add(value)
        else:
            engine.add(value)

    # Direct crate imports (`use refact_core::…`) are crate edges.
    for _is_pub, full in raw_uses.get(mod, []):
        if CRATE_RE.match(full):
            crates.add(full.split("::")[0])
    # Every `crate::`/`super::`/`self::` path in the body, inline or inside a
    # `use`, is a real edge: a call written as `crate::chat::summarization::f(x)`
    # binds the file to that module just as firmly as an import does.
    for ref in set(REF_RE.findall(body)):
        add_edge(re.sub(r"\s+", "", ref))
    info[mod] = {
        "lines": len([l for l in body.splitlines() if l.strip()]),
        "crates": crates,
        "engine": engine,
        "deps": deps,
        "test_only": False,
    }

facade = {
    mod
    for mod in files
    if mod not in test_only
    and not PUB_DEF.findall(code(prod(read(mods[mod]))))
    and info[mod]["crates"]
    and not info[mod]["engine"]
    and not info[mod]["deps"]
}
movable = set(facade) | {m for m in files if info[m]["test_only"]}
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
    (m for m in movable - facade if not info[m]["test_only"] and not is_island(m)),
    key=lambda m: -info[m]["lines"],
)
test_mass = sorted(
    (m for m in movable if info[m]["test_only"] or m in facade), key=lambda m: -info[m]["lines"]
)
total = sum(i["lines"] for i in info.values())
lines = [
    f"### {TARGET}: {len(files)} files / {total} production lines",
    f"already-shim (pure crate facade): {len(facade)} files / "
    f"{sum(info[m]['lines'] for m in facade)} lines",
    f"MOVABLE now: {len(candidates)} files / {sum(info[m]['lines'] for m in candidates)} lines",
    f"LEFT (blocked): {len(blocked)} files / {sum(info[m]['lines'] for m in blocked)} lines",
    f"stays in engine (test-only modules + shims): {len(test_mass)} files / "
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

dest = os.path.join(ENGINE, "target", f"{TARGET.split('::')[-1]}_movable_census.txt")
with open(dest, "w", encoding="utf-8") as fh:
    fh.write("\n".join(lines) + "\n")
print("\n".join(lines[:5]))
