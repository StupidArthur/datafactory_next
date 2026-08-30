param([Parameter(Mandatory = $true)][string]$ReportPath)

$ErrorActionPreference = 'Stop'
$report = Get-Content -LiteralPath $ReportPath -Raw | ConvertFrom-Json
$executableLines = 0
$uncovered = @()

foreach ($file in $report.data[0].files) {
    $sourceLines = if (Test-Path -LiteralPath $file.filename) { Get-Content -LiteralPath $file.filename } else { @() }
    $groups = $file.segments |
        Where-Object { $null -ne $_ -and $_.Count -gt 3 -and $_[3] -eq $true } |
        Group-Object { $_[0] }
    foreach ($group in $groups) {
        $lineNumber = [int]$group.Name
        if ($lineNumber -le $sourceLines.Count -and $sourceLines[$lineNumber - 1].Trim() -match '^[\)\]\}\(\[\{,;?]+$') {
            continue
        }
        $executableLines++
        $maximum = ($group.Group | ForEach-Object { $_[2] } | Measure-Object -Maximum).Maximum
        if ($maximum -eq 0) {
            $uncovered += "$($file.filename):$($group.Name)"
        }
    }
}

if ($uncovered.Count -gt 0) {
    Write-Error "Rust line coverage failed. Uncovered lines:`n$($uncovered -join "`n")"
    exit 1
}

Write-Output "Rust physical executable line coverage: $executableLines/$executableLines (100%)"
