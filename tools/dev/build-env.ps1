# Windows build environment for the Refact engine.
# Import into PowerShell before cargo/vsce builds:
#   . C:\Raid\Repos\Rust\refact-main\tools\dev\build-env.ps1

$msvcRoot = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
$msvcTools = (Get-ChildItem "$msvcRoot\VC\Tools\MSVC" -Directory -ErrorAction SilentlyContinue |
    Sort-Object Name -Descending | Select-Object -First 1).FullName
$cargoBin = "$env:USERPROFILE\.cargo\bin"
$nodeBin = "C:\Program Files\nodejs"

$paths = @($cargoBin, $nodeBin)
if ($msvcTools) { $paths += "$msvcTools\bin\Hostx64\x64" }

foreach ($p in $paths) {
    if ($p -and (Test-Path $p) -and ($env:PATH -notlike "*$p*")) {
        $env:PATH = "$p;$env:PATH"
    }
}

# sccache cache size; applied when the sccache server (re)starts. CARGO_INCREMENTAL
# is NOT set here: engine-build.ps1 sets it per invocation - 0 while sccache is
# active (required for cache hits), 1 when the wrapper is bypassed.
$env:SCCACHE_CACHE_SIZE = "50G"
$env:NODE_OPTIONS = "--max-old-space-size=8192"

# ---------------------------------------------------------------------------
# Build artifacts live on D:, not next to the sources.
#
# Two reasons, both measured on this machine:
#
#  1. Disk. A single engine target/ is 15-40 GB, and every agent worktree gets
#     its own. Keeping them off C: stops the system drive filling up.
#  2. Command line length. rustc's argv contains the target dir once per
#     dependency (~176 crates, each contributing --out-dir / -L dependency /
#     --extern paths). Shortening the target path by N characters removes
#     ~176*N characters from the command line. Moving from
#     C:\Raid\Repos\Rust\refact-main\refact-agent\engine\target (56 chars) to
#     D:\refact\engine\target (23) removes ~5 800, which is what makes the
#     builds stop tripping Windows' 32 767-char limit.
#
# Scheme: <REFACT_BUILD_ROOT>\<project>\engine\target
#   REFACT_BUILD_ROOT defaults to D:\refact, so this repo builds into
#   D:\refact\engine\target. Other projects on this machine get their own
#   subtree under the same root (D:\refact, D:\otherproject, ...).
$buildProject = "refact"
$buildRoot = if ($env:REFACT_BUILD_ROOT) { $env:REFACT_BUILD_ROOT } else { "D:\$buildProject" }
$env:REFACT_BUILD_ROOT = $buildRoot
$env:CARGO_TARGET_DIR = Join-Path $buildRoot 'engine\target'
if (-not (Test-Path $env:CARGO_TARGET_DIR)) {
    New-Item -ItemType Directory -Force -Path $env:CARGO_TARGET_DIR | Out-Null
}

# Agent worktrees also live on D: by default. Must be set in the environment of the
# process that SPAWNS agents (the engine), not just in this shell - the engine reads
# REFACT_WORKTREE_ROOT once, at startup. Exporting it here means a dev-launched
# engine inherits it; an already-running engine needs a restart to pick it up.
if (-not $env:REFACT_WORKTREE_ROOT) {
    $env:REFACT_WORKTREE_ROOT = 'D:\wt'
}
if (-not (Test-Path $env:REFACT_WORKTREE_ROOT)) {
    New-Item -ItemType Directory -Force -Path $env:REFACT_WORKTREE_ROOT | Out-Null
}

Write-Host "PATH prepared (cargo, Node, MSVC=$msvcTools)"
Write-Host "CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR (build artifacts on D:)"
