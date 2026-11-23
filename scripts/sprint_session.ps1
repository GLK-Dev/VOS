<#
.SYNOPSIS
Runs a timed Recovery Sprint session for a project and appends the real result
to docs/recovery_benchmark.md and docs/command_usage_audit.md.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Project,
    [ValidateSet("continue", "search", "start", "status", "manual")]
    [string]$FirstCommand = "continue",
    [switch]$WithoutVos
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$recLog = Join-Path $repoRoot "docs\recovery_benchmark.md"
$cmdAudit = Join-Path $repoRoot "docs\command_usage_audit.md"

$sw = [System.Diagnostics.Stopwatch]::StartNew()
if (-not $WithoutVos) {
    Write-Host "`n=== VOS Recovery ($Project) ===" -ForegroundColor Cyan
    if ($FirstCommand -eq "continue") {
        vos continue $Project
    } elseif ($FirstCommand -eq "start") {
        vos start $Project
    } elseif ($FirstCommand -eq "status") {
        vos status $Project
    } else {
        $q = Read-Host "Enter search query"
        vos search $q --project $Project
    }
    Write-Host "`n--- Status & Drift ---" -ForegroundColor DarkCyan
    vos status $Project
    vos drift $Project
} else {
    Write-Host "`n=== Baseline Recovery WITHOUT VOS ($Project) ===" -ForegroundColor Yellow
}

Read-Host "`nPress ENTER as soon as you make your first meaningful edit or decision"
$sw.Stop()
$elapsedMin = [math]::Round($sw.Elapsed.TotalMinutes, 1)
if ($elapsedMin -lt 0.1) { $elapsedMin = 0.1 }

$daysAway = Read-Host "Days away (check 'Last activity' above, or enter number)"
if ([string]::IsNullOrWhiteSpace($daysAway)) { $daysAway = "0" }

$quality = Read-Host "Recovery Quality (1=useless .. 5=full context)"
$notes = Read-Host "Short notes (helpful sections / false positives)"

$today = Get-Date -Format "yyyy-MM-dd"
$cmdLabel = if ($WithoutVos) { "manual" } else { $FirstCommand }
$row = "| $today | $Project | $daysAway | $cmdLabel | $elapsedMin | $quality | $notes |"
Add-Content -Path $recLog -Value $row -Encoding UTF8

$auditEntry = @"

### ``$Project`` ($today)
- **First command:** ``$cmdLabel``
- **Recovery time:** $elapsedMin min | **Quality:** $quality/5
- **Notes:** $notes
"@
Add-Content -Path $cmdAudit -Value $auditEntry -Encoding UTF8

Write-Host "`nLogged session: $row" -ForegroundColor Green
& pwsh -NoProfile -File (Join-Path $PSScriptRoot "analyze_recovery.ps1")
