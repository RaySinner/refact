"""Z-3 http blocker assault harness.

Re-uses the authoritative census (`tools/dev/chat_movable_census.py`) so every
number here comes from the same resolver the cards are planned against, then:

* prints the census' own tables VERBATIM for the `http` module,
* prints the full engine-resident edge list per blocked file (module + symbol)
  instead of the census' truncated `blocker=` column,
* aggregates blocked production lines per blocker symbol,
* asserts the Z-3 post-conditions (retargeted symbols gone, harvested files
  present in refact-http, no crate -> engine dependency).

Run: python tools/dev/z3-check.py
No shell separators are used anywhere; every check is python-side.
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
CENSUS = os.path.join(HERE, "chat_movable_census.py")
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENGINE, "src")
HTTP_CRATE = os.path.join(ENGINE, "crates", "refact-http")

TOP5 = [
    "http::routers::v1::mcp_marketplace",
    "http::routers::v1::tasks",
    "http::routers::v1::buddy",
    "http::routers::v1",
    "http::routers::v1::buddy_opportunities",
]

# Files harvested by this card into refact-http (engine file -> crate file).
HARVESTED = {
    "http::routers::v1::mcp_marketplace": "src/routers/v1/mcp_marketplace.rs",
}

# Top-5 blockers by blocked line count: chosen strategy | host crate | blocker symbols |
# the precondition any later card must satisfy before the file can move.
STRATEGY_TABLE = [
    (
        "http::routers::v1::mcp_marketplace",
        "pure (retarget to crate-owned symbols)",
        "refact-http",
        "integrations::mcp::mcp_resources (only engine edge)",
        "none — done in this card",
    ),
    (
        "http::routers::v1::tasks",
        "needs a seam (tasks::* wrappers move onto refact_runtime_api + refact_tasks)",
        "refact-http",
        "tasks::comments, tasks::events, tasks::storage",
        "tasks/{comments,events,storage}.rs stay engine-resident because they wrap "
        "Arc<GlobalContext>; retarget handler call sites to the crate fns first",
    ),
    (
        "http::routers::v1::buddy",
        "needs a seam (buddy::speech bodies must move to refact-buddy-core/runtime-api)",
        "refact-http",
        "buddy::diagnostics, buddy::speech",
        "buddy::speech::render_buddy_* bodies (271 lines) are engine-resident; "
        "buddy::diagnostics is a facade but written as a named re-export, not a glob",
    ),
    (
        "http::routers::v1",
        "gcx-shaped (router table — always last)",
        "refact-http",
        "chat (engine island) + 24 not-yet-moved subrouters",
        "every subrouter it mounts must be crate-side first",
    ),
    (
        "http::routers::v1::buddy_opportunities",
        "needs a seam (buddy::voice_service body) + already-cleared mcp_marketplace edge",
        "refact-http",
        "buddy::speech, buddy::voice_service::QuestAccept",
        "buddy::voice_service is a 1060-line engine module; SpeechIntent already lives in "
        "refact-buddy-core, but the rendering body does not",
    ),
]

# `crate::`/`super::` paths that must no longer appear in the harvested file.
RETARGET_MUST_BE_GONE = [
    "crate::integrations::mcp::mcp_resources::remove_indexed_resources",
]


def crate_dependency_graph():
    """Parse every crate Cargo.toml into refact-* -> refact-* edges (DRY: one source)."""
    crates_dir = os.path.join(ENGINE, "crates")
    graph = {}
    for name in sorted(os.listdir(crates_dir)):
        manifest = os.path.join(crates_dir, name, "Cargo.toml")
        if not os.path.exists(manifest):
            continue
        deps = set()
        with open(manifest, encoding="utf-8") as fh:
            for line in fh:
                m = re.match(r"\s*(refact-[a-z0-9-]+)\s*=", line)
                if m:
                    deps.add(m.group(1))
        graph[name] = deps
    return graph


def reaches_refact_lsp(host, graph):
    """True when `host` (transitively) depends on refact-lsp — that would be a cycle."""
    seen, queue = set(), [host]
    while queue:
        crate = queue.pop()
        for dep in graph.get(crate, ()):
            if dep == "refact-lsp":
                return True, crate
            if dep not in seen:
                seen.add(dep)
                queue.append(dep)
    return False, None


def load_census():
    saved_argv = sys.argv
    sys.argv = ["chat_movable_census.py", "http"]
    g = {"__name__": "census_http", "__file__": CENSUS}
    try:
        with open(CENSUS, encoding="utf-8") as fh:
            src = fh.read()
        exec(compile(src, CENSUS, "exec"), g)
    finally:
        sys.argv = saved_argv
    return g
    return g


def per_file_engine_edges(g):
    """Replicate the census `add_edge` closure but keep per-symbol detail."""
    files = g["files"]
    info = g["info"]
    out = {}
    for mod in files:
        if info[mod]["test_only"]:
            continue
        body = g["code"](g["prod"](g["read"](g["mods"][mod])))
        engine = set()
        crates = set()
        deps = set()

        def add_edge(ref, engine=engine, crates=crates, deps=deps, mod=mod):
            target = g["absolute"](mod, ref)
            tmod, rest = g["longest_module"](target)
            if tmod is None or tmod == mod:
                return
            star = (not rest) or rest[-1] == "*"
            if star and tmod not in g["wild"]:
                if tmod in files:
                    deps.add(tmod)
                else:
                    engine.add(tmod)
                return
            kind, value = g["symbol_origin"](tmod, None if star else rest[-1])
            if kind == "crate":
                crates.add(value)
                return
            if value in files and value != mod:
                deps.add(value)
            else:
                engine.add(value)

        for _is_pub, full in g["raw_uses"].get(mod, []):
            if g["CRATE_RE"].match(full):
                crates.add(full.split("::")[0])
        for ref in set(g["REF_RE"].findall(body)):
            add_edge(re.sub(r"\s+", "", ref))
        out[mod] = {"engine": sorted(engine), "crates": sorted(crates), "deps": sorted(deps)}
    return out


def main():
    g = load_census()
    edges = per_file_engine_edges(g)
    info = g["info"]

    dest = os.path.join(ENGINE, "target", "http_movable_census.txt")
    with open(dest, encoding="utf-8") as fh:
        tables = fh.read()
    print("=== census tables (verbatim) ===")
    print(tables, end="" if tables.endswith("\n") else "\n")

    print("=== engine edges per top-5 blocked file ===")
    for mod in TOP5:
        row = edges.get(mod, {"engine": [], "crates": [], "deps": []})
        print("%s  (%d prod lines)" % (mod, info.get(mod, {}).get("lines", 0)))
        for sym in row["engine"]:
            print("    engine  %s" % sym)
        for dep in row["deps"]:
            print("    http    %s" % dep)
        for crate in row["crates"]:
            print("    crate   %s" % crate)

    print("=== aggregate blocked prod lines per blocker symbol ===")
    agg = {}
    for mod, row in edges.items():
        if mod not in g["files"]:
            continue
        if mod in g["movable"]:
            continue
        lines = info[mod]["lines"]
        for sym in row["engine"]:
            agg[sym.split("::")[0] if False else sym] = agg.get(sym, 0) + lines
    for sym, lines in sorted(agg.items(), key=lambda kv: -kv[1]):
        print("%8d  %s" % (lines, sym))

    failures = []

    print("=== harvest assertions ===")
    for mod, crate_rel in HARVESTED.items():
        crate_file = os.path.join(HTTP_CRATE, crate_rel.replace("/", os.sep))
        engine_file = g["mods"][mod]
        ok = os.path.exists(crate_file)
        print("%-55s crate=%s" % (mod, "yes" if ok else "MISSING"))
        if not ok:
            failures.append("%s not present in refact-http" % crate_rel)
            continue
        with open(engine_file, encoding="utf-8") as fh:
            engine_text = fh.read()
        prod = g["code"](g["prod"](engine_text))
        is_shim = "pub use refact_http::" in prod
        print("    engine file is a refact_http shim: %s" % is_shim)
        if not is_shim:
            failures.append("%s engine file is not a refact_http shim" % mod)
        with open(crate_file, encoding="utf-8") as fh:
            crate_text = fh.read()
        for needle in RETARGET_MUST_BE_GONE:
            if needle in crate_text:
                failures.append("%s still references %s" % (crate_rel, needle))
                print("    STILL REFERENCED: %s" % needle)
        for needle, label in (
            ("refact_lsp", "engine crate name"),
            ("crate::integrations", "engine module"),
            ("crate::app_state", "engine module"),
            ("crate::custom_error", "engine module"),
            ("crate::global_context", "engine module"),
            ("crate::http", "engine module"),
        ):
            if needle in g["code"](g["prod"](crate_text)):
                failures.append("%s contains %s (%s)" % (crate_rel, needle, label))
                print("    FOUND forbidden token: %s (%s)" % (needle, label))

    cargo = os.path.join(HTTP_CRATE, "Cargo.toml")
    with open(cargo, encoding="utf-8") as fh:
        cargo_text = fh.read()
    if "refact-lsp" in cargo_text:
        failures.append("refact-http/Cargo.toml mentions refact-lsp")
    print("refact-http/Cargo.toml mentions refact-lsp: %s" % ("refact-lsp" in cargo_text))

    print("=== top-5 blocker strategies + cycle check ===")
    cargo_deps = crate_dependency_graph()
    for row in STRATEGY_TABLE:
        mod, strategy, host, symbols, precond = row
        reach, path = reaches_refact_lsp(host, cargo_deps)
        verdict = "CYCLE" if reach else "ok"
        print("%-45s %-34s %-26s %s" % (mod.split("::")[-1], strategy, host, verdict))
        print("      symbols: %s" % symbols)
        print("      precondition: %s" % precond)
    print("=== fresh grouped blocker table (remainder) ===")
    groups = {}
    for mod, row in edges.items():
        if mod not in g["files"] or mod in g["movable"] or not row["engine"]:
            continue
        roots = sorted({s.split("::")[0] + "::" + s.split("::")[1] if s.count("::") >= 1 else s
                        for s in row["engine"]})
        key = ",".join(roots)
        groups.setdefault(key, []).append((info[mod]["lines"], mod))
    for key, members in sorted(groups.items(), key=lambda kv: -sum(m[0] for m in kv[1])):
        total = sum(m[0] for m in members)
        print("%8d  %2d file(s)  blocker-group=%s" % (total, len(members), key))
        for lines, mod in sorted(members, reverse=True)[:4]:
            print("              %6d  %s" % (lines, mod))

    print("=== result ===")
    if failures:
        for f in failures:
            print("FAIL: %s" % f)
        print("z3-check: %d failure(s)" % len(failures))
        return 1
    print("z3-check: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
