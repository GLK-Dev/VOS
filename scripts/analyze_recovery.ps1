<#
.SYNOPSIS
Computes Median/Mean Recovery Time, Median Quality, Best/Worst Project, and First-Command stats
from docs/recovery_benchmark.md without changing VOS Rust code.
#>
param(
    [string]$LogPath = (Join-Path $PSScriptRoot "..\docs\recovery_benchmark.md")
)

$ErrorActionPreference = "Stop"
if (-not (Test-Path $LogPath)) {
    throw "Log file not found: $LogPath"
}

$rows = @()
foreach ($line in Get-Content -Path $LogPath -Encoding UTF8) {
    $trim = $line.Trim()
    if (-not $trim.StartsWith("|") -or $trim.Contains("<!--") -or $trim -match '^\|\s*(Date|---)\s*\|') {
        continue
    }
    $cols = $trim.Trim('|').Split('|') | ForEach-Object { $_.Trim() }
    if ($cols.Count -lt 6) { continue }

    $timeVal = 0.0
    $qualVal = 0.0
    $timeClean = ($cols[4] -replace '[^0-9\.]', '')
    $qualClean = ($cols[5] -replace '[^0-9\.]', '')
    if ([double]::TryParse($timeClean, [System.Globalization.NumberStyles]::Float, [System.Globalization.CultureInfo]::InvariantCulture, [ref]$timeVal) -and [double]::TryParse($qualClean, [System.Globalization.NumberStyles]::Float, [System.Globalization.CultureInfo]::InvariantCulture, [ref]$qualVal)) {
        $rows += [pscustomobject]@{
            Date         = $cols[0]
            Project      = $cols[1]
            DaysAway     = $cols[2]
            FirstCommand = $cols[3].ToLowerInvariant()
            TimeMin      = $timeVal
            Quality      = $qualVal
        }
    }
}

if ($rows.Count -eq 0) {
    Write-Host "No recovery sessions logged yet in $LogPath."
    exit 0
}

function Get-Median([double[]]$values) {
    $sorted = $values | Sort-Object
    $n = $sorted.Count
    if ($n % 2 -eq 1) { return $sorted[[int][Math]::Floor($n / 2)] }
    return ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2.0
}

$times = [double[]]($rows | ForEach-Object { $_.TimeMin })
$quals = [double[]]($rows | ForEach-Object { $_.Quality })
$meanTime = ($times | Measure-Object -Average).Average
$medTime = Get-Median $times
$medQual = Get-Median $quals

$byProj = $rows | Group-Object Project | ForEach-Object {
    [pscustomobject]@{
        Project = $_.Name
        Runs    = $_.Count
        AvgTime = [math]::Round(($_.Group | Measure-Object TimeMin -Average).Average, 1)
        AvgQual = [math]::Round(($_.Group | Measure-Object Quality -Average).Average, 2)
    }
} | Sort-Object AvgTime, @{ Expression = "AvgQual"; Descending = $true }

Write-Host "Recovery Sprint Summary ($($rows.Count) sessions):"
Write-Host ("  Median Recovery Time: {0:N1} min (Mean: {1:N1} min)" -f $medTime, $meanTime)
Write-Host ("  Median Quality:       {0:N1} / 5" -f $medQual)
Write-Host ("  Best Project:         {0} ({1} min avg, quality {2})" -f $byProj[0].Project, $byProj[0].AvgTime, $byProj[0].AvgQual)
Write-Host ("  Worst Project:        {0} ({1} min avg, quality {2})" -f $byProj[-1].Project, $byProj[-1].AvgTime, $byProj[-1].AvgQual)

Write-Host "`nFirst Command Breakdown:"
$rows | Group-Object FirstCommand | Sort-Object Count -Descending | ForEach-Object {
    $share = [math]::Round(($_.Count / $rows.Count) * 100.0, 1)
    Write-Host ("  {0,-12} : {1,2} ({2}%)" -f $_.Name, $_.Count, $share)
}

