# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools/dev/verify-buddy-harvest-2.ps1
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
$obs = Join-Path $buddy "observers"
$core = Join-Path $engine "crates\refact-buddy-core"
$coreObs = Join-Path $core "src\observers"

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

# --- 1. seam direction: no crate -> engine dependency ---
$coreManifest = Read-Text (Join-Path $core "Cargo.toml")
Check "buddy-core does not depend on refact-lsp" (-not ($coreManifest -match "refact-lsp")) "refact-buddy-core/Cargo.toml mentions refact-lsp"
Check "buddy-core no longer depends on refact-integrations" (-not ($coreManifest -match "refact-integrations")) "observers move must not add an integrations edge"
Check "buddy-core declares the refact-caps-core edge" ($coreManifest -match "refact-caps-core") "provider_health needs DefaultModels"
Check "buddy-core declares the strsim edge" ($coreManifest -match "strsim") "task_health needs strsim::normalized_levenshtein"

$coreLib = Read-Text (Join-Path $core "src\lib.rs")
Check "buddy-core declares the observers module" ($coreLib -match "pub mod observers;") "pub mod observers missing from lib.rs"

$engineManifest = Read-Text (Join-Path $engine "Cargo.toml")
Check "engine still depends on refact-buddy-core" ($engineManifest -match "refact-buddy-core") "engine Cargo.toml missing refact-buddy-core"

# --- 2. moved pure logic lives in buddy-core ---
$moved = [ordered]@{
    "observers\mod.rs"                = @("pub struct ObserverContext", "pub struct Ephemeral<T>")
    "observers\chat_pattern.rs"       = @("pub fn count_retry_streak", "pub fn detect_chat_pattern_facts")
    "observers\diagnostic_cluster.rs" = @("pub fn detect_diagnostic_cluster_facts", "pub fn diagnostic_source_bucket")
    "observers\provider_health.rs"    = @("pub fn detect_provider_health_facts")
    "observers\task_health.rs"        = @("pub const MAX_TASK_CLUSTER_ENTRIES", "pub struct TaskHealthEntry", "pub fn detect_task_health_facts", "pub async fn detect_orphaned_task_memory_facts")
    "observers\trajectory_clutter.rs" = @("pub const MAX_TRAJECTORY_SCAN_FILES", "pub async fn scan_trajectories_dir", "pub fn detect_trajectory_clutter_facts", "pub fn path_hash")
}
foreach ($relative in $moved.Keys) {
    $path = Join-Path $coreObs (Split-Path $relative -Leaf)
    if (-not (Test-Path $path)) {
        Check "buddy-core owns $relative" $false "file missing"
        continue
    }
    $text = Read-Text $path
    foreach ($symbol in $moved[$relative]) {
        Check "buddy-core $relative defines $symbol" ($text.Contains($symbol)) "missing symbol"
    }
}

# build_observer_registry intentionally stays engine-side (it wires AppState-bound observers)
$coreModText = Read-Text (Join-Path $coreObs "mod.rs")
Check "buddy-core observers/mod.rs is not the registry" (-not $coreModText.Contains("build_observer_registry")) "registry must stay in the engine"

$engineObsMod = Read-Text (Join-Path $obs "mod.rs")
Check "engine observers/mod.rs keeps the registry" ($engineObsMod.Contains("pub fn build_observer_registry")) "registry moved away"
Check "engine observers/mod.rs re-exports ObserverContext" ($engineObsMod -match "pub use refact_buddy_core::observers::\{[^}]*ObserverContext") "ObserverContext must be re-exported for every observer impl"
Check "engine observers/mod.rs re-exports Ephemeral" ($engineObsMod -match "pub use refact_buddy_core::observers::\{[^}]*Ephemeral") "Ephemeral must be re-exported"

# --- 3. every engine observer file is a shim: glob re-export + trait impl only ---
$shims = [ordered]@{
    "chat_pattern.rs"       = @("pub use refact_buddy_core::observers::chat_pattern::*;", "pub struct ChatPatternObserver")
    "diagnostic_cluster.rs" = @("pub use refact_buddy_core::observers::diagnostic_cluster::*;", "pub struct DiagnosticClusterObserver")
    "provider_health.rs"    = @("pub use refact_buddy_core::observers::provider_health::*;", "pub struct ProviderHealthObserver")
    "task_health.rs"        = @("pub use refact_buddy_core::observers::task_health::*;", "pub struct TaskHealthObserver")
    "trajectory_clutter.rs" = @("pub use refact_buddy_core::observers::trajectory_clutter::*;", "pub struct TrajectoryClutterObserver")
}
foreach ($file in $shims.Keys) {
    $text = Read-Text (Join-Path $obs $file)
    foreach ($needle in $shims[$file]) {
        Check "engine observers/$file contains '$needle'" ($text.Contains($needle)) "shim shape missing"
    }
    Check "engine observers/$file still implements the trait" ($text.Contains("impl BuddyObserver for")) "observer impl removed"
}
Check "engine observers/diagnostic_cluster.rs no longer defines pure logic" (-not (Read-Text (Join-Path $obs "diagnostic_cluster.rs")).Contains("pub fn detect_diagnostic_cluster_facts")) "duplicate definition left behind"
Check "engine observers/task_health.rs no longer defines pure logic" (-not (Read-Text (Join-Path $obs "task_health.rs")).Contains("pub fn detect_task_health_facts")) "duplicate definition left behind"
Check "engine observers/trajectory_clutter.rs no longer defines pure logic" (-not (Read-Text (Join-Path $obs "trajectory_clutter.rs")).Contains("pub fn scan_trajectories_dir")) "duplicate definition left behind"
Check "engine observers/provider_health.rs no longer defines pure logic" (-not (Read-Text (Join-Path $obs "provider_health.rs")).Contains("pub fn detect_provider_health_facts")) "duplicate definition left behind"
Check "engine observers/chat_pattern.rs no longer defines pure logic" (-not (Read-Text (Join-Path $obs "chat_pattern.rs")).Contains("pub fn count_retry_streak")) "duplicate definition left behind"

# --- 4. every consumer path in the engine still resolves ---
$consumers = @(
    "src\buddy\background.rs",
    "src\buddy\pulse.rs",
    "src\buddy\tests\chat_reactions_and_pulse.rs",
    "src\buddy\tests\issues_diagnostics_events.rs",
    "src\buddy\tests\opportunity_resolution.rs",
    "src\buddy\tests\policy_humor_observers.rs",
    "src\buddy\tests\task_health_and_investigations.rs",
    "src\buddy\tests\tools_routes_and_privacy.rs"
)
foreach ($relative in $consumers) {
    $text = Read-Text (Join-Path $engine $relative)
    if ($text -match "observers::trajectory_clutter::") {
        Check "$relative still resolves trajectory_clutter" ((Read-Text (Join-Path $obs "trajectory_clutter.rs")).Contains("pub use refact_buddy_core::observers::trajectory_clutter::*;")) "no glob re-export"
    }
    if ($text -match "observers::task_health::") {
        Check "$relative still resolves task_health" ((Read-Text (Join-Path $obs "task_health.rs")).Contains("pub use refact_buddy_core::observers::task_health::*;")) "no glob re-export"
    }
    if ($text -match "observers::provider_health::") {
        Check "$relative still resolves provider_health" ((Read-Text (Join-Path $obs "provider_health.rs")).Contains("pub use refact_buddy_core::observers::provider_health::*;")) "no glob re-export"
    }
    if ($text -match "observers::diagnostic_cluster::") {
        Check "$relative still resolves diagnostic_cluster" ((Read-Text (Join-Path $obs "diagnostic_cluster.rs")).Contains("pub use refact_buddy_core::observers::diagnostic_cluster::*;")) "no glob re-export"
    }
    if ($text -match "observers::chat_pattern::") {
        Check "$relative still resolves chat_pattern" ((Read-Text (Join-Path $obs "chat_pattern.rs")).Contains("pub use refact_buddy_core::observers::chat_pattern::*;")) "no glob re-export"
    }
    if ($text -match "observers::(Ephemeral|ObserverContext|BuddyObserver)") {
        Check "$relative still resolves observers::mod symbols" ($engineObsMod -match "pub use refact_buddy_core::observers::\{") "no re-export of ObserverContext/Ephemeral"
    }
}

# --- 5. inline tests moved verbatim (names preserved on the crate side) ---
$movedTests = @{
    "observers\diagnostic_cluster.rs" = @(
        "frontend_diagnostic_predicate_matches_frontend_shapes",
        "diagnostic_cluster_key_includes_source_bucket",
        "diagnostic_cluster_key_includes_model_bucket"
    )
    "observers\task_health.rs" = @("detects_orphaned_card_scoped_task_memory")
}
foreach ($relative in $movedTests.Keys) {
    $coreText = Read-Text (Join-Path $coreObs (Split-Path $relative -Leaf))
    $engineText = Read-Text (Join-Path $obs (Split-Path $relative -Leaf))
    foreach ($testName in $movedTests[$relative]) {
        Check "core $relative test $testName present" ($coreText.Contains("fn $testName")) "test not moved"
        Check "engine $relative test $testName removed" (-not $engineText.Contains("fn $testName")) "test duplicated in the engine shim"
    }
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

foreach ($name in @("TaskHealthObserver", "TrajectoryClutterObserver", "ProviderHealthObserver", "DiagnosticClusterObserver", "ChatPatternObserver")) {
    $coreHits = Select-String -Path (Join-Path $coreObs "*.rs") -Pattern ("struct " + $name) -SimpleMatch -ErrorAction SilentlyContinue
    Check "buddy-core does not define $name" ($null -eq $coreHits -or $coreHits.Count -eq 0) "AppState-bound observer leaked into buddy-core"
}

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
