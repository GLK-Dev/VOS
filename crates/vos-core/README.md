# vos-core

Shared domain models, error types, compact JSON projections, and deterministic path/hash/classification utilities for VOS (Velocity Orientation System).

## Module Map

- [`src/lib.rs`](src/lib.rs) — public re-exports for core models and helpers.
- [`src/models.rs`](src/models.rs) — `Project`, `ObjectRecord`, `RelationType`, `GitContext`, `ProjectContinue`, `ProjectStatus`, `ProjectGraph`, `WorkSession`, and benchmark/insight DTOs.
- [`src/compact.rs`](src/compact.rs) — token-efficient agent projections (`CompactContinue`, `CompactStatus`, `CompactWhy`, `CompactGraph`, `CompactSearchHit`).
- [`src/error.rs`](src/error.rs) — typed `VosError` definitions.
- [`src/util.rs`](src/util.rs) — `canonicalize_path`, file classification (`ObjectKind`), noise filtering, search visibility, and SHA-256 fingerprinting.