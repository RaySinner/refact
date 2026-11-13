"""L-6 read-only proof: the K3a move preserved both halves byte-for-byte.

Half 2 (crate body): the pre-move production region + exactly the documented
                     edits (crate-path rewrites, private->pub widening).
Half 1 (engine shim): a glob re-export + the `#[cfg(test)]`-gated re-imports the
                     kept test module needs + that module verbatim.

Reuses the mover's own helpers, so it recomputes the expected file from the
mover's rules instead of re-deriving them.

Run from the repo root: python tools/dev/l6-verbatim.py
"""
import difflib
import pathlib
import re
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import importlib.util

_spec = importlib.util.spec_from_file_location(
    "l6_move", pathlib.Path(__file__).resolve().parent / "l6-move.py")
mover = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mover)

SRC = mover.SRC
DST = mover.DST


def original(mod):
    raw = subprocess.run(["git", "-C", str(mover.ENG), "show",
                          "%s:refact-agent/engine/src/tools/%s.rs" % (mover.BASE, mod)],
                         capture_output=True, check=True).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n").split("\n")


ok = True
for mod in mover.MODS:
    lines = original(mod)
    prod, test = mover.split_test(lines)

    body_text = mover.crate_body(mod, prod, [])
    got_body = (DST / (mod + ".rs")).read_text(encoding="utf-8").rstrip("\n")
    same_body = body_text.rstrip("\n") == got_body

    pre = mover.test_prelude(mod, prod, test)
    expected_shim = "pub use refact_tools::tools::%s::*;\n\n" % mod
    if pre:
        expected_shim += "\n".join("#[cfg(test)]\n" + line for line in pre) + "\n"
    expected_shim += "\n".join(test).rstrip("\n")
    got_shim = (SRC / (mod + ".rs")).read_text(encoding="utf-8").rstrip("\n")
    same_shim = expected_shim == got_shim

    ok = ok and same_body and same_shim
    print("%-22s crate body verbatim: %-5s | engine shim (prelude + tests) verbatim: %s"
          % (mod, same_body, same_shim))
    for label, exp, got in [("body", body_text, got_body), ("shim", expected_shim, got_shim)]:
        if exp.rstrip("\n") != got.rstrip("\n"):
            for line in list(difflib.unified_diff(exp.split("\n"), got.split("\n"),
                                                  "expected", "got", lineterm=""))[:30]:
                print("   ", label, line)

print("RESULT:", "ALL VERBATIM" if ok else "MISMATCH")
sys.exit(0 if ok else 1)
