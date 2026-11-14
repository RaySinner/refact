# P-1 verification harness.
# Single in-repo script invocation: no separators, no pipes, no globs.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\p1-check.ps1
$ErrorActionPreference = "Continue"
$py = Join-Path $PSScriptRoot "p1-check.py"
& python $py
$code = $LASTEXITCODE
Write-Output "EXITCODE=$code"
exit $code
