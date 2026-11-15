"""AB-2 mover: settle the crate-typed protocol layer of `chat::stream_core`.

`chat::stream_core` is the largest declared island (census: 2970 production
lines). Its real blocker is not the chat graph: the census records
`deps=[chat::cache_guard]` plus engine-local provider helpers, and its crate
edges are already `refact_core`, `refact_chat_api`, `refact_chat_history`,
`refact_llm`, `refact_perf`, `refact_privacy`, `refact-runtime-api`.

This moves the provider-agnostic protocol layer -- the half that needs nothing
but those crates -- into `refact-llm::stream_core`, which already depends on
exactly them:

  * `merge_usage`, `LlmStreamError`, `StreamRunParams`
  * `ChoiceFinal`, `StreamCollector`, `ReplayCollector`,
    `PartialOutputCollector`, `ChoiceAccumulator`
  * the `think` / `<function_calls>` stream parsers, delta-op emission
  * `process_stream_event_data`, the NDJSON framing helpers
  * `finalize_accumulators`, `merge_thinking_blocks`, `normalize_tool_call`

What stays in the engine (transport, provider registry, session, tests):
`run_llm_stream*`, the HTTP/WebSocket request paths, the provider refresh and
credential helpers, `read_error_body_bounded` / `format_llm_error_body`, the
abort plumbing, `ABORT_ERROR_MESSAGE`, `LlmStreamOutcome`, and the whole
`#[cfg(test)]` module (which keeps exercising the moved parsers through the
engine's `pub use` shim, exactly as before).

Every moved line is byte-identical except for
  * explicit path rewrites (engine paths -> crate paths), listed in REWRITES;
  * the `pub` widening the moved body needs to stay reachable/reusable through
    the engine's `pub use` shim -- the same widening earlier waves performed.
    Trait-impl methods are never widened (that would be E0449); only free
    items, the listed inherent methods, and the listed struct fields are.

The engine file becomes `pub use refact_llm::stream_core::*;` followed by the
residue, byte-for-byte.

Run: python tools/dev/ab2-move.py            (apply)
     python tools/dev/ab2-move.py --dry-run  (print only)
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENGINE, "src", "chat", "stream_core.rs")
CRATE = os.path.join(ENGINE, "crates", "refact-llm")
DST = os.path.join(CRATE, "src", "stream_core.rs")
CRATE_LIB = os.path.join(CRATE, "src", "lib.rs")

# 1-based inclusive ranges, each carrying the anchor its first line must contain.
MOVED = [
    (61, 182, "fn merge_usage("),
    (612, 699, "#[derive(Default, Clone)]"),
    (701, 1057, "const FUNCTION_CALLS_OPEN_TAG:"),
    (1059, 1235, "fn process_stream_event_data<C: StreamCollector>("),
    (1237, 1331, "const MAX_NDJSON_LINE_BYTES: usize"),
    (1333, 1434, "fn finalize_accumulators<C: StreamCollector>("),
    (2980, 3141, "/// Merges incoming thinking blocks"),
]

REWRITES = [
    ("crate::llm::adapter::", "crate::adapter::"),
    ("crate::call_validation::ChatToolCall", "refact_core::chat_types::ChatToolCall"),
    ("crate::call_validation::ChatToolFunction", "refact_core::chat_types::ChatToolFunction"),
]

ALLOWED_REFS = (
    "crate::adapter",
    "crate::canonical",
    "refact_chat_api",
    "refact_core",
    "refact_chat_history",
)

# inherent methods the engine residue calls through the shim
WIDEN_METHODS = ["new", "replay"]

# structs whose fields the engine residue and its kept tests touch
WIDEN_FIELDS = ["LlmStreamError", "ReplayCollector", "PartialOutputCollector", "ChoiceAccumulator"]

CRATE_HEADER = """use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use refact_chat_api::DeltaOp;
use refact_chat_history::openai_merge::ToolCallAccumulator;
use refact_chat_history::retry_policy::{
    classify_llm_error_for_retry, should_retry_llm_error, RetryDecision,
};
use refact_core::chat_types::ChatUsage;
use refact_core::llm_types::BaseModelRecord;

use crate::adapter::{LlmWireAdapter, StreamParseError, WireFormat};
use crate::canonical::{LlmRequest, LlmStreamDelta};
"""

SHIM_HEADER = "pub use refact_llm::stream_core::*;\n"

def read(path):
    with open(path, "r", encoding="utf-8", newline="") as fh:
        return fh.read()


def write(path, text):
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def probe(text, start, anchor):
    line = text.splitlines()[start - 1]
    if anchor not in line:
        raise SystemExit(
            "anchor mismatch at line %d: expected %r, found %r" % (start, anchor, line)
        )


def widen(lines):
    out = []
    for line in lines:
        if line.startswith(("fn ", "struct ", "enum ", "const ", "static ", "type ")):
            out.append("pub " + line)
        elif line.startswith("    fn "):
            name = re.split(r"[<(]", line[4:])[0][len("fn "):].strip()
            if name in WIDEN_METHODS:
                out.append("    pub " + line[4:])
            else:
                out.append(line)
        else:
            out.append(line)
    for name in WIDEN_FIELDS:
        head = "pub struct %s" % name
        start = next(
            (
                i
                for i, line in enumerate(out)
                if line.startswith(head)
                and (len(line) == len(head) or line[len(head)] in " <{")
            ),
            None,
        )
        if start is None:
            raise SystemExit("struct %s not found for field widening" % name)
        for i in range(start + 1, len(out)):
            stripped = out[i].strip()
            if stripped.startswith("}"):
                break
            if stripped and not stripped.startswith(("pub ", "//", "#")):
                out[i] = "    pub " + stripped
    return out


def main():
    dry = "--dry-run" in sys.argv
    text = read(SRC).replace("\r\n", "\n")
    lines = text.split("\n")

    moved_idx = set()
    for start, end, anchor in MOVED:
        probe(text, start, anchor)
        moved_idx.update(range(start - 1, end))

    moved_lines = [line for i, line in enumerate(lines) if i in moved_idx]
    kept_lines = [line for i, line in enumerate(lines) if i not in moved_idx]
    moved_text = "\n".join(moved_lines)

    unknown = sorted(
        {
            m.group(0)
            for m in re.finditer(r"\bcrate::[A-Za-z_][A-Za-z0-9_:]*", moved_text)
            if not m.group(0).startswith(ALLOWED_REFS)
        }
    )
    for old, new in REWRITES:
        moved_text = moved_text.replace(old, new)
    unknown_after = sorted(
        {
            m.group(0)
            for m in re.finditer(r"\bcrate::[A-Za-z_][A-Za-z0-9_:]*", moved_text)
            if not m.group(0).startswith(ALLOWED_REFS)
        }
    )

    body = "\n".join(widen(moved_text.split("\n")))
    crate_text = CRATE_HEADER + "\n" + body.rstrip("\n") + "\n"
    shim_text = SHIM_HEADER + "\n".join(kept_lines).lstrip("\n").rstrip("\n") + "\n"

    lib = read(CRATE_LIB)
    lib_new = lib
    if "pub mod stream_core;" not in lib_new:
        lib_new = lib_new.replace(
            "pub mod provider_quirks;", "pub mod provider_quirks;\npub mod stream_core;"
        )

    print("moved production lines : %d" % len([l for l in moved_lines if l.strip()]))
    print("engine residue lines   : %d" % len(kept_lines))
    print("crate body lines       : %d" % len(crate_text.split("\n")))
    print("unknown crate:: refs   : %s" % (unknown or "none"))
    print("unknown after rewrite  : %s" % (unknown_after or "none"))
    print(
        "pub items in crate body: %d"
        % len(re.findall(r"(?m)^pub (?:struct|enum|fn|const|type)", body))
    )
    print("trait-impl methods hit : %d" % body.count("pub fn fmt("))
    print("lib.rs registration    : %s" % ("added" if lib_new != lib else "already present"))
    if dry:
        return
    write(DST, crate_text)
    write(SRC, shim_text)
    if lib_new != lib:
        write(CRATE_LIB, lib_new)
    print("applied.")


main()
