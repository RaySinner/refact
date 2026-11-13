"""Verify that every external crate root used by moved files is a declared dependency.

Collects (a) the first segment of every `use` path, and (b) crate roots referenced
in expression position (`serde_json::json!`, `tokio::fs::...`), ignoring
turbofish (`from_slice::<T>`).

Usage: python y3-externs.py
"""
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CRATE = os.path.join(
    ROOT, "refact-agent", "engine", "crates", "refact-http", "src", "routers", "v1"
)

STD = {"std", "core", "alloc", "crate", "self", "super", "test", "tests", "main"}

KNOWN_DEPS = set()
with open(
    os.path.join(ROOT, "refact-agent", "engine", "crates", "refact-http", "Cargo.toml"),
    encoding="utf-8",
) as fh:
    for line in fh:
        m = re.match(r"^([A-Za-z0-9_-]+)\s*=", line)
        if m:
            KNOWN_DEPS.add(m.group(1).replace("-", "_"))

# `astral-tokio-tar` exposes the library as `tokio_tar`.
KNOWN_DEPS.add("tokio_tar")

USE_STMT = re.compile(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+([^;]+);", re.S)
INLINE = re.compile(r"(?<![:\w])([a-z_][a-z0-9_]*)::(?=[A-Za-z_])")


def strip_uses(text):
    return USE_STMT.sub("", text)


def roots(text):
    found = set()
    for m in USE_STMT.finditer(text):
        body = re.sub(r"\s+", " ", m.group(1))
        prefix = body.split("{")[0].rstrip(" :")
        head = prefix.split("::")[0].strip()
        if re.match(r"^[a-z_][a-z0-9_]*$", head):
            found.add(head)
    found |= set(INLINE.findall(strip_uses(text)))
    return {f for f in found - STD if f not in ("refact_http",)}


missing = {}
for name in sorted(os.listdir(CRATE)):
    if not name.endswith(".rs"):
        continue
    with open(os.path.join(CRATE, name), encoding="utf-8") as fh:
        text = fh.read()
    unknown = sorted(r for r in roots(text) if r not in KNOWN_DEPS)
    if unknown:
        missing[name] = unknown

if not missing:
    print("OK: every external root used by the moved files is a declared dependency")
for name, unknown in missing.items():
    print(f"{name}: undeclared -> {unknown}")
