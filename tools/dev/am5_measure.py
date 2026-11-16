#!/usr/bin/env python3
"""AM-5 measure kickstart helper: prod-line counts + pub symbols for one file.

Usage:  python <engine>/target/am5_measure.py <file.rs>
Prints: <file>  prod=<n>  total=<m>
then one line per top-level pub item: "    <lineno>  <sig>"
"""

import io
import re
import sys

SRC_START = re.compile(r'^\s*#\[cfg\(test\)\]\s*$')


def strip_comments_srcdoc(lines):
    """Drop // line comments and doc comments (/// //!) for prod counting."""
    out = []
    in_block = False
    for line in lines:
        s = line.strip()
        if in_block:
            if '*/' in s:
                in_block = False
            continue
        if s.startswith('/*'):
            if '*/' not in s:
                in_block = True
            continue
        if s.startswith('//'):
            continue
        out.append(line)
    return out


def prod_lines(path):
    with io.open(path, 'r', encoding='utf-8', errors='surrogateescape') as fh:
        lines = fh.read().splitlines()
    for i, line in enumerate(lines):
        if SRC_START.match(line):
            stripped = strip_comments_srcdoc(lines[:i])
            stripped = [l for l in stripped if l.strip()]
            return i, len(stripped), len(lines)
    stripped = strip_comments_srcdoc(lines)
    stripped = [l for l in stripped if l.strip()]
    return -1, len(stripped), len(lines)


PUB_RE = re.compile(r'^\s*(pub(?:\(crate\))?)\s+(?:(async\s+)?)?(fn|struct|enum|trait|type|use|mod|const|static|macro_rules!)\s+([A-Za-z0-9_]+)')


def main():
    path = sys.argv[1]
    ts, prod, total = prod_lines(path)
    print('%s  prod=%d  total=%d  test_start=%d' % (path.replace('\\', '/'), prod, total, ts))
    with io.open(path, 'r', encoding='utf-8', errors='surrogateescape') as fh:
        lines = fh.read().splitlines()
    # walk items skipping the test module
    depth = 0
    in_test = False
    for i, line in enumerate(lines):
        if ts >= 0 and i >= ts:
            break
        if depth == 0 and PUB_RE.match(line):
            print('    %4d  %s' % (i + 1, line.strip()[:110]))
        depth += line.count('{') - line.count('}')
        depth += line.count('(') - line.count(')')
    return 0


if __name__ == '__main__':
    sys.exit(main())
