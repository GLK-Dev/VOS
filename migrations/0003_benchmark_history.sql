CREATE TABLE IF NOT EXISTS benchmark_runs (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    suite_name            TEXT NOT NULL,
    total_cases           INTEGER NOT NULL,
    top1_passed           INTEGER NOT NULL,
    top1_total            INTEGER NOT NULL,
    top3_passed           INTEGER NOT NULL,
    top3_total            INTEGER NOT NULL,
    top5_passed           INTEGER NOT NULL,
    top5_total            INTEGER NOT NULL,
    mrr                   REAL NOT NULL,
    noise_hits            INTEGER NOT NULL,
    total_returned_hits   INTEGER NOT NULL,
    cross_project_matched INTEGER NOT NULL,
    cross_project_total   INTEGER NOT NULL,
    created_at            INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_benchmark_runs_time ON benchmark_runs(created_at DESC, id DESC);
