"""Run the movability census for a fixed module list and write every report.

Usage:  python tools/dev/aa6-census-all.py <out_dir> [module ...]

Used by the AA-6 card to capture a before/after snapshot of every module whose
numbers the census fix could move, and by tools/dev/aa6-check.py as its gate.
Each module's full report is copied out of
`refact-agent/engine/target/<module>_movable_census.txt`.
"""
import os
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CENSUS = os.path.join(ROOT, "tools", "dev", "chat_movable_census.py")
TARGET = os.path.join(ROOT, "refact-agent", "engine", "target")

DEFAULT_MODULES = (
    "chat",
    "integrations",
    "providers",
    "postprocessing",
    "vecdb",
    "scheduler",
    "memories",
    "tools",
    "knowledge",
    "files_in_workspace",
)


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    out_dir = sys.argv[1]
    modules = sys.argv[2:] or list(DEFAULT_MODULES)
    os.makedirs(out_dir, exist_ok=True)
    failures = 0
    for module in modules:
        result = subprocess.run(
            [sys.executable, CENSUS, module],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        report = os.path.join(TARGET, f"{module}_movable_census.txt")
        dest = os.path.join(out_dir, f"{module}.txt")
        if os.path.exists(report):
            shutil.copyfile(report, dest)
        else:
            with open(dest, "w", encoding="utf-8") as fh:
                fh.write(result.stdout.decode("utf-8", "replace"))
        with open(dest, encoding="utf-8") as fh:
            head = fh.readline().strip() or "<empty>"
        print(f"{'ok ' if result.returncode == 0 else 'FAIL'} {head}")
        if result.returncode != 0:
            failures += 1
    print(f"census-all: {len(modules) - failures}/{len(modules)} modules ok")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
