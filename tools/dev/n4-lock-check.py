"""N-4 Cargo.lock check + repair helper.

Verifies the lock is still CRLF, that `refact-tools` resolves its new deps
(`refact-ext`, `chrono-tz`), and repairs the dependency list in place if needed
(preserving CRLF, so the diff stays 3 lines instead of ~8,400).

Usage:
    python tools/dev/n4-lock-check.py           # check only
    python tools/dev/n4-lock-check.py --fix     # add missing deps to the block
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
LOCK = ROOT / "refact-agent" / "engine" / "Cargo.lock"
WANT = ["chrono-tz", "refact-ext"]


def read_text(path):
    return path.read_bytes().decode("utf-8")


def package_block(text, name):
    m = re.search(r"(?ms)^\[\[package\]\]\n?name = \"%s\"\n.*?(?=\[\[package\]\]|\Z)"
                  % re.escape(name), text)
    return m


def main():
    raw = LOCK.read_bytes()
    crlf, lf = raw.count(b"\r\n"), raw.count(b"\n")
    text = read_text(LOCK).replace("\r\n", "\n")
    ok = True

    print("CRLF=%d LF=%d %s" % (crlf, lf, "OK (pure CRLF)" if crlf == lf else "MIXED!"))
    ok = ok and crlf == lf

    for name in ["refact-ext", "chrono-tz"]:
        present = re.search(r'(?m)^name = "%s"$' % re.escape(name), text) is not None
        print("package %-12s %s" % (name, "present" if present else "MISSING"))
        ok = ok and present

    m = package_block(text, "refact-tools")
    if not m:
        print("refact-tools block MISSING")
        return 1
    start, end = m.span()
    block = text[start:end]
    deps = re.findall(r'(?m)^ "([^"]+)",$', block)
    print("refact-tools deps: %d" % len(deps))

    missing = [w for w in WANT if w not in deps]
    if missing and "--fix" in sys.argv:
        lines = block.split("\n")
        inserted = 0
        for want in missing:
            for i, line in enumerate(lines):
                if line.startswith(" "):
                    dep = line.strip().strip('"').split(" ")[0]
                    if dep > want:
                        lines.insert(i, ' "%s",' % want)
                        inserted += 1
                        break
            else:
                close = next(i for i, l in enumerate(lines) if l.rstrip() == "]")
                lines.insert(close, ' "%s",' % want)
                inserted += 1
        block = "\n".join(lines)
        text = text[:start] + block + text[end:]
        LOCK.write_bytes(text.replace("\n", "\r\n").encode("utf-8"))
        print("inserted %d deps -> re-run to verify" % inserted)
        return 0

    for want in WANT:
        print("  %s in refact-tools deps: %s" % (want, want in deps))
    print("  refact-lsp in refact-tools deps: %s" % ("refact-lsp" in deps))
    ok = ok and not missing and "refact-lsp" not in deps

    print("\nRESULT: %s" % ("ALL CHECKS PASSED" if ok else "FAILED"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
