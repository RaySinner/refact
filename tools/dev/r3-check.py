"""R-3 verification harness (in-repo only; the build guard refuses cargo from agent cards).

Usage:  python tools/dev/r3-check.py

Proves the three moves statically:

  R1  every moved body is byte-identical to its pre-move text (plus the documented
      rewrite table / widening);
  R2  `refact-http`, `refact-integrations` and `refact-runtime-api` never gain a
      `refact-lsp` edge, and every crate root the moved code names is declared;
  R3  the engine `mcp_config_sharing.rs` is a glob shim and keeps every
      `#[cfg(test)]` item verbatim; the shim prelude re-imports what the tests reach;
  R4  no symbol was duplicated or lost: each moved definition exists exactly once,
      on the crate side;
  R5  the engine symbols that used to be engine-resident now resolve through the
      crate (no `crate::` engine path left in the crate files);
  R6  no route path / handler name / JSON payload changed: no file other than the
      documented ones is touched;
  R7  delimiters balanced, no merge markers;
  R8  the honesty census re-run reports the new movable/blocked numbers and
      `mcp_config_sharing` is no longer a blocked candidate.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENG = os.path.join(ROOT, "refact-agent", "engine")
V1 = os.path.join(ENG, "src", "http", "routers", "v1")
C_V1 = os.path.join(ENG, "crates", "refact-http", "src", "routers", "v1")
INTEG = os.path.join(ENG, "crates", "refact-integrations")
RTAPI = os.path.join(ENG, "crates", "refact-runtime-api")

BASE = os.environ.get("R3_BASE", "60c7da99f")
MOVED_ROUTER = "mcp_config_sharing"

FAILURES = []
CHECKS = [0]


def read(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def normalise(text):
    return text.replace("\r\n", "\n")


def git(*args):
    return subprocess.run(
        ["git", "-C", ROOT, *args], capture_output=True, check=False
    ).stdout.decode("utf-8", errors="replace")


def check(cond, message):
    CHECKS[0] += 1
    if not cond:
        FAILURES.append(message)


def balance(text):
    stack, pairs, i = [], {")": "(", "]": "[", "}": "{"}, 0
    while i < len(text):
        if text[i : i + 2] == "//":
            i = text.find("\n", i)
            if i == -1:
                break
            continue
        ch = text[i]
        if ch == '"':
            i += 1
            while i < len(text) and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
            i += 1
            continue
        if ch == "'":
            m = re.match(r"'(\\.|[^\\'])'", text[i:])
            if m:
                i += m.end()
                continue
        if ch in "([{":
            stack.append(ch)
        elif ch in ")]}":
            if not stack or stack.pop() != pairs[ch]:
                return False
        i += 1
    return not stack


CFG_TEST = re.compile(r"(?m)^#\[cfg\(test\)\]\n")


def cfg_test_items(text):
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
    return items, out


def declared_deps(cargo_path):
    deps = set()
    for line in read(cargo_path).splitlines():
        m = re.match(r"^([A-Za-z0-9_-]+)\s*=", line)
        if m:
            deps.add(m.group(1).replace("-", "_"))
    return deps


def lock_entry_names(crate):
    lock = read(os.path.join(ENG, "Cargo.lock"))
    start = lock.find('name = "%s"' % crate)
    block = lock[start : lock.find("[[package]]", start)]
    return {
        re.sub(r" .*$", "", entry).replace("-", "_")
        for entry in re.findall(r'^ "([^"]+)",$', block, re.M)
    }


ALLOWED = {
    "refact-agent/engine/src/http/routers/v1/%s.rs" % MOVED_ROUTER,
    "refact-agent/engine/crates/refact-http/src/routers/v1/%s.rs" % MOVED_ROUTER,
    "refact-agent/engine/crates/refact-http/src/routers/v1/mod.rs",
    "refact-agent/engine/src/integrations/mcp/integr_mcp_common.rs",
    "refact-agent/engine/src/integrations/mcp/mcp_resources.rs",
    "refact-agent/engine/crates/refact-integrations/src/mcp/mod.rs",
    "refact-agent/engine/crates/refact-integrations/src/mcp/tool_catalog_cache.rs",
    "refact-agent/engine/crates/refact-runtime-api/src/lib.rs",
    "refact-agent/engine/crates/refact-runtime-api/src/integrations_mcp_resources.rs",
    "refact-agent/engine/Cargo.lock",
    "tools/dev/r3-audit.py",
    "tools/dev/r3-move.py",
    "tools/dev/r3-check.py",
}


def main():
    print("# base commit: %s" % BASE)

    # ---------------------------------------------------------------- R6 scope
    changed = set(git("diff", "--name-only", BASE).split())
    untracked = {
        line[3:].strip()
        for line in git("status", "--porcelain", "--untracked-files=all").splitlines()
        if line[:2] == "??"
    }
    unexpected = sorted((changed | untracked) - ALLOWED)
    check(not unexpected, "R6 no unrelated file changed (got %s)" % unexpected)

    # ------------------------------------------------------- R2 no crate->engine
    for crate in ("refact-http", "refact-integrations", "refact-runtime-api"):
        manifest = read(os.path.join(ENG, "crates", crate, "Cargo.toml"))
        check("refact-lsp" not in manifest, "R2 %s/Cargo.toml must not depend on refact-lsp" % crate)

    # ---------------------------------------------- R1/R3/R5 the moved router
    engine_text = normalise(read(os.path.join(V1, MOVED_ROUTER + ".rs")))
    crate_text = normalise(read(os.path.join(C_V1, MOVED_ROUTER + ".rs")))
    base_text = normalise(git("show", "%s:%s" % (BASE, "refact-agent/engine/src/http/routers/v1/%s.rs" % MOVED_ROUTER)))
    check(bool(base_text.strip()), "R0 base source recovered for %s" % MOVED_ROUTER)

    check(
        engine_text.startswith("pub use refact_http::routers::v1::%s::*;" % MOVED_ROUTER),
        "R3 engine file starts with the glob shim",
    )
    check(
        "pub mod %s;" % MOVED_ROUTER in read(os.path.join(C_V1, "mod.rs")),
        "R2 declared in refact-http routers/v1/mod.rs",
    )
    check(
        re.search(r"(?m)^mod %s;" % MOVED_ROUTER, read(os.path.join(ENG, "src", "http", "routers", "v1.rs")))
        is not None,
        "R2 still declared in the engine v1.rs",
    )

    base_items, base_prod = cfg_test_items(base_text)
    check(len(base_items) == 3, "R0 three cfg(test) items in the base (%d)" % len(base_items))
    for item in base_items:
        check(item in engine_text, "R3 cfg(test) item kept verbatim: %r" % item.splitlines()[0])

    # prelude must carry every identifier the retained tests reach
    testmass = "\n".join(base_items)
    for prelude in (
        "use std::collections::HashMap;",
        "use serde_json::{json, Value};",
        "use refact_integrations::mcp::mcp_naming;",
    ):
        check("#[cfg(test)]\n%s\n" % prelude in engine_text, "R3 prelude carries %s" % prelude)
        name = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", prelude)[-1]
        check(name in testmass, "R0 prelude %s really used by the tests" % name)

    # the crate body is the pre-move production body with the documented rewrites
    expected = base_prod
    for pattern in (
        "const REDACTED: &str",
        "fn is_secret_field(",
        "fn redact_secrets(",
        "async fn collect_mcp_yaml_files(",
        "fn apply_secrets_to_config(",
        "fn sanitize_imported_server_name(",
    ):
        check(expected.count(pattern) == 1, "R0 widening anchor unique: %s" % pattern)
        expected = expected.replace(pattern, "pub " + pattern, 1)
    rewrites = [
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
            "crate::integrations::mcp::mcp_resources::remove_indexed_resources(\n        std::sync::Arc::downgrade(&gcx),\n        path.to_string(),\n    )\n    .await;",
            "refact_runtime_api::integrations_mcp_resources::remove_indexed_resources(\n        gcx.cache_dir.clone(),\n        gcx.vec_db.clone(),\n        path.to_string(),\n    )\n    .await;",
        ),
        (
            "crate::integrations::running_integrations::load_integrations",
            "refact_runtime_api::integrations_runtime::load_integrations",
        ),
    ]
    for old, new in rewrites:
        check(expected.count(old) == 1, "R0 rewrite anchor unique: %s" % old.splitlines()[0])
        expected = expected.replace(old, new)
    check(
        expected.rstrip("\n") == crate_text.rstrip("\n"),
        "R1 crate body is the pre-move body plus the documented rewrites",
    )

    stray = sorted(set(re.findall(r"crate::[\w:]*", crate_text)))
    check(not stray, "R5 no `crate::` engine path left in the crate file (%s)" % stray)
    handlers = set(re.findall(r"(?m)^pub async fn (handle_[A-Za-z0-9_]+)", base_text))
    check(bool(handlers), "R0 base exposes handlers")
    check(
        all(re.search(r"(?m)^pub async fn %s" % h, crate_text) for h in handlers),
        "R1 every handler preserved (%s)" % sorted(handlers),
    )

    for name, text in (("crate", crate_text), ("engine", engine_text)):
        check(balance(text), "R7 %s file delimiters balanced" % name)
        check("<<<<<<<" not in text and ">>>>>>>" not in text, "R7 no merge markers (%s)" % name)

    # ------------------------------------------- R1/R4/R5 tool catalog cache move
    cache_path = os.path.join(INTEG, "src", "mcp", "tool_catalog_cache.rs")
    cache_text = normalise(read(cache_path))
    integr_base = normalise(
        git("show", "%s:refact-agent/engine/src/integrations/mcp/integr_mcp_common.rs" % BASE)
    )
    for fn in ("tool_catalog_cache_store", "tool_catalog_cache_get", "tool_catalog_cache_remove"):
        m = re.search(r"(?m)^pub fn %s\(.*?\n\}\n" % fn, integr_base, re.S)
        check(m is not None, "R0 base defines %s" % fn)
        if m:
            check(m.group(0).strip() in cache_text, "R1 %s body moved verbatim" % fn)
    check("pub mod tool_catalog_cache;" in read(os.path.join(INTEG, "src", "mcp", "mod.rs")), "R2 declared")
    engine_integr = normalise(read(os.path.join(ENG, "src", "integrations", "mcp", "integr_mcp_common.rs")))
    check(
        "static MCP_TOOL_CATALOG_CACHE" not in engine_integr,
        "R4 the cache static is gone from the engine",
    )
    check(
        "pub use refact_integrations::mcp::tool_catalog_cache::{" in engine_integr,
        "R5 engine re-exports the moved accessors",
    )
    check(
        "tool_catalog_cache_store(&config_path, &tools);" in engine_integr
        and "advance_mcp_catalog_generation(&gcx);" in engine_integr,
        "R1 call sites untouched",
    )
    deps = declared_deps(os.path.join(INTEG, "Cargo.toml"))
    check("rmcp" in deps, "R2 refact-integrations declares rmcp")

    # --------------------------------------- R1/R4/R5 mcp_resources move
    rt_path = os.path.join(RTAPI, "src", "integrations_mcp_resources.rs")
    rt_text = normalise(read(rt_path))
    resources_base = normalise(
        git("show", "%s:refact-agent/engine/src/integrations/mcp/mcp_resources.rs" % BASE)
    )
    m = re.search(r"(?m)^fn server_name_for_path\(config_path: &str\) -> String \{.*?\n\}\n", resources_base, re.S)
    check(m is not None, "R0 base defines server_name_for_path")
    if m:
        check(
            m.group(0).replace("fn server_name_for_path", "pub fn server_name_for_path", 1).strip() in rt_text,
            "R1 server_name_for_path body moved verbatim (widened)",
        )
    body = re.search(r"(?m)^pub async fn remove_indexed_resources\(.*?\n\}\n", resources_base, re.S)
    check(body is not None, "R0 base defines remove_indexed_resources")
    if body:
        core = body.group(0).split("let server_name = server_name_for_path(&config_path);", 1)[1]
        check(
            core.strip() in rt_text,
            "R1 remove_indexed_resources body moved verbatim after the gcx split",
        )
        check("gcx" not in rt_text, "R1 the crate version takes explicit args, not gcx")
    check(
        "pub mod integrations_mcp_resources;" in read(os.path.join(RTAPI, "src", "lib.rs")),
        "R2 declared in refact-runtime-api/lib.rs",
    )
    engine_resources = normalise(read(os.path.join(ENG, "src", "integrations", "mcp", "mcp_resources.rs")))
    check(
        engine_resources.count("fn server_name_for_path(") == 0,
        "R4 server_name_for_path is no longer defined in the engine",
    )
    check(
        "use refact_runtime_api::integrations_mcp_resources::server_name_for_path;" in engine_resources,
        "R5 engine imports the moved helper",
    )
    check(
        "refact_runtime_api::integrations_mcp_resources::remove_indexed_resources(" in engine_resources,
        "R5 engine wrapper delegates to the crate",
    )
    check(
        re.search(
            r"(?m)^pub async fn remove_indexed_resources\(gcx_weak: Weak<GlobalContext>, config_path: String\)",
            engine_resources,
        )
        is not None,
        "R1 the engine wrapper keeps the original signature",
    )
    rt_deps = declared_deps(os.path.join(RTAPI, "Cargo.toml"))
    check("refact_core" in rt_deps, "R2 refact-runtime-api declares refact-core")
    for name in ("refact-integrations", "refact-runtime-api"):
        check(
            name.replace("-", "_") in lock_entry_names("refact-http"),
            "R2 %s present in the refact-http lock entry" % name,
        )
    http_manifest = read(os.path.join(ENG, "crates", "refact-http", "Cargo.toml"))
    for dep in ("refact-integrations", "refact-runtime-api", "refact-core", "chrono", "tracing", "serde_yaml"):
        check(dep in http_manifest, "R2 refact-http declares %s" % dep)

    # -------------------------------------------------------------- R8 census
    target_dir = os.path.join(ENG, "target")
    os.makedirs(target_dir, exist_ok=True)
    census = subprocess.run(
        [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), "http"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    print(census.stdout.strip())
    report = read(os.path.join(target_dir, "http_movable_census.txt"))
    check(
        re.search(r"(?m)^\s*\d+\s+http::routers::v1::mcp_config_sharing\b", report) is None,
        "R8 mcp_config_sharing is no longer blocked or movable (it is a shim)",
    )
    check("MOVABLE now: " in report, "R8 census report present")

    print("%d checks, %d failures" % (CHECKS[0], len(FAILURES)))
    for failure in FAILURES:
        print("FAIL %s" % failure)
    return 1 if FAILURES else 0


if __name__ == "__main__":
    sys.exit(main())
