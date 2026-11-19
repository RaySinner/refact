"""Honest prod/test split per file: an item prefixed by #[cfg(test)] is test.

Handles: `use ...;` (single line, possibly multi-line braces), `fn ...;`,
`fn ... { }`, `mod x;`, `mod x { }`, `impl ... { }`, `struct ...;`, etc.
"""
import os
import re
import sys

ROOT = sys.argv[1] if len(sys.argv) > 1 else r'C:\Raid\Repos\Rust\refact-main\refact-agent\engine\src\buddy'
ATTR = re.compile(r'^\s*#\[cfg\(test\)\]')


def split(path):
    lines = open(path, encoding='utf-8', errors='ignore').read().splitlines()
    prod = 0
    test = 0
    pending = False          # a #[cfg(test)] was seen, item has not started
    in_item = False          # inside a test-gated item
    depth = 0
    item_depth = 0
    for raw in lines:
        s = raw.strip()
        if not in_item and ATTR.match(raw):
            pending = True
            continue
        if pending:
            if s == '' or s.startswith('//'):
                continue
            # the item starts here
            in_item = True
            pending = False
            item_depth = depth
        if in_item:
            test += 1
            depth += raw.count('{') - raw.count('}')
            # item ends when braces close back to item_depth AND the line ends the item
            if depth <= item_depth and (s.endswith(';') or s.endswith('}')):
                in_item = False
            continue
        if s and not s.startswith('//'):
            prod += 1
        depth += raw.count('{') - raw.count('}')
    return prod, test


tot_p = tot_t = 0
rows = []
for dp, _, fs in os.walk(ROOT):
    rel = os.path.relpath(dp, ROOT)
    for f in sorted(fs):
        if not f.endswith('.rs'):
            continue
        p = os.path.join(dp, f)
        pr, te = split(p)
        rows.append((pr, te, os.path.join(rel, f)))
        tot_p += pr
        tot_t += te

rows.sort(reverse=True)
shim_p = 0
code_p = 0
for pr, te, name in rows:
    body = open(os.path.join(ROOT, name), encoding='utf-8', errors='ignore').read().splitlines()
    code = [l.strip() for l in body
            if l.strip() and not l.strip().startswith('//') and not l.strip().startswith('#[')]
    is_shim = bool(code) and all(
        l.startswith('pub use ') or l.startswith('pub(crate) use ')
        or l.startswith('mod ') or l.startswith('pub mod ')
        for l in code)
    if pr:
        if is_shim:
            shim_p += pr
        else:
            code_p += pr
    print('%6d prod %6d test  %s  %s' % (pr, te, 'SHIM' if is_shim else 'CODE', name))

print('---')
print('TOTAL prod:', tot_p, ' test:', tot_t)
print('prod in pure shims:', shim_p, ' prod in code files:', code_p)
