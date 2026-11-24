<#
.SYNOPSIS
Runs a parallelized v1.1.2 automated multi-project audit across all projects in ~/.vos/vos.db
and writes objective metrics & latencies to docs/sprint_trial_report.md.
#>
param(
    [string]$OutFile = (Join-Path (Split-Path -Parent $PSScriptRoot) "docs\sprint_trial_report.md"),
    [string]$VosBin = "vos",
    [int]$ThrottleLimit = 8
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$projLines = & $VosBin project list | Where-Object { $_ -match '^\S+\s+\(proj_' }
$projects = @()
for ($idx = 0; $idx -lt $projLines.Count; $idx++) {
    if ($projLines[$idx] -match '^([^\s]+)\s+\(proj_') {
        $projects += [pscustomobject]@{ Order = $idx; Name = $matches[1] }
    }
}

$benchPath = Join-Path (Split-Path -Parent $PSScriptRoot) "tests\search_live.yaml"

# Run read-only per-project audits in parallel across WAL-enabled SQLite
$rows = $projects | ForEach-Object -ThrottleLimit $ThrottleLimit -Parallel {
    $bin = $using:VosBin
    $p = $_.Name
    $order = $_.Order

    $swC = [System.Diagnostics.Stopwatch]::StartNew(); $contOut = & $bin continue $p; $swC.Stop()
    $swS = [System.Diagnostics.Stopwatch]::StartNew(); $statOut = & $bin status $p; $swS.Stop()
    $swD = [System.Diagnostics.Stopwatch]::StartNew(); $null = & $bin drift $p; $swD.Stop()
    $swG = [System.Diagnostics.Stopwatch]::StartNew(); $graphOut = & $bin graph $p --limit 5; $swG.Stop()
    $swO = [System.Diagnostics.Stopwatch]::StartNew(); $null = & $bin ownership $p; $swO.Stop()
    $swCov = [System.Diagnostics.Stopwatch]::StartNew(); $null = & $bin coverage $p; $swCov.Stop()

    $active = if (($contOut | Select-Object -First 1) -match '\((\d+)\s+active') { $matches[1] } else { "0" }
    $rels = if (($graphOut | Select-Object -First 1) -match '\((\d+)\s+relations') { $matches[1] } else { "0" }
    $health = if (($statOut | Where-Object { $_ -match '^Health:\s+(\S+)' } | Select-Object -First 1) -match '^Health:\s+(\S+)') { $matches[1] } else { "-" }
    $crit = if (($statOut | Where-Object { $_ -match '^Critical Risks:\s+(\d+)' } | Select-Object -First 1) -match '^Critical Risks:\s+(\d+)') { $matches[1] } else { "0" }
    $high = if (($statOut | Where-Object { $_ -match '^High Risks:\s+(\d+)' } | Select-Object -First 1) -match '^High Risks:\s+(\d+)') { $matches[1] } else { "0" }
    $covPct = if (($statOut | Where-Object { $_ -match '^Coverage:\s+([0-9\.]+%)' } | Select-Object -First 1) -match '^Coverage:\s+([0-9\.]+%)') { $matches[1] } else { "0.0%" }
    $docHealth = if (($statOut | Where-Object { $_ -match '^Documentation Health:\s+(\S+)' } | Select-Object -First 1) -match '^Documentation Health:\s+(\S+)') { $matches[1] } else { "-" }

    [pscustomobject]@{
        Order     = $order
        Project   = $p
        Active    = $active
        Relations = $rels
        Health    = $health
        DocHealth = $docHealth
        CovPct    = $covPct
        Crit      = $crit
        High      = $high
        MsC       = $swC.ElapsedMilliseconds
        MsS       = $swS.ElapsedMilliseconds
        MsD       = $swD.ElapsedMilliseconds
        MsG       = $swG.ElapsedMilliseconds
        MsO       = $swO.ElapsedMilliseconds
        MsCov     = $swCov.ElapsedMilliseconds
    }
} | Sort-Object Order

$benchOut = & $VosBin benchmark search --config $benchPath

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine("# VOS v1.1.2 Multi-Project Automated Sprint Audit")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("> Generated automatically on $(Get-Date -Format 'yyyy-MM-dd HH:mm') against live ``~/.vos/vos.db``.")
[void]$sb.AppendLine("> Measures machine latency and extracted Observation/Understanding/Decision signals across all indexed repositories.")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("| Project | Active | Relations | Health | Doc Health | Coverage | Crit | High | Continue (ms) | Status (ms) | Drift (ms) | Graph (ms) | Own (ms) | Cov (ms) |")
[void]$sb.AppendLine("| --- | ---: | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
foreach ($r in $rows) {
    [void]$sb.AppendLine("| $($r.Project) | $($r.Active) | $($r.Relations) | $($r.Health) | $($r.DocHealth) | $($r.CovPct) | $($r.Crit) | $($r.High) | $($r.MsC) | $($r.MsS) | $($r.MsD) | $($r.MsG) | $($r.MsO) | $($r.MsCov) |")
}
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
