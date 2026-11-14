<#
R-5 verification harness.

Runs the in-repo, target-independent proof that the `src/app_state.rs` split is
mechanical (every base line survives once, the `pub use` surface and the test module
are byte-identical, the children are leaves, the intended islands stay in the parent),
then the engine build gate:

    powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/r5-verify.ps1
    powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/r5-verify.ps1 -SkipBuild
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

Step "r5-check.py (line accounting, verbatim bodies, islands)" {
    python tools/dev/r5-check.py
}

if (-not $SkipBuild) {
    Step "engine-build.ps1 -Mode check" {
        & "$repoRoot/tools/dev/engine-build.ps1"
    }
    if ($IncludeTests) {
        # Informational only: the engine test target carries the documented baseline
        # errors (25x StageJob/StageProduct/now_ms in src/tools/review_agents/mod.rs,
        # 9x IndexingEverywhere/is_valid_file in files_in_workspace/tests.rs,
        # 2x json! in tool_task_spawn_agent.rs), so a non-zero exit here is expected.
        Write-Host ""
        Write-Host "=== engine-build.ps1 -Mode check-tests (informational) ==="
        & "$repoRoot/tools/dev/engine-build.ps1" -Mode check-tests
        Write-Host "check-tests exit code: $LASTEXITCODE (101 = documented baseline)"
        $global:LASTEXITCODE = 0
    }
}

Write-Host ""
if ($failed.Count -gt 0) {
    Write-Host "### R-5 VERIFY: FAIL -> $($failed -join ', ')"
    exit 1
}
Write-Host "### R-5 VERIFY: PASS"
exit 0
