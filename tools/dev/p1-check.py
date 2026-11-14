"""P-1 static proof: the movable `tools/` body landed in refact-tools.

Read-only, no compilation (the sanctioned compile gate is a separate verification
card). It answers the four acceptance criteria deterministically:

  1. N already-shim / M moved / K left + reasons, with per-file prod-line counts;
  2. `refact-tools/Cargo.toml` never mentions `refact-lsp`, and no crate file
     references an engine module;
  3. a 10-path sample of the `tools_list.rs` constructor surface still resolves;
  4. no moved file keeps a repo-relative `include_str!`/`include_bytes!`.

Run:  powershell -NoProfile -ExecutionPolicy Bypass -File tools\\dev\\p1-check.ps1
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src" / "tools"
DST = ENG / "crates" / "refact-tools" / "src" / "tools"
CRATE_TOML = ENG / "crates" / "refact-tools" / "Cargo.toml"
CENSUS = ENG / "target" / "tools_movable_census.txt"

# Last commit before this card: distinguishes "was production, now a shim" (moved
# by this card) from "was already a shim" (moved by an earlier card).
BASE = "68efcde49"

# The deliberate engine-side shell island: these never move.
ISLAND = {
    "tool_process", "tool_shell", "shell_gate", "shell_gate_llm",
    "tool_process_write_stdin", "tools_list",
}

SHIM_RE = re.compile(r"(?m)^pub use refact_tools::tools::([\w:]+)::\*;\s*$")

failures = []


def fail(msg):
    failures.append(msg)


def read(path):
    return path.read_text(encoding="utf-8", errors="replace")


def base_text(rel):
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:refact-agent/engine/src/tools/%s" % (BASE, rel)],
        capture_output=True,
    )
    if raw.returncode != 0:
        return ""
    return raw.stdout.decode("utf-8", errors="replace").replace("\r\n", "\n")


def was_shim_at_base(rel):
    return SHIM_RE.search(base_text(rel)) is not None


def mask_text(text):
    """Blank every literal so brace counting sees structure only.

    Multi-line raw strings (`r#"{"a": 1}"#`) are everywhere in these tools; a
    line-local scanner cannot see the closing quote on a later line and would
    count the fixture's braces as Rust structure. Newlines are preserved.
    """
    out, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        if c == "r" and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            m = re.compile(r'r(#*)"').match(text, i)
            if m:
                end = text.find('"' + m.group(1), m.end())
                end = n if end == -1 else end + len(m.group(1)) + 1
                out.append(re.sub(r"[^\n]", " ", text[i:end]))
                i = end
                continue
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end == -1 else end
            out.append(re.sub(r"[^\n]", " ", text[i:end]))
            i = end
            continue
        if text.startswith("/*", i):
            end = text.find("*/", i)
            end = n if end == -1 else end + 2
            out.append(re.sub(r"[^\n]", " ", text[i:end]))
            i = end
            continue
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j = min(j + 1, n)
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
            continue
        if c == "'":
            m = re.compile(r"'(?:\\.|[^'\\])'").match(text, i)
            if m:
                out.append(" " * len(m.group(0)))
                i = m.end()
                continue
        out.append(c)
        i += 1
    return "".join(out)


def split_prod_test(lines):
    """(production lines, test lines): only brace-matched `#[cfg(test)] mod x {}`
    blocks count as test code, because production code also sits below them."""
    masked = mask_text("\n".join(lines)).split("\n")
    prod, test, i = [], [], 0
    while i < len(lines):
        if lines[i].strip().startswith("#[cfg(test)]") and i + 1 < len(lines) \
                and re.match(r"^(pub )?mod [A-Za-z_]\w*\s*\{", lines[i + 1].strip()):
            depth, j = 0, i + 1
            while j < len(lines):
                depth += masked[j].count("{") - masked[j].count("}")
                if depth == 0:
                    break
                j += 1
            test.extend(lines[i: j + 1])
            i = j + 1
            continue
        prod.append(lines[i])
        i += 1
    return prod, test


def prod_lines(path):
    prod, test = split_prod_test(read(path).split("\n"))
    return (
        len([l for l in prod if l.strip()]),
        len([l for l in test if l.strip()]),
    )


def census_movable():
    if not CENSUS.exists():
        fail("census file missing: %s (run tools/dev/chat_movable_census.py tools)" % CENSUS)
        return []
    mods, inside = [], False
    for line in read(CENSUS).split("\n"):
        if line.startswith("### MOVABLE"):
            inside = True
            continue
        if inside and line.startswith("###"):
            break
        if inside and line.strip():
            mods.append(line.split()[1])
    return mods


def engine_file_for(mod):
    parts = mod.split("::")[1:]
    if not parts:
        return SRC / "mod.rs"
    cand = SRC.joinpath(*parts[:-1]) / (parts[-1] + ".rs")
    if cand.exists():
        return cand
    cand = SRC.joinpath(*parts) / "mod.rs"
    return cand if cand.exists() else None


def crate_file_for(mod):
    parts = mod.split("::")[1:]
    if not parts:
        return DST.parent / "tools.rs"
    cand = DST.joinpath(*parts[:-1]) / (parts[-1] + ".rs")
    if cand.exists():
        return cand
    cand = DST.joinpath(*parts) / "mod.rs"
    return cand if cand.exists() else None


def find_crate_target(parts):
    for k in range(len(parts), 0, -1):
        cand = DST.joinpath(*parts[:k]).with_suffix(".rs")
        if cand.exists():
            return cand
        cand = DST.joinpath(*parts[:k]) / "mod.rs"
        if cand.exists():
            return cand
    return None


def engine_module_for(parts):
    """Engine module file a `tools::<...>` path names, longest existing prefix."""
    for k in range(len(parts), 0, -1):
        cand = SRC.joinpath(*parts[:k]).with_suffix(".rs")
        if cand.exists():
            return cand
        cand = SRC.joinpath(*parts[:k]) / "mod.rs"
        if cand.exists():
            return cand
    return None


SYMBOL_RE = re.compile(r"^[A-Z]\w*$")


def engine_module_symbol(parts):
    """Index of the first type-like segment after a module prefix, or None."""
    for i, part in enumerate(parts):
        if SYMBOL_RE.match(part):
            return part
    return None


def dir_totals(engine_mod_rs):
    """(children in the crate, total children, root production lines).

    The root's own `mod.rs` body is counted here; the children are either listed
    as individual movers (they kept an engine shim) or as directory children
    (their engine file was removed with the root), so counting them here too
    would double-count.
    """
    engine_dir = engine_mod_rs.parent
    crate_dir = DST / engine_dir.relative_to(SRC)
    names = {c.name for c in engine_dir.glob("*.rs")}
    names |= {c.name for c in crate_dir.glob("*.rs")}
    names.discard("mod.rs")
    in_crate = sum(1 for n in names if (crate_dir / n).exists())
    root = crate_dir / "mod.rs"
    prod = prod_lines(root)[0] if root.exists() else 0
    return (in_crate, len(names), prod)


def check_crate_has_no_engine_edge():
    if re.search(r"(?m)^\s*refact-lsp\s*=", read(CRATE_TOML)):
        fail("refact-tools/Cargo.toml depends on refact-lsp")
    for path in DST.rglob("*.rs"):
        for ref in set(re.findall(r"crate::[\w:]+", read(path))):
            if not ref.startswith("crate::tools::"):
                fail("%s references engine module %s" % (path.relative_to(ROOT), ref))


def check_includes_are_manifest_relative():
    for path in DST.rglob("*.rs"):
        for i, line in enumerate(read(path).split("\n"), 1):
            if "include_str!" in line or "include_bytes!" in line:
                if 'env!("CARGO_MANIFEST_DIR")' not in line:
                    fail("%s:%d keeps a repo-relative include: %s"
                         % (path.relative_to(ROOT), i, line.strip()[:90]))


def check_shims_resolve():
    for path in sorted(SRC.rglob("*.rs")):
        m = SHIM_RE.search(read(path))
        if not m:
            continue
        target = find_crate_target(m.group(1).split("::"))
        if target is None:
            fail("engine shim %s re-exports missing crate module %s"
                 % (path.relative_to(SRC).as_posix(), m.group(1)))


def check_constructor_surface(sample=10):
    """tools_list.rs is the constructor surface: every `crate::tools::<m>::<T>` it
    names must still resolve through the shim chain.

    Returns (sampled paths that cross into the crate, engine-side paths)."""
    text = read(SRC / "tools_list.rs")
    paths = sorted(set(re.findall(r"crate::tools::[\w:]+", text)))
    checked = []
    engine_side = []
    for path in paths:
        parts = path.split("::")[2:]
        if len(parts) < 2:
            continue
        engine = engine_module_for(parts)
        if engine is None:
            fail("constructor path %s has no engine module" % path)
            continue
        symbol = engine_module_symbol(parts)
        if symbol is None:
            continue
        if not SHIM_RE.search(read(engine)):
            engine_side.append(path)
            continue
        symbol_parts = parts[: parts.index(symbol) + 1]
        target = find_crate_target(symbol_parts)
        if target is None:
            fail("constructor path %s has no crate counterpart" % path)
            continue
        # A directory root re-exports its children (`pub use self::<child>::*`), so a
        # symbol may live in any file of that directory.
        define_re = re.compile(r"(?<![\w])%s(?![\w])" % re.escape(symbol))
        candidates = [target]
        if target.name == "mod.rs":
            candidates += sorted(target.parent.glob("*.rs"))
        if not any(define_re.search(read(c)) for c in candidates):
            fail("crate %s does not define %s" % (target.relative_to(DST), symbol))
        checked.append((path, target.relative_to(ROOT)))
    return checked[:sample], engine_side


def main():
    mods = census_movable()
    moved, dir_roots, nested, already_shim, left, gone = [], [], [], [], [], []
    for mod in mods:
        engine = engine_file_for(mod)
        if engine is None:
            crate = crate_file_for(mod)
            if crate is not None:
                nested.append((mod, crate))
            else:
                gone.append((mod, "no engine file and no crate file"))
            continue
        rel = engine.relative_to(SRC).as_posix()
        if SHIM_RE.search(read(engine)):
            if was_shim_at_base(rel):
                already_shim.append((mod, engine))
                continue
            if engine.name == "mod.rs":
                dir_roots.append((mod, engine))
                continue
            crate = crate_file_for(mod)
            if crate is None:
                left.append((mod, engine, "engine shim without crate module"))
                continue
            moved.append((mod, engine, crate))
            continue
        if mod.split("::")[-1] in ISLAND:
            left.append((mod, engine, "engine shell island (deliberate)"))
            continue
        if engine.name == "mod.rs":
            left.append((mod, engine, "engine module table (directory root)"))
            continue
        left.append((mod, engine, "engine-only dependency"))

    moved_prod = sum(prod_lines(c)[0] for _m, _e, c in moved)
    nested_prod = sum(prod_lines(c)[0] for _m, c in nested)
    roots_prod = sum(dir_totals(e)[2] for _m, e in dir_roots)
    left_prod = sum(prod_lines(e)[0] for _m, e, _r in left)

    print("### refact-tools harvest tally (census MOVABLE list, %d modules)" % len(mods))
    print("moved this card: %d files + %d directory roots + %d directory children"
          % (len(moved), len(dir_roots), len(nested)))
    print("already-shim (earlier cards): %d | left in engine: %d | unaccounted: %d"
          % (len(already_shim), len(left), len(gone)))
    print("production lines: moved %d | left %d | total %d"
          % (moved_prod + nested_prod + roots_prod, left_prod,
             moved_prod + nested_prod + roots_prod + left_prod))
    print()
    print("### moved now (engine file -> crate file, prod lines)")
    for mod, engine, crate in sorted(moved):
        prod, _ = prod_lines(crate)
        _shim_prod, shim_test = prod_lines(engine)
        print("  %-44s -> %-40s prod=%4d engine-test-lines=%4d"
              % (mod, crate.relative_to(DST).as_posix(), prod, shim_test))
    print()
    print("### directory roots moved under a shimmed mod.rs")
    for mod, engine in sorted(dir_roots):
        in_crate, total, prod = dir_totals(engine)
        print("  %-44s %d/%d children in the crate, prod=%d"
              % (mod, in_crate, total, prod))
    print()
    print("### moved as a directory child (engine file removed with its root)")
    for mod, crate in sorted(nested):
        prod, _ = prod_lines(crate)
        print("  %-44s -> %-40s prod=%4d"
              % (mod, crate.relative_to(DST).as_posix(), prod))
    print()
    print("### left in engine")
    for mod, engine, reason in sorted(left):
        prod, test = prod_lines(engine)
        print("  %-44s prod=%4d test=%4d  %s" % (mod, prod, test, reason))
    print()
    print("### already-shim (moved by an earlier card, nothing left to move)")
    for mod, engine in sorted(already_shim):
        print("  %-44s %s" % (mod, engine.relative_to(SRC).as_posix()))
    print()
    print("### unaccounted census entries")
    for mod, reason in sorted(gone):
        print("  %-44s %s" % (mod, reason))

    check_crate_has_no_engine_edge()
    check_includes_are_manifest_relative()
    check_shims_resolve()
    checked, engine_side = check_constructor_surface()

    print()
    print("### constructor-surface check (tools_list.rs names %d paths: %d resolve into the"
          " crate, %d are engine-side)"
          % (len(checked) + len(engine_side), len(checked), len(engine_side)))
    for path, crate in checked:
        print("  %-56s -> %s" % (path, crate.as_posix()))
    print("  engine-side (island / not-yet-moved modules):")
    for path in engine_side[:12]:
        print("    %s" % path)

    print()
    print("### crate dependency guard")
    print("  Cargo.toml mentions refact-lsp: %s"
          % bool(re.search(r"(?m)^\s*refact-lsp\s*=", read(CRATE_TOML))))
    print("  crate files referencing an engine module: %d"
          % sum(1 for p in DST.rglob("*.rs")
                for r in set(re.findall(r"crate::[\w:]+", read(p)))
                if not r.startswith("crate::tools::")))

    print()
    if failures:
        print("### FAILURES (%d)" % len(failures))
        for f in failures:
            print("  %s" % f)
        return 1
    print("### OK: no crate -> engine edge, every engine shim resolves,"
          " constructor-surface sample resolves, includes are manifest-relative")
    return 0


if __name__ == "__main__":
    sys.exit(main())
