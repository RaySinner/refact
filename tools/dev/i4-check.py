#!/usr/bin/env python3
"""Static verification for card I-4 (clear the chat/http blocker cluster).

The project build guard refuses compilation from agent cards, so this harness
checks the structural invariants the moved code must satisfy:

  R1 every card symbol resolves to a crate-resident definition or a documented
     engine wrapper / re-export;
  R2 the moved crate modules contain no `crate::` engine path (a crate must
     never depend on the engine);
  R3 no duplicate definition survives in the engine, and no orphan call;
  R4 `constants::CHAT_TOP_N` resolves to a crate re-export with the value 12;
  R5 documented leave-behinds are still engine-side;
  R6 no `;`-free command needed: balanced braces and no merge markers in every
     touched file.

Usage:  python tools/dev/i4-check.py
Exit 0 = all checks passed.
"""

import re
import sys
from pathlib import Path

ENGINE = Path("refact-agent/engine")
SRC = ENGINE / "src"
CRATES = ENGINE / "crates"

problems = []
checks = 0


def read(path):
    return path.read_text(encoding="utf-8-sig", errors="replace")


def check(cond, message):
    global checks
    checks += 1
    if not cond:
        problems.append(message)


def has(path, needle):
    return needle in read(path)


def crate_files_with(needle):
    return [p for p in CRATES.rglob("*.rs") if needle in read(p)]


def engine_files_with(needle):
    return [p for p in SRC.rglob("*.rs") if needle in read(p)]


# --- R1: crate-resident definitions -----------------------------------------

core_constants = CRATES / "refact-core/src/constants.rs"
check(core_constants.exists(), "R1 refact-core/src/constants.rs missing")
if core_constants.exists():
    text = read(core_constants)
    check("pub const CHAT_TOP_N: usize = 12;" in text, "R1 CHAT_TOP_N must stay 12")
    check('pub const CLOUD_URL: &str = "https://flexus.team/v1";' in text,
          "R1 CLOUD_URL moved verbatim")

engine_constants = read(SRC / "constants.rs")
check("pub use refact_core::constants::{CHAT_TOP_N, CLOUD_URL};" in engine_constants,
      "R1 engine constants.rs must be a crate re-export shim")

core_lib = read(CRATES / "refact-core/src/lib.rs")
check("pub mod constants;" in core_lib, "R1 refact-core lib.rs must declare constants")

runtime_api_tool_catalog = read(CRATES / "refact-runtime-api/src/tool_catalog.rs")
check("pub fn tool_catalog_snapshot_rollout_enabled() -> bool" in runtime_api_tool_catalog,
      "R1 tool_catalog_snapshot_rollout_enabled must live in refact-runtime-api")
check("pub fn tool_catalog_snapshot_rollout_enabled_for(value: Option<&str>) -> bool"
      in runtime_api_tool_catalog,
      "R1 tool_catalog_snapshot_rollout_enabled_for must live in refact-runtime-api")
check('pub const TOOL_CATALOG_SNAPSHOTS_ENV: &str = "REFACT_TOOL_CATALOG_SNAPSHOTS";'
      in runtime_api_tool_catalog,
      "R1 TOOL_CATALOG_SNAPSHOTS_ENV must live in refact-runtime-api")

runtime_api_memories = read(CRATES / "refact-runtime-api/src/memories_runtime.rs")
check("pub async fn memory_plane_roots(gcx: SharedGlobalContext) -> MemoryPlaneRoots"
      in runtime_api_memories,
      "R1 memory_plane_roots must live in refact-runtime-api::memories_runtime")
check("pub async fn memory_plane_roots_for_ctx(ctx: &ChatCtx) -> MemoryPlaneRoots"
      in runtime_api_memories,
      "R1 memory_plane_roots_for_ctx must live in refact-runtime-api::memories_runtime")

privacy_destinations = CRATES / "refact-privacy/src/destinations.rs"
check(privacy_destinations.exists(), "R1 refact-privacy/src/destinations.rs missing")
if privacy_destinations.exists():
    text = read(privacy_destinations)
    check("pub fn clear_for_model<T: PrivacyAudited>(" in text,
          "R1 clear_for_model must live in refact-privacy::destinations")
    check("pub fn clear_for_mcp<T: PrivacyAudited>(" in text,
          "R1 clear_for_mcp must live in refact-privacy::destinations")
    check("pub trait DestinationExt<T>" in text,
          "R1 DestinationExt must live in refact-privacy::destinations")
    check("mod tests {" in text, "R1 destinations inline tests must move verbatim")

oauth_runtime = CRATES / "refact-runtime-api/src/providers_oauth_runtime.rs"
check(oauth_runtime.exists(), "R1 providers_oauth_runtime.rs missing")
if oauth_runtime.exists():
    text = read(oauth_runtime)
    check("pub async fn force_refresh_claude_code_for_retry(" in text,
          "R1 force_refresh_claude_code_for_retry must live in refact-runtime-api")
    for helper in [
        "pub fn needs_refresh",
        "pub fn proactive_refresh_can_reuse_newer_expiry",
        "pub async fn invalidate_caps",
        "pub async fn current_claude_code_provider",
        "pub async fn sync_claude_code_tokens_from_disk",
        "pub async fn save_claude_code_refreshed_tokens",
        "pub const REFRESH_BEFORE_EXPIRY_MS",
    ]:
        check(helper in text, f"R1 oauth helper must live in refact-runtime-api: {helper}")

task_storage = read(CRATES / "refact-runtime-api/src/task_storage.rs")
check("pub async fn enrich_task_with_session_state_for_services(" in task_storage,
      "R1 enrich_task_with_session_state_for_services must live in refact-runtime-api")

task_comments = CRATES / "refact-runtime-api/src/task_comments.rs"
check(task_comments.exists(), "R1 task_comments.rs missing")
if task_comments.exists():
    check("pub async fn create_card_comment_for_ctx(" in read(task_comments),
          "R1 create_card_comment_for_ctx must live in refact-runtime-api")

trajectory_runtime = read(CRATES / "refact-runtime-api/src/trajectory_runtime.rs")
check("pub async fn save_initial_task_chat_trajectory(" in trajectory_runtime,
      "R1 save_initial_task_chat_trajectory must live in refact-runtime-api")
check("I'm your **Task Planner**" in trajectory_runtime,
      "R1 planner greeting must move verbatim")

scheduler_crate = read(CRATES / "refact-runtime-api/src/scheduler.rs")
check("pub async fn active_durable_cron_store(" in scheduler_crate,
      "R1 active_durable_cron_store must live in refact-runtime-api::scheduler")
check("pub async fn active_durable_cron_store_for_services(" in scheduler_crate,
      "R1 active_durable_cron_store_for_services must live in refact-runtime-api::scheduler")

runtime_api_lib = read(CRATES / "refact-runtime-api/src/lib.rs")
for module in [
    "pub mod task_comments;",
    "pub mod providers_oauth_runtime;",
    "pub mod memories_runtime;",
    "pub mod task_storage;",
]:
    check(module in runtime_api_lib, f"R1 refact-runtime-api lib.rs must declare {module}")

privacy_lib = read(CRATES / "refact-privacy/src/lib.rs")
check("pub mod destinations;" in privacy_lib, "R1 refact-privacy lib.rs must declare destinations")
check("pub use destinations::{clear_for_mcp, clear_for_model, DestinationExt};" in privacy_lib,
      "R1 refact-privacy must re-export the destination helpers")

# --- R2: no engine dependency inside the moved crate modules ----------------

# `crate::global_context`, `crate::files_correction`, `crate::services`,
# `crate::memories*`, `crate::tool_catalog` all exist INSIDE refact-runtime-api,
# so only engine-only module paths are forbidden here.
moved_crate_modules = [
    ("refact-runtime-api/src/providers_oauth_runtime.rs", ["crate::caps::", "crate::tools::", "crate::buddy::"]),
    ("refact-runtime-api/src/task_comments.rs", ["crate::tasks::", "crate::chat::"]),
    ("refact-runtime-api/src/trajectory_runtime.rs", ["crate::chat::", "crate::app_state::"]),
    ("refact-runtime-api/src/scheduler.rs", ["crate::app_state::PathServices::"]),
    ("refact-privacy/src/destinations.rs", ["crate::caps::", "crate::privacy::"]),
]
for rel, forbidden in moved_crate_modules:
    path = CRATES / rel
    if not path.exists():
        problems.append(f"R2 missing crate module {rel}")
        checks += 1
        continue
    text = read(path)
    for needle in forbidden:
        check(needle not in text, f"R2 {rel} must not reference engine path {needle}")

# --- R3: no duplicate definition / orphan in the engine ---------------------

def function_count(path, signature):
    return len(re.findall(re.escape(signature), read(path)))


engine_oauth = SRC / "providers/oauth_refresh.rs"
check(function_count(engine_oauth, "pub async fn force_refresh_claude_code_for_retry(") == 1,
      "R3 engine force_refresh_claude_code_for_retry must be a single wrapper")
check("refact_runtime_api::providers_oauth_runtime::force_refresh_claude_code_for_retry("
      in read(engine_oauth),
      "R3 engine wrapper must delegate to the crate")
check("pub(crate) async fn refresh_claude_code_instance_if_needed(" in read(engine_oauth),
      "R3 engine-side refresh_claude_code_instance_if_needed must stay")

engine_indexing = read(SRC / "indexing_routing.rs")
check("pub use refact_runtime_api::memories_runtime::memory_plane_roots_for_ctx;"
      in engine_indexing,
      "R3 engine indexing_routing must re-export memory_plane_roots_for_ctx")
check(function_count(SRC / "indexing_routing.rs", "pub async fn memory_plane_roots(") == 1,
      "R3 engine memory_plane_roots wrapper must be a single definition")
check("crate::memories::get_global_knowledge_dir(&gcx)" not in engine_indexing,
      "R3 engine must not keep the moved memory_plane_roots body")

engine_comments = read(SRC / "tasks/comments.rs")
check("pub(crate) use refact_runtime_api::task_comments::create_card_comment_for_ctx;"
      in engine_comments,
      "R3 engine tasks/comments.rs must re-export create_card_comment_for_ctx")
check("storage::update_board_atomic_for_ctx" not in engine_comments,
      "R3 engine must not keep the moved comment body")

engine_events = read(SRC / "tasks/events.rs")
check(function_count(SRC / "tasks/events.rs", "pub async fn enrich_task_with_session_state_for_services(") == 0,
      "R3 enrich_task_with_session_state_for_services must not be redefined in the engine")

engine_load = read(SRC / "chat/trajectories/load.rs")
check(function_count(SRC / "chat/trajectories/load.rs", "pub async fn save_initial_task_chat_trajectory(") == 0,
      "R3 save_initial_task_chat_trajectory must not be redefined in the engine")
check("pub use refact_runtime_api::trajectory_runtime::save_initial_task_chat_trajectory;"
      in engine_load,
      "R3 engine load.rs must re-export save_initial_task_chat_trajectory")

engine_scheduler = read(SRC / "scheduler/mod.rs")
check("pub use refact_runtime_api::scheduler::{" in engine_scheduler,
      "R3 engine scheduler must re-export the durable-store helpers")

engine_app_state = read(SRC / "app_state.rs")
check(function_count(SRC / "app_state.rs", "pub(crate) fn tool_catalog_snapshot_rollout_enabled(") == 0,
      "R3 app_state must not redefine the rollout helper")
check("pub(crate) use refact_runtime_api::tool_catalog_snapshot_rollout_enabled;" in engine_app_state,
      "R3 app_state must re-export the rollout helper")
check("pub(crate) use refact_runtime_api::tool_catalog_snapshot_rollout_enabled_for;" in engine_app_state,
      "R3 app_state must re-export the rollout helper used only by tests")

engine_privacy_dest = read(SRC / "privacy/destinations.rs")
check("pub use refact_privacy::destinations::*;" in engine_privacy_dest,
      "R3 engine privacy/destinations.rs must be a glob shim")
check("fn clear_for_model" not in engine_privacy_dest,
      "R3 engine must not redefine clear_for_model")

# --- R4: CHAT_TOP_N call sites still resolve --------------------------------

engine_constants_users = engine_files_with("constants::CHAT_TOP_N")
check(len(engine_constants_users) > 0,
      "R4 expected `constants::CHAT_TOP_N` call sites in the engine")
check("pub const CHAT_TOP_N: usize = 12;" in read(core_constants),
      "R4 CHAT_TOP_N numeric value must not change")

# --- R5: documented leave-behinds stay engine-side --------------------------

tokens = read(SRC / "tokens.rs")
check("pub async fn cached_tokenizer(" in tokens,
      "R5 tokens::cached_tokenizer stays engine-side (HTTP download with auth)")
check("AUTHORIZATION" in tokens,
      "R5 cached_tokenizer still downloads with bearer auth, so it cannot move")
check("pub use refact_core::tokens_runtime::{" in tokens,
      "R5 pure token counters remain crate re-exports (U-2)")

# --- R6: hygiene ------------------------------------------------------------

touched = [
    SRC / "constants.rs",
    SRC / "app_state.rs",
    SRC / "indexing_routing.rs",
    SRC / "tasks/events.rs",
    SRC / "tasks/comments.rs",
    SRC / "scheduler/mod.rs",
    SRC / "privacy/destinations.rs",
    SRC / "chat/trajectories/load.rs",
    SRC / "providers/oauth_refresh.rs",
    CRATES / "refact-core/src/constants.rs",
    CRATES / "refact-core/src/lib.rs",
    CRATES / "refact-runtime-api/src/lib.rs",
    CRATES / "refact-runtime-api/src/tool_catalog.rs",
    CRATES / "refact-runtime-api/src/memories_runtime.rs",
    CRATES / "refact-runtime-api/src/task_storage.rs",
    CRATES / "refact-runtime-api/src/task_comments.rs",
    CRATES / "refact-runtime-api/src/trajectory_runtime.rs",
    CRATES / "refact-runtime-api/src/scheduler.rs",
    CRATES / "refact-runtime-api/src/providers_oauth_runtime.rs",
    CRATES / "refact-privacy/src/lib.rs",
    CRATES / "refact-privacy/src/destinations.rs",
]
for path in touched:
    if not path.exists():
        problems.append(f"R6 missing touched file {path}")
        checks += 1
        continue
    text = read(path)
    check("<<<<<<<" not in text and ">>>>>>>" not in text,
          f"R6 merge markers in {path}")
    check(text.count("{") == text.count("}"),
          f"R6 unbalanced braces in {path}: {text.count('{')} vs {text.count('}')}")
    check(text.count("(") == text.count(")"),
          f"R6 unbalanced parens in {path}")

print(f"i4-check: {checks} checks executed, {len(problems)} problem(s)")
for problem in problems:
    print(f"  FAIL {problem}")
sys.exit(1 if problems else 0)
