"""AL-1 behaviour-preservation check.

For each settled file, finds the newest git revision in which the engine-side
`chat/trajectories/<name>.rs` still held its body, then reverses the mover's
transformations on the moved copy and normalises away the *mechanical* classes
(path rewrites, test gating, visibility widening). Anything left is a real
content change.

Discovering the base revision from history (instead of pinning `HEAD~1`) keeps
this honest after the settlement is committed.

Run: python tools/dev/al1_diff.py
"""
import difflib
import io
import os
import re
import subprocess
import sys

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
RA_TRAJ = os.path.join(ENGINE, "crates", "refact-runtime-api", "src", "trajectories")

FILES = ["load", "paths", "reload", "watcher", "queries", "store"]
MIN_BODY_LINES = 100

REVERSE = [
    ("refact_chat_api::setparams::map_legacy_mode_to_id",
     "crate::yaml_configs::customization_registry::map_legacy_mode_to_id"),
    ("refact_files::path_utils::canonicalize_normalized_path",
     "crate::files_correction::canonicalize_normalized_path"),
    ("refact_chat_api::session::goal_snapshot_from_messages",
     "crate::chat::session::goal_snapshot_from_messages"),
    ("refact_chat_history::trajectory_index", "crate::chat::trajectory_index"),
    ("refact_core::chat_config::timeouts", "crate::chat::config::timeouts"),
    ("refact_perf::perf_diagnostics", "crate::chat::perf_diagnostics"),
    ("crate::chat_ctx::ChatCtx", "crate::chat::ctx::ChatCtx"),
    ("refact_chat_api::types::SessionsMap", "crate::chat::SessionsMap"),
    ("refact_chat_api::types", "crate::chat::types"),
    ("refact_chat_api::TraceParent::from_parts", "crate::subchat::TraceParent::from_parts"),
    ("crate::trajectory_prefix::", "refact_runtime_api::trajectory_prefix::"),
    ("crate::trajectory_runtime::", "refact_runtime_api::trajectory_runtime::"),
    ("crate::trajectory_model_runtime::", "refact_runtime_api::trajectory_model_runtime::"),
    ("crate::global_context::GlobalContextCtx", "refact_runtime_api::GlobalContextCtx"),
    ("refact_core::call_validation::", "crate::call_validation::"),
    ("refact_core::custom_error::ScratchError", "crate::custom_error::ScratchError"),
    ("crate::task_storage_runtime::", "crate::tasks::storage::"),
    ("refact_worktrees::service::", "crate::worktrees::service::"),
    ("refact_worktrees::types::", "crate::worktrees::types::"),
]

WIDEN = re.compile(
    r"\bpub (async\s+)?(fn|struct|enum|trait|type|const|static|union|mod|use)\b")
VIS = re.compile(r"pub\((?:crate|super|self)\)\s*")
FIELD = re.compile(r"pub ([a-z_][A-Za-z0-9_]*\s*:)")
CFG = re.compile(r"\s*#\[cfg\((?:not\()?(?:any\()?test")


def git(*args):
    return subprocess.run(["git"] + list(args), cwd=ROOT, capture_output=True)


def engine_path(name):
    return "refact-agent/engine/src/chat/trajectories/%s.rs" % name


def last_full_revision(name):
    """Newest commit whose engine blob still held the body (not the one-line shim)."""
    log = git("log", "--format=%H", "-n", "40", "--", engine_path(name))
    for rev in log.stdout.decode().split():
        blob = git("show", "%s:%s" % (rev, engine_path(name)))
        if blob.returncode != 0:
            continue
        text = blob.stdout.decode("utf-8", "replace")
        if len(text.splitlines()) >= MIN_BODY_LINES:
            return rev, text.replace("\r\n", "\n")
    raise SystemExit("no full-body revision found for %s" % name)


def normalize(text):
    """Collapse the move's mechanical classes: path rewrites, test gating,
    visibility widening, and whitespace."""
    for old, new in REVERSE:
        text = text.replace(old, new)
    out = []
    for line in text.split("\n"):
        if CFG.match(line):
            continue
        line = VIS.sub("", line)
        line = WIDEN.sub(r"\1\2", line)
        line = FIELD.sub(r"\1", line)
        line = re.sub(r"\s+", " ", line).strip()
        out.append(line)
    while out and out[-1] == "":
        out.pop()
    return out


failures = 0
for name in FILES:
    rev, body = last_full_revision(name)
    orig = normalize(body)
    moved_path = os.path.join(RA_TRAJ, name + ".rs")
    if not os.path.isfile(moved_path):
        print("== %s: MISSING moved file" % name)
        failures += 1
        continue
    moved = normalize(open(moved_path, encoding="utf-8", errors="replace").read())
    diff = [l for l in difflib.unified_diff(orig, moved, lineterm="", n=0)
            if l[:1] in "+-" and not l.startswith(("+++", "---"))]
    if diff:
        failures += 1
    print("== %s vs %s: orig=%d moved=%d -> %s"
          % (name, rev[:9], len(orig), len(moved),
             "IDENTICAL (after normalisation)" if not diff
             else "%d REAL differences" % len(diff)))
    for line in diff[:24]:
        print("     " + line[:150])

print("al1_diff: %d file(s) with real differences" % failures)
sys.exit(1 if failures else 0)
