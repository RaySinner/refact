"""Move five http v1 router files from the engine into `refact-http`.

Only import lines change; everything else is byte-identical. The engine keeps
1-line glob shims so every `crate::` path still resolves.
"""
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENGINE, "src", "http", "routers", "v1")
CRATE = os.path.join(ENGINE, "crates", "refact-http", "src", "routers", "v1")

FILES = [
    "ext_management",
    "plugins",
    "buddy_frontend_error",
    "mcp_interactions",
    "gui_help_handlers",
]

REWRITES = {
    "ext_management": [
        (
            "use crate::app_state::AppState;",
            "use refact_runtime_api::app_state::AppState;",
        ),
        (
            "use crate::chat::ctx::ChatCtx;",
            "use refact_runtime_api::chat_ctx::ChatCtx;",
        ),
        (
            "use crate::custom_error::ScratchError;",
            "use refact_core::custom_error::ScratchError;",
        ),
        (
            "use crate::ext::config_dirs::{CommandSource, ExtDirs, get_ext_dirs};",
            "use refact_runtime_api::ext_config_dirs::{CommandSource, ExtDirs, get_ext_dirs};",
        ),
        (
            "use crate::ext::hooks::{HookConfig, HookEvent, load_hooks};",
            "use refact_ext::hooks::{HookConfig, HookEvent, load_hooks};",
        ),
        (
            "use crate::ext::skills::{load_skill_full, load_skill_indices};",
            "use refact_ext::skills::{load_skill_full, load_skill_indices};",
        ),
        (
            "use crate::ext::slash_commands::{load_slash_commands, parse_frontmatter_and_body};",
            "use refact_ext::slash_commands::{load_slash_commands, parse_frontmatter_and_body};",
        ),
        (
            "use crate::files_correction::get_unscoped_project_dirs_for_ctx;",
            "use refact_runtime_api::files_correction::get_unscoped_project_dirs_for_ctx;",
        ),
        (
            "use crate::http::routers::v1::at_commands::invalidate_slash_cache;",
            "use refact_runtime_api::slash_cache::invalidate_slash_cache;",
        ),
        (
            "use crate::buddy::drafts::{draft_kind_str, DraftTarget, DraftValidationError};",
            "use refact_buddy_core::drafts::{draft_kind_str, DraftTarget, DraftValidationError};",
        ),
        (
            "use crate::buddy::types::DraftKind;",
            "use refact_buddy_core::types::DraftKind;",
        ),
        (
            "use crate::ext::competitor_import::types::{Competitor, ImportReport};",
            "use refact_ext::competitor_import::types::{Competitor, ImportReport};",
        ),
        (
            "    use crate::ext::config_dirs::ExtDirs;",
            "    use refact_runtime_api::ext_config_dirs::ExtDirs;",
        ),
        (
            "pub(crate) async fn write_file_atomic(",
            "pub async fn write_file_atomic(",
        ),
    ],
    "plugins": [
        (
            "use crate::app_state::AppState;",
            "use refact_runtime_api::app_state::AppState;",
        ),
        (
            "use crate::custom_error::ScratchError;",
            "use refact_core::custom_error::ScratchError;",
        ),
        (
            "use crate::ext::plugins::{",
            "use refact_runtime_api::ext_plugins::{",
        ),
    ],
    "buddy_frontend_error": [
        (
            "use crate::app_state::AppState;",
            "use refact_runtime_api::app_state::AppState;",
        ),
        (
            "use crate::buddy::actor::redact_sensitive;",
            "use refact_buddy_core::actor::redact_sensitive;",
        ),
        (
            "use crate::buddy::diagnostics::{classify_error, DiagnosticContext, DiagnosticSeverity};",
            "use refact_buddy_core::diagnostics::{\n    classify_error, DiagnosticContext, DiagnosticSeverity,\n};",
        ),
        (
            "use crate::custom_error::ScratchError;",
            "use refact_core::custom_error::ScratchError;",
        ),
    ],
    "mcp_interactions": [
        (
            "use crate::custom_error::ScratchError;",
            "use refact_core::custom_error::ScratchError;",
        ),
        (
            "use crate::integrations::mcp::mcp_interactions::{",
            "use refact_integrations::mcp::mcp_interactions::{",
        ),
    ],
    "gui_help_handlers": [
        (
            "use crate::app_state::AppState;",
            "use refact_runtime_api::app_state::AppState;",
        ),
        (
            "use crate::at_commands::at_file::{file_repair_candidates, return_one_candidate_or_a_good_error};",
            "use refact_runtime_api::at_command_executor::{\n    file_repair_candidates, return_one_candidate_or_a_good_error,\n};",
        ),
        (
            "use crate::custom_error::ScratchError;",
            "use refact_core::custom_error::ScratchError;",
        ),
        (
            "use crate::files_correction::{correct_to_nearest_dir_path, preprocess_path_for_normalization};",
            "use refact_files::path_utils::preprocess_path_for_normalization;\nuse refact_runtime_api::files_correction::correct_to_nearest_dir_path;",
        ),
    ],
}

for name in FILES:
    engine_path = os.path.join(SRC, name + ".rs")
    crate_path = os.path.join(CRATE, name + ".rs")
    with open(engine_path, "r", encoding="utf-8", newline="") as fh:
        text = fh.read()
    for old, new in REWRITES[name]:
        if old not in text:
            raise SystemExit(f"{name}: pattern not found: {old!r}")
        text = text.replace(old, new, 1)
    with open(crate_path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)
    with open(engine_path, "w", encoding="utf-8", newline="") as fh:
        fh.write(f"pub use refact_http::routers::v1::{name}::*;\n")
    print(f"moved {name}: {len(text.splitlines())} lines")
