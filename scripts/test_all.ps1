<#
.SYNOPSIS
Fast unified runner for all VOS checks:
1) cargo clippy + cargo test --workspace
2) 30-scenario parallelized live sprint (run_v112_sprint.ps1)
3) 10-project parallelized audit + 50-query search benchmark (run_sprint_audit.ps1)
4) Optional experimental Recovery Difficulty predictor check (-IncludeExperimental)
#>
param(
    [string]$VosBin = "vos",
    [switch]$IncludeExperimental
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$repoRoot = Split-Path -Parent $PSScriptRoot
$totalSw = [System.Diagnostics.Stopwatch]::StartNew()

$swClippy = [System.Diagnostics.Stopwatch]::StartNew()
& cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }
$swClippy.Stop()

$swTest = [System.Diagnostics.Stopwatch]::StartNew()
& cargo test --workspace --quiet
if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }
$swTest.Stop()

# Ensure workspace scripts/docs are synced in ~/.vos/vos.db before idempotency scenario #30
& $VosBin index $repoRoot --project vos | Out-Null

# Run sprint + audit (+ optional experimental predictor checks) concurrently using ThreadJobs
$swLive = [System.Diagnostics.Stopwatch]::StartNew()
$sprintPath = Join-Path $PSScriptRoot "run_v112_sprint.ps1"
$auditPath = Join-Path $PSScriptRoot "run_sprint_audit.ps1"

$jobSprint = Start-ThreadJob -ScriptBlock {
    param($p, $bin)
    & $p -VosBin $bin
} -ArgumentList $sprintPath, $VosBin

$jobAudit = Start-ThreadJob -ScriptBlock {
    param($p, $bin)
    & $p -VosBin $bin
} -ArgumentList $auditPath, $VosBin

$jobs = @($jobSprint, $jobAudit)
if ($IncludeExperimental) {
    $jobExp = Start-ThreadJob -ScriptBlock {
        param($bin)
        $projects = & $bin project list | Where-Object { $_ -match '^([^\s]+)\s+\(proj_' } | ForEach-Object { if ($_ -match '^([^\s]+)\s+\(proj_') { $matches[1] } }
        $rows = $projects | ForEach-Object -ThrottleLimit 8 -Parallel {
            $b = $using:bin
            $p = $_
            $out = & $b status $p --experimental
            $diffLine = ($out | Where-Object { $_ -match 'Recovery Difficulty:' } | Select-Object -First 1)
            if (-not $diffLine) { throw "Missing Recovery Difficulty for $p" }
            "$p -> $($diffLine.Trim())"
        }
        $rows
    } -ArgumentList $VosBin
    $jobs += $jobExp
}

$null = Wait-Job -Job $jobs
foreach ($j in $jobs) {
    if ($j.State -ne "Completed") {
        Receive-Job -Job $j
        throw "Background test job failed ($($j.State))"
    }
    Receive-Job -Job $j
}
Remove-Job -Job $jobs
$swLive.Stop()
$totalSw.Stop()

Write-Host ""
Write-Host "ALL TESTS PASSED in $([math]::Round($totalSw.Elapsed.TotalSeconds, 2))s ($($totalSw.ElapsedMilliseconds) ms)" -ForegroundColor Green
Write-Host "  clippy: $($swClippy.ElapsedMilliseconds) ms | cargo test: $($swTest.ElapsedMilliseconds) ms | live sprint+audit: $($swLive.ElapsedMilliseconds) ms"
