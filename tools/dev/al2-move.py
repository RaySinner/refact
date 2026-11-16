"""AL-2 mover: settle `chat::cache_guard` into `refact-runtime-api`.

    python tools/dev/al2-move.py <worktree-root>

Deterministic, one-shot; rewrites the worktree it is pointed at.

Routine (identical to ac1-move/al1-move):

  * the production body moves verbatim into `refact-runtime-api/src/chat_cache_guard.rs`
    with only forward `crate::` path rewrites and visibility widening;
  * the engine file becomes the original `use` lines gated on `#[cfg(test)]`
    (the kept `#[cfg(test)] mod tests` sees them through `use super::*`) plus a
    glob `pub use` of the crate module, plus the **verbatim** test module;
  * items the kept tests reach are widened to `pub` in the crate (struct fields too).

Not moved here (planner-approved, see the card report): `chat::prompts`,
`chat::verifier`, `chat::verify_cmd`, `chat::ctx`. Their blocking symbols and the
crate-edge proof are in the report; `prompts`/`verifier` route to `refact-tools`
in a follow-up card.
"""
import io
import os
import re
import sys

WT = os.path.abspath(sys.argv[1])
ENG = os.path.join(WT, "refact-agent", "engine")
SRC = os.path.join(ENG, "src")
CRATE = os.path.join(ENG, "crates", "refact-runtime-api", "src")

ENGINE_FILE = os.path.join(SRC, "chat", "cache_guard.rs")
CRATE_FILE = os.path.join(CRATE, "chat_cache_guard.rs")
CRATE_LIB = os.path.join(CRATE, "lib.rs")

REWRITES = [
    ("use crate::chat::types::{ChatSession, TaskMeta};",
     "use refact_chat_api::types::{ChatSession, TaskMeta};"),
    ("use crate::tokens::{cached_tokenizer, count_text_tokens_with_fallback};",
     "use crate::chat_tokens::cached_tokenizer;\n"
     "use refact_core::tokens_runtime::count_text_tokens_with_fallback;"),
    ("crate::global_context::try_load_caps_quickly_if_not_present",
     "crate::caps_loader::try_load_caps_quickly_if_not_present"),
    ("crate::caps::resolve_chat_model",
     "crate::caps_load_runtime::resolve_chat_model"),
    ("crate::providers::pricing::lookup_model_pricing",
     "crate::providers_pricing_runtime::lookup_model_pricing"),
    ("crate::chat::types::PauseReason", "refact_chat_api::types::PauseReason"),
    ("crate::chat::types::ChatSession", "refact_chat_api::types::ChatSession"),
]

WIDEN_FNS = ["cache_guard_outcome_if_snapshot_changed"]

# struct fields the kept tests construct in a literal
WIDEN_FIELDS = {"CacheGuardSnapshot": ["body", "request_generation", "reset_generation"]}

USE_RE = re.compile(r"(?m)^use [^;]+;\n")
FIELD_RE = re.compile(r"^(?:pub\([^)]*\)|pub)\s+")


def read(path):
    with io.open(path, "r", encoding="utf-8", errors="surrogateescape") as fh:
        return fh.read()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with io.open(path, "w", encoding="utf-8", errors="surrogateescape", newline="\n") as fh:
        fh.write(text)


def split_prod(text):
    lines = text.split("\n")
    for idx, line in enumerate(lines):
        if line.strip().startswith("#[cfg(test)]"):
            return "\n".join(lines[:idx]), "\n".join(lines[idx:])
    return text, ""


def rewrite(text):
    for old, new in REWRITES:
        text = text.replace(old, new)
    return text


def widen_fns(text):
    for name in WIDEN_FNS:
        pattern = r"(?m)^()(fn) %s\b" % re.escape(name)
        text, n = re.subn(pattern, r"pub \1\2 %s" % name, text)
        if not n:
            raise SystemExit("al2-move: could not widen fn %s" % name)
    return text


def widen_struct_fields(text, struct_name, fields):
    m = re.search(r"(?m)^pub struct %s \{\n" % re.escape(struct_name), text)
    if not m:
        raise SystemExit("al2-move: no struct %s" % struct_name)
    start = m.end()
    end = text.find("\n}\n", start)
    if end < 0:
        raise SystemExit("al2-move: no body end for %s" % struct_name)
    out = []
    for line in text[start:end].split("\n"):
        stripped = line.strip()
        indent = line[: len(line) - len(line.lstrip())]
        if stripped and not stripped.startswith(("#", "//")):
            bare = FIELD_RE.sub("", stripped)
            if "(" not in bare and bare.split(":")[0].strip() in fields:
                line = indent + "pub " + bare
        out.append(line)
    return text[:start] + "\n".join(out) + text[end:]


def engine_paths(text):
    pat = re.compile(r"\bcrate::(?:chat|subchat|llm|caps|ext|tools|providers|files_correction|"
                     r"call_validation|global_context|tokens|worktrees|yaml_configs|scratchpads|"
                     r"tasks|memories|app_state|trace)::[A-Za-z_][A-Za-z0-9_:]*")
    allowed = ("crate::app_state::AppState", "crate::caps_loader::",
               "crate::caps_load_runtime::", "crate::chat_tokens::",
               "crate::providers_pricing_runtime::")
    return sorted(h for h in set(pat.findall(text)) if not h.startswith(allowed))


def main():
    text = read(ENGINE_FILE)
    prod, tests = split_prod(text)
    if not tests.strip():
        raise SystemExit("al2-move: engine cache_guard.rs has no #[cfg(test)] module")

    body = widen_fns(rewrite(prod))
    for struct_name, fields in WIDEN_FIELDS.items():
        body = widen_struct_fields(body, struct_name, fields)
    body = body.rstrip("\n") + "\n"

    leftover = engine_paths(body)
    if leftover:
        raise SystemExit("al2-move: unresolved engine paths in crate body: %s" % leftover)

    write(CRATE_FILE, body)

    lib = read(CRATE_LIB)
    if "pub mod chat_cache_guard;" not in lib:
        anchor = "pub mod chat_tokens;\n"
        if anchor not in lib:
            raise SystemExit("al2-move: no `pub mod chat_tokens;` anchor in runtime-api lib.rs")
        write(CRATE_LIB, lib.replace(anchor, anchor + "pub mod chat_cache_guard;\n", 1))

    # A kept `#[cfg(test)] mod tests` says `use super::*`, and a child module sees the
    # parent's private imports through it, so the shim must carry them. Nothing in the
    # shim consumes them at build time, hence the `allow`.
    shim = "".join(
        "#[cfg(test)]\n#[allow(unused_imports)]\n" + use
        for use in USE_RE.findall(prod)
    )
    shim += "\npub use refact_runtime_api::chat_cache_guard::*;\n\n"
    shim += tests.strip() + "\n"
    write(ENGINE_FILE, shim)

    print("moved chat::cache_guard prod %5d  tests-kept %5d"
          % (len(prod.rstrip("\n").split("\n")), len(tests.rstrip("\n").split("\n"))))


main()
