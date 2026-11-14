# P-2 acceptance harness: chat::post_merge_check bodies extracted to refact-chat-api / refact-runtime-api.
# Single-file invocation only (agent verifiers reject multi-part shell commands):
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/p2-check.ps1
$ErrorActionPreference = "Continue"
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$engine = Join-Path $repo "refact-agent\engine"
$failed = 0

function Check($name, $ok, $detail) {
    if ($ok) {
        Write-Output ("PASS  {0}" -f $name)
    } else {
        Write-Output ("FAIL  {0}  {1}" -f $name, $detail)
        $script:failed = 1
    }
}

# 1. no tools file references the old engine runner module.
$hits = & git -C $repo grep -n "post_merge_check::runner" -- "refact-agent/engine/src/tools"
Check "grep -rn post_merge_check::runner src/tools is empty" (-not $hits) ($hits -join " | ")

# 2. no engine file references crate::chat::post_merge_check any more.
$hits = & git -C $repo grep -n "crate::chat::post_merge_check" -- "refact-agent/engine/src"
Check "no crate::chat::post_merge_check in engine src" (-not $hits) ($hits -join " | ")

# 3. the engine module is a shim: no local bodies left.
$modText = Get-Content (Join-Path $engine "src\chat\post_merge_check\mod.rs") -Raw
Check "mod.rs is a pub use shim" ($modText -match "pub use refact_runtime_api::post_merge_check::\*")
Check "policy.rs is gone" (-not (Test-Path (Join-Path $engine "src\chat\post_merge_check\policy.rs")))
Check "runner.rs is gone" (-not (Test-Path (Join-Path $engine "src\chat\post_merge_check\runner.rs")))

# 4. the crate module owns the moved bodies.
$crateText = Get-Content (Join-Path $engine "crates\refact-runtime-api\src\post_merge_check.rs") -Raw
Check "crate owns SystemPostMergeCommandRunner" ($crateText -match "struct SystemPostMergeCommandRunner")
Check "crate owns post_merge_check_with_runner" ($crateText -match "pub async fn post_merge_check_with_runner")
Check "crate owns store_regression_result" ($crateText -match "async fn store_regression_result")
Check "crate re-exports the timeout const" ($crateText -match "DEFAULT_POST_MERGE_CHECK_TIMEOUT_SECS")
Check "crate body has no engine path" (-not ($crateText -match "crate::chat::"))

# 5. no crate -> engine dependency in either direction.
$engineToml = Get-Content (Join-Path $engine "Cargo.toml") -Raw
Check "engine depends on refact-chat-api" ($engineToml -match "refact-chat-api = \{ path = ")
Check "engine depends on refact-runtime-api" ($engineToml -match "refact-runtime-api = \{ path = ")
$chatApiToml = Get-Content (Join-Path $engine "crates\refact-chat-api\Cargo.toml") -Raw
Check "chat-api does not depend on refact-lsp" (-not ($chatApiToml -match "refact-lsp"))
$runtimeApiToml = Get-Content (Join-Path $engine "crates\refact-runtime-api\Cargo.toml") -Raw
Check "runtime-api does not depend on refact-lsp" (-not ($runtimeApiToml -match "refact-lsp"))

# 6. parse/verification-command logic is crate-resident.
$verifyCrate = Get-Content (Join-Path $engine "crates\refact-chat-api\src\verify_cmd.rs") -Raw
Check "chat-api owns parse_verification_argv" ($verifyCrate -match "pub fn parse_verification_argv")
Check "chat-api owns verification_commands" ($verifyCrate -match "pub fn verification_commands")
Check "chat-api verify_cmd has no engine path" (-not ($verifyCrate -match "crate::tools::"))

# 7. the two blocked tool files, with production-line counts.
foreach ($tool in @("tool_task_merge_agent", "tool_task_batch")) {
    $path = Join-Path $engine ("src\tools\{0}.rs" -f $tool)
    $lines = (Get-Content $path | Measure-Object -Line).Lines
    Check ("{0}.rs present ({1} lines)" -f $tool, $lines) ($lines -gt 0) "missing"
}

# 8. honest census: both modules must now report zero blocked production lines.
& python (Join-Path $repo "tools\dev\chat_movable_census.py") "tools" | Out-Null
$censusText = Get-Content (Join-Path $engine "target\tools_movable_census.txt") -Raw
Check "census core numbers present" ($censusText -match "### tools: \d+ files / \d+ production lines") "census text unreadable"
Check "census no longer blocks tool_task_merge_agent" (-not ($censusText -match "tool_task_merge_agent\s+blocker="))
Check "census no longer blocks tool_task_batch" (-not ($censusText -match "tool_task_batch\s+blocker="))

Write-Output ""
if ($failed -eq 0) {
    Write-Output "P-2 HARNESS: ALL CHECKS PASSED"
    exit 0
}
Write-Output "P-2 HARNESS: FAILURES PRESENT"
exit 1
