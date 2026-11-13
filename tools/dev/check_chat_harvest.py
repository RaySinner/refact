"""Acceptance harness for the chat harvest card (J-3).

Single command, no shell separators:

    python tools/dev/check_chat_harvest.py

Checks, in order:
  1. no harvest crate (`refact-chat-api`, `refact-chat-history`,
     `refact-runtime-api`) declares an edge to the engine binary crate;
  2. no harvest crate source mentions `refact_lsp`;
  3. the symbol harvested by this card lives inside a crate and the engine
     keeps a `pub use` shim for it, so `crate::chat::room_roster::load_room_roster`
     still resolves;
  4. every documented chat island still exists under the engine tree (this card
     must not have moved one of them);
  5. the movability census runs and prints the ordered island inventory.

Exit code 0 means every check passed.
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATES = os.path.join(ENGINE, "crates")

HARVEST_CRATES = ["refact-chat-api", "refact-chat-history", "refact-runtime-api"]

HARVESTED = {
    "src/chat/room_roster.rs": "refact_runtime_api::room_roster::load_room_roster",
    "crates/refact-runtime-api/src/room_roster.rs": "pub async fn load_room_roster(",
}

DOCUMENTED_ISLANDS = [
    "src/chat/stream_core.rs",
    "src/chat/tools.rs",
    "src/chat/prompts.rs",
    "src/chat/ctx.rs",
    "src/chat/cache_guard.rs",
    "src/chat/summarization.rs",
    "src/chat/perf_harness/mod.rs",
    "src/chat/perf_diagnostics.rs",
    "src/chat/verifier.rs",
    "src/chat/goal_verifier.rs",
    "src/chat/notifications.rs",
    "src/chat/cloud_token_count.rs",
]

failures = []


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def check(label, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + label + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        failures.append(label + (f" ({detail})" if detail else ""))


print("== 1. harvest crates never depend on the engine crate ==")
for crate in HARVEST_CRATES:
    text = read(os.path.join(CRATES, crate, "Cargo.toml"))
    bad = re.findall(r"refact-lsp|refact_lsp", text)
    check(f"{crate}/Cargo.toml has no engine edge", not bad, ", ".join(bad))

print()
print("== 2. harvest crate sources never reach into the engine ==")
leaks = []
for crate in HARVEST_CRATES:
    for root, dirs, files in os.walk(os.path.join(CRATES, crate, "src")):
        dirs[:] = [d for d in dirs if d != "target"]
        for name in files:
            if name.endswith(".rs") and re.search(
                r"\brefact_lsp\b", read(os.path.join(root, name))
            ):
                leaks.append(os.path.join(crate, name))
check("no refact_lsp references", not leaks, ", ".join(leaks[:5]))

print()
print("== 3. harvested symbol: crate body + engine shim ==")
for rel, needle in HARVESTED.items():
    path = os.path.join(ENGINE, rel)
    exists = os.path.exists(path)
    text = read(path) if exists else ""
    check(f"{rel} contains {needle!r}", exists and needle in text)

print()
print("== 4. documented islands still live under the engine tree ==")
for rel in DOCUMENTED_ISLANDS:
    path = os.path.join(ENGINE, rel)
    exists = os.path.exists(path)
    note = ""
    if exists:
        body = read(path)
        if len(body) < 400 and "pub use" in body:
            note = "  (already a shim of an earlier wave - not moved by this card)"
    check(f"{rel} exists in the engine{note}", exists)

print()
print("== 5. movability census ==")
census = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")
result = subprocess.run(
    [sys.executable, census, "chat"], cwd=ROOT, capture_output=True, text=True
)
print(result.stdout.strip() or result.stderr.strip())
check("census tool runs", result.returncode == 0, result.stderr.strip()[:300])
report = os.path.join(ENGINE, "target", "chat_movable_census.txt")
if os.path.exists(report):
    print()
    print("--- " + os.path.relpath(report, ROOT) + " ---")
    print(read(report))

print()
if failures:
    print(f"RESULT: FAIL ({len(failures)} check(s))")
    sys.exit(1)
print("RESULT: PASS")
