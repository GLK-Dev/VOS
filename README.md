# VOS — Project Memory System

VOS is a local-first **Project Memory System** designed to help you recover project context quickly across code, notes, documents, and model files.

See [SPEC.md](SPEC.md) for the canonical v0.1 architecture and [AGENTS.md](AGENTS.md) for engineering rules.

## Quick Start (v0.1)

```bash
# Build the CLI
cargo build --release -p vos-cli

# Create a project bound to a directory
vos project create my-app ./path/to/project

# Index the directory (idempotent, tracks changes and deletions)
vos index ./path/to/project --project my-app

# Search across indexed files and chunks (SQLite FTS5 + BM25)
vos search "scheduler" --project my-app

# Inspect an object or recover project context
vos show ./path/to/project/src/main.rs
vos context my-app
vos timeline my-app
```
