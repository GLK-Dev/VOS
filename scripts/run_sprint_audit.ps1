<#
.SYNOPSIS
Runs an automated multi-project audit (continue, status, drift, evolution, benchmark)
across all projects in ~/.vos/vos.db and writes objective timings & signal counts
to docs/sprint_trial_report.md. Never invents human recovery times.
#>
param(
    [string]$OutFile = (Join-Path (Split-Path -Parent $PSScriptRoot) "docs\sprint_trial_report.md")
)

$ErrorActionPreference = "Stop"
$projLines = vos project list | Where-Object { $_ -match '^(\S+)\s+\(proj_' }
$projects = foreach ($line in $projLines) {
    if ($line -match '^([^\s]+)\s+\(proj_') { $matches[1] }
}

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine("# VOS Multi-Project Automated Sprint Audit")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("> Generated automatically on $(Get-Date -Format 'yyyy-MM-dd HH:mm') against live ``~/.vos/vos.db``.")
[void]$sb.AppendLine("> Measures machine latency and extracted signals across real repositories (human Recovery Time & Quality 1-5 are logged separately in ``docs/recovery_benchmark.md``).")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("| Project | Active Files | Health | Last Activity | Pending Tasks | Drift Findings | Continue (ms) | Status (ms) | Drift (ms) |")
[void]$sb.AppendLine("| --- | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: |")

foreach ($p in $projects) {
    $swC = [System.Diagnostics.Stopwatch]::StartNew()
    $contOut = vos continue $p
    $swC.Stop()

    $swS = [System.Diagnostics.Stopwatch]::StartNew()
    $statOut = vos status $p
    $swS.Stop()

    $swD = [System.Diagnostics.Stopwatch]::StartNew()
    $driftOut = vos drift $p
    $swD.Stop()

    $active = if (($contOut | Select-Object -First 1) -match '\((\d+)\s+active') { $matches[1] } else { "0" }
    $health = if (($statOut | Where-Object { $_ -match '^Health:\s+(\S+)' } | Select-Object -First 1) -match '^Health:\s+(\S+)') { $matches[1] } else { "-" }
    $lastActLine = ($contOut | Where-Object { $_ -match '^\s+\d+\s+\w+\(s\)\s+ago' -or $_ -match '^\s+just now' } | Select-Object -First 1)
    $lastAct = if ($lastActLine) { $lastActLine.Trim() } else { "-" }

    $pendingCount = ($statOut | Where-Object { $_ -match '^\s+-\s+' -and $_ -notmatch '^\s+-\s+\w' } | Measure-Object).Count
    # Count items strictly under Pending work:
    $inPending = $false
    $pendingCount = 0
    foreach ($l in $statOut) {
        if ($l -eq "Pending work:") { $inPending = $true; continue }
        if ($inPending -and ($l -match '^[A-Z]' -or [string]::IsNullOrWhiteSpace($l))) {
            if ($l -match '^[A-Z]') { $inPending = $false }
            continue
        }
        if ($inPending -and $l -match '^\s+-\s+') { $pendingCount++ }
    }

    $driftCount = ($driftOut | Where-Object { $_ -match '^\[(documentation|roadmap) drift\]' } | Measure-Object).Count

    [void]$sb.AppendLine("| $p | $active | $health | $lastAct | $pendingCount | $driftCount | $($swC.ElapsedMilliseconds) | $($swS.ElapsedMilliseconds) | $($swD.ElapsedMilliseconds) |")
}

$benchOut = vos benchmark search --config (Join-Path (Split-Path -Parent $PSScriptRoot) "tests\search_live.yaml")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("## Live 50-Query Search Benchmark Snapshot")
[void]$sb.AppendLine("")
[void]$sb.AppendLine('```text')
foreach ($l in ($benchOut | Select-Object -Last 18)) {
    [void]$sb.AppendLine($l)
}
[void]$sb.AppendLine('```')

Set-Content -Path $OutFile -Value $sb.ToString() -Encoding UTF8
Write-Host "Audit report written to $OutFile"
