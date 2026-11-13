# L-3 verification harness: the four cheap blockers are cleared.
#
#  Q-1 `ToolAgentHandle` alias            -> refact_runtime_api::tool_agent   (1 line)
#  Q-2 `integrations::load_integrations`  -> refact_runtime_api::integrations_runtime
#      (+ IntegrationRecord / read_integrations_d / join_config_path /
#         get_config_dirs / split_path_into_project_and_integration)
#  Q-3 `chat::trajectories::{find_trajectory_path, save_trajectory_snapshot, set_trajectory_label}`
#      -> refact_runtime_api::trajectory_runtime
#
# Usage (identical file, either location, no pipes and no separators needed):
#   powershell -NoProfile -ExecutionPolicy Bypass -File refact-agent\engine\target\l3-check.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\l3-check.ps1
$ErrorActionPreference = "Stop"
$fail = 0
$checks = 0

function Assert-True([bool]$cond, [string]$name) {
    $script:checks++
    if ($cond) { Write-Output "OK   $name" }
    else { Write-Output "FAIL $name"; $script:fail++ }
}

# Locate <repo>/refact-agent/engine by walking up from this script, so the harness works
# from refact-agent/engine/target/ and from tools/dev/ without editing.
$probe = $PSScriptRoot
$engine = $null
for ($i = 0; $i -lt 8 -and $probe; $i++) {
    $candidate = Join-Path $probe "refact-agent\engine"
    if (Test-Path (Join-Path $candidate "src\integrations\running_integrations.rs")) { $engine = $candidate; break }
    $candidate2 = Join-Path $probe "engine"
    if (Test-Path (Join-Path $candidate2 "src\integrations\running_integrations.rs")) { $engine = $candidate2; break }
    $probe = Split-Path $probe -Parent
}
if (-not $engine) { Write-Output "FAIL cannot locate refact-agent/engine"; exit 1 }
Assert-True $true "located engine at $engine"

function Read-Text([string]$relative) {
    return [IO.File]::ReadAllText((Join-Path $engine $relative))
}

$apiCrate = Join-Path $engine "crates\refact-runtime-api"
$apiToml = Join-Path $apiCrate "Cargo.toml"
$integrationsToml = Join-Path $engine "crates\refact-integrations\Cargo.toml"
$historyToml = Join-Path $engine "crates\refact-chat-history\Cargo.toml"

$apiToolAgent = [IO.File]::ReadAllText((Join-Path $apiCrate "src\tool_agent.rs"))
$apiIntegrations = [IO.File]::ReadAllText((Join-Path $apiCrate "src\integrations_runtime.rs"))
$apiTrajectory = [IO.File]::ReadAllText((Join-Path $apiCrate "src\trajectory_runtime.rs"))
$apiLib = [IO.File]::ReadAllText((Join-Path $apiCrate "src\lib.rs"))
$apiFactory = [IO.File]::ReadAllText((Join-Path $apiCrate "src\context_factory.rs"))
$apiGcxHelpers = [IO.File]::ReadAllText((Join-Path $apiCrate "src\gcx_helpers.rs"))

# ---------------------------------------------------------------- R1 dependency direction
$apiTomlText = [IO.File]::ReadAllText($apiToml)
Assert-True (-not ($apiTomlText -match "refact-lsp")) "runtime-api Cargo.toml does not mention refact-lsp"
Assert-True (-not ($apiTomlText -match 'path\s*=\s*"\.\./\.\.')) "runtime-api Cargo.toml has no path dep on the engine crate"
Assert-True ($apiTomlText -match 'name = "refact-runtime-api"') "manifest is refact-runtime-api"

$integrationsTomlText = [IO.File]::ReadAllText($integrationsToml)
Assert-True (-not ($integrationsTomlText -match "refact-lsp")) "refact-integrations Cargo.toml does not mention refact-lsp"
Assert-True (-not ($integrationsTomlText -match 'path\s*=\s*"\.\./\.\.')) "refact-integrations Cargo.toml has no path dep on the engine crate"

$historyTomlText = [IO.File]::ReadAllText($historyToml)
Assert-True (-not ($historyTomlText -match "refact-lsp")) "refact-chat-history Cargo.toml does not mention refact-lsp"
Assert-True (-not ($historyTomlText -match 'path\s*=\s*"\.\./\.\.')) "refact-chat-history Cargo.toml has no path dep on the engine crate"

# ---------------------------------------------------------------- Q-1 the ToolAgentHandle alias
Assert-True ($apiLib -match "(?m)^pub mod tool_agent;") "runtime-api lib.rs declares tool_agent"
Assert-True ($apiToolAgent -match 'pub type ToolAgentHandle = refact_agents_core::tool_agent::ToolAgentHandle<crate::app_state::AppState>;') "alias lives in refact-runtime-api and names crate AppState"
Assert-True ($apiToolAgent -match 'pub use refact_agents_core::tool_agent::ToolAgentSpec;') "ToolAgentSpec stays re-exported from the alias module"

$engineToolAgent = Read-Text "src\agents\tool_agent.rs"
Assert-True ($engineToolAgent -match 'pub use refact_runtime_api::tool_agent::\{ToolAgentHandle, ToolAgentSpec\};') "engine tool_agent.rs is a pure re-export"
Assert-True (-not ($engineToolAgent -match 'pub type ToolAgentHandle')) "engine no longer declares the alias"
Assert-True (-not ($engineToolAgent -match 'refact_agents_core::tool_agent::ToolAgentHandle')) "engine no longer names the agents-core generic handle"
Assert-True ($engineToolAgent -match 'ToolAgentHandle::begin\(') "engine tests still exercise the handle behaviour"
Assert-True (([regex]::Matches($engineToolAgent, '#\[(?:tokio::)?test\]')).Count -eq 8) "engine tool_agent tests are intact"

# ---------------------------------------------------------------- Q-2 integrations loader
Assert-True ($apiLib -match "(?m)^pub mod integrations_runtime;") "runtime-api lib.rs declares integrations_runtime"
foreach ($sym in @("find_trajectory_path", "save_trajectory_snapshot", "set_trajectory_label")) {
    Assert-True ($apiTrajectory -match ("pub async fn " + [regex]::Escape($sym) + "\(")) "trajectory_runtime declares $sym"
}

foreach ($sym in @("pub trait IntegrationProvider", "pub struct IntegrationRecord", "pub fn read_integrations_d",
                   "pub fn join_config_path", "pub async fn get_config_dirs",
                   "pub fn split_path_into_project_and_integration", "pub async fn load_integrations")) {
    Assert-True ($apiIntegrations -match [regex]::Escape($sym)) "integrations_runtime declares $sym"
}
Assert-True ($apiIntegrations -match 'gcx\.context_factory\.get\(\)\.integration_provider\(\)') "crate loader reaches the engine through the ContextFactory seam"
Assert-True (-not ($apiIntegrations -match 'crate::integrations::')) "crate module has no engine integrations path"
Assert-True (-not ($apiIntegrations -match 'crate::app_state::AppState')) "crate module does not depend on AppState"

Assert-True ($apiFactory -match 'fn integration_provider\(&self\) -> Arc<dyn crate::integrations_runtime::IntegrationProvider>;') "ContextFactory exposes integration_provider"

$engineRunning = Read-Text "src\integrations\running_integrations.rs"
Assert-True ($engineRunning -match 'pub struct EngineIntegrationProvider;') "engine provides EngineIntegrationProvider"
Assert-True (-not ($engineRunning -match 'fn setup_integration')) "engine sink implementation is gone"
Assert-True (-not ($engineRunning -match 'IntegrationMapSink')) "engine sink struct is gone"
Assert-True (-not ($engineRunning -match 'read_integrations_d')) "engine loader body is gone"
Assert-True ($engineRunning -match 'pub use refact_runtime_api::integrations_runtime::load_integrations;') "engine re-exports the crate loader at the identical path"

$engineSettingUp = Read-Text "src\integrations\setting_up_integrations.rs"
Assert-True (-not ($engineSettingUp -match 'fn read_integrations_d')) "engine no longer defines read_integrations_d"
Assert-True (-not ($engineSettingUp -match 'fn get_config_dirs')) "engine no longer defines get_config_dirs"
Assert-True (-not ($engineSettingUp -match 'fn join_config_path')) "engine no longer defines join_config_path"
Assert-True (-not ($engineSettingUp -match 'fn split_path_into_project_and_integration')) "engine no longer defines split_path_into_project_and_integration"
Assert-True (-not ($engineSettingUp -match 'pub struct IntegrationRecord \{')) "engine no longer defines IntegrationRecord"
Assert-True ($engineSettingUp -match 'pub use refact_runtime_api::integrations_runtime::\{') "engine re-exports the moved helpers"
Assert-True ($engineSettingUp -match 'pub struct IntegrationResult \{') "IntegrationResult stays in the engine"
Assert-True ($engineSettingUp -match 'preserve_unknown_fields_restores_omitted_tokens') "engine integration tests are intact"

$engineAppState = Read-Text "src\app_state.rs"
Assert-True ($engineAppState -match 'fn integration_provider\(') "EngineContextFactory installs the provider"
Assert-True ($engineAppState -match 'EngineIntegrationProvider') "factory returns the engine provider"

# ---------------------------------------------------------------- Q-3 trajectory functions
$enginePaths = Read-Text "src\chat\trajectories\paths.rs"
Assert-True (-not ($enginePaths -match 'pub async fn find_trajectory_path\(')) "engine paths.rs no longer defines find_trajectory_path"
Assert-True ($enginePaths -match 'pub use refact_runtime_api::trajectory_runtime::find_trajectory_path;') "engine paths.rs re-exports find_trajectory_path"
Assert-True ($enginePaths -match 'fn find_trajectory_file\(') "engine keeps its private find_trajectory_file"

$engineStore = Read-Text "src\chat\trajectories\store.rs"
Assert-True (-not ($engineStore -match 'pub async fn save_trajectory_snapshot\(')) "engine store.rs no longer defines save_trajectory_snapshot"
Assert-True ($engineStore -match 'pub use refact_runtime_api::trajectory_runtime::save_trajectory_snapshot;') "engine store.rs re-exports save_trajectory_snapshot"
Assert-True (-not ($engineStore -match 'save_trajectory_snapshot_for_gcx')) "old gcx_helpers name is gone from the engine"

$engineLoad = Read-Text "src\chat\trajectories\load.rs"
Assert-True (-not ($engineLoad -match 'pub async fn set_trajectory_label\(')) "engine load.rs no longer defines set_trajectory_label"
Assert-True ($engineLoad -match 'pub use refact_runtime_api::trajectory_runtime::set_trajectory_label;') "engine load.rs re-exports set_trajectory_label"

$engineMod = Read-Text "src\chat\trajectories\mod.rs"
Assert-True ($engineMod -match 'find_trajectory_path,') "trajectories/mod.rs still re-exports find_trajectory_path"
Assert-True ($engineMod -match 'save_trajectory_snapshot,') "trajectories/mod.rs still re-exports save_trajectory_snapshot"
Assert-True ($engineMod -match 'set_trajectory_label,') "trajectories/mod.rs still re-exports set_trajectory_label"

$engineGcxHelpers = Read-Text "crates\refact-runtime-api\src\gcx_helpers.rs"
Assert-True ($engineGcxHelpers -match 'pub async fn save_trajectory_snapshot\(') "gcx helper keeps the crate-side snapshot writer without the _for_gcx suffix"
Assert-True (-not ($engineGcxHelpers -match 'save_trajectory_snapshot_for_gcx')) "the suffixed helper name is gone"

# ---------------------------------------------------------------- Q-1..Q-3 consumers unchanged
$engineIntegMod = Read-Text "src\integrations\mod.rs"
Assert-True ($engineIntegMod -match 'pub use running_integrations::load_integrations;') "integrations/mod.rs keeps its public re-export"
$engineToolsList = Read-Text "src\tools\tools_list.rs"
Assert-True ($engineToolsList -match 'use crate::integrations::load_integrations;') "tools_list import line is unchanged"
$engineToolTraj = Read-Text "src\tools\tool_trajectory_context.rs"
Assert-True ($engineToolTraj -match 'crate::chat::trajectories::find_trajectory_path') "tool_trajectory_context import line is unchanged"
$engineSetLabel = Read-Text "src\tools\tool_set_trajectory_label.rs"
Assert-True ($engineSetLabel -match 'use crate::chat::trajectories::\{set_trajectory_label, TRAJECTORY_LABEL_MAX_CHARS\};') "tool_set_trajectory_label import line is unchanged"
$engineToolReview = Read-Text "src\tools\tool_review.rs"
Assert-True ($engineToolReview -match 'use crate::agents::tool_agent::ToolAgentHandle;') "tool_review import line is unchanged"
$engineHandoff = Read-Text "src\tools\tool_handoff_to_mode.rs"
Assert-True ($engineHandoff -match 'crate::chat::trajectories::save_trajectory_snapshot') "tool_handoff_to_mode call sites are unchanged"

# acceptance criterion: engine-side greps show only re-exports
$apiTrajectoryText = $apiTrajectory
Assert-True (-not ($apiTrajectoryText -match 'TRAJECTORY_LABEL_MAX_CHARS\s*:\s*usize')) "label length const is not duplicated into runtime-api"
$historyMetrics = [IO.File]::ReadAllText((Join-Path $engine "crates\refact-chat-history\src\trajectory_metrics.rs"))
Assert-True ($historyMetrics -match 'pub const TRAJECTORY_LABEL_MAX_CHARS') "refact-chat-history still owns TRAJECTORY_LABEL_MAX_CHARS"

Write-Output ""
Write-Output "checks=$checks failures=$fail"
if ($fail -ne 0) { exit 1 }
exit 0
