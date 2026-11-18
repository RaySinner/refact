"""BN-2 gate: did `chat::context_rebuild::pipeline` really leave the engine?

    python tools/dev/bn2-check.py

The card's acceptance criteria are all structural, so they are asserted here
rather than restated in prose:

* B1 the crate owns the pipeline body (`refact-runtime-api/src/chat_context_rebuild.rs`);
* B2 the engine module is a re-export that keeps every old public name;
* B3 the moved body references no engine module (`crate::chat::`, `crate::tools::`,
  `crate::caps::`, `crate::agentic::`, `crate::app_state::`, `GlobalContext` as a type);
* B4 the compress-chat core lives in `refact-chat-history::chat_compress` and
  `refact-tools` re-exports it;
* B5 the crate dependency graph still has no crate -> `refact-lsp` edge;
* B6 the census no longer reports the pipeline as blocked on `global_context`.

Exit code 0 when every check passes, 1 otherwise.
"""

import os
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
CRATES = os.path.join(ENGINE, "crates")
CENSUS = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")

CRATE_MODULE = os.path.join(CRATES, "refact-runtime-api", "src", "chat_context_rebuild.rs")
PREPARE_MODULE = os.path.join(CRATES, "refact-runtime-api", "src", "chat_prepare.rs")
ENGINE_MODULE = os.path.join(ENGINE, "src", "chat", "context_rebuild", "pipeline.rs")
ENGINE_SHIM = os.path.join(ENGINE, "src", "chat", "context_rebuild", "mod.rs")
COMPRESS_CORE = os.path.join(CRATES, "refact-chat-history", "src", "chat_compress.rs")
COMPRESS_TOOL = os.path.join(CRATES, "refact-tools", "src", "tools", "tool_compress_chat.rs")
HANDOFF_SEAM = os.path.join(ENGINE, "src", "app_state", "handoff.rs")

ENGINE_PATHS = (
    "crate::chat::",
    "crate::tools::",
    "crate::caps::",
    "crate::agentic::",
    "crate::app_state::",
    "crate::global_context::tests",
    "crate::call_validation::",
)

PUBLIC_NAMES = (
    "apply_context_rebuild_with_reason",
    "apply_deterministic_compaction_for_recovery",
    "apply_manual_context_rebuild",
    "drain_pending",
    "preview_manual_context_rebuild",
    "rebuild_session",
)

failures = []


def check(name, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        failures.append(name)


def read(path):
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def production(text):
    """Drop the trailing `#[cfg(test)] mod tests { .. }` block."""
    index = text.find("\n#[cfg(test)]")
    return text if index == -1 else text[:index]


def crate_depends_on_engine(cargo):
    for line in read(cargo).splitlines():
        stripped = line.strip()
        if stripped.startswith("#") or "path" not in stripped:
            continue
        if "refact-lsp" in stripped:
            return True
    return False


def main():
    print("=== BN-2 gate: context rebuild pipeline extraction ===")
    check("B1 crate module present", os.path.isfile(CRATE_MODULE))
    if failures:
        print("\nBN-2 RESULT: FAIL")
        return 1

    crate_src = read(CRATE_MODULE)
    engine_src = read(ENGINE_MODULE)
    engine_prod = production(engine_src)
    crate_prod = production(crate_src)

    missing = [name for name in PUBLIC_NAMES if f"pub async fn {name}(" not in crate_src]
    check("B1b crate exposes every public entry point", not missing, ", ".join(missing))

    leaked = [path for path in ENGINE_PATHS if path in crate_prod]
    check("B3 no engine module path in the moved body", not leaked, ", ".join(leaked))
    check(
        "B3b no engine `GlobalContext` type in the moved body",
        re.search(r"\bArc<GlobalContext>", crate_prod) is None,
    )
    check(
        "B3c crate module takes SharedGlobalContext",
        "SharedGlobalContext" in crate_prod,
    )

    check(
        "B2 engine module is a re-export",
        "pub use refact_runtime_api::chat_context_rebuild::*;" in engine_prod,
    )
    check(
        "B2b engine re-export keeps no production body",
        "pub async fn rebuild_session(" not in engine_prod,
    )
    mod_src = read(ENGINE_SHIM)
    check(
        "B2c parent module keeps its old surface",
        all(name in mod_src for name in PUBLIC_NAMES),
    )

    check("B4a compress core moved to chat-history", os.path.isfile(COMPRESS_CORE))
    compress_core = read(COMPRESS_CORE) if os.path.isfile(COMPRESS_CORE) else ""
    check(
        "B4b compress core is pure (no tool/context dependency)",
        "refact_runtime_api" not in compress_core and "AtCommandsContext" not in compress_core,
    )
    tool_src = read(COMPRESS_TOOL)
    check(
        "B4c refact-tools re-exports the moved core",
        "pub use refact_chat_history::chat_compress::*;" in tool_src,
    )
    check(
        "B4d tool shell no longer defines the moved helpers",
        "pub fn compress_chat_apply_head_output(" not in tool_src,
    )

    check("B4e pending-handoff seam installed", os.path.isfile(HANDOFF_SEAM))
    prepare_src = read(PREPARE_MODULE)
    check(
        "B4f canonical tool builder moved with the rebuild budget check",
        "pub async fn build_canonical_openai_tools(" in prepare_src,
    )

    edges = []
    for crate in ("refact-runtime-api", "refact-chat-api", "refact-chat-history", "refact-tools"):
        cargo = os.path.join(CRATES, crate, "Cargo.toml")
        edges.append((crate, crate_depends_on_engine(cargo) if os.path.isfile(cargo) else False))
    for crate, has_engine in edges:
        print(f"  {crate:20} -> refact-lsp : {'PRESENT (cycle!)' if has_engine else 'absent'}")
    check("B5 no crate depends on the engine", not any(has_engine for _, has_engine in edges))

    report = os.path.join(ENGINE, "target", "chat_movable_census.txt")
    proc = subprocess.run(
        [sys.executable, CENSUS, "chat"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    check("B6 census runs", proc.returncode == 0)
    census = read(report) if os.path.isfile(report) else ""
    left = census.split("### LEFT")[-1].split("### ALREADY-SHIM")[0]
    shim = census.split("### ALREADY-SHIM")[-1]
    check(
        "B6b census no longer blocks the pipeline on global_context",
        "chat::context_rebuild::pipeline" not in left,
    )
    check(
        "B6c census now classifies the pipeline as a pure crate facade",
        "chat::context_rebuild::pipeline" in shim,
    )
    for line in census.splitlines():
        if "chat::context_rebuild::pipeline" in line:
            print(f"  census: {line.strip()}")

    print(f"\nBN-2 RESULT: {'PASS' if not failures else 'FAIL'}")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
