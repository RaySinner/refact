"""Per-file blocker analysis for a module, precise about engine-local definitions.

Unlike census_module.py this does NOT credit a module-level `pub use refact_x::*`
with names the engine module defines itself. For every `crate::<path>` reference in
a file it decides:

  crate   - the referenced engine module re-exports the name from a real crate
            (the engine module is a shim for it), or the path head is a crate root
  engine  - the name is defined locally in the engine source that owns the path
  missing - the referenced engine source file / module cannot be found

Usage:
  python y3-blockers.py <module>            summary for every file in the module
  python y3-blockers.py --file <rel.rs>     detail for one file
  python y3-blockers.py --simulate <module>
        removes ONE blocker symbol (or `module::name` group) at a time from the
        hard-reference set of every file in the module and reports how many files
        the closure then frees, largest first.
"""
import os
import re
import sys

ROOT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
    "refact-agent",
    "engine",
    "src",
)

USE_STMT = re.compile(r"\buse[ \t]+([^;]+);", re.S)
CRATE_REF = re.compile(r"\bcrate::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)")
PUB_USE = re.compile(
    r"pub(?:\([^)]*\))?[ \t]+use[ \t]+(refact_[a-z0-9_]+)::([^;]+);", re.S
)
LOCAL_DEF = re.compile(
    r"^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|type|const|static|union)[ \t]+([A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def prod_part(text):
    m = re.search(r"^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]\s*\r?\n\s*mod ", text, re.M)
    return text[: m.start()] if m else text


def strip_comments(text):
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


def module_files():
    mods = {}
    for dirpath, dirs, files in os.walk(ROOT):
        dirs[:] = [d for d in dirs if d not in ("target",)]
        for name in files:
            if not name.endswith(".rs"):
                continue
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, ROOT).replace("\\", "/")[:-3]
            if rel.endswith("/mod"):
                rel = rel[:-4]
            mods[rel.replace("/", "::")] = full
    return mods


MODS = module_files()

ROOT_ALIAS = {}
LIB = os.path.join(ROOT, "lib.rs")
for _m in re.finditer(
    r"pub use (refact_[a-z0-9_]+)::([A-Za-z_][A-Za-z0-9_]*)\s*;", read(LIB)
):
    ROOT_ALIAS[_m.group(2)] = _m.group(1)


def owner(rel_path):
    """Locate the engine source file that owns a `crate::a::b::c` path."""
    seg = rel_path.split("::")
    for cut in range(len(seg), 0, -1):
        cand = "::".join(seg[:cut])
        if cand in MODS:
            return cand, seg[cut:]
    if seg[0] in ROOT_ALIAS and seg[0] not in MODS:
        return f"{ROOT_ALIAS[seg[0]]}::{seg[0]}", seg[1:]
    return None, seg


def reexported(mod, name):
    """Crate name if engine module `mod` re-exports `name` from a crate."""
    if mod.startswith("refact_") and "::" in mod:
        return mod.split("::")[0]
    text = read(MODS[mod])
    for m in PUB_USE.finditer(text):
        crate = m.group(1)
        expanded = []
        expand("", m.group(2), expanded)
        for piece in expanded:
            if piece.split("::")[-1] != "*" and piece.split("::")[-1] != name:
                continue
            if re.search(
                r"^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:unsafe[ \t]+)?"
                r"(?:fn|struct|enum|trait|type|const|static|union|mod)[ \t]+"
                + re.escape(name)
                + r"\b",
                text,
                re.M,
            ):
                return None
            return crate
    return None


def classify(ref):
    mod, rest = owner(ref)
    if mod is None:
        head = ref.split("::")[0]
        if head.startswith("refact_"):
            return "crate", head
        return "missing", ref
    if mod.endswith("::tests") or mod == "tests":
        return "engine", f"{mod}::{rest[0] if rest else ''}"
    if not rest:
        return facade_or_module(mod)
    name = rest[0]
    crate = reexported(mod, name)
    if crate:
        return "crate", f"{crate}::{name}"
    return "engine", f"{mod}::{name}"


def facade_or_module(mod):
    """A bare `crate::a::b` reference: crate if `b` is a pure re-export facade."""
    if mod.startswith("refact_"):
        return "crate", mod.split("::")[0]
    text = read(MODS[mod])
    if PUBLIC_DEF_TIGHT.search(text) or MACRO_DEF.search(text):
        return "engine", mod
    own = [
        u
        for _p, u in [
            (m.group(1) is not None, m.group(2))
            for m in re.finditer(r"\b(pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([^;]+);", text, re.S)
        ]
        if not u.strip().startswith("refact_") and not u.strip().startswith("crate::")
    ]
    if own:
        return "engine", mod
    crates = sorted(
        {
            m.group(1)
            for m in re.finditer(r"\buse[ \t]+(refact_[a-z0-9_]+)::", text)
        }
        | {m.group(1) for m in re.finditer(r"\buse[ \t]+crate::([A-Za-z_][A-Za-z0-9_]*)", text)}
    )
    if len(crates) == 1:
        head = crates[0]
        return "crate", ROOT_ALIAS.get(head, head)
    return "crate", ",".join(crates) if crates else mod


PUBLIC_DEF_TIGHT = re.compile(
    r"^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|type|const|static|union|mod)[ \t]+[A-Za-z_]",
    re.M,
)
MACRO_DEF = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?macro_rules!", re.M)


def refs_of(path):
    body = strip_comments(prod_part(read(path)))
    out = set()
    for m in USE_STMT.finditer(body):
        parts = []
        expand("", m.group(1), parts)
        for full in parts:
            if full.startswith("crate::"):
                out.add(full[len("crate::"):])
    for m in CRATE_REF.finditer(re.sub(USE_STMT, "", body)):
        out.add(m.group(1))
    return sorted(out)


LOCAL_ROOTS = {
    "crate", "self", "super", "std", "core", "alloc", "test", "tests",
}


def extern_roots(body):
    """External crate roots referenced by a file (use statements + inline paths)."""
    roots = set()
    for m in USE_STMT.finditer(body):
        parts = []
        expand("", m.group(1), parts)
        for piece in parts:
            head = piece.split("::")[0]
            if re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", head):
                roots.add(head)
    for m in re.finditer(r"\b([a-z_][a-z0-9_]*)::", body):
        roots.add(m.group(1))
    roots -= LOCAL_ROOTS
    roots -= set(MODS)
    roots = {r for r in roots if not any(k.startswith(r + "::") for k in MODS)}
    return sorted(roots)


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return
    if args[0] == "--simulate":
        simulate(args[1] if len(args) > 1 else "http")
        return
    if args[0] == "--tests":
        for rel in args[1:]:
            mod = rel.replace("\\", "/").replace("/", "::")[:-3]
            path = MODS.get(mod)
            if not path:
                print(f"{mod} <not found>")
                continue
            text = read(path)
            m = re.search(r"^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]", text, re.M)
            if not m:
                print(f"no cfg(test) in {mod}")
                continue
            tail = text[m.start():]
            refs = sorted(set(CRATE_REF.findall(tail)))
            print(f"{mod}: test-part crate:: refs -> {refs}")
        return
    if args[0] == "--testsafe":
        for rel in args[1:]:
            mod = rel.replace("\\", "/").replace("/", "::")[:-3]
            path = MODS.get(mod)
            if not path:
                print(f"{mod} <not found>")
                continue
            text = read(path)
            m = re.search(r"^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]", text, re.M)
            if not m:
                print(f"SAFE     {mod}  (no tests; move whole file)")
                continue
            prod, tail = text[: m.start()], text[m.start():]
            private = set()
            for pm in re.finditer(
                r"^[ \t]*(?:async[ \t]+)?(?:unsafe[ \t]+)?"
                r"(?:fn|struct|enum|trait|type|const|static|union|macro_rules!)[ \t]+"
                r"([A-Za-z_][A-Za-z0-9_]*)",
                prod,
                re.M,
            ):
                private.add(pm.group(1))
            private -= set(
                re.findall(
                    r"^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:unsafe[ \t]+)?"
                    r"(?:fn|struct|enum|trait|type|const|static|union)[ \t]+"
                    r"([A-Za-z_][A-Za-z0-9_]*)",
                    prod,
                    re.M,
                )
            )
            used = {i for i in private if re.search(r"\b" + re.escape(i) + r"\b", tail)}
            label = "SAFE    " if not used else "UNSAFE  "
            print(f"{label} {mod}  private-in-test: {sorted(used)}")
        return
    if args[0] == "--prod":
        for rel in args[1:]:
            mod = rel.replace("\\", "/").replace("/", "::")[:-3]
            path = MODS.get(mod)
            if not path:
                print(f"{mod} <not found>")
                continue
            body = strip_comments(prod_part(read(path)))
            print(f"### {mod}: {body.count(chr(10)) + 1} prod lines")
            for ref in sorted(set(CRATE_REF.findall(body))):
                kind, val = classify(ref)
                print(f"  {kind:8s} crate::{ref}  ->  {val}")
        return
    if args[0] == "--full":
        full(args[1] if len(args) > 1 else "http")
        return
    if args[0] == "--extern":
        for rel in args[1:]:
            mod = rel.replace("\\", "/").replace("/", "::")[:-3]
            path = MODS.get(mod)
            if not path:
                print(f"{mod} <not found>")
                continue
            body = strip_comments(read(path))
            prod = strip_comments(prod_part(read(path)))
            print(f"### {mod}")
            print(f"    prod  externs: {sorted(extern_roots(prod))}")
            print(f"    whole externs: {sorted(extern_roots(body))}")
        return
    if args[0] == "--plan":
        plan(args[1] if len(args) > 1 else "http")
        return
    if args[0] == "--summary":
        summary(args[1] if len(args) > 1 else "http")
        return
    if args[0] == "--file":
        targets = [a.replace("\\", "/").replace("/", "::")[:-3] for a in args[1:]]
    else:
        module = args[0]
        targets = [m for m in MODS if m == module or m.startswith(module + "::")]
    for mod in sorted(targets):
        if mod not in MODS:
            print(f"##### {mod} <not found>")
            continue
        hard = []
        for ref in refs_of(MODS[mod]):
            kind, val = classify(ref)
            if kind in ("engine", "missing"):
                hard.append(f"{val}")
        label = "BLOCKED" if hard else "movable"
        print(f"{label:8s} {mod}" + (f"  <- {', '.join(sorted(set(hard)))}" if hard else ""))


def hard_map(module):
    targets = [m for m in MODS if m == module or m.startswith(module + "::")]
    hard = {}
    for mod in targets:
        refs = refs_of(MODS[mod])
        hard[mod] = {classify(r)[1] for r in refs if classify(r)[0] in ("engine", "missing")}
    return targets, hard


def text_refs(text):
    body = strip_comments(text)
    out = set()
    for m in USE_STMT.finditer(body):
        parts = []
        expand("", m.group(1), parts)
        for full in parts:
            if full.startswith("crate::"):
                out.add(full[len("crate::"):])
    for m in CRATE_REF.finditer(re.sub(USE_STMT, "", body)):
        out.add(m.group(1))
    return sorted(out)


def hard_of(text):
    return {
        classify(r)[1]
        for r in text_refs(text)
        if classify(r)[0] in ("engine", "missing")
    }


def full(module):
    """List every file in the module that can move as a whole (prod + tests)."""
    targets = [m for m in MODS if m == module or m.startswith(module + "::")]
    whole = []
    prod_only = []
    for mod in sorted(targets):
        text = read(MODS[mod])
        if not hard_of(text):
            lines = text.count("\n") + 1
            whole.append((lines, mod))
        elif not hard_of(prod_part(text)):
            pr = prod_part(text)
            prod_only.append((pr.count("\n") + 1, mod, sorted(hard_of(text))))
    whole.sort(reverse=True)
    prod_only.sort(reverse=True)
    print(f"### {module}: {len(targets)} files")
    print(f"### movable whole (prod+tests clean): {len(whole)} files / "
          f"{sum(l for l, _ in whole)} lines")
    for lines, mod in whole:
        print(f"{lines:6d}  {mod}")
    print(f"### movable prod only (tests need engine helpers): {len(prod_only)} files")
    for lines, mod, hard in prod_only:
        print(f"{lines:6d}  {mod}  tests need: {hard}")


PRIVATE_DEF = re.compile(
    r"^[ \t]*(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|type|const|static|union)[ \t]+([A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)
PUBLIC_DEF = re.compile(
    r"^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|type|const|static|union)[ \t]+([A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)


def plan(module):
    """Classify every file: what can move verbatim, and what it would need."""
    targets = [m for m in MODS if m == module or m.startswith(module + "::")]
    rows = []
    for mod in sorted(targets):
        text = read(MODS[mod])
        prod = prod_part(text)
        split = re.search(r"^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]", text, re.M)
        tail = text[split.start() :] if split else ""
        prod_hard = sorted(hard_of(prod))
        test_hard = sorted(hard_of(tail)) if tail else []
        local_macro = set(re.findall(r"macro_rules! ([A-Za-z_][A-Za-z0-9_]*)", prod))
        private = (set(PRIVATE_DEF.findall(prod)) | local_macro) - set(
            PUBLIC_DEF.findall(prod)
        )
        used = sorted(i for i in private if re.search(r"\b" + re.escape(i) + r"\b", tail))
        if not prod_hard and not test_hard:
            kind = "MOVE_WHOLE"
        elif not prod_hard and not used:
            kind = "MOVE_PROD_KEEP_TESTS"
        elif not prod_hard:
            kind = "BLOCKED_BY_TEST_PRIVATE"
        else:
            kind = "BLOCKED_BY_PROD"
        rows.append((prod.count("\n") + 1, mod, kind, prod_hard, test_hard, used))
    for lines, mod, kind, prod_hard, test_hard, used in sorted(rows, key=lambda r: -r[0]):
        print(f"{kind:24s} {lines:5d}  {mod}")
        if kind == "BLOCKED_BY_PROD":
            print(f"        prod needs: {prod_hard}")
        if kind == "BLOCKED_BY_TEST_PRIVATE":
            print(f"        tests use private: {used}")
        if kind == "MOVE_PROD_KEEP_TESTS":
            print(f"        tests stay in engine, need: {test_hard}")


def summary(module):
    """Count files/lines per plan class, plus the lines behind each blocker."""
    targets = [m for m in MODS if m == module or m.startswith(module + "::")]
    buckets = {}
    blockers = {}
    for mod in targets:
        text = read(MODS[mod])
        prod = prod_part(text)
        split = re.search(r"^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]", text, re.M)
        tail = text[split.start() :] if split else ""
        prod_hard = sorted(hard_of(prod))
        test_hard = sorted(hard_of(tail)) if tail else []
        private = (set(PRIVATE_DEF.findall(prod)) | set(
            re.findall(r"macro_rules! ([A-Za-z_][A-Za-z0-9_]*)", prod)
        )) - set(PUBLIC_DEF.findall(prod))
        used = [i for i in private if re.search(r"\b" + re.escape(i) + r"\b", tail)]
        lines = prod.count("\n") + 1
        if not prod_hard and not test_hard:
            kind = "MOVE_WHOLE"
        elif not prod_hard and not used:
            kind = "MOVE_PROD_KEEP_TESTS"
        elif not prod_hard:
            kind = "BLOCKED_BY_TEST_PRIVATE"
        else:
            kind = "BLOCKED_BY_PROD"
        count, total = buckets.get(kind, (0, 0))
        buckets[kind] = (count + 1, total + lines)
        for blocker in prod_hard:
            c, l = blockers.get(blocker, (0, 0))
            blockers[blocker] = (c + 1, l + lines)
    print(f"### {module} plan summary")
    for kind, (count, total) in sorted(buckets.items()):
        print(f"  {kind:24s} {count:3d} files / {total:6d} prod lines")
    print("### blockers by blocked prod lines")
    for blocker, (count, total) in sorted(blockers.items(), key=lambda kv: -kv[1][1])[:25]:
        print(f"  {total:6d} lines  {count:2d} files  {blocker}")


def closure_blocked(targets, hard, removed):
    """Files still blocked once every symbol in `removed` is considered resolvable."""
    remaining = {m: {h for h in hs if h not in removed} for m, hs in hard.items()}
    movable = {m for m, hs in remaining.items() if not hs}
    changed = True
    while changed:
        changed = False
        for mod in targets:
            if mod in movable:
                continue
            hs = remaining[mod]
            unresolved = [
                h
                for h in hs
                if not (h.split("::")[0] in remaining and h.split("::")[0] in movable)
                and not (h.split("::")[0] in remaining and mod == h.split("::")[0])
            ]
            if not unresolved:
                movable.add(mod)
                changed = True
    return set(targets) - movable


def simulate(module):
    targets, hard = hard_map(module)
    base_blocked = closure_blocked(targets, hard, set())
    symbols = sorted({h for hs in hard.values() for h in hs})
    rows = []
    for sym in symbols:
        after = closure_blocked(targets, hard, {sym})
        freed = base_blocked - after
        if freed:
            rows.append((len(freed), sym, sorted(freed)))
    print(f"### {module}: {len(targets)} files, blocked {len(base_blocked)}")
    for count, sym, freed in sorted(rows, key=lambda r: -r[0]):
        print(f"{count:4d} freed  {sym}")
        for f in freed:
            print(f"            {f}")
    if not rows:
        print("no single blocker frees a file on its own")


main()
