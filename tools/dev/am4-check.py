"""AM-4 verification harness: asserts the http harvest landed and nothing engine-shaped leaked.

Run: python tools/dev/am4-check.py
Exits 0 when every assertion holds, 1 otherwise. Single command, no `;`.
"""
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
ENGINE_V1 = os.path.join(ENGINE, "src", "http", "routers", "v1")
CRATE_V1 = os.path.join(ENGINE, "crates", "refact-http", "src", "routers", "v1")

MOVED = ("mcp_oauth", "v1_integrations", "buddy", "buddy_opportunities")

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


print("== AM-4 check: http routers harvested into refact-http ==")

for name in MOVED:
    crate_path = os.path.join(CRATE_V1, name + ".rs")
    engine_path = os.path.join(ENGINE_V1, name + ".rs")
    if os.path.isfile(crate_path):
        ok(f"crate file present: {name}")
    else:
        bad(f"crate file present: {name}", "missing in refact-http")
    engine_text = read(engine_path)
    first = engine_text.splitlines()[0] if engine_text.strip() else ""
    if first == f"pub use refact_http::routers::v1::{name}::*;":
        ok(f"engine file is a glob shim: {name}")
    else:
        bad(f"engine file is a glob shim: {name}", first)

crate_mod = read(os.path.join(CRATE_V1, "mod.rs"))
for name in MOVED:
    if re.search(rf"^pub mod {name};$", crate_mod, re.M):
        ok(f"crate v1 mod declares {name}")
    else:
        bad(f"crate v1 mod declares {name}")

print("== AM-4 check: no engine-resident paths in the moved bodies ==")
for name in MOVED:
    body = read(os.path.join(CRATE_V1, name + ".rs"))
    residual = sorted(set(re.findall(r"crate::(?!routers)[a-zA-Z0-9_:]+", body)))
    if residual:
        bad(f"crate body free of engine paths: {name}", str(residual))
    else:
        ok(f"crate body free of engine paths: {name}")

print("== AM-4 check: crate manifest has no refact-lsp edge ==")
http_toml = read(os.path.join(ENGINE, "crates", "refact-http", "Cargo.toml"))
if "refact-lsp" in http_toml:
    bad("refact-http does not depend on refact-lsp")
else:
    ok("refact-http does not depend on refact-lsp")
if re.search(r'^rust-embed\s*=', http_toml, re.M):
    ok("refact-http declares rust-embed for the integration icon assets")
else:
    bad("refact-http declares rust-embed for the integration icon assets")

print("== AM-4 check: embedded asset folder still resolves ==")
integrations = read(os.path.join(CRATE_V1, "v1_integrations.rs"))
m = re.search(r'#\[folder = "([^"]+)"\]', integrations)
if not m:
    bad("v1_integrations declares a #[folder] attribute")
else:
    folder = m.group(1)
    resolved = os.path.normpath(os.path.join(ENGINE, "crates", "refact-http", folder))
    if os.path.isdir(resolved):
        ok(f"embedded folder resolves: {folder}")
    else:
        bad("embedded folder resolves", resolved)

print("== AM-4 check: the seam is installed by the engine ==")
seam = os.path.join(ENGINE, "crates", "refact-runtime-api", "src", "buddy_speech_runtime.rs")
if os.path.isfile(seam):
    ok("buddy_speech_runtime seam exists")
else:
    bad("buddy_speech_runtime seam exists")

seam_text = read(seam) if os.path.isfile(seam) else ""
for item in ("trait BuddySpeechRuntime", "fn render_buddy_speech", "fn complete_quest_with_voice"):
    if item in seam_text:
        ok(f"seam declares {item}")
    else:
        bad(f"seam declares {item}")

factory = read(os.path.join(ENGINE, "crates", "refact-runtime-api", "src", "context_factory.rs"))
if "fn buddy_speech(&self)" in factory:
    ok("ContextFactory exposes buddy_speech()")
else:
    bad("ContextFactory exposes buddy_speech()")

engine_factory = read(os.path.join(ENGINE, "src", "app_state.rs"))
if "impl refact_runtime_api::buddy_speech_runtime::BuddySpeechRuntime for EngineBuddySpeechRuntime" in engine_factory:
    ok("engine implements BuddySpeechRuntime")
else:
    bad("engine implements BuddySpeechRuntime")

print("== AM-4 check: routes and handlers are unchanged ==")
routes = read(os.path.join(ENGINE, "src", "http", "routers", "v1.rs"))
for needle in (
    '"/mcp/oauth/start"',
    '"/mcp/oauth/callback"',
    '"/integrations"',
    '"/buddy"',
    '"/buddy/opportunities"',
):
    if needle in routes:
        ok(f"route kept: {needle}")
    else:
        bad(f"route kept: {needle}")

for handler in (
    "handle_v1_mcp_oauth_start",
    "handle_v1_mcp_oauth_exchange",
    "handle_v1_mcp_oauth_callback",
    "handle_v1_mcp_oauth_logout",
    "handle_v1_mcp_oauth_cancel",
    "handle_v1_mcp_oauth_status",
    "handle_v1_integration_icon",
    "handle_v1_integrations_mcp_logs",
    "handle_v1_buddy_settings_update",
    "handle_v1_buddy_opportunity_accept",
):
    if handler in routes:
        ok(f"route handler wired: {handler}")
    else:
        bad(f"route handler wired: {handler}")

print("== AM-4 check: inline tests moved verbatim ==")
engine_mcp_oauth = read(os.path.join(ENGINE_V1, "mcp_oauth.rs"))
crate_mcp_oauth = read(os.path.join(CRATE_V1, "mcp_oauth.rs"))
if "mod tests {" in engine_mcp_oauth:
    ok("mcp_oauth inline test module kept in the engine shim")
else:
    bad("mcp_oauth inline test module kept in the engine shim")
for fn in (
    "test_html_escape_script_tags",
    "test_html_response_contains_csp_header",
    "test_config_path_traversal_rejected",
    "test_logout_clears_tokens_from_config",
):
    if fn in engine_mcp_oauth and fn not in crate_mcp_oauth:
        ok(f"test kept engine-side only: {fn}")
    else:
        bad(f"test kept engine-side only: {fn}")

print("== AM-4 check: engine tests still reach the moved helpers ==")
for helper in ("dispatch_action", "enrich_investigation_context", "read_log_content", "INVESTIGATION_SYSTEM_PROMPT"):
    if helper in read(os.path.join(CRATE_V1, "buddy_opportunities.rs")):
        ok(f"buddy_opportunities exposes {helper}")
    else:
        bad(f"buddy_opportunities exposes {helper}")

print("== AM-4 check: after-census ==")
import subprocess  # noqa: E402

census = subprocess.run(
    [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), "http"],
    capture_output=True,
    text=True,
)
summary = [line for line in census.stdout.splitlines() if line.strip()]
print("\n".join(summary))
if census.returncode != 0:
    bad("census ran", census.stderr.strip())
else:
    joined = "\n".join(summary)
    if "MOVABLE now: 0 files / 0 lines" in joined:
        ok("MOVABLE now is 0/0")
    else:
        for line in summary:
            if line.startswith("MOVABLE now:"):
                bad("MOVABLE now is 0/0", line)

print()
if failures:
    print(f"AM-4 check FAILED: {len(failures)} failures of {checks}")
    for f in failures:
        print(f"  - {f}")
    sys.exit(1)
print(f"AM-4 check OK: {checks} assertions")
