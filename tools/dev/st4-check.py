"""ST-4 self-check: chat_reactions moved verbatim + shim shape + no crate->engine edge."""
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")

ENGINE = "refact-agent/engine"
ORIG = f"{ENGINE}/src/buddy/chat_reactions.rs"
CRATE = f"{ENGINE}/crates/refact-runtime-api/src/buddy_chat_reactions.rs"

failures = []


def check(name, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        failures.append(name)


# Commit that still holds the pre-move engine file. HEAD is not usable: after the
# move the engine path is the shim, so the comparison must pin the last commit
# that carried the original body.
PRE_MOVE = "1334104d0"


def git_show(path):
    out = subprocess.run(
        ["git", "show", f"{PRE_MOVE}:{path}"],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if out.returncode != 0:
        raise SystemExit(f"git show {PRE_MOVE}:{path} failed: {out.stderr}")
    return out.stdout


original = git_show(ORIG)
moved = open(CRATE, encoding="utf-8").read()
shim = open(ORIG, encoding="utf-8").read()


def prod_lines(text):
    """Production lines of the original: everything before the inline test module."""
    marker = "#[cfg(test)]\nmod tests {"
    idx = text.find(marker)
    return text if idx == -1 else text[:idx]


def norm_body(lines):
    """Body lines with the import/attribute preamble dropped.

    Only the module path of a call/parameter type may change: the move rewrites
    `crate::buddy::actor::X` / `super::actor::X` to the crate's own `X`, which is
    the documented exception to "verbatim".
    """
    out = []
    started = False
    for raw in lines:
        line = raw.rstrip()
        if not started:
            if re.match(r"^(pub )?(async )?fn |^static |^pub struct |^#\[derive", line):
                started = True
            else:
                continue
        line = line.replace("crate::buddy::actor::", "").replace("super::actor::", "")
        out.append(line)
    return [line for line in out if line.strip()]


orig_body = norm_body(prod_lines(original).splitlines())
new_body = norm_body(prod_lines(moved).splitlines())

check(
    "moved body preserves the original production body",
    orig_body == new_body,
    f"orig={len(orig_body)} new={len(new_body)} first_diff="
    + str(next((i for i, (a, b) in enumerate(zip(orig_body, new_body)) if a != b), None)),
)

check(
    "engine file is a pure crate facade",
    shim.strip().splitlines()[0] == "pub use refact_runtime_api::buddy_chat_reactions::*;",
)
prod_shim = prod_lines(shim).strip().splitlines()
check(
    "engine facade has no production logic besides the re-export",
    all(
        line.startswith("pub use") or not line.strip() or line.startswith("//")
        for line in prod_shim
    ),
    str(prod_shim),
)

# crate->engine: the crate must not name the engine crate `refact_lsp` / `crate::chat`.
crate_engine_refs = [
    line
    for line in moved.splitlines()
    if re.search(r"\brefact_lsp\b", line) or re.search(r"^use crate::(chat|buddy|ext|tools)::", line)
]
check("crate module has no engine-module imports", not crate_engine_refs, str(crate_engine_refs[:5]))

# engine must depend on the crate for chat_reactions (no copies left behind).
check(
    "engine facade forwards to refact-runtime-api",
    "refact_runtime_api::buddy_chat_reactions" in shim,
)

# public surface the engine test modules import by name.
needed = [
    "AcceptedUserMessage",
    "ChatActivityCompletion",
    "maybe_enqueue_chat_reaction",
    "maybe_enqueue_chat_activity_reaction",
    "chat_reaction_candidate",
    "should_observe_thread",
    "INSIGHT_LINES",
    "AMBIENT_LINES",
    "HUMOR_LINES",
    "CHAT_ACTIVITY_ANALYSIS_TEXT",
    "ChatReactionKind",
    "ChatReactionLimiter",
    "ChatReactionSkipReason",
    "ChatReactionDebugState",
    "GLOBAL_HOURLY_CAP",
    "PER_CHAT_COOLDOWN_SECS",
]
missing = [name for name in needed if name not in moved]
check(
    "crate module exposes every name the engine tests import",
    not missing or moved.count("pub use refact_buddy_core::chat_reactions::*;") == 1,
    str(missing),
)

# the in-file unit test moved with the body
check(
    "the file's own unit test moved into the crate",
    "fn chat_reaction_candidate_reports_skip_reasons" in moved and "mod tests {" in moved,
)
check(
    "the engine file no longer duplicates that test",
    "fn chat_reaction_candidate_reports_skip_reasons" not in shim,
)

# module registration
lib = open(f"{ENGINE}/crates/refact-runtime-api/src/lib.rs", encoding="utf-8").read()
check("runtime-api registers the new module", "pub mod buddy_chat_reactions;" in lib)

print()
if failures:
    print(f"ST-4 CHECK FAILED ({len(failures)}): {', '.join(failures)}")
    sys.exit(1)
print("ST-4 CHECK PASSED")
