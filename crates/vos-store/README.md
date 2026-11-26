# vos-store

SQLite persistence, FTS5 search, project context recovery, dependency graph, and insight analytics crate for VOS (Velocity Orientation System).

## Module Map

- [`src/archaeology.rs`](src/archaeology.rs) — Git commit archaeology and temporal co-change partner detection.
- [`src/symbols.rs`](src/symbols.rs) — `symbols_fts` queries, file outlines, and `slice_object`.
- [`src/lib.rs`](src/lib.rs) — `Store` connection lifecycle, migrations, project CRUD, tags, and [`doctor`](src/lib.rs) integrity repair.
- [`src/batch.rs`](src/batch.rs) — transactional `IndexBatch` upserts, soft-deletes, and `sync_relations`.
- [`src/queries.rs`](src/queries.rs) — `show_object_scoped`, `project_context`, and `project_timeline`.
- [`src/search.rs`](src/search.rs) — FTS5 + `SearchFilters` ranking, snippets, and explain signals.
- [`src/recovery.rs`](src/recovery.rs) — `project_resume`, `project_start`, `project_continue`, `why_object`, and `find_related`.
- [`src/decision.rs`](src/decision.rs) — `project_status`, `explain_concept`, and `changed_since`.
- [`src/git.rs`](src/git.rs) — deterministic local Git branch, dirty counters, and last-commit inspection (`GitContext`).
- [`src/graph.rs`](src/graph.rs) — directed dependency traversal (`project_graph`, `object_relations`).
- [`src/insight.rs`](src/insight.rs) — `project_ownership`, `project_coverage`, and `analyze_area_risks`.
- [`src/evolution.rs`](src/evolution.rs) — `project_evolution`, `project_drift`, and `all_project_drifts`.
- [`src/workflow.rs`](src/workflow.rs) — `ProjectSnapshot` diffs, `WorkSession` tracking, and `SessionStats`.
- [`src/experimental.rs`](src/experimental.rs) — `predict_recovery_difficulty`, `experimental_hub_importance`, `experimental_doc_quality`, and `experimental_why_impact`.
- [`src/benchmark.rs`](src/benchmark.rs) — YAML search accuracy evaluator and `benchmark_runs` history.
- [`src/cognitive.rs`](src/cognitive.rs) — hash-anchored facts (`facts_fts`), area leases, handoffs, and cross-project recall.
- [`src/concepts.rs`](src/concepts.rs) — concept expansion and query intent detection from `config/concepts.yaml`.