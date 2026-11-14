"""R-3 mover (idempotent).

Three documented, mechanical moves that clear 3 of `http`'s top blockers:

  1. the MCP tool-catalog cache (`static MCP_TOOL_CATALOG_CACHE` + its three
     accessors, which are pure: no `GlobalContext`, no `AppState`) moves from
     the engine `integrations/mcp/integr_mcp_common.rs` into
     `refact-integrations/src/mcp/tool_catalog_cache.rs` (a crate the engine
     already depends on, so nothing new is introduced);
  2. `remove_indexed_resources` + `server_name_for_path` move from the engine
     `integrations/mcp/mcp_resources.rs` into `refact-runtime-api`; `gcx` is
     replaced by explicit arguments (`cache_dir`, `vec_db`) and the engine keeps
     an identical-signature wrapper;
  3. `http/routers/v1/mcp_config_sharing.rs` moves verbatim into `refact-http`
     (engine file becomes a glob shim + verbatim `#[cfg(test)]` mass), which is
     possible only because of 1 and 2.

Every target file is *derived* from the base commit's text, so re-running is a
no-op and a partial application fails loudly. Run from the repo root:
`python tools/dev/r3-move.py`.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENG = os.path.join(ROOT, "refact-agent", "engine")
BASE = os.environ.get("R3_BASE", "60c7da99f")

ENG_INTEGR = "refact-agent/engine/src/integrations/mcp/integr_mcp_common.rs"
ENG_RESOURCES = "refact-agent/engine/src/integrations/mcp/mcp_resources.rs"
ENG_ROUTER = "refact-agent/engine/src/http/routers/v1/mcp_config_sharing.rs"

CACHE_ENTRY = "refact-agent/engine/crates/refact-integrations/src/mcp/tool_catalog_cache.rs"
CACHE_MOD = "refact-agent/engine/crates/refact-integrations/src/mcp/mod.rs"
RESOURCES_ENTRY = "refact-agent/engine/crates/refact-runtime-api/src/integrations_mcp_resources.rs"
RTAPI_LIB = "refact-agent/engine/crates/refact-runtime-api/src/lib.rs"
ROUTER_ENTRY = "refact-agent/engine/crates/refact-http/src/routers/v1/mcp_config_sharing.rs"
ROUTER_MOD = "refact-agent/engine/crates/refact-http/src/routers/v1/mod.rs"

LOG = []


def read(rel):
    with open(os.path.join(ROOT, rel), encoding="utf-8") as fh:
        return fh.read().replace("\r\n", "\n")


def write(rel, text):
    with open(os.path.join(ROOT, rel), "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)


def put(rel, expected, label):
    if os.path.exists(os.path.join(ROOT, rel)) and read(rel) == expected:
        LOG.append("same: %s" % label)
        return
    write(rel, expected)
    LOG.append("write: %s" % label)


def base(rel):
    out = subprocess.run(
        ["git", "-C", ROOT, "show", "%s:%s" % (BASE, rel)], capture_output=True
    ).stdout.decode("utf-8", "replace").replace("\r\n", "\n")
    if not out.strip():
        raise SystemExit("R3-MOVE: cannot read %s at %s" % (rel, BASE))
    return out


def sub_once(text, old, new, label):
    if text.count(old) != 1:
        raise SystemExit("R3-MOVE: %s anchor occurs %d times: %r" % (label, text.count(old), old[:60]))
    return text.replace(old, new, 1)


def add_mod_line(text, after, line, label):
    if line in text:
        LOG.append("already declared: %s" % label)
        return text
    return sub_once(text, after, after + line, label)


# ---------------------------------------------------------------- cfg(test) split

CFG_TEST = re.compile(r"(?m)^#\[cfg\(test\)\]\n")


def split_cfg_items(text):
    items, out = [], text
    while True:
        m = CFG_TEST.search(out)
        if not m:
            break
        start = m.start()
        rest = out[m.end():]
        semi = rest.find(";")
        brace = rest.find("{")
        if brace == -1 or (semi != -1 and semi < brace):
            end = m.end() + semi + 1
        else:
            depth, k = 0, m.end() + brace
            while k < len(out):
                if out[k] == "{":
                    depth += 1
                elif out[k] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                k += 1
            end = k + 1
        items.append(out[start:end])
        out = out[:start] + out[end:]
    return out, items


# ------------------------------------------------------- move 1: tool catalog cache

CACHE_BEFORE = """/// Process-scoped cache of the last successfully listed tool catalog per
/// config path. Lets `mcp_integr_tools` expose tools while a slow server is
/// still starting or reconnecting; execution against a not-yet-connected
/// client fails gracefully in `ToolMCP::tool_execute`.
static MCP_TOOL_CATALOG_CACHE: std::sync::Mutex<
    Option<HashMap<String, (std::time::Instant, Arc<Vec<rmcp::model::Tool>>)>>,
> = std::sync::Mutex::new(None);
const TOOL_CATALOG_CACHE_TTL_SECS: u64 = 1800;
const TOOL_CATALOG_CACHE_MAX_ENTRIES: usize = 64;
const MCP_CONNECT_TIMEOUT_SECS: u64 = 15;
"""

CACHE_AFTER = """pub use refact_integrations::mcp::tool_catalog_cache::{
    tool_catalog_cache_get, tool_catalog_cache_remove, tool_catalog_cache_store,
};
const MCP_CONNECT_TIMEOUT_SECS: u64 = 15;
"""

CACHE_STORE = """pub fn tool_catalog_cache_store(config_path: &str, tools: &[rmcp::model::Tool]) {
    let mut guard = match MCP_TOOL_CATALOG_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let cache = guard.get_or_insert_with(HashMap::new);
    if cache.len() >= TOOL_CATALOG_CACHE_MAX_ENTRIES && !cache.contains_key(config_path) {
        if let Some(oldest_key) = cache
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest_key);
        }
    }
    cache.insert(
        config_path.to_string(),
        (std::time::Instant::now(), Arc::new(tools.to_vec())),
    );
}

"""

CACHE_GET_REMOVE = """pub fn tool_catalog_cache_get(config_path: &str) -> Option<Arc<Vec<rmcp::model::Tool>>> {
    let mut guard = match MCP_TOOL_CATALOG_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let cache = guard.as_mut()?;
    match cache.get(config_path) {
        Some((at, tools)) if at.elapsed() < Duration::from_secs(TOOL_CATALOG_CACHE_TTL_SECS) => {
            Some(Arc::clone(tools))
        }
        Some(_) => {
            cache.remove(config_path);
            None
        }
        None => None,
    }
}

pub fn tool_catalog_cache_remove(config_path: &str) {
    let mut guard = match MCP_TOOL_CATALOG_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(cache) = guard.as_mut() {
        cache.remove(config_path);
    }
}

"""

CACHE_ENTRY_HEADER = """use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

"""


def move_tool_catalog_cache():
    engine_base = base(ENG_INTEGR)
    moved_head = sub_once(engine_base, CACHE_BEFORE, CACHE_AFTER, "cache head")
    moved_head = sub_once(moved_head, CACHE_STORE, "", "cache store")
    moved_head = sub_once(moved_head, CACHE_GET_REMOVE, "", "cache get/remove")
    put(ENG_INTEGR, moved_head, "engine integr_mcp_common.rs (cache moved out)")

    entry = (
        CACHE_ENTRY_HEADER
        + CACHE_BEFORE.replace("const MCP_CONNECT_TIMEOUT_SECS: u64 = 15;\n", "")
        + "\n"
        + CACHE_STORE
        + CACHE_GET_REMOVE
    )
    put(CACHE_ENTRY, entry.rstrip("\n") + "\n", "refact-integrations tool_catalog_cache.rs")

    put(
        CACHE_MOD,
        add_mod_line(
            read(CACHE_MOD),
            "pub mod mcp_path_resolution;\n",
            "pub mod tool_catalog_cache;\n",
            "refact-integrations/mcp/mod.rs",
        ),
        "refact-integrations/mcp/mod.rs",
    )


# --------------------------------------------------------- move 2: mcp_resources

RES_HEADER = """use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex as AMutex;

use refact_core::vecdb_types::VecdbSearch;

"""

RES_SIGNATURE = """pub async fn remove_indexed_resources(
    cache_dir: PathBuf,
    vec_db: Arc<AMutex<Option<Arc<dyn VecdbSearch>>>>,
    config_path: String,
) {
"""

RES_NAME_BLOCK = """fn server_name_for_path(config_path: &str) -> String {
    let path = std::path::Path::new(config_path);
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mcp")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

"""

RES_SPLIT = "    let server_name = server_name_for_path(&config_path);\n"

RES_OLD_SIGNATURE = (
    "pub async fn remove_indexed_resources(gcx_weak: Weak<GlobalContext>, config_path: String) {\n"
)

RES_OLD_HEAD = """    let gcx = match gcx_weak.upgrade() {
        Some(g) => g,
        None => return,
    };

    let (cache_dir, vec_db) = { (gcx.cache_dir.clone(), gcx.vec_db.clone()) };

"""

RES_WRAPPER = """pub async fn remove_indexed_resources(gcx_weak: Weak<GlobalContext>, config_path: String) {
    let Some(gcx) = gcx_weak.upgrade() else {
        return;
    };
    refact_runtime_api::integrations_mcp_resources::remove_indexed_resources(
        gcx.cache_dir.clone(),
        gcx.vec_db.clone(),
        config_path,
    )
    .await;
}
"""

RES_IMPORT = "use refact_runtime_api::integrations_mcp_resources::server_name_for_path;\n"


def build_resources_module(engine_base):
    name_block = re.search(r"(?m)^" + re.escape(RES_NAME_BLOCK), engine_base)
    if not name_block:
        raise SystemExit("R3-MOVE: base server_name_for_path not found")
    start = engine_base.index(RES_OLD_SIGNATURE)
    rest = engine_base[start + len(RES_OLD_SIGNATURE) :]
    if not rest.startswith(RES_OLD_HEAD):
        raise SystemExit("R3-MOVE: base remove_indexed_resources head mismatch")
    tail = rest[len(RES_OLD_HEAD) :]
    end = tail.index("\n}\n") + len("\n}\n")
    return (
        RES_HEADER
        + RES_NAME_BLOCK.replace("fn server_name_for_path", "pub fn server_name_for_path", 1).rstrip("\n")
        + "\n\n"
        + RES_SIGNATURE
        + RES_SPLIT
        + tail[:end].split(RES_SPLIT, 1)[1]
    )


def move_mcp_resources():
    engine_base = base(ENG_RESOURCES)
    put(
        RESOURCES_ENTRY,
        build_resources_module(engine_base),
        "refact-runtime-api integrations_mcp_resources.rs",
    )

    moved = sub_once(
        engine_base,
        "use crate::global_context::GlobalContext;\n",
        "use crate::global_context::GlobalContext;\n" + RES_IMPORT,
        "resources import",
    )
    moved = sub_once(moved, RES_NAME_BLOCK, "", "resources name block")
    start = moved.index(RES_OLD_SIGNATURE)
    rest = moved[start + len(RES_OLD_SIGNATURE) :]
    end = rest.index("\n}\n") + len("\n}\n")
    put(ENG_RESOURCES, moved[:start] + RES_WRAPPER + rest[end:], "engine mcp_resources.rs")

    put(
        RTAPI_LIB,
        add_mod_line(
            read(RTAPI_LIB),
            "pub mod integr_abstract;\n",
            "pub mod integrations_mcp_resources;\n",
            "refact-runtime-api/lib.rs",
        ),
        "refact-runtime-api/lib.rs",
    )


# ---------------------------------------------------- move 3: mcp_config_sharing

REWRITES = [
    ("use crate::app_state::AppState;", "use refact_runtime_api::AppState;"),
    (
        "use crate::custom_error::ScratchError;",
        "use refact_core::custom_error::ScratchError;",
    ),
    (
        "use crate::integrations::mcp::mcp_naming;",
        "use refact_integrations::mcp::mcp_naming;",
    ),
    (
        "crate::global_context::GlobalContext",
        "refact_runtime_api::global_context::GlobalContext",
    ),
    (
        "crate::integrations::mcp::integr_mcp_common::tool_catalog_cache_remove",
        "refact_integrations::mcp::tool_catalog_cache::tool_catalog_cache_remove",
    ),
    (
        "crate::integrations::mcp::mcp_resources::remove_indexed_resources(\n"
        "        std::sync::Arc::downgrade(&gcx),\n"
        "        path.to_string(),\n"
        "    )\n"
        "    .await;",
        "refact_runtime_api::integrations_mcp_resources::remove_indexed_resources(\n"
        "        gcx.cache_dir.clone(),\n"
        "        gcx.vec_db.clone(),\n"
        "        path.to_string(),\n"
        "    )\n"
        "    .await;",
    ),
    (
        "crate::integrations::running_integrations::load_integrations",
        "refact_runtime_api::integrations_runtime::load_integrations",
    ),
]

# Non-pub production items the kept `#[cfg(test)]` mass reaches through `super::*`.
WIDEN = [
    "const REDACTED: &str",
    "fn is_secret_field(",
    "fn redact_secrets(",
    "async fn collect_mcp_yaml_files(",
    "fn apply_secrets_to_config(",
    "fn sanitize_imported_server_name(",
]

# Imports the kept `#[cfg(test)]` mass needs: a glob re-export carries only public
# items and never a module's own `use` statements.
PRELUDE = [
    "use std::collections::HashMap;",
    "use serde_json::{json, Value};",
    "use refact_integrations::mcp::mcp_naming;",
]


def move_mcp_config_sharing():
    engine_base = base(ENG_ROUTER)
    prod, cfg_items = split_cfg_items(engine_base)
    for pattern in WIDEN:
        prod = sub_once(prod, pattern, "pub " + pattern, "WIDEN %s" % pattern[:32])
    for old, new in REWRITES:
        prod = sub_once(prod, old, new, "REWRITE %s" % old.splitlines()[0][:48])
    stray = sorted(set(re.findall(r"crate::[\w:]*", prod)))
    if stray:
        raise SystemExit("R3-MOVE: stray engine refs left in mcp_config_sharing: %s" % stray)
    put(ROUTER_ENTRY, prod.rstrip("\n") + "\n", "refact-http mcp_config_sharing.rs")

    if len(cfg_items) != 3:
        raise SystemExit("R3-MOVE: expected 3 cfg(test) items, got %d" % len(cfg_items))
    shim = "pub use refact_http::routers::v1::mcp_config_sharing::*;\n\n"
    for line in PRELUDE:
        shim += "#[cfg(test)]\n%s\n" % line
    for item in cfg_items:
        shim += "\n" + item.rstrip("\n") + "\n"
    put(ENG_ROUTER, shim, "engine mcp_config_sharing.rs shim")

    put(
        ROUTER_MOD,
        add_mod_line(
            read(ROUTER_MOD),
            "pub mod mcp_interactions;\n",
            "pub mod mcp_config_sharing;\n",
            "refact-http/routers/v1/mod.rs",
        ),
        "refact-http/routers/v1/mod.rs",
    )


def main():
    move_tool_catalog_cache()
    move_mcp_resources()
    move_mcp_config_sharing()
    print("\n".join(LOG))
    return 0


if __name__ == "__main__":
    sys.exit(main())
