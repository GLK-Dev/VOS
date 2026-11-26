# VOS Validation & Benchmark Scripts

Deterministic PowerShell automation suites covering build gates, search accuracy, session lifecycles, and live multi-repo recovery audits:

- [`test_all.ps1`](test_all.ps1) — runs `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and live sprint scenarios (`-IncludeExperimental`).
- [`test_24_sessions.ps1`](test_24_sessions.ps1) — isolated 24-session recovery lifecycle validation across live project copies.
- [`run_sprint_audit.ps1`](run_sprint_audit.ps1) — live multi-project latency + `search_live.yaml` threshold audit (`--min-top5 90 --min-mrr 0.75`).
- [`sprint_session.ps1`](sprint_session.ps1) — interactive timed Recovery Benchmark session runner.
- [`analyze_recovery.ps1`](analyze_recovery.ps1) — computes Predicted-vs-Actual calibration metrics from `vos session stats`.
- [`run_v112_sprint.ps1`](run_v112_sprint.ps1) — regression audit script for v1.1.2/v1.2 insight checks.