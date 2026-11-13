"""Byte-identity check for the I-1 context_rebuild harvest.

Every symbol that moved from `engine/src/chat/context_rebuild.rs` into
`refact-chat-api/src/context_rebuild.rs` (or into the engine's new
`chat/context_rebuild/pipeline.rs`) must be identical to its original after
normalising only `pub` / `pub(crate)` / `pub(super)` visibility and whitespace.

Usage:
    python tools/dev/verify-i1-island-body.py <original.rs|GITREF:path> <crate.rs> <pipeline.rs>

`GITREF:path` (e.g. `HEAD:refact-agent/engine/src/chat/context_rebuild.rs`) reads the
original straight out of git as UTF-8, so non-ASCII prose survives intact.

Exits non-zero when any checked item differs, and prints
`=== checks=N fail=M ===` on the last line so a PowerShell wrapper can assert it.
"""

import re
import subprocess
import sys

# Anchors are unique substrings that start an item declaration. Each entry is
# (label, anchor) where the anchor is searched in BOTH files.
CRATE_ITEMS = [
    ("ManualCompressionPreview", "struct ManualCompressionPreview"),
    ("ManualCompressionApplyResult", "struct ManualCompressionApplyResult"),
    ("pending_tools", "fn pending_tools("),
    ("at_auto_cap", "fn at_auto_cap("),
    ("idle", "fn idle("),
    ("compaction_safe", "fn compaction_safe("),
    ("status", "fn status("),
    ("emit_compression_skipped_status", "fn emit_compression_skipped_status("),
    ("AttemptGuard", "struct AttemptGuard"),
    ("AttemptGuard::drop", "impl Drop for AttemptGuard"),
    ("prepare_explicit_idle", "fn prepare_explicit_idle("),
    (
        "resolve_effective_compression_cap",
        "fn resolve_effective_compression_cap(",
    ),
    ("CompactionOutcome", "enum CompactionOutcome"),
    ("CompactionOutcome::applied", "impl CompactionOutcome"),
]

PIPELINE_ITEMS = [
    ("RECONSTRUCTION_TIMEOUT", "const RECONSTRUCTION_TIMEOUT"),
    ("model_tokenizer", "fn model_tokenizer("),
    ("estimated_tokens", "fn estimated_tokens("),
    ("budget_prefix_is_usable", "fn budget_prefix_is_usable("),
    ("payload_request_cap", "fn payload_request_cap("),
    ("validate_output", "fn validate_output("),
    ("ReconstructionMetrics", "struct ReconstructionMetrics"),
    ("ReconstructionMetrics::to_json", "impl ReconstructionMetrics"),
    ("commit_report", "fn commit_report<"),
    ("resolve_model", "fn resolve_model("),
    ("preview_manual_context_rebuild", "fn preview_manual_context_rebuild("),
    ("rebuild_session", "fn rebuild_session("),
    ("apply_manual_context_rebuild", "fn apply_manual_context_rebuild("),
    (
        "apply_context_rebuild_with_reason",
        "fn apply_context_rebuild_with_reason(",
    ),
    (
        "apply_deterministic_compaction_for_recovery",
        "fn apply_deterministic_compaction_for_recovery(",
    ),
    ("drain_pending", "fn drain_pending("),
]


def read(path):
    if ":" in path and not path[1:2] == ":":
        ref, _, repo_path = path.partition(":")
        return subprocess.run(
            ["git", "show", ref + ":" + repo_path],
            capture_output=True,
            check=True,
        ).stdout.decode("utf-8", errors="replace")
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        return handle.read()


def normalise(text):
    # Normalise only: visibility narrowing/widening, whitespace, and the trailing
    # comma a collapsed parameter list loses when `pub(crate)` becomes `pub`.
    text = re.sub(r"\bpub(?:\s*\([^)]*\))?\s+", "", text)
    text = re.sub(r"\s+", " ", text)
    text = re.sub(r"[\s,]+\)", ")", text)
    text = re.sub(r"\(\s+", "(", text)
    return text.strip()


def item_at(text, anchor):
    """Normalised source of the item whose declaration starts at `anchor`."""
    index = text.find(anchor)
    if index < 0:
        return None
    # Walk back to the line start so the declaration is included verbatim.
    start = text.rfind("\n", 0, index) + 1
    depth = 0
    saw_brace = False
    cursor = start
    while cursor < len(text):
        char = text[cursor]
        if char == "{":
            depth += 1
            saw_brace = True
        elif char == "}":
            depth -= 1
            if saw_brace and depth == 0:
                cursor += 1
                break
        elif char == ";" and not saw_brace:
            cursor += 1
            break
        cursor += 1
    return normalise(text[start:cursor])


def compare(label, anchor, original, moved, target):
    want = item_at(original, anchor)
    got = item_at(moved, anchor)
    if want is None:
        print("FAIL  original has no item %r" % label)
        return 1
    if got is None:
        print("FAIL  %s has no item %r" % (target, label))
        return 1
    if want != got:
        print("FAIL  %s %r differs from the original" % (target, label))
        return 1
    print("PASS  %s %r byte-identical" % (target, label))
    return 0


def main():
    original_path, crate_path, pipeline_path = sys.argv[1:4]
    original = read(original_path)
    crate = read(crate_path)
    pipeline = read(pipeline_path)

    checks = 0
    fail = 0
    for label, anchor in CRATE_ITEMS:
        checks += 1
        fail += compare(label, anchor, original, crate, "chat-api")
    for label, anchor in PIPELINE_ITEMS:
        checks += 1
        fail += compare(label, anchor, original, pipeline, "pipeline")

    print("=== checks=%d fail=%d ===" % (checks, fail))
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())
