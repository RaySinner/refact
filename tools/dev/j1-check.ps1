# J-1 verification harness: the integrations::mcp::mcp_auth body lives in refact-integrations;
# the engine keeps only the gcx-bound mcp_oauth_refresh_task behind a pub-use shim.
#
# Usage (identical file, either location, no pipes and no separators needed):
#   powershell -NoProfile -ExecutionPolicy Bypass -File refact-agent\engine\target\j1-check.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\j1-check.ps1
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
    if (Test-Path (Join-Path $candidate "src\integrations\mcp\mcp_auth.rs")) { $engine = $candidate; break }
    $candidate2 = Join-Path $probe "engine"
    if (Test-Path (Join-Path $candidate2 "src\integrations\mcp\mcp_auth.rs")) { $engine = $candidate2; break }
    $probe = Split-Path $probe -Parent
}
if (-not $engine) { Write-Output "FAIL cannot locate refact-agent/engine"; exit 1 }
Assert-True $true "located engine at $engine"

$src = Join-Path $engine "src/integrations/mcp/mcp_auth.rs"
$crateDir = Join-Path $engine "crates/refact-integrations"
$crate = Join-Path $crateDir "src/mcp/mcp_auth.rs"
$toml = Join-Path $crateDir "Cargo.toml"

$shimText = [IO.File]::ReadAllText($src)
$crateText = [IO.File]::ReadAllText($crate)
$tomlText = [IO.File]::ReadAllText($toml)

# R1 dependency direction: the crate must never point back at the engine binary crate
Assert-True (-not ($tomlText -match "refact-lsp")) "Cargo.toml does not mention refact-lsp"
Assert-True (-not ($tomlText -match 'path\s*=\s*"\.\./\.\.')) "Cargo.toml has no path dep on the engine crate"
Assert-True ($tomlText -match 'name = "refact-integrations"') "Cargo.toml is the refact-integrations manifest"

# R2 the engine file is a shim
Assert-True ($shimText -match 'pub use refact_integrations::mcp::mcp_auth::\*;') "engine file re-exports the crate module"
Assert-True (-not ($shimText -match 'pub struct MCPAuthSettings')) "MCPAuthSettings is gone from the engine"
Assert-True (-not ($shimText -match 'pub struct MCPOAuthTokens')) "MCPOAuthTokens is gone from the engine"
Assert-True (-not ($shimText -match 'pub struct MCPTokenManager')) "MCPTokenManager is gone from the engine"
Assert-True (-not ($shimText -match 'fn probe_mcp_auth')) "probe_mcp_auth is gone from the engine"
Assert-True (-not ($shimText -match 'fn save_tokens_to_config')) "save_tokens_to_config is gone from the engine"
Assert-True (-not ($shimText -match 'fn load_tokens_from_config')) "load_tokens_from_config is gone from the engine"
Assert-True (-not ($shimText -match 'fn clear_tokens_from_config')) "clear_tokens_from_config is gone from the engine"
Assert-True (-not ($shimText -match 'mod tests')) "no test module left behind in the engine shim"

# R3 the engine keeps exactly the gcx-bound entry point, verbatim
$defs = [regex]::Matches($shimText, "(?m)^\s*(?:pub )?(?:async )?fn\s+\w+")
Assert-True ($defs.Count -eq 1) "engine shim defines exactly one function"
Assert-True ($shimText -match 'pub async fn mcp_oauth_refresh_task\(') "mcp_oauth_refresh_task kept with its signature"
Assert-True ($shimText -match 'downcast_mut::<SessionMCP>\(\)') "refresh task still downcasts to engine SessionMCP"
Assert-True ($shimText -match 'use super::session_mcp::\{SessionMCP, MCPAuthStatus\};') "refresh task keeps its engine session import"
Assert-True ($shimText -match 'REFRESH_RETRY_BASE_SECS') "refresh task reaches shared constants through the glob"
Assert-True ($shimText -match 'REFRESH_RETRY_MAX_SECS') "refresh task reaches the backoff cap through the glob"
Assert-True ($shimText -match 'tokens_from_response\(') "refresh task reaches tokens_from_response through the glob"
Assert-True ($shimText -match 'is_permanent_oauth_error\(') "refresh task reaches is_permanent_oauth_error through the glob"
Assert-True ($shimText -match 'is_token_expired\(') "refresh task reaches is_token_expired through the glob"

# R4 the receiving crate declares every moved symbol
foreach ($sym in @("MCPAuthSettings", "MCPOAuthTokens", "MCPTokenManager", "MCPOAuthSessionManager",
                   "WwwAuthenticateInfo", "MCPAuthProbeResult", "AuthType",
                   "probe_mcp_auth", "save_tokens_to_config", "load_tokens_from_config",
                   "clear_tokens_from_config", "create_auth_manager_from_tokens",
                   "needs_refresh", "needs_refresh_at", "is_permanent_oauth_error",
                   "is_token_expired", "tokens_from_response", "ensure_oauth2_pkce_auth_type")) {
    Assert-True ($crateText -match ("\b" + [regex]::Escape($sym) + "\b")) "crate declares $sym"
}
Assert-True ($crateText -match 'pub const REFRESH_BEFORE_EXPIRY_MS') "crate declares REFRESH_BEFORE_EXPIRY_MS"
Assert-True ($crateText -match 'pub const REFRESH_RETRY_BASE_SECS') "crate declares REFRESH_RETRY_BASE_SECS"
Assert-True ($crateText -match 'pub const REFRESH_RETRY_MAX_SECS') "crate declares REFRESH_RETRY_MAX_SECS"
Assert-True (-not ($crateText -match 'fn mcp_oauth_refresh_task')) "crate does not define the engine-bound refresh task"

# R5 the crate module is engine-free
Assert-True (-not ($crateText -match 'crate::integrations')) "crate module has no engine path"
Assert-True (-not ($crateText -match 'crate::global_context')) "crate module has no engine global_context path"

# R6 module wiring
$mcpMod = [IO.File]::ReadAllText((Join-Path $crateDir "src/mcp/mod.rs"))
Assert-True ($mcpMod -match "(?m)^pub mod mcp_auth;") "crate mcp/mod.rs declares mcp_auth"
$libText = [IO.File]::ReadAllText((Join-Path $crateDir "src/lib.rs"))
Assert-True ($libText -match "(?m)^pub mod mcp;") "crate lib.rs declares the mcp module"
$engineMod = [IO.File]::ReadAllText((Join-Path $engine "src/integrations/mcp/mod.rs"))
Assert-True ($engineMod -match "(?m)^pub mod mcp_auth;") "engine mcp/mod.rs still declares mcp_auth"

# R7 asset include paths resolve from the crate (the include_str! move trap)
$assets = [regex]::Matches($crateText, 'concat!\(env!\("CARGO_MANIFEST_DIR"\), "/([^"]+)"\)')
Assert-True ($assets.Count -ge 2) "crate test retargets both MCP schema assets"
foreach ($m in $assets) {
    $rel = $m.Groups[1].Value.TrimStart("/").Replace("/", "\")
    $resolved = Join-Path $crateDir $rel
    Assert-True (Test-Path $resolved) "asset resolves: $rel"
}
Assert-True ($crateText -match 'mcp_http_schema\.yaml') "crate test references mcp_http_schema.yaml"
Assert-True ($crateText -match 'mcp_sse_schema\.yaml') "crate test references mcp_sse_schema.yaml"
Assert-True (Test-Path (Join-Path $engine "src/integrations/mcp/mcp_http_schema.yaml")) "engine schema source still exists"
Assert-True (Test-Path (Join-Path $engine "src/integrations/mcp/mcp_sse_schema.yaml")) "engine sse schema source still exists"

# R8 tests moved verbatim
$engineTests = ([regex]::Matches($shimText, '#\[(?:tokio::)?test\]')).Count
$crateTests = ([regex]::Matches($crateText, '#\[(?:tokio::)?test\]')).Count
Assert-True ($engineTests -eq 0) "engine shim carries no tests"
Assert-True ($crateTests -ge 60) "crate module carries the moved tests (found $crateTests)"

# R9 every consumer import still resolves
$consumers = @(
    @("src/http/routers/v1/mcp_oauth.rs", "integrations::mcp::mcp_auth"),
    @("src/buddy/observers/mcp_auth.rs", "integrations::mcp::mcp_auth"),
    @("src/integrations/mcp/integr_mcp_common.rs", "super::mcp_auth"),
    @("src/integrations/mcp/session_mcp.rs", "super::mcp_auth"),
    @("src/integrations/mcp/mcp_sse_transport.rs", "super::mcp_auth")
)
foreach ($pair in $consumers) {
    $t = [IO.File]::ReadAllText((Join-Path $engine $pair[0]))
    Assert-True ($t -match [regex]::Escape($pair[1])) "consumer imports $($pair[1]): $($pair[0])"
}

Write-Output ""
Write-Output "checks=$checks failures=$fail"
if ($fail -ne 0) { exit 1 }
exit 0
