import os
import re

SRC = r"C:\Raid\Repos\Rust\refact-main\refact-agent\engine\src"
TESTS = re.compile(r"^[ \t]*#\[cfg\((?:test|any\(test)[^\n]*\)\]", re.M)

rows = []
for name in sorted(os.listdir(SRC)):
    p = os.path.join(SRC, name)
    if not os.path.isfile(p) or not name.endswith(".rs"):
        continue
    txt = open(p, "r", encoding="utf-8", errors="replace").read()
    m = TESTS.search(txt)
    prod = (txt[: m.start()].count("\n") + 1) if m else (txt.count("\n") + 1)
    tot = txt.count("\n") + 1
    rows.append((name, prod, tot - prod))

total_prod = sum(r[1] for r in rows)
total_test = sum(r[2] for r in rows)
print(f"{'file':<28}{'prod':>7}{'test':>8}")
print("-" * 44)
for n, p, t in sorted(rows, key=lambda r: -r[1]):
    if p < 20:
        continue
    print(f"{n:<28}{p:>7}{t:>8}")
print("-" * 44)
print(f"{'ROOT .rs TOTAL':<28}{total_prod:>7}{total_test:>8}")
print()
print(f"root files with >=20 prod lines: {sum(1 for r in rows if r[1] >= 20)}")
print("these are the files that must leave the crate to shrink refact-lsp further,")
print("since every module directory is now either a shim or already counted in the census.")
