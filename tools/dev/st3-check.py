"""ST-3 verification: buddy harvest (pulse, mcp_auth, stalled_turn, voice_service).

Checks:
 1. census numbers for `buddy` before/after the move (report only);
 2. every harvested engine file is a pure `use` shim;
 3. each new `refact-runtime-api` module is registered in its `lib.rs`;
 4. no crate file reaches back into the engine (`crate::<engine module>`);
 5. the crate dependency edges checked for cycles, printed both directions.
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATE = os.path.join(ENGINE, "crates", "refact-runtime-api", "src")

HARVESTED_ENGINE_FILES = [
    "buddy/pulse.rs",
    "buddy/observers/mcp_auth.rs",
    "buddy/interventions/stalled_turn.rs",
    "buddy/voice_service.rs",
]

NEW_CRATE_ITEMS = [
    "pub mod buddy_pulse;",
    "pub mod buddy_observers;",
    "pub mod buddy_interventions;",
    "pub mod buddy_voice_service;",
]

NEW_CRATE_FILES = [
    "buddy_pulse.rs",
    "buddy_interventions/stalled_turn.rs",
    "buddy_voice_service.rs",
    "buddy_observers/mcp_auth.rs",
]

# Engine-only module names: these exist in `refact-lsp` and NOT in this crate, so a
# `crate::<name>` reference would be a crate -> engine dependency (and a compile error).
ENGINE_ONLY = [
    "buddy::",
    "chat::",
    "tasks::",
    "subchat::",
    "llm::",
    "yaml_configs::",
    "stats::",
    "caps::",
    "providers::",
    "vecdb::",
    "at_commands::",
    "tools::",
    "http::",
    "exec::",
]

CFG_TEST = re.compile(r"(?m)^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]")
USE_STMT = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?use\s")

failures = []


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def strip_cfg_test(text):
    while True:
        match = CFG_TEST.search(text)
        if match is None:
            return text
        semi = text.find(";", match.end())
        brace = text.find("{", match.end())
        if semi != -1 and (brace == -1 or semi < brace):
            text = text[: match.start()] + text[semi + 1 :]
            continue
        depth, index = 0, brace
        while index < len(text):
            if text[index] == "{":
                depth += 1
            elif text[index] == "}":
                depth -= 1
                if depth == 0:
                    break
            index += 1
        text = text[: match.start()] + text[index + 1 :]


def statements(body):
    out, current = [], []
    for line in body.splitlines():
        code = line.split("//", 1)[0]
        current.append(code)
        if ";" in code:
            out.append(re.sub(r"\s+", " ", " ".join(current)).strip())
            current = []
    if "".join(current).strip():
        out.append(re.sub(r"\s+", " ", " ".join(current)).strip())
    return [item for item in out if item.strip(" ;")]


print("### 1. census")
result = subprocess.run(
    [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), "buddy"],
    cwd=ROOT,
    capture_output=True,
    text=True,
)
print(result.stdout.strip())
if result.returncode != 0:
    failures.append("census script failed: " + result.stderr.strip()[-400:])

print("### 2. engine shims")
for relative in HARVESTED_ENGINE_FILES:
    path = os.path.join(ENGINE, "src", *relative.split("/"))
    body = strip_cfg_test(read(path))
    items = statements(body)
    uses = [item for item in items if USE_STMT.match(item)]
    ok = bool(items) and len(uses) == len(items)
    detail = "" if ok else " | " + "; ".join(s[:60] for s in items if s not in uses)
    print(f"  {'OK  ' if ok else 'FAIL'} {relative:38} stmts={len(items)} uses={len(uses)}{detail}")
    if not ok:
        failures.append(relative + " is not a pure shim")

print("### 3. crate registration")
lib = read(os.path.join(CRATE, "lib.rs"))
for item in NEW_CRATE_ITEMS:
    ok = item in lib
    print(f"  {'OK  ' if ok else 'FAIL'} {item}")
    if not ok:
        failures.append("refact-runtime-api/lib.rs misses " + item)

for relative in NEW_CRATE_FILES:
    path = os.path.join(CRATE, *relative.split("/"))
    ok = os.path.isfile(path)
    print(f"  {'OK  ' if ok else 'FAIL'} {relative} exists")
    if not ok:
        failures.append("missing crate file " + relative)

print("### 4. crate files must not reach back into the engine")
for relative in NEW_CRATE_FILES:
    path = os.path.join(CRATE, *relative.split("/"))
    body = read(path)
    hits = sorted(
        {
            pattern
            for pattern in ENGINE_ONLY
            if re.search(r"\bcrate::" + re.escape(pattern), body)
        }
    )
    print(f"  {'OK  ' if not hits else 'FAIL'} {relative:34} backrefs={hits}")
    if hits:
        failures.append(relative + " reaches into the engine: " + str(hits))

print("### 5. dependency edges (both directions)")
crate_toml = read(os.path.join(ENGINE, "crates", "refact-runtime-api", "Cargo.toml"))
engine_toml = read(os.path.join(ENGINE, "Cargo.toml"))
for name in ("refact-buddy-core", "refact-chat-api", "refact-core", "refact-ext", "refact-worktrees"):
    forward = name in crate_toml
    reverse = re.search(r"^" + re.escape(name) + r"\s*=\s*\{[^}]*refact-runtime-api", crate_toml, re.M)
    print(f"  {'OK  ' if forward else 'FAIL'} {name:20} runtime-api -> {name}: {forward}")
    if not forward:
        failures.append("refact-runtime-api does not depend on " + name)
    if reverse:
        failures.append(name + " depends back on refact-runtime-api (cycle)")
engine_edge = "refact-runtime-api" in engine_toml
print(f"  {'OK  ' if engine_edge else 'FAIL'} refact-lsp -> refact-runtime-api: {engine_edge}")
if not engine_edge:
    failures.append("engine does not depend on refact-runtime-api")

print()
if failures:
    print("### RESULT: FAIL")
    for failure in failures:
        print("  - " + failure)
    sys.exit(1)
print("### RESULT: PASS")
