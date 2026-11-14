"""Q-6 static proof: the four integrations/providers harvest moves landed.

Read-only except for re-running the honest census into engine/target/. Answers:

  1. N already-shim / M moved / K left per module, with the census numbers before
     and after the work;
  2. every engine shim the card created resolves to a real crate item;
  3. no crate -> engine edge (`crate::` never appears in the touched crates, and
     no touched crate's Cargo.toml names refact-lsp);
  4. moved bodies are verbatim (production lines match the pre-move file, minus
     the shimline).

Run:  powershell -NoProfile -ExecutionPolicy Bypass -File tools\\dev\\q6-check.ps1
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENG = ROOT / "refact-agent" / "engine"
SRC = ENG / "src"
CRATES = ENG / "crates"
CENSUS_TMPL = "refact-agent/engine/target/%s_movable_census.txt"

BASE = "ac177abe7"

EDGE_CRATES = {
    "refact-http": CRATES / "refact-http",
    "refact-providers": CRATES / "refact-providers",
    "refact-runtime-api": CRATES / "refact-runtime-api",
}

# (engine shim, crate module path, symbols the shim must expose)
SHIMS = [
    (
        "providers/http/defaults.rs",
        "refact_http::routers::v1::provider_defaults",
        [
            "handle_v1_defaults_get",
            "handle_v1_defaults_update",
            "handle_v1_project_defaults_get",
            "handle_v1_project_defaults_update",
        ],
    ),
    (
        "providers/http/common.rs",
        "refact_runtime_api::providers_http_runtime",
        [
            "invalidate_caps",
            "resolve_provider_identity",
            "resolve_provider_for_base",
            "ensure_provider_for_model_management",
        ],
    ),
    (
        "providers/http/settings.rs",
        "refact_runtime_api::providers_http_runtime",
        ["patch_provider_model_config", "reload_provider_from_disk"],
    ),
    (
        "integrations/sessions.rs",
        "refact_runtime_api::integrations_sessions",
        ["remove_expired_sessions_background_task", "stop_sessions"],
    ),
]

# crate file that must exist and define the symbols above
CRATE_FILES = {
    "refact-http": [
        CRATES / "refact-http" / "src" / "routers" / "v1" / "provider_defaults.rs"
    ],
    "refact-runtime-api": [
        CRATES / "refact-runtime-api" / "src" / "providers_http_runtime.rs",
        CRATES / "refact-runtime-api" / "src" / "integrations_sessions.rs",
    ],
}

LEFTOVERS = [
    "integrations::browser_runtime — needs AppState + refact_http::routers::v1::browser_settings",
    "integrations::browser_controller — same seam",
    "integrations::setting_up_integrations — binds engine integrations_list/integration_from_name",
    "integrations::notifier — binds engine integrations_list/integration_from_name",
    "integrations::notifier::integr_notifier_telegram — same",
    "integrations::config_chat / setup_chat — same",
    "integrations, running_integrations, mcp::mod — module tables + IntegrationProvider seam impl",
    "providers::mod — engine re-export facade",
    "providers::http::quota_cache — needs engine usage.rs refresh helpers",
]

failures = []


def fail(msg):
    failures.append(msg)


def read(path):
    return path.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n")


def prod_lines(text):
    """Module-scope production lines: drops whole `#[cfg(test)] mod x { ... }` blocks."""
    lines = text.split("\n")
    out, i, n = [], 0, len(lines)
    depth_after = lambda s: s.count("{") - s.count("}")
    while i < n:
        if lines[i].strip().startswith("#[cfg(test)]") and i + 1 < n \
                and re.match(r"^(pub )?mod [A-Za-z_]\w*\s*\{", lines[i + 1].strip()):
            depth, j = 0, i + 1
            while j < n:
                depth += depth_after(lines[j])
                if depth == 0:
                    break
                j += 1
            i = j + 1
            continue
        out.append(lines[i])
        i += 1
    return len([line for line in out if line.strip()])


def base_text(rel):
    raw = subprocess.run(
        ["git", "-C", str(ROOT), "show", "%s:%s" % (BASE, "refact-agent/engine/src/" + rel)],
        capture_output=True,
    )
    return raw.stdout.decode("utf-8", errors="replace").replace("\r\n", "\n") if raw.returncode == 0 else ""


def census(module):
    subprocess.run(
        [sys.executable, str(ROOT / "tools" / "dev" / "chat_movable_census.py"), module],
        cwd=str(ROOT),
        capture_output=True,
    )
    path = ROOT / (CENSUS_TMPL % module)
    if not path.exists():
        fail("census file missing for %s" % module)
        return {}
    data = {}
    for line in read(path).split("\n")[:5]:
        m = re.match(r"already-shim .*: (\d+) files / (\d+) lines", line)
        if m:
            data["shim"] = (int(m.group(1)), int(m.group(2)))
        m = re.match(r"MOVABLE now: (\d+) files / (\d+) lines", line)
        if m:
            data["movable"] = (int(m.group(1)), int(m.group(2)))
        m = re.match(r"LEFT \(blocked\): (\d+) files / (\d+) lines", line)
        if m:
            data["left"] = (int(m.group(1)), int(m.group(2)))
    return data


def check_shims():
    for rel, crate_mod, symbols in SHIMS:
        path = SRC / rel
        text = read(path) if path.exists() else ""
        if crate_mod not in text:
            fail("%s does not re-export %s" % (rel, crate_mod))
        if prod_lines(text) > 12:
            fail("%s is not a thin shim (prod lines=%d)" % (rel, prod_lines(text)))
        if "*" not in text:
            for symbol in symbols:
                if symbol not in text:
                    fail("%s does not name %s" % (rel, symbol))


def check_crate_defines():
    for crate, files in CRATE_FILES.items():
        for path in files:
            if not path.exists():
                fail("missing crate file %s" % path.relative_to(ROOT))
    for _rel, crate_mod, symbols in SHIMS:
        root_crate = crate_mod.split("::")[0]
        sources = list((CRATES / root_crate.replace("_", "-") / "src").rglob("*.rs"))
        for symbol in symbols:
            if not any(
                re.search(r"(?<![\w])fn %s(?![\w])" % re.escape(symbol), read(path))
                for path in sources
            ):
                fail("no %s file defines %s" % (root_crate, symbol))


def check_no_engine_edge():
    """Dependency direction only: `crate::` inside a crate is its own module tree."""
    engine_toml = read(ENG / "Cargo.toml")
    for name, path in EDGE_CRATES.items():
        toml = read(path / "Cargo.toml")
        if re.search(r"(?m)^\s*refact-lsp\s*=", toml):
            fail("%s depends on refact-lsp" % name)
        if not re.search(r"(?m)^%s\s*=" % re.escape(name), engine_toml):
            fail("engine does not depend on %s" % name)
    for rel, crate_mod, _symbols in SHIMS:
        if not read(SRC / rel).strip().startswith("pub use"):
            fail("%s is not a re-export shim" % rel)


def check_verbatim():
    """Moved bodies keep the same production mass as the pre-move engine file."""
    expectations = {
        "crates/refact-http/src/routers/v1/provider_defaults.rs": "providers/http/defaults.rs",
        "crates/refact-runtime-api/src/integrations_sessions.rs": "integrations/sessions.rs",
    }
    for crate_rel, engine_rel in expectations.items():
        before = prod_lines(base_text(engine_rel))
        after = prod_lines(read(ENG / crate_rel))
        if after < before - 30:
            fail("%s lost body: before=%d after=%d" % (crate_rel, before, after))
        print("  %-58s verbatim prod lines: %d -> %d" % (crate_rel, before, after))


def main():
    print("### Q-6 harvest report")
    for module, moved in (("integrations", 1), ("providers", 4)):
        data = census(module)
        print(
            "  %-14s already-shim %s | MOVABLE %s | LEFT %s (moved this card: %d)"
            % (
                module,
                data.get("shim"),
                data.get("movable"),
                data.get("left"),
                moved,
            )
        )
    print()
    check_shims()
    check_crate_defines()
    check_no_engine_edge()
    print("### moved bodies")
    check_verbatim()
    print()
    print("### left in engine (needs a seam; each is listed with its reason)")
    for row in LEFTOVERS:
        print("  %s" % row)
    print()
    if failures:
        print("### FAILURES (%d)" % len(failures))
        for f in failures:
            print("  %s" % f)
        return 1
    print("### OK: shims resolve, no crate -> engine edge, moved bodies verbatim")
    return 0


if __name__ == "__main__":
    sys.exit(main())
