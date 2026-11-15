"""Z-6 static verification for the integr_cmdline(+_service) -> refact-runtime-api move.

Usage:  python tools/dev/z6-check.py

Nothing here compiles Rust (see the card: `engine-build.ps1 -Mode check` is the
compile gate; this script is the structural gate). Every check is deterministic
and reads only files in the worktree.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATE = os.path.join(ENGINE, "crates", "refact-runtime-api")
CRATE_SRC = os.path.join(CRATE, "src")


def _crate_modules():
    """Module set of the crate, including root-level re-exports."""
    mods = set()
    for root, dirs, files in os.walk(CRATE_SRC):
        dirs[:] = [d for d in dirs if d not in ("target", ".git")]
        for name in files:
            if not name.endswith(".rs"):
                continue
            rel = os.path.relpath(os.path.join(root, name), CRATE_SRC)[:-3].replace(os.sep, "::")
            if rel.endswith("::mod"):
                rel = rel[: -len("::mod")]
            mods.add(rel)
            for part in rel.split("::"):
                mods.add(part)
    mods.add("Tool")
    lib = open(os.path.join(CRATE_SRC, "lib.rs"), encoding="utf-8", errors="replace").read()
    for stmt in re.findall(r"pub use [^;]+;", lib, re.S):
        for name in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", stmt.split("::")[-1]):
            if name[:1].isupper() or name.islower():
                mods.add(name)
        head = stmt.split("::")
        if len(head) > 1:
            mods.add(re.sub(r"\W", "", head[0].replace("pub use", "")))
    return mods


mods_all = _crate_modules()

SHIM_CMD = os.path.join(ENGINE, "src", "integrations", "integr_cmdline.rs")
SHIM_SVC = os.path.join(ENGINE, "src", "integrations", "integr_cmdline_service.rs")
MOVED_CMD = os.path.join(CRATE_SRC, "integrations", "cmdline.rs")
MOVED_SVC = os.path.join(CRATE_SRC, "integrations", "cmdline_service.rs")

CHECKS = []


def check(ok, label, detail=""):
    CHECKS.append((bool(ok), label, detail))


def read(p):
    with open(p, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def cfg_test_free(text):
    """Return the text with every `#[cfg(test)]`-attributed item removed."""
    pat = re.compile(r"(?m)^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]")
    while True:
        m = pat.search(text)
        if m is None:
            return text
        rest = text[m.end():].lstrip()
        if rest.startswith("mod ") or rest.startswith("pub mod "):
            semi = text.find(";", m.end())
            brace = text.find("{", m.end())
            if semi != -1 and (brace == -1 or semi < brace):
                text = text[: m.start()] + text[semi + 1:]
                continue
            if brace == -1:
                return text[: m.start()]
            depth, i = 0, brace
            while i < len(text):
                if text[i] == "{":
                    depth += 1
                elif text[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            text = text[: m.start()] + text[i + 1:]
            continue
        semi = text.find(";", m.end())
        if semi == -1:
            return text[: m.start()]
        text = text[: m.start()] + text[semi + 1:]


def crate_modules():
    mods = set()
    for root, dirs, files in os.walk(CRATE_SRC):
        dirs[:] = [d for d in dirs if d not in ("target", ".git")]
        for name in files:
            if not name.endswith(".rs"):
                continue
            rel = os.path.relpath(os.path.join(root, name), CRATE_SRC)[:-3].replace(os.sep, "::")
            if rel.endswith("::mod"):
                rel = rel[: -len("::mod")]
            mods.add(rel)
            for part in rel.split("::"):
                mods.add(part)
    return mods


def prod_only(text):
    i = text.find("#[cfg(test)]\nmod tests {")
    return text[:i] if i != -1 else text


# ---------------------------------------------------------------- 1. the move
check(os.path.exists(MOVED_CMD), "refact-runtime-api/src/integrations/cmdline.rs exists")
check(os.path.exists(MOVED_SVC), "refact-runtime-api/src/integrations/cmdline_service.rs exists")
check("integrations" in read(os.path.join(CRATE_SRC, "lib.rs")),
      "lib.rs still declares `pub mod integrations`")

MOVED_PRICING = os.path.join(CRATE_SRC, "providers_pricing_runtime.rs")
SHIM_PRICING = os.path.join(ENGINE, "src", "providers", "pricing.rs")
check(os.path.exists(MOVED_PRICING),
      "refact-runtime-api/src/providers_pricing_runtime.rs exists")
check("pub mod providers_pricing_runtime;" in read(os.path.join(CRATE_SRC, "lib.rs")),
      "lib.rs declares `pub mod providers_pricing_runtime;`")
shim_pricing = read(SHIM_PRICING)
check(shim_pricing.splitlines()[0].strip()
      == "pub use refact_runtime_api::providers_pricing_runtime::*;",
      "providers/pricing.rs shim starts with the crate re-export")
moved_pricing = read(MOVED_PRICING)
for name in ["pub async fn lookup_model_pricing", "pub async fn lookup_model_pricing_for_ctx",
             "lookup_provider_custom_pricing_for_ctx"]:
    check(name in moved_pricing, f"moved providers_pricing_runtime.rs declares `{name}`")
check("pub use refact_pricing_core::{" in moved_pricing,
      "providers_pricing_runtime.rs re-exports the pricing-core computors")
check("mod tests" not in moved_pricing,
      "providers_pricing_runtime.rs has no test module (tests stay in the engine)")
check("mod tests {" in shim_pricing and "make_test_gcx" in shim_pricing,
      "providers/pricing.rs keeps the original test module verbatim")
for bad in ["crate::providers::", "crate::chat::ctx", "refact_runtime_api::"]:
    check(bad not in moved_pricing, f"providers_pricing_runtime.rs has no engine path `{bad}`")
for first in sorted(set(re.findall(r"\bcrate::([A-Za-z_][A-Za-z0-9_]*)", moved_pricing))):
    check(first in mods_all, f"`crate::{first}` resolves inside refact-runtime-api")

mod_mod = read(os.path.join(CRATE_SRC, "integrations", "mod.rs"))
check("pub mod cmdline;" in mod_mod, "integrations/mod.rs declares `pub mod cmdline;`")
check("pub mod cmdline_service;" in mod_mod,
      "integrations/mod.rs declares `pub mod cmdline_service;`")

# ------------------------------------------------------- 2. engine-side shims
shim_cmd = read(SHIM_CMD)
shim_svc = read(SHIM_SVC)
check(shim_cmd.splitlines()[0].strip()
      == "pub use refact_runtime_api::integrations::cmdline::*;",
      "integr_cmdline.rs shim starts with the cmdline crate re-export")
check(shim_svc.splitlines()[0].strip()
      == "pub use refact_runtime_api::integrations::cmdline_service::*;",
      "integr_cmdline_service.rs shim starts with the cmdline_service crate re-export")

for label, text, names in [
    ("cmdline", shim_cmd, ["pub struct ToolCmdline", "pub fn execute_blocking_command",
                           "CMDLINE_INTEGRATION_SCHEMA"]),
    ("cmdline_service", shim_svc, ["pub struct ToolService"]),
]:
    body = cfg_test_free(text)
    for name in names:
        check(name not in body,
              f"engine {label} shim carries no prod body for `{name}`")

check(shim_cmd.count("mod tests {") == 1, "integr_cmdline.rs keeps exactly one test module")
check(shim_svc.count("mod tests {") == 1,
      "integr_cmdline_service.rs keeps exactly one test module")
check(shim_cmd.count("#[tokio::test]") + shim_cmd.count("#[test]") >= 12,
      "integr_cmdline.rs test count preserved",
      f"found {shim_cmd.count('#[tokio::test]') + shim_cmd.count('#[test]')}")
check(shim_svc.count("#[tokio::test]") + shim_svc.count("#[test]") >= 2,
      "integr_cmdline_service.rs test count preserved",
      f"found {shim_svc.count('#[tokio::test]') + shim_svc.count('#[test]')}")

# --------------------------------------------- 3. moved bodies keep the API
moved_cmd = read(MOVED_CMD)
moved_svc = read(MOVED_SVC)
for name in ["pub struct CmdlineParam", "pub struct CmdlineToolConfig", "pub struct ToolCmdline",
             "pub async fn execute_blocking_command", "pub fn replace_args",
             "pub fn resolve_cmdline_workdir", "pub const CMDLINE_INTEGRATION_SCHEMA",
             "impl Tool for ToolCmdline", "impl IntegrationTrait for ToolCmdline",
             "pub fn append_status_line", "pub fn exec_extra", "pub fn tool_failed_for_status"]:
    check(name in moved_cmd, f"moved cmdline.rs declares `{name}`")
for name in ["pub struct ToolService", "pub fn parse_service_tool_args",
             "pub const CMDLINE_SERVICE_INTEGRATION_SCHEMA", "impl Tool for ToolService",
             "impl IntegrationTrait for ToolService"]:
    check(name in moved_svc, f"moved cmdline_service.rs declares `{name}`")
check("mod tests" not in moved_cmd, "cmdline.rs has no test module (tests stay in the engine)")
check("mod tests" not in moved_svc,
      "cmdline_service.rs has no test module (tests stay in the engine)")

# the crate module must reach its own siblings, not the engine shim
check("use super::cmdline::{format_output, replace_args, CmdlineToolConfig};" in moved_svc,
      "cmdline_service.rs imports the sibling crate module via `super::cmdline`")

# ---------------------------------- 4. no engine-module path survives the move
FORBIDDEN = [
    "crate::app_state", "crate::exec::", "crate::call_validation", "crate::postprocessing",
    "crate::at_commands::", "crate::custom_error", "crate::integrations::integr_cmdline",
    "crate::scheduler", "crate::subchat", "crate::vecdb", "crate::files_in_workspace",
    "refact_runtime_api::",
]
for label, text in [("cmdline.rs", moved_cmd), ("cmdline_service.rs", moved_svc)]:
    for bad in FORBIDDEN:
        check(bad not in text, f"{label} has no engine path `{bad}`")

mods = crate_modules()
mods.add("Tool")
refs = set()
for text in (moved_cmd, moved_svc):
    refs.update(re.findall(r"\bcrate::([A-Za-z_][A-Za-z0-9_]*)", text))
for first in sorted(refs):
    check(first in mods, f"`crate::{first}` resolves inside refact-runtime-api")

# ------------------------------------------------- 5. dependency hygiene
def manifest(name):
    return read(os.path.join(ENGINE, "crates", name, "Cargo.toml"))


rtapi = read(os.path.join(CRATE, "Cargo.toml"))
for lower in ["refact-exec", "refact-integrations", "refact-postprocessing",
              "refact-buddy-core", "refact-tool-api", "refact-chat-api",
              "refact-pricing-core", "refact-providers", "refact-core"]:
    check("refact-runtime-api" not in manifest(lower),
          f"{lower} has no edge to refact-runtime-api (no crate cycle)")
for dep in ["refact-pricing-core", "shell-escape", "refact-integrations", "refact-exec",
            "refact-postprocessing", "refact-core", "refact-chat-api", "refact-buddy-core"]:
    check(dep in rtapi, f"refact-runtime-api declares `{dep}`")

# ------------------------------------------------ 6. callers still compile
engine_mod = read(os.path.join(ENGINE, "src", "integrations", "mod.rs"))
check("integr_cmdline::ToolCmdline" in engine_mod,
      "engine integrations/mod.rs still constructs `integr_cmdline::ToolCmdline`")
check("integr_cmdline_service::ToolService" in engine_mod,
      "engine integrations/mod.rs still constructs `integr_cmdline_service::ToolService`")

failed = [c for c in CHECKS if not c[0]]
for ok, label, detail in CHECKS:
    if not ok:
        print(f"FAIL: {label}" + (f" ({detail})" if detail else ""))
print(f"VERIFY: {'PASS' if not failed else 'FAIL'} ({len(CHECKS) - len(failed)}/{len(CHECKS)} checks)")
sys.exit(1 if failed else 0)
