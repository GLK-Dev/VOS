# VOS — Gemini CLI & Antigravity Instructions

Follow `AGENTS.md` and `SPEC.md` strictly:
- **English-first:** All source code, comments, commit messages, and project documentation must be in English.
- **Safety & determinism:** No `todo!()`, `unimplemented!()`, fake metrics, or `.unwrap()` / `.expect()` outside tests. Prefer deterministic SQLite + FTS5 logic over AI/embeddings/daemons.
- **Strict crate separation:** `apps/vos-cli` only handles CLI parsing and output formatting; domain logic and SQL live in `crates/vos-core`, `crates/vos-store`, `crates/vos-indexer`, and `crates/vos-mcp`.
- **Idempotency & FTS5 sync:** Keep `vos index` idempotent and ensure deleted objects (`is_deleted = 1`) stay excluded from active search and graph queries.

## Project Memory & MCP Setup (`vos mcp`)

Connect Gemini CLI / Antigravity to the stateless stdio MCP server:

```json
{
  "mcpServers": {
    "vos": {
      "command": "vos",
      "args": ["mcp"]
    }
  }
}
```

When calling the CLI directly from shell commands, pass `--json --compact` to keep responses concise (~1-2 KB instead of verbose nested JSON):
- `vos continue <project> --experimental --json --compact`
- `vos status <project> --experimental --json --compact`
- `vos search "<query>" --project <project> --json --compact`
- `vos why <path> --project <project> --experimental --json --compact`

## Required Checks

```powershell
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pwsh -NoProfile -File scripts\test_all.ps1 -IncludeExperimental
pwsh -NoProfile -File scripts\test_24_sessions.ps1
```