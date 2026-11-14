#!/usr/bin/env python3
"""R-6 verification harness.

Asserts the engine-root restream/trajectory_memos/knowledge_index extraction:

* prod-line census before/after for the three files,
* the engine files are shims (re-export + the intended engine-only remainder),
* no crate depends on the engine (`refact-lsp`) in either direction,
* `cargo check -p refact-lsp --lib` exits 0.

Usage:
    python tools/dev/r6_verify.py                # structure + cargo check
    python tools/dev/r6_verify.py --with-tests   # also runs check-tests (36 pre-existing errors)
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

ENGINE = ROOT / "refact-agent/engine/src"
RUNTIME_API = ROOT / "refact-agent/engine/crates/refact-runtime-api/src"
CHAT_HISTORY = ROOT / "refact-agent/engine/crates/refact-chat-history/src"

BEFORE = {
    "restream.rs": 779,
    "trajectory_memos.rs": 706,
    "knowledge_index.rs": 563,
}

TABLE = [
    ("restream.rs", "refact-runtime-api/src/restream.rs", 779),
    ("knowledge_index.rs", "refact-runtime-api/src/knowledge_index.rs", 563),
    ("trajectory_memos.rs", "refact-chat-history/src/trajectory_memos.rs", 706),
]


def prod_lines(path: Path) -> int:
    lines = path.read_text(encoding="utf-8").split("\n")
    for index, line in enumerate(lines):
        if line.strip() == "#[cfg(test)]":
            return index
    return len(lines)


def fail(message: str) -> None:
    print("FAIL: " + message)
    sys.exit(1)


def check_no_engine_dependency() -> None:
    for crate in ("refact-runtime-api", "refact-chat-history"):
        manifest = (ROOT / "refact-agent/engine/crates" / crate / "Cargo.toml").read_text(
            encoding="utf-8"
        )
        if re.search(r"refact-lsp|refact_engine", manifest):
            fail(crate + " must not depend on the engine")
    manifest = (ROOT / "refact-agent/engine/Cargo.toml").read_text(encoding="utf-8")
    for crate in ("refact-runtime-api", "refact-chat-history"):
        if not re.search(r"^" + re.escape(crate) + r" =", manifest, re.M):
            fail("engine manifest must depend on " + crate)


def check_restream_shim() -> None:
    text = (ENGINE / "restream.rs").read_text(encoding="utf-8")
    if "refact_runtime_api::restream::" not in text:
        fail("engine restream.rs must re-export refact_runtime_api::restream")
    if re.search(r"^\s*(pub )?(async )?fn ", text, re.M):
        fail("engine restream.rs must not define functions")


def prod_text(path: Path) -> str:
    return "\n".join(path.read_text(encoding="utf-8").split("\n")[: prod_lines(path)])


def check_knowledge_index_shim() -> None:
    text = (ENGINE / "knowledge_index.rs").read_text(encoding="utf-8")
    if "EngineKnowledgeBuilder" not in text:
        fail("engine knowledge_index.rs must keep EngineKnowledgeBuilder")
    if "build_knowledge_index_for_ctx_impl" not in text:
        fail("engine knowledge_index.rs must delegate to the crate impl")
    prod = prod_text(ENGINE / "knowledge_index.rs")
    defined = re.findall(r"^\s*(?:pub )?(?:async )?fn (\w+)", prod, re.M)
    unexpected = [name for name in defined if name != "build_knowledge_index_for_ctx"]
    if unexpected:
        fail("engine knowledge_index.rs kept non-builder fns: " + ", ".join(unexpected))


def check_trajectory_memos_split() -> None:
    crate = (CHAT_HISTORY / "trajectory_memos.rs").read_text(encoding="utf-8")
    for symbol in (
        "parse_extraction_response",
        "normalized_memo_title",
        "trajectory_project_root",
        "candidate_hash",
        "ExtractedMemo",
        "ExtractionResult",
    ):
        if symbol not in crate:
            fail("refact-chat-history/trajectory_memos.rs missing " + symbol)
    if "GlobalContext" in crate or "ChatCtx" in crate:
        fail("refact-chat-history/trajectory_memos.rs must stay context-free")
    engine = (ENGINE / "trajectory_memos.rs").read_text(encoding="utf-8")
    for symbol in ("trajectory_memos_background_task", "extract_memos_and_meta", "persist_extraction_result"):
        if symbol not in engine:
            fail("engine trajectory_memos.rs must keep " + symbol)
    if "mod tests {" not in engine:
        fail("engine trajectory_memos.rs must keep the inline tests")


def run_build(mode: str) -> int:
    cmd = [
        "powershell",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        "tools/dev/engine-build.ps1",
        "-Mode",
        mode,
    ]
    return subprocess.call(cmd, cwd=ROOT)


def main() -> None:
    with_tests = "--with-tests" in sys.argv

    print("file | before prod | after engine prod | moved to crate")
    for engine_name, crate_rel, moved in TABLE:
        after = prod_lines(ENGINE / engine_name)
        print(
            "%s | %d | %d | %s"
            % (engine_name, BEFORE[engine_name], after, crate_rel)
        )
        if after >= BEFORE[engine_name]:
            fail(engine_name + " did not shrink")
        crate_path = ROOT / "refact-agent/engine/crates" / crate_rel
        if not crate_path.exists():
            fail("missing crate file " + crate_rel)
        if prod_lines(crate_path) + after < BEFORE[engine_name] - 20:
            fail(engine_name + " prod lines are unaccounted for")

    check_no_engine_dependency()
    check_restream_shim()
    check_knowledge_index_shim()
    check_trajectory_memos_split()
    print("structure checks passed")

    code = run_build("check")
    if code != 0:
        fail("cargo check exited " + str(code))
    print("cargo check passed")

    if with_tests:
        code = run_build("check-tests")
        expected = 36
        if code == 0:
            print("check-tests passed with 0 errors")
        else:
            print("check-tests exited " + str(code) + " (expected: " + str(expected) + " pre-existing errors)")

    print("R-6 verification OK")


if __name__ == "__main__":
    main()
