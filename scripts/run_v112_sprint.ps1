<#
.SYNOPSIS
Fast automated 30-scenario verification sprint for VOS v1.1.2 across all 10 indexed projects.
Runs Observation/Understanding/Decision checks, verifies assertions, measures latency,
and writes results to docs/v1_1_2_sprint_audit.md in ~2 seconds.
#>
param(
    [string]$OutFile = (Join-Path (Split-Path -Parent $PSScriptRoot) "docs\v1_1_2_sprint_audit.md")
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$scenarios = @(
    @{ Id = 1;  Project = "vos";         Task = "Dashboard & Critical Risk";   Args = @("status", "vos");                                    Expect = @("Health: Active", "Critical Risks:", "crates/vos-store") },
    @{ Id = 2;  Project = "vos";         Task = "Dependency graph tree";       Args = @("graph", "vos", "--limit", "3");                     Expect = @("Graph: vos", "crates\vos-store\src\lib.rs") },
    @{ Id = 3;  Project = "vos";         Task = "Explain target with deps";    Args = @("why", "crates/vos-store/src/lib.rs", "--project", "vos"); Expect = @("Dependencies:", "crates\vos-store\src\insight.rs") },
    @{ Id = 4;  Project = "vos";         Task = "Explain concept";             Args = @("explain", "project context", "--project", "vos");   Expect = @("Concept:", "Files contributing:") },
    @{ Id = 5;  Project = "tessera-os";  Task = "Resume & hubs";               Args = @("continue", "tessera-os");                           Expect = @("Hub node", "Ownership hotspots", "Coverage warnings") },
    @{ Id = 6;  Project = "tessera-os";  Task = "Dashboard triage";            Args = @("status", "tessera-os");                             Expect = @("Critical Risks: 1", "apps/forge", "ROADMAP.md:") },
    @{ Id = 7;  Project = "tessera-os";  Task = "Relation-weighted coverage";  Args = @("coverage", "tessera-os");                           Expect = @("Code Files: 367", "Well Covered", "Underdocumented") },
    @{ Id = 8;  Project = "tessera-os";  Task = "Activity ownership hotspots"; Args = @("ownership", "tessera-os");                          Expect = @("sdk/libtessera", "Most Stable Areas") },
    @{ Id = 9;  Project = "tessera-os";  Task = "Focused graph traversal";     Args = @("graph", "tessera-os", "--target", "docs/ROADMAP.md", "--depth", "1"); Expect = @("ROADMAP.md", "[md_link]") },
    @{ Id = 10; Project = "rustallm";    Task = "Continue prioritization";     Args = @("continue", "rustallm");                             Expect = @("Hub node (12 relations)", "src/compute") },
    @{ Id = 11; Project = "rustallm";    Task = "Status Critical Risk";        Args = @("status", "rustallm");                               Expect = @("Critical Risks: 2", "src/tensor", "src/loader") },
    @{ Id = 12; Project = "rustallm";    Task = "Structural & roadmap drift";  Args = @("drift", "rustallm");                                Expect = @("[critical risk]", "[structural risk drift]", "[roadmap drift]") },
    @{ Id = 13; Project = "rustallm";    Task = "Generic mod.rs related";      Args = @("related", "transformer/mod.rs", "--project", "rustallm"); Expect = @("layer.rs", "kv_cache.rs") },
    @{ Id = 14; Project = "rustallm";    Task = "Bidirectional graph";         Args = @("graph", "rustallm", "--target", "transformer/mod.rs", "--direction", "both"); Expect = @("kv_cache.rs [rust_mod]", "src\lib.rs [rust_mod]") },
    @{ Id = 15; Project = "tortor";      Task = "Core module hub graph";       Args = @("graph", "tortor", "--target", "src/core/mod.rs");   Expect = @("assembler.rs [rust_mod]", "torrent.rs [rust_mod]") },
    @{ Id = 16; Project = "tortor";      Task = "High-activity drift";         Args = @("drift", "tortor");                                  Expect = @("[structural risk drift]", "src/core") },
    @{ Id = 17; Project = "tortor";      Task = "Status coverage telemetry";   Args = @("status", "tortor");                                 Expect = @("Documentation Health: Low", "Dependency hubs:") },
    @{ Id = 18; Project = "auraclean";   Task = "Arch md/path graph";          Args = @("graph", "auraclean", "--target", "ARCHITECTURE.md"); Expect = @("lock_detector.rs [path_ref]") },
    @{ Id = 19; Project = "auraclean";   Task = "Config-safe drift";           Args = @("drift", "auraclean");                               Expect = @("Drift report: auraclean") },
    @{ Id = 20; Project = "auraclean";   Task = "Coverage ratio & areas";      Args = @("coverage", "auraclean");                            Expect = @("Coverage:   12.5%", "Partially Covered") },
    @{ Id = 21; Project = "velyx";       Task = "Doc-heavy Good health";       Args = @("status", "velyx");                                  Expect = @("Documentation Health: Good", "Coverage: 133.3%") },
    @{ Id = 22; Project = "velyx";       Task = "Whitepaper link graph";       Args = @("graph", "velyx", "--target", "README.md");          Expect = @("WHITEPAPER.md [md_link]") },
    @{ Id = 23; Project = "cyberframe";  Task = "Crate lib.rs mod graph";      Args = @("graph", "cyberframe", "--target", "crates/cyber_core/src/lib.rs"); Expect = @("layout.rs [rust_mod]", "measure.rs [rust_mod]") },
    @{ Id = 24; Project = "cyberframe";  Task = "Renderer search";             Args = @("search", "renderer", "--project", "cyberframe");    Expect = @("cyberframe") },
    @{ Id = 25; Project = "cyberframe";  Task = "Ownership areas";             Args = @("ownership", "cyberframe");                          Expect = @("crates/cyber_core", "Most Stable Areas") },
    @{ Id = 26; Project = "claudecode";  Task = "Honest empty-doc status";     Args = @("status", "claudecode");                             Expect = @("missing README.md", "Documentation Health: Low") },
    @{ Id = 27; Project = "claudecode";  Task = ".bat classified as code";     Args = @("continue", "claudecode");                           Expect = @("[code]", "Coverage warnings") },
    @{ Id = 28; Project = "cloudcode";   Task = "Small project continue";      Args = @("continue", "cloudcode");                            Expect = @("Project: cloudcode", "Continue from:") },
    @{ Id = 29; Project = "vm-transfer"; Task = "Small project status";        Args = @("status", "vm-transfer");                            Expect = @("Project: vm-transfer", "Health:") },
    @{ Id = 30; Project = "vos";         Task = "Idempotent re-index check";   Args = @("index", "E:\VSCode projects\VOS", "--project", "vos"); Expect = @("Updated:   0", "Deleted:   0", "Relations:") }
)

$results = @()
$passed = 0
$latencies = @()

foreach ($s in $scenarios) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $cmdArgs = $s.Args
    $raw = (& vos @cmdArgs) 2>&1 | Out-String
    $exit = $LASTEXITCODE
    $sw.Stop()
    $ms = $sw.ElapsedMilliseconds
    $latencies += $ms

    $ok = ($exit -eq 0)
    $missing = @()
    foreach ($pat in $s.Expect) {
        if (-not $raw.Contains($pat)) {
            $ok = $false
            $missing += $pat
        }
    }
    if ($ok) { $passed++ }
    $cmdLine = "vos " + (($s.Args | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }) -join " ")
    $results += [pscustomobject]@{
        Id      = $s.Id
        Project = $s.Project
        Task    = $s.Task
        Command = $cmdLine
        Ms      = $ms
        Status  = if ($ok) { "PASS" } else { "FAIL ($($missing -join '; '))" }
    }
}

$sorted = $latencies | Sort-Object
$median = $sorted[[int]([math]::Floor($sorted.Count / 2))]
$mean = [math]::Round(($latencies | Measure-Object -Average).Average, 1)

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine("# VOS v1.1.2 Automated Verification Sprint")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("> Executed on ``$(Get-Date -Format 'yyyy-MM-dd HH:mm')`` across 10 real local projects.")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("## Summary")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("- **Scenarios executed:** $($results.Count)")
[void]$sb.AppendLine("- **Passed:** $passed / $($results.Count) ($([math]::Round(100.0 * $passed / $results.Count, 1))%)")
[void]$sb.AppendLine("- **Median CLI latency:** $median ms (Mean: $mean ms)")
[void]$sb.AppendLine("")
[void]$sb.AppendLine("| # | Project | Task | Command | Latency (ms) | Result |")
[void]$sb.AppendLine("| ---: | --- | --- | --- | ---: | --- |")
foreach ($r in $results) {
    [void]$sb.AppendLine("| $($r.Id) | ``$($r.Project)`` | $($r.Task) | ``$($r.Command)`` | $($r.Ms) | $($r.Status) |")
}

Set-Content -Path $OutFile -Value $sb.ToString() -Encoding UTF8
Write-Host "Sprint passed: $passed / $($results.Count) | Median: $median ms | Mean: $mean ms"
foreach ($r in $results | Where-Object { $_.Status -ne "PASS" }) {
    Write-Host "FAILED #$($r.Id) ($($r.Project)): $($r.Status)" -ForegroundColor Red
}
