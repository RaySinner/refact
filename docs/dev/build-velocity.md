# Build Velocity: which check answers which question

Every change type has a *cheapest sufficient check*. Reaching for a heavier one
wastes 45+ minutes and answers the same question no better. The rule that
follows from the table below:

**Prepare, then launch — never build a runnable artifact to answer a compile
question.** A build only produces an artifact; it does not verify anything a
check would not have told you, and a release engine build (LTO + `opt-level=z` +
strip) costs 40-50 minutes cold on this machine.

## Verification matrix

| Change type | Cheapest sufficient check | Approx. cost |
|---|---|---|
| Plugin-only (VS Code / JetBrains) | `npm run test` / `types` / `lint` in `plugins/vscode`, or `gradlew test` in `plugins/intellij` | minutes — **no cargo at all** |
| Engine change (`refact-agent/engine/src/...`) | `tools/dev/engine-build.ps1` (`check` mode: `cargo check -p refact-lsp --lib`) | fast; still type-checks the full dependency graph, skips other crates' test targets |
| Workspace crate change (`crates/*`) | `engine-build.ps1 -Mode check-all` (`cargo check --workspace --all-targets`) | slower; checks every crate incl. test targets |
| Test-only change | `engine-build.ps1 -Mode check-tests` (`cargo check --profile test --all-targets`) — type-checks tests without linking | cheap |
| Before launching the extension locally | `tools/dev/build-all.ps1` (default `-Profile fast`: `fast-release`, no LTO) | tens of minutes |
| Shipping a release | `tools/dev/build-all.ps1 -Profile release` | slow, once |

Engine unit tests, when actually needed: `engine-build.ps1 -Mode test`
(`cargo test --lib`).

## Why fast instead of release for local testing

The `fast-release` profile inherits release settings but sets `lto = false`,
`codegen-units = 16`, `incremental = true`. LTO shrinks the binary and buys
runtime performance; it buys nothing for a local install you throw away. The
default `-Profile fast` cut the full GUI → engine → VSIX chain from roughly
1h20m to a fraction of that.

## Where the artifacts land

| Artifact | Path |
|---|---|
| Engine binary (fast) | `refact-agent/engine/target/fast-release/refact.exe` |
| Engine binary (release) | `refact-agent/engine/target/release/refact.exe` |
| Engine binary (dev) | `refact-agent/engine/target/debug/refact-lsp.exe` |
| GUI dist | `refact-agent/gui/dist/chat/` (+ `refact-chat-js-<version>.tgz`) |
| VSIX | newest `plugins/vscode/*.vsix` (copied engine sits at `plugins/vscode/assets/refact.exe`) |

`build-all.ps1` prints which profile produced the VSIX at the end; a `fast`
VSIX is explicitly flagged as **local testing only**.

## Engine-build mode reference

See the header of `tools/dev/engine-build.ps1` for the full table:

- `check` (default) — `cargo check -p refact-lsp --lib`; "does my change compile".
- `check-all` — `cargo check --workspace --all-targets`; workspace crate or cross-crate test impact.
- `check-tests` — `cargo check --profile test --all-targets`; test edits, no linking.
- `fast` — `cargo build --profile fast-release --bin refact-lsp`; a runnable binary without LTO.
- `release` — shipping artifact only. Never a verification step.
- `test` — `cargo test --lib`.
- `clean` — drop `target/fast-release`.- `clean` — drop `target/fast-release`.
- `cache-status` — sccache stats, repo-root length, and whether the root is on the sccache-cached path.

## sccache and worktree root length (Windows)

The `refact-lsp` rustc command line grows **~176 characters per character of
repo-root length** (143 `--extern` + 34 `-L native` paths add a fixed ~16k).
Windows caps a command line at 32 767, so roots above ~90 characters overflow
(`os error 206`) once sccache wraps rustc, silently costing every worktree
build a full recompile.

Default agent worktree roots (`~\.cache\refact\worktrees\<hash>\<uuid>` ≈ 94
chars) are too long. Set the env override:

```
REFACT_WORKTREE_ROOT=C:\wt
```

Worktrees and their registry then live under `C:\wt\<sha256(source_root)>\<uuid>`
— short enough for sccache, and `SCCACHE_BASEDIRS` makes builds in different
worktrees hit the same cache entries. Roots longer than 120 characters are
refused by the engine; between 90 and 120 it warns. Unset, the default
`~/.cache/refact` location is used unchanged.

`tools/dev/engine-build.ps1 -Mode cache-status` prints the current repo-root
length, the predicted argv length, and whether the root is on the cached path.

## Windows Defender

Real-time scanning of `target/` dirs, `node_modules`, and the cargo/sccache
caches routinely costs 2-3x on link-heavy Rust builds (this repo links one
very large binary). One command adds narrowly scoped exclusions for exactly the
hot paths:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\windows-defender-exclusions.ps1 -WhatIf   # list only
Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','<repo>\tools\dev\windows-defender-exclusions.ps1'   # apply (elevated)
```

It excludes: repo `refact-agent\engine\target`, GUI/VSCode `node_modules`,
`plugins\intellij\build`, the worktrees root (`REFACT_WORKTREE_ROOT`, default
`%USERPROFILE%\.cache\refact\worktrees`), `%USERPROFILE%\.cargo`, and the
sccache cache dir. It is idempotent, removes its own exclusions with
`-Remove`, and never excludes drive roots, `%USERPROFILE%`, or `%TEMP%` as a
whole.

**Measure the effect yourself:** time one warm `engine-build.ps1` (check mode)
before applying, then again after, and compare. Defenders gains vary per
machine; the numbers are yours, not the script's.
