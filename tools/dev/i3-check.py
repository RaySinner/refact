#!/usr/bin/env python3
"""Static verification for card I-3 (integrations / at_commands / ext blocker cluster).

The project build guard refuses compilation from agent cards, so this harness
checks the structural invariants the rewrite must satisfy:

  R1 every file touched by the card is brace/paren balanced and free of merge markers;
  R2 none of the three target directories still reaches an *engine* path
     (`crate::<module>`) for one of the moved symbols;
  R3 every receiving crate module declares the symbol it is expected to own;
  R4 no moved crate module references an unknown `crate::` module - every module it
     names must exist inside the receiving crate.

Usage:  python tools/dev/i3-check.py
Exit 0 = all checks passed.
"""

import re
import sys
from pathlib import Path

ENGINE = Path("refact-agent/engine")
SRC = ENGINE / "src"
CRATES = ENGINE / "crates"

TARGET_DIRS = ["src/integrations", "src/at_commands", "src/ext"]

# engine paths (as `crate::...`) that must not survive in the target directories
FORBIDDEN_IN_TARGETS = [
    r"crate::tools::tools_description",
    r"crate::tools::scope_utils",
    r"crate::files_correction::",
    r"crate::exec::command_policy",
    r"crate::postprocessing::pp_context_files",
    r"crate::memories::memories_search_for_app",
    r"crate::http::routers::v1::at_commands::invalidate_slash_cache",
    r"crate::privacy::records::filter_path_enrichment_for_model_context",
    r"crate::privacy::records::declared_file_records",
    r"crate::privacy::records::privacy_record_scope",
    r"crate::privacy::records::tool_enrichment_from_path_references",
    r"crate::privacy::destinations::clear_for_mcp",
    r"crate::privacy::destinations::clear_for_model",
]

# crate module -> symbols it must declare
CRATE_OWNERS = {
    "crates/refact-privacy/src/destination_clear.rs": ["pub fn clear_for_mcp"],
    "crates/refact-runtime-api/src/scope_utils.rs": [
        "pub async fn resolve_scope(",
        "pub async fn resolve_existing_path_with_execution_scope(",
        "pub fn escape_path_for_scope_filter(",
        "pub struct ScopedListingLimited",
    ],
    "crates/refact-runtime-api/src/pp_tool_context.rs": [
        "pub struct ToolPPContext",
        "pub async fn postprocess_context_files_for_tool_ctx(",
    ],
    "crates/refact-runtime-api/src/gcx_helpers.rs": [
        "pub async fn filter_path_enrichment_for_model_context(",
    ],
}

# engine file -> the crate path it must re-export from
ENGINE_SHIMS = {
    "src/privacy/destinations.rs": "refact_privacy::destination_clear",
    "src/privacy/gcx_wrappers.rs": "refact_runtime_api::gcx_helpers",
    "src/tools/scope_utils.rs": "refact_runtime_api::scope_utils::*",
    "src/postprocessing/pp_context_files.rs":
        "refact_runtime_api::pp_tool_context::postprocess_context_files_for_tool_ctx",
}

# engine file -> array of crate paths it must import for a moved symbol
CONSUMER_IMPORTS = {
    "src/at_commands/at_file.rs": ["refact_runtime_api::scope_utils::"],
    "src/at_commands/at_tree.rs": ["refact_runtime_api::scope_utils::"],
    "src/at_commands/at_knowledge.rs": ["refact_runtime_api::memories_runtime::"],
    "src/at_commands/executor.rs": ["refact_runtime_api::pp_tool_context::"],
    "src/integrations/mcp/tool_mcp.rs": ["refact_privacy::destination_clear::clear_for_mcp"],
    "src/integrations/integr_cmdline.rs": [
        "refact_runtime_api::command_policy::build_exec_request_for_gcx",
        "refact_runtime_api::gcx_helpers::filter_path_enrichment_for_model_context",
    ],
    "src/integrations/mcp/session_mcp.rs": ["refact_runtime_api::slash_cache::invalidate_slash_cache"],
}

# concrete crate paths the rewritten consumers rely on: (module file, symbol)
CRATE_PATH_TARGETS = [
    ("crates/refact-tool-api/src/tool_desc.rs",
     ["ToolDesc", "ToolSource", "ToolSourceType", "MatchConfirmDenyResult"]),
    ("crates/refact-runtime-api/src/tool_trait.rs", ["Tool"]),
    ("crates/refact-runtime-api/src/slash_cache.rs", ["invalidate_slash_cache"]),
    ("crates/refact-runtime-api/src/files_correction.rs",
     ["get_active_project_path", "get_project_dirs",
      "check_if_its_inside_a_workspace_or_config"]),
    ("crates/refact-files/src/path_utils.rs",
     ["canonical_path", "canonicalize_normalized_path", "any_glob_matches_path"]),
    ("crates/refact-worktrees/src/path_mappings.rs", ["project_dirs_for_unscoped_paths"]),
    ("crates/refact-privacy/src/destination_clear.rs", ["clear_for_mcp"]),
    ("crates/refact-runtime-api/src/gcx_helpers.rs",
     ["declared_file_records", "privacy_record_scope",
      "tool_enrichment_from_path_references", "filter_path_enrichment_for_model_context"]),
    ("crates/refact-privacy/src/model_context.rs", ["observed_file_records"]),
    ("crates/refact-privacy/src/observation_reason.rs", ["provider_destination"]),
    ("crates/refact-exec/src/command_policy.rs",
     ["CommandKind", "CommandPolicyInput", "ExecSource"]),
    ("crates/refact-runtime-api/src/command_policy.rs", ["build_exec_request_for_gcx"]),
    ("crates/refact-runtime-api/src/scope_utils.rs",
     ["format_scope_notices", "is_worktree_root_alias",
      "list_execution_scope_root_limited", "list_scoped_files_under_dir_limited",
      "resolve_existing_path_with_execution_scope",
      "resolve_existing_path_with_execution_scope_for_model_context"]),
    ("crates/refact-runtime-api/src/pp_tool_context.rs",
     ["postprocess_context_files_for_tool_ctx"]),
    ("crates/refact-runtime-api/src/memories_runtime.rs", ["memories_search_for_app"]),
]

problems = []
checks = 0


def check(cond, message):
    global checks
    checks += 1
    if not cond:
        problems.append(message)


def read(path):
    return path.read_text(encoding="utf-8-sig", errors="replace")


def balanced(text):
    """Brace-balance that skips comments, strings, char literals and lifetimes."""
    pairs = {")": "(", "]": "[", "}": "{"}
    stack = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if c == "/" and nxt == "/":
            j = text.find("\n", i)
            i = n if j < 0 else j + 1
            continue
        if c == "/" and nxt == "*":
            depth = 1
            i += 2
            while i < n and depth:
                if text[i] == "/" and i + 1 < n and text[i + 1] == "*":
                    depth += 1
                    i += 2
                elif text[i] == "*" and i + 1 < n and text[i + 1] == "/":
                    depth -= 1
                    i += 2
                else:
                    i += 1
            continue
        # raw string
        if c == "r" and nxt in "#\"":
            hashes = 0
            j = i + 1
            while j < n and text[j] == "#":
                hashes += 1
                j += 1
            if j < n and text[j] == '"':
                closing = '"' + "#" * hashes
                k = text.find(closing, j + 1)
                i = n if k < 0 else k + len(closing)
                continue
        if c == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                    continue
                if text[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        # lifetime or char literal
        if c == "'":
            if i + 2 < n and text[i + 2] == "'" and text[i + 1] != "\\":
                i += 3
                continue
            if i + 1 < n and text[i + 1] == "\\":
                j = text.find("'", i + 1)
                i = n if j < 0 else j + 1
                continue
            i += 1
            while i < n and (text[i].isalnum() or text[i] == "_"):
                i += 1
            continue
        if c in "([{":
            stack.append(c)
        elif c in ")]}":
            if not stack or stack.pop() != pairs[c]:
                return False
        i += 1
    return not stack


def crate_modules(crate_dir):
    """Module names reachable as `crate::<name>` inside one crate."""
    names = set()
    src = crate_dir / "src"
    for p in src.rglob("*.rs"):
        rel = p.relative_to(src)
        names.add(rel.with_suffix("").as_posix().replace("/", "::"))
        names.add(rel.with_suffix("").name)
        if rel.name in ("mod.rs", "lib.rs"):
            parent = rel.parent.as_posix()
            if parent != ".":
                names.add(parent.replace("/", "::"))
                names.add(parent.split("/")[-1])
    names |= {"lib", "crate"}
    return names


targets = sorted(p for d in TARGET_DIRS for p in (SRC / d).rglob("*.rs"))
extra = [ENGINE / rel for rel in
         ["src/privacy/destinations.rs", "src/privacy/gcx_wrappers.rs",
          "src/tools/scope_utils.rs", "src/postprocessing/pp_context_files.rs",
          "src/postprocessing/gcx_pp_context.rs"]]
extra += [ENGINE / rel for rel in CRATE_OWNERS]

# R1 -------------------------------------------------------------------
for p in sorted(set(targets + extra)):
    check(p.exists(), f"R1 missing file: {p}")
    if not p.exists():
        continue
    text = read(p)
    check(balanced(text), f"R1 unbalanced braces/parens: {p}")
    check("<<<<<<<" not in text and ">>>>>>>" not in text, f"R1 merge marker: {p}")

# R2 -------------------------------------------------------------------
for p in targets:
    for i, raw in enumerate(read(p).splitlines(), 1):
        line = raw.strip()
        if line.startswith("//"):
            continue
        for pat in FORBIDDEN_IN_TARGETS:
            check(not re.search(pat, line), f"R2 {p}:{i} still reaches {pat}")
            if re.search(pat, line):
                break

# R3 -------------------------------------------------------------------
for rel, decls in CRATE_OWNERS.items():
    p = ENGINE / rel
    check(p.exists(), f"R3 missing crate module: {rel}")
    if not p.exists():
        continue
    text = read(p)
    for decl in decls:
        check(decl in text, f"R3 {rel} does not declare `{decl}`")

# R4 -------------------------------------------------------------------
for rel in CRATE_OWNERS:
    p = ENGINE / rel
    if not p.exists():
        continue
    crate_root = ENGINE / rel.split("/src/")[0]
    known = crate_modules(crate_root)
    for i, raw in enumerate(read(p).splitlines(), 1):
        line = raw.strip()
        if line.startswith("//"):
            continue
        for m in re.finditer(r"crate::([A-Za-z_][A-Za-z0-9_]*)", line):
            seg = m.group(1)
            check(seg in known, f"R4 {rel}:{i} names unknown crate module `crate::{seg}`")

# R5 -------------------------------------------------------------------
for rel, marker in ENGINE_SHIMS.items():
    p = ENGINE / rel
    check(p.exists(), f"R5 missing engine file: {rel}")
    if p.exists():
        check(marker in read(p), f"R5 {rel} no longer re-exports `{marker}`")

for rel, markers in CONSUMER_IMPORTS.items():
    p = ENGINE / rel
    check(p.exists(), f"R5 missing consumer file: {rel}")
    if not p.exists():
        continue
    text = read(p)
    for marker in markers:
        check(marker in text, f"R5 {rel} no longer imports `{marker}`")

# R6 ---- every crate path the rewritten consumers target actually exports the symbol
for rel, symbols in CRATE_PATH_TARGETS:
    p = ENGINE / rel
    check(p.exists(), f"R6 missing crate module: {rel}")
    if not p.exists():
        continue
    text = read(p)
    for symbol in symbols:
        decl = re.search(
            r"\bpub (?:async )?(?:fn|struct|enum|trait|type|const|static) " + re.escape(symbol) + r"\b",
            text,
        )
        if not decl:
            for m in re.finditer(r"pub use [^;]+;", text):
                if re.search(r"\b" + re.escape(symbol) + r"\b", m.group(0)):
                    decl = m
                    break
        check(decl is not None, f"R6 {rel} does not export `{symbol}`")

if problems:
    print(f"FAILED ({len(problems)} problems, {checks} checks)")
    for x in problems:
        print("  ", x)
    sys.exit(1)

print(f"OK ({checks} checks)")
