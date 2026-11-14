"""P-3 mover: `src/tools/buddy/memory.rs` -> `crates/refact-tools/src/tools/buddy/memory.rs`.

Production code moves to the crate; the inline `#[cfg(test)] mod tests` stays in
the engine shim behind the crate glob re-export (the L-wave recipe: see
`src/tools/tool_task_memory.rs`). The four engine-private items the tests reach
(`WRITABLE_KINDS`, `MAX_KNOWLEDGE_SCAN_FILES`, `find_duplicate_content`,
`scan_cards_in_dirs`) are widened to `pub` so the glob carries them.

The import block is rewritten once (so it is crate-resolvable in both places) and
re-emitted verbatim in the shim's test prelude. Idempotent: read from the
pristine pre-move commit.

    python tools/dev/p3-move.py
"""
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"

BASE = "68efcde49"
REL = "buddy/memory.rs"

BODY_ANCHOR = "const MEMORY_KINDS: &[&str] = &["

# (engine form, crate form) applied to the whole import block
IMPORT_REWRITES = [
    (
        "use crate::buddy::memory_lifecycle::{ archive_memory_file_checked_for_ctx, "
        "compute_content_hash, find_memory_by_source_for_ctx, normalize_kind, normalize_tags, "
        "propose_supersede_for_near_duplicate_for_ctx, MemoryCreatePayload, MemoryLifecycleOp, "
        "MemoryLifecyclePayload, MemoryOpStatus, MemoryOpType, MemorySource, };",
        "use refact_buddy_core::memory_lifecycle_model::{\n"
        "    compute_content_hash, normalize_kind, normalize_tags, MemoryCreatePayload,\n"
        "    MemoryLifecycleOp, MemoryLifecyclePayload, MemoryOpStatus, MemoryOpType, MemorySource,\n"
        "};\n"
        "use refact_runtime_api::memory_lifecycle_runtime::{\n"
        "    archive_memory_file_checked_for_ctx, find_memory_by_source_for_ctx,\n"
        "    propose_supersede_for_near_duplicate_for_ctx,\n"
        "};",
    ),
    ("crate::buddy::storage::enqueue_memory_op", "refact_buddy_core::storage::enqueue_memory_op"),
    ("crate::file_filter::KNOWLEDGE_FOLDER_NAME", "refact_files::file_filter::KNOWLEDGE_FOLDER_NAME"),
    (
        "crate::knowledge_graph::kg_structs::KnowledgeFrontmatter",
        "refact_knowledge_graph::kg_structs::KnowledgeFrontmatter",
    ),
]

WIDEN = [
    (r"^const WRITABLE_KINDS: ", "pub const WRITABLE_KINDS: "),
    (r"^const MAX_KNOWLEDGE_SCAN_FILES: ", "pub const MAX_KNOWLEDGE_SCAN_FILES: "),
    (r"^async fn find_duplicate_content\(", "pub async fn find_duplicate_content("),
    (r"^async fn scan_cards_in_dirs\(", "pub async fn scan_cards_in_dirs("),
]

TEST_MARKER = re.compile(r"(?m)^#\[cfg\(test\)\]\nmod tests \{")
IMPORT_STMT = re.compile(r"(?ms)^(?:pub )?use\b.*?;")


def base_text():
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:refact-agent/engine/src/tools/%s" % (BASE, REL)],
        capture_output=True,
        check=True,
    ).stdout.decode("utf-8")
    return raw.replace("\r\n", "\n")


def flatten(text):
    return re.sub(r"\n[ \t]*", " ", text.strip())


def rewrite_imports(header):
    """Collapse the import block into single-line statements, then retarget it."""
    stmts = [flatten(stmt) for stmt in IMPORT_STMT.findall(header)]
    block = "\n".join(stmts)
    block = block.replace(stmts[0], flatten(stmts[0]), 1)
    out = []
    for stmt in stmts:
        out.append(stmt)
    text = "\n".join(out)
    for old, new in IMPORT_REWRITES:
        if old not in text:
            raise SystemExit("import rewrite anchor missing: %s" % old[:70])
        text = text.replace(old, new)
    return text + "\n"


def test_prelude(header):
    """The crate-resolvable import block, kept test-only in the shim.

    `mod tests` starts with `use super::*;` and used to inherit the file's own
    imports; after the move those live in the crate, so the shim re-imports them
    for the test module only (the `tool_task_memory.rs` recipe).
    """
    out = []
    for stmt in IMPORT_STMT.findall(header):
        out.append("#[cfg(test)]")
        out.append("#[allow(unused_imports)]")
        out.append(flatten(stmt))
    return "\n".join(out) + "\n"


def main():
    text = base_text()
    m = TEST_MARKER.search(text)
    if not m:
        raise SystemExit("no inline #[cfg(test)] mod tests in %s" % REL)
    prod, tests = text[: m.start()].rstrip("\n") + "\n", text[m.start():]

    anchor = prod.index(BODY_ANCHOR)
    header = rewrite_imports(prod[:anchor])
    body = header + "\n" + prod[anchor:]

    for pattern, replacement in WIDEN:
        body, count = re.subn(pattern, replacement, body, count=1, flags=re.M)
        if count != 1:
            raise SystemExit("widening anchor missing: %s" % pattern)

    stray = sorted(
        {ref for ref in re.findall(r"crate::[\w:]*", body) if not ref.startswith("crate::tools::")}
    )
    if stray:
        raise SystemExit("STRAY engine-only crate:: path: %s" % stray)

    out = DST / REL
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(body.rstrip("\n") + "\n", encoding="utf-8", newline="\n")
    print("crate  %-28s %4d lines" % (REL, body.count("\n") + 1))

    shim = (
        "pub use refact_tools::tools::buddy::memory::*;\n\n"
        + test_prelude(header)
        + "\n"
        + tests.rstrip("\n")
        + "\n"
    )
    (SRC / REL).write_text(shim, encoding="utf-8", newline="\n")
    print("engine %-28s %4d lines" % (REL, shim.count("\n") + 1))


if __name__ == "__main__":
    main()
