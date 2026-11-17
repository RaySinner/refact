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
* turns every `use` statement into *symbol* edges, so `use crate::m::{A, B}`
  binds `A` and `B` instead of the module `crate::m`. Reading the brace group
  as a whole-module reference used to hide an engine-resident re-export behind a
  facade that also globs a crate (a facade's `pub use refact_x::*` plus one
  engine `pub use child::*` made every import through it look crate-only);
* attributes **glob imports by name**. `use super::*` used to contribute no edge
  at all, so a file that takes an engine-resident helper (`build_spec`,
  `execute_autonomous_spec`) from its parent's `pub(super) use child::*` looked
  edge-free and was reported MOVABLE — the exact false positive that made
  `buddy::jobs::autonomous_chats::{local_jobs, memory_jobs}` look harvestable.
  Every identifier a file actually uses is now resolved through the globbing
  module's *visible item set* (its own items, then its explicit imports, then the
  engine modules it globs, and only last the crates it globs), and the resolved
  origin becomes the edge. That is the honest rule for wildcards: **a wildcard
  from crate `C` covers a name only when no engine-resident definition of that
  name exists on the path the reference travels**;
* keeps a glob of a module that lives *outside* this tree as a hard engine edge:
  `use crate::chat::prompts::*` can never be satisfied from the destination
  crate. A glob of an in-tree module (`use super::*`, `use super::helper::*`)
  adds no edge of its own — the names the file actually uses are attributed to
  their real owners, which is stricter than treating the globbed module as one
  opaque dependency;
* computes the movability closure, so a module is movable only when every
  production edge lands on a crate or on another movable module;
* prints, per blocked file, the first engine-resident dependency.

`MOVABLE now` additionally excludes, and reports separately, every file that
cannot move *now* even though its own edges look clean:

* documented islands (`ISLANDS`, `ISLANDS_BY_TREE`) and everything that depends
  on them — an island needs its own card before its dependants can follow;
* module tree roots (`mod.rs` files that only declare children) — they are the
  destination, not a file to move into it;
* files whose crate edge points at a crate that already depends on the
  destination crate (`DEST_HINTS`) — moving those is a cargo dependency cycle.

Every file lands in exactly one class and the class line counts are printed
against the tree total, so a miscount is visible instead of silent.

Output: `refact-agent/engine/target/<module>_movable_census.txt`.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.environ.get("CENSUS_SRC_OVERRIDE") or os.path.join(ENGINE, "src")
OUT_DIR = os.environ.get("CENSUS_OUT_DIR") or os.path.join(ENGINE, "target")

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
# Module-level items only: a `use super::*` supplies what sits in the parent's
# namespace, not the methods nested inside its `impl` blocks, so indented
# declarations must not enter the visible set.
ANY_DEF = re.compile(
    r"(?m)^(?:pub(?:\([^)]*\))?[ \t]+)?(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|type|const|static|union|mod)[ \t]+([A-Za-z_][A-Za-z0-9_]*)"
)
MACRO_DEF = re.compile(r"(?m)^macro_rules![ \t]+([A-Za-z_][A-Za-z0-9_]*)")
CHILD_MOD = re.compile(
    r"(?m)^(?:pub(?:\([^)]*\))?[ \t]+)?mod[ \t]+([A-Za-z_][A-Za-z0-9_]*)"
)
FULL_DEF = re.compile(
    r"(?m)^(?:pub(?:\([^)]*\))?[ \t]+)?(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|type|const|static|union|impl|macro_rules!)\b"
)
ANY_MOD_DECL = re.compile(r"(?m)^(?:pub(?:\([^)]*\))?[ \t]+)?mod\s+[A-Za-z_]")
IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
# `fn f(a: T)` / `|a: T|` / `Foo { a: b }`: the name is introduced by the file
# itself, so a glob must not claim it (`quota_cache` is a parameter of
# `fetch_provider_quota`, not the sibling module of the same name).
BIND_NAME = re.compile(r"(?:(?<=[,(])|(?<=\|))[ \t\n]*([A-Za-z_][A-Za-z0-9_]*)[ \t\n]*:")
PATH_KEYWORDS = ("self", "super", "crate")


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


CHAR_LITERAL = re.compile(r"'(?:\\.|[^\\'])'")


def skip_string(text, quote):
    """`quote` indexes the opening `"`; return the index of the closing one."""
    i = quote + 1
    while i < len(text):
        if text[i] == "\\":
            i += 2
            continue
        if text[i] == '"':
            return i
        i += 1
    return -1


def skip_raw_string(text, r):
    """`r` indexes an `r`; return the last index of the raw literal it opens, or
    `r` itself when this `r` is not a raw string."""
    j = r + 1
    hashes = 0
    while j < len(text) and text[j] == "#":
        hashes += 1
        j += 1
    if j >= len(text) or text[j] != '"':
        return r
    close = '"' + "#" * hashes
    end = text.find(close, j + 1)
    return len(text) - 1 if end == -1 else end + len(close) - 1


def match_brace(text, open_index):
    """Index of the `}` matching the `{` at `open_index`, or -1.

    Braces inside string/char literals, raw strings and comments must not count:
    a test module containing a `'{'`/`"{"` literal otherwise unbalances a naive
    counter and the whole production tail of the file is silently deleted.
    """
    depth = 0
    i = open_index
    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""
        if ch == "/" and nxt == "/":
            nl = text.find("\n", i)
            i = len(text) if nl == -1 else nl
            continue
        if ch == "/" and nxt == "*":
            end = text.find("*/", i + 2)
            i = len(text) if end == -1 else end + 2
            continue
        if ch == "r" and nxt in '#"':
            nxt_i = skip_raw_string(text, i)
            if nxt_i != i:
                i = nxt_i + 1
                continue
        if ch == "b" and nxt == '"':
            end = skip_string(text, i + 1)
            if end == -1:
                return -1
            i = end + 1
            continue
        if ch == '"':
            end = skip_string(text, i)
            if end == -1:
                return -1
            i = end + 1
            continue
        if ch == "'":
            literal = CHAR_LITERAL.match(text, i)
            i = literal.end() if literal else i + 1
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return -1


def _scan(text, start, targets):
    """First index at or after `start` holding a character of `targets` that sits
    outside literals, comments and `()`/`[]` nesting."""
    depth = 0
    i = start
    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""
        if ch == "/" and nxt == "/":
            nl = text.find("\n", i)
            i = len(text) if nl == -1 else nl
            continue
        if ch == "/" and nxt == "*":
            end = text.find("*/", i + 2)
            i = len(text) if end == -1 else end + 2
            continue
        if ch == "r" and nxt in '#"':
            end = skip_raw_string(text, i)
            if end != i:
                i = end + 1
                continue
        if ch == "b" and nxt == '"':
            end = skip_string(text, i + 1)
            if end == -1:
                return -1
            i = end + 1
            continue
        if ch == '"':
            end = skip_string(text, i)
            if end == -1:
                return -1
            i = end + 1
            continue
        if ch == "'":
            literal = CHAR_LITERAL.match(text, i)
            i = literal.end() if literal else i + 1
            continue
        if ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        elif depth == 0 and ch in targets:
            return i
        i += 1
    return -1


def item_end(text, start):
    """Exclusive end of the item whose attributes end at `start`, or -1.

    A `use` declaration terminates at its `;` (its `{…}` group is not a body);
    every other item terminates at its `;` unless a `{` body opens first, in
    which case the matching `}` decides. Getting this wrong is how a
    `#[cfg(test)] async fn` with no `;` in sight used to swallow the rest of the
    file — `fn`/`impl`/`struct` bodies are brace-delimited exactly like `mod`.
    """
    i = start
    while True:
        m = re.match(r"\s*#!?\[", text[i:])
        if not m:
            break
        depth, j, close = 0, i + m.end() - 1, -1
        while j < len(text):
            if text[j] == "[":
                depth += 1
            elif text[j] == "]":
                depth -= 1
                if depth == 0:
                    close = j
                    break
            j += 1
        if close == -1:
            return -1
        i = close + 1
    if re.match(r"\s*(?:pub(?:\([^)]*\))?\s+)?use\b", text[i : i + 300]):
        semi = _scan(text, i, ";")
        return semi + 1 if semi != -1 else -1
    brace = _scan(text, i, "{")
    semi = _scan(text, i, ";")
    if brace != -1 and (semi == -1 or brace < semi):
        close = match_brace(text, brace)
        return close + 1 if close != -1 else -1
    return semi + 1 if semi != -1 else -1


def prod(text):
    """Remove `#[cfg(test)]` items without discarding the production code around them.

    Cutting at the first `#[cfg(test)]` truncates a file whose test modules are
    declared near the top (`chat/queue/mod.rs` declares `#[cfg(test)] mod tests;`
    above the real imports) and then silently hides every production edge below.
    Here only the attributed item goes; see `item_end` for how its extent is found.
    """
    while True:
        m = CFG_TEST.search(text)
        if m is None:
            return text
        end = item_end(text, m.end())
        if end == -1:
            return text[: m.start()]
        text = text[: m.start()] + text[end:]


def code(text):
    out = []
    for line in text.splitlines():
        i = line.find("//")
        if i != -1 and line[:i].count('"') % 2 == 0:
            line = line[:i]
        out.append(line)
    return "\n".join(out)


def strip_literals(text):
    """Blank string/char/raw literals, keeping length, so identifier scanning
    sees code only. A string such as `"prompts"` must not look like a use of a
    module item."""
    out, i = [], 0
    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""
        if ch == "r" and nxt in '#"':
            end = skip_raw_string(text, i)
            if end != i:
                out.append(" " * (end - i + 1))
                i = end + 1
                continue
        if ch == "b" and nxt == '"':
            end = skip_string(text, i + 1)
            if end != -1:
                out.append(" " * (end - i + 1))
                i = end + 1
                continue
        if ch == '"':
            end = skip_string(text, i)
            if end != -1:
                out.append(" " * (end - i + 1))
                i = end + 1
                continue
        if ch == "'":
            literal = CHAR_LITERAL.match(text, i)
            if literal:
                out.append(" " * (literal.end() - i))
                i = literal.end()
                continue
        out.append(ch)
        i += 1
    return "".join(out)


def glob_module(cur_mod, ref):
    """The module a `use …::*` imports names from, or None.

    `longest_module` leaves the trailing `*` in its tail (`chat::queue::*` →
    `("chat::queue", ["*"])`), so a plain `if rest` test would reject every glob
    import. Only stars — no real path segment — may remain.
    """
    tmod, rest = longest_module(absolute(cur_mod, ref))
    if tmod is None or [p for p in rest if p != "*"]:
        return None
    return tmod


def import_tail(full):
    """The name a `use` statement binds (`use a::b::C as D` binds `D`)."""
    return re.split(r"\s+as\s+", full)[-1].strip().split("::")[-1]

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
definitions = {}
for mod, path in mods.items():
    text = read(path)
    if mod in test_only:
        exports[mod] = {}
        raw_uses[mod] = []
        definitions[mod] = set()
        continue
    body = code(prod(text))
    exports[mod] = {n: f"engine:{mod}" for n in set(PUB_DEF.findall(body))}
    definitions[mod] = set(ANY_DEF.findall(body)) | set(MACRO_DEF.findall(body))
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


# ---------------------------------------------------------------------------
# Glob imports carry *names*, and they used to carry none.
#
# `use super::*` contributes no `use` edge here (the star is filtered out of
# `engines_uses`, and `REF_RE` cannot match `super::*`), so a file that took an
# engine-resident helper from its parent's `pub(super) use child::*` looked
# edge-free and was reported MOVABLE — the false positive that made
# `buddy::jobs::autonomous_chats::{local_jobs, memory_jobs}` look harvestable.
# The parent facade globs crate shims (`local_evidence`, `spec`, `redaction`),
# which is why the `wild` set alone used to excuse *every* name reached through
# it.
#
# Each glob import is now resolved by name through the globbing module's
# *visible item set*: its own declarations, then its explicit imports, then the
# in-tree modules it globs, and only last the crates it globs. That is the
# honest wildcard rule — a crate glob covers a name only when no
# engine-resident definition of that name exists on the path the reference
# travels.
# ---------------------------------------------------------------------------
LOCAL_BIND = re.compile(
    r"\b(?:fn|struct|enum|trait|type|const|static|union|let|for|macro_rules!)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)"
)

child_mods = {}
visible = {}
for mod in mods:
    child_mods[mod] = set(CHILD_MOD.findall(read(mods[mod]))) if mod not in test_only else set()
    names = definitions.get(mod, set())
    visible[mod] = {
        n: (f"engine:{mod}::{n}" if n in child_mods[mod] else f"engine:{mod}") for n in names
    }

engine_globs = {}
for mod, uses in raw_uses.items():
    eg = []
    for _is_pub, full in uses:
        if not full.endswith("::*"):
            continue
        if CRATE_RE.match(full):
            continue
        tmod = glob_module(mod, full)
        if tmod is not None and tmod != mod:
            eg.append(tmod)
    engine_globs[mod] = eg


def reached_origin(tmod, name):
    """Origin of `name` as reached through module `tmod`, visible-set first."""
    origin = visible.get(tmod, {}).get(name) or exports.get(tmod, {}).get(name)
    if origin:
        return origin
    if tmod in wild:
        return "crate:" + ",".join(sorted(wild[tmod]))
    return None


for _ in range(80):
    changed = False
    for mod, uses in raw_uses.items():
        for _is_pub, full in uses:
            if full.endswith("::*"):
                continue
            name = import_tail(full)
            if name in ("self", "*") or name in definitions.get(mod, ()):
                continue
            if CRATE_RE.match(full):
                origin = "crate:" + full.split("::")[0]
            else:
                tmod, rest = longest_module(absolute(mod, full))
                if tmod is None or tmod == mod:
                    continue
                if not rest:
                    origin = f"engine:{tmod}"
                else:
                    origin = reached_origin(tmod, name) or f"engine:{tmod}::{name}"
            if visible[mod].get(name) != origin:
                visible[mod][name] = origin
                changed = True
    for mod in mods:
        for tmod in engine_globs.get(mod, ()):
            for name, origin in visible.get(tmod, {}).items():
                if name not in visible[mod]:
                    visible[mod][name] = origin
                    changed = True
    if not changed:
        break


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


# A module with no item of its own is a pure re-export shim: every name it
# exposes comes from what it globs. `crate::call_validation::ChatMessage` is
# therefore `refact_core::call_validation::ChatMessage`, not an engine symbol —
# `exports` cannot say so, because a crate glob never enters the export table.
PURE_SHIM = {
    mod
    for mod in mods
    if mod not in test_only and not FULL_DEF.search(code(prod(read(mods[mod]))))
}


def resolve_path(path):
    """Resolve an absolute module path segment by segment.

    `symbol_origin` only looks the *final* name up in the longest engine module
    prefix. A path such as `providers::claude_code::CLAUDE_CODE_OAUTH_EXPIRES_AT_HEADER`
    therefore missed the `pub use refact_providers::claude_code;` re-export one
    segment earlier and was billed as `engine:providers::CONST` — which made every
    provider-touching file look engine-coupled when the symbol has lived in
    `refact-providers` for several waves. The honest rule is the compiler's: the
    first segment that resolves to a crate re-export decides, whatever follows.
    """
    seg = [p for p in path.split("::") if p]
    cur, i = None, 0
    while i < len(seg):
        cand = "::".join(seg[: i + 1])
        if cand in mods:
            cur, i = cand, i + 1
            continue
        if cur is None:
            return ("engine", path)
        origin = exports.get(cur, {}).get(seg[i])
        child = cur + "::" + seg[i]
        if origin and origin.startswith("engine:") and child in mods:
            cur, i = child, i + 1
            continue
        if origin is None:
            if cur in PURE_SHIM and cur in wild:
                return ("crate", ",".join(sorted(wild[cur])))
            return ("engine", cur)
        kind, value = origin.split(":", 1)
        if kind == "crate":
            return ("crate", value)
        tmod, _rest = longest_module(value)
        cur = tmod or value
        i += 1
    return ("engine", cur or path)


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
        # A bare module path (`crate::files_in_workspace`) names a module that
        # lives in this tree or outside it — never a crate, even when that
        # module's `mod.rs` happens to glob a crate. `files_in_workspace` calls
        # `files_in_workspace::on_workspaces_init`, and the crate glob inside
        # `files_in_workspace/mod.rs` used to turn that engine call into a
        # `refact_files` edge, which is the same false positive one level up.
        if star:
            if tmod in files:
                deps.add(tmod)
            elif tmod in PURE_SHIM and tmod in wild:
                # A bare path into a pure re-export shim is the crate behind it.
                crates.update(wild[tmod])
            else:
                engine.add(tmod)
            return
        kind, value = resolve_path(target)
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
    # An import is a *symbol* edge, not a whole-module one. `use crate::m::{A, B}`
    # is already expanded to one absolute path per name, so binding `A` and `B`
    # finds the engine symbol a facade re-exports even when the facade also globs
    # a crate ("`pub use refact_x::*` + `pub use engine_child::*`"): the old code
    # read only the brace prefix `crate::m`, took the glob branch, and credited the
    # whole module to `wild`, which silently marked engine-resident descendants as
    # covered by the crate.
    engines_uses = [
        path
        for _is_pub, path in raw_uses.get(mod, [])
        if path.split("::")[0] in ("crate", "self", "super") and path.split("::")[-1] != "*"
    ]
    use_paths = {path for path in engines_uses if not path.endswith("::self")}
    # `use crate::m::{self, X}` really does bind the module `m` itself.
    for path in engines_uses:
        if path.endswith("::self"):
            add_edge(path[: -len("::self")])
    for path in use_paths:
        add_edge(path)
    # Names the file takes from a glob import. They are resolved through the
    # globbing module's visible item set (never through a bare `crates` claim),
    # so an engine-resident definition wins over a crate glob on the same path:
    # `local_jobs` gets `build_autonomous_job_spec` from `runtime`, not from the
    # `refact_buddy_core` glob beside it.
    # A file's own items and its own bindings always win over a glob import, so
    # they are never attributed: a local `let spec = …` must not be read as the
    # sibling module `spec`, and a `pub(crate) fn` must not be read as a crate
    # name that happens to be spelled the same. `import_tail` yields `*`/`self`
    # too, which are not bindable names.
    own_binds = (
        set(LOCAL_BIND.findall(body))
        | {import_tail(p) for _is_pub, p in raw_uses.get(mod, [])}
        | definitions.get(mod, set())
        | set(BIND_NAME.findall(body))
    )
    own_binds = {n for n in own_binds if IDENT_RE.fullmatch(n)}
    for name in sorted(set(IDENT_RE.findall(strip_literals(body))) - own_binds):
        if name in PATH_KEYWORDS:
            continue
        origin = visible.get(mod, {}).get(name)
        if not origin:
            continue
        kind, value = origin.split(":", 1)
        if kind == "crate":
            crates.update(value.split(","))
            continue
        # `engine:buddy::actor::f` names the module `buddy::actor`, so the
        # dependency is that module, never the tree root it lives under.
        owner, _rest = longest_module(value)
        owner = owner or value
        if owner == mod:
            continue
        if owner in files and owner != mod:
            deps.add(owner)
        else:
            engine.add(value)
    # A glob of a module that stays in the engine (`use super::helper::*` where
    # `helper` is not part of this tree) cannot be satisfied from the
    # destination crate at all, so it is a hard engine edge. A glob of an
    # in-tree module is not an extra edge here: the names the file actually uses
    # were attributed to their real owners just above, which is stricter.
    for target in engine_globs.get(mod, ()):
        if target not in files:
            engine.add(target)
    # Every `crate::`/`super::`/`self::` path in the body, inline or inside a
    # `use`, is a real edge: a call written as `crate::chat::summarization::f(x)`
    # binds the file to that module just as firmly as an import does.
    for ref in set(REF_RE.findall(body)):
        ref = re.sub(r"\s+", "", ref)
        # A brace head (`use crate::m::{A, B}`) is the module prefix of the
        # imports handled above; the imports carry the real edges.
        if not any(path.startswith(ref + "::") for path in use_paths):
            add_edge(ref)
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
# A shim is transparent: `crate::tasks::storage` is one line of
# `pub use refact_runtime_api::task_storage_runtime::*;`, so a file that calls it
# is crate-typed once it names the crate directly — which is what the harvest
# does. Billing the shim as an engine edge made `chat::task_agent_monitor::*`
# (2 702 lines) look blocked by engine code it never touches.
for _mod in files:
    if _mod in test_only:
        continue
    for _name in sorted(info[_mod]["engine"]):
        _owner, _rest = longest_module(_name)
        _owner = _owner or _name
        if _owner in facade and _owner != _mod:
            info[_mod]["crates"] |= info[_owner]["crates"]
            info[_mod]["engine"].discard(_name)
    for _name in sorted(info[_mod]["deps"]):
        if _name in facade:
            info[_mod]["crates"] |= info[_name]["crates"]
            info[_mod]["deps"].discard(_name)

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
    tree_islands = tuple(
        f"{TARGET}::{leaf}" for leaf in ISLANDS_BY_TREE.get(TARGET, ())
    )
    return any(
        mod == island or mod.startswith(island + "::")
        for island in ISLANDS + tree_islands
    )


# ---------------------------------------------------------------------------
# Honest separation of "movable now" from "movable in principle".
#
# Three defects used to inflate the harvest list:
#
# * documented islands were only filtered out of the *report*, so their
#   dependants stayed "movable" and were printed as harvestable now, only to be
#   unmovable in practice until the island's own card lands;
# * `ISLANDS` only described the `chat` tree, so a tree that declares its own
#   island (e.g. the `tools` shell island from the harvest card) had those files
#   reported as MOVABLE;
# * a module whose edges all land on `refact_*` crates was reported movable even
#   when one of those crates already depends on the destination crate — moving
#   the file there is a cargo dependency cycle, not a harvest.
#
# Islands and crate cycles are now removed from the movability closure itself,
# so dependants drop out too, and both are reported as their own classes.
# ---------------------------------------------------------------------------
CRATES = os.path.join(ENGINE, "crates")

# Destination crate per engine module tree. Unknown trees keep the old
# (cycle-blind) behaviour, so only reviewed trees change.
DEST_HINTS = {"tools": "refact-tools"}

# Islands that belong to a single tree and are named by that tree's own harvest
# card ("do NOT move" list). They are never harvest candidates, and neither is
# anything that depends on them.
ISLANDS_BY_TREE = {
    "tools": (
        "tool_process",
        "tool_shell",
        "shell_gate",
        "shell_gate_llm",
        "tool_process_write_stdin",
        "tools_list",
    ),
}


def workspace_crate_deps():
    out = {}
    if not os.path.isdir(CRATES):
        return out
    for entry in sorted(os.listdir(CRATES)):
        toml = os.path.join(CRATES, entry, "Cargo.toml")
        if not os.path.isfile(toml):
            continue
        body = read(toml)
        m = re.search(r'(?m)^\s*name\s*=\s*"([^"]+)"', body)
        out[m.group(1) if m else entry] = set(
            re.findall(r"(?m)^\s*(refact-[a-z0-9-]+)\s*=", body)
        )
    return out


CRATE_DEPS = workspace_crate_deps()
DEST = DEST_HINTS.get(TARGET)


def crate_closure(start):
    seen, stack = set(), [start]
    while stack:
        for dep in CRATE_DEPS.get(stack.pop(), ()):
            if dep not in seen:
                seen.add(dep)
                stack.append(dep)
    return seen


def cycle_blocker(mod):
    """A crate edge into a crate that already depends on DEST cannot move into
    DEST: cargo would see a cycle."""
    if not DEST:
        return None
    for crate in sorted(info[mod]["crates"]):
        for one in crate.split(","):
            if one and DEST in crate_closure(one.replace("_", "-")):
                return one
    return None


def is_host(mod):
    """A module tree root only declares its children; it is the destination, not
    a file that can be moved into it."""
    body = code(prod(read(mods[mod])))
    return not FULL_DEF.search(body) and bool(ANY_MOD_DECL.search(body))


def island_blocker(mod, seen=None):
    """First declared island reachable through this module's engine deps."""
    seen = set() if seen is None else seen
    for dep in sorted(info[mod]["deps"]):
        if dep in seen:
            continue
        seen.add(dep)
        if is_island(dep):
            return dep
        found = island_blocker(dep, seen)
        if found:
            return found
    return None


def count(mods_list):
    return f"{len(mods_list)} files / {sum(info[m]['lines'] for m in mods_list)} lines"


hosts = sorted(
    (m for m in files if m not in test_only and is_host(m)),
    key=lambda m: -info[m]["lines"],
)
crate_cycles = {
    m: cycle_blocker(m) for m in files if m not in test_only and cycle_blocker(m)
}

# Islands, tree roots and crate cycles are not movable *now*; dropping them from
# the set makes the closure prune every module that depends on them.
movable -= {m for m in files if is_island(m)} | set(hosts) | set(crate_cycles)
while True:
    drop = {m for m in movable if not info[m]["deps"] <= movable}
    if not drop:
        break
    movable -= drop

# Every file lands in exactly one class, so the class line counts add up to the
# tree total; a file that could be described two ways takes the first class it
# matches below (an island stays an island even if it is now crate-only).
MOVABLE_LABEL = "MOVABLE now"
CLASS_ORDER = [
    ("test-only modules (stay in engine)", lambda m: info[m]["test_only"]),
    ("declared islands (never harvested here)", is_island),
    (
        "BLOCKED on a destination-crate cycle"
        + (f" (dest={DEST})" if DEST else " (destination crate unknown, not checked)"),
        lambda m: m in crate_cycles,
    ),
    ("module hosts (destination tree roots, never move)", lambda m: m in set(hosts)),
    ("already-shim (pure crate facade)", lambda m: m in facade),
    (MOVABLE_LABEL, lambda m: m in movable),
    (
        "BLOCKED on another island (that island needs its own card first)",
        lambda m: island_blocker(m) is not None,
    ),
    ("LEFT (blocked on engine code)", lambda m: True),
]
classes = {label: [] for label, _ in CLASS_ORDER}
for mod in files:
    for label, matches in CLASS_ORDER:
        if matches(mod):
            classes[label].append(mod)
            break
for label in classes:
    classes[label].sort(key=lambda m: -info[m]["lines"])

total = sum(i["lines"] for i in info.values())
lines = [f"### {TARGET}: {len(files)} files / {total} production lines"]
for label, _ in CLASS_ORDER:
    lines.append(f"{label}: {count(classes[label])}")
listed = sum(info[m]["lines"] for mods_list in classes.values() for m in mods_list)
lines.append(f"[classes cover {listed} of {total} production lines]")
if listed != total:
    lines.append("!! COVERAGE BUG: classes do not partition the tree")
lines.append("")


def body(label):
    return sorted(classes[label], key=lambda m: -info[m]["lines"])


lines.append("### MOVABLE now (non-shim, non-test, no island, no crate cycle)")
for mod in body(MOVABLE_LABEL):
    lines.append(
        f"  {info[mod]['lines']:6}  {mod:56} crates={','.join(sorted(info[mod]['crates']))}"
    )
lines.append("")
lines.append("### BLOCKED ON ANOTHER ISLAND (that island needs its own card first)")
for mod in body("BLOCKED on another island (that island needs its own card first)"):
    lines.append(f"  {info[mod]['lines']:6}  {mod:56} island={island_blocker(mod)}")
lines.append("")
lines.append("### BLOCKED ON A DESTINATION-CRATE CYCLE")
for mod in body(
    "BLOCKED on a destination-crate cycle"
    + (f" (dest={DEST})" if DEST else " (destination crate unknown, not checked)")
):
    lines.append(
        f"  {info[mod]['lines']:6}  {mod:56} {crate_cycles[mod]} already depends on {DEST}"
    )
lines.append("")
lines.append("### DECLARED ISLANDS (own card each, never harvested here)")
for mod in body("declared islands (never harvested here)"):
    engine = sorted(info[mod]["engine"] | info[mod]["deps"])
    lines.append(f"  {info[mod]['lines']:6}  {mod:56} blocker={engine[0] if engine else '-'}")
lines.append("")
lines.append("### MODULE HOSTS (tree roots; their children move, they do not)")
for mod in body("module hosts (destination tree roots, never move)"):
    lines.append(f"  {info[mod]['lines']:6}  {mod}")
lines.append("")
lines.append("### LEFT (blocked on engine code)")
for mod in body("LEFT (blocked on engine code)"):
    engine = sorted(info[mod]["engine"] | info[mod]["deps"])
    lines.append(f"  {info[mod]['lines']:6}  {mod:56} blocker={engine[0] if engine else '-'}")
    if len(engine) > 1:
        lines.append(f"          others: {', '.join(engine[1:6])}")
lines.append("")
lines.append("### ALREADY-SHIM (pure crate facade)")
for mod in body("already-shim (pure crate facade)"):
    lines.append(f"  {info[mod]['lines']:6}  {mod}")

dest = os.path.join(OUT_DIR, f"{TARGET.split('::')[-1]}_movable_census.txt")
with open(dest, "w", encoding="utf-8") as fh:
    fh.write("\n".join(lines) + "\n")
print("\n".join(lines[:9]))
