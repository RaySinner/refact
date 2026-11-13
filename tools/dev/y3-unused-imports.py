"""Flag `use` items in the moved files whose last segment never appears in the body.

Cannot replace rustc, but catches the one class of warning the move could
introduce: an import path rewritten but the name silently unused.

Usage: python tools/dev/y3-unused-imports.py
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CRATE = os.path.join(
    ROOT, "refact-agent", "engine", "crates", "refact-http", "src", "routers", "v1"
)

USE_STMT = re.compile(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([^;]+);", re.S)

TRAIT_SUFFIXES = ("Ext", "Write", "Read", "Iterator", "Future", "TryFrom", "FromStr")


def is_trait_import(name):
    return name.endswith(TRAIT_SUFFIXES)


def names(body):
    body = re.sub(r"\s+", " ", body)
    out = []
    for piece in re.split(r"[,{]", body):
        piece = piece.strip().strip("}").strip()
        if not piece or piece.endswith("::*"):
            continue
        last = piece.split("::")[-1].split(" as ")[-1].strip()
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", last):
            out.append(last)
    return out


problems = []
for name in sorted(os.listdir(CRATE)):
    if not name.endswith(".rs"):
        continue
    path = os.path.join(CRATE, name)
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    rest = USE_STMT.sub("", text)
    for m in USE_STMT.finditer(text):
        for imported in names(m.group(1)):
            if not imported:
                continue
            if is_trait_import(imported):
                continue
            if not re.search(r"(?<![A-Za-z0-9_])" + re.escape(imported) + r"(?![A-Za-z0-9_])", rest):
                problems.append(f"{name}: unused import `{imported}`")

if problems:
    print("\n".join(problems))
    sys.exit(1)
print("OK: every imported name is referenced")
