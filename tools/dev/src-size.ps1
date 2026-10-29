param([string]$Scope = 'refact-agent/engine/src')
$root = (Resolve-Path $Scope).Path
$dirs = Get-ChildItem $root -Directory
$rows = @()
foreach ($d in $dirs) {
    $rs = Get-ChildItem $d.FullName -Recurse -Filter *.rs -File
    $lines = 0
    foreach ($f in $rs) { $lines += ([IO.File]::ReadAllLines($f.FullName)).Count }
    $rows += [pscustomobject]@{ Dir = $d.Name; Files = $rs.Count; Lines = $lines }
}
# top-level files
$top = Get-ChildItem $root -Filter *.rs -File
$topLines = 0
foreach ($f in $top) { $topLines += ([IO.File]::ReadAllLines($f.FullName)).Count }
$rows | Sort-Object -Property Lines -Descending | Format-Table -AutoSize
Write-Output ("TOPLEVEL_FILES: " + $top.Count + " lines: " + $topLines)
