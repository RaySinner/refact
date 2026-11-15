"""Z-3 move: retarget `mcp_marketplace` off its only engine blocker, then harvest it.

Strategy per the card (top blocker #1, `integrations::mcp::mcp_resources`):
the crate-side implementation already exists (`refact_runtime_api::integrations_mcp_resources::
remove_indexed_resources` with explicit `cache_dir`/`vec_db` args, moved by R-3). So this card
calls it directly with explicit args instead of going through the engine's
`Weak<GlobalContext>` wrapper, and the file becomes movable -> harvested into `refact-http`.

Every replacement is an exact multi-line string with an expected occurrence count; a mismatch
aborts the run. Re-runnable from the base text (regenerates both files every time).

Run: python tools/dev/z3-move.py
"""
import os
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENGINE = ROOT / "refact-agent" / "engine"
ENGINE_FILE = ENGINE / "src" / "http" / "routers" / "v1" / "mcp_marketplace.rs"
CRATE_FILE = ENGINE / "crates" / "refact-http" / "src" / "routers" / "v1" / "mcp_marketplace.rs"
CRATE_MOD = ENGINE / "crates" / "refact-http" / "src" / "routers" / "v1" / "mod.rs"
CRATE_CARGO = ENGINE / "crates" / "refact-http" / "Cargo.toml"

SHIM = "pub use refact_http::routers::v1::mcp_marketplace::*;\n"

# (old, new, expected occurrences in the engine text)
REPLACEMENTS = [
    (
        "use crate::app_state::AppState;\n"
        "use crate::custom_error::ScratchError;\n"
        "use crate::global_context::{GlobalContext, SharedGlobalContext};\n"
        "use crate::http::routers::v1::mcp_marketplace_sources::{\n",
        "use refact_runtime_api::AppState;\n"
        "use refact_core::custom_error::ScratchError;\n"
        "use refact_runtime_api::global_context::{GlobalContext, SharedGlobalContext};\n"
        "use crate::routers::v1::mcp_marketplace_sources::{\n",
        1,
    ),
    (
        "use crate::integrations::mcp::mcp_naming;\n",
        "use refact_integrations::mcp::mcp_naming;\n",
        1,
    ),
    (
        "#[cfg(test)]\n"
        "use crate::http::routers::v1::mcp_marketplace_sources::{OFFICIAL_MCP_SOURCE_ID, SMITHERY_SOURCE_ID};\n",
        "#[cfg(test)]\n"
        "use crate::routers::v1::mcp_marketplace_sources::{OFFICIAL_MCP_SOURCE_ID, SMITHERY_SOURCE_ID};\n",
        1,
    ),
    (
        '    serde_json::from_str(include_str!(\n'
        '        "../../../yaml_configs/mcp_marketplace_index.json"\n'
        "    ))\n",
        '    serde_json::from_str(concat!(\n'
        '        env!("CARGO_MANIFEST_DIR"),\n'
        '        "/../../src/yaml_configs/mcp_marketplace_index.json"\n'
        "    ))\n",
        1,
    ),
    (
        "crate::integrations::setting_up_integrations::write_config_atomically",
        "refact_runtime_api::integrations::setting_up_integrations::write_config_atomically",
        1,
    ),
    (
        "crate::integrations::mcp::integr_mcp_common::tool_catalog_cache_remove",
        "refact_integrations::mcp::tool_catalog_cache::tool_catalog_cache_remove",
        2,
    ),
    (
        "crate::integrations::mcp::mcp_resources::remove_indexed_resources(\n"
        "        Arc::downgrade(&gcx),\n"
        "        config_path_str.clone(),\n"
        "    )\n",
        "refact_runtime_api::integrations_mcp_resources::remove_indexed_resources(\n"
        "        gcx.cache_dir.clone(),\n"
        "        gcx.vec_db.clone(),\n"
        "        config_path_str.clone(),\n"
        "    )\n",
        2,
    ),
    (
        "crate::integrations::running_integrations::load_integrations",
        "refact_runtime_api::integrations_runtime::load_integrations",
        1,
    ),
    (
        "crate::integrations::mcp::mcp_auth::",
        "refact_integrations::mcp::mcp_auth::",
        1,
    ),
    (
        "crate::integrations::mcp::mcp_path_resolution::",
        "refact_integrations::mcp::mcp_path_resolution::",
        1,
    ),
    (
        "use crate::http::routers::v1::mcp_marketplace_sources::",
        "use crate::routers::v1::mcp_marketplace_sources::",
        5,
    ),
]

CARGO_DEPS = [
    ("percent-encoding = \"2.3\"\n", "percent-encoding"),
    ("reqwest = { workspace = true }\n", "reqwest"),
]
CARGO_ANCHOR = "serde_json = { workspace = true }\n"


def main():
    base = ENGINE_FILE.read_text(encoding="utf-8")
    if base.strip() == SHIM.strip():
        print("engine file is already the shim; regenerate not possible (re-run against base commit)")
        return 1

    text = base
    for old, new, expected in REPLACEMENTS:
        found = text.count(old)
        if found != expected:
            print("ABORT: %d occurrence(s) of %r, expected %d" % (found, old[:70], expected))
            return 1
        text = text.replace(old, new)

    leftover = sorted(set(re.findall(r"crate::(?:integrations|app_state|global_context|custom_error|http)\b", text)))
    if leftover:
        print("ABORT: unreplaced engine paths remain: %s" % leftover)
        return 1

    CRATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    CRATE_FILE.write_text(text, encoding="utf-8")
    ENGINE_FILE.write_text(SHIM, encoding="utf-8")

    mod_text = CRATE_MOD.read_text(encoding="utf-8")
    if "pub mod mcp_marketplace;\n" not in mod_text:
        mod_text = mod_text.replace(
            "pub mod mcp_marketplace_sources;\n",
            "pub mod mcp_marketplace;\npub mod mcp_marketplace_sources;\n",
        )
        CRATE_MOD.write_text(mod_text, encoding="utf-8")

    cargo = CRATE_CARGO.read_text(encoding="utf-8")
    for line, dep in CARGO_DEPS:
        if re.search(r"(?m)^%s\b" % re.escape(dep), cargo):
            continue
        if CARGO_ANCHOR not in cargo:
            print("ABORT: cannot place dep %s in Cargo.toml" % dep)
            return 1
        cargo = cargo.replace(CARGO_ANCHOR, line + CARGO_ANCHOR, 1)
    CRATE_CARGO.write_text(cargo, encoding="utf-8")

    print("moved: %d bytes -> %s" % (len(text), CRATE_FILE))
    print("engine shim: %s" % ENGINE_FILE)
    print("crate mod decl + Cargo deps updated")
    return 0


if __name__ == "__main__":
    sys.exit(main())
