param([string]$Scope = 'refact-agent/engine/src')
$files = Get-ChildItem -Path $Scope -Recurse -Filter *.rs -File
$files = $files | Where-Object {
    ($_.Name -notmatch '^tests') -and ($_.FullName -notmatch '\\tests\\')
}
$total = 0
$rows = @()
foreach ($f in $files) {
    $lines = [IO.File]::ReadAllLines($f.FullName)
    $prodEnd = $lines.Length
    for ($i = 0; $i -lt $lines.Length; $i++) {
        if ($lines[$i] -match '^#\[cfg\(test\)\]') { $prodEnd = $i; break }
    }
    $count = 0
    for ($i = 0; $i -lt $prodEnd; $i++) {
        $m = [regex]::Matches($lines[$i], '(?<![A-Za-z0-9_])gcx\b')
        $count += $m.Count
    }
    if ($count -gt 0) {
        $rel = $f.FullName.Substring((Resolve-Path $Scope).Path.Length + 1)
        $rows += [pscustomobject]@{ File = $rel; Gcx = $count }
        $total += $count
    }
}
$rows | Sort-Object -Property Gcx -Descending | Select-Object -First 40 | Format-Table -AutoSize
Write-Output ("TOTAL_PROD_GCX: " + $total)
Write-Output ("FILES_WITH_GCX: " + $rows.Count)
