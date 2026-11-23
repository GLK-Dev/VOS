PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    root_path   TEXT,
    description TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS objects (
    id           TEXT PRIMARY KEY,
    project_id   TEXT REFERENCES projects(id) ON DELETE SET NULL,
    path         TEXT NOT NULL UNIQUE,
    kind         TEXT NOT NULL,
    title        TEXT NOT NULL,
    summary      TEXT,
    content_hash TEXT NOT NULL,
    size_bytes   INTEGER NOT NULL,
    modified_at  INTEGER NOT NULL,
    indexed_at   INTEGER NOT NULL,
    is_deleted   INTEGER NOT NULL DEFAULT 0,
    deleted_at   INTEGER
);

CREATE INDEX IF NOT EXISTS idx_objects_project ON objects(project_id);
CREATE INDEX IF NOT EXISTS idx_objects_kind ON objects(kind);
CREATE INDEX IF NOT EXISTS idx_objects_deleted ON objects(is_deleted);

CREATE TABLE IF NOT EXISTS chunks (
    id          TEXT PRIMARY KEY,
    object_id   TEXT NOT NULL REFERENCES objects(id) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL,
    text        TEXT NOT NULL,
    UNIQUE (object_id, chunk_index)
);

CREATE INDEX IF NOT EXISTS idx_chunks_object ON chunks(object_id);

CREATE TABLE IF NOT EXISTS tags (
    object_id TEXT NOT NULL REFERENCES objects(id) ON DELETE CASCADE,
    tag       TEXT NOT NULL,
    source    TEXT NOT NULL DEFAULT 'manual',
    PRIMARY KEY (object_id, tag)
);

CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);

CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    object_id  TEXT REFERENCES objects(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    path       TEXT NOT NULL,
    detail     TEXT,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_project_time ON events(project_id, created_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS objects_fts USING fts5(
    title,
    path,
    summary,
    content='objects',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS objects_ai AFTER INSERT ON objects BEGIN
    INSERT INTO objects_fts(rowid, title, path, summary)
    VALUES (NEW.rowid, NEW.title, NEW.path, COALESCE(NEW.summary, ''));
END;

CREATE TRIGGER IF NOT EXISTS objects_ad AFTER DELETE ON objects BEGIN
    INSERT INTO objects_fts(objects_fts, rowid, title, path, summary)
    VALUES ('delete', OLD.rowid, OLD.title, OLD.path, COALESCE(OLD.summary, ''));
END;

CREATE TRIGGER IF NOT EXISTS objects_au AFTER UPDATE ON objects BEGIN
    INSERT INTO objects_fts(objects_fts, rowid, title, path, summary)
    VALUES ('delete', OLD.rowid, OLD.title, OLD.path, COALESCE(OLD.summary, ''));
    INSERT INTO objects_fts(rowid, title, path, summary)
    VALUES (NEW.rowid, NEW.title, NEW.path, COALESCE(NEW.summary, ''));
END;

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    text,
    content='chunks',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, text)
    VALUES (NEW.rowid, NEW.text);
END;

CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text)
    VALUES ('delete', OLD.rowid, OLD.text);
END;

CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text)
    VALUES ('delete', OLD.rowid, OLD.text);
    INSERT INTO chunks_fts(rowid, text)
    VALUES (NEW.rowid, NEW.text);
END;
