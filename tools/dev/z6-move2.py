"""Z-6 second harvest: providers::pricing -> refact-runtime-api::providers_pricing_runtime.

Same strategy as the cmdline move: the census blocker (`providers::ModelPricing`) is
an artifact (it is `refact_providers::traits::ModelPricing`); the real edges are all
`ChatCtx`/`Arc<GlobalContext>` shaped, so the prod body belongs in runtime-api and
the engine file becomes a `pub use` shim carrying the original `#[cfg(test)]` block.
"""
import os
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
ENGINE = os.path.join(ROOT, "refact-agent", "engine")
SRC = os.path.join(ENGINE, "src", "providers", "pricing.rs")
CRATE = os.path.join(ENGINE, "crates", "refact-runtime-api", "src", "providers_pricing_runtime.rs")

APPLY = "--apply" in sys.argv
MARKER = "#[cfg(test)]\nmod tests {"

CRATE_PRELUDE_OLD = """use std::sync::Arc;

use crate::global_context::GlobalContext;
use refact_runtime_api::GlobalContextCtx;
use crate::providers::traits::ModelPricing;

pub use refact_pricing_core::{
    compute_cost, pricing_provider_aliases, standard_model_pricing_from_caps,
};
"""

CRATE_PRELUDE_NEW = """use std::sync::Arc;

use crate::chat_ctx::ChatCtx;
use crate::global_context::GlobalContext;
use crate::GlobalContextCtx;
use refact_providers::traits::ModelPricing;

pub use refact_pricing_core::{
    compute_cost, pricing_provider_aliases, standard_model_pricing_from_caps,
};
"""

SHIM = """pub use refact_runtime_api::providers_pricing_runtime::*;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use refact_providers::traits::ModelPricing;

"""


def subn(text, old, new, n, label):
    got = text.count(old)
    if got != n:
        raise SystemExit(f"[{label}] EXPECTED {n}, FOUND {got}:\n{old[:200]}")
    return text.replace(old, new)


def main():
    text = open(SRC, encoding="utf-8", newline="").read().replace("\r\n", "\n")
    i = text.index(MARKER)
    prod, tests = text[:i], text[i:]
    body = subn(prod, CRATE_PRELUDE_OLD, CRATE_PRELUDE_NEW, 1, "prelude")
    body = subn(body, "crate::chat::ctx::ChatCtx", "ChatCtx", 2, "chat_ctx")
    shim = SHIM + tests
    print(f"prod {len(prod.splitlines())}L -> crates/refact-runtime-api/src/"
          f"providers_pricing_runtime.rs ({len(body.splitlines())}L); tests "
          f"{len(tests.splitlines())}L stay in the engine shim")
    for ln in body.splitlines():
        if "providers::" in ln and "refact_providers" not in ln:
            print("   CHECK:", ln.strip())
    if APPLY:
        with open(CRATE, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(body)
        with open(SRC, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(shim)
        print("APPLIED")


main()
