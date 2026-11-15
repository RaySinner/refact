"""Z-2 step 2: classify the `engine-build.ps1 -Mode check-tests` log against the
documented pre-existing 36-error baseline.

    python tools/dev/z2-baseline.py [refact-agent/engine/target/z2-check-tests.log]

PowerShell's `*>` redirection writes UTF-16 and drops rustc's `--> file:line`
lines, so errors are classified by their message. The documented baseline is only
  * `cannot find macro json` (2, src/tools/tool_task_spawn_agent.rs),
  * StageJob / StageProduct / now_ms (25, src/tools/review_agents/mod.rs),
  * IndexingEverywhere / is_valid_file / now_ms (9, src/files_in_workspace/tests.rs).
Anything else is a regression this card would have introduced.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DEFAULT_LOG = os.path.join(ROOT, "refact-agent", "engine", "target", "z2-check-tests.log")

KNOWN = (
    "cannot find macro `json`",
    "`StageJob`",
    "`StageProduct`",
    "`IndexingEverywhere`",
    "`is_valid_file`",
    "`now_ms`",
)

log_path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_LOG
raw = open(log_path, "rb").read()
if b"\x00" in raw[:400]:
    log = raw.decode("utf-16", errors="replace")
else:
    log = raw.decode("utf-8", errors="replace")

ERROR = re.compile(r"^error(?:\[E\d+\])?: (.+)$", re.M)
messages = [m.strip() for m in ERROR.findall(log)]
summary = [m for m in messages if m.startswith("could not compile")]
errors = [m for m in messages if not m.startswith("could not compile")]

print("%d errors" % len(errors))
buckets = {}
for message in errors:
    for known in KNOWN:
        if known in message:
            buckets[known] = buckets.get(known, 0) + 1
            break
    else:
        buckets[message] = buckets.get(message, 0) + 1

for name, count in sorted(buckets.items()):
    print("  %-46s %d" % (name, count))

unexpected = {name: count for name, count in buckets.items() if name not in KNOWN}
ok = len(errors) == 36 and not unexpected and summary
print("summary line: %s" % (summary[:1] or "none"))
print("baseline match: %s" % ok)
sys.exit(0 if ok else 1)
