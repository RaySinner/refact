"""Z-2 step 0/after: per-module census + the movable list, plus asset-reference and
`include_str!` sweeps for every touched file.

    python tools/dev/z2-report.py
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENG = os.path.join(ROOT, "refact-agent", "engine")
MODULES = [
    "scheduler", "tasks", "caps", "codegraph", "llm",
    "yaml_configs", "stats", "worktrees", "exec",
]

for module in MODULES:
    out = subprocess.run(
        [sys.executable, os.path.join(ROOT, "tools", "dev", "chat_movable_census.py"), module],
        cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    print("### " + (out.stdout or "").strip().replace("\n", "\n"))
    report = os.path.join(ENG, "target", "%s_movable_census.txt" % module)
    if os.path.exists(report):
        for line in open(report, encoding="utf-8", errors="replace").read().split("\n"):
            if line.startswith("###") or line.strip().startswith(tuple("0123456789")):
                print("    " + line)
    print()

print("### include_str!/include_bytes! in the touched files")
touched = []
for module in ("scheduler", "tasks", "caps"):
    for root, dirs, files in os.walk(os.path.join(ENG, "src", module)):
        dirs[:] = [d for d in dirs if d != "target"]
        touched.extend(os.path.join(root, f) for f in files if f.endswith(".rs"))
touched.append(os.path.join(ENG, "src", "app_state.rs"))
for root, dirs, files in os.walk(os.path.join(ENG, "crates", "refact-runtime-api", "src")):
    touched.extend(os.path.join(root, f) for f in files if f.endswith(".rs"))
hits = []
for path in touched:
    body = open(path, encoding="utf-8", errors="replace").read()
    for needle in ("include_str!", "include_bytes!"):
        if needle in body:
            hits.append((os.path.relpath(path, ENG).replace("\\", "/"), needle))
print("  %s" % (hits or "none"))
