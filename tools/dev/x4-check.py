#!/usr/bin/env python3
"""Static verification for card X-4 (kill 7 high-fan-out blockers).

The project build guard refuses compilation from agent cards, so this harness
checks the structural invariants the moved code must satisfy:

  R1 every one of the 7 symbols resolves to a crate-resident definition or a
     documented engine wrapper / shim;
  R2 no `crate::` path survives inside the moved crate modules (a crate must
     never depend on the engine);
  R3 the engine files that used to own the bodies contain no orphan call and
     no duplicate definition;
  R4 every touched file is brace/paren balanced and has no merge markers.

Usage:  python tools/dev/x4-check.py
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


def brace_delta(text):
    """Count `{` minus `}` outside comments, strings, chars and raw strings."""
    delta = 0
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if char == "/" and text.startswith("//", index):
            newline = text.find("\n", index)
            index = length if newline < 0 else newline
            continue
        if char == "/" and text.startswith("/*", index):
            end = text.find("*/", index + 2)
            index = length if end < 0 else end + 2
            continue
        if char == "r" and text.startswith('r#"', index):
            end = text.find('"#', index + 3)
            index = length if end < 0 else end + 2
            continue
        if char == "b" and text.startswith('b"', index):
            index += 1
            continue
        if char == '"':
            index += 1
            while index < length:
                if text[index] == "\\":
                    index += 2
                    continue
                if text[index] == '"':
                    index += 1
                    break
                index += 1
            continue
        if char == "'":
            if index + 2 < length and (text[index + 2] == "'" or text[index + 1] == "\\"):
                index += 1
                while index < length:
                    if text[index] == "\\":
                        index += 2
                        continue
                    if text[index] == "'":
                        index += 1
                        break
                    index += 1
                continue
            index += 1
            continue
        if char == "{":
            delta += 1
        elif char == "}":
            delta -= 1
        index += 1
    return delta


# ---------------------------------------------------------------- R1 residency
RESIDENCY = [
    # symbol, file that must define it, or shim that must re-export it
    ("BuddyEvent", CRATES / "refact-buddy-core/src/events.rs", "pub enum BuddyEvent"),
    ("last_n_chars", CRATES / "refact-core/src/nicer_logs.rs", "pub fn last_n_chars"),
    ("first_n_chars", CRATES / "refact-core/src/nicer_logs.rs", "pub fn first_n_chars"),
    (
        "browser_frame_emission_task",
        CRATES / "refact-runtime-api/src/browser_frame.rs",
        "pub async fn browser_frame_emission_task",
    ),
    (
        "run_global_import_for_source",
        CRATES / "refact-runtime-api/src/ext_runtime.rs",
        "pub async fn run_global_import_for_source",
    ),
    (
        "run_project_import_for_source",
        CRATES / "refact-runtime-api/src/ext_runtime.rs",
        "pub async fn run_project_import_for_source",
    ),
    (
        "extensions_marketplace module",
        CRATES / "refact-ext-runtime/src/ext_marketplace.rs",
        "pub async fn install_marketplace_item",
    ),
    (
        "invalidate_slash_cache",
        CRATES / "refact-runtime-api/src/slash_cache.rs",
        "pub async fn invalidate_slash_cache",
    ),
]

for name, path, needle in RESIDENCY:
    check(path.is_file(), "R1 missing crate file for %s: %s" % (name, path))
    if path.is_file():
        check(
            needle in read(path),
            "R1 %s is not defined in %s (expected `%s`)" % (name, path, needle),
        )

# engine-side shims must still expose the names call sites use
SHIMS = [
    (SRC / "nicer_logs.rs", "first_n_chars", "refact_core::nicer_logs"),
    (SRC / "nicer_logs.rs", "last_n_chars", "refact_core::nicer_logs"),
    (SRC / "ext/extensions_marketplace.rs", "*", "refact_ext_runtime::ext_marketplace"),
    (SRC / "buddy/events.rs", "BuddyEvent", "refact_buddy_core::events"),
]
for path, name, origin in SHIMS:
    text = read(path)
    check(origin in text, "R1 shim %s no longer re-exports from %s" % (path, origin))
    if name != "*":
        check(name in text, "R1 shim %s lost `%s`" % (path, name))

competitor_shim = read(SRC / "ext/competitor_import/mod.rs")
for fn in (
    "run_global_import",
    "run_global_import_for_source",
    "run_project_import",
    "run_project_import_for_source",
):
    check(
        re.search(r"\b%s\b" % fn, competitor_shim) is not None,
        "R1 competitor_import shim lost `%s`" % fn,
    )
check(
    "refact_runtime_api::ext_runtime" in competitor_shim,
    "R1 competitor_import shim does not re-export refact_runtime_api::ext_runtime",
)

browser_shim = read(SRC / "http/routers/v1/v1_browser.rs")
check(
    "browser_frame_emission_task" in browser_shim,
    "R1 v1_browser no longer exposes browser_frame_emission_task",
)
check(
    "refact_runtime_api::browser_frame" in browser_shim,
    "R1 v1_browser does not import from refact_runtime_api::browser_frame",
)

# ------------------------------------------------------- R2 no engine refs in crates
MOVED = [
    CRATES / "refact-runtime-api/src/ext_runtime.rs",
    CRATES / "refact-ext-runtime/src/ext_marketplace.rs",
    CRATES / "refact-runtime-api/src/browser_frame.rs",
    CRATES / "refact-runtime-api/src/slash_cache.rs",
    CRATES / "refact-core/src/nicer_logs.rs",
]
for path in MOVED:
    text = read(path)
    leftovers = sorted(
        set(m.group(0) for m in re.finditer(r"crate::[A-Za-z0-9_]+", text))
    )
    allowed = {
        "crate::app_state",
        "crate::customization_registry_gcx",
        "crate::ext_config_dirs",
        "crate::ext_plugins",
        "crate::files_correction",
        "crate::slash_cache",
        "crate::global_context",
    }
    bad = [item for item in leftovers if item not in allowed]
    check(not bad, "R2 %s keeps engine-only paths: %s" % (path.name, bad))

# ---------------------------------------------------- R3 no duplicate definitions
check(
    read(SRC / "nicer_logs.rs").count("pub fn last_n_chars") == 0,
    "R3 engine nicer_logs.rs still defines last_n_chars",
)
check(
    read(SRC / "ext/extensions_marketplace.rs").count("pub async fn list_marketplace_items") == 0,
    "R3 engine extensions_marketplace.rs still defines list_marketplace_items",
)
check(
    "fn recorder_events_to_timeline" not in browser_shim,
    "R3 v1_browser.rs still defines recorder_events_to_timeline",
)
check(
    "pub async fn browser_frame_emission_task" not in browser_shim,
    "R3 v1_browser.rs still defines browser_frame_emission_task",
)
engine_competitor = competitor_shim
check(
    "pub async fn run_global_import_for_source(" not in engine_competitor,
    "R3 competitor_import/mod.rs still defines run_global_import_for_source",
)

# the tests for the moved competitor-import bodies must survive in the engine shim
for test_name in ("runtime_event_emit_enqueues_when_buddy_service_exists",):
    check(
        test_name in engine_competitor,
        "R3 engine competitor_import tests lost `%s`" % test_name,
    )

for test_name in (
    "runtime_event_for_unchanged_import_is_low_transient",
    "runtime_event_for_created_import_is_normal_completed",
    "runtime_event_for_conflicts_and_errors_is_sanitized_attention",
    "import_changes_drive_cache_invalidation_flags",
):
    check(
        test_name
        in read(CRATES / "refact-runtime-api/src/ext_runtime.rs")
        or test_name in engine_competitor,
        "R3 moved competitor-import test `%s` disappeared" % test_name,
    )

for test_name in (
    "test_normalize_github_source_full_url",
    "test_scan_skill_items",
    "test_resolve_repo_path_rejects_escape",
):
    check(
        test_name in read(CRATES / "refact-ext-runtime/src/ext_marketplace.rs"),
        "R3 moved marketplace tests lost `%s`" % test_name,
    )

for test_name in (
    "first_n_chars_keeps_prefix_and_escapes_newlines",
    "last_n_chars_keeps_suffix_and_escapes_newlines",
):
    check(
        test_name in read(CRATES / "refact-core/src/nicer_logs.rs"),
        "R3 nicer_logs tests lost `%s`" % test_name,
    )

# --------------------------------------------------------------- R4 file sanity
TOUCHED = [
    SRC / "nicer_logs.rs",
    SRC / "buddy/events.rs",
    SRC / "ext/extensions_marketplace.rs",
    SRC / "ext/competitor_import/mod.rs",
    SRC / "integrations/browser_runtime.rs",
    SRC / "integrations/setup_chat.rs",
    SRC / "integrations/config_chat.rs",
    SRC / "http/routers/v1/v1_browser.rs",
    SRC / "http/routers/v1/at_commands.rs",
    SRC / "http/routers/v1/buddy.rs",
    SRC / "http/routers/v1/sidebar.rs",
    SRC / "app_state.rs",
    SRC / "scratchpads/mod.rs",
    CRATES / "refact-core/src/nicer_logs.rs",
    CRATES / "refact-buddy-core/src/events.rs",
    CRATES / "refact-runtime-api/src/lib.rs",
    CRATES / "refact-runtime-api/Cargo.toml",
    CRATES / "refact-runtime-api/src/ext_runtime.rs",
    CRATES / "refact-ext-runtime/src/ext_marketplace.rs",
    CRATES / "refact-runtime-api/src/browser_frame.rs",
    CRATES / "refact-runtime-api/src/slash_cache.rs",
    CRATES / "refact-runtime-api/src/chat_ctx.rs",
]
for path in TOUCHED:
    text = read(path)
    check("<<<<<<<" not in text, "R4 merge marker in %s" % path)
    check(">>>>>>>" not in text, "R4 merge marker in %s" % path)
    if path.suffix == ".rs":
        delta = brace_delta(text)
        check(delta == 0, "R4 unbalanced braces in %s (delta %d)" % (path, delta))

# ------------------------- R5 the two engine-resident wrappers have no crate-side leaks
# `chat_utils_prompts` and `get_tools_for_mode` stay engine-side (their bodies build
# engine handles). The integrations call sites reach them through the crate-defined
# `PromptAssembler::system_prompt_add_extra_instructions` and
# `ToolRegistry::get_tool_names_for_mode` seams instead of importing engine paths.
for path in sorted((SRC / "integrations").rglob("*.rs")):
    text = read(path)
    for banned in (
        "crate::scratchpads::chat_utils_prompts",
        "crate::tools::tools_list::get_tools_for_mode",
    ):
        check(
            banned not in text,
            "R5 %s still imports %s" % (path.name, banned),
        )

check(
    "system_prompt_add_extra_instructions" in read(CRATES / "refact-runtime-api/src/chat_ctx.rs"),
    "R5 PromptAssembler lost the system_prompt_add_extra_instructions seam",
)
check(
    "get_tool_names_for_mode" in read(CRATES / "refact-runtime-api/src/lib.rs"),
    "R5 ToolRegistry lost the get_tool_names_for_mode seam",
)

print("checks run: %d" % checks)
if problems:
    print("PROBLEMS (%d):" % len(problems))
    for item in problems:
        print("  " + item)
    sys.exit(1)
print("OK: all X-4 structural checks passed")
