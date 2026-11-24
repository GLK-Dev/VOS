<#
.SYNOPSIS
Runs a timed human Recovery Sprint session using VOS `session start`/`stop` + `--experimental`
insights, and logs Predicted Difficulty, Recovery Time, Quality, and Helpful/Ignored signals
to `docs/recovery_benchmark.md` and `docs/command_usage_audit.md`.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Project,
    [ValidateSet("continue", "status", "drift", "graph", "coverage", "why", "start", "search", "manual")]
    [string]$FirstCommand = "continue",
    [string]$Task = "Resume project context",
    [string]$Target = "",
    [switch]$WithoutVos
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$repoRoot = Split-Path -Parent $PSScriptRoot
$recLog = Join-Path $repoRoot "docs\recovery_benchmark.md"
$cmdAudit = Join-Path $repoRoot "docs\command_usage_audit.md"

$predicted = "manual"
$sw = [System.Diagnostics.Stopwatch]::StartNew()

if (-not $WithoutVos) {
    $startOut = (vos session start $Project --note $Task) 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw "Failed to start VOS session: $startOut" }
    if ($startOut -match 'Predicted Difficulty:\s+(\S+)') {
        $predicted = $matches[1]
    } else {
        $predicted = "Unknown"
    }

    Write-Host "`n=== VOS Recovery ($Project | Predicted: $predicted) ===" -ForegroundColor Cyan
    switch ($FirstCommand) {
        "continue" { vos continue $Project --experimental }
        "status"   { vos status $Project --experimental }
        "drift"    { vos drift $Project }
        "graph"    { vos graph $Project --experimental }
        "coverage" { vos coverage $Project --experimental }
        "why"      {
            $t = if ([string]::IsNullOrWhiteSpace($Target)) { Read-Host "Enter target object (path or filename)" } else { $Target }
            vos why $t --project $Project --experimental
        }
        "start"    { vos start $Project }
        "search"   {
            $q = Read-Host "Enter search query"
            vos search $q --project $Project
        }
    }
    if ($FirstCommand -ne "status") {
        Write-Host "`n--- Experimental Status Snapshot ---" -ForegroundColor DarkCyan
        vos status $Project --experimental | Select-String -Pattern 'Health:|Critical Risks:|High Risks:|Coverage:|Documentation Health:|Recovery Difficulty:|^\s+\+'
    }
} else {
    Write-Host "`n=== Baseline Recovery WITHOUT VOS ($Project) ===" -ForegroundColor Yellow
}

Read-Host "`nPress ENTER as soon as you make your first meaningful edit or decision"
$sw.Stop()
$elapsedMin = [math]::Round($sw.Elapsed.TotalMinutes, 1)
if ($elapsedMin -lt 0.1) { $elapsedMin = 0.1 }

$daysAway = Read-Host "Days away (check 'Last activity' above, or enter number)"
if ([string]::IsNullOrWhiteSpace($daysAway)) { $daysAway = "0" }

$qualityRaw = Read-Host "Recovery Quality (1=useless .. 5=full context)"
$quality = 3
if (-not [int]::TryParse($qualityRaw, [ref]$quality) -or $quality -lt 1 -or $quality -gt 5) {
    throw "Quality must be an integer from 1 to 5"
}

$helpful = Read-Host "Helpful signals (comma-separated, e.g. Critical Risk, Hub Importance, Continue)"
if ([string]::IsNullOrWhiteSpace($helpful)) { $helpful = "-" }

$ignored = Read-Host "Ignored / noisy signals (comma-separated, or leave empty)"
if ([string]::IsNullOrWhiteSpace($ignored)) { $ignored = "-" }

$notes = Read-Host "Short notes (key takeaway or false positive)"
if ([string]::IsNullOrWhiteSpace($notes)) { $notes = "-" }

# Sanitize pipe characters so Markdown tables remain aligned
$safeHelpful = ($helpful -replace '\|', '/').Trim()
$safeIgnored = ($ignored -replace '\|', '/').Trim()
$safeNotes = ($notes -replace '\|', '/').Trim()

if (-not $WithoutVos) {
    $stopNote = "helpful=[$safeHelpful] ignored=[$safeIgnored] $safeNotes"
    vos session stop $Project --quality $quality --note $stopNote | Out-Null
}

$today = Get-Date -Format "yyyy-MM-dd"
$cmdLabel = if ($WithoutVos) { "manual" } else { $FirstCommand }
$row = "| $today | $Project | $daysAway | $cmdLabel | $predicted | $elapsedMin | $quality | $safeHelpful | $safeIgnored | $safeNotes |"
Add-Content -Path $recLog -Value $row -Encoding UTF8

$auditEntry = @"

### ``$Project`` ($today | Predicted: $predicted)
- **First command:** ``$cmdLabel``
- **Recovery time:** $elapsedMin min | **Quality:** $quality/5
- **Helpful signals:** $safeHelpful
- **Ignored / noisy:** $safeIgnored
- **Notes:** $safeNotes
"@
Add-Content -Path $cmdAudit -Value $auditEntry -Encoding UTF8

Write-Host "`nLogged session: $row" -ForegroundColor Green
& pwsh -NoProfile -File (Join-Path $PSScriptRoot "analyze_recovery.ps1")
