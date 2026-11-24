<#
.SYNOPSIS
Runs a 24-session end-to-end workflow test (`snapshot`, `session start`, recovery query,
`session stop`, `session stats`) against an isolated copy of `~/.vos/vos.db` spanning all
10 indexed projects and all 3 Predicted Difficulty tiers (`Easy`, `Medium`, `Hard`).
Saves results to docs/experimental_session_validation.md without touching `~/.vos/vos.db`.
#>
param(
    [string]$VosBin = "vos",
    [string]$OutFile = (Join-Path (Split-Path -Parent $PSScriptRoot) "docs\experimental_session_validation.md")
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$liveDb = Join-Path $HOME ".vos\vos.db"
if (-not (Test-Path $liveDb)) {
    throw "Live VOS DB not found at $liveDb"
}

$tmpDb = Join-Path ([System.IO.Path]::GetTempPath()) ("vos_24_sessions_" + [guid]::NewGuid().ToString("N") + ".db")
Copy-Item -Path $liveDb -Destination $tmpDb -Force

try {
    # Clear any pre-existing sessions in the temp copy so metrics reflect exactly these 24 sessions
    $sessions = @(
        @{ Id = 1;  Proj = "rustallm";    Task = "Triage critical hubs";         Cmd = @("status", "rustallm", "--experimental");                 Expect = @("Critical Risks: 2", "src/tensor") },
        @{ Id = 2;  Proj = "rustallm";    Task = "Inspect transformer graph";      Cmd = @("graph", "rustallm", "--target", "transformer/mod.rs", "--direction", "both"); Expect = @("kv_cache.rs [rust_mod]", "src\lib.rs") },
        @{ Id = 3;  Proj = "rustallm";    Task = "Review roadmap drift";           Cmd = @("drift", "rustallm");                                  Expect = @("[roadmap drift]", "[critical risk]") },
        @{ Id = 4;  Proj = "tessera-os";  Task = "Resume large workspace";         Cmd = @("continue", "tessera-os");                             Expect = @("Hub node", "Ownership hotspots") },
        @{ Id = 5;  Proj = "tessera-os";  Task = "Audit coverage gaps";            Cmd = @("coverage", "tessera-os");                             Expect = @("Code Files: 367", "Underdocumented") },
        @{ Id = 6;  Proj = "tessera-os";  Task = "Check forge critical risk";      Cmd = @("status", "tessera-os");                               Expect = @("apps/forge", "ROADMAP.md:") },
        @{ Id = 7;  Proj = "vos";         Task = "Verify insight store deps";      Cmd = @("graph", "vos", "--target", "crates/vos-store/src/lib.rs"); Expect = @("batch.rs [rust_mod]", "graph.rs [rust_mod]") },
        @{ Id = 8;  Proj = "vos";         Task = "Verify clean TODO scanner";      Cmd = @("status", "vos");                                      Expect = @("Critical Risks: 0", "crates/vos-store") },
        @{ Id = 9;  Proj = "vos";         Task = "Explain concept from docs";      Cmd = @("explain", "project context", "--project", "vos");     Expect = @("Concept:", "SPEC.md") },
        @{ Id = 10; Proj = "tortor";      Task = "Trace core module";              Cmd = @("graph", "tortor", "--target", "src/core/mod.rs");     Expect = @("assembler.rs [rust_mod]", "torrent.rs [rust_mod]") },
        @{ Id = 11; Proj = "tortor";      Task = "Check active ownership";         Cmd = @("ownership", "tortor");                                Expect = @("Most Active Areas", "src/core") },
        @{ Id = 12; Proj = "tortor";      Task = "Check structural drift";         Cmd = @("drift", "tortor");                                    Expect = @("[structural risk drift]", "src/core") },
        @{ Id = 13; Proj = "auraclean";   Task = "Inspect architecture links";     Cmd = @("graph", "auraclean", "--target", "ARCHITECTURE.md");  Expect = @("lock_detector.rs [path_ref]") },
        @{ Id = 14; Proj = "auraclean";   Task = "Check warm status";              Cmd = @("status", "auraclean");                                Expect = @("Health: Warm", "Coverage: 12.5%") },
        @{ Id = 15; Proj = "velyx";       Task = "Check doc coverage";             Cmd = @("status", "velyx");                                    Expect = @("Documentation Health: Good", "Coverage: 133.3%") },
        @{ Id = 16; Proj = "velyx";       Task = "Trace whitepaper links";         Cmd = @("graph", "velyx", "--target", "README.md");            Expect = @("WHITEPAPER.md [md_link]") },
        @{ Id = 17; Proj = "cyberframe";  Task = "Trace layout widgets";           Cmd = @("graph", "cyberframe", "--target", "crates/cyber_core/src/lib.rs"); Expect = @("layout.rs [rust_mod]", "widgets.rs [rust_mod]") },
        @{ Id = 18; Proj = "cyberframe";  Task = "Search renderer symbol";         Cmd = @("search", "renderer", "--project", "cyberframe");      Expect = @("cyberframe") },
        @{ Id = 19; Proj = "claudecode";  Task = "Triage docless repo";            Cmd = @("status", "claudecode");                               Expect = @("missing README.md", "Documentation Health: Low") },
        @{ Id = 20; Proj = "claudecode";  Task = "Locate entry scripts";           Cmd = @("continue", "claudecode");                             Expect = @("[code]", "Continue from:") },
        @{ Id = 21; Proj = "cloudcode";   Task = "Resume micro repo";              Cmd = @("continue", "cloudcode");                              Expect = @("Project: cloudcode", "Continue from:") },
        @{ Id = 22; Proj = "cloudcode";   Task = "Check micro repo ownership";     Cmd = @("ownership", "cloudcode");                             Expect = @("Ownership: cloudcode") },
        @{ Id = 23; Proj = "vm-transfer"; Task = "Inspect micro status";           Cmd = @("status", "vm-transfer", "--experimental");            Expect = @("Recovery Difficulty: Easy", "Coverage: 0.0%") },
        @{ Id = 24; Proj = "vm-transfer"; Task = "Check micro drift";              Cmd = @("drift", "vm-transfer");                               Expect = @("Drift report: vm-transfer") }
    )

    $preStats = (& $VosBin --db $tmpDb session stats) 2>&1 | Out-String
    $preTotal = if ($preStats -match 'total=(\d+)') { [int]$matches[1] } else { 0 }
    $preCompleted = if ($preStats -match 'completed=(\d+)') { [int]$matches[1] } else { 0 }

    $snapStart = (& $VosBin --db $tmpDb snapshot rustallm --label "batch-start") | Out-String
    if ($LASTEXITCODE -ne 0) { throw "Initial snapshot failed: $snapStart" }

    $rows = @()
    $passedCount = 0

    foreach ($s in $sessions) {
        $startOut = (& $VosBin --db $tmpDb session start $s.Proj --note $s.Task) 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) { throw "session start failed for #$($s.Id) $($s.Proj): $startOut" }

        $predTier = if ($startOut -match 'Predicted Difficulty:\s+(\S+)\s+\(score:\s+(\d+)\)') { $matches[1] } else { "?" }
        $predScore = if ($startOut -match 'score:\s+(\d+)') { $matches[1] } else { "?" }
        $retPoint = if ($startOut -match 'Suggested return point:\s+(.+)') { $matches[1].Trim() } else { "-" }

        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $cmdArgs = $s.Cmd
        $cmdOut = (& $VosBin --db $tmpDb @cmdArgs) 2>&1 | Out-String
        $cmdExit = $LASTEXITCODE
        $sw.Stop()

        $ok = ($cmdExit -eq 0)
        $matched = 0
        foreach ($exp in $s.Expect) {
            if ($cmdOut.Contains($exp)) {
                $matched++
            } else {
                $ok = $false
            }
        }
        if ($ok) { $passedCount++ }

        # Deterministic signal quality score (1..5): verified assertions + doc/graph richness, never invented human ratings
        $qual = if (-not $ok) {
            1
        } elseif ($cmdOut.Contains("missing README.md") -or $cmdOut.Contains("(no documentation")) {
            3
        } elseif ($matched -ge 2) {
            5
        } else {
            4
        }

        $stopOut = (& $VosBin --db $tmpDb session stop $s.Proj --quality $qual --note "verified=$($matched)/$($s.Expect.Count) ($($sw.ElapsedMilliseconds)ms)") 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) { throw "session stop failed for #$($s.Id) $($s.Proj): $stopOut" }

        $rows += [pscustomobject]@{
            Id        = $s.Id
            Project   = $s.Proj
            Task      = $s.Task
            Predicted = "$predTier ($predScore)"
            Tier      = $predTier
            Quality   = $qual
            Ms        = $sw.ElapsedMilliseconds
            ReturnPt  = $retPoint
            Result    = if ($ok) { "PASS" } else { "FAIL" }
        }
    }

    $snapEnd = (& $VosBin --db $tmpDb snapshot rustallm --label "batch-end") | Out-String
    $id1 = if ($snapStart -match 'Saved snapshot #(\d+)') { $matches[1] } else { "1" }; $id2 = if ($snapEnd -match 'Saved snapshot #(\d+)') { $matches[1] } else { "2" }; $diffOut = (& $VosBin --db $tmpDb compare-snapshot $id1 $id2) 2>&1 | Out-String
    $statsOut = (& $VosBin --db $tmpDb session stats) 2>&1 | Out-String

    $expTotal = $preTotal + 24
    $expCompleted = $preCompleted + 24
    if (-not $statsOut.Contains("total=$expTotal completed=$expCompleted")) {
        throw "Expected 24 completed sessions in stats, got:`n$statsOut"
    }
    if (-not ($statsOut.Contains("Easy") -and $statsOut.Contains("Medium") -and $statsOut.Contains("Hard"))) {
        throw "Expected all 3 difficulty tiers (Easy, Medium, Hard) in session stats:`n$statsOut"
    }

    $sb = [System.Text.StringBuilder]::new()
    [void]$sb.AppendLine("# VOS 1.2.0-experimental — 24-Session Isolated Validation Run")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("> Executed on ``$(Get-Date -Format 'yyyy-MM-dd HH:mm')`` against an isolated copy of ``~/.vos/vos.db`` (``10`` real projects).")
    [void]$sb.AppendLine("> All sessions are agent-executed lifecycle tests (``vos session start`` -> recovery check -> ``vos session stop``). Human Recovery Benchmark sessions remain tracked separately.")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("## Summary")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("- **Sessions executed:** 24 / 24 completed (``active=0``)")
    [void]$sb.AppendLine("- **Scenario assertions passed:** $passedCount / 24")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine('```text')
    [void]$sb.AppendLine($statsOut.TrimEnd())
    [void]$sb.AppendLine('```')
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("## Per-Session Log")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("| # | Project | Task | Predicted | Signal Quality | CLI (ms) | Return Point | Result |")
    [void]$sb.AppendLine("| ---: | --- | --- | --- | ---: | ---: | --- | --- |")
    foreach ($r in $rows) {
        [void]$sb.AppendLine("| $($r.Id) | ``$($r.Project)`` | $($r.Task) | $($r.Predicted) | $($r.Quality)/5 | $($r.Ms) | ``$($r.ReturnPt)`` | $($r.Result) |")
    }
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("## Snapshot Diff Check (``compare-snapshot $id1 $id2``)")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine('```text')
    [void]$sb.AppendLine($diffOut.TrimEnd())
    [void]$sb.AppendLine('```')

    Set-Content -Path $OutFile -Value $sb.ToString() -Encoding UTF8

    Write-Host "24-Session Test: $passedCount / 24 PASSED | Report -> $OutFile" -ForegroundColor Green
    Write-Output $statsOut
    if ($passedCount -ne 24) {
        exit 1
    }
}
finally {
    if (Test-Path $tmpDb) {
        Remove-Item -Path $tmpDb -Force -ErrorAction SilentlyContinue
    }
}
