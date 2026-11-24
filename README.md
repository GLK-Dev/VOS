# VOS — Project Memory Platform

VOS is a local-first **Project Memory System** built on SQLite + FTS5 + deterministic heuristics to help you recover project context, track evolution, detect drift, and decide what to do next.

See [SPEC.md](SPEC.md) for the canonical v0.1 specification, [AGENTS.md](AGENTS.md) for engineering rules, and the workspace crates ([models.rs](crates/vos-core/src/models.rs), [lib.rs](crates/vos-store/src/lib.rs), [indexer](crates/vos-indexer/src/lib.rs), [main.rs](apps/vos-cli/src/main.rs)) for implementation details.

## Project Status

**Version:** VOS 1.2.0-experimental (`experimental/recovery-predictor`)

**Status:** Validation Active (closed-loop recovery measurement active via `vos session` + `sprint_session.ps1`; `main` remains frozen at `VOS 1.1.2`)

### Validation Summary

- `10` real projects indexed (`780+` active objects, `1690+` deterministic relations)
- `76 / 76` workspace tests passing (`0` clippy warnings under `-D warnings`)
- `30 / 30` live sprint recovery scenarios passing (`~110ms` median CLI latency)
- `24 / 24` isolated multi-project session lifecycle checks passing (`active=0`)

### Search Benchmark (`tests/search_live.yaml`)

- Top-1: `100.0%` (`3/3`)
- Top-3: `100.0%` (`19/19`)
- Top-5: `95.6%` (`43/45`)
- Cross-Project Recall: `100.0%` (`6/6`)

### Current Focus

Collecting timed human Recovery Benchmark sessions (`scripts/sprint_session.ps1` + `scripts/analyze_recovery.ps1`) to evaluate `Recovery Difficulty`, `False Alarm` (`Hard -> Easy`), `Missed Difficulty` (`Easy -> Hard`), and per-signal utility (`Critical Risk`, `Hub Importance`, `Coverage Health`, `Ownership Hotspots`).

## Quick Start

```bash
# Install locally (release build)
cargo install --path apps/vos-cli

# Register and idempotently index a project
vos project create my-app ./path/to/project
vos project remove old-app
vos index ./path/to/project --project my-app
vos index --all

# Context Recovery & Continuation
vos start my-app
vos continue my-app
vos continue my-app --experimental
vos resume my-app

# Decision Support & Project Evolution
vos status my-app --experimental
vos status --all --experimental
vos evolution my-app --since 30d
vos drift my-app
vos drift --all
vos changed-since 2026-11-01 my-app

# Dependency Graph, Ownership & Coverage
vos graph my-app --experimental
vos graph my-app --target src/lib.rs --direction both
vos ownership my-app --days 14
vos coverage my-app --experimental
vos why src/lib.rs --project my-app --experimental

# Snapshots, Sessions & Scriptable JSON Output
vos snapshot my-app --label before-break
vos snapshot --all --label before-break
vos compare-snapshot 1 2
vos compare-snapshot --project my-app --live
vos session start my-app --note "Resume task"
vos session stop my-app --quality 5 --note "Next step"
vos session stats my-app
vos status my-app --experimental
vos status --all --experimental --json

# Search, Explainability & Quality Benchmarks
vos search "scheduler" --explain
vos search --tag auth --kind code --project my-app
vos search --since 7d --kind code --project my-app
vos tags --project my-app
vos tag src/lib.rs --project my-app core
vos untag src/lib.rs --project my-app core
vos related scheduler
vos explain "project context"
vos benchmark search --config tests/search_live.yaml --save --min-top5 90 --min-mrr 0.75
vos benchmark history
vos doctor
```