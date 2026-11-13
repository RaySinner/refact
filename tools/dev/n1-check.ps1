# N-1 verification harness: static proof that the K6a1 + K6a2 move is
# behaviour-preserving.
#
#  K6a1 agent spawn core : tool_task_spawn_agent, tool_task_agent, tool_task_agent_finish
#  K6a2 agent polling    : tool_task_check_agents, tool_task_wait_for_agents
#
# Usage (single command, no separators, no pipes, no globs):
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\dev\n1-check.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File refact-agent\engine\target\n1-check.ps1
$ErrorActionPreference = "Stop"
# Windows PowerShell decodes a native command's stdout with the console output
# encoding. The pre-move sources are UTF-8 with box-drawing characters and
# emoji, so without this `git show` returns mojibake and every byte comparison
# below fails for reasons that have nothing to do with the move.
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
$fail = 0
$checks = 0

function Assert-True([bool]$cond, [string]$name) {
    $script:checks++
    if ($cond) { Write-Output "OK   $name" }
    else { Write-Output "FAIL $name"; $script:fail++ }
}

# Locate <repo> by walking up from this script, so the harness works from
# tools/dev/ and from refact-agent/engine/target/ without editing.
$probe = $PSScriptRoot
$repo = $null
for ($i = 0; $i -lt 8 -and $probe; $i++) {
    if ((Test-Path (Join-Path $probe "refact-agent\engine\src\tools")) -and
        (Test-Path (Join-Path $probe "tools\dev\engine-build.ps1"))) { $repo = $probe; break }
    $probe = Split-Path $probe -Parent
}
if (-not $repo) { Write-Output "FAIL cannot locate the repository root"; exit 1 }
$engine = Join-Path $repo "refact-agent\engine"
$src = Join-Path $engine "src\tools"
$crate = Join-Path $engine "crates\refact-tools"
$dst = Join-Path $crate "src\tools"
Assert-True $true "located engine at $engine"

$MODS = @("tool_task_spawn_agent", "tool_task_agent", "tool_task_agent_finish",
          "tool_task_check_agents", "tool_task_wait_for_agents")
$BASE = "bca526164"
$TEST_COUNT = @{
    "tool_task_spawn_agent" = 47
    "tool_task_agent" = 3
    "tool_task_agent_finish" = 30
    "tool_task_check_agents" = 31
    "tool_task_wait_for_agents" = 15
}

# ---------------------------------------------------------------- helpers
function Read-Text([string]$path) {
    return [IO.File]::ReadAllText($path, [Text.Encoding]::UTF8)
}

# `git show` hands back CRLF on Windows and the files are LF, so normalise.
function Orig-Lines([string]$mod) {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $text = & git -C $repo show ("{0}:refact-agent/engine/src/tools/{1}.rs" -f $BASE, $mod) | Out-String
    }
    finally { $ErrorActionPreference = $prev }
    return [string[]]($text -replace "`r`n", "`n" -split "`n")
}

function File-Lines([string]$path) {
    return [string[]]((Read-Text $path) -replace "`r`n", "`n" -split "`n")
}

function Trim-TrailingBlanks([string[]]$lines) {
    $end = $lines.Count
    while ($end -gt 0 -and $lines[$end - 1] -eq "") { $end-- }
    if ($end -eq 0) { return [string[]]@() }
    return [string[]]$lines[0..($end - 1)]
}

function Find-CfgTest([string[]]$lines) {
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($i -gt 5 -and $lines[$i].Trim() -eq "#[cfg(test)]") { return $i }
    }
    return -1
}

# The crate body may only differ from the pre-move production block by the two
# documented path rewrites and by gaining visibility, so normalising the
# visibility keywords away has to make the two line sequences identical.
function Normalize-Line([string]$line) {
    $m = [regex]::Match($line, "^(?<ind>\s*)(?<rest>.*)$")
    $ind = $m.Groups["ind"].Value
    $rest = $m.Groups["rest"].Value
    if ($rest.StartsWith("pub(crate) ")) { $rest = $rest.Substring(11) }
    elseif ($rest.StartsWith("pub(super) ")) { $rest = $rest.Substring(11) }
    elseif ($rest.StartsWith("pub ")) { $rest = $rest.Substring(4) }
    return $ind + $rest
}

function Normalize-Lines([string[]]$lines) {
    $out = @()
    foreach ($l in $lines) { $out += (Normalize-Line $l) }
    return [string[]]$out
}

# ---------------------------------------------------------------- R1 files exist
foreach ($mod in $MODS) {
    $crateFile = Join-Path $dst "$mod.rs"
    $engineFile = Join-Path $src "$mod.rs"
    $crateLen = 0
    if (Test-Path $crateFile) { $crateLen = (Read-Text $crateFile).Length }
    Assert-True ($crateLen -gt 4000) "crate $mod.rs exists and is non-trivial ($crateLen bytes)"
    Assert-True (Test-Path $engineFile) "engine $mod.rs still exists"
}

# ---------------------------------------------------------------- R2 no engine edge
foreach ($mod in $MODS) {
    $body = Read-Text (Join-Path $dst "$mod.rs")
    $stray = @([regex]::Matches($body, "crate::[\w:]*") |
        ForEach-Object { $_.Value } |
        Where-Object { -not $_.StartsWith("crate::tools::") } |
        Sort-Object -Unique)
    Assert-True ($stray.Count -eq 0) "crate $mod.rs has no engine-only crate:: ref ($($stray -join ', '))"
}

# ---------------------------------------------------------------- R3 the two path rewrites landed
$spawn = Read-Text (Join-Path $dst "tool_task_spawn_agent.rs")
Assert-True ($spawn.Contains("use refact_runtime_api::global_context::GlobalContext;")) `
    "crate tool_task_spawn_agent.rs imports GlobalContext from refact-runtime-api"
Assert-True (-not ($spawn.Contains("crate::global_context::"))) `
    "crate tool_task_spawn_agent.rs no longer names crate::global_context"
$finish = Read-Text (Join-Path $dst "tool_task_agent_finish.rs")
Assert-True ($finish.Contains("refact_chat_api::agent_notifications::notify_planner_agents_finished")) `
    "crate tool_task_agent_finish.rs reaches the notifier through refact-chat-api"
Assert-True (-not ($finish.Contains("crate::chat::task_agent_monitor"))) `
    "crate tool_task_agent_finish.rs no longer names crate::chat::task_agent_monitor"

# ---------------------------------------------------------------- R4 engine files are shims
foreach ($mod in $MODS) {
    $body = Read-Text (Join-Path $src "$mod.rs")
    Assert-True ($body.StartsWith("pub use refact_tools::tools::$mod::*;")) `
        "engine $mod.rs starts with the re-export shim"
}

# ---------------------------------------------------------------- R5..R8 per-module fidelity
foreach ($mod in $MODS) {
    $origLines = Orig-Lines $mod
    $cut = Find-CfgTest $origLines
    Assert-True ($cut -gt 0) "found #[cfg(test)] in pre-move $mod.rs"
    if ($cut -le 0) { continue }

    $prod = [string[]]$origLines[0..($cut - 1)]
    $prod = [string[]]($prod -replace "use crate::global_context::GlobalContext;",
                                      "use refact_runtime_api::global_context::GlobalContext;")
    $prod = [string[]]($prod -replace "crate::chat::task_agent_monitor::notify_planner_agents_finished",
                                      "refact_chat_api::agent_notifications::notify_planner_agents_finished")
    $prod = Trim-TrailingBlanks $prod
    $prodNorm = Normalize-Lines $prod

    $crateNorm = Normalize-Lines (Trim-TrailingBlanks (File-Lines (Join-Path $dst "$mod.rs")))

    Assert-True ($prodNorm.Count -eq $crateNorm.Count) `
        "crate $mod.rs has the same production line count ($($prodNorm.Count) vs $($crateNorm.Count))"
    if ($prodNorm.Count -eq $crateNorm.Count) {
        $diff = @()
        for ($i = 0; $i -lt $prodNorm.Count; $i++) {
            if ($prodNorm[$i] -ne $crateNorm[$i]) { $diff += $i }
        }
        Assert-True ($diff.Count -eq 0) `
            "crate $mod.rs production body is verbatim modulo rewrites and visibility ($($diff.Count) differing lines: $($diff -join ','))"
    }

    $testBlock = (($origLines[$cut..($origLines.Count - 1)]) -join "`n").TrimEnd("`n")
    $shim = Read-Text (Join-Path $src "$mod.rs")
    Assert-True ($shim.Contains($testBlock)) "engine $mod.rs keeps its test module verbatim"
    $shimTests = [regex]::Matches($shim, "#\[(?:tokio::)?test[\(\]]").Count
    Assert-True ($shimTests -eq $TEST_COUNT[$mod]) `
        "engine $mod.rs keeps $($TEST_COUNT[$mod]) tests (found $shimTests)"
    $crateTests = [regex]::Matches((Read-Text (Join-Path $dst "$mod.rs")), "#\[(?:tokio::)?test[\(\]]").Count
    Assert-True ($crateTests -eq 0) "crate $mod.rs carries no test module (tests stay in the engine)"
}

# ---------------------------------------------------------------- R8b no non-pub top-level item
# The engine shim is a glob re-export; a non-`pub` item at the crate module's
# top level is invisible to it, so any unnoticed one is a latent compile error
# for whatever engine code reaches it. Inherent `impl` methods and struct
# fields are covered by the R11 export/field assertions below.
foreach ($mod in $MODS) {
    $lines = File-Lines (Join-Path $dst "$mod.rs")
    $depth = 0
    $bad = @()
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($depth -eq 0 -and
            $line -match "^(async )?(fn|struct|enum|const|static|type|trait|union|macro_rules!)\s") {
            $bad += $i
        }
        $depth += ([regex]::Matches($line, "\{").Count - [regex]::Matches($line, "\}").Count)
    }
    Assert-True ($bad.Count -eq 0) "crate $mod.rs has no non-pub top-level item (lines $($bad -join ','))"
}

# ---------------------------------------------------------------- R8c trait impls stay bare
# A visibility qualifier on an `impl Trait for T` method is a syntax error, so
# the widener must leave those alone.
foreach ($mod in $MODS) {
    $lines = File-Lines (Join-Path $dst "$mod.rs")
    $depth = 0
    $inTraitImpl = $false
    $bad = @()
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($depth -eq 0) {
            $inTraitImpl = ($line -match "^impl\b") -and ($line -match "\bfor\b")
        }
        if ($depth -ge 1 -and $inTraitImpl -and $line -match "^\s+pub\s") {
            $bad += $i
        }
        $depth += ([regex]::Matches($line, "\{").Count - [regex]::Matches($line, "\}").Count)
    }
    Assert-True ($bad.Count -eq 0) "crate $mod.rs has no pub inside an impl-Trait-for block (lines $($bad -join ','))"
}

# ---------------------------------------------------------------- R9 crate module table
$toolsRs = Read-Text (Join-Path $crate "src\tools.rs")
foreach ($mod in $MODS) {
    Assert-True ($toolsRs.Contains("pub mod $mod;")) "crate tools.rs declares pub mod $mod"
}
Assert-True ($toolsRs.Contains('#[cfg(feature = "test-util")]')) `
    "crate tools.rs keeps the settings_guard cfg gate"

# ---------------------------------------------------------------- R10 manifest stays engine-free
$crateToml = Read-Text (Join-Path $crate "Cargo.toml")
Assert-True (-not ($crateToml.Contains("refact-lsp"))) "crate manifest never mentions refact-lsp"
Assert-True (-not ($crateToml -match 'path\s*=\s*"\.\./\.\.')) "crate manifest has no path dep on the engine crate"

# ---------------------------------------------------------------- R11 constructor surface resolves
$toolsList = Read-Text (Join-Path $src "tools_list.rs")
$ctors = @(
    "crate::tools::tool_task_spawn_agent::ToolTaskSpawnAgent::new()",
    "crate::tools::tool_task_agent::ToolTaskAgentUpdate::new()",
    "crate::tools::tool_task_agent::ToolTaskAgentComplete::new()",
    "crate::tools::tool_task_agent::ToolTaskAgentFail::new()",
    "crate::tools::tool_task_agent::ToolTaskAssignAgent::new()",
    "crate::tools::tool_task_agent_finish::ToolTaskAgentFinish::new()",
    "crate::tools::tool_task_check_agents::ToolTaskCheckAgents::new()",
    "crate::tools::tool_task_wait_for_agents::ToolTaskWaitForAgents::new()"
)
foreach ($ctor in $ctors) {
    Assert-True ($toolsList.Contains($ctor)) "tools_list.rs still constructs $ctor"
}

# every engine file that names a moved module can still do so through the shim
$named = @{}
foreach ($file in Get-ChildItem (Join-Path $src "*.rs")) {
    $text = Read-Text $file.FullName
    foreach ($m in [regex]::Matches($text, "crate::tools::(\w+)::")) {
        $named[$m.Groups[1].Value] = $true
    }
}
foreach ($mod in $MODS) {
    Assert-True ($named.ContainsKey($mod)) "engine call sites still name crate::tools::$mod::"
}

# the names the staying engine code reaches are declared pub in the crate
$EXPORTS = @{
    "tool_task_spawn_agent" = @("pub struct PreparedWorktree", "pub async fn prepare_agent_worktree(",
        "pub async fn prepare_agent_worktree_with_suffix(", "pub fn find_abandoned_worktrees(",
        "pub fn abandoned_worktrees_error(", "pub fn build_agent_prompt(", "pub fn build_agent_thread_params(",
        "pub fn mark_card_agent_started(", "pub fn restore_original_card_if_current_agent(",
        "pub async fn resolve_agent_model(", "pub async fn resolve_invoking_planner_chat_id(",
        "pub async fn retain(", "pub fn branch_name(", "pub fn worktree_name(", "pub fn worktree_path(",
        "pub fn worktree_meta(", "pub struct ToolTaskSpawnAgent")
    "tool_task_agent" = @("pub struct ToolTaskAgentUpdate", "pub struct ToolTaskAgentComplete",
        "pub struct ToolTaskAgentFail", "pub struct ToolTaskAssignAgent", "pub fn resolve_task_agent_scope(")
    "tool_task_agent_finish" = @("pub struct ToolTaskAgentFinish", "pub struct ParsedFinishReport",
        "pub async fn auto_commit_worktree_with_message(", "pub fn is_room_finisher(",
        "pub fn sanitize_commit_component(", "pub fn parse_finish_report(", "pub fn agents_active(")
    "tool_task_check_agents" = @("pub struct ToolTaskCheckAgents", "pub struct AgentStatus",
        "pub struct AgentStatusQuery", "pub async fn get_agent_statuses_for_ctx(",
        "pub fn parse_agent_status_query(", "pub fn format_agent_statuses(",
        "pub fn waitable_agent_statuses<'a>(", "pub fn has_active_agent_statuses(",
        "pub fn agent_status_input_schema(")
    "tool_task_wait_for_agents" = @("pub struct ToolTaskWaitForAgents", "pub fn parse_wake_up_after_secs(",
        "pub fn resolve_waiting_card_ids(")
}
foreach ($mod in $MODS) {
    $body = Read-Text (Join-Path $dst "$mod.rs")
    foreach ($sym in $EXPORTS[$mod]) {
        Assert-True ($body.Contains($sym)) "crate $mod.rs exports $sym"
    }
}

# the engine-side tests read private fields, so those fields have to be pub
$FIELDS = @{
    "tool_task_spawn_agent" = @("pub meta: WorktreeMeta,", "pub base_branch_mismatch_warning: Option<String>,")
    "tool_task_check_agents" = @("pub card_id: String,", "pub room_members: Vec<TeamMember>,",
        "pub variant_key: Option<String>,", "pub change_seq: u64,")
    "tool_task_agent_finish" = @("pub markdown: String,", "pub structured: Option<FinalReport>,")
}
foreach ($mod in $FIELDS.Keys) {
    $body = Read-Text (Join-Path $dst "$mod.rs")
    foreach ($f in $FIELDS[$mod]) {
        Assert-True ($body.Contains($f)) "crate $mod.rs exposes field $f"
    }
}

# ---------------------------------------------------------------- R12 scope guard
$prev = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try { $dirty = @(& git -C $repo status --porcelain) } finally { $ErrorActionPreference = $prev }
$touched = @()
foreach ($line in $dirty) {
    if ($line.Length -gt 3) { $touched += ($line.Substring(3).Trim() -replace "\\", "/") }
}
$allowed = @("refact-agent/engine/src/tools/", "refact-agent/engine/crates/refact-tools/", "tools/dev/n1-")
$extra = @($touched | Where-Object {
    $p = $_
    @($allowed | Where-Object { $p.StartsWith($_) }).Count -eq 0
})
Assert-True ($extra.Count -eq 0) "no unrelated file touched ($($extra -join ', '))"
Assert-True (-not ($touched -contains "refact-agent/engine/src/tools/mod.rs")) "src/tools/mod.rs untouched"
Assert-True (-not ($touched -contains "refact-agent/engine/Cargo.toml")) "engine Cargo.toml untouched"
Assert-True (-not ($touched -contains "refact-agent/engine/Cargo.lock")) "engine Cargo.lock untouched"
Assert-True (-not ($touched -contains "refact-agent/engine/src/tools/tools_list.rs")) "tools_list.rs untouched"
Assert-True (-not ($touched -contains "refact-agent/engine/src/tools/task_tool_helpers.rs")) "task_tool_helpers.rs untouched"
Assert-True (-not ($touched -contains "refact-agent/engine/src/tools/tool_task_batch.rs")) "tool_task_batch.rs untouched"

Write-Output ""
Write-Output "checks=$checks failures=$fail"
if ($fail -ne 0) { exit 1 }
exit 0
