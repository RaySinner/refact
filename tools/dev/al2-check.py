"""AL-2 structural checks + body-preservation diff for the `chat::cache_guard` move.

    python tools/dev/al2-check.py <worktree-root> [<base-commit>]

Deterministic and read-only. Checks, in order:

  A. the engine file is a pure shim (test-gated `use` lines + glob + the verbatim
     `#[cfg(test)] mod tests`), with no production item left behind;
  B. the destination file carries the production items and no unresolved
     engine-module path;
  C. `refact-runtime-api` declares `pub mod chat_cache_guard;`;
  D. the cargo edges the card asserts, in both directions (cycle proof);
  E. body preservation: the moved production body, with the mechanical rewrites
     reversed, is byte-identical to the pre-move body at `<base-commit>`.
"""
import io
import os
import re
import subprocess
import sys

WT = os.path.abspath(sys.argv[1])
BASE = sys.argv[2] if len(sys.argv) > 2 else "bf24bd08a"

ENGINE_REL = "refact-agent/engine/src/chat/cache_guard.rs"
CRATE_REL = "refact-agent/engine/crates/refact-runtime-api/src/chat_cache_guard.rs"
LIB_REL = "refact-agent/engine/crates/refact-runtime-api/src/lib.rs"
CHAT_MOD_REL = "refact-agent/engine/src/chat/mod.rs"
CHAT_API_CARGO = "refact-agent/engine/crates/refact-chat-api/Cargo.toml"
RUNTIME_CARGO = "refact-agent/engine/crates/refact-runtime-api/Cargo.toml"
TOOLS_CARGO = "refact-agent/engine/crates/refact-tools/Cargo.toml"

FAILED = []
RAN = [0]


def check(label, ok, detail=""):
    RAN[0] += 1
    if ok:
        print("  ok   %s" % label)
    else:
        print("  FAIL %s%s" % (label, ("  -- " + detail) if detail else ""))
        FAILED.append(label)


def read(rel):
    with io.open(os.path.join(WT, rel), encoding="utf-8", errors="surrogateescape") as fh:
        return fh.read()


def git_show(rev, rel):
    out = subprocess.run(
        ["git", "-C", WT, "show", "%s:%s" % (rev, rel)],
        capture_output=True,
    )
    if out.returncode != 0:
        raise SystemExit("al2-check: git show %s:%s failed: %s"
                         % (rev, rel, out.stderr.decode("utf-8", "replace")))
    return out.stdout.decode("utf-8", "surrogateescape")


def split_prod(text):
    lines = text.split("\n")
    for idx, line in enumerate(lines):
        if line.strip().startswith("#[cfg(test)]"):
            return "\n".join(lines[:idx]), "\n".join(lines[idx:])
    return text, ""


REWRITES = [
    ("use refact_chat_api::types::{ChatSession, TaskMeta};",
     "use crate::chat::types::{ChatSession, TaskMeta};"),
    ("use crate::chat_tokens::cached_tokenizer;",
     "use crate::tokens::cached_tokenizer;"),
    ("use refact_core::tokens_runtime::count_text_tokens_with_fallback;",
     "use crate::tokens::count_text_tokens_with_fallback;"),
    ("crate::caps_loader::try_load_caps_quickly_if_not_present",
     "crate::global_context::try_load_caps_quickly_if_not_present"),
    ("crate::caps_load_runtime::resolve_chat_model",
     "crate::caps::resolve_chat_model"),
    ("crate::providers_pricing_runtime::lookup_model_pricing",
     "crate::providers::pricing::lookup_model_pricing"),
    ("refact_chat_api::types::PauseReason", "crate::chat::types::PauseReason"),
    ("refact_chat_api::types::ChatSession", "crate::chat::types::ChatSession"),
]

PROD_ITEMS = [
    "pub enum CacheGuardOutcome",
    "pub struct CacheGuardSnapshot",
    "pub async fn is_guard_enabled(",
    "pub async fn estimate_extra_cache_miss_usd(",
    "pub async fn check_or_pause_cache_guard(",
    "pub fn cache_guard_outcome_if_snapshot_changed(",
    "pub async fn commit_cache_guard_snapshot(",
]

ENGINE_PATH_RE = re.compile(
    r"\bcrate::(?:chat|subchat|llm|caps|ext|tools|providers|files_correction|call_validation|"
    r"global_context|tokens|worktrees|yaml_configs|scratchpads|tasks|memories|trace)::"
    r"[A-Za-z_][A-Za-z0-9_:]*")
ENGINE_PATH_ALLOWED = (
    "crate::app_state::", "crate::caps_loader::", "crate::caps_load_runtime::",
    "crate::chat_tokens::", "crate::providers_pricing_runtime::",
)


def norm(text):
    """Normalise the mechanical classes: blank-line noise, the split token import,
    and field visibility widening."""
    text = text.replace(
        "use crate::tokens::{cached_tokenizer, count_text_tokens_with_fallback};",
        "use crate::tokens::cached_tokenizer;\n"
        "use crate::tokens::count_text_tokens_with_fallback;")
    text = re.sub(
        r"(?m)^(    )(?:pub\(crate\) |pub )?(body|request_generation|reset_generation):",
        r"\1\2:", text)
    return "\n".join(line.rstrip() for line in text.rstrip("\n").split("\n") if line.strip())


def main():
    print("=== A. engine shim ===")
    engine = read(ENGINE_REL)
    check("engine file exists", os.path.exists(os.path.join(WT, ENGINE_REL)))
    check("engine globs the crate module",
          "pub use refact_runtime_api::chat_cache_guard::*;" in engine)
    check("engine keeps the verbatim test module",
          re.search(r"(?m)^#\[cfg\(test\)\]\nmod tests \{", engine) is not None)
    for item in PROD_ITEMS:
        check("engine no longer declares `%s`" % item, item not in engine)
    check("engine has no `fn ` item in its production half",
          not re.search(r"(?m)^(?:pub )?(?:async )?fn ",
                        split_prod(engine)[0]))

    print("=== B. destination file ===")
    crate = read(CRATE_REL)
    for item in PROD_ITEMS:
        check("crate declares `%s`" % item, item in crate)
    leftover = sorted(h for h in set(ENGINE_PATH_RE.findall(crate))
                      if not h.startswith(ENGINE_PATH_ALLOWED))
    check("crate has no unresolved engine-module path", not leftover, str(leftover))
    check("crate keeps the chat-api glob (verbatim body)",
          "pub use refact_chat_api::cache_guard::*;" in crate)
    check("crate snapshot fields are public for the kept tests",
          re.search(r"pub struct CacheGuardSnapshot \{\n    pub body:", crate) is not None)

    print("=== C. module declaration ===")
    lib = read(LIB_REL)
    check("runtime-api declares pub mod chat_cache_guard;",
          "pub mod chat_cache_guard;" in lib)
    check("engine chat/mod.rs still declares pub mod cache_guard;",
          "pub mod cache_guard;" in read(CHAT_MOD_REL))

    print("=== D. cargo edges (cycle proof) ===")
    chat_api = read(CHAT_API_CARGO)
    runtime = read(RUNTIME_CARGO)
    tools = read(TOOLS_CARGO)
    check("EDGE refact-runtime-api -> refact-chat-api",
          re.search(r"(?m)^refact-chat-api\s*=", runtime) is not None)
    check("EDGE refact-chat-api -> refact-runtime-api is absent",
          re.search(r"(?m)^refact-runtime-api\s*=", chat_api) is None)
    check("EDGE refact-tools -> refact-runtime-api",
          re.search(r"(?m)^refact-runtime-api\s*=", tools) is not None)
    check("EDGE refact-runtime-api -> refact-tools is absent",
          re.search(r"(?m)^refact-tools\s*=", runtime) is None)
    check("EDGE refact-tools -> refact-chat-api (blocks the card's stated destination)",
          re.search(r"(?m)^refact-chat-api\s*=", tools) is not None)
    for dep in ("refact-files", "refact-scratchpads"):
        check("EDGE refact-chat-api -> %s is absent (card Spec premise)" % dep,
              re.search(r"(?m)^%s\s*=" % re.escape(dep), chat_api) is None)

    print("=== E. body preservation vs %s ===" % BASE)
    original = git_show(BASE, ENGINE_REL)
    orig_prod, _orig_tests = split_prod(original)
    crate_prod, _crate_tests = split_prod(crate)
    reversed_body = crate_prod
    for new, old in REWRITES:
        reversed_body = reversed_body.replace(new, old)
    reversed_body = re.sub(r"(?m)^pub (fn cache_guard_outcome_if_snapshot_changed)",
                           r"\1", reversed_body)
    a, b = norm(orig_prod), norm(reversed_body)
    same = a == b
    check("production body is byte-identical modulo the declared rewrites", same)
    if not same:
        a_lines, b_lines = a.split("\n"), b.split("\n")
        shown = 0
        for i in range(max(len(a_lines), len(b_lines))):
            la = a_lines[i] if i < len(a_lines) else "<eof>"
            lb = b_lines[i] if i < len(b_lines) else "<eof>"
            if la != lb:
                print("    line %d\n      orig  %r\n      moved %r" % (i + 1, la, lb))
                shown += 1
                if shown >= 10:
                    break

    print("")
    print("AL-2 checks: %d run, %d failed" % (RAN[0], len(FAILED)))
    if FAILED:
        for label in FAILED:
            print("  - %s" % label)
        sys.exit(1)
    print("AL-2 CHECK PASSED")


main()
