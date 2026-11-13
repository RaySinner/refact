"""Body-identity diff for the Y-1 chat harvest (see verify-chat-harvest.ps1).

Compares every function/struct/const that moved out of `engine/src/chat` against its
original in `git HEAD`, after normalising ONLY the signature-visibility and
import-path rewrites the move requires. Prints `=== checks=N fail=M ===` on the last
line; exits 0 regardless (the PowerShell driver reads the counters).
"""

import os
import re
import subprocess
import sys

BASE = None

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# Original file paths, as they existed before the move.
WRITER_PATH = "refact-agent/engine/src/chat/trajectories/writer.rs"
ROSTER_PATH = "refact-agent/engine/src/chat/room_roster.rs"


def resolve_base():
    """Newest reachable revision whose engine writer.rs still holds the un-moved module.

    `HEAD` is the wrong base once the move is committed, so the base is discovered by walking
    recent history for the last revision where the engine file was still the full module.
    `REFACT_HARVEST_BASE` overrides the search.
    """
    override = os.environ.get("REFACT_HARVEST_BASE")
    if override:
        return override
    revs = subprocess.run(
        ["git", "log", "--format=%H", "-n", "50"], capture_output=True, cwd=REPO
    ).stdout.decode().split()
    revs.append("HEAD")
    for rev in revs:
        blob = subprocess.run(
            ["git", "show", rev + ":" + WRITER_PATH], capture_output=True, cwd=REPO
        )
        if blob.returncode == 0 and blob.stdout.count(b"\n") > 100:
            return rev
    raise SystemExit(
        "could not find a revision that still contains the un-moved engine writer.rs; "
        "pass REFACT_HARVEST_BASE=<rev>"
    )


def repo_path(*parts):
    return os.path.join(REPO, *parts)


def git_show(path):
    global BASE
    if BASE is None:
        BASE = resolve_base()
    out = subprocess.run(
        ["git", "show", BASE + ":" + path], capture_output=True, cwd=REPO
    )
    if out.returncode != 0:
        raise SystemExit("git show failed for " + path + ": " + out.stderr.decode("utf-8", "replace"))
    return out.stdout.decode("utf-8", "replace").replace("\r\n", "\n")


def read(path):
    with open(repo_path(*path.split("/")), encoding="utf-8") as handle:
        return handle.read().replace("\r\n", "\n")


def norm(text):
    text = re.sub(r"\bpub\(super\)", "pub", text)
    text = re.sub(r"\bpub\(crate\)", "pub", text)
    text = re.sub(r"(?m)^(\s*)pub\s+", r"\1", text)
    text = text.replace(
        "crate::runtime_settings::current()", "refact_core::trajectory_settings::current()"
    )
    text = text.replace(
        "crate::chat::types::{TrajectoryCommitIntent, TrajectorySourceIdentity}",
        "refact_chat_api::TrajectorySourceIdentity",
    )
    text = text.replace('feature = "bench"', 'feature = "test-util"')
    text = text.replace("crate::chat::internal_roles::EVENT_ROLE", "EVENT_ROLE")
    text = text.replace("use refact_core::chat_types::", "use crate::chat_types::")
    return text


def body(text, marker):
    i = text.find(marker)
    if i < 0:
        return None
    j = text.find("{", i)
    semi = text.find(";", i)
    if j < 0 or (semi >= 0 and semi < j):
        return text[i:semi + 1]
    depth = 0
    k = j
    while k < len(text):
        if text[k] == "{":
            depth += 1
        elif text[k] == "}":
            depth -= 1
            if depth == 0:
                return text[i:k + 1]
        k += 1
    return None


WRITER = [
    "async fn atomic_write_file",
    "fn unique_trajectory_tmp_path",
    "async fn atomic_write_json_with_tmp_path",
    "fn rollout_switch_enabled",
    "fn trajectory_writer_rollout_enabled()",
    "fn trajectory_writer_rollout_enabled_for",
    "fn trajectory_watcher_self_write_rollout_enabled_for",
    "const TRAJECTORY_SELF_WRITE_TTL",
    "struct TrajectoryWriteFingerprint",
    "fn trajectory_self_writes()",
    "fn watcher_trajectory_source",
    "fn prune_trajectory_self_writes",
    "async fn canonical_trajectory_write_path",
    "async fn trajectory_write_fingerprint",
    "async fn trajectory_fingerprint_matches",
    "async fn register_trajectory_self_write",
    "fn discard_trajectory_self_write",
    "async fn complete_trajectory_self_write",
    "async fn consume_trajectory_self_write",
    "fn clear_trajectory_self_writes",
    "impl Drop for TrajectorySelfWriteScope",
    "fn clear_all_trajectory_self_writes",
    "async fn atomic_write_json(",
]

ROSTER = [
    "fn render_room_roster",
    "fn identity_key",
    "fn render_member_entry",
    "fn join_capped",
    "fn truncate",
    "fn roster_for_card",
    "fn room_roster_message",
    "fn upsert_room_roster_message",
]


def test_names(text):
    return sorted(re.findall(r"fn (\w+)\(\)", text))


def norm_tests(text):
    text = norm(text)
    text = text.replace(
        "use crate::tasks::rooms::new_room_member;",
        "use refact_tasks::rooms::new_room_member;",
    )
    for line in (
        "    use crate::tasks::types::TeamMemberReport;\n",
        "    use crate::tasks::types::TeamStatus;\n",
        "    use refact_tasks::types::{TeamMemberReport, TeamStatus};\n",
    ):
        text = text.replace(line, "")
    return text


def main():
    global BASE
    BASE = resolve_base()
    print("base revision: " + BASE)
    old_writer = git_show(WRITER_PATH)
    new_writer = read("refact-agent/engine/crates/refact-chat-history/src/trajectory_writer.rs")
    old_roster = git_show(ROSTER_PATH)
    new_roster = read("refact-agent/engine/crates/refact-chat-api/src/room_roster.rs")
    new_engine_roster = read("refact-agent/engine/src/chat/room_roster.rs")

    checks = 0
    fail = 0

    for name, markers, old, new in (
        ("writer", WRITER, old_writer, new_writer),
        ("room_roster", ROSTER, old_roster, new_roster),
    ):
        for marker in markers:
            checks += 1
            a = body(old, marker)
            b = body(new, marker)
            if a is None:
                print("MISSING-OLD %-10s %s" % (name, marker))
                fail += 1
            elif b is None:
                print("MISSING-NEW %-10s %s" % (name, marker))
                fail += 1
            elif norm(a) == norm(b):
                print("IDENTICAL   %-10s %s" % (name, marker))
            else:
                fail += 1
                print("DIFFERS     %-10s %s" % (name, marker))
                na, nb = norm(a).split("\n"), norm(b).split("\n")
                for idx in range(max(len(na), len(nb))):
                    la = na[idx] if idx < len(na) else "<none>"
                    lb = nb[idx] if idx < len(nb) else "<none>"
                    if la != lb:
                        print("    old: " + la)
                        print("    new: " + lb)

    kept_old = body(old_roster, "async fn load_room_roster")
    kept_new = body(new_engine_roster, "async fn load_room_roster")
    checks += 1
    if kept_old is None or kept_new is None:
        print("MISSING      room_roster  async fn load_room_roster (kept in engine)")
        fail += 1
    elif norm(kept_old) == norm(kept_new):
        print("IDENTICAL    room_roster  async fn load_room_roster (kept in engine)")
    else:
        print("DIFFERS      room_roster  async fn load_room_roster (kept in engine)")
        fail += 1

    print()
    print("=== roster test count old=%d new=%d ===" % (
        len(test_names(old_roster)), len(test_names(new_roster))
    ))
    old_names = test_names(old_roster)
    for test in test_names(new_roster):
        checks += 1
        if test in old_names:
            print("TEST-PRESENT  " + test)
        else:
            print("TEST-MISSING  " + test)
            fail += 1

    checks += 1
    old_tests = body(old_roster, "#[cfg(test)]")
    new_tests = body(new_roster, "#[cfg(test)]")
    if old_tests is None or new_tests is None:
        print("TESTS-BODY   missing on one side")
        fail += 1
    elif norm_tests(old_tests) == norm_tests(new_tests):
        print("TESTS-BODY   identical (imports rewritten only)")
    else:
        print("TESTS-BODY   differs")
        fail += 1

    print()
    print("=== checks=%d fail=%d ===" % (checks, fail))
    sys.exit(0)


if __name__ == "__main__":
    main()
