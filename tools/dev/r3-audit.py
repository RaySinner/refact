"""R-3 blocker audit: for each `http` module, list the engine-resident symbols it
consumes (the real blockers behind the census' single "blocker=" column).

Usage: python tools/dev/r3-audit.py [module-substring]
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SRC = os.path.join(ROOT, "refact-agent", "engine", "src")

FILTER = sys.argv[1] if len(sys.argv) > 1 else ""

USE_STMT = re.compile(r"\b(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);", re.S)


def read(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def strip_tests(text):
    i = re.search(r"(?m)^[ \t]*#\[cfg\(test\)\]", text)
    return text[: i.start()] if i else text


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


def facade_kind(path):
    body = strip_tests(read(path))
    lines = [l for l in body.splitlines() if l.strip() and not l.strip().startswith("//")]
    if not lines:
        return "empty"
    only_uses = all(l.strip().startswith(("use ", "pub use", "pub(crate) use")) for l in lines)
    total = len(lines)
    return "facade(%d)" % total if only_uses else "engine(%d)" % total


def resolve(cur_mod, ref):
    parts = [p.strip() for p in ref.split("::") if p.strip()]
    head = parts[0]
    if head == "crate":
        return "::".join(parts[1:])
    if head == "super":
        base = cur_mod.split("::")
        rest = parts
        while rest and rest[0] == "super":
            base = base[:-1]
            rest = rest[1:]
        return "::".join(base + rest)
    if head == "self":
        return cur_mod + ("::" + "::".join(parts[1:]) if len(parts) > 1 else "")
    return ref


def module_file(engine_path):
    for cand in (
        os.path.join(SRC, engine_path.replace("::", os.sep) + ".rs"),
        os.path.join(SRC, engine_path.replace("::", os.sep), "mod.rs"),
    ):
        if os.path.exists(cand):
            return cand
    return None


files = []
for root, dirs, names in os.walk(os.path.join(SRC, "http")):
    for name in names:
        if name.endswith(".rs"):
            files.append(os.path.join(root, name))

for full in sorted(files):
    rel = os.path.relpath(full, SRC)[:-3].replace(os.sep, "::")
    if FILTER and FILTER not in rel:
        continue
    body = strip_tests(read(full))
    lines = [l for l in body.splitlines() if l.strip() and not l.strip().startswith(("//", "/*"))]
    if lines and all(l.strip().startswith("pub use") for l in lines):
        continue
    rows = []
    for m in USE_STMT.finditer(body):
        out = []
        expand("", m.group(1), out)
        for item in out:
            if not item.startswith(("crate::", "super::", "self::")):
                continue
            target = resolve(rel, item)
            seg = target.split("::")
            mod = None
            for k in range(len(seg), 0, -1):
                cand = "::".join(seg[:k])
                if module_file(cand):
                    mod, rest = cand, seg[k:]
                    break
            if mod is None:
                rows.append(("???", target))
                continue
            kind = facade_kind(os.path.join(SRC, mod.replace("::", os.sep) + ".rs")
                               if os.path.exists(os.path.join(SRC, mod.replace("::", os.sep) + ".rs"))
                               else os.path.join(SRC, mod.replace("::", os.sep), "mod.rs"))
            rows.append((kind, target))
    if not rows:
        continue
    print("=== %s" % rel)
    for kind, target in sorted(set(rows)):
        print("    %-14s %s" % (kind, target))
