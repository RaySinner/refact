"""AA-6 in-repo verification gate.

Usage:  python tools/dev/aa6-check.py

Deterministic, and it does not compile Rust (the card's compile gate is
`tools/dev/engine-build.ps1 -Mode check`). Two halves:

1. **Behavioural fixture.** `tools/dev/aa6_fixture.py` writes a synthetic engine
   tree in which each of the four census artifacts has exactly one discriminating
   probe, and the census is run against it with `CENSUS_SRC_OVERRIDE` /
   `CENSUS_OUT_DIR`. A probe fails when the fix is not in place.
2. **Real-tree assertions.** `integrations` reports zero movable payload, no
   blocker is an invented crate symbol, and no file is reported as its own
   blocker, across ten modules.
"""
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "tools", "dev"))

import aa6_fixture  # noqa: E402

CENSUS = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")
ENGINE_TARGET = os.path.join(ROOT, "refact-agent", "engine", "target")
ENGINE_SRC = os.path.join(ROOT, "refact-agent", "engine", "src")
ENGINE_ROOTS = {
    (n[:-3] if n.endswith(".rs") else n)
    for n in os.listdir(ENGINE_SRC)
    if n.endswith(".rs") or os.path.isdir(os.path.join(ENGINE_SRC, n))
}

CHECKS = []


def check(ok, label, detail=""):
    CHECKS.append((bool(ok), label, detail))


def read(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def run_census(module, src=None, out_dir=None):
    env = dict(os.environ)
    if src:
        env["CENSUS_SRC_OVERRIDE"] = src
    if out_dir:
        env["CENSUS_OUT_DIR"] = out_dir
    result = subprocess.run(
        [sys.executable, CENSUS, module],
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    target_dir = out_dir or ENGINE_TARGET
    report = os.path.join(target_dir, f"{module.split('::')[-1]}_movable_census.txt")
    body = read(report) if os.path.exists(report) else ""
    return result.returncode, result.stdout.decode("utf-8", "replace"), body


def header(body, key):
    m = re.search(rf"(?m)^{re.escape(key)}: (\d+) files / (\d+) lines$", body)
    return (int(m.group(1)), int(m.group(2))) if m else None


def section(body, name):
    parts = body.split(f"### {name}", 1)
    if len(parts) < 2:
        return []
    return [l.strip() for l in parts[1].split("###", 1)[0].splitlines() if l.strip()]


def movable_names(body):
    return [entry.split()[1] for entry in section(body, "MOVABLE (non-shim, non-test)")]


def blocked_map(body):
    out = {}
    for line in section(body, "LEFT / ISLANDS"):
        if line.startswith("others:"):
            continue
        parts = line.split()
        if len(parts) < 2:
            continue
        m = re.search(r"blocker=(\S+)", line)
        out[parts[1]] = m.group(1) if m else "-"
    return out


# ------------------------------------------------- 1. fixture: artifact probes
with tempfile.TemporaryDirectory() as tmp:
    fixture_src = aa6_fixture.materialise(os.path.join(tmp, "src"))
    fixture_out = os.path.join(tmp, "out")
    os.makedirs(fixture_out, exist_ok=True)
    code_, out, body = run_census("fx", src=fixture_src, out_dir=fixture_out)
    check(code_ == 0, "fixture census exits 0", out[-300:])
    head = header(body, "MOVABLE now")
    check(head is not None, "fixture report has a MOVABLE header", body[:120])

    movable = movable_names(body)
    blocked = blocked_map(body)

    # artifact 1: a bare sibling path must be a real edge.
    check("fx" in blocked, "artifact 1 (bare sibling path): `fx` is blocked, not movable",
          body)
    check(blocked.get("fx", "").startswith("fx::bare"),
          "artifact 1: `fx` is blocked by its bare-path sibling", blocked.get("fx", ""))

    # artifact 2: `use super::*;` must pull the globbed sibling's own edges in.
    check("fx::glob::leaf" in blocked,
          "artifact 2 (private glob): `leaf` is blocked, not movable", body)
    check(blocked.get("fx::glob::leaf", "").startswith("fx::glob"),
          "artifact 2: `leaf` is blocked through its private glob",
          blocked.get("fx::glob::leaf", ""))

    # artifact 3: a crate alias through `pub use refact_x::y;` is a crate edge.
    check("fx::alias::user" in movable,
          "artifact 3 (crate alias): `user` is movable, not engine-blocked", body)
    check("fx::alias::Thing" not in {v for v in blocked.values()},
          "artifact 3: no invented `fx::alias::Thing` blocker", str(blocked))

    # artifact 4: a declaration-only file is a shim, not movable payload.
    check("fx::decl" not in movable,
          "artifact 4 (declaration-only): `fx::decl` is not movable payload", body)
    shims = header(body, "already-shim (facade / declaration-only / test-only)")
    check(shims is not None and shims[0] >= 1,
          "artifact 4: declaration-only file is counted as a shim", body[:200])

# ------------------------------------------------- 2. the real tree: ten modules
MODULES = ("integrations", "providers", "chat", "tools", "memories",
           "files_in_workspace", "knowledge", "vecdb", "scheduler", "postprocessing")
PHANTOM = {
    "AccessDenied", "AuthorizationPending", "ProviderTrait", "OAuthTokens",
    "ProviderModel", "ProviderVariant", "ClaudeCodeProvider", "ClaudeCodeUsage",
    "GoogleAntigravityQuotaSummary", "exchange_code_for_session", "refresh_access_token",
    "lock_refresh_guard", "ProviderDefaults", "Error", "Max", "SlowDown", "ExpiredToken",
}
for module in MODULES:
    code_, out, body = run_census(module)
    check(code_ == 0, f"census {module} exits 0", out[-200:])
    check(header(body, "MOVABLE now") is not None, f"census {module} prints a MOVABLE header")
    blocked = blocked_map(body)
    for who, blocker in blocked.items():
        check(blocker != who, f"{module}: {who} is not its own blocker")
        tail = blocker.split("::")[-1]
        check(tail not in PHANTOM,
              f"{module}: {who} blocker `{blocker}` is not a phantom crate symbol")
        check(
            blocker == "subchat" or "::" in blocker or blocker.split("::")[0] in ENGINE_ROOTS,
            f"{module}: {who} blocker `{blocker}` is a module path, not a symbol",
        )

code_, out, body = run_census("integrations")
check(header(body, "MOVABLE now") == (0, 0),
      "integrations reports 0 movable files / 0 lines", body[:300])
code_, out, body = run_census("providers")
mv = header(body, "MOVABLE now")
check(mv is not None and mv[1] > 0,
      "providers reports its genuinely movable payload", body[:300])
for entry in section(body, "MOVABLE (non-shim, non-test)"):
    check("crates=" in entry and entry.split("crates=")[-1].strip() != "",
          f"providers movable entry names its crate edges: {entry}")

failed = [c for c in CHECKS if not c[0]]
for ok, label, detail in CHECKS:
    if not ok:
        print(f"FAIL: {label}" + (f" ({detail})" if detail else ""))
print(f"VERIFY: {'PASS' if not failed else 'FAIL'} ({len(CHECKS) - len(failed)}/{len(CHECKS)} checks)")
sys.exit(1 if failed else 0)
