<#
.SYNOPSIS
Computes Median/Mean Recovery Time, Median Quality, With-VOS vs Baseline (manual) A/B comparison,
Predicted-vs-Actual correlation, False Alarm / Missed Difficulty counts, and Helpful/Ignored signal tallies
from docs/recovery_benchmark.md (plus live `vos session stats`).
#>
param(
    [string]$LogPath = (Join-Path $PSScriptRoot "..\docs\recovery_benchmark.md"),
    [switch]$Json
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

if (-not (Test-Path $LogPath)) {
    throw "Log file not found: $LogPath"
}

function Get-Median([double[]]$values) {
    if (-not $values -or $values.Count -eq 0) { return $null }
    $sorted = $values | Sort-Object
    $n = $sorted.Count
    if ($n % 2 -eq 1) { return [double]$sorted[[int][Math]::Floor($n / 2)] }
    return ([double]$sorted[$n / 2 - 1] + [double]$sorted[$n / 2]) / 2.0
}

function Get-GroupSummary($items) {
    if (-not $items -or $items.Count -eq 0) { return $null }
    $t = [double[]]($items | ForEach-Object { $_.TimeMin })
    $q = [double[]]($items | ForEach-Object { $_.Quality })
    [pscustomobject]@{
        sessions    = $items.Count
        median_min  = [math]::Round((Get-Median $t), 2)
        mean_min    = [math]::Round(($t | Measure-Object -Average).Average, 2)
        avg_quality = [math]::Round(($q | Measure-Object -Average).Average, 2)
    }
}

$rows = @()
foreach ($line in Get-Content -Path $LogPath -Encoding UTF8) {
    $trim = $line.Trim()
    if (-not $trim.StartsWith("|") -or $trim.Contains("<!--") -or $trim -match '^\|\s*(Date|---)\s*\|') {
        continue
    }
    $cols = $trim.Trim('|').Split('|') | ForEach-Object { $_.Trim() }
    if ($cols.Count -lt 6) { continue }

    $isV12 = $cols.Count -ge 9
    $timeCol = if ($isV12) { $cols[5] } else { $cols[4] }
    $qualCol = if ($isV12) { $cols[6] } else { $cols[5] }

    $timeVal = 0.0
    $qualVal = 0.0
    $timeClean = ($timeCol -replace '[^0-9\.]', '')
    $qualClean = ($qualCol -replace '[^0-9\.]', '')
    if ([double]::TryParse($timeClean, [System.Globalization.NumberStyles]::Float, [System.Globalization.CultureInfo]::InvariantCulture, [ref]$timeVal) -and
        [double]::TryParse($qualClean, [System.Globalization.NumberStyles]::Float, [System.Globalization.CultureInfo]::InvariantCulture, [ref]$qualVal)) {
        $rows += [pscustomobject]@{
            Date         = $cols[0]
            Project      = $cols[1]
            DaysAway     = $cols[2]
            FirstCommand = $cols[3].ToLowerInvariant()
            Predicted    = if ($isV12) { $cols[4] } else { "Unknown" }
            TimeMin      = $timeVal
            Quality      = $qualVal
            Helpful      = if ($isV12 -and $cols.Count -ge 8) { $cols[7] } else { "-" }
            Ignored      = if ($isV12 -and $cols.Count -ge 9) { $cols[8] } else { "-" }
        }
    }
}

$dbSessionStats = $null
if (Get-Command vos -ErrorAction SilentlyContinue) {
    $rawStats = (vos session stats --json) 2>$null | Out-String
    if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($rawStats)) {
        try { $dbSessionStats = $rawStats | ConvertFrom-Json } catch {}
    }
}

if ($rows.Count -eq 0) {
    if ($Json) {
        [pscustomobject]@{
            sessions         = 0
            log_path         = $LogPath
            ab_comparison    = $null
            predicted_actual = @()
            signals          = $null
            sqlite_stats     = $dbSessionStats
        } | ConvertTo-Json -Depth 6
        exit 0
    }
    Write-Host "No recovery sessions logged yet in $LogPath."
    if ($dbSessionStats) {
        Write-Host "`nSQLite Work Sessions Summary:"
        vos session stats | ForEach-Object { Write-Host "  $_" }
    }
    exit 0
}

$times = [double[]]($rows | ForEach-Object { $_.TimeMin })
$quals = [double[]]($rows | ForEach-Object { $_.Quality })
$meanTime = ($times | Measure-Object -Average).Average
$medTime = Get-Median $times
$medQual = Get-Median $quals

$byProj = @($rows | Group-Object Project | ForEach-Object {
    [pscustomobject]@{
        Project = $_.Name
        Runs    = $_.Count
        AvgTime = [math]::Round(($_.Group | Measure-Object TimeMin -Average).Average, 1)
        AvgQual = [math]::Round(($_.Group | Measure-Object Quality -Average).Average, 2)
    }
} | Sort-Object AvgTime, @{ Expression = "AvgQual"; Descending = $true })

$vosRows = @($rows | Where-Object { $_.FirstCommand -ne "manual" })
$manualRows = @($rows | Where-Object { $_.FirstCommand -eq "manual" })
$vosSummary = Get-GroupSummary $vosRows
$manualSummary = Get-GroupSummary $manualRows
$speedupRatio = if ($vosSummary -and $manualSummary -and $vosSummary.median_min -gt 0) {
    [math]::Round($manualSummary.median_min / $vosSummary.median_min, 2)
} else { $null }
$timeSavedMin = if ($vosSummary -and $manualSummary) {
    [math]::Round($manualSummary.median_min - $vosSummary.median_min, 1)
} else { $null }

$predRows = @($rows | Where-Object { $_.Predicted -in @("Easy", "Medium", "Hard") })
$tierSummaries = @()
$falseAlarms = 0; $hardTotal = 0
$missedDiff = 0; $easyTotal = 0
if ($predRows.Count -gt 0) {
    foreach ($tier in @("Easy", "Medium", "Hard")) {
        $bucket = @($predRows | Where-Object { $_.Predicted -eq $tier })
        if ($bucket.Count -gt 0) {
            $tierSummaries += [pscustomobject]@{
                tier        = $tier
                sessions    = $bucket.Count
                avg_min     = [math]::Round(($bucket | Measure-Object TimeMin -Average).Average, 1)
                avg_quality = [math]::Round(($bucket | Measure-Object Quality -Average).Average, 2)
            }
        }
    }
    $hardBucket = @($predRows | Where-Object { $_.Predicted -eq "Hard" })
    $easyBucket = @($predRows | Where-Object { $_.Predicted -eq "Easy" })
    $hardTotal = $hardBucket.Count
    $easyTotal = $easyBucket.Count
    $falseAlarms = @($hardBucket | Where-Object { $_.Quality -ge 4 -and $_.TimeMin -le 5.0 }).Count
    $missedDiff = @($easyBucket | Where-Object { $_.Quality -le 2 -or $_.TimeMin -ge 15.0 }).Count
}

# Prefer the counters computed by the CLI (single source of truth for thresholds);
# fall back to the markdown-derived values when the SQLite stats are unavailable or older.
$statsSource = 'markdown'
if ($dbSessionStats -and $null -ne $dbSessionStats.false_alarms -and $null -ne $dbSessionStats.missed_difficulty) {
    $mdCounts = "$falseAlarms/$hardTotal/$missedDiff/$easyTotal"
    $falseAlarms = [int]$dbSessionStats.false_alarms
    $hardTotal = [int]$dbSessionStats.hard_scored_sessions
    $missedDiff = [int]$dbSessionStats.missed_difficulty
    $easyTotal = [int]$dbSessionStats.easy_scored_sessions
    $statsSource = 'sqlite'
    if ($mdCounts -ne "$falseAlarms/$hardTotal/$missedDiff/$easyTotal" -and -not $Json) {
        Write-Warning "SQLite counters (FA/Hard/MD/Easy = $falseAlarms/$hardTotal/$missedDiff/$easyTotal) differ from the markdown log ($mdCounts); the log may contain sessions that were not recorded via 'vos session'."
    }
}

$helpfulCounts = @{}
$ignoredCounts = @{}
foreach ($r in $rows) {
    foreach ($tok in ($r.Helpful -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ -and $_ -ne "-" })) {
        $helpfulCounts[$tok] = ($helpfulCounts[$tok] ?? 0) + 1
    }
    foreach ($tok in ($r.Ignored -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ -and $_ -ne "-" })) {
        $ignoredCounts[$tok] = ($ignoredCounts[$tok] ?? 0) + 1
    }
}

if ($Json) {
    [pscustomobject]@{
        sessions       = $rows.Count
        median_min     = [math]::Round($medTime, 2)
        mean_min       = [math]::Round($meanTime, 2)
        median_quality = [math]::Round($medQual, 2)
        ab_comparison  = [pscustomobject]@{
            with_vos             = $vosSummary
            without_vos          = $manualSummary
            median_speedup_ratio = $speedupRatio
            median_saved_min     = $timeSavedMin
        }
        predicted_actual = [pscustomobject]@{
            by_tier           = $tierSummaries
            stats_source      = $statsSource
            false_alarms      = $falseAlarms
            hard_total        = $hardTotal
            missed_difficulty = $missedDiff
            easy_total        = $easyTotal
        }
        signals = [pscustomobject]@{
            helpful = $helpfulCounts
            ignored = $ignoredCounts
        }
        by_project   = $byProj
        sqlite_stats = $dbSessionStats
    } | ConvertTo-Json -Depth 6
    exit 0
}

Write-Host "Recovery Sprint Summary ($($rows.Count) sessions):"
Write-Host ("  Median Recovery Time: {0:N1} min (Mean: {1:N1} min)" -f $medTime, $meanTime)
Write-Host ("  Median Quality:       {0:N1} / 5" -f $medQual)
Write-Host ("  Best Project:         {0} ({1} min avg, quality {2})" -f $byProj[0].Project, $byProj[0].AvgTime, $byProj[0].AvgQual)
Write-Host ("  Worst Project:        {0} ({1} min avg, quality {2})" -f $byProj[-1].Project, $byProj[-1].AvgTime, $byProj[-1].AvgQual)

if ($vosSummary -or $manualSummary) {
    Write-Host "`nWith VOS vs Baseline (manual):"
    if ($vosSummary) {
        Write-Host ("  With VOS    : {0,2} sessions | median: {1} min | mean: {2} min | avg quality: {3}/5" -f $vosSummary.sessions, $vosSummary.median_min, $vosSummary.mean_min, $vosSummary.avg_quality)
    }
    if ($manualSummary) {
        Write-Host ("  Without VOS : {0,2} sessions | median: {1} min | mean: {2} min | avg quality: {3}/5" -f $manualSummary.sessions, $manualSummary.median_min, $manualSummary.mean_min, $manualSummary.avg_quality)
    }
    if ($null -ne $speedupRatio) {
        Write-Host ("  Delta       : {0}x median speedup ({1:+0.0;-0.0;0.0} min saved)" -f $speedupRatio, $timeSavedMin)
    }
}

Write-Host "`nFirst Command Breakdown:"
$rows | Group-Object FirstCommand | Sort-Object Count -Descending | ForEach-Object {
    $share = [math]::Round(($_.Count / $rows.Count) * 100.0, 1)
    Write-Host ("  {0,-12} : {1,2} ({2}%)" -f $_.Name, $_.Count, $share)
}

if ($predRows.Count -gt 0) {
    Write-Host "`nPredicted vs Actual:"
    foreach ($t in $tierSummaries) {
        Write-Host ("  {0,-6} : {1,2} sessions | avg time: {2} min | avg quality: {3}/5" -f $t.tier, $t.sessions, $t.avg_min, $t.avg_quality)
    }
    $faPct = if ($hardTotal -gt 0) { " ($([math]::Round(($falseAlarms / $hardTotal) * 100, 1))% of Hard)" } else { "" }
    $mdPct = if ($easyTotal -gt 0) { " ($([math]::Round(($missedDiff / $easyTotal) * 100, 1))% of Easy)" } else { "" }
    Write-Host ("  Source: {0}" -f $statsSource)
    Write-Host ("  False Alarms (Hard -> <=5m & Q>=4):   {0}{1}" -f $falseAlarms, $faPct)
    Write-Host ("  Missed Difficulty (Easy -> >=15m/Q<=2): {0}{1}" -f $missedDiff, $mdPct)
}

if ($helpfulCounts.Count -gt 0 -or $ignoredCounts.Count -gt 0) {
    Write-Host "`nSignal Usefulness Tally:"
    foreach ($kv in ($helpfulCounts.GetEnumerator() | Sort-Object Value -Descending)) {
        Write-Host ("  + Helpful : {0} ({1}x)" -f $kv.Key, $kv.Value)
    }
    foreach ($kv in ($ignoredCounts.GetEnumerator() | Sort-Object Value -Descending)) {
        Write-Host ("  - Ignored : {0} ({1}x)" -f $kv.Key, $kv.Value)
    }
}

if ($dbSessionStats) {
    Write-Host "`nSQLite Work Sessions Summary:"
    vos session stats | ForEach-Object { Write-Host "  $_" }
}