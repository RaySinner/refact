# Q-2 verification harness: shell_gate <-> shell_gate_llm cycle broken.
# Single in-repo script invocation: no separators, no pipes, no globs.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\q2-check.ps1
$ErrorActionPreference = "Continue"
$py = Join-Path $PSScriptRoot "q2-check.py"
& python $py
$code = $LASTEXITCODE
Write-Output "EXITCODE=$code"
exit $code
