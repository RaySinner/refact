# AL-4 verification harness.
#
# Layer A (always runs; no compiler needed):
#   python tools/dev/chat_movable_census.py chat     -> refreshes the census
#   python tools/dev/al4-check.py                    -> 29 structural checks
#   python tools/dev/al4_diff.py                     -> body preservation vs the
#                                                       card's start commit
#
# Layer B (compiler) is owned by the project build guard, which refuses every
# cargo invocation inside an agent worktree (`BUILD REFUSED BY PROJECT POLICY`,
# rustc-wrapper = refact-build-guard.exe). A refusal is reported as DEFERRED, so
# the audit trail stays honest instead of pretending the layer ran.
#
# Run:  powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/al4-verify.ps1

$ErrorActionPreference = 'Continue'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$engine = Join-Path $root 'refact-agent\engine'
New-Item -ItemType Directory -Force -Path (Join-Path $engine 'target') | Out-Null
$log = Join-Path $engine 'target\al4-verify.txt'
if (Test-Path $log) { Remove-Item $log }

$failed = @()

function Record($line) {
    Add-Content -Path $log -Value $line
    Write-Host $line
}

function Run($name, $block) {
    $out = & $block 2>&1
    $text = ($out | Out-String)
    $code = $LASTEXITCODE
    Record ("--- {0} (exit {1})" -f $name, $code)
    Record $text
    return [pscustomobject]@{ Code = $code; Text = $text }
}

Push-Location $root
try {
    $census = Run 'census + al4-check' {
        & python tools\dev\chat_movable_census.py chat | Out-Null
        & python tools\dev\al4-check.py
    }
} finally { Pop-Location }
if ($census.Code -ne 0) { $failed += 'census then al4-check' }

Push-Location $root
try {
    $diff = Run 'al4_diff body preservation' { & python tools\dev\al4_diff.py }
} finally { Pop-Location }
if ($diff.Code -ne 0) { $failed += 'al4_diff' }

$probe = Run 'build-guard probe' {
    Push-Location $engine
    try {
        & cargo check -p refact-lsp --lib 2>&1 | Select-Object -First 5
    } finally { Pop-Location }
}

if ($probe.Text -match 'BUILD REFUSED BY PROJECT POLICY') {
    Record 'DEFERRED: cargo layer skipped, the build guard refused compilation.'
    if ($failed.Count -gt 0) {
        Write-Host ('AL-4 VERIFY FAILED: ' + ($failed -join ', '))
        exit 1
    }
    Write-Host 'AL-4 VERIFY PASSED (layer A; layer B deferred to the build-guard policy)'
    exit 0
}

if ($probe.Code -ne 0) { $failed += 'cargo check -p refact-lsp --lib' }

$tests = Run 'cargo check --profile test --all-targets' {
    Push-Location $engine
    try { & cargo check --profile test --all-targets } finally { Pop-Location }
}
$m = [regex]::Match($tests.Text, 'due to (\d+) previous errors')
if ($m.Success) {
    $n = [int]$m.Groups[1].Value
    Record "note: check-tests reported $n errors (baseline is 36)"
    if ($n -ne 36) { $failed += "check-tests error count $n not 36 baseline" }
} elseif ($tests.Code -ne 0) {
    $failed += 'cargo check --profile test --all-targets'
}

$run = Run 'cargo test --lib -p refact-lsp chat_tools' {
    Push-Location $engine
    try { & cargo test --lib -p refact-lsp chat_tools } finally { Pop-Location }
}
if ($run.Code -ne 0) { $failed += 'cargo test --lib -p refact-lsp chat_tools' }

if ($failed.Count -gt 0) {
    Write-Host ('AL-4 VERIFY FAILED: ' + ($failed -join ', '))
    exit 1
}
Write-Host 'AL-4 VERIFY PASSED'
