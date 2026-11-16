"""AL-2 probe: dump crate dependency edges and the engine-symbol residue of the five files."""
import io
import os
import re
import sys

WT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.getcwd()
ENG = os.path.join(WT, "refact-agent", "engine")
CRATES = os.path.join(ENG, "crates")


def read(path):
    with io.open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def deps_of(crate_dir):
    cargo = os.path.join(CRATES, crate_dir, "Cargo.toml")
    if not os.path.exists(cargo):
        return None
    text = read(cargo)
    return sorted(set(re.findall(r"^(refact[-a-z0-9_]+)\s*=\s*\{", text, re.M)))


print("=== workspace crate deps (refact-*) ===")
for name in sorted(os.listdir(CRATES)):
    d = deps_of(name)
    if d is None:
        continue
    print("%-28s -> %s" % (name, ", ".join(d)))
