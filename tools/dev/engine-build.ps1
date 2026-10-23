# Single entry point for building the Refact engine on Windows.
#
#   .\tools\dev\engine-build.ps1                      # check  (default) - does my refact-lsp change compile?
#   .\tools\dev\engine-build.ps1 -Mode check-all      # whole workspace incl. every crate's test targets
#   .\tools\dev\engine-build.ps1 -Mode check-tests    # type-check tests WITHOUT linking
#   .\tools\dev\engine-build.ps1 -Mode fast           # optimized binary, no LTO
#   .\tools\dev\engine-build.ps1 -Mode release        # shipping artifact ONLY (slow)
#   .\tools\dev\engine-build.ps1 -Mode test           # cargo test --lib (links the test binary)
#   .\tools\dev\engine-build.ps1 -Mode clean          # drop target/fast-release
#   .\tools\dev\engine-build.ps1 -Mode cache-status   # sccache server health, stats, detected mode
#   .\tools\dev\engine-build.ps1 -Mode cache-reset    # force a server restart (builds never need it)
#
# Which check to run:
#   check       - 90% of cases: you edited files under refact-agent/engine/src
#                 (fastest; still type-checks the whole dependency graph)
#   check-all   - you touched a workspace crate under engine/crates, or need
#                 every crate's test targets type-checked
#   check-tests - you edited #[cfg(test)] code and want errors without paying
#                 the link step; use -Mode test only when you will RUN tests
#
# Verification is `check`. A release build takes tens of minutes (LTO +
# opt-level="z" + strip) and is never the right way to find out whether code
# compiles - it is only for producing an artifact you package or ship.
#
# sccache policy: try sccache FIRST so worktrees share compilation hits, and
# only fall back to the wrapper-bypass config when the build dies with the
# Windows 32 767-char command-line overflow (os error 206). Without sccache,
# worktree cold builds pay a full recompile of ~85 crates every time.
#
# CARGO_INCREMENTAL must match the chosen mode: 0 while sccache is active
# (sccache cannot cache incremental artifacts), 1 when the wrapper is bypassed
# (otherwise the user pays full recompiles with no cache at all).
#
# SCCACHE_BASEDIRS makes sccache hash paths relative to the listed roots, so
# the same crate built in different worktrees (or in the main checkout) hits
# the same cache entry. The list (main checkout root, the default worktree
# parent, C:\wt, this repo root, plus REFACT_SCCACHE_BASEDIRS semicolon-
# separated) is applied PER COMPILE REQUEST: the client sends its own
# environment, so the server keeps working when the list changes and does not
# need a restart for it (observed: the server PID stays the same while
# different clients report different base dirs).
#
# The server is one per user and is shared by every concurrent build (other
# agents, the IDE, rust-analyzer). This script therefore NEVER stops it: a
# healthy server is reused as-is, one is started only when none responds. An
# earlier version stopped the server whenever the basedirs marker of the last
# run differed, which - because the list contains the current worktree root -
# happened on every run from a different worktree. That killed the server
# another build was using; that build then printed "the server looks like it
# shut down unexpectedly" and compiled locally, which is the degradation that
# inflated every measurement. Explicit restarts stay available via
# -Mode cache-reset.
#
# ASCII only on purpose: Windows PowerShell 5.1 reads .ps1 files as ANSI when
# there is no BOM, so non-ASCII characters here would be mangled.
[CmdletBinding()]
param(
    [ValidateSet('check', 'check-all', 'check-tests', 'fast', 'release', 'test', 'clean', 'cache-status', 'cache-reset')]
    [string]$Mode = 'check'
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$engineDir = Join-Path $repoRoot 'refact-agent\engine'

if (-not (Test-Path (Join-Path $engineDir 'Cargo.toml'))) {
    Write-Error "engine directory not found: $engineDir"
    exit 2
}

# PATH (cargo, Node, MSVC), NODE_OPTIONS. CARGO_INCREMENTAL is set per
# invocation below, not here - see the sccache policy comment above.
. (Join-Path $PSScriptRoot 'build-env.ps1')

# Wrapper-bypass config for the os error 206 fallback.
#
# `cargo --config 'build.rustc-wrapper=""'` cannot be used here: PowerShell 5.1
# and cmd both strip the inner quotes before cargo sees them, and cargo rejects
# the resulting bare `build.rustc-wrapper=` as invalid TOML. A config FILE avoids
# the shell entirely, so that is what this writes.
$sccacheBypass = Join-Path $env:TEMP 'refact-engine-build-nosccache.toml'
[IO.File]::WriteAllText($sccacheBypass, "[build]`r`nrustc-wrapper = `"`"`r`n")
$sccacheEnable = Join-Path $env:TEMP 'refact-engine-build-sccache.toml'
[IO.File]::WriteAllText($sccacheEnable, "[build]`r`nrustc-wrapper = `"sccache`"`r`n")

function Get-MainRepoRoot {
    # The main checkout root for this worktree: the parent of the shared .git dir.
    try {
        $gitCommon = (& git -C $repoRoot rev-parse --path-format=absolute --git-common-dir 2>$null |
            Select-Object -First 1)
        if ($gitCommon) {
            $commonPath = if ([IO.Path]::IsPathRooted($gitCommon)) {
                [IO.Path]::GetFullPath($gitCommon)
            } else {
                [IO.Path]::GetFullPath((Join-Path $repoRoot $gitCommon))
            }
            return (Split-Path -Parent $commonPath)
        }
    } catch { }
    return $null
}

function Set-SccacheBaseDirs {
    # Stable superset, independent of the current directory, so a server
    # started by any build already covers every root used on this machine.
    $dirs = New-Object System.Collections.Generic.List[string]
    $main = Get-MainRepoRoot
    if ($main -and (Test-Path $main)) { $dirs.Add($main) }
    if (-not $dirs.Contains($repoRoot)) { $dirs.Add($repoRoot) }
    $defaultWorktrees = Join-Path $env:USERPROFILE '.cache\refact\worktrees'
    if ((Test-Path $defaultWorktrees) -and -not $dirs.Contains($defaultWorktrees)) {
        $dirs.Add($defaultWorktrees)
    }
    if ((Test-Path 'C:\wt') -and -not $dirs.Contains('C:\wt')) { $dirs.Add('C:\wt') }
    if ($env:REFACT_SCCACHE_BASEDIRS) {
        foreach ($extra in $env:REFACT_SCCACHE_BASEDIRS.Split(';')) {
            $extra = $extra.Trim()
            if ($extra -and (Test-Path $extra) -and -not $dirs.Contains($extra)) {
                $dirs.Add($extra)
            }
        }
    }
    $env:SCCACHE_BASEDIRS = ($dirs -join ';')
}

function Get-SccacheStats {
    # --show-stats talks to the shared server; a dead or missing server makes it
    # fail, which is exactly the "is one running?" probe used below.
    $ErrorActionPreference = 'Continue'
    try {
        return ((& sccache --show-stats 2>&1) -join "`n")
    }
    finally {
        $ErrorActionPreference = 'Stop'
    }
}

function Test-SccacheServerRunning {
    # --show-stats is NOT a liveness probe: with no server running it still
    # exits 0 and prints a zeroed table (Max cache size 10 GiB, Base directories
    # (none)), which is how an earlier version of this script convinced itself a
    # dead server was healthy and compiled for 27 minutes without caching.
    # The server is the sccache.exe process with no compiler on its command
    # line; clients are "sccache.exe <rustc> ...".
    $procs = @(Get-CimInstance Win32_Process -Filter "Name='sccache.exe'" -ErrorAction SilentlyContinue)
    foreach ($p in $procs) {
        if (-not $p.CommandLine) { continue }
        if ($p.CommandLine -notmatch 'rustc') { return $true }
    }
    return $false
}

function Test-SccachePortOpen {
    param([int]$Port)
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $connect = $client.BeginConnect('127.0.0.1', $Port, $null, $null)
        if (-not $connect.AsyncWaitHandle.WaitOne(250)) { return $false }
        $client.EndConnect($connect)
        return $true
    }
    catch { return $false }
    finally { $client.Close() }
}

function Wait-SccacheServerReady {
    # --start-server returns once the port is bound, but the very first client
    # can still lose the race: cargo calls `rustc -vV` through the wrapper
    # immediately, and a refused connection makes sccache print "the server
    # looks like it shut down unexpectedly, compiling locally instead" and skip
    # caching for the whole build. Wait for the port to accept a connection.
    $port = 4226
    if ($env:SCCACHE_SERVER_PORT -match '^\d+$') { $port = [int]$env:SCCACHE_SERVER_PORT }
    for ($i = 0; $i -lt 50; $i++) {
        if (Test-SccachePortOpen $port) { return $true }
        Start-Sleep -Milliseconds 100
    }
    return $false
}

function Get-SccacheCounter {
    param([string]$Stats, [string]$Name)
    if ($Stats -match ('(?m)^' + [regex]::Escape($Name) + '\s+(\d+)\s*$')) {
        return [int]$Matches[1]
    }
    return $null
}

function Get-SccacheStatePath {
    return (Join-Path $env:TEMP 'refact-engine-build-sccache-state.txt')
}

function Read-SccacheState {
    # Counters are cumulative since the server started, so a counter that went
    # backwards means the server was restarted by somebody else between runs.
    $path = Get-SccacheStatePath
    if (-not (Test-Path $path)) { return $null }
    $state = @{}
    foreach ($line in [IO.File]::ReadAllLines($path)) {
        if ($line -match '^([a-z_]+)=(\d+)$') { $state[$Matches[1]] = [int]$Matches[2] }
    }
    if ($state.Count -eq 0) { return $null }
    return $state
}

function Save-SccacheState {
    param([string]$Stats)
    $requests = Get-SccacheCounter $Stats 'Compile requests'
    if ($null -eq $requests) { return }
    $hits = Get-SccacheCounter $Stats 'Cache hits'
    if ($null -eq $hits) { $hits = 0 }
    [IO.File]::WriteAllText((Get-SccacheStatePath), ("compile_requests={0}`r`ncache_hits={1}`r`n" -f $requests, $hits))
}

function Write-SccacheServerState {
    param([string]$Stats)
    if (-not (Test-SccacheServerRunning)) {
        Write-Host 'sccache: server is NOT running - nothing is being cached until one starts' -ForegroundColor Yellow
        return $false
    }
    $previous = Read-SccacheState
    $requests = Get-SccacheCounter $Stats 'Compile requests'
    $hits = Get-SccacheCounter $Stats 'Cache hits'
    if ($null -eq $requests) { $requests = 0 }
    if ($null -eq $hits) { $hits = 0 }
    Write-Host ("sccache: server up - compile requests {0}, cache hits {1}" -f $requests, $hits)
    $restarted = $false
    if ($previous -and $previous.ContainsKey('compile_requests') -and $requests -lt $previous['compile_requests']) {
        $restarted = $true
        Write-Host ("WARNING: sccache server was restarted since the previous run (compile requests {0} -> {1}), so the previous run's cache accounting was lost. Something outside this script stopped it - see docs/dev/build-velocity.md#sccache-degraded" -f $previous['compile_requests'], $requests) -ForegroundColor Yellow
    }
    Save-SccacheState $Stats
    return $restarted
}

function Ensure-SccacheServer {
    # The server is shared with every other build on the machine, so only ever
    # start a missing one. SCCACHE_BASEDIRS is sent by the client with each
    # compile request, so a running server serves this worktree correctly no
    # matter which client started it.
    if (Test-SccacheServerRunning) {
        Write-Host 'engine-build: sccache server is up - reusing it (never stopped by this script)'
    }
    else {
        $out = ((& sccache --start-server 2>&1) -join ' ').Trim()
        $code = $LASTEXITCODE
        if ($code -ne 0 -and $out -notmatch 'Address in use') {
            Write-Host "WARNING: sccache server could not be started ($out) - this build compiles locally" -ForegroundColor Yellow
            return
        }
        Write-Host 'engine-build: sccache server was down - started it'
    }
    if (-not (Wait-SccacheServerReady)) {
        Write-Host 'WARNING: sccache server is not accepting connections - this build may compile locally' -ForegroundColor Yellow
    }
}

function Write-SccacheRunDelta {
    # Per-run cache accounting. Counters are shared with every other build on the
    # machine, so the delta is "this run plus whoever else was compiling".
    param($BeforeRequests, $BeforeHits)
    $afterStats = Get-SccacheStats
    $afterRequests = Get-SccacheCounter $afterStats 'Compile requests'
    $afterHits = Get-SccacheCounter $afterStats 'Cache hits'
    if ($null -eq $BeforeRequests -or $null -eq $afterRequests) {
        Write-Host 'engine-build: sccache counters unavailable after the build'
        return
    }
    $requests = $afterRequests - $BeforeRequests
    $hits = 0
    if ($null -ne $BeforeHits -and $null -ne $afterHits) { $hits = $afterHits - $BeforeHits }
    if ($requests -lt 0 -or $hits -lt 0) {
        Write-Host 'WARNING: sccache counters went backwards during the build - the server was restarted mid-run (see docs/dev/build-velocity.md#sccache-degraded)' -ForegroundColor Yellow
        $script:sccacheDegraded = $true
    }
    elseif ($requests -eq 0) {
        # A warm tree makes cargo a no-op: nothing was compiled, so there is
        # nothing to cache and a stray sccache warning means nothing here.
        Write-Host 'engine-build: sccache this run - nothing recompiled, nothing to cache'
        $script:sccacheDegraded = $false
    }
    else {
        Write-Host ("engine-build: sccache this run - {0} compile request(s), {1} cache hit(s), {2} miss(es)" -f $requests, $hits, ($requests - $hits))
    }
    Save-SccacheState $afterStats
}


function Test-SpawnFailure {
    param([string]$Output)
    return ($Output -match 'os error 206' -or
        $Output -match 'failed to spawn' -or
        $Output -match 'filename or extension is too long')
}

function Get-PredictedArgvLength {
    # Measured on this crate graph: the refact-lsp rustc argv is ~176 characters
    # per character of repo-root length plus a fixed ~16k (143 --extern and 34
    # -L native paths). Windows caps a command line at 32767, and sccache appends
    # its own --emit dep-info -o argument on top, so keep a margin.
    return (176 * $repoRoot.Length) + 16030
}

# Run cargo with sccache first; on the command-line-overflow signature retry
# once with the wrapper bypassed. Returns the final exit code. Prints which
# mode (sccache/bypass) was used.
function Invoke-Cargo {
    param([string[]]$CargoArgs)

    Push-Location $engineDir
    # cargo writes progress ("Compiling ...") to stderr; with the script-wide
    # ErrorActionPreference=Stop those lines would become terminating errors
    # inside the 2>&1 pipeline. Downgrade for the cargo invocations only.
    $ErrorActionPreference = 'Continue'
    # Output is collected into a variable and echoed with Write-Host so that
    # only the exit code leaves this function (a Tee-Object pipeline would put
    # every cargo line into the caller's $exitCode).
    try {
        if (Get-Command sccache -ErrorAction SilentlyContinue) {
            $predicted = Get-PredictedArgvLength
            if ($predicted -gt 31000) {
                # Do not burn a full compile pass on a build sccache cannot wrap.
                Write-Host ("engine-build: skipping sccache - predicted rustc command line is {0} chars (Windows limit 32767). Repo root is {1} chars; keep it <= 90 to stay on the cached path." -f $predicted, $repoRoot.Length)
            }
            else {
                Set-SccacheBaseDirs
                Ensure-SccacheServer
                $env:CARGO_INCREMENTAL = '0'
                Write-Host "engine-build: using sccache (CARGO_INCREMENTAL=0, SCCACHE_BASEDIRS=$env:SCCACHE_BASEDIRS)"
                $beforeStats = Get-SccacheStats
                $beforeRequests = Get-SccacheCounter $beforeStats 'Compile requests'
                $beforeHits = Get-SccacheCounter $beforeStats 'Cache hits'
                $out = @(& cargo @('--config', $sccacheEnable) @CargoArgs 2>&1)
                $exitCode = $LASTEXITCODE
                $out | ForEach-Object { Write-Host $_ }
                $outText = $out | Out-String
                if ($outText -match 'sccache: warning' -or
                    $outText -match 'compiling locally instead' -or
                    $outText -match 'shut down unexpectedly') {
                    $script:sccacheDegraded = $true
                }
                Write-SccacheRunDelta $beforeRequests $beforeHits
                if ($exitCode -eq 0) { return 0 }
                if (-not (Test-SpawnFailure ($out | Out-String))) { return $exitCode }
                Write-Warning 'sccache run failed with the command-line overflow signature; retrying once with the rustc wrapper bypassed.'
            }
        }

        $env:CARGO_INCREMENTAL = '1'
        Write-Host 'engine-build: using sccache bypass (CARGO_INCREMENTAL=1)'
        $out = @(& cargo @('--config', $sccacheBypass) @CargoArgs 2>&1)
        $out | ForEach-Object { Write-Host $_ }
        return $LASTEXITCODE
    }
    finally {
        Pop-Location
        $ErrorActionPreference = 'Stop'
    }
}

function Write-Banner {
    param([string]$Text)
    Write-Host ''
    Write-Host "=== engine-build: $Text ===" -ForegroundColor Cyan
    Write-Host ''
}

$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$exitCode = 0
$script:sccacheDegraded = $false

switch ($Mode) {
    'check' {
        Write-Banner 'mode check (cargo check -p refact-lsp --lib) - verifying a refact-lsp change compiles'
        # A Rust-side check must never drag the frontend build in with it.
        $env:REFACT_SKIP_GUI_BUILD = '1'
        $exitCode = Invoke-Cargo @('check', '-p', 'refact-lsp', '--lib')
    }
    'check-all' {
        Write-Banner 'mode check-all (cargo check --workspace --all-targets) - whole workspace incl. test targets'
        $env:REFACT_SKIP_GUI_BUILD = '1'
        $exitCode = Invoke-Cargo @('check', '--workspace', '--all-targets')
    }
    'check-tests' {
        Write-Banner 'mode check-tests (cargo check --profile test --all-targets) - type-check tests without linking'
        $env:REFACT_SKIP_GUI_BUILD = '1'
        $exitCode = Invoke-Cargo @('check', '--profile', 'test', '--all-targets')
    }
    'fast' {
        Write-Banner 'mode fast (cargo build --profile fast-release --bin refact-lsp) - runnable binary, no LTO'
        $env:REFACT_SKIP_GUI_BUILD = '1'
        $exitCode = Invoke-Cargo @('build', '--profile', 'fast-release', '--bin', 'refact-lsp')
    }
    'release' {
        Write-Banner 'mode release (cargo build --release --bin refact-lsp)'
        Write-Warning 'release builds take tens of minutes. Use it to produce a shipping artifact, not to verify a change - run check mode for that.'
        $exitCode = Invoke-Cargo @('build', '--release', '--bin', 'refact-lsp')
    }
    'test' {
        Write-Banner 'mode test (cargo test --lib) - compiles the whole test target first'
        $exitCode = Invoke-Cargo @('test', '--lib')
    }
    'clean' {
        Write-Banner 'mode clean (remove target/fast-release)'
        $fastDir = Join-Path $engineDir 'target\fast-release'
        if (Test-Path $fastDir) {
            Remove-Item -Recurse -Force $fastDir
            Write-Host "removed $fastDir"
        }
        else {
            Write-Host "nothing to remove: $fastDir"
        }
    }
    'cache-status' {
        Write-Banner 'mode cache-status (sccache stats and detected build mode)'
        Set-SccacheBaseDirs
        $predicted = Get-PredictedArgvLength
        $cachedPath = $predicted -le 31000
        Write-Host ("repo root: {0} ({1} chars)" -f $repoRoot, $repoRoot.Length)
        Write-Host ("predicted refact-lsp rustc argv: {0} chars (Windows limit 32767, sccache skipped above 31000)" -f $predicted)
        if ($cachedPath) {
            Write-Host 'cached path: YES - repo root length is within the sccache-safe range, sccache can wrap this build' -ForegroundColor Green
        }
        else {
            Write-Host 'cached path: NO - repo root is too long for sccache (os error 206). Keep worktree roots short; agent worktrees can set REFACT_WORKTREE_ROOT=C:\wt' -ForegroundColor Yellow
        }
        Write-Host "SCCACHE_BASEDIRS: $env:SCCACHE_BASEDIRS"
        if (Get-Command sccache -ErrorAction SilentlyContinue) {
            Write-Host 'sccache: installed - engine-build.ps1 uses it first, falls back to the wrapper bypass on os error 206.'
            Write-Host ("sccache client version: {0}" -f ((& sccache --version 2>&1) -join ' '))
            $stats = Get-SccacheStats
            if (Test-SccacheServerRunning) {
                Write-Host 'sccache: server is running - this script only starts a missing server, it never stops a healthy one'
                $cacheDir = Join-Path $env:LOCALAPPDATA 'Mozilla\sccache\cache'
                if (Test-Path $cacheDir) {
                    $sum = (Get-ChildItem $cacheDir -Recurse -File -ErrorAction SilentlyContinue |
                        Measure-Object Length -Sum)
                    Write-Host ("sccache: cache dir {0} - {1} files, {2:N1} GiB on disk (SCCACHE_CACHE_SIZE={3})" -f
                        $cacheDir, $sum.Count, ($sum.Sum / 1GB), $env:SCCACHE_CACHE_SIZE)
                }
                else {
                    Write-Host "sccache: cache dir not found yet: $cacheDir"
                }
                Write-SccacheServerState $stats | Out-Null
                $requests = Get-SccacheCounter $stats 'Compile requests'
                $hits = Get-SccacheCounter $stats 'Cache hits'
                if ($requests -gt 0 -and $hits -eq 0) {
                    Write-Host 'WARNING: sccache has served compile requests but recorded no cache hits since the server started - the cache is not being reused (see docs/dev/build-velocity.md#sccache-degraded)' -ForegroundColor Yellow
                }
                $ra = Get-Process -Name 'rust-analyzer' -ErrorAction SilentlyContinue
                if ($ra) {
                    Write-Host ("sccache: note - {0} rust-analyzer process(es) are also sccache clients; they share this server and are harmless (basedirs are per request)" -f @($ra).Count)
                }
            }
            else {
                Write-Host 'WARNING: sccache server is NOT running - the next build will start one; until then nothing is cached' -ForegroundColor Yellow
            }
            Write-Host ''
            $stats | Write-Host
        }
        else {
            Write-Host 'sccache: NOT installed - engine-build.ps1 always uses the bypass (CARGO_INCREMENTAL=1).'
            Write-Host 'install with: cargo install sccache'
        }
    }
    'cache-reset' {
        Write-Banner 'mode cache-reset (restart sccache server with the current SCCACHE_BASEDIRS)'
        Set-SccacheBaseDirs
        & sccache --stop-server 2>&1 | Out-Null
        & sccache --start-server 2>&1 | Out-Null
        Write-Host "sccache server restarted with SCCACHE_BASEDIRS=$env:SCCACHE_BASEDIRS"
        Write-Host 'note: builds do not need this - SCCACHE_BASEDIRS is applied per compile request and this script never stops a healthy server.'
    }
}

$stopwatch.Stop()
if ($script:sccacheDegraded) {
    Write-Host ''
    Write-Host 'WARNING: sccache degraded to local compilation this run - the build was not cached (see -Mode cache-status and docs/dev/build-velocity.md#sccache-degraded)' -ForegroundColor Yellow
}
if ($exitCode -isnot [int]) {
    Write-Host "engine-build: exit code is not an int: $($exitCode -join ', ') - coercing to the last value"
    $last = $exitCode | Select-Object -Last 1
    if ($last -is [int]) { $exitCode = $last } else { $exitCode = 1 }
}
Write-Host ''
Write-Host ("=== engine-build: {0} finished in {1:N1}s, exit {2} ===" -f $Mode, $stopwatch.Elapsed.TotalSeconds, $exitCode) -ForegroundColor Cyan
exit $exitCode
