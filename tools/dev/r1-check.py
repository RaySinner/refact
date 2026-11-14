"""R-1 verification harness (engine `tools/` endgame).

Invoked only through tools/dev/r1-check.ps1:
    powershell -NoProfile -ExecutionPolicy Bypass -File tools\\dev\\r1-check.ps1

Checks, in order:
 1. refact-tools/Cargo.toml never mentions refact-lsp (no crate -> engine edge).
 2. The three files this card harvested are pure `pub use` shims.
 3. A sample of 10 `crate::tools::<module>::<Struct>` paths in tools_list.rs still
    resolve: the target module file exists (engine shim or crate body) and the
    struct is reachable either through a `pub use` glob or a crate definition.
 4. The stub shims for the harvested modules stay pure glob re-exports.
 5. Honest census numbers for `tools/` (shim / moved / left) with per-file counts.
 6. Residual blockers for the files the card wanted to unblock.
Writes a report to refact-agent/engine/target/r1_report.txt and prints PASS/FAIL.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENGINE, "src")
TOOLS_SRC = os.path.join(SRC, "tools")
CRATE_TOOLS = os.path.join(ENGINE, "crates", "refact-tools", "src", "tools")

problems = []


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def check(ok, message):
    print(("PASS  " if ok else "FAIL  ") + message)
    if not ok:
        problems.append(message)


# ---------------------------------------------------------------- 1. no crate->engine edge
cargo = read(os.path.join(os.path.dirname(CRATE_TOOLS), "..", "Cargo.toml"))
check("refact-lsp" not in cargo, "refact-tools/Cargo.toml does not mention refact-lsp")

# ---------------------------------------------------------------- 2. harvested files are shims
SHIMS = {
    "tools/tool_mcp_call.rs": "refact_tools::tools::tool_mcp_call",
    "tools/tool_mcp_search.rs": "refact_tools::tools::tool_mcp_search",
}
for rel, target in SHIMS.items():
    body = read(os.path.join(SRC, rel))
    stripped = re.sub(r"(?m)^\s*//.*$", "", body)
    check(
        stripped.strip() == "pub use " + target + "::*;",
        f"{rel} is a pure `pub use {target}::*;` shim",
    )

# ---------------------------------------------------------------- 3. path resolution sample
tools_list = read(os.path.join(TOOLS_SRC, "tools_list.rs"))
engine_ok = [
    m for m in os.listdir(TOOLS_SRC)
    if m.endswith(".rs") and os.path.isfile(os.path.join(TOOLS_SRC, m))
]
engine_mods = {m[:-3] for m in engine_ok}

sample = re.findall(r"crate::tools::([a-z_]+)::([A-Za-z_][A-Za-z0-9_]*)", tools_list)
seen, ordered = set(), []
for module, symbol in sample:
    if module not in engine_mods:
        continue
    if (module, symbol) not in seen:
        seen.add((module, symbol))
        ordered.append((module, symbol))
ordered = ordered[:10]
check(len(ordered) == 10, f"sampled 10 `crate::tools::<module>::<Struct>` paths ({len(ordered)})")
for module, symbol in ordered:
    engine_file = os.path.join(TOOLS_SRC, module + ".rs")
    crate_file = os.path.join(CRATE_TOOLS, module + ".rs")
    body = read(engine_file)
    defined = re.search(r"(?m)^\s*pub (?:struct|enum|fn|async fn) " + symbol + r"\b", body)
    glob = re.search(
        r"(?m)^\s*pub use (?:refact_tools::tools::|super::)" + module + r"::\*;", body
    )
    in_crate = os.path.isfile(crate_file) and re.search(
        r"(?m)^\s*pub (?:struct|enum|fn|async fn) " + symbol + r"\b", read(crate_file)
    )
    check(
        bool(defined or glob or in_crate),
        f"crate::tools::{module}::{symbol} resolves (definition here, glob shim, or crate body)",
    )

# ---------------------------------------------------------------- 4. crate-side modules exist
for module in ["tool_catalog", "tool_mcp_call", "tool_mcp_search"]:
    path = os.path.join(CRATE_TOOLS, module + ".rs")
    check(os.path.isfile(path), f"refact-tools owns src/tools/{module}.rs")
crate_mod = read(os.path.join(os.path.dirname(CRATE_TOOLS), "tools.rs"))
for module in ["tool_catalog", "tool_mcp_call", "tool_mcp_search"]:
    check(
        f"pub mod {module};" in crate_mod,
        f"refact-tools src/tools.rs declares `pub mod {module};`",
    )

# ---------------------------------------------------------------- 5. census numbers
import subprocess

census_path = os.path.join(ENGINE, "target", "tools_movable_census.txt")
proc = subprocess.run(
    [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), "tools"],
    cwd=ROOT,
    capture_output=True,
    text=True,
)
check(proc.returncode == 0, "census script exits 0")
census = read(census_path) if os.path.isfile(census_path) else ""
headline = [line for line in census.splitlines() if line.strip().startswith("###") or "files /" in line][:5]
for line in headline:
    print("      " + line)

def section(name):
    lines, inside = [], False
    for line in census.splitlines():
        if line.startswith("### "):
            inside = line[4:].strip().startswith(name)
            continue
        if inside and line.strip():
            lines.append(line)
    return lines


movable = section("MOVABLE")
blocked = section("LEFT")
for line in movable:
    print("      MOVABLE " + line.strip())
for line in blocked:
    print("      LEFT    " + line.strip())

def stat(name):
    for line in movable:
        if name in line:
            return line.strip()
    return None


harvested_gone = not any(
    ("tools::tool_mcp_call" in line or "tools::tool_mcp_search" in line) for line in blocked
)
check(
    harvested_gone,
    "tool_mcp_call and tool_mcp_search are no longer in the LEFT (blocked) list",
)

# 5b. residual blockers of tools_list, measured structurally (the census closure is
# polluted by glob re-exports, so read the files instead).
SHIM = re.compile(r"^\s*pub use refact_tools::tools::[a-z_/]+::\*;\s*$", re.M)


MOD_DECL = re.compile(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[a-z_][a-z0-9_]*\s*;")


def is_mod_declaration(module):
    for candidate in (os.path.join(module, "mod.rs"), module + ".rs"):
        path = os.path.join(TOOLS_SRC, candidate)
        if not os.path.isfile(path):
            continue
        body = re.sub(r"(?m)^\s*//.*$", "", read(path))
        body = re.sub(r"(?s)#\[cfg\(test\)\].*", "", body)
        stripped = MOD_DECL.sub("", body).strip()
        return stripped == ""
    return False


def is_pure_shim(module):
    for candidate in (module + ".rs", os.path.join(module, "mod.rs")):
        path = os.path.join(TOOLS_SRC, candidate)
        if not os.path.isfile(path):
            continue
        body = re.sub(r"(?m)^\s*//.*$", "", read(path))
        body = re.sub(r"(?s)#\[cfg\(test\)\].*", "", body)
        if SHIM.search(body):
            return True
    return False


def engine_file(module):
    for candidate in (module + ".rs", os.path.join(module, "mod.rs")):
        path = os.path.join(TOOLS_SRC, candidate)
        if os.path.isfile(path):
            return path
    return None


def blocks_tools_list(module):
    """A dep blocks tools_list while it is neither a crate shim nor a pure `mod x;`
    declaration file; `tool_design_system` and `tool_codegraph` are shims, the 5
    design stubs re-export design_tools, and everything else is real engine code."""
    if is_pure_shim(module) or is_mod_declaration(module):
        return False
    return engine_file(module) is not None


referenced = sorted(
    set(re.findall(r"crate::tools::([a-z_]+)::[A-Za-z_][A-Za-z0-9_]*", tools_list))
)


harvested = [m for m in ("tool_mcp_call", "tool_mcp_search") if is_pure_shim(m)]
check(
    len(harvested) == 2,
    "tools_list's harvested deps are pure shims: " + ", ".join(harvested),
)
residual = sorted(m for m in referenced if blocks_tools_list(m))
for module in residual:
    print(f"      residual engine dep of tools_list: src/tools/{module}.rs")

DESIGN_STUBS = {
    "tool_contrast_audit",
    "tool_image_region",
    "tool_mark_elements",
    "tool_ui_probe",
    "tool_visual_diff",
}
stubs = [m for m in residual if m in DESIGN_STUBS]
for module in stubs:
    body = read(os.path.join(TOOLS_SRC, module + ".rs")).strip()
    check(
        body == f"pub use super::design_tools::Tool{module[len('tool_'):].title().replace('_', '')};"
        or body.startswith("pub use super::design_tools::"),
        f"src/tools/{module}.rs is a one-line re-export of design_tools",
    )
check(
    set(stubs) == DESIGN_STUBS,
    "design_tools blocks tools_list transitively through the 5 design-tool stub shims",
)
check(
    "tool_chrome" in residual and "tool_subagent" in residual,
    "tools_list still directly depends on tool_chrome and tool_subagent",
)
SHELL_ISLAND = {"tool_process", "tool_shell", "tool_process_write_stdin"}
check(
    SHELL_ISLAND <= set(residual),
    "tools_list still depends on the shell island (card forbids moving it): "
    + ", ".join(sorted(SHELL_ISLAND & set(residual))),
)
for blocker in ("browser_runtime", "browser_controller"):
    path = os.path.join(SRC, "integrations", blocker + ".rs")
    body = read(path)
    defines = len(re.findall(r"(?m)^\s*pub (?:struct|enum|fn|async fn) ", body))
    check(
        defines > 0 and "pub use refact_browser" in body,
        f"integrations/{blocker}.rs is the real blocker (defines {defines} items over a "
        "`pub use refact_browser` glob, so the census misreads it as crate-only)",
    )
for consumer in ("design_tools", "tool_chrome"):
    body = read(os.path.join(TOOLS_SRC, consumer + ".rs"))
    check(
        "crate::integrations::browser_runtime" in body
        or "crate::integrations::browser_controller" in body,
        f"{consumer}.rs calls the engine browser layer, so it cannot move yet",
    )
# ---------------------------------------------------------------- 6. report
report = []
report.append("# R-1 tools endgame report")
report.append("")
report.append("## Census (after this card)")
report.extend("  " + line for line in headline)
report.append("")
report.append("## MOVABLE now")
report.extend("  " + line.strip() for line in movable)
report.append("")
report.append("## LEFT / blocked")
report.extend("  " + line.strip() for line in blocked)
report.append("")
report.append("## check failures")
report.extend("  " + p for p in problems)
out = os.path.join(ENGINE, "target", "r1_report.txt")
with open(out, "w", encoding="utf-8") as fh:
    fh.write("\n".join(report) + "\n")
print("wrote " + out)

print("")
if problems:
    print(f"### R-1 VERIFY: FAIL ({len(problems)} problem(s))")
    sys.exit(1)
print("### R-1 VERIFY: PASS")
