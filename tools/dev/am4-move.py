"""AM-4 move: harvest http::routers::v1::{mcp_oauth,v1_integrations,buddy,buddy_opportunities}.

Run: python tools/dev/am4-move.py
Each engine file becomes a pure glob re-export shim (keeping any trailing
#[cfg(test)] module verbatim); the whole production text lands in
crates/refact-http/src/routers/v1/, with only the engine-side `crate::` paths
retargeted onto the crates that now own those symbols.
"""
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
ENGINE_V1 = os.path.join(ENGINE, "src", "http", "routers", "v1")
CRATE_V1 = os.path.join(ENGINE, "crates", "refact-http", "src", "routers", "v1")

NAMES = ("mcp_oauth", "v1_integrations", "buddy", "buddy_opportunities")

# Longer prefixes first: the first match wins.
RETARGETS = [
    ("crate::buddy::settings::BuddyStorageMetadata", "refact_buddy_core::snapshot::BuddyStorageMetadata"),
    ("crate::buddy::settings::", "refact_buddy_core::settings::"),
    ("crate::buddy::actor::render_buddy_speech", "refact_runtime_api::buddy_speech_runtime::render_buddy_speech"),
    ("crate::buddy::actor::complete_quest_with_voice", "refact_runtime_api::buddy_speech_runtime::complete_quest_with_voice"),
    ("crate::buddy::actor::buddy_snapshot", "refact_runtime_api::buddy_actor_runtime::buddy_snapshot"),
    ("crate::buddy::actor::buddy_update_speech_user", "refact_runtime_api::buddy_actor_runtime::buddy_update_speech_user"),
    ("crate::buddy::actor::latest_project_root", "refact_runtime_api::buddy_actor_runtime::latest_project_root"),
    ("crate::buddy::actor::resolve_buddy_chat_model", "refact_runtime_api::buddy_actor_runtime::resolve_buddy_chat_model"),
    ("crate::buddy::actor::resolve_diagnostic", "refact_runtime_api::buddy_actor_runtime::resolve_diagnostic"),
    ("crate::buddy::actor::buddy_apply", "refact_runtime_api::buddy_runtime::buddy_apply"),
    ("crate::buddy::actor::", "refact_buddy_core::actor::"),
    ("crate::buddy::storage::atomic_write_json", "refact_buddy_core::storage::atomic_write_json"),
    ("crate::buddy::storage::load_recent_diagnostics", "refact_buddy_core::storage::load_recent_diagnostics"),
    ("crate::buddy::storage::apply_memory_batch", "refact_runtime_api::buddy_memory_ops::apply_memory_batch"),
    ("crate::buddy::diagnostics::", "refact_runtime_api::buddy_diagnostics::"),
    ("crate::buddy::pulse_inject::", "refact_runtime_api::buddy_pulse_inject::"),
    ("crate::buddy::issues::", "refact_runtime_api::buddy_issue_runtime::"),
    ("crate::buddy::types::", "refact_buddy_core::types::"),
    ("crate::buddy::user_activity::", "refact_buddy_core::user_activity::"),
    ("crate::buddy::voice_service::", "refact_buddy_core::voice_service::"),
    ("crate::buddy::state::", "refact_buddy_core::state::"),
    ("crate::buddy::receipts::", "refact_buddy_core::receipts::"),
    ("crate::buddy::drafts::", "refact_buddy_core::drafts::"),
    ("crate::buddy::opportunities::", "refact_buddy_core::opportunities::"),
    ("crate::buddy::briefing::", "refact_buddy_core::briefing::"),
    ("crate::buddy::conversation_ledger::", "refact_buddy_core::conversation_ledger::"),
    ("crate::buddy::speech_policy::", "refact_buddy_core::speech_policy::"),
    ("crate::chat::trajectories::save_trajectory_snapshot", "refact_runtime_api::trajectory_runtime::save_trajectory_snapshot"),
    ("crate::chat::ctx::ChatCtx", "refact_runtime_api::chat_ctx::ChatCtx"),
    ("crate::files_correction::get_unscoped_project_dirs_for_ctx", "refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx"),
    ("crate::files_correction::get_project_dirs", "refact_runtime_api::files_correction::get_project_dirs"),
    ("crate::ext::config_dirs::get_ext_dirs", "refact_runtime_api::ext_config_dirs::get_ext_dirs"),
    ("crate::tools::tool_buddy_get_logs::", "refact_tools::tools::tool_buddy_get_logs::"),
    ("crate::http::routers::v1::mcp_marketplace::", "crate::routers::v1::mcp_marketplace::"),
    ("crate::llm::safe_truncate", "refact_core::string_utils::safe_truncate"),
    ("crate::call_validation::", "refact_core::call_validation::"),
    ("crate::global_context::", "refact_runtime_api::global_context::"),
    ("crate::app_state::AppState", "refact_runtime_api::app_state::AppState"),
    ("crate::custom_error::ScratchError", "refact_core::custom_error::ScratchError"),
    ("crate::integrations::mcp::session_mcp::", "refact_runtime_api::integrations::mcp::session_mcp::"),
    ("crate::integrations::mcp::mcp_auth::", "refact_integrations::mcp::mcp_auth::"),
    ("crate::integrations::setting_up_integrations::", "refact_runtime_api::integrations::setting_up_integrations::"),
    ("crate::integrations::running_integrations::load_integrations", "refact_runtime_api::integrations_runtime::load_integrations"),
    ("log::info!", "tracing::info!"),
]

failures = []


def read(path):
    with open(path, encoding="utf8") as f:
        return f.read()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf8", newline="\n") as f:
        f.write(text)


def split_trailing_tests(text):
    m = re.search(r"\n#\[cfg\(test\)\]\nmod ", text)
    if not m:
        return text, None
    return text[: m.start() + 1], text[m.start() + 1 :]


def retarget(text):
    for old, new in RETARGETS:
        text = text.replace(old, new)
    return text


def move(name):
    engine_path = os.path.join(ENGINE_V1, name + ".rs")
    src = read(engine_path)
    if src.startswith(f"pub use refact_http::routers::v1::{name}::*;"):
        print(f"  SKIP {name}: already a shim")
        return
    prod, tests = split_trailing_tests(src)
    body = retarget(prod).replace("pub(crate) ", "pub ")
    body = body.replace("#[cfg(test)]\npub async fn read_log_content(", "pub async fn read_log_content(")
    if "assert" in body or "#[tokio::test]" in body:
        failures.append(f"{name}: production body still contains test code")
    crate_text = body.rstrip("\n") + "\n"
    write(os.path.join(CRATE_V1, name + ".rs"), crate_text)
    shim = f"pub use refact_http::routers::v1::{name}::*;\n"
    if tests:
        shim += "\n" + tests
    write(engine_path, shim)
    print(
        f"  MOVED {name}: crate {len(crate_text.splitlines())} lines, "
        f"shim {len(shim.splitlines())} lines, trailing tests kept={bool(tests)}"
    )


def declare_crate_mods():
    path = os.path.join(CRATE_V1, "mod.rs")
    text = read(path)
    added = []
    for name in NAMES:
        if re.search(rf"^pub mod {name};$", text, re.M):
            continue
        text = text.rstrip("\n") + f"\npub mod {name};\n"
        added.append(name)
    write(path, text)
    print(f"  crate mod.rs added: {added}")


print("== AM-4 move ==")
for n in NAMES:
    move(n)
declare_crate_mods()

path = os.path.join(CRATE_V1, "v1_integrations.rs")
text = read(path).replace(
    '[folder = "assets/integrations/"]', '[folder = "../../assets/integrations/"]'
)
write(path, text)

residue = 0
for n in NAMES:
    body = read(os.path.join(CRATE_V1, n + ".rs"))
    for h in sorted(set(re.findall(r"crate::[a-zA-Z0-9_:]+", body))):
        print(f"  RESIDUE {n}: {h}")
        residue += 1
    if 'assets/integrations' in body and '../../assets/integrations' not in body:
        print(f"  ASSET-PATH {n}")
        residue += 1

print()
if failures or residue:
    print(f"AM-4 move FAILED: {len(failures)} problems, {residue} residual engine paths")
    for f in failures:
        print(f"  - {f}")
    sys.exit(1)
print("AM-4 move done: no residual engine paths")
