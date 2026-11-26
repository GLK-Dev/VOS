# Changelog

All notable changes to **VOS (Velocity Orientation System)** are documented in this file.

## [2.0.0] - 2026-11-26

Official graduation of the `v1.2` -> `v2.0` innovation series from `experimental/recovery-predictor` into `main`, with **0** external AI/daemon dependencies and **35** MCP tools.

### Highlights & Added Capabilities

- **Adaptive Context Compiler (`vos pack` / `vos_pack`)**
  - Deterministic value-per-token context packing strictly bounded by `--budget <tokens>` (`128..128000`, `--raw` Markdown output).
  - Scores workspace pulse (`GitContext` + sessions/handoffs), `AreaLease`s, verified/stale `ProjectFact`s, ranked AST slices (`symbols_fts` + `slice_object`), `search_with_filters`, open questions, and 1-hop graph / `co_change` neighbors.
- **Self-Calibrating Recovery Telemetry (`vos calibrate` / `vos_calibrate`)**
  - Computes empirical `false_alarm_rate` and `missed_difficulty_rate` across completed `work_sessions` and tunes prediction weights dynamically (`CalibrationWeights` + dirty-Git penalty).
- **On-Demand Single-Binary Constellation HUD (`vos ui` / `vos_constellation`)**
  - Portfolio matrix (`vos ui --once`) and zero-daemon interactive localhost Constellation UI (`127.0.0.1:4173`) with SVG constellation map, multi-level drill-down (`Portfolio -> Project -> Hubs/Facts -> File/Symbols/Archaeology`), one-click session start/stop, and embedded `vos pack` compiler.
- **Pre-Flight Blast Simulator & Guardrails (`vos preflight`, `vos affected-tests`, `vos guard`, `vos.rules.toml`)**
  - Reverse-dependency blast simulation + historical temporal coupling + anchored facts + lease collision check.
  - Automated impacted test & minimal command discovery (`cargo test -p ...`, `pwsh`, `pytest`, `npm test`).
  - Declarative layer rules (`vos.rules.toml`) verified deterministically against code graph edges and source imports (`vos guard --strict` dogfooded on VOS itself).
- **Multi-Agent Cognitive Bus (`vos fact`, `vos claim`, `vos release`, `vos handoff`, `vos recall`)**
  - Hash-anchored engineering facts (`facts_fts`) with automatic `[Stale]` detection upon file content hash changes.
  - Time-bounded advisory `area_leases` for concurrent agents/developers, structured `agent_handoffs`, and cross-project solution recall (`vos recall`).
- **Decision Archaeology & Zero-Daemon Symbol Index (`vos archaeology`, `vos symbols`, `vos slice`)**
  - Deterministic Git commit + doc mention + `co_change` partner + session forensics.
  - SQLite `symbols_fts` index with exact AST/heuristic span slicing (`Rust`, `Python`, `TS/JS`, `C/C++`, `Markdown`).
- **Agent Native Layer & Onboarding (`vos mcp`, `vos init`, `vos watch`, `GUIL-1.0`)**
  - Stateless stdio JSON-RPC 2.0 MCP server (35 tools), global `--compact` single-line JSON mode, structured JSON errors on `stderr`.
  - One-command `vos init` (project registration, idempotent index, `.mcp.json` / `.vscode/mcp.json`), foreground `vos watch`, multi-language import/include reference extraction, deterministic `GitContext`, and project license (`LICENSE.md`, `GUIL-1.0`).

## [1.1.2] - 2026-11-24

- Stable SQLite + FTS5 project indexing, `vos start` / `vos continue` / `vos status` / `vos drift` / `vos graph` / `vos ownership` / `vos coverage` / `vos why` / `vos snapshot` / `vos session`, and string-literal-safe `TODO`/`FIXME` scanner (`5b122d6`).
