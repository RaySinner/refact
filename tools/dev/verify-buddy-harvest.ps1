# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/verify-buddy-harvest.ps1
param(
    [switch]$Quiet
)

$ErrorActionPreference = "Stop"
$repo = $PSScriptRoot
while (-not (Test-Path (Join-Path $repo "refact-agent\engine\Cargo.toml"))) {
    $parent = Split-Path $repo -Parent
    if ($parent -eq $repo -or [string]::IsNullOrEmpty($parent)) {
        throw "could not locate the repository root from $PSScriptRoot"
    }
    $repo = $parent
}
$engine = Join-Path $repo "refact-agent\engine"
$buddy = Join-Path $engine "src\buddy"
$core = Join-Path $engine "crates\refact-buddy-core"

$failures = New-Object System.Collections.Generic.List[string]
$checks = New-Object System.Collections.Generic.List[string]

function Check {
    param([string]$Name, [bool]$Ok, [string]$Detail)
    if ($Ok) {
        $checks.Add("PASS  $Name")
    } else {
        $checks.Add("FAIL  $Name :: $Detail")
        $failures.Add("$Name :: $Detail")
    }
}

function Read-Text {
    param([string]$Path)
    return [System.IO.File]::ReadAllText($Path)
}

# --- 1. no crate -> engine dependency (both directions of the seam) ---
$coreManifest = Read-Text (Join-Path $core "Cargo.toml")
Check "buddy-core does not depend on refact-lsp" (-not ($coreManifest -match "refact-lsp")) "refact-buddy-core/Cargo.toml mentions refact-lsp"

$engineManifest = Read-Text (Join-Path $engine "Cargo.toml")
Check "engine depends on refact-buddy-core" ($engineManifest -match "refact-buddy-core") "engine Cargo.toml missing refact-buddy-core"

# --- 2. moved symbols live in buddy-core ---
$coreWorkflows = Read-Text (Join-Path $core "src\workflows.rs")
foreach ($symbol in @("pub fn workflow_failure_summary", "pub fn workflow_failure_report", "pub fn canonical_signal_type")) {
    Check "buddy-core workflows defines $symbol" ($coreWorkflows.Contains($symbol)) "missing from crates/refact-buddy-core/src/workflows.rs"
}

$corePulse = Read-Text (Join-Path $core "src\pulse_inject.rs")
foreach ($symbol in @(
        "pub struct BuddyPulsePayload",
        "pub struct PulsePreference",
        "pub struct PulseLesson",
        "pub struct PulseFriction",
        "pub struct PulseReport",
        "pub struct PulseActivitySection",
        "pub struct PulseActivityGroup",
        "pub const BUDDY_PULSE_MARKER",
        "pub fn pulse_message_from_payload",
        "pub fn render_pulse_as_markdown",
        "pub async fn read_preferences",
        "pub async fn build_recent_reports",
        "pub fn build_activity_section_from_actions",
        "pub fn payload_is_empty",
        "pub fn pulse_friction_from_diagnostics"
    )) {
    Check "buddy-core pulse_inject defines $symbol" ($corePulse.Contains($symbol)) "missing from crates/refact-buddy-core/src/pulse_inject.rs"
}

$coreLib = Read-Text (Join-Path $core "src\lib.rs")
Check "buddy-core declares pulse_inject module" ($coreLib -match "pub mod pulse_inject;") "pub mod pulse_inject missing from lib.rs"

# --- 3. engine side is a shim: only glob re-export + AppState wrappers ---
$engineWorkflows = Read-Text (Join-Path $buddy "workflows.rs")
Check "engine workflows re-exports the core module" ($engineWorkflows -match "pub use refact_buddy_core::workflows::\*;") "glob re-export missing"
foreach ($gone in @("pub fn workflow_failure_summary", "pub fn workflow_failure_report", "pub fn canonical_signal_type")) {
    Check "engine workflows no longer defines $gone" (-not $engineWorkflows.Contains($gone)) "duplicate definition left behind"
}

$enginePulse = Read-Text (Join-Path $buddy "pulse_inject.rs")
Check "engine pulse_inject re-exports the core module" ($enginePulse -match "pub use refact_buddy_core::pulse_inject::\*;") "glob re-export missing"
foreach ($gone in @("pub struct BuddyPulsePayload", "pub struct PulseFriction", "pub fn render_pulse_as_markdown", "pub fn read_preferences")) {
    Check "engine pulse_inject no longer defines $gone" (-not $enginePulse.Contains($gone)) "duplicate definition left behind"
}

$wrapperCount = ([regex]::Matches($enginePulse, "pub async fn build_buddy_pulse")).Count
Check "engine keeps exactly the two AppState builders" ($wrapperCount -eq 2) "found $wrapperCount build_buddy_pulse entry points"

# --- 4. every engine-visible path still resolves through the globs ---
$engineConsumers = @(
    "src\buddy\background.rs",
    "src\buddy\scheduler.rs",
    "src\buddy\jobs\autonomous_chats\runtime.rs",
    "src\http\routers\v1\buddy.rs",
    "src\chat\prompts.rs",
    "src\app_state.rs"
)
foreach ($relative in $engineConsumers) {
    $text = Read-Text (Join-Path $engine $relative)
    $usesWorkflows = $text -match "workflow_failure_summary|workflow_failure_report|canonical_signal_type|WorkflowFailureReport|WorkflowFailureCategory"
    if ($usesWorkflows) {
        Check "$relative can still resolve workflow helpers" ((Read-Text (Join-Path $buddy "workflows.rs")).Contains("pub use refact_buddy_core::workflows::*;")) "no glob re-export for $relative"
    }
    $usesPulse = $text -match "build_buddy_pulse_payload|build_buddy_pulse_message|BUDDY_PULSE_MARKER"
    if ($usesPulse) {
        Check "$relative can still resolve pulse builders" (((Read-Text (Join-Path $buddy "pulse_inject.rs")).Contains("pub async fn build_buddy_pulse_payload"))) "pulse builder missing for $relative"
    }
}

# --- 5. tests moved verbatim (names preserved on the crate side) ---
Check "core workflows test workflow_failure_report_classifies_model_tool_and_cancellation_errors present" ($coreWorkflows.Contains("fn workflow_failure_report_classifies_model_tool_and_cancellation_errors")) "test not moved to buddy-core"
Check "engine workflows no longer defines that test" (-not $engineWorkflows.Contains("workflow_failure_report_classifies_model_tool_and_cancellation_errors")) "test duplicated in the engine shim"
foreach ($testName in @(
        "pulse_under_2000_chars_when_full",
        "preferences_exclude_low_confidence_and_keep_high",
        "pulse_message_uses_correct_marker",
        "pulse_payload_serializes_in_extra_map",
        "activity_section_groups_actions_and_reports_pattern",
        "empty_payload_is_detected",
        "friction_ranks_recent_diagnostics_and_counts_pulse_types"
    )) {
    Check "core test $testName present" ($corePulse.Contains("async fn $testName") -or $corePulse.Contains("fn $testName")) "test not moved to buddy-core"
}
foreach ($testName in @(
        "pulse_returns_none_when_empty",
        "pulse_excludes_low_confidence_prefs",
        "pulse_includes_user_activity_section_with_pattern",
        "pulse_message_uses_correct_marker",
        "pulse_payload_serializes_in_extra_map"
    )) {
    Check "engine test $testName still present" ($enginePulse.Contains("async fn $testName")) "AppState-bound test dropped"
}

# --- 6. scope guards: excluded code stayed in the engine ---
$excluded = @(
    "src\buddy\memory_lifecycle.rs",
    "src\buddy\jobs\autonomous_chats\mod.rs",
    "src\buddy\jobs\autonomous_chats\runtime.rs",
    "src\buddy\jobs\autonomous_chats\spec.rs",
    "src\buddy\jobs\autonomous_chats\tests.rs"
)
foreach ($relative in $excluded) {
    Check "engine still owns $relative" (Test-Path (Join-Path $engine $relative)) "excluded file left the engine"
}
$coreMirrors = Get-ChildItem -Recurse -File $core -Filter *.rs | Where-Object {
    $_.Name -eq "memory_lifecycle.rs" -or $_.FullName -match "autonomous_chats"
}
Check "buddy-core holds no memory_lifecycle/autonomous_chats copy" ($coreMirrors.Count -eq 0) "excluded code duplicated into buddy-core"

if (-not $Quiet) {
    $checks | ForEach-Object { Write-Output $_ }
    Write-Output ""
}

if ($failures.Count -gt 0) {
    Write-Output "RESULT: FAIL ($($failures.Count) of $($checks.Count) checks)"
    exit 1
}
Write-Output "RESULT: PASS ($($checks.Count) checks)"
exit 0
