# VOS v0.1 Specification (Canonical)

> **Status:** Canonical v0.1 specification, distilled from `Documents/Idea/Planing1.md`.
> Any older architecture drafts in `Documents/Idea/` are exploratory notes only.

## 1. Vision & Scope

**VOS (Velocity Orientation System)** is a local-first **Project Memory System** built on top of an existing host OS (Windows / Linux):
a fast, deterministic tool that minimizes context-recovery time and helps a developer or AI agent answer:

- What belongs to this project (code, notes, docs, models)?
- What changed recently?
- Where did I stop, and how do I recover context quickly?

### v0.1 Non-Goals (Explicitly Out of Scope)

To avoid building infrastructure before proving daily utility, v0.1 **must not** include:

- Background daemon (`vosd`) or filesystem watchers (`notify`)
- Job queues or async worker pools
- Embeddings, vector databases, Ollama/LLM calls, RAG, or autonomous agents
- Relation graphs (deferred to v0.2)
- Custom shell, Wayland compositor, or desktop UI

---

## 2. Workspace Architecture

```text
vos/ (repo root)
├── Cargo.toml                  # Workspace manifest
├── config/
│   └── concepts.yaml           # Architectural concept dictionary
├── migrations/
│   ├── 0001_init.sql           # SQLite schema + FTS5 + triggers + indexes
│   ├── 0002_noise.sql          # Noise classification column and indexes
│   └── 0003_benchmark_history.sql # Saved benchmark accuracy runs
├── apps/
│   └── vos-cli/                # Binary `vos`: CLI parsing & terminal rendering only
└── crates/
    ├── vos-core/               # Domain models, error types, path/hash/classification helpers
    ├── vos-store/              # SQLite persistence, FTS5 search, context & timeline queries
    ├── vos-indexer/            # Directory walker, ignore rules, chunking, idempotent sync
    └── vos-mcp/                # Stateless stdio MCP JSON-RPC 2.0 server and compact tool dispatchers
```

**Layering rule:** `vos-cli` contains zero business logic or raw SQL; it parses arguments, calls services in `vos-store` / `vos-indexer` / `vos-mcp`, and formats output.

---

## 3. Canonical SQLite Schema (v0.1)

SQLite runs with `PRAGMA foreign_keys = ON;` and `PRAGMA journal_mode = WAL;`.

### Tables

1. **`projects`**
   - `id TEXT PRIMARY KEY`
   - `name TEXT NOT NULL UNIQUE`
   - `root_path TEXT`
   - `description TEXT`
   - `created_at INTEGER NOT NULL`
   - `updated_at INTEGER NOT NULL`

2. **`objects`**
   - `id TEXT PRIMARY KEY`
   - `project_id TEXT REFERENCES projects(id) ON DELETE SET NULL`
   - `path TEXT NOT NULL UNIQUE` (canonical absolute path)
   - `kind TEXT NOT NULL` (`code` | `doc` | `note` | `model` | `media` | `other`)
   - `title TEXT NOT NULL`
   - `summary TEXT` (first meaningful lines or extracted metadata)
   - `content_hash TEXT NOT NULL` (SHA-256 hex)
   - `size_bytes INTEGER NOT NULL`
   - `modified_at INTEGER NOT NULL`
   - `indexed_at INTEGER NOT NULL`
   - `is_deleted INTEGER NOT NULL DEFAULT 0`
   - `deleted_at INTEGER`
   - `is_noise INTEGER NOT NULL DEFAULT 0` (lockfiles, VCS/editor housekeeping)

3. **`chunks`**
   - `id TEXT PRIMARY KEY`
   - `object_id TEXT NOT NULL REFERENCES objects(id) ON DELETE CASCADE`
   - `chunk_index INTEGER NOT NULL`
   - `text TEXT NOT NULL`
   - `UNIQUE (object_id, chunk_index)`

4. **`tags`**
   - `object_id TEXT NOT NULL REFERENCES objects(id) ON DELETE CASCADE`
   - `tag TEXT NOT NULL`
   - `source TEXT NOT NULL DEFAULT 'manual'` (`auto` | `manual`)
   - `PRIMARY KEY (object_id, tag)`

5. **`events`** (Project Timeline)
   - `id INTEGER PRIMARY KEY AUTOINCREMENT`
   - `project_id TEXT REFERENCES projects(id) ON DELETE CASCADE`
   - `object_id TEXT REFERENCES objects(id) ON DELETE CASCADE`
   - `event_type TEXT NOT NULL` (`added` | `modified` | `deleted` | `restored` | `tagged`)
   - `path TEXT NOT NULL`
   - `detail TEXT`
   - `created_at INTEGER NOT NULL`

### Full-Text Search (FTS5 External Content)

- `objects_fts`: `USING fts5(title, path, summary, content='objects', content_rowid='rowid', tokenize='porter unicode61')` with `AFTER INSERT`, `AFTER UPDATE`, `AFTER DELETE` triggers.
- `chunks_fts`: `USING fts5(text, content='chunks', content_rowid='rowid', tokenize='porter unicode61')` with `AFTER INSERT`, `AFTER UPDATE`, `AFTER DELETE` triggers.

Soft-deleted objects (`is_deleted = 1`) are filtered out in search queries (`WHERE o.is_deleted = 0`).

---

## 4. Indexing Semantics

Running `vos index <path>` must be **idempotent**:

1. Walk `<path>` while respecting `.gitignore` / `.vosignore` (unless `--no-ignore`) and skipping ignored directories (`.git`, `.cargo`, `target`, `node_modules`, `.venv`, `__pycache__`, `dist`, `.vos`).
2. Classify each file (`code`, `note`, `doc`, `model`, `media`, `other`).
3. Compute SHA-256 `content_hash` and `modified_at`.
4. If the path exists in `objects` with the same `content_hash` and `is_deleted = 0`, skip re-chunking.
5. Otherwise upsert `objects`, replace `chunks` for text-readable files (`code`, `note`), and record an `events` entry (`added`, `modified`, or `restored`).
6. For objects scoped under the indexed path/project that are no longer on disk and have `is_deleted = 0`, mark `is_deleted = 1`, set `deleted_at`, and record a `deleted` event.

---

## 5. CLI Contract (v0.1)

- `vos project create <name> <path> [--description <text>]`
- `vos project list`
- `vos index <path> [--project <name>]`
- `vos search <query> [--project <name>] [--limit <n>] [--explain] [--include-internal]`
- `vos show <id_or_path>`
- `vos tag <id_or_path> <tags...>`
- `vos context <project> [--limit <n>] [--include-noise]`
- `vos timeline <project> [--limit <n>] [--flat]`
- `vos activity <project> [--days <n>] [--include-noise]`
- `vos start <project>`
- `vos continue <project>`
- `vos status <project>`
- `vos explain <concept> [--project <name>]`
- `vos changed-since <date> [project]`
- `vos evolution <project> [--since <date_or_Nd>]`
- `vos drift <project>`
- `vos resume <project>`
- `vos related <target> [--project <name>] [--limit <n>]`
- `vos why <target> [--project <name>]`
- `vos benchmark search [--config <path>] [--save]`
- `vos benchmark history [--limit <n>]`

Global flag: `--db <path>` (or `VOS_DB` env var, defaulting to `~/.vos/vos.db`).

---

## 6. v1.0 Feature Freeze & Next Focus

- **v0.2 (Validation-driven):** PDF/rich metadata extractors, explicit `relations`, optional local embeddings if FTS5 misses semantic queries.
- **v0.3 (Automation):** Background daemon (`vosd`) + filesystem watcher (`notify`) + local API.

---

## 7. VOS 1.1 – 1.2 Extensions (Implemented)

Beyond the canonical v0.1 baseline above, VOS 1.2 includes the following additive migrations, commands, and agent integrations:

- **Migrations:** `0001_init.sql`, `0002_noise.sql`, `0003_benchmark_history.sql`, `0004_relations.sql`, `0005_snapshots_and_sessions.sql`, `0006_symbols.sql`.
- **Insight, Snapshot & Workflow Commands:** `vos init`, `vos watch`, `vos archaeology`, `vos slice`, `vos symbols`, `vos graph`, `vos ownership`, `vos coverage`, `vos snapshot [--all]`, `vos compare-snapshot`, `vos drift [--all]`, `vos session start|stop|list|stats`.
- **Git Recovery Context:** `vos status` and `vos continue` attach deterministic `GitContext` (current branch, dirty counters, last commit hash/subject/timestamp) when a project root resides in a Git repository.
- **Agent Optimization & MCP:** global `--compact` single-line projected JSON, structured `{"error": "..."}` output on `stderr` under `--json`, and `vos mcp` (stateless newline-delimited JSON-RPC 2.0 server in `crates/vos-mcp`).
