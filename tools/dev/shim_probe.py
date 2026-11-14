"""Report shim status for engine modules given as `a::b::c` arguments.

A module is a PURE SHIM when every production line that is not a comment is a
`use` statement (its whole body moved to a crate).  Anything else still holds
engine code, so it blocks harvest candidates that import it.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SRC = os.path.join(ROOT, "refact-agent", "engine", "src")

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]")


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def strip_cfg_test(text):
    out, i = [], 0
    while True:
        m = CFG_TEST.search(text, i)
        if m is None:
            out.append(text[i:])
            return "".join(out)
        out.append(text[i : m.start()])
        rest = text[m.end() :].lstrip()
        if rest.startswith("mod ") or rest.startswith("pub mod "):
            semi = text.find(";", m.end())
            brace = text.find("{", m.end())
            if semi != -1 and (brace == -1 or semi < brace):
                i = semi + 1
                continue
            depth, j = 0, brace
            while j < len(text):
                if text[j] == "{":
                    depth += 1
                elif text[j] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                j += 1
            i = j + 1
            continue
        semi = text.find(";", m.end())
        if semi == -1:
            return "".join(out)
        i = semi + 1


def classify(rel):
    base = os.path.join(SRC, *rel.split("::"))
    for cand in (base + ".rs", os.path.join(base, "mod.rs")):
        if os.path.isfile(cand):
            body = strip_cfg_test(read(cand))
            prod = []
            for line in body.splitlines():
                code = line.split("//", 1)[0].strip()
                if code:
                    prod.append(code)
            uses = [l for l in prod if re.match(r"^pub(\([^)]*\))?\s+use\s", l) or l.startswith("use ")]
            kind = "PURE-SHIM" if prod and len(uses) == len(prod) else "HAS-CODE"
            return f"{kind:10} lines={len(read(cand).splitlines()):6} prod={len(prod):6} {cand}"
    return f"MISSING    {rel}"


for m in sys.argv[1:]:
    print(classify(m))
