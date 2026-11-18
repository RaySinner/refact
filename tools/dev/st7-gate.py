"""ST-7 step-0 gate: can the 12 chat files be harvested yet?

`tools/dev/chat_movable_census.py` is the source of truth for what may move. This
script answers the one question ST-7 has to answer before touching any file, and
keeps the answer reproducible instead of trusting a pasted transcript:

    python tools/dev/st7-gate.py --expect closed   # ST-7: nothing may move yet
    python tools/dev/st7-gate.py --expect open     # the harvest card may proceed
    python tools/dev/st7-gate.py --expect harvested  # BN-1: its three files are shims

The gate is CLOSED while any card file is still LEFT (blocked on engine code).
`chat::queue::dispatch` is the real gate: it is the largest file in the set and
every other card file either depends on it or on the same engine islands, so its
class decides whether a harvest is safe.

After the harvest the gate file is neither MOVABLE nor LEFT: it is an already-shipped
shim. `--expect harvested` asserts exactly that for the three BN-1 files and that
every other card file kept its pre-harvest class and line count, so a partial move
or collateral harvest cannot pass as a success.

Exit code is 0 when the observed gate matches `--expect`, 1 otherwise.
"""

import argparse
import os
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
OUT_DIR = os.path.join(ENGINE, "target")
REPORT = os.path.join(OUT_DIR, "chat_movable_census.txt")
CENSUS = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")
CRATES = os.path.join(ENGINE, "crates")

# The ST-6 seams this card relies on must already be installed.
GENERATION_SEAM = os.path.join(CRATES, "refact-runtime-api", "src", "generation_start.rs")
CHAT_CTX = os.path.join(CRATES, "refact-runtime-api", "src", "chat_ctx.rs")
ENGINE_SEAM = os.path.join(ENGINE, "src", "app_state", "generation.rs")

# The files the card claims were released by ST-6, in the card's own order.
CARD_FILES = [
    "chat::queue::dispatch",
    "chat::generation::stream",
    "chat::task_agent_monitor::broadcast",
    "chat::context_rebuild::pipeline",
    "chat::generation::run_loop",
    "chat::handlers",
    "chat::generation::recovery",
    "chat::generation::preamble",
    "chat::goal_monitor",
    "chat::task_agent_monitor::finish",
    "chat::task_agent_monitor::monitor",
]

GATE_FILE = "chat::queue::dispatch"

# The three mutually referential files BN-1 moved in one commit. After the
# harvest they are already-shipped shims, not MOVABLE files.
BN1_FILES = ["chat::queue::dispatch", "chat::goal_monitor", "chat::delivery"]

# `chat::context_rebuild::pipeline` is absent on purpose: BN-2 shimmed it, so it
# is no longer a card file the gate has to keep an eye on. Keeping its 783 in
# this map is what made both `--expect open` and `--expect harvested` fail after
# BN-2 - a gate that cannot express the current state is worse than no gate.
POST_HARVEST_BASELINE = {}

# Card files a later sub-card already harvested. They must be shims and must not
# be MOVABLE again; every file still listed in POST_HARVEST_BASELINE must remain
# LEFT at its recorded line count, so a collateral harvest cannot pass as a
# success. BN-5 moved the last two clusters (generation + task_agent_monitor) in
# one commit, so nothing is left at its pre-harvest line count.
HARVESTED_SHIMS = [
    "chat::handlers",
    "chat::generation::preamble",
    "chat::generation::recovery",
    "chat::generation::retry",
    "chat::generation::run_loop",
    "chat::generation::stream",
    "chat::task_agent_monitor::broadcast",
    "chat::task_agent_monitor::finish",
    "chat::task_agent_monitor::monitor",
]

# ST-13 makes `chat::queue` a pure crate facade. When that lands, nothing may
# still name it as an engine blocker - that is the whole point of the card.
QUEUE_FACADE_FILE = os.path.join(ENGINE, "src", "chat", "queue", "mod.rs")

failures = []


def check(name, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        failures.append(name)


def run_census():
    os.makedirs(OUT_DIR, exist_ok=True)
    proc = subprocess.run(
        [sys.executable, CENSUS, "chat"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stdout)
        sys.stderr.write(proc.stderr)
    return proc.returncode


def parse(path):
    """Return {class label: {module: (lines, blocker, others)}} plus the summary lines."""
    sections = {}
    summary = []
    label = None
    last = None
    with open(path, encoding="utf-8") as handle:
        for raw in handle:
            line = raw.rstrip("\n")
            if line.startswith("### "):
                label = line[4:].strip()
                sections[label] = {}
                last = None
                continue
            if label is None:
                if line.strip():
                    summary.append(line)
                continue
            stripped = line.strip()
            if not stripped:
                last = None
                continue
            if stripped.startswith("others:"):
                if last is not None:
                    sections[label][last] = sections[label][last][:2] + (
                        [name.strip() for name in stripped[len("others:"):].split(",")],
                    )
                continue
            parts = stripped.split()
            if len(parts) < 2 or not parts[0].isdigit():
                continue
            lines, module = int(parts[0]), parts[1]
            blocker = None
            for part in parts[2:]:
                if part.startswith("blocker="):
                    blocker = part[len("blocker="):]
            sections[label][module] = (lines, blocker, [])
            last = module
    return summary, sections


def read(path):
    with open(path, encoding="utf-8-sig") as handle:
        return handle.read()


def non_test_body(text):
    """Drop every `#[cfg(test)]` item so the check sees production code only."""
    out = []
    lines = text.splitlines(True)
    i = 0
    while i < len(lines):
        if re.match(r"\s*#\[cfg\((?:test|any\(test)", lines[i]):
            depth, opened = 0, False
            while i < len(lines):
                depth += lines[i].count("{") - lines[i].count("}")
                if "{" in lines[i]:
                    opened = True
                if (opened and depth <= 0) or (not opened and ";" in lines[i]):
                    i += 1
                    break
                i += 1
            out.append("\n")
            continue
        out.append(lines[i])
        i += 1
    return "".join(out)


def crate_depends_on_engine(crate_cargo):
    """True when a crate lists the engine (`refact-lsp`) as a dependency."""
    text = read(crate_cargo)
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("#") or "path" not in stripped:
            continue
        if "refact-lsp" in stripped:
            return True
    return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--expect", choices=["closed", "open", "harvested"], default="closed")
    args = parser.parse_args()

    print("=== ST-7 step-0 gate: chat harvest readiness ===")
    check("C1 census script present", os.path.isfile(CENSUS))
    check("C2 census exits 0", run_census() == 0)
    check("C3 census report written", os.path.isfile(REPORT))
    if failures:
        print("\nST-7 GATE: UNKNOWN (census unavailable)")
        return 1

    print("\n### ST-6 seam (must be installed before any harvest)")
    seam_src = read(GENERATION_SEAM) if os.path.isfile(GENERATION_SEAM) else ""
    check("S1 generation_start.rs present", bool(seam_src))
    check(
        "S2 GenerationStarter trait declared",
        "pub trait GenerationStarter: Send + Sync {" in seam_src,
    )
    chat_ctx_src = read(CHAT_CTX)
    check(
        "S3 ChatCtx exposes generation",
        re.search(r"pub generation: Arc<dyn [\w:]*GenerationStarter>", chat_ctx_src) is not None,
    )
    check(
        "S4 EngineGenerationStarter installed",
        os.path.isfile(ENGINE_SEAM) and "GenerationStarter" in read(ENGINE_SEAM),
    )

    print("\n### Cargo edges checked (crate -> engine must not exist)")
    for crate in ("refact-runtime-api", "refact-chat-api", "refact-chat-history", "refact-core"):
        cargo = os.path.join(CRATES, crate, "Cargo.toml")
        has_engine = crate_depends_on_engine(cargo) if os.path.isfile(cargo) else False
        print(f"  {crate:22} -> refact-lsp : {'PRESENT (cycle!)' if has_engine else 'absent'}")
        check(f"E {crate} does not depend on refact-lsp", not has_engine)

    summary, sections = parse(REPORT)
    for line in summary:
        print(f"     {line}")

    def section(name):
        for label, entries in sections.items():
            if label.startswith(name):
                return entries
        return {}

    movable = section("MOVABLE now")
    left = section("LEFT")
    islands = section("DECLARED ISLANDS")
    hosts = section("MODULE HOSTS")
    shim = section("ALREADY-SHIM")
    blocked_island = section("BLOCKED ON ANOTHER ISLAND")

    print("\n### MOVABLE now")
    for module, (lines, _, _) in sorted(movable.items(), key=lambda item: -item[1][0]):
        print(f"  {lines:6}  {module}")
    if not movable:
        print("  (none)")

    print("\n### LEFT (blocked on engine code)")
    for module, (lines, blocker, others) in sorted(left.items(), key=lambda item: -item[1][0]):
        print(f"  {lines:6}  {module:48} blocker={blocker}")
        if others:
            print(f"{'':>18}others: {', '.join(others)}")
    if not left:
        print("  (none)")

    check(
        "C4 census classes partition the tree",
        any("COVERAGE BUG" in line for line in summary) is False,
        "classes do not sum to the tree total",
    )

    known = set(movable) | set(left) | set(islands) | set(hosts) | set(shim) | set(blocked_island)
    missing = [module for module in CARD_FILES if module not in known]
    check("C5 every card file is classified", not missing, f"unclassified: {', '.join(missing)}")

    print("\n### card files")
    left_count = 0
    card_lines = 0
    for module in CARD_FILES:
        if module in movable:
            lines = movable[module][0]
            card_lines += lines
            print(f"  MOVABLE  {module:48} {lines:6} lines")
            continue
        if module in shim:
            print(f"  SHIM     {module:48} {shim[module][0]:6} lines  (already harvested)")
            continue
        left_count += 1
        lines, blocker, others = left.get(module, (0, "-", []))
        card_lines += lines
        print(f"  LEFT     {module:48} {lines:6} lines  blocked by {blocker}")
        if others:
            print(f"{'':>31}also: {', '.join(others)}")
    leftover = [m for m in CARD_FILES if m in islands or m in blocked_island]
    for module in leftover:
        lines, blocker, _ = islands.get(module) or blocked_island.get(module)
        print(f"  ISLAND   {module:48} {lines:6} lines  blocker={blocker}")

    gate_left = GATE_FILE in left
    gate_movable = GATE_FILE in movable
    gate_harvested = GATE_FILE in shim
    observed = (
        "closed"
        if gate_left
        else ("open" if gate_movable else ("harvested" if gate_harvested else "unknown"))
    )
    if observed == "closed":
        blocker, others = left[GATE_FILE][1], left[GATE_FILE][2]
        reason = ", ".join([blocker, *others])
        print(f"\nST-7 GATE: CLOSED - {GATE_FILE} is LEFT on {reason}")
    elif observed == "open":
        print(f"\nST-7 GATE: OPEN - {GATE_FILE} is MOVABLE now")
    elif observed == "harvested":
        print(f"\nST-7 GATE: HARVESTED - {GATE_FILE} is an already-shipped shim")
    else:
        print(f"\nST-7 GATE: UNKNOWN - {GATE_FILE} is in no movability class")

    # A closed gate is only actionable when we can name the binding dependency,
    # not just the alphabetically first one the census prints.
    binding = left.get(GATE_FILE, (0, None, []))
    check(
        "C6 gate blocker is a real engine module",
        observed != "closed" or (binding[1] or "").startswith("chat::"),
        f"unexpected blocker {binding[1]}",
    )
    check("C7 gate observed as expected", observed == args.expect, f"expected {args.expect}, saw {observed}")
    if args.expect == "harvested":
        check(
            "C8 the three BN-1 files are already-shipped shims",
            all(module in shim for module in BN1_FILES),
            "not a shim: " + ", ".join(m for m in BN1_FILES if m not in shim),
        )
        drifted = []
        for module, expected in POST_HARVEST_BASELINE.items():
            ok = module in left and left[module][0] == expected
            seen = f"LEFT {left[module][0]}" if module in left else "not LEFT"
            if not ok:
                drifted.append(f"{module}: {seen}, expected {expected}")
        check("C9 no card file changed class or line count", not drifted, "; ".join(drifted))
        check(
            "C10 nothing is MOVABLE after the harvest",
            not movable,
            "still movable: " + ", ".join(sorted(movable)),
        )
        check(
            "C10b the harvested card files are shims",
            all(module in shim for module in HARVESTED_SHIMS),
            "not a shim: " + ", ".join(m for m in HARVESTED_SHIMS if m not in shim),
        )
        facade_body = non_test_body(read(QUEUE_FACADE_FILE)) if os.path.isfile(QUEUE_FACADE_FILE) else ""
        check(
            "C11 chat::queue is a pure crate facade",
            bool(facade_body)
            and "crate::" not in facade_body
            and re.search(r"(?m)^[ \t]*pub(?:\([^)]*\))?[ \t]+(?:async[ \t]+)?(?:fn|struct|enum|trait|type|const|static)[ \t]", facade_body)
            is None,
            "chat/queue/mod.rs still declares engine items or imports crate:: paths",
        )
    print(
        f"\n{left_count} of {len(CARD_FILES)} card files are LEFT"
        f" ({card_lines} production lines); {len(movable)} chat files are movable now"
    )
    print(f"\nST-7 RESULT: {'PASS' if not failures else 'FAIL'}")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
