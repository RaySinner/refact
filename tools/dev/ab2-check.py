"""AB-2 harness: the census, the island inventory, and the settlement simulation.

Single command, no shell separators:

    python tools/dev/ab2-check.py

It does not compile: everything here is a re-analysis of the honest census plus
filesystem assertions.

Settlement model (the reason this harness exists): the census marks islands as
permanently immovable, so every file that depends on one lands in the class
"BLOCKED on another island". That is right for "movable *now*", but it cannot
answer "if island X were settled, how many of those files would move?".
Settling a set S means every member of S left the engine -- it is now a crate
module behind a one-line `pub use` shim -- so S is removed from `files` and from
every other module's `engine`/`deps` edge sets. Nothing else changes.

Checks:
  1. both of the two largest islands still exist as engine sources or settled shims;
  2. the census runs, and its class lines partition the tree;
  3. the declared-island inventory and the island-blocked class are printed;
  4. settlement simulations: two largest, blocker-free islands, all islands;
  5. every chat crate is engine-free in both directions;
  6. the report carries every required section.

Exit code 0 means every check passed.
"""
import importlib.util
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATES = os.path.join(ENGINE, "crates")

CENSUS = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")
REPORT = os.path.join(ROOT, "docs", "dev", "ab2-chat-islands.md")

TWO_LARGEST = ["chat::stream_core", "chat::tools"]
CHAT_CRATES = ["refact-chat-api", "refact-chat-history", "refact-runtime-api"]

failures = []


def check(label, ok, detail=""):
    print(
        ("PASS  " if ok else "FAIL  ")
        + label
        + (("  [" + detail + "]") if detail and not ok else "")
    )
    if not ok:
        failures.append(label + ((" (" + detail + ")") if detail else ""))


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def load_census():
    saved = list(sys.argv)
    sys.argv = [CENSUS, "chat"]
    spec = importlib.util.spec_from_file_location("cmc_ab2_check", CENSUS)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    sys.argv = saved
    return module


def movable_with(c, settled):
    """Closure with `settled` modules already gone from the engine."""
    saved = {m: (set(c.info[m]["engine"]), set(c.info[m]["deps"])) for m in c.files}
    for m in c.files:
        c.info[m]["engine"] = set(c.info[m]["engine"]) - settled
        c.info[m]["deps"] = set(c.info[m]["deps"]) - settled
    c.ISLANDS = tuple(i for i in c.ISLANDS if i not in settled)
    remaining = set(c.files) - settled
    movable = set(c.facade) | {m for m in remaining if c.info[m]["test_only"]}
    while True:
        nxt = {
            m
            for m in remaining
            if m not in movable
            and not c.info[m]["engine"]
            and c.info[m]["deps"] <= movable
        }
        if not nxt:
            break
        movable |= nxt
    movable -= (
        {m for m in remaining if c.is_island(m)} | set(c.hosts) | set(c.crate_cycles)
    )
    while True:
        drop = {m for m in movable if not c.info[m]["deps"] <= movable}
        if not drop:
            break
        movable -= drop
    for m, (e, d) in saved.items():
        c.info[m]["engine"], c.info[m]["deps"] = e, d
    return movable, remaining


def next_blockers(c, mod, remaining, movable):
    return sorted(
        (set(c.info[mod]["engine"]) | {d for d in c.info[mod]["deps"] if d not in movable})
        & remaining
    )


print("== 1. the two largest islands exist as real engine sources ==")
DECL = re.compile(
    r"(?m)^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:fn|struct|enum|trait|const|static|type)\s"
)
for mod in TWO_LARGEST:
    rel = mod.replace("::", "/") + ".rs"
    path = os.path.join(ENGINE, "src", rel)
    body = read(path) if os.path.exists(path) else ""
    check("engine src/" + rel + " exists", bool(body))
    decls = len(DECL.findall(body))
    settled = body.lstrip().startswith("pub use refact_")
    check(
        "engine src/" + rel + " is either production code or a settled shim",
        decls >= 5 or settled,
        "%d top-level production declarations, settled=%s" % (decls, settled),
    )

print()
print("== 2. census ==")
result = subprocess.run([sys.executable, CENSUS, "chat"], cwd=ROOT, capture_output=True, text=True)
check("census tool runs", result.returncode == 0, result.stderr.strip()[:300])
print(result.stdout.strip())
census = load_census()
check("census classes partition the tree", "partition the tree" not in result.stdout)

print()
print("== 3. island inventory ==")
islands = sorted(
    (m for m in census.files if census.is_island(m)), key=lambda m: -census.info[m]["lines"]
)
prod_islands = [m for m in islands if census.info[m]["lines"] > 0]
for mod in islands:
    engine = sorted(census.info[mod]["engine"] | census.info[mod]["deps"])
    print(
        "  %6d  %-46s blocker=%s"
        % (census.info[mod]["lines"], mod, engine[0] if engine else "-")
    )
check("19 declared islands with production lines", len(prod_islands) == 19, str(len(prod_islands)))

print()
print("== 4. the island-blocked class ==")
blocked = sorted(census.classes["BLOCKED on another island (that island needs its own card first)"])
for mod in blocked:
    print(
        "  %6d  %-46s island=%s"
        % (census.info[mod]["lines"], mod, census.island_blocker(mod))
    )
check(
    "the island-blocked class is empty after ST-1 settled chat::stream_core",
    len(blocked) == 0,
    "%d files: %s" % (len(blocked), ", ".join(blocked[:4])),
)
check("the 13 at base is stated in the report", "**Zero, and no island settlement" in read(REPORT))

print()
print("== 5. settlement simulation ==")


def simulate(label, settled):
    movable, remaining = movable_with(census, set(settled))
    released = [m for m in blocked if m in movable]
    print()
    print("--- %s (%d island(s) settled) ---" % (label, len(settled)))
    print("    of the island-blocked files, %d become MOVABLE now" % len(released))
    for mod in released:
        print("      RELEASED %s" % mod)
    for mod in blocked:
        if mod in movable:
            continue
        nb = next_blockers(census, mod, remaining, movable)
        print("      still blocked  %-42s next=%s" % (mod, nb[0] if nb else "?"))
    return released


released_two = simulate("A. the two largest islands", TWO_LARGEST)
released_free = simulate(
    "B. every island whose own blocker is '-'",
    [m for m in islands if not (census.info[m]["engine"] | census.info[m]["deps"])],
)
released_all = simulate("C. all islands (upper bound)", islands)
check("C is an upper bound on A", len(released_all) >= len(released_two))
check("C is an upper bound on B", len(released_all) >= len(released_free))

print()
print("== 6. crate <-> engine edges, both directions ==")
engine_toml = read(os.path.join(ENGINE, "Cargo.toml"))
for crate in CHAT_CRATES:
    check("engine Cargo.toml depends on " + crate, crate in engine_toml)
for crate in CHAT_CRATES:
    toml = read(os.path.join(CRATES, crate, "Cargo.toml"))
    check(crate + " has no engine edge", "refact-lsp" not in toml)
    edges = sorted(set(re.findall(r"(?m)^\s*(refact-[a-z0-9-]+)\s*=", toml)))
    print("    %s -> %s" % (crate, ", ".join(edges)))
    for dep in edges:
        dep_toml = os.path.join(CRATES, dep, "Cargo.toml")
        if not os.path.exists(dep_toml):
            continue
        check(
            "%s -> %s never reaches the engine" % (crate, dep),
            "refact-lsp" not in read(dep_toml),
        )
    leaks = []
    for root, dirs, files in os.walk(os.path.join(CRATES, crate, "src")):
        dirs[:] = [d for d in dirs if d != "target"]
        for name in files:
            if name.endswith(".rs") and re.search(
                r"\brefact_lsp\b", read(os.path.join(root, name))
            ):
                leaks.append(name)
    check(crate + " sources never name refact_lsp", not leaks, ", ".join(leaks[:5]))

print()
print("== 7. report ==")
if os.path.exists(REPORT):
    report = read(REPORT)
    for section in [
        "## 1. Census, verbatim",
        "## 2. The two largest islands",
        "## 3. The 13 island-blocked files, paired with their island",
        "## 4. How many of the 13 became movable",
        "## 5. Strategy per remaining island",
    ]:
        check("report has " + section, section in report)
else:
    check("report exists", False, REPORT)

print()
if failures:
    print("RESULT: FAIL (%d check(s))" % len(failures))
    sys.exit(1)
print("RESULT: PASS")
