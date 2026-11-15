"""Z-2 step 0: are the six 0-blocked tiny modules' "movable" entries real bodies?

Each is a declaration-only module root (`pub mod` + `pub use`), so there is no
production body to harvest; the module still has to exist because engine paths
(`crate::<module>::<child>`) resolve through it.

    python tools/dev/z2-tiny.py
"""
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SRC = os.path.join(ROOT, "refact-agent", "engine", "src")

for module in ("codegraph", "llm", "yaml_configs", "stats", "worktrees", "exec"):
    path = os.path.join(SRC, module, "mod.rs")
    body = open(path, encoding="utf-8", errors="replace").read()
    fns = re.findall(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+\w+", body)
    children = re.findall(r"(?m)^pub mod (\w+);", body)
    reexports = len(re.findall(r"(?m)^pub use ", body))
    print(
        "%-14s lines=%3d fn=%d pub-mod=%d pub-use=%d children=%s"
        % (module, len(body.split("\n")), len(fns), len(children), reexports, ",".join(children))
    )
