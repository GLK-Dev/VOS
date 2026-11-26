# vos-mcp

Stateless stdio **Model Context Protocol (MCP)** server crate for VOS (Velocity Orientation System).

## Architecture

- [`src/lib.rs`](src/lib.rs) — newline-delimited JSON-RPC 2.0 loop (`serve_stdio`, `serve_reader_writer`, `handle_request`), supporting `initialize`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call`, exiting cleanly on EOF with zero background daemons.
- [`src/tools.rs`](src/tools.rs) — JSON Schema tool definitions and deterministic dispatchers into `vos-store`, `vos-indexer`, and `vos-core::compact` (`CompactContinue`, `CompactStatus`, `CompactWhy`, `CompactGraph`, `CompactSearchHit`).

## Exposed Tools

- `vos_list_projects`
- `vos_continue`
- `vos_status`
- `vos_search`
- `vos_show`
- `vos_why`
- `vos_graph`
- `vos_drift`
- `vos_index`
- `vos_session_start`
- `vos_session_stop`

Launch via `vos mcp` (optionally with `--db <path>` or `VOS_DB`).