# VOS Product & Innovation Roadmap

> **VOS — Velocity Orientation System**
> Local-first deterministic project memory, causal archaeology, multi-agent coordination, and token-budgeted context compilation built in Rust + SQLite + FTS5.

Current baseline: **`v1.2.0-experimental`** (`experimental/recovery-predictor`, `main` frozen at `1.1.2`).
Engineering rule across every milestone: deterministic SQLite/Rust logic first, zero mandatory background daemons, explainable signals only, and measurable improvements in **Context Recovery Time**, **Recovery Quality**, and **Agent Token Efficiency**.

---

## Competitive Differentiation (Why VOS Has No Direct Peer)

| Existing Category | What It Does | Structural Blind Spot | How VOS Solves It Differently |
| --- | --- | --- | --- |
| AI chat memory (`Mem0`, static `CLAUDE.md`) | Stores prose memories | **Silent staleness** when files/commits change; massive token bloat | **Hash-anchored facts** (`expires_on_hash_change`) + live `GitContext` + `Compact*` schemas |
| RAG / Vector Search | Semantic chunk similarity | **Graph-blind**, slow, heavy dependencies, confuses icons/logs with core code | **FTS5 + AST symbols + deterministic dependency/co-change graph** in `<100ms` |
| Git CLI / IDE Search | Raw file text & commit history | Zero synthesis of "where did I stop, what risks exist, why does this file matter?" | **Cross-project Recovery Engine** measuring `Recovery Difficulty` & session ROI |
| Multi-Agent Editors | Isolated ad-hoc agent threads | Agents overwrite each other (`Hey` parallel commit race) & duplicate scans | **Shared SQLite Fact Graph, Area Leases, and Pre-Flight Blast Briefing** |

---

## Phase Overview

```text
v1.2 (Validation) ──▶ v1.3 (Causal Archaeology & AST) ──▶ v1.4 (Multi-Agent Cognitive Bus)
                                                                       │
                                                                       ▼
                       v2.0 (Adaptive Context Compiler & HUD) ◀── v1.5 (Architectural Guardrails)
```

---

## v1.2 — Validation & Release Gate (`Current -> main`)

- [x] **AI-Agent Native Layer:** `vos mcp` stateless stdio server (21 tools), `--compact` projections (~97% JSON size reduction on large repos), structured `{"error": "..."}` JSON output.
- [x] **One-Command Onboarding:** `vos init` (project registration + idempotent indexing + safe `.mcp.json` & `.vscode/mcp.json` scaffolding).
- [x] **Deterministic Git Context:** live branch, dirty file list (`changed_files`), and last commit summary inside `vos status` / `vos continue`.
- [x] **Multi-Language Static Graph:** Rust (`mod`/`use`/`Cargo.toml`), C/C++ (`#include`), TypeScript/JavaScript (`import`/`require`), Python (`import`/`from`), plus foreground `vos watch`.
- [ ] **20–30 Real Recovery Sessions Gate:** log real timed pause-recovery sessions across active projects (`docs/recovery_benchmark.md`) to audit `False Alarms` vs `Missed Difficulty` before merging `experimental/recovery-predictor` into `main`.

---

## v1.3 — Decision Archaeology & Symbol-Level Precision

Move from file-level memory to **symbol-level precision + causal "why was this code written?" memory** without running heavyweight LSP daemons.

### 1. Decision Archaeology (`vos archaeology <target>`)
- **Commit-to-Decision Linking:** deterministically mine Git log history, `ADR-*` headers, `OPEN QUESTION` / `РЕШЕНО` annotations, and `work_sessions` stop notes to bind every hotspot file to *the commit and rationale that shaped it*.
- **Temporal Co-Change Graph (`co_change` edges):** parse recent Git commits to detect pairs of files that repeatedly change together *without any static import* (e.g., Rust core logic + SQL migration + YAML benchmark + PowerShell test). Surface forgotten paired files right inside `vos continue` and `vos preflight`.

### 2. Zero-Daemon Symbol Index (`tree-sitter` AST Outline)
- Extract exact function/struct/trait/enum/component signatures and byte/line spans into `symbols` + `symbols_fts`.
- **Line-Range Agent Retrieval (`vos slice` / `vos_slice` MCP):** let AI agents fetch only the 35-line function + its callers rather than reading an 800-line file, cutting agent context cost another **5x–10x**.

---

## v1.4 — Multi-Agent Cognitive Bus & Cross-Repo Recall

Solve the real failure modes that appear when multiple AI assistants (`Claude Code`, `Antigravity`, `Gemini CLI`, `GitHub Copilot`) and a human developer collaborate on the same repo—and across multiple personal repos.

### 1. Hash-Anchored Verified Micro-Facts (`vos fact` / `vos_remember`)
- Store crisp, typed engineering facts pinned to file paths + SHA-256 `content_hash` + symbol names (e.g., *"Search `NON_WORK_ASSET_PENALTY = -40.0` ensures `.svg` icons never beat SQL migrations"*).
- **Auto-Staleness Detection:** the instant `vos index` or `vos watch` observes a hash change on the anchored file (or downstream dependency), VOS flags the fact as `stale_needs_review` instead of feeding outdated lies into an agent prompt.

### 2. Multi-Agent Workspace Leases & Handoff Protocol (`vos claim` / `vos handoff`)
- Lightweight area-level leases in SQLite (`agent:claude-code -> crates/hey-style`, TTL + heartbeat) surfaced in `vos_continue` and `vos_status` so two parallel agents don't edit the same crate simultaneously.
- Structured **Agent-to-Agent Handoff** records (edited files, failing checks left behind, verified commands run, exact next step).

### 3. Cross-Project Solution Recall (`vos recall "<symptom or pattern>"`)
- Search across **all registered projects' git commits, ADRs, stop notes, and symbol implementations** to surface solutions *you already solved elsewhere* (e.g., *"You fixed a 1 MiB Windows thread stack overflow in `VOS` commit `f9289fc` and built custom `chamfer` tests in `Hey`"*).

---

## v1.5 — Pre-Flight Blast Simulator & Architectural Guardrails

Turn VOS from passive memory into an active engineering safety shield executed locally in `<50ms`.

### 1. Pre-Edit Preflight Briefing (`vos preflight <paths...>`)
- Before changing critical hubs (e.g., `crates/hey-core/src/lib.rs` with 57 relations), emit a single compact report:
  1. Direct & transitive dependents at risk (`Graph`).
  2. Historical temporal co-change partners likely needing matching updates.
  3. Open questions, verified facts, and missing doc coverage attached to touched areas.
  4. Minimal crate/test commands covering the impacted blast radius.

### 2. Declarative Architecture Boundary Guard (`vos.rules.toml` + `vos guard`)
- Allow any repository to declare crisp structural invariants checked deterministically on the VOS relation graph:
  - *Example (`Hey`):* `crates/hey-core`, `crates/hey-layout`, `crates/hey-widgets` must have `0` edges into `winit` / `wgpu` / `tiny-skia`.
  - *Example (`VOS`):* `apps/vos-cli` must contain `0` direct SQL queries or `rusqlite` imports.
- Report boundary violations directly inside `vos drift`, CI gates, and MCP `vos_status`.

### 3. Test-to-Module Impact Selector (`vos affected-tests <paths...>`)
- Map test files to source modules via static graph + reverse edges so humans and AI agents verify changes with targeted test runs in sub-second feedback loops.

---

## v2.0 — Adaptive Context Compiler & Constellation HUD

### 1. Token-Budgeted Context Compiler (`vos pack "<goal>" --budget <tokens>`)
- Instead of guessing which 4–5 commands to run or dumping truncated files, solve a deterministic **Knapsack / Value-per-Token** packing pass over:
  - Active session + `GitContext` + stale-checked facts
  - Top ranked `symbols_fts` slices for `<goal>`
  - 1-hop `Graph` / `co_change` neighbors and `OPEN QUESTION`s
- Guarantees output fits strictly inside `--budget` (`2,000` tokens for local 7B–32B copilots like LM Studio, `6,000` for fast cloud coding, `16,000` for deep architectural planning).

### 2. Self-Calibrating Recovery Telemetry
- Replace static heuristic weights in `predict_recovery_difficulty` with deterministic empirical calibration fit over your own `work_sessions` dataset (adjusting weights per project complexity, open questions, and dirty git state from observed `duration_secs` and `recovery_quality`).

### 3. On-Demand Local Constellation HUD (`vos ui`)
- Interactive local-only terminal TUI (`ratatui`) or single-binary localhost visualization launched on demand (zero background service when closed):
  - Multi-project portfolio constellation (`VOS`, `Hey`, `TesseraOS`, `rustallm`, ...) showing live pulse, dirty git state, and critical risk flares.
  - Interactive zoom from **Portfolio -> Crate -> File -> AST Symbol -> Git Commit + Verified Fact**, with one-key session start/stop and recovery velocity charts.

---

## Deferred / Gate-Protected Features

- **Optional Semantic Embeddings (`vos search --semantic`):** intentionally deferred until `docs/recovery_benchmark.md` documents recurring queries across 20–30 real sessions that `FTS5 + concepts.yaml + AST symbols` fail to resolve.
- **Persistent Background Daemon (`vosd`):** deferred indefinitely unless foreground `vos watch`, `vos session start/stop`, and MCP on-demand re-indexing prove insufficient.