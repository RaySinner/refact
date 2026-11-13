"""L-5 (K2) verification harness: static proof that the file-edit move is behaviour-preserving.

Complements the real build (`tools/dev/engine-build.ps1`). Every assertion is a
filesystem/text check with no shell separators.

Run: python tools/dev/k2-check.py
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
CRATE = ENG / "crates" / "refact-tools"
DST = CRATE / "src" / "tools"

# 13 K2 files: 10 moved, 2 already-shim, 1 engine-only facade (file_edit/mod.rs)
MOVED = [
    ("file_edit/auxiliary.rs", "file_edit::auxiliary", 19),
    ("file_edit/tool_apply_patch.rs", "file_edit::tool_apply_patch", 3),
    ("file_edit/tool_create_textdoc.rs", "file_edit::tool_create_textdoc", 0),
    ("file_edit/tool_undo_textdoc.rs", "file_edit::tool_undo_textdoc", 0),
    ("file_edit/tool_update_textdoc.rs", "file_edit::tool_update_textdoc", 0),
    ("file_edit/tool_update_textdoc_anchored.rs", "file_edit::tool_update_textdoc_anchored", 0),
    ("file_edit/tool_update_textdoc_by_lines.rs", "file_edit::tool_update_textdoc_by_lines", 0),
    ("file_edit/tool_update_textdoc_regex.rs", "file_edit::tool_update_textdoc_regex", 0),
    ("tool_mv.rs", "tool_mv", 0),
    ("tool_rm.rs", "tool_rm", 2),
]
ALREADY_SHIM = [
    ("file_edit/openai_apply_patch.rs", "pub use refact_file_edit_core::openai_apply_patch::*;"),
    ("file_edit/undo_history.rs", "pub use refact_tool_api::undo_history::*;"),
]
UNTOUCHED = ["ctx.rs", "tools_description.rs", "tool_shell.rs", "tools_list.rs",
             "tests_schema.rs", "shell_gate.rs", "file_edit/mod.rs", "mod.rs"]

checks = []


def check(name, ok, detail=""):
    checks.append((name, ok, detail))
    print(("PASS " if ok else "FAIL ") + name + ((" -- " + detail) if detail and not ok else ""))


def read(p):
    return p.read_bytes().decode("utf-8")


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True).stdout.strip()


# 1. crate files exist, carry production code, and contain no engine-only crate:: path
for rel, _mod, _tests in MOVED:
    body = read(DST / rel)
    check(f"crate {rel} exists with production code", len(body) > 200, f"{len(body)} bytes")
    stray = sorted({m for m in re.findall(r"crate::[\w:]*", body)
                    if not m.startswith("crate::tools::")})
    check(f"crate {rel} has no engine-only crate:: refs", not stray, str(stray))
    check(f"crate {rel} carries no #[cfg(test)] block", "#[cfg(test)]" not in body)

# 2. engine files are shims, keep their original tests verbatim
for rel, mod, want_tests in MOVED:
    body = read(SRC / rel)
    check(f"engine {rel} starts with the re-export shim",
          body.startswith(f"pub use refact_tools::tools::{mod}::*;"), body.splitlines()[0] if body else "")
    got = len(re.findall(r"#\[(?:tokio::)?test[\(\]]", body))
    check(f"engine {rel} keeps {want_tests} tests", got == want_tests, f"got {got}")
    if want_tests:
        check(f"engine {rel} keeps its mod tests block", "#[cfg(test)]\nmod tests {" in body)

# 3. widened items the staying engine tests reach
check("crate tool_rm.rs exposes parse_recursive",
      "pub fn parse_recursive(" in read(DST / "tool_rm.rs"))
check("crate file_edit/tool_apply_patch.rs exposes absolute_path_inside_workspace",
      "pub fn absolute_path_inside_workspace(" in read(DST / "file_edit" / "tool_apply_patch.rs"))

# 4. the two already-shim K2 files are one-liners on both sides
for rel, line in ALREADY_SHIM:
    check(f"crate {rel} is the same one-line re-export", read(DST / rel).strip() == line)
    check(f"engine {rel} is the same one-line re-export", read(SRC / rel).strip() == line)

# 5. crate module table mirrors the engine tree
tools_rs = read(CRATE / "src" / "tools.rs")
for mod in ["file_edit", "tool_mv", "tool_rm"]:
    check(f"crate tools.rs declares pub mod {mod}", f"pub mod {mod};" in tools_rs)

# 6. engine module table is untouched: `mod tool_mv;` / `mod tool_rm;` still register the shims
engine_mod = read(SRC / "mod.rs")
for line in ["mod tool_mv;", "mod tool_rm;", "pub mod file_edit;"]:
    check(f"engine tools/mod.rs still declares {line}", line in engine_mod)
check("crate tool_rm.rs still defines the ToolRm struct",
      "pub struct ToolRm" in read(DST / "tool_rm.rs"))
check("crate tool_mv.rs still defines the ToolMv struct",
      "pub struct ToolMv" in read(DST / "tool_mv.rs"))
check("crate file_edit/mod.rs declares the same 10 submodules",
      read(DST / "file_edit" / "mod.rs") == read(SRC / "file_edit" / "mod.rs"))

# 6. crate manifest parity
crate_toml = read(CRATE / "Cargo.toml")
for dep in ['refact-file-edit-core = { path = "../refact-file-edit-core" }',
            'refact-scope-utils = { path = "../refact-scope-utils" }',
            'diff = "0.1.13"',
            'dunce = "1.0.5"']:
    check(f"crate manifest declares {dep.split(' ')[0]}", dep in crate_toml)
check("crate manifest never mentions refact-lsp", "refact-lsp" not in crate_toml)

lock = read(ENG / "Cargo.lock")
tools_block = lock.split('name = "refact-tools"')[1].split("[[package]]")[0]
for dep in ["diff", "dunce", "refact-file-edit-core", "refact-scope-utils"]:
    check(f"Cargo.lock refact-tools block depends on {dep}", f' "{dep}",' in tools_block)
check("Cargo.lock refact-tools block still has no refact-lsp", "refact-lsp" not in tools_block)
lsp_block = lock.split('name = "refact-lsp"')[1].split("[[package]]")[0]
check("Cargo.lock refact-lsp block unchanged w.r.t. refact-tools",
      ' "refact-tools",' in lsp_block)

# 7. moved symbols are reachable through the crate
definitions = {
    "resolve_path_with_scope": DST / "file_edit" / "auxiliary.rs",
    "convert_edit_to_diffchunks": DST / "file_edit" / "auxiliary.rs",
    "write_file_for_ctx": DST / "file_edit" / "auxiliary.rs",
    "tool_apply_patch_exec": DST / "file_edit" / "tool_apply_patch.rs",
    "tool_create_text_doc_exec": DST / "file_edit" / "tool_create_textdoc.rs",
    "tool_undo_text_doc_exec": DST / "file_edit" / "tool_undo_textdoc.rs",
    "tool_update_text_doc_exec": DST / "file_edit" / "tool_update_textdoc.rs",
    "tool_update_text_doc_anchored_exec": DST / "file_edit" / "tool_update_textdoc_anchored.rs",
    "tool_update_text_doc_by_lines_exec": DST / "file_edit" / "tool_update_textdoc_by_lines.rs",
    "tool_update_text_doc_regex_exec": DST / "file_edit" / "tool_update_textdoc_regex.rs",
    "ToolMv": DST / "tool_mv.rs",
    "ToolRm": DST / "tool_rm.rs",
}
for symbol, path in definitions.items():
    body = read(path)
    check(f"crate defines {symbol}", f"pub struct {symbol}" in body or
          f"pub async fn {symbol}" in body or f"pub fn {symbol}" in body)

# 8. constructor surface: every engine call site still names the same path and the
#    named module resolves through a shim
engine_refs = set()
for path in SRC.rglob("*.rs"):
    engine_refs.update(re.findall(r"crate::tools::(file_edit::\w+|tool_mv|tool_rm)::", read(path)))
for ref in sorted(engine_refs):
    rel = ref.replace("::", "/") + ".rs"
    shim = f"pub use refact_tools::tools::{ref}::*;"
    check(f"crate::tools::{ref}:: still resolves via a shim", read(SRC / rel).startswith(shim))
check("every K2 engine call-site module resolves through a shim",
      len(engine_refs) == 10, str(sorted(engine_refs)))
for consumer in ["tools/tools_list.rs", "tools/tests_schema.rs", "app_state.rs",
                 "tools/shell_gate.rs", "tools/tool_process.rs", "tools/tool_shell.rs",
                 "http/routers/v1/file_edit_tools.rs"]:
    path = ENG / "src" / consumer
    check(f"{consumer} still names a K2 module path",
          re.search(r"crate::tools::(file_edit|tool_mv|tool_rm)::", read(path)) is not None)

# 9. nothing that must stay was disturbed
dirty = git("status", "--porcelain").splitlines()
touched = {line.strip().split(None, 1)[1].replace("\\", "/")
           for line in dirty if line.strip().split(None, 1)[1:]}
allowed_prefixes = (
    "refact-agent/engine/src/tools/file_edit/",
    "refact-agent/engine/src/tools/tool_mv.rs",
    "refact-agent/engine/src/tools/tool_rm.rs",
    "refact-agent/engine/crates/refact-tools/",
    "refact-agent/engine/Cargo.lock",
    "tools/dev/k2-check.py",
    "tools/dev/k2-check.ps1",
)
extra = [t for t in touched if not t.startswith(allowed_prefixes)]
check("no unrelated file touched", not extra, str(extra))
for rel in UNTOUCHED:
    check(f"{rel} untouched", f"refact-agent/engine/src/tools/{rel}" not in touched)
check("engine Cargo.toml untouched", "refact-agent/engine/Cargo.toml" not in touched)

failed = [c for c in checks if not c[1]]
print(f"\n{len(checks) - len(failed)}/{len(checks)} checks passed")
if failed:
    print("RESULT: FAILED")
    sys.exit(1)
print("RESULT: ALL CHECKS PASSED")
