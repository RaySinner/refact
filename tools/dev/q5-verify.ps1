<#
Q-5 verification harness.

Runs the in-repo, target-independent checks that prove the buddy harvest is
mechanical (pure `pub use` shims, crate registration, no crate->engine
back-references, byte-identical production bodies modulo imports/paths), then the
engine build gates the planner agreed on.

    powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/q5-verify.ps1
    powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/q5-verify.ps1 -SkipBuild
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$IncludeTests
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $repoRoot

$failed = @()

function Step {
    param([string]$Name, [scriptblock]$Body)
    Write-Host ""
    Write-Host "=== $Name ==="
    & $Body
    if ($LASTEXITCODE -ne 0) {
        $script:failed += $Name
    }
}

Step "buddy_move_verify.py (shims, registration, no back-references)" {
    python tools/dev/buddy_move_verify.py
}

Step "buddy_body_diff.py (base body vs moved body, normalized)" {
    python tools/dev/buddy_body_diff.py
}

if (-not $SkipBuild) {
    Step "engine-build.ps1 -Mode check" {
        & "$repoRoot/tools/dev/engine-build.ps1"
    }
    if ($IncludeTests) {
        # Informational: the engine test target has pre-existing failures in
        # src/tools/review_agents/mod.rs (plus the 11 documented baseline errors),
        # so a non-zero exit here is expected. Read the log tail, not the code.
        Write-Host ""
        Write-Host "=== engine-build.ps1 -Mode check-tests (informational) ==="
        & "$repoRoot/tools/dev/engine-build.ps1" -Mode check-tests
        Write-Host "check-tests exit code: $LASTEXITCODE (101 = expected baseline)"
        $global:LASTEXITCODE = 0
    }
}

Write-Host ""
if ($failed.Count -gt 0) {
    Write-Host "### Q-5 VERIFY: FAIL -> $($failed -join ', ')"
    exit 1
}
Write-Host "### Q-5 VERIFY: PASS"
exit 0
