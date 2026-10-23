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
- `clean` — drop `target/fast-release`.
- `cache-status` — sccache stats, repo-root length, and whether the root is on the sccache-cached path.
- `cache-reset` — restart the sccache server with the current `SCCACHE_BASEDIRS`.

The sccache server is one per user and is shared by every build on the machine
(other agents, the IDE, rust-analyzer). `engine-build.ps1` therefore never stops
it: a running server is reused, one is started only when none responds.
`SCCACHE_BASEDIRS` (main checkout, `~/.cache/refact/worktrees`, `C:\wt`, the
current repo root, plus `REFACT_SCCACHE_BASEDIRS`) is applied per compile
request - the client sends its environment - so the list can differ between
worktrees without any restart. An explicit restart stays available via
`-Mode cache-reset`.

Every build ends with a per-run line:

```
engine-build: sccache this run - 812 compile request(s), 640 cache hit(s), 172 miss(es)
```

Counters live in the shared server, so the delta also covers whatever else was
compiling at the same time. If a run degrades to local compilation it ends with
one prominent `WARNING: sccache degraded to local compilation this run` line
instead.

## If sccache degraded - what to look at

Symptom: a run prints `sccache: warning: ...` / `The server looks like it shut
down unexpectedly` / `WARNING: sccache degraded to local compilation this run`,
and the build costs a full recompile.

Start with `tools/dev/engine-build.ps1 -Mode cache-status`. It prints the server
state (up/down), the counters, the client version, the cache directory and its
size on disk, `SCCACHE_CACHE_SIZE`, `SCCACHE_BASEDIRS`, and whether the root is
short enough for sccache to wrap rustc. It also warns when the compile-request
counter went *backwards* since the previous run, which means the server was
restarted by something else between runs.

sccache 0.18 facts that cost real time to rediscover:

- **`--show-stats` is not a liveness probe.** With no server running it still
  exits 0 and prints a zeroed table (`Max cache size 10 GiB`, the *client's*
  `SCCACHE_BASEDIRS`). Code that greps its output for `Compile requests` to
  decide "is a server up?" concludes "yes" for a dead server. The server is the
  `sccache.exe` process whose command line has no `rustc` in it; clients are
  `sccache.exe <rustc> ...`.
- **No server means silent local compilation.** Clients do not resurrect a
  missing server; they just run the compiler. If the server dies *while* a
  client is connected, that client prints `The server looks like it shut down
  unexpectedly, compiling locally instead`.
- **`SCCACHE_BASEDIRS` is per compile request**, sent by the client. The server
  PID does not change when the list changes, so restarting the server to pick
  up new base dirs is useless - and harmful (see below).
- **`-o <file>` makes a call non-cacheable** (`Non-cacheable reasons: -o`).
  Cargo uses `--out-dir`, so `cargo check` units are cacheable; hand-written
  probes with `-o` are not.
- **`include!`-ed files are part of the key** (via dep-info). Identical content
  hits, changed content misses.

Checklist, in the order the causes were actually observed:

1. **The server was stopped by another client.** The server is one per user and
   shared by every build. Any client running `sccache --stop-server` kills it
   for everybody. `engine-build.ps1` never does that any more (it only starts a
   missing server and waits for the port to accept connections); an older copy
   of the script did it on every run from a different worktree, because the
   basedirs marker it compared contained the current worktree root. If the
   warning reappears, look for a script or a person stopping the server.
   rust-analyzer and other IDEs are *not* a cause: basedirs are per request, so
   a foreign client cannot invalidate the server.
2. **Nothing was compiled.** A warm tree makes cargo a no-op: zero compile
   requests, so zero hits, and any stray sccache warning is noise. The run ends
   with `sccache this run - nothing recompiled, nothing to cache`.
3. **Cache too small / evicting.** `SCCACHE_CACHE_SIZE` is 50G
   (`tools/dev/build-env.ps1`), the cache dir is
   `%LOCALAPPDATA%\Mozilla\sccache\cache`. A cache sitting at the limit shows a
   `Cache hits rate` near zero with a healthy number of `Compilations` - raise
   the size instead of clearing it.
4. **Command line too long for a crate.** `os error 206` / `failed to spawn` /
   `filename or extension is too long`: the repo root is too long for the
   ~176-chars-per-root-char rustc argv. `-Mode cache-status` prints the
   predicted length; keep worktree roots short (`REFACT_WORKTREE_ROOT=C:\wt`).
5. **Defender scanning the cache.** Real-time scanning of the cache and
   `target/` dirs slows every hit; see the Defender section below.
6. **Nothing else helps:** `-Mode cache-reset`, then compare the
   `sccache this run` lines of two runs that actually recompile something.

### What sccache can and cannot save on this crate graph

Measured on a cold worktree (`cargo check -p refact-lsp --lib`, 2026-10-23):

```
engine-build: sccache this run - 884 compile request(s), 503 cache hit(s), 381 miss(es)
```

So roughly half the units of a cold check come straight out of the cache -
dependency crates that other worktrees already built.

`refact-lsp` itself used to be the one unit that could never hit: its build
script ran `shadow_rs::ShadowBuilder::builder().build()`, which wrote the
wall-clock build time into `OUT_DIR/shadow.rs` - as the `BUILD_TIME*` constants
*and* in the generated file's `// Generation time:` header - and sccache hashes
`include!`-ed files. Every build had a different key, so the biggest crate in
the graph was always a full local compile (about 27 minutes here), and that was
the dominant cost of a cold worktree build.

`refact-agent/engine/build.rs` now pins `SOURCE_DATE_EPOCH` to the HEAD commit
timestamp before calling `ShadowBuilder` (shadow-rs honours that variable for
every timestamp it emits, header included). `OUT_DIR/shadow.rs` is therefore
byte-identical for a given commit and `refact-lsp` is cached like any other
crate. `BUILD_TIME*` keeps its meaning - it is the commit time, not an empty
value - and an explicitly exported `SOURCE_DATE_EPOCH` still wins. To see the
hit, force the crate to recompile (`cargo clean -p refact-lsp`) and run
`-Mode check` again: the second run reports `refact-lsp` as a cache hit instead
of a ~27 minute local compile.

Measured on this Windows box (2026-10-23, `-Mode check`, `refact-lsp` cleaned
with `cargo clean -p refact-lsp` before each run so it really recompiles):

```
run B (populates the cache): 869.5s - 2 compile request(s), 0 cache hit(s), 2 miss(es)
run C (same inputs):          37.8s - 2 compile request(s), 1 cache hit(s), 1 miss(es)
```

`OUT_DIR/shadow.rs` was byte-identical between the two runs, and `BUILD_TIME`
carries the HEAD commit timestamp instead of the wall clock.

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
