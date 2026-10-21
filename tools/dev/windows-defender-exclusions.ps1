# Windows Defender exclusions for build hot paths.
# Real-time scanning of Rust target/ dirs and node_modules routinely adds a
# 2-3x penalty on link-heavy builds. This script adds (or removes) narrowly
# scoped exclusions for exactly the paths this repo builds in.
#
# Usage:
#   powershell -NoProfile -File tools\dev\windows-defender-exclusions.ps1            # add (requires admin)
#   powershell -NoProfile -File tools\dev\windows-defender-exclusions.ps1 -WhatIf    # list only, no changes
#   powershell -NoProfile -File tools\dev\windows-defender-exclusions.ps1 -Remove    # remove what this script adds (admin)
#
# -WhatIf never needs elevation; adding/removing does. Idempotent: re-running
# does not duplicate exclusions.

param(
    [switch]$WhatIf,
    [switch]$Remove
)

$ErrorActionPreference = 'Stop'
$script:ScriptName = 'windows-defender-exclusions.ps1'

function Test-IsAdmin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-SccacheCacheDir {
    $sccache = Get-Command sccache -ErrorAction SilentlyContinue
    if ($sccache) {
        try {
            $stats = & $sccache.Source show-stats 2>$null
            foreach ($line in $stats) {
                if ($line -match 'Cache location:\s+(.+)$') {
                    return $Matches[1].Trim()
                }
            }
        } catch { }
    }
    $fallback = Join-Path $env:LOCALAPPDATA 'Mozilla\sccache'
    if (Test-Path $fallback) { return $fallback }
    return $null
}

function Get-ExclusionPaths {
    # Repo root = two levels above tools\dev.
    $repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $candidates = @(
        (Join-Path $repoRoot 'refact-agent\engine\target'),
        (Join-Path $repoRoot 'refact-agent\gui\node_modules'),
        (Join-Path $repoRoot 'plugins\vscode\node_modules'),
        (Join-Path $repoRoot 'plugins\intellij\build')
    )

    $worktreeRoot = $env:REFACT_WORKTREE_ROOT
    if (-not $worktreeRoot) {
        $worktreeRoot = Join-Path $env:USERPROFILE '.cache\refact\worktrees'
    }
    if ($worktreeRoot) { $candidates += $worktreeRoot }

    if ($env:USERPROFILE) { $candidates += (Join-Path $env:USERPROFILE '.cargo') }

    $sccacheDir = Get-SccacheCacheDir
    if ($sccacheDir) { $candidates += $sccacheDir }

    # Skip absent paths (repo subdirs only; env-derived roots are kept even if
    # not yet created). Resolve so the exclusion matches the real path.
    $paths = @()
    foreach ($p in $candidates) {
        if (Test-Path -LiteralPath $p) {
            $resolved = (Resolve-Path -LiteralPath $p).Path
            if ($paths -notcontains $resolved) { $paths += $resolved }
        } elseif ($p -like "$repoRoot*") {
            Write-Host "skip (absent): $p"
        } else {
            # Env-derived dir (worktree root / cargo home) that does not exist yet.
            Write-Host "skip (absent): $p"
        }
    }
    return $paths
}

function Get-CurrentExclusions {
    try {
        $pref = Get-MpPreference -ErrorAction SilentlyContinue
        if ($null -ne $pref) { return @($pref.ExclusionPath | Where-Object { $_ }) }
    } catch { }
    return $null
}

Write-Host "=== $ScriptName ==="

# Defender availability check: adding/removing needs the cmdlets.
if (-not (Get-Command Add-MpPreference -ErrorAction SilentlyContinue) -and -not $WhatIf) {
    Write-Host ''
    Write-Host 'Add-MpPreference is not available on this system.'
    Write-Host 'Windows Defender appears to be disabled, removed, or replaced by a third-party AV.'
    Write-Host 'Nothing to do - this is not an error.'
    exit 0
}

$paths = Get-ExclusionPaths

Write-Host ''
Write-Host "Exclusion paths this script manages ($($paths.Count)):"
foreach ($p in $paths) { Write-Host "  $p" }

if ($WhatIf) {
    Write-Host ''
    Write-Host "WhatIf: no changes made. Re-run elevated without -WhatIf to apply."
    Write-Host ''
    Write-Host 'To re-run elevated from a normal shell:'
    $relaunch = '-NoProfile','-ExecutionPolicy','Bypass','-File', $PSCommandPath
    Write-Host ('  Start-Process powershell -Verb RunAs -ArgumentList ''' + ($relaunch -join ''',''') + '''')
    exit 0
}

# Elevation guard for any real change.
if (-not (Test-IsAdmin)) {
    Write-Host ''
    Write-Host 'ERROR: this script must run elevated (as Administrator) to change Defender exclusions.'
    Write-Host 'Re-run it from an elevated PowerShell, or from a normal shell via:'
    $relaunch = '-NoProfile','-ExecutionPolicy','Bypass','-File', $PSCommandPath
    Write-Host ('  Start-Process powershell -Verb RunAs -ArgumentList ''' + ($relaunch -join ''',''') + '''')
    exit 1
}

Write-Host ''
Write-Host 'Current Defender exclusions (before):'
$before = Get-CurrentExclusions
if ($null -eq $before) {
    Write-Host '  (unable to read Get-MpPreference)'
} elseif ($before.Count -eq 0) {
    Write-Host '  (none)'
} else {
    foreach ($p in $before) { Write-Host "  $p" }
}

if ($Remove) {
    Write-Host ''
    Write-Host 'Removing exclusions added by this script...'
    foreach ($p in $paths) {
        if ($before -contains $p) {
            Write-Host "  removing: $p"
            Remove-MpPreference -ExclusionPath $p
        } else {
            Write-Host "  not present, skipping: $p"
        }
    }
} else {
    Write-Host ''
    Write-Host 'Adding exclusions...'
    foreach ($p in $paths) {
        if ($before -contains $p) {
            Write-Host "  already excluded, skipping: $p"
        } else {
            Write-Host "  adding: $p"
            Add-MpPreference -ExclusionPath $p
        }
    }
}

Write-Host ''
Write-Host 'Current Defender exclusions (after):'
$after = Get-CurrentExclusions
if ($null -eq $after) {
    Write-Host '  (unable to read Get-MpPreference)'
} elseif ($after.Count -eq 0) {
    Write-Host '  (none)'
} else {
    foreach ($p in $after) { Write-Host "  $p" }
}

if ($null -ne $before -and $null -ne $after) {
    Write-Host ''
    $added = @($after | Where-Object { $before -notcontains $_ })
    $removedCount = @($before | Where-Object { $after -notcontains $_ }).Count
    Write-Host "Diff: +$($added.Count) added, -$removedCount removed by this run."
}

Write-Host ''
Write-Host 'Done.'
exit 0
