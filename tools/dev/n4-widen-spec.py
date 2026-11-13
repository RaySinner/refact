"""N-4 widen planner: exactly which prod items/fields the staying tests touch."""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
SRC = ROOT / "refact-agent" / "engine" / "src" / "tools"

MODS = [
    "tool_task_documents", "tool_task_memory",
    "subagent_phases", "tool_add_workspace_folder", "tool_create_knowledge", "tool_knowledge",
    "tool_activate_skill", "tool_config_subagent",
    "tool_cron_list", "tool_cron_delete",
]

ITEM_RE = re.compile(r"^((?:pub(?:\([a-z_]*\))?\s+)?)(?:async\s+)?(fn|struct|enum|trait|const|static|type)\s+(\w+)")
DECL_RE = re.compile(
    r"^((?:pub(?:\([a-z_]*\))?\s+)?)(?:(struct|enum)\s+(\w+)|(?:async\s+)?(fn|const|static|type)\s+(\w+))")


def test_start(lines):
    for i in range(len(lines) - 1):
        if lines[i].strip() == "#[cfg(test)]" and re.match(r"^(pub )?mod tests\b", lines[i + 1].strip()):
            return i
    return len(lines)


def strip_visibility(line):
    return re.sub(r"^\s*pub(?:\([a-z_]*\))?\s+", "", line)


for mod in MODS:
    text = (SRC / f"{mod}.rs").read_text(encoding="utf-8").replace("\r\n", "\n")
    lines = text.split("\n")
    start = test_start(lines)
    prod, test = lines[:start], lines[start:]
    test_t = "\n".join(test)
    if not test:
        print(f"########## {mod}: NO TESTS")
        continue
    print(f"########## {mod} (test region {len(test)} lines)")
    # 1. top-level prod items referenced by name in the tests
    for i, line in enumerate(prod):
        m = ITEM_RE.match(line)
        if not m:
            continue
        vis_kw = m.group(1).strip()
        kind, name = m.group(2), m.group(3)
        if not re.search(r"(?<![\w])%s(?![\w])" % re.escape(name), test_t):
            continue
        need = "WIDEN->pub" if vis_kw != "pub" else "ok(pub)"
        print(f"   ITEM   {need:12s} {kind:7s} {name}  L{i+1}  (was {vis_kw or 'priv'})")
    # 2. struct literals built in tests, and their field reads
    for m in re.finditer(r"\b([A-Z]\w+)\s*\{", test_t):
        pass
    lits = sorted(set(re.findall(r"\b([A-Z]\w+)\s*\{", test_t)))
    for st in lits:
        # find decl in prod
        decl = None
        for i, line in enumerate(prod):
            dm = re.match(r"^\s*((?:pub(?:\([a-z_]*\))?\s+)?)struct\s+%s\b" % re.escape(st), line)
            if dm:
                decl = (i, dm.group(1).strip() or "priv")
                break
        if decl is None:
            continue
        i0, vis = decl
        fields = []
        j = i0 + 1
        while j < len(prod) and "{" not in prod[j] and ";" not in prod[j]:
            j += 1
        j += 1
        while j < len(prod):
            s = prod[j].strip()
            if s.startswith("}"):
                break
            fm = re.match(r"^((?:pub(?:\([a-z_]*\))?\s+)?)([A-Za-z_]\w*)\s*:", s)
            if fm:
                fields.append(((fm.group(1) or "").strip() or "priv", fm.group(2)))
            j += 1
        print(f"   STRUCT {st}  L{i0+1} vis={vis}")
        for fv, fn in fields:
            touched = re.search(r"\.%s\b" % re.escape(fn), test_t) is not None
            if fv != "pub" or touched:
                print(f"        field {(fn):28s} {fv:11s} {'-> PUB (touched)' if touched and fv != 'pub' else ''}")
    # 3. fn-local type annotations in tests referencing prod structs
    print("   test `let ... : Type` uses:", sorted(set(re.findall(r":\s*([A-Z]\w+)", test_t)))[:20])
