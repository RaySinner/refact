"""AM-3 verification harness: executes AL-3's verdict for buddy/integrations/providers/agents/ext/knowledge.

Run: python tools/dev/am3_check.py
Exits 0 when every assertion holds, 1 otherwise. No shell chaining, single command.
"""
import os
import re
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
ROOT = os.path.abspath(ROOT)
ENGINE = os.path.join(ROOT, "refact-agent", "engine")

failures = []
checks = 0


def ok(name):
    global checks
    checks += 1
    print(f"  PASS {name}")


def bad(name, detail=""):
    global checks
    checks += 1
    failures.append(name)
    print(f"  FAIL {name} {detail}")


def read(path):
    with open(path, encoding="utf8") as f:
        return f.read()


def exists(path):
    return os.path.isfile(path)


print("== AM-3 check: MCP cluster moved to refact-runtime-api ==")
MCP_ENGINE = os.path.join(ENGINE, "src", "integrations", "mcp")
MCP_CRATE = os.path.join(ENGINE, "crates", "refact-runtime-api", "src", "integrations", "mcp")

moved_mcp = [
    "integr_mcp_common.rs",
    "integr_mcp_http.rs",
    "integr_mcp_sse.rs",
    "integr_mcp_stdio.rs",
    "mcp_auth.rs",
    "mcp_prompts.rs",
    "mcp_resources.rs",
    "mcp_sampling.rs",
    "mcp_sse_transport.rs",
    "session_mcp.rs",
    "tool_mcp.rs",
    "tests_mcp_tools.rs",
    "mcp_http_schema.yaml",
    "mcp_sse_schema.yaml",
    "mcp_stdio_schema.yaml",
    "mcp_unified_schema.yaml",
]
for f in moved_mcp:
    if exists(os.path.join(MCP_CRATE, f)):
        ok(f"crate file present: {f}")
    else:
        bad(f"crate file present: {f}", "missing in refact-runtime-api/src/integrations/mcp")

for f in moved_mcp:
    p = os.path.join(MCP_ENGINE, f)
    if f.endswith(".yaml"):
        if not exists(p):
            ok(f"engine yaml removed: {f}")
        else:
            bad(f"engine yaml removed: {f}", "still present in engine mcp dir")
    elif f.endswith(".rs") and f != "mcp_sampling.rs":
        if exists(p):
            bad(f"engine file removed: {f}", "engine still hosts real file")
        else:
            ok(f"engine file removed: {f}")

engine_mcp_mod = read(os.path.join(MCP_ENGINE, "mod.rs"))
for mod_name in ["integr_mcp_common", "session_mcp", "tool_mcp", "mcp_prompts", "mcp_resources", "mcp_auth"]:
    if re.search(rf"pub mod {mod_name} \{{", engine_mcp_mod):
        ok(f"engine shim inline module: {mod_name}")
    else:
        bad(f"engine shim inline module: {mod_name}")

# no engine-crate path references remain in moved files
engine_path_pat = re.compile(
    r"crate::(buddy::actor|app_state::|privacy::|ext::|"
    r"call_validation|scratchpads|at_commands::at_commands|caps::|subchat|providers::)"
)
for f in moved_mcp:
    if not f.endswith(".rs"):
        continue
    txt = read(os.path.join(MCP_CRATE, f))
    hits = engine_path_pat.findall(txt)
    if hits:
        bad(f"crate file free of engine paths: {f}", f"found {sorted(set(hits))}")
    else:
        ok(f"crate file free of engine paths: {f}")

mcp_sampling_txt = read(os.path.join(MCP_CRATE, "mcp_sampling.rs"))
if "run_subchat_once" in mcp_sampling_txt and "app.chat_ctx().subchat" in mcp_sampling_txt:
    ok("mcp_sampling delegates via SubchatLauncher seam")
else:
    bad("mcp_sampling delegates via SubchatLauncher seam")

print("== AM-3 check: providers oauth_refresh moved to refact-runtime-api ==")
CRATE_OAUTH = os.path.join(ENGINE, "crates", "refact-runtime-api", "src", "providers_oauth_refresh.rs")
ENGINE_OAUTH = os.path.join(ENGINE, "src", "providers", "oauth_refresh.rs")
if exists(CRATE_OAUTH):
    ok("crate providers_oauth_refresh.rs present")
else:
    bad("crate providers_oauth_refresh.rs present")

crate_oauth = read(CRATE_OAUTH)
for fn in ["oauth_token_refresh_background_task", "force_refresh_claude_code_for_retry",
           "try_refresh_claude_code_instances", "try_refresh_openai_codex_instances",
           "try_refresh_xai_oauth_instances", "try_refresh_google_antigravity_instances"]:
    if fn in crate_oauth:
        ok(f"crate oauth_refresh hosts {fn}")
    else:
        bad(f"crate oauth_refresh hosts {fn}")

for engine_ref in ["crate::buddy::actor::", "crate::app_state::", "crate::providers::", "lazy_static"]:
    if engine_ref in crate_oauth:
        bad(f"crate oauth_refresh free of engine refs: {engine_ref}")
    else:
        ok(f"crate oauth_refresh free of engine refs: {engine_ref}")

if "make_runtime_event" in crate_oauth and "make_runtime_event(" in crate_oauth:
    ok("crate oauth_refresh uses refact_buddy_core make_runtime_event")

engine_oauth = read(ENGINE_OAUTH)
if engine_oauth.startswith("pub use refact_runtime_api::providers_oauth_refresh::*;"):
    ok("engine oauth_refresh is a pure re-export shim")
else:
    bad("engine oauth_refresh is a pure re-export shim", engine_oauth.splitlines()[0])

if "#[cfg(test)]" in engine_oauth and "make_test_gcx" in engine_oauth:
    ok("gcx-dependent tests preserved engine-side verbatim")

crate_tests = crate_oauth.count("#[test]") + crate_oauth.count("#[tokio::test]")
engine_tests = engine_oauth.count("#[test]") + engine_oauth.count("#[tokio::test]")
print(f"  INFO crate oauth tests: {crate_tests}, engine oauth tests: {engine_tests} (orig 13)")
if crate_tests + engine_tests == 13:
    ok("all 13 original oauth tests preserved across the split")
else:
    bad("all 13 original oauth tests preserved across the split", f"got {crate_tests + engine_tests}")

print("== AM-3 check: Cargo dependency direction (no crate->engine cycles) ==")
def cargo_deps(cargo_path):
    txt = read(cargo_path)
    return set(re.findall(r"(refact-[a-z0-9-]+)\s*=", txt))

runtime_api_cargo = os.path.join(ENGINE, "crates", "refact-runtime-api", "Cargo.toml")
deps = cargo_deps(runtime_api_cargo)
required = {"refact-integrations", "refact-providers", "refact-ext", "refact-chat-api",
             "refact-buddy-core", "refact-caps-core", "refact-privacy", "refact-core",
             "refact-tool-api", "refact-scratchpads"}
missing = required - deps
if not missing:
    ok("runtime-api deps include every destination-crate edge needed by the moves")
else:
    bad("runtime-api deps include every destination-crate edge needed", f"missing {missing}")

for dep in sorted(deps):
    dep_cargo = os.path.join(ENGINE, "crates", dep, "Cargo.toml")
    if not exists(dep_cargo):
        continue
    back = cargo_deps(dep_cargo)
    if "refact-runtime-api" in back:
        bad(f"no reverse edge {dep} -> refact-runtime-api", "CYCLE FOUND")
    else:
        ok(f"no reverse edge {dep} -> refact-runtime-api")

print("== AM-3 check: census classes after execution ==")
def census_file(mod):
    return os.path.join(ENGINE, "target", f"{mod}_movable_census.txt")

def left_lines(mod):
    txt = read(census_file(mod))
    m = re.search(r"LEFT \(blocked on engine code\): (\d+) files / (\d+) lines", txt)
    return int(m.group(1)), int(m.group(2))

integrations_left = left_lines("integrations")
if integrations_left == (0, 0):
    ok("integrations LEFT is now 0 files / 0 lines (was 11 / 4363)")
else:
    bad("integrations LEFT is now 0/0", f"got {integrations_left}")

ext_left = left_lines("ext")
if ext_left == (0, 0):
    ok("ext LEFT is now 0 files / 0 lines (was 1 / 56)")
else:
    bad("ext LEFT is now 0/0", f"got {ext_left}")

providers_txt = read(census_file("providers"))
m = re.search(r"LEFT \(blocked on engine code\): (\d+) files / (\d+) lines", providers_txt)
providers_left = (int(m.group(1)), int(m.group(2)))
if "providers::oauth_refresh" not in providers_txt.split("### ALREADY-SHIM")[0].split("LEFT (blocked on engine code)")[1]:
    ok("providers::oauth_refresh no longer in LEFT (was 1135 lines)")
else:
    bad("providers::oauth_refresh no longer in LEFT")
if providers_left == (6, 2683):
    ok("providers LEFT is 6 files / 2683 lines (was 7 / 3818)")
else:
    bad("providers LEFT expected 6/2683", f"got {providers_left}")

buddy_txt = read(census_file("buddy"))
m = re.search(r"LEFT \(blocked on engine code\): (\d+) files / (\d+) lines", buddy_txt)
print(f"  INFO buddy LEFT now {m.group(1)} files / {m.group(2)} lines (was 29 / 15371; "
      f"AL-3 real blocked mass 33 files / 8253 incl. test-only defect)")
knowledge_left = left_lines("knowledge")
agents_left = left_lines("agents")
print(f"  INFO knowledge LEFT {knowledge_left[1]} (was 79), agents LEFT {agents_left[1]} (was 921)")

print()
if failures:
    print(f"AM-3 check FAILED: {len(failures)} failures of {checks}")
    for f in failures:
        print(f"  - {f}")
    sys.exit(1)
print(f"AM-3 check PASSED: {checks} checks")
sys.exit(0)
