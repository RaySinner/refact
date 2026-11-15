"""Synthetic module tree used by tools/dev/aa6-check.py.

Each of the four census artifacts AA-6 fixed gets one discriminating probe here,
i.e. the synthetic module is classified differently before and after the fix:

* `fx::mod` reaches a sibling through a **bare path** (`bare::child::…`) — the
  shape used by the real `integrations/mod.rs`. Before: invisible, so `fx` looked
  movable. After: `fx` is blocked.
* `fx::glob::leaf` writes `use super::*;` and calls a helper that only the
  parent's `pub(crate) use helper::*;` supplies; that helper depends on
  `outside::alpha`. Before: no edge, `leaf` looked movable. After: blocked.
* `fx::alias::user` reaches a crate type through a **module-level crate alias**
  (`pub use refact_providers::things;` in `fx::alias`). Before: the alias was
  read as the engine symbol `fx::alias::Thing`, so `user` looked blocked for the
  wrong reason. After: a crate edge, `user` is movable.
* `fx::decl::mod` is **declaration-only** (`pub mod facade;`). Before: `pub mod`
  counted as a definition, so it was listed as movable payload. After: reported
  as a shim.

`outside::alpha` is referenced by the probes and deliberately lives outside `fx`,
so the census sees a genuine engine-resident dependency.
"""
import os

FILES = {
    "lib.rs": "pub mod fx;\npub mod outside;\n",
    "outside/mod.rs": "pub mod alpha;\n",
    "outside/alpha.rs": "pub fn alpha_helper() -> u32 { 1 }\n",
    "fx/mod.rs": (
        "pub mod bare;\npub mod glob;\npub mod alias;\npub mod decl;\n\n"
        "pub fn dispatch() -> u32 {\n    bare::child::child_helper()\n}\n"
    ),
    "fx/bare/mod.rs": "pub mod child;\n",
    "fx/bare/child.rs": (
        "pub fn child_helper() -> u32 {\n    crate::outside::alpha::alpha_helper()\n}\n"
    ),
    "fx/glob/mod.rs": "pub mod helper;\npub mod leaf;\npub(crate) use helper::*;\n",
    "fx/glob/helper.rs": (
        "pub(crate) fn sibling_helper() -> u32 {\n"
        "    crate::outside::alpha::alpha_helper()\n}\n"
    ),
    "fx/glob/leaf.rs": (
        "use super::*;\n\npub fn uses_the_glob() -> u32 {\n    sibling_helper()\n}\n"
    ),
    "fx/alias/mod.rs": "pub mod user;\npub use refact_providers::things;\n",
    "fx/alias/user.rs": (
        "use super::things::Thing;\n\npub fn takes_a_thing() -> Thing {\n    Thing::default()\n}\n"
    ),
    "fx/decl/mod.rs": "pub mod facade;\n",
    "fx/decl/facade.rs": "pub use refact_providers::things::Thing;\n",
}


def materialise(root):
    """Write the fixture tree under `root` and return `root`."""
    for rel, content in FILES.items():
        path = os.path.join(root, *rel.split("/"))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(content)
    return root
