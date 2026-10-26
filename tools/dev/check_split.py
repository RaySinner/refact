#!/usr/bin/env python3
"""Static pre-check for Rust module splits.

Catches the three error classes that have burned full `cargo check` runs:

  E0364  `pub use child::X` where X is not `pub` in child
  E0624  inherent method called from a sibling module but not `pub(super)`/`pub`
  E0449  visibility qualifier on a trait item

Usage:
    python tools/dev/check_split.py <module_dir>

Exit code 0 = no problems found, 1 = problems found.
"""

import os
import re
import sys

VIS_PREFIXES = ("pub", "pub(crate)", "pub(super)", "pub(self)")

RE_MOD_DECL = re.compile(r"^\s*(?:pub(?:\([a-z]+\))?\s+)?mod\s+([a-z_][a-z0-9_]*)\s*;")
RE_PUB_USE = re.compile(r"^\s*pub\s+use\s+([a-z_][a-z0-9_]*)::\{([^}]*)\}", re.M)
RE_PUB_USE_ONE = re.compile(r"^\s*pub\s+use\s+([a-z_][a-z0-9_]*)::([a-z_][a-z0-9_]*)\s*;", re.M)
RE_ITEM_DEF = re.compile(
    r"^(?P<vis>pub(?:\([a-z]+\))?\s+)?"
    r"(?:(?:async|const|unsafe|extern(?:\s+\"[^\"]*\")?)\s+)*"
    r"(?P<kind>struct|enum|fn|const|static|type|trait|union)\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)
RE_METHOD_DEF = re.compile(
    r"^(?P<indent>\s+)(?P<vis>pub(?:\([a-z]+\))?\s+)?"
    r"(?P<async>async\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
    re.M,
)
RE_IMPL_OPEN = re.compile(r"^\s*impl(?:<[^>]*>)?\s+", re.M)
RE_SELF_CALL = re.compile(r"(?:self|service|svc|coordinator|driver)\.([a-z_][a-z0-9_]*)\s*\(")
RE_CFG_TEST = re.compile(r"#\[cfg\(test\)\]")


def read(path):
    # utf-8-sig strips a BOM, which would otherwise break every `^`-anchored regex
    with open(path, "r", encoding="utf-8-sig", errors="replace") as fh:
        return fh.read()


def strip_test_blocks(text):
    """Remove trailing `#[cfg(test)] mod tests { ... }` region if it is last."""
    idx = text.find("#[cfg(test)]\nmod tests")
    if idx > 0:
        return text[:idx]
    return text


def method_visibilities(text):
    """name -> set of vis strings found for inherent methods."""
    out = {}
    for m in RE_METHOD_DEF.finditer(text):
        name = m.group("name")
        vis = (m.group("vis") or "").strip()
        out.setdefault(name, set()).add(vis)
    return out


def top_level_pub(text):
    """Names of items declared `pub` (not pub(crate)/pub(super)) at any indent 0."""
    names = set()
    for m in RE_ITEM_DEF.finditer(text):
        vis = (m.group("vis") or "").strip()
        if vis == "pub":
            names.add(m.group("name"))
    return names


def main():
    if len(sys.argv) != 2:
        print("usage: check_split.py <module_dir>")
        return 2
    root = sys.argv[1]
    if not os.path.isdir(root):
        print("not a directory: %s" % root)
        return 2

    files = {}
    for name in os.listdir(root):
        if name.endswith(".rs"):
            files[name[:-3]] = os.path.join(root, name)

    problems = []

    # ---- E0364: pub use of non-pub items -------------------------------
    for mod, path in files.items():
        text = read(path)
        for m in RE_PUB_USE.finditer(text):
            child = m.group(1)
            if child not in files:
                continue
            items = [x.strip() for x in m.group(2).split(",") if x.strip()]
            child_pub = top_level_pub(read(files[child]))
            for item in items:
                item = item.split(" as ")[0].strip()
                if not item:
                    continue
                if item not in child_pub:
                    problems.append(
                        "E0364? %s.rs: `pub use %s::%s` but `%s` is not `pub` in %s.rs"
                        % (mod, child, item, item, child)
                    )
        for m in RE_PUB_USE_ONE.finditer(text):
            child, item = m.group(1), m.group(2)
            if child not in files:
                continue
            child_pub = top_level_pub(read(files[child]))
            if item not in child_pub:
                problems.append(
                    "E0364? %s.rs: `pub use %s::%s` but `%s` is not `pub` in %s.rs"
                    % (mod, child, item, item, child)
                )

    # ---- E0624: sibling call to private inherent method ----------------
    # Methods declared inside `impl Trait for Type` are trait items, NOT inherent
    # methods: trait impls with the same method name on different types are fine,
    # and visibility inside them is governed by the trait. So we attribute every
    # method declaration to its innermost impl and record the impl's "self type".
    def parse_impl_blocks(text):
        """Yield (self_type, {method_name: visibility}) for every impl block."""
        lines = text.split("\n")
        result = []
        stack = []  # list of (self_type, brace_depth_at_open)
        depth = 0
        current = None  # (self_type, depth_at_open, methods)
        impl_re = re.compile(r"^\s*impl(?:<[^>]*)?\s+(?:.*?\s+for\s+)?(?P<ty>\w[\w:<>]*)")
        for ln in lines:
            opens = ln.count("{")
            closes = ln.count("}")
            if current is None:
                m = impl_re.match(ln)
                if m and opens > 0:
                    current = (m.group("ty"), depth, {})
                    depth += opens
                    depth -= closes
                    continue
            else:
                for m in RE_METHOD_DEF.finditer(ln):
                    vis = (m.group("vis") or "").strip()
                    current[2].setdefault(m.group("name"), set()).add(vis)
                depth += opens
                depth -= closes
                if depth <= current[1]:
                    result.append(current)
                    current = None
                    continue
        if current is not None:
            result.append(current)
        return result

    impls_by_mod = {}
    inherent_private = set()  # (module, method_name) for non-trait impls
    trait_methods = set()
    for mod, path in files.items():
        if mod.startswith("tests"):
            continue
        blocks = parse_impl_blocks(strip_test_blocks(read(path)))
        for self_ty, _depth, methods in blocks:
            # `is_trait_impl` was broken: parse_impl_blocks only captures the type
            # AFTER "for", so the marker never appears in self_ty. Re-read the raw
            # impl headers for this module instead.
            is_trait = False
            raw = strip_test_blocks(read(path))
            for hm in re.finditer(r"^\s*impl[^{]*\bfor\b", raw, re.M):
                is_trait = True
                break
            for name, viss in methods.items():
                if is_trait:
                    trait_methods.add(name)
                elif "" in viss:
                    inherent_private.add((mod, name))

    for mod, path in files.items():
        if mod.startswith("tests"):
            continue
        body = strip_test_blocks(read(path))
        called = set(RE_SELF_CALL.findall(body))
        for name in called:
            if name in trait_methods:
                continue  # trait method: visibility governed by trait, skip
            if (mod, name) in inherent_private:
                continue  # defined in the same module: fine
            # defined privately in another module and called cross-module?
            for other_mod in impls_by_mod or []:
                pass
        # second pass: find private inherent methods in OTHER modules
        for name in called:
            for (om, oname) in inherent_private:
                if oname == name and om != mod:
                    problems.append(
                        "E0624? %s.rs calls `%s(...)` but it is a private inherent method in %s.rs"
                        % (mod, name, om)
                    )

    # ---- E0449: visibility on trait items ------------------------------
    for mod, path in files.items():
        text = read(path)
        for block in re.finditer(r"trait\s+[A-Za-z_][A-Za-z0-9_]*[^{]*\{(.*?)\n\}", text, re.S):
            inner = block.group(1)
            for m in re.finditer(r"^\s+(pub(?:\([a-z]+\))?)\s+(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)", inner, re.M):
                problems.append(
                    "E0449? %s.rs: trait item `%s` has visibility `%s`"
                    % (mod, m.group(2), m.group(1))
                )

    if problems:
        print("PROBLEMS FOUND (%d):" % len(problems))
        for p in sorted(set(problems)):
            print("  " + p)
        return 1

    print("OK: no E0364/E0624/E0449 candidates in %s" % root)
    return 0


if __name__ == "__main__":
    sys.exit(main())
