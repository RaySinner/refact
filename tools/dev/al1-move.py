"""AL-1 mover: settle the `chat::trajectories` SCC into `refact-runtime-api`.

Measured shape (census, AL-1 step 0):

  load   986 prod   deps=[paths, watcher]
  paths 1054 prod   deps=[load]
  reload 663 prod   deps=[watcher]
  watcher 721 prod  deps=[paths, queries, reload]
  queries 1681 prod deps=[load, paths, reload, watcher]  engine=[subchat::from_parts]
  store 1316 prod   deps=[load, paths]

All six carry `ChatCtx`/`AppState`/`GlobalContext`-shaped coupling to
`refact-runtime-api`-resident modules (chat_ctx, app_state, global_context,
task_storage, files_correction, runtime_settings, indexing_routing,
trajectory_runtime/trajectory_prefix/trajectory_model_runtime).

Destination crate, both Cargo edges checked:

  * `refact-chat-history/Cargo.toml` deps = refact-core, refact-chat-api,
    refact-files, refact-perf  -> it has no runtime-api dependency, but
  * `refact-runtime-api/Cargo.toml` deps INCLUDE `refact-chat-history`,
    so adding runtime-api to chat-history is a hard cargo cycle.

Therefore the destination is `refact_runtime_api::trajectories::{load, paths,
reload, watcher, queries, store}`, with the two already-crate-typed siblings
(`model`, `writer`) mirrored as `refact_chat_history` facades. The engine files
become one-line `pub use` shims, which keeps `chat/trajectories/mod.rs`
(including its `#[cfg(test)] use …::{…}` blocks and its `pub(crate) use
paths::ValidTrajectoryCandidate`) resolving unchanged.

Run: python tools/dev/al1-move.py            (apply)
     python tools/dev/al1-move.py --dry-run  (print only)
"""
import importlib.util
import io
import os
import re
import sys

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
TRAJ = os.path.join(ENGINE, "src", "chat", "trajectories")
RA = os.path.join(ENGINE, "crates", "refact-runtime-api")
RA_SRC = os.path.join(RA, "src")
RA_TRAJ = os.path.join(RA_SRC, "trajectories")
RA_LIB = os.path.join(RA_SRC, "lib.rs")
RA_TOML = os.path.join(RA, "Cargo.toml")

FILES = ["load", "paths", "reload", "watcher", "queries", "store"]

# longest-first
REWRITES = [
    ("crate::yaml_configs::customization_registry::map_legacy_mode_to_id",
     "refact_chat_api::setparams::map_legacy_mode_to_id"),
    ("crate::files_correction::canonicalize_normalized_path",
     "refact_files::path_utils::canonicalize_normalized_path"),
    ("crate::chat::session::goal_snapshot_from_messages",
     "refact_chat_api::session::goal_snapshot_from_messages"),
    ("crate::chat::trajectory_index", "refact_chat_history::trajectory_index"),
    ("crate::chat::config::timeouts", "refact_core::chat_config::timeouts"),
    ("crate::chat::perf_diagnostics", "refact_perf::perf_diagnostics"),
    ("crate::chat::ctx::ChatCtx", "crate::chat_ctx::ChatCtx"),
    ("crate::chat::SessionsMap", "refact_chat_api::types::SessionsMap"),
    ("crate::chat::types", "refact_chat_api::types"),
    ("crate::subchat::TraceParent::from_parts", "refact_chat_api::TraceParent::from_parts"),
    ("refact_runtime_api::trajectory_prefix::", "crate::trajectory_prefix::"),
    ("refact_runtime_api::trajectory_runtime::", "crate::trajectory_runtime::"),
    ("refact_runtime_api::trajectory_model_runtime::", "crate::trajectory_model_runtime::"),
    ("refact_runtime_api::GlobalContextCtx", "crate::global_context::GlobalContextCtx"),
    ("crate::call_validation::", "refact_core::call_validation::"),
    ("crate::call_validation", "refact_core::call_validation"),
    ("crate::custom_error::ScratchError", "refact_core::custom_error::ScratchError"),
    ("crate::tasks::storage::", "crate::task_storage_runtime::"),
    ("crate::worktrees::service::", "refact_worktrees::service::"),
    ("crate::worktrees::types::", "refact_worktrees::types::"),
]

FORBIDDEN = (
    "crate::chat::", "crate::tasks::", "crate::worktrees::", "crate::call_validation",
    "crate::custom_error::", "crate::subchat::", "crate::yaml_configs::",
    "refact_runtime_api::",
)

BARE_ITEM_RE = re.compile(
    r"^(fn|struct|enum|trait|type|const|static|union|mod)\b")
STRUCT_HEAD_RE = re.compile(r"^pub struct ([A-Za-z_][A-Za-z0-9_]*)")

MOD_RS = '''pub mod load;
pub mod model;
pub mod paths;
pub mod queries;
pub mod reload;
pub mod store;
pub mod watcher;
pub mod writer;

pub(crate) use queries::{
    calculate_line_changes_from_chat_messages, calculate_task_progress_from_chat_messages,
    calculate_token_totals_from_chat_messages, canonical_child_path_under_root,
    count_user_messages, ensure_real_dir_tree, is_placeholder_title, is_real_dir, is_real_file,
    safe_new_trajectory_file_in_dir, safe_trajectory_file_in_dir, spawn_task_name_generation_task,
    spawn_title_generation_task, trajectory_meta_title,
};
'''

MODEL_RS = '''pub use refact_chat_history::trajectory_model::*;
pub use refact_chat_history::trajectory_snapshot::trajectory_snapshot_from_session;
pub use crate::trajectory_model_runtime::{
    apply_mode_defaults_to_thread, get_session_runtime_for_trajectory_source,
    get_session_state_for_chat,
};
'''

WRITER_RS = "pub use refact_chat_history::trajectory_writer::*;\n"


def read(path):
    with open(path, "r", encoding="utf-8", newline="") as fh:
        return fh.read()


def write(path, text):
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


SPEC = importlib.util.spec_from_file_location(
    "census", os.path.join(HERE, "chat_movable_census.py"))
CENSUS = importlib.util.module_from_spec(SPEC)
sys.argv = ["chat_movable_census.py", "chat"]


def census_brace(text, index):
    """Local brace matcher (strings/comments/lifetimes aware enough for this)."""
    depth, i = 0, index
    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""
        if ch == "/" and nxt == "/":
            nl = text.find("\n", i)
            i = len(text) if nl == -1 else nl
            continue
        if ch == "/" and nxt == "*":
            end = text.find("*/", i + 2)
            i = len(text) if end == -1 else end + 2
            continue
        if ch == '"':
            i += 1
            while i < len(text) and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
            i += 1
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return -1


def widen(text):
    """Widen `pub(crate)`/`pub(super)`/bare top-level items so the engine's glob
    shim can re-export them, and widen struct fields so engine test literals
    keep compiling. Trait-impl/inherent method bodies are untouched."""
    text = re.sub(r"(?m)(^|\s)pub\((?:crate|super)\)(?=[\s])", r"\1pub", text)
    out = []
    for line in text.split("\n"):
        if BARE_ITEM_RE.match(line):
            out.append("pub " + line)
        else:
            out.append(line)
    text = "\n".join(out)

    starts = []
    for m in STRUCT_HEAD_RE.finditer(text):
        if text[m.end():m.end() + 1] in ("<", " ", "{"):
            brace = text.find("{", m.end())
            if brace != -1 and text[m.end():brace].count(";") == 0:
                starts.append((brace, m.start()))
    bodies = []
    for brace, _ in starts:
        close = census_brace(text, brace)
        if close != -1:
            bodies.append((brace, close))
    if not bodies:
        return text
    out, cursor = [], 0
    for brace, close in sorted(bodies):
        out.append(text[cursor:brace + 1])
        for line in text[brace + 1:close].split("\n"):
            stripped = line.strip()
            if stripped and not stripped.startswith(("pub ", "//", "#", "}")):
                out.append("    pub " + stripped)
            else:
                out.append(line)
        out.append(text[close:close + 1])
        cursor = close + 1
    out.append(text[cursor:])
    return "".join(out)


def main():
    dry = "--dry-run" in sys.argv
    os.makedirs(RA_TRAJ, exist_ok=True)
    total = 0
    for name in FILES:
        src = os.path.join(TRAJ, name + ".rs")
        text = read(src).replace("\r\n", "\n")
        moved = text
        moved = re.sub(r"#\[cfg\(test\)\]",
                       '#[cfg(any(test, feature = "test-util"))]', moved)
        moved = re.sub(r"#\[cfg\(not\(test\)\)\]",
                       '#[cfg(not(any(test, feature = "test-util")))]', moved)
        for old, new in REWRITES:
            moved = moved.replace(old, new)
        moved = widen(moved)
        leftovers = sorted({m.group(0) for m in re.finditer(
            r"\b(?:crate|refact_runtime_api|refact_chat_api)::[A-Za-z_][A-Za-z0-9_:]*", moved)
            if m.group(0).startswith(("crate::chat::", "crate::tasks::", "crate::worktrees::",
                                      "crate::call_validation", "crate::custom_error::",
                                      "crate::subchat::", "crate::yaml_configs::",
                                      "refact_runtime_api::"))})
        prod = len([l for l in moved.split("\n") if l.strip()])
        total += prod
        print("== %-8s moved_lines(prod)=%-5d crate_lines=%-5d leftovers=%s"
              % (name, prod, len(moved.split("\n")), leftovers or "none"))
        if not dry:
            write(os.path.join(RA_TRAJ, name + ".rs"), moved.rstrip("\n") + "\n")
            write(src, "pub use refact_runtime_api::trajectories::%s::*;\n" % name)
    if not dry:
        write(os.path.join(RA_TRAJ, "mod.rs"), MOD_RS)
        write(os.path.join(RA_TRAJ, "model.rs"), MODEL_RS)
        write(os.path.join(RA_TRAJ, "writer.rs"), WRITER_RS)
    print("total moved production lines: %d" % total)

    lib = read(RA_LIB)
    if "pub mod trajectories;" not in lib:
        lib = lib.replace("pub mod command_line;",
                          "pub mod command_line;\npub mod trajectories;", 1)
        print("lib.rs: registered `pub mod trajectories;`")
        if not dry:
            write(RA_LIB, lib)
    else:
        print("lib.rs: already registered")

    toml = read(RA_TOML)
    if re.search(r"(?m)^md5\s*=", toml):
        print("Cargo.toml: md5 already present")
    else:
        toml = toml.replace('serde.workspace = true', 'md5 = "0.7"\nserde.workspace = true', 1)
        print("Cargo.toml: added md5")
        if not dry:
            write(RA_TOML, toml)
    print("done." if not dry else "dry run only.")


main()
