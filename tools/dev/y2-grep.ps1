$pattern = '^(pub fn |pub async fn )'
Get-ChildItem refact-agent/engine/src/providers/*.rs | ForEach-Object {
    $count = (Select-String -Path $_.FullName -Pattern $pattern -AllMatches | Measure-Object).Count
    $name = $_.Name
    Write-Output ("{0,-20} {1}" -f $name, $count)
}
