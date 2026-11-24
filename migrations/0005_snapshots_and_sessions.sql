CREATE TABLE IF NOT EXISTS snapshots (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id           TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    label                TEXT,
    health               TEXT NOT NULL,
    active_objects       INTEGER NOT NULL,
    deleted_objects      INTEGER NOT NULL,
    relations_count      INTEGER NOT NULL,
    coverage_ratio       REAL NOT NULL,
    documentation_health TEXT NOT NULL,
    critical_risks       INTEGER NOT NULL,
    high_risks           INTEGER NOT NULL,
    open_questions_text  TEXT NOT NULL DEFAULT '',
    pending_work_text    TEXT NOT NULL DEFAULT '',
    active_areas_text    TEXT NOT NULL DEFAULT '',
    risk_areas_text      TEXT NOT NULL DEFAULT '',
    recent_changes_text  TEXT NOT NULL DEFAULT '',
    created_at           INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_snapshots_project_time ON snapshots(project_id, created_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS work_sessions (
    id                    TEXT PRIMARY KEY,
    project_id            TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    started_at            INTEGER NOT NULL,
    ended_at              INTEGER,
    duration_secs         INTEGER,
    predicted_difficulty  TEXT NOT NULL,
    predicted_score       INTEGER NOT NULL,
    recovery_quality      INTEGER,
    return_point          TEXT,
    start_note            TEXT,
    stop_note             TEXT,
    files_changed_count   INTEGER NOT NULL DEFAULT 0,
    productive_areas_text TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_work_sessions_project_time ON work_sessions(project_id, started_at DESC);
