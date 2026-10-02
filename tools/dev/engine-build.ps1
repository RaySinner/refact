# Single entry point for building the Refact engine on Windows.
#
#   .\tools\dev\engine-build.ps1                 # check   (default) - verify a change compiles
#   .\tools\dev\engine-build.ps1 -Mode fast      # optimized binary, no LTO
#   .\tools\dev\engine-build.ps1 -Mode release   # shipping artifact ONLY (slow)
#   .\tools\dev\engine-build.ps1 -Mode test      # cargo test --lib
#   .\tools\dev\engine-build.ps1 -Mode clean     # drop target/fast-release
#
# Verification is `check`. A release build takes tens of minutes (LTO +
# opt-level="z" + strip) and is never the right way to find out whether code
# compiles - it is only for producing an artifact you package or ship.
#
# ASCII only on purpose: Windows PowerShell 5.1 reads .ps1 files as ANSI when
# there is no BOM, so non-ASCII characters here would be mangled.
[CmdletBinding()]
param(
    [ValidateSet('check', 'fast', 'release', 'test', 'clean')]
    [string]$Mode = 'check'
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$engineDir = Join-Path $repoRoot 'refact-agent\engine'

if (-not (Test-Path (Join-Path $engineDir 'Cargo.toml'))) {
    Write-Error "engine directory not found: $engineDir"
    exit 2
}

# PATH (cargo, LLVM, Node, MSVC), CARGO_INCREMENTAL=0 for sccache, NODE_OPTIONS.
. (Join-Path $PSScriptRoot 'build-env.ps1')

# Bypass the global sccache rustc-wrapper.
#
# The user-wide ~/.cargo/config.toml sets `rustc-wrapper = "sccache"`. sccache
# re-spawns rustc with the full argv, and `refact-lsp` alone has ~150 --extern
# paths under a long worktree directory, so the command line exceeds the Windows
# 32 767-character limit. sccache then dies with "os error 206" and rustc never
# runs at all - which also hides real type errors behind an opaque spawn failure.
#
# `cargo --config 'build.rustc-wrapper=""'` cannot be used here: PowerShell 5.1
# and cmd both strip the inner quotes before cargo sees them, and cargo rejects
# the resulting bare `build.rustc-wrapper=` as invalid TOML. A config FILE avoids
# the shell entirely, so that is what this writes.
$sccacheBypass = Join-Path $env:TEMP 'refact-engine-build-nosccache.toml'
[IO.File]::WriteAllText($sccacheBypass, "[build]`r`nrustc-wrapper = `"`"`r`n")

function Invoke-Cargo {
    param([string[]]$CargoArgs)
    $proc = Start-Process -FilePath 'cargo' `
        -ArgumentList (@('--config', $sccacheBypass) + $CargoArgs) `
        -WorkingDirectory $engineDir -NoNewWindow -Wait -PassThru
    return $proc.ExitCode
}

function Write-Banner {
    param([string]$Text)
    Write-Host ''
    Write-Host "=== engine-build: $Text ===" -ForegroundColor Cyan
    Write-Host ''
}

$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$exitCode = 0

switch ($Mode) {
    'check' {
        Write-Banner 'mode check (cargo check --workspace --all-targets) - verifying a change compiles'
        # A Rust-side check must never drag the frontend build in with it.
        $env:REFACT_SKIP_GUI_BUILD = '1'
        $exitCode = Invoke-Cargo @('check', '--workspace', '--all-targets')
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
}

$stopwatch.Stop()
Write-Host ''
Write-Host ("=== engine-build: {0} finished in {1:N1}s, exit {2} ===" -f $Mode, $stopwatch.Elapsed.TotalSeconds, $exitCode) -ForegroundColor Cyan
exit $exitCode
