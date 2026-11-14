"""Q-5 verification: buddy harvest moved only AppState-shaped modules, and the
engine still sees them through `pub use` shims.

Checks, in order:

1. the census numbers for `buddy` after the move;
2. every harvested engine file is a pure `use` shim;
3. each new `refact-runtime-api` module is registered in its `lib.rs`;
4. no crate file reaches back into `crate::` (the engine);
5. the engine build gates are the ones the planner agreed on.
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATE = os.path.join(ENGINE, "crates", "refact-runtime-api", "src")

HARVESTED_ENGINE_FILES = [
    "buddy/observers/chat_pattern.rs",
    "buddy/observers/customization_drift.rs",
    "buddy/observers/diagnostic_cluster.rs",
    "buddy/observers/memory_garden.rs",
    "buddy/observers/privacy_degraded.rs",
    "buddy/observers/provider_health.rs",
    "buddy/observers/trajectory_clutter.rs",
    "buddy/observers/worktree_hygiene.rs",
    "buddy/chat_interjection.rs",
    "buddy/diagnostics.rs",
    "buddy/pulse_inject.rs",
]

NEW_CRATE_MODULES = [
    "buddy_observers",
    "buddy_chat_interjection",
    "buddy_diagnostics",
    "buddy_privacy_degraded",
    "buddy_pulse_inject",
]

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]")

failures = []


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def strip_cfg_test(text):
    while True:
        m = CFG_TEST.search(text)
        if m is None:
            return text
        rest = text[m.end() :].lstrip()
        if rest.startswith("mod ") or rest.startswith("pub mod "):
            semi = text.find(";", m.end())
            brace = text.find("{", m.end())
            if semi != -1 and (brace == -1 or semi < brace):
                text = text[: m.start()] + text[semi + 1 :]
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
            text = text[: m.start()] + text[j + 1 :]
            continue
        semi = text.find(";", m.end())
        if semi == -1:
            return text[: m.start()]
        text = text[: m.start()] + text[semi + 1 :]


print("### 1. census")
out = subprocess.run(
    [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), "buddy"],
    cwd=ROOT,
    capture_output=True,
    text=True,
)
print(out.stdout.strip())
if out.returncode != 0:
    failures.append("census script failed: " + out.stderr.strip()[-400:])

print("### 2. engine shims")
USE_STMT = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?use\s")


def statements(body):
    """Top-level `;`-terminated statements with comments removed."""
    out, cur = [], []
    for line in body.splitlines():
        code = line.split("//", 1)[0]
        cur.append(code)
        if ";" in code:
            out.append(re.sub(r"\s+", " ", " ".join(cur)).strip())
            cur = []
    if "".join(cur).strip():
        out.append(re.sub(r"\s+", " ", " ".join(cur)).strip())
    return [s for s in out if s]


for rel in HARVESTED_ENGINE_FILES:
    path = os.path.join(ENGINE, "src", *rel.split("/"))
    body = strip_cfg_test(read(path))
    stmts = statements(body)
    uses = [s for s in stmts if USE_STMT.match(s)]
    ok = bool(stmts) and len(uses) == len(stmts)
    detail = "" if ok else " | " + "; ".join(s[:40] for s in stmts if s not in uses)
    print(f"  {'OK  ' if ok else 'FAIL'} {rel:44} stmts={len(stmts)} uses={len(uses)}{detail}")
    if not ok:
        failures.append(f"{rel} is not a pure shim")

print("### 3. crate module registration")
lib = read(os.path.join(CRATE, "lib.rs"))
for mod in NEW_CRATE_MODULES:
    ok = f"pub mod {mod};" in lib
    print(f"  {'OK  ' if ok else 'FAIL'} pub mod {mod};")
    if not ok:
        failures.append(f"refact-runtime-api/lib.rs does not declare {mod}")

print("### 4. crate must not reach back into the engine")
for name in os.listdir(CRATE):
    if not name.endswith(".rs"):
        continue
    if not any(name.startswith(m) or name == m + ".rs" for m in NEW_CRATE_MODULES):
        continue
    body = read(os.path.join(CRATE, name))
    hits = re.findall(r"\bcrate::(?!app_state|buddy_|global_context|trajectory_runtime|files_correction|gcx_helpers|chat_ctx|services|customization_registry_gcx|ext_config_dirs|task_storage|tool_ctx|ctx_traits)", body)
    print(f"  {'OK  ' if not hits else 'FAIL'} {name:34} backrefs={len(hits)}")
    if hits:
        failures.append(f"{name} reaches into the engine: {sorted(set(hits))}")

print()
if failures:
    print("### RESULT: FAIL")
    for f in failures:
        print("  - " + f)
    sys.exit(1)
print("### RESULT: PASS")
