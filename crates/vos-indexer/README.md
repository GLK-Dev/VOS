# vos-indexer

Deterministic filesystem walker, ignore filter, content chunker, multi-language relation extractor, project bootstrapper, and foreground watch engine for VOS.

## Module Map

- [`src/lib.rs`](src/lib.rs) — `index_path`, `index_path_with_options`, `IndexOptions`, `IndexReport`, and chunk extraction.
- [`src/relations.rs`](src/relations.rs) — Rust (`mod`/`use`/`Cargo.toml`), Markdown link, and path reference extraction + scoped relation resolution.
- [`src/lang_refs.rs`](src/lang_refs.rs) — C/C++ `#include`, TypeScript/JavaScript relative `import`/`require`, and Python `import`/`from` extractors.
- [`src/setup.rs`](src/setup.rs) — idempotent project bootstrapper (`init_project`, `InitOptions`, `InitReport`, `.mcp.json` / `.vscode/mcp.json` scaffolding).
- [`src/symbols.rs`](src/symbols.rs) — deterministic AST & Markdown outline/line-span extractor (Rust, TS/JS, Python, C/C++, Markdown).
- [`src/watch.rs`](src/watch.rs) — deterministic foreground filesystem watch loop (`watch_path`, `WatchOptions`).
- [`tests/search_benchmark.rs`](tests/search_benchmark.rs) — fixture search accuracy suite.