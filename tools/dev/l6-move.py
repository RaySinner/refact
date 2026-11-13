"""One-off mover for L-6 (K3a read/search group) -> refact-tools.

Idempotent: reads every source file from the pre-move commit, so re-running it
reproduces the exact same output. Run from the repo root:
python tools/dev/l6-move.py

The read-only proof that the result is byte-for-byte faithful lives in
tools/dev/l6-verbatim.py.
"""
import pathlib
import re
import subprocess

ENG = pathlib.Path(__file__).resolve().parents[2] / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"

MODS = [
    "tool_cat",
    "tool_tree",
    "tool_glob",
    "tool_regex_search",
    "tool_search",
    "tool_ast_definition",
    "native_enrichment",
]

REWRITES = [
    ("crate::scratchpads::multimodality::", "refact_core::chat_types::"),
    ("crate::global_context::", "refact_runtime_api::global_context::"),
    ("crate::codegraph::", "refact_codegraph::"),
    ("crate::tools::scope_utils::", "refact_runtime_api::scope_utils::"),
    ("crate::tools::ctx::", "refact_runtime_api::tool_ctx::"),
]

WIDEN = {
    "tool_cat": ["fn clamp_cat_line_range", "fn expansion_cap_notice"],
    "tool_tree": [],
    "tool_glob": [
        "struct GlobOutcome",
        "const DEFAULT_MAX_RESULTS",
        "const CAP_MAX_RESULTS",
        "fn glob_match_options",
        "fn path_matches_glob",
        "fn run_glob",
        "fn parse_max_results",
    ],
    "tool_regex_search": [
        "struct RegexMatch",
        "struct RegexSearchOutcome",
        "const MAX_SEARCH_FILE_BYTES",
        "const MAX_CONCURRENT_FILE_READS",
        "fn search_single_file",
        "fn search_files_with_regex",
        "fn parse_usize_arg",
    ],
    "tool_search": [
        "struct FallbackOutcome",
        "const FALLBACK_MAX_CANDIDATE_FILES",
        "const MAX_SEARCH_FILE_BYTES",
        "fn parse_usize_arg",
        "fn semantic_no_match_result",
        "fn direct_worktree_fallback_search",
    ],
    "tool_ast_definition": [
        "const MAX_SYMBOLS",
        "static IDENTIFIER_RE",
        "fn type_hierarchy_sections",
        "fn related_memory_entities",
        "fn compute_related_memories",
        "fn symbol_def_via_codegraph",
    ],
    "native_enrichment": ["const MAX_REFERENCES", "fn workspace_relative_path"],
}

# structs whose fields the engine-side tests / engine call sites read
PUB_FIELD_STRUCTS = {
    "tool_glob": ["GlobOutcome"],
    "tool_regex_search": ["RegexMatch", "RegexSearchOutcome"],
    "tool_search": ["FallbackOutcome"],
    "native_enrichment": ["NativeReferences"],
}

# native_enrichment is consumed by the engine module tool_knowledge, so every
# pub(crate) item has to become pub for the re-export shim to reach it.
WIDEN_CRATE_TO_PUB = {"native_enrichment"}

# Traits the kept tests need in scope through method-call syntax only: the
# method name never appears as an identifier, so a textual scan cannot see it.
TRAIT_METHOD_HINTS = {
    "tool_execute": "Tool",
    "tool_description": "Tool",
    "tool_depends_on": "Tool",
    "tool_ctx": "GlobalContextCtx",
    "chat_ctx": "GlobalContextCtx",
    "app_state": "GlobalContextCtx",
}

BASE = "620f9c2ee"


def split_test(lines):
    for i, line in enumerate(lines):
        if i > 5 and line.strip() == "#[cfg(test)]":
            return lines[:i], lines[i:]
    raise SystemExit("no #[cfg(test)] found")


def use_statements(text):
    """Column-0 `use` statements (the module's own import block)."""
    return re.findall(r"(?m)^use [^;]*;", text)


def nested_use_statements(text):
    """Every `use` statement, including the ones inside `mod tests { … }`."""
    return re.findall(r"(?m)^\s*use [^;]*;", text)


def imported_names(statement):
    """Return the identifiers a `use ...;` statement binds."""
    m = re.match(r"(?s)^\s*use (.*);$", statement.strip())
    body = " ".join(m.group(1).split())
    names = set()
    if body.endswith("::*"):
        return names
    brace = re.search(r"\{([^}]*)\}$", body)
    if brace:
        prefix = body[: body.index("{")].rstrip(":").rstrip()
        for part in brace.group(1).split(","):
            part = part.strip()
            if not part:
                continue
            if part == "self":
                names.add(prefix.split("::")[-1].strip())
            else:
                names.add(part.split(" as ")[-1].strip())
    else:
        names.add(body.split(" as ")[-1].split("::")[-1].strip())
    return names


def precise_imports(statement, identifiers):
    """Emit only the imports a test module actually needs.

    Splits `use path::{a, b};` into `use path::a;` / `use path::b;` and keeps
    only the names that appear in the test module.
    """
    m = re.match(r"(?s)^use (.*);$", statement)
    body = " ".join(m.group(1).split())
    if body.endswith("::*"):
        return []
    if " as " in body:
        alias = body.split(" as ")[-1].strip()
        return [statement] if alias in identifiers else []
    brace = re.search(r"\{([^}]*)\}$", body)
    if brace:
        prefix = body[: body.index("{")].rstrip(":").rstrip()
        out = []
        for part in brace.group(1).split(","):
            name = part.strip()
            if not name:
                continue
            if name == "self":
                leaf = prefix.split("::")[-1].strip()
                if leaf in identifiers:
                    out.append(statement)
            elif name in identifiers:
                out.append("use %s::%s;" % (prefix, name))
        return out
    name = body.split("::")[-1].strip()
    return [statement] if name in identifiers else []


def names_an_identifier(text, name):
    """True when `name` may be needed in scope.

    Deliberately wide: a `::`-qualified occurrence (`ContextEnum::ChatMessage`)
    does not strictly need the type imported, but keeping the import is only a
    warning, while dropping a needed one is a hard error.
    """
    return re.search(r"(?<![\w])%s\b" % re.escape(name), text) is not None


def trait_names(test_text):
    return {trait for method, trait in TRAIT_METHOD_HINTS.items()
            if re.search(r"\.%s\(" % method, test_text)}


def widen(lines, mod, log):
    end = len(lines)
    if mod in WIDEN_CRATE_TO_PUB:
        for i in range(end):
            if lines[i].strip().startswith("pub(crate) "):
                lines[i] = lines[i].replace("pub(crate) ", "pub ", 1)
                log.append("pub(crate)->pub %s L%d" % (mod, i + 1))
    for i in range(end):
        s = lines[i].strip()
        for p in WIDEN[mod]:
            if s.startswith(p) or s.startswith("async " + p):
                lines[i] = "pub " + lines[i]
                log.append("widen %s L%d %s" % (mod, i + 1, s))
                break
    for name in PUB_FIELD_STRUCTS.get(mod, []):
        start = None
        for i in range(end):
            if re.match(r"^(pub(\(crate\))? )?struct %s\b" % name, lines[i].strip()):
                start = i
                break
        if start is None:
            raise SystemExit("struct %s not found in %s" % (name, mod))
        for j in range(start + 1, end):
            s = lines[j].strip()
            if s.startswith("}"):
                break
            if s and not s.startswith("pub ") and not s.startswith("//"):
                lines[j] = re.sub(r"^(\s*)", r"\1pub ", lines[j])
                log.append("pub-field %s L%d %s" % (mod, j + 1, s))


def source_text(mod):
    """Read the pristine pre-move file so the script is idempotent."""
    text = subprocess.run(
        ["git", "-C", str(ENG), "show", "%s:refact-agent/engine/src/tools/%s.rs" % (BASE, mod)],
        capture_output=True, check=True).stdout.decode("utf-8")
    return text


def crate_body(mod, prod, log):
    """The crate-side file body: prod verbatim + documented edits."""
    lines = list(prod)
    widen(lines, mod, log)
    text = "\n".join(lines)
    for old, new in REWRITES:
        text = text.replace(old, new)
    # `GlobalContextCtx` is a trait import kept for the engine-side test module
    # (it enables `gcx.tool_ctx(..)`); in the crate body it has no caller, so
    # carrying it over would only add an unused-import warning.
    if not re.search(r"\.(tool_ctx|chat_ctx|app_state)\(", text):
        text = text.replace("use refact_runtime_api::GlobalContextCtx;\n", "")
    if mod in WIDEN_CRATE_TO_PUB:
        text = text.replace("pub(crate) fn ", "pub fn ")
        text = text.replace("pub(crate) struct ", "pub struct ")
    return text


def test_prelude(mod, prod, test):
    """`#[cfg(test)]`-gated re-imports the kept test module needs."""
    test_text = "\n".join(test)
    already = set()
    for statement in nested_use_statements(test_text):
        already |= imported_names(statement)
    needed = trait_names(test_text)
    for statement in use_statements("\n".join(prod)):
        if statement.startswith("pub(") or re.match(r"(?m)^use crate::tools::%s::" % mod, statement):
            continue
        for name in imported_names(statement):
            if name in already:
                continue
            if names_an_identifier(test_text, name):
                needed.add(name)
    pre = []
    for statement in use_statements("\n".join(prod)):
        if statement.startswith("pub(") or re.match(r"(?m)^use crate::tools::%s::" % mod, statement):
            continue
        for line in precise_imports(statement, needed):
            if line not in pre:
                pre.append(line)
    return pre


def main():
    log = []
    for mod in MODS:
        lines = source_text(mod).replace("\r\n", "\n").split("\n")
        prod, test = split_test(lines)

        text = crate_body(mod, prod, log)
        stray = sorted({m for m in re.findall(r"crate::[\w:]*", text)
                        if not m.startswith("crate::tools::")})
        if stray:
            print("STRAY in %s: %s" % (mod, stray))
        (DST / (mod + ".rs")).write_text(text.rstrip("\n") + "\n", encoding="utf-8")

        pre = test_prelude(mod, prod, test)
        shim = "pub use refact_tools::tools::%s::*;\n\n" % mod
        if pre:
            shim += "\n".join("#[cfg(test)]\n" + line for line in pre) + "\n"
        shim += "\n".join(test).rstrip("\n")
        (SRC / (mod + ".rs")).write_text(shim + "\n", encoding="utf-8")
        log.append("%s: prod %d -> crate; shim prelude %d; test lines %d"
                   % (mod, len(prod), len(pre), len(test)))

    print("\n".join(log))


if __name__ == "__main__":
    main()
