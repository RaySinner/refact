"""AL-1 verification: chat::trajectories SCC settlement.

Checks the delivered state of the trajectories move, per the card:

1. every engine file of the SCC is a `pub use` facade (no production body left);
2. the destination crate holds the trajectories module tree (source files) and
   the engine facade points at it;
3. `refact-chat-history` is not a dependency of, and does not depend on, the
   destination crate in a way that closes a cycle (both Cargo edges printed);
4. body preservation: each moved file is byte-identical to the original up to
   the declared mechanical transforms (path rewrites, test gating, visibility
   widening) — delegated to `tools/dev/al1_diff.py`;
5. the census reports the SCC out of `LEFT`.

Run:  python tools/dev/al1-check.py
"""
import io
import os
import re
import subprocess
import sys

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
TRAJ = os.path.join(ENGINE, "src", "chat", "trajectories")
RA_TRAJ = os.path.join(ENGINE, "crates", "refact-runtime-api", "src", "trajectories")
RA_TOML = os.path.join(ENGINE, "crates", "refact-runtime-api", "Cargo.toml")
CH_TOML = os.path.join(ENGINE, "crates", "refact-chat-history", "Cargo.toml")

FILES = ["load", "paths", "reload", "watcher", "queries", "store"]

checks = []


def check(label, ok, detail=""):
    checks.append((label, bool(ok), str(detail)))


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


# 1. engine facades.
PROD_DEF = re.compile(
    r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:async[ \t]+)?(?:unsafe[ \t]+)?"
    r"(?:fn|struct|enum|trait|impl|static|const)\b")
for name in FILES:
    path = os.path.join(TRAJ, name + ".rs")
    check("engine %s.rs exists" % name, os.path.isfile(path))
    if not os.path.isfile(path):
        continue
    text = read(path)
    m = re.search(r"(?m)^[ \t]*#\[cfg\(test\)\]", text)
    head = text[: m.start()] if m else text
    defs = PROD_DEF.findall(head)
    body = [l for l in head.splitlines() if l.strip()]
    check("engine %s.rs is a pure facade" % name, not defs and len(body) == 1, body[:2])
    check(
        "engine %s.rs points at refact_runtime_api::trajectories" % name,
        ("pub use refact_runtime_api::trajectories::%s::*;" % name) in text,
    )

# 2. destination tree.
for name in FILES + ["mod", "model", "writer"]:
    check("crate %s.rs exists" % name, os.path.isfile(os.path.join(RA_TRAJ, name + ".rs")))
mod_rs = read(os.path.join(RA_TRAJ, "mod.rs"))
for name in FILES:
    check("crate mod.rs declares %s" % name,
          re.search(r"(?m)^pub mod %s;" % name, mod_rs) is not None)
check("crate module registered in runtime-api lib.rs",
      "pub mod trajectories;" in read(os.path.join(ENGINE, "crates/refact-runtime-api/src/lib.rs")))

# 3. cargo edges, printed.
ra_toml = read(RA_TOML)
ch_toml = read(CH_TOML)
ra_deps = set(re.findall(r"(?m)^(refact-[a-z0-9-]+)\s*=", ra_toml))
ch_deps = set(re.findall(r"(?m)^(refact-[a-z0-9-]+)\s*=", ch_toml))
print("EDGE refact-runtime-api -> refact-chat-history : %s"
      % ("refact-chat-history" in ra_deps))
print("EDGE refact-chat-history -> refact-runtime-api : %s"
      % ("refact-runtime-api" in ch_deps))
print("EDGE refact-chat-history -> refact-lsp         : %s"
      % ("refact-lsp" in ch_deps))
check("no cycle: runtime-api -> chat-history && !(chat-history -> runtime-api)",
      "refact-chat-history" in ra_deps and "refact-runtime-api" not in ch_deps)
check("refact-chat-history has no refact-lsp dep", "refact-lsp" not in ch_deps)
check("no refact-lsp dep anywhere in refact-chat-history",
      "refact-lsp" not in ch_toml)
check("md5 added to refact-runtime-api", re.search(r"(?m)^md5\s*=", ra_toml) is not None)

# 4. body preservation (delegated).
diff = subprocess.run([sys.executable, os.path.join(ROOT, "tools", "dev", "al1_diff.py")],
                      cwd=ROOT, capture_output=True)
out = diff.stdout.decode("utf-8", "replace")
print("---- al1_diff.py ----")
print(out.rstrip())
check("body preservation: no real differences", "REAL differences" not in out, "")

# 5. census.
CENSUS = os.path.join(ENGINE, "target", "chat_movable_census.txt")
if os.path.isfile(CENSUS):
    census = read(CENSUS)
    for line in census.splitlines()[:10]:
        print("CENSUS " + line)
    left = census.split("### LEFT")[1].split("### ALREADY-SHIM")[0]
    check("census LEFT has no chat::trajectories::* entry",
          not re.search(r"chat::trajectories::(load|paths|reload|watcher|queries|store)\b",
                        left))
    shim = census.split("### ALREADY-SHIM")[1]
    check("census lists chat::trajectories::load as already-shim",
          re.search(r"(?m)^\s+\d+\s+chat::trajectories::load\s*$", shim) is not None)
else:
    check("census file exists", False, CENSUS)

failed = [c for c in checks if not c[1]]
for label, ok, detail in checks:
    print(("PASS " if ok else "FAIL ") + label + (("  [" + detail + "]") if detail else ""))
print("AL-1 checks: %d run, %d failed" % (len(checks), len(failed)))
sys.exit(1 if failed else 0)
