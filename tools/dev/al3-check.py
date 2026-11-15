"""AL-3 in-repo verification gate and worth-it table generator.

    python tools/dev/al3-check.py

Read-only, deterministic, no Rust compilation (the compile gate remains
`tools/dev/engine-build.ps1 -Mode check`).

It answers the two questions the card asks, from the source text and the crate
graph, using `tools/dev/al3_edges.py` (a source-derived edge resolver that fixes
five census blind spots -- see that file's docstring):

1. **The `BLOCKED on a destination-crate cycle` line, per module, before and
   after.** The census only knows one destination (`DEST_HINTS = {"tools":
   "refact-tools"}`); every other tree prints "destination crate unknown, not
   checked". This harness evaluates every candidate destination with the census's
   own `cycle_blocker()` rule, restricted to files that are engine-free *and*
   legal for at least one destination (a file with engine edges is blocked by
   code, not by a crate cycle, and a file blocked by neither is simply movable).
2. **The worth-it / needs-a-seam / not-worth-it table** for `buddy`,
   `integrations` and `providers`, with the released line count per group. A file
   whose blockers are all themselves movable is a *chain* file: it needs no new
   seam, only the files in front of it to move.

Assertions are derived from the same computation, so the table cannot drift from
the numbers.
"""
import os
import sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "tools", "dev"))

import al3_edges as A  # noqa: E402

TREES = ("tools", "buddy", "integrations", "providers", "http")
# Candidate landing crates per tree, in preference order. The first is the tree's
# own crate; `refact-http` / `refact-runtime-api` are the two sinks the earlier
# cards (U-8, Y-2, AB-3) already used for axum/ChatCtx-bound engine files.
DEST_CANDIDATES = {
    "tools": ("refact-tools",),
    "buddy": ("refact-buddy-core", "refact-runtime-api"),
    "integrations": ("refact-integrations", "refact-runtime-api"),
    # `refact-http` is the landing crate Y-2/U-8 already documented for the
    # axum/shared-runtime half of this tree, so it must be a candidate here too;
    # without it those files look cycle-blocked against every other candidate.
    "providers": ("refact-providers", "refact-http", "refact-runtime-api"),
    "http": ("refact-http",),
}
SHIM_LABEL = "already-shim (pure crate facade)"
FREE_LABEL = "MOVABLE now"
CYCLE_LABEL = "BLOCKED on a destination-crate cycle"

checks = []


def check(ok, label, detail=""):
    checks.append((bool(ok), label, detail))


def canonical(tree, path, here):
    """The file of `tree` that `path` names, or None when it is not in the tree."""
    parts = [p for p in path.split("::") if p]
    base = ""
    if parts and parts[0] == "super":
        base = here
        while parts and parts[0] == "super":
            base = base.rsplit("::", 1)[0] if "::" in base else ""
            parts = parts[1:]
    elif parts and parts[0] == "crate":
        parts = parts[1:]
    for k in range(len(parts), 0, -1):
        cand = "::".join(parts[:k])
        full = (base + "::" + cand) if base else cand
        if full == tree or full.startswith(tree + "::"):
            return full
    return None


def legal(c, crates, dest):
    """No crate edge of this file already depends on `dest` (else cargo cycles)."""
    return not any(
        x != dest and dest in A.closure(c, x.replace("_", "-"))
        for x in crates
    )


def analyse(tree):
    A._BIND.clear()
    A._READ.clear()
    c = A.load(tree)
    testish = A.cfg_test_modules(c)
    files = {}
    for label in A.GROUPS + (SHIM_LABEL,):
        for mod in c.classes.get(label, []):
            if mod in testish:
                continue
            crates, engine = A.edges(c, mod)
            inside, outside = set(), set()
            for e in engine:
                canon = canonical(tree, e, mod)
                if canon is None:
                    outside.add(e)
                elif canon != mod:
                    inside.add(canon)
            dests = DEST_CANDIDATES[tree]
            files[mod] = {
                "lines": c.info[mod]["lines"],
                "crates": crates,
                "inside": inside,
                "outside": outside,
                "shim": label == SHIM_LABEL,
                "class": label,
                "free_for": [d for d in dests if not engine and legal(c, crates, d)],
                "cycle_for": [
                    d for d in dests if not engine and not legal(c, crates, d)
                ],
            }
    return c, files


def cycle_blocked_files(files):
    """Engine-free files with no legal destination at all."""
    return {
        m: d
        for m, d in files.items()
        if not d["free_for"] and d["cycle_for"] and not d["shim"]
    }


def cycle_for_dest(files, dest):
    """Engine-free files that cannot land in `dest` (that crate sits above them)."""
    return {
        m: d
        for m, d in files.items()
        if not d["shim"] and dest in d["cycle_for"]
    }


print("=" * 78)
print("A. `BLOCKED on a destination-crate cycle`, per module")
print("=" * 78)
cycle_report = {}
for tree in TREES:
    c, files = analyse(tree)
    dest = c.DEST
    census_line = "%s files / %d lines" % (
        len(c.crate_cycles),
        sum(c.info[m]["lines"] for m in c.crate_cycles),
    )
    print(
        "%-14s census (dest=%s): %s"
        % (tree, dest or "unknown", census_line if dest else "not checked (0 / 0)")
    )
    per_dest = {}
    for dst in DEST_CANDIDATES[tree]:
        hit = cycle_for_dest(files, dst)
        per_dest[dst] = hit
        print(
            "               dest=%-22s cycle-blocked %2d files / %5d lines"
            % (dst, len(hit), sum(files[m]["lines"] for m in hit))
        )
        for mod in sorted(hit, key=lambda m: -files[m]["lines"])[:5]:
            print("                   %6d  %-44s" % (files[mod]["lines"], mod))
    residual = cycle_blocked_files(files)
    print(
        "               RESIDUAL cycle-blocked (no candidate crate works): %d files / %d lines"
        % (len(residual), sum(files[m]["lines"] for m in residual))
    )
    cycle_report[tree] = per_dest

check(
    not cycle_report["tools"]["refact-tools"],
    "tools has no destination-crate cycle (AB-4 shape still holds)",
    str(sorted(cycle_report["tools"]["refact-tools"]))[:200],
)
check(
    not cycle_report["http"]["refact-http"],
    "http has no destination-crate cycle",
    str(sorted(cycle_report["http"]["refact-http"]))[:200],
)
for tree, dest in (("buddy", "refact-buddy-core"), ("providers", "refact-providers")):
    check(
        bool(cycle_report[tree][dest]),
        "%s names a latent cycle against its own crate %s" % (tree, dest),
        str(sorted(cycle_report[tree][dest]))[:200],
    )
check(
    not cycle_report["integrations"]["refact-integrations"],
    "integrations has no cycle against its own crate",
    str(sorted(cycle_report["integrations"]["refact-integrations"]))[:200],
)
check(
    bool(cycle_report["providers"]["refact-providers"]),
    "providers names a latent cycle against its own crate refact-providers",
    str(sorted(cycle_report["providers"]["refact-providers"]))[:200],
)
for tree in TREES:
    residual = cycle_blocked_files(analyse(tree)[1])
    check(
        not residual,
        "%s has no file that is cycle-blocked for every candidate destination" % tree,
        str(sorted(residual))[:200],
    )

print()
print("=" * 78)
print("B. worth-it / needs-a-seam / not-worth-it table")
print("=" * 78)
table = {}
for tree in ("buddy", "integrations", "providers"):
    c, files = analyse(tree)

    def movable(mod, seen=None):
        """A file leaves the engine once it is engine-free, has a legal landing,
        and every in-tree blocker of it does too."""
        seen = set() if seen is None else seen
        if mod in seen:
            return False
        seen.add(mod)
        d = files[mod]
        if d["outside"] or not d["free_for"]:
            return False
        return all(movable(inner, seen) for inner in d["inside"])

    for mod, d in files.items():
        d["movable"] = movable(mod)

    blocked = {m: d for m, d in files.items() if not d["shim"] and not d["movable"]}
    chain = {
        m
        for m, d in blocked.items()
        if not d["outside"] and all(files.get(s, {}).get("movable") for s in d["inside"])
    }
    seam = {m: d for m, d in blocked.items() if m not in chain}
    table[tree] = {"files": files, "blocked": blocked, "chain": chain, "seam": seam}
    own_crate = DEST_CANDIDATES[tree][0]
    free_own = [
        m
        for m in files
        if files[m]["movable"] and not files[m]["shim"] and own_crate in files[m]["free_for"]
    ]
    free_alt = [
        m
        for m in files
        if files[m]["movable"] and not files[m]["shim"] and own_crate not in files[m]["free_for"]
    ]
    print(
        "### %-13s production %2d files; no seam needed %2d / %5d lines "
        "(own crate %2d / %5d, alternate crate %2d / %5d); "
        "chain-only %2d / %5d lines; needs a seam %2d / %5d lines"
        % (
            tree,
            len([m for m in files if not files[m]["shim"]]),
            len(free_own) + len(free_alt),
            sum(files[m]["lines"] for m in free_own + free_alt),
            len(free_own),
            sum(files[m]["lines"] for m in free_own),
            len(free_alt),
            sum(files[m]["lines"] for m in free_alt),
            len(chain),
            sum(blocked[m]["lines"] for m in chain),
            len(seam),
            sum(d["lines"] for d in seam.values()),
        )
    )
    print("    no seam needed, own crate works:")
    for m in sorted(free_own, key=lambda m: -files[m]["lines"]):
        print("      %6d  %s" % (files[m]["lines"], m))
    print("    no seam needed, must land in an alternate crate:")
    for m in sorted(free_alt, key=lambda m: -files[m]["lines"]):
        print(
            "      %6d  %-46s free_for=%s"
            % (files[m]["lines"], m, ",".join(files[m]["free_for"]))
        )
    print("    chain-only (blockers are themselves movable):")
    for m in sorted(chain, key=lambda m: -blocked[m]["lines"]):
        print(
            "      %6d  %-46s via %s"
            % (blocked[m]["lines"], m, ",".join(sorted(blocked[m]["inside"])))
        )
    per_symbol = defaultdict(lambda: [0, 0])
    for m in blocked:
        for sym in blocked[m]["inside"]:
            if files.get(sym, {}).get("movable"):
                continue
            per_symbol[sym][0] += 1
            per_symbol[sym][1] += blocked[m]["lines"]
    print("    in-tree blockers that are themselves stuck (>=120 lines):")
    for sym, (n, lines) in sorted(per_symbol.items(), key=lambda kv: -kv[1][1]):
        if lines < 120:
            continue
        print(
            "      %5d lines  %2d files  %-44s shim=%s"
            % (lines, n, sym, files.get(sym, {}).get("shim"))
        )
    per_out = defaultdict(lambda: [0, 0])
    for m in seam:
        for sym in seam[m]["outside"]:
            per_out[sym][0] += 1
            per_out[sym][1] += seam[m]["lines"]
    print("    out-of-tree seams that release >=120 lines:")
    if not per_out:
        print("      (none -- every blocker is inside the tree)")
    for sym, (n, lines) in sorted(per_out.items(), key=lambda kv: -kv[1][1]):
        if lines < 120:
            continue
        print("      %5d lines  %2d files  %s" % (lines, n, sym))
    print("    stuck files, ranked:")
    for m in sorted(seam, key=lambda m: -seam[m]["lines"]):
        d = seam[m]
        print("      %6d  %-46s" % (d["lines"], m))
        if d["inside"]:
            print(
                "              in-tree: %s"
                % ", ".join(sorted(d["inside"]))
            )
        if d["outside"]:
            print("              seam:    %s" % ", ".join(sorted(d["outside"])))
    print()

# The table must be internally consistent: every non-movable file is in exactly
# one bucket, and bucketed lines add up to the blocked total.
for tree, t in table.items():
    check(
        not (set(t["chain"]) & set(t["seam"])),
        "%s: no file is both chain-only and seam-blocked" % tree,
    )
    check(
        sum(t["blocked"][m]["lines"] for m in t["chain"])
        + sum(t["blocked"][m]["lines"] for m in t["seam"])
        == sum(d["lines"] for d in t["blocked"].values()),
        "%s: the two buckets add up to the blocked line total" % tree,
    )

# Named expectations, so a silent drift in the analysis fails the gate.
check(
    table["integrations"]["files"]["integrations::mcp::mcp_sampling"]["outside"]
    == {"subchat::runner"},
    "integrations::mcp::mcp_sampling is blocked only by the subchat seam",
    str(table["integrations"]["files"]["integrations::mcp::mcp_sampling"]["outside"]),
)
check(
    "integrations::mcp::session_mcp" in table["integrations"]["seam"],
    "integrations::mcp::session_mcp is blocked by the MCP cluster itself",
)
check(
    table["providers"]["files"]["providers::oauth_refresh"]["free_for"]
    == ["refact-http", "refact-runtime-api"],
    "providers::oauth_refresh can land in refact-http or refact-runtime-api",
    str(table["providers"]["files"]["providers::oauth_refresh"]["free_for"]),
)
check(
    table["providers"]["files"]["providers::http::quota_cache"]["free_for"]
    == ["refact-http"],
    "providers::http::quota_cache can only land in refact-http",
    str(table["providers"]["files"]["providers::http::quota_cache"]["free_for"]),
)
check(
    table["buddy"]["files"]["buddy::jobs::autonomous_chats::local_jobs"]["outside"]
    == set(),
    "buddy::jobs::autonomous_chats::local_jobs has no out-of-tree seam",
    str(
        table["buddy"]["files"]["buddy::jobs::autonomous_chats::local_jobs"]["outside"]
    ),
)

failed = [c for c in checks if not c[0]]
for ok, label, detail in checks:
    if not ok:
        print("FAIL: %s" % label + (" (%s)" % detail if detail else ""))
print(
    "VERIFY: %s (%d/%d checks)"
    % ("PASS" if not failed else "FAIL", len(checks) - len(failed), len(checks))
)
sys.exit(1 if failed else 0)
