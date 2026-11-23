# VOS — Project Memory Platform

VOS is a local-first **Project Memory System** built on SQLite + FTS5 + deterministic heuristics to help you recover project context, track evolution, detect drift, and decide what to do next.

See [SPEC.md](SPEC.md) for the canonical specification and [AGENTS.md](AGENTS.md) for engineering rules.

## Project Status

**Version:** VOS v1.0.1

**Status:** Validated & Frozen

### Validation Summary

- 10 real projects indexed
- 778 active files
- 20/20 recovery scenarios passed
- 24/24 tests passing
- 0 clippy warnings

### Search Benchmark

- Top-1: 100.0%
- Top-3: 100.0%
- Top-5: 93.3%
- Cross-Project Recall: 100.0%

### Current Focus

Recovery Benchmark data collection.

No new features are planned until sufficient Recovery Benchmark data is collected.

### Known Watch Items

- Search quality monitoring on expanded corpus
- Top-5 drift monitoring
- MRR monitoring

## Quick Start

```bash
# Install locally
cargo install --path apps/vos-cli

# Register and idempotently index a project
vos project create my-app ./path/to/project
vos index ./path/to/project --project my-app

# Context Recovery & Continuation
vos start my-app
vos continue my-app
vos resume my-app

# Decision Support & Project Evolution
vos status my-app
vos evolution my-app --since 30d
vos drift my-app
vos changed-since 2026-11-01 my-app

# Search, Explainability & Quality Benchmarks
vos search "scheduler" --explain
vos why scheduler_fairness.rs
vos related scheduler
vos explain "project context"
vos benchmark search --config tests/search_live.yaml --save
vos benchmark history
```
