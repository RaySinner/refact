# R-1 verification harness.
# Single in-repo script invocation: no separators, no pipes, no globs.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\r1-check.ps1
$ErrorActionPreference = "Continue"
$py = Join-Path $PSScriptRoot "r1-check.py"
& python $py
$code = $LASTEXITCODE
Write-Output "EXITCODE=$code"
exit $code
