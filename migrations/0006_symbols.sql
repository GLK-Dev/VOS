CREATE TABLE IF NOT EXISTS symbols (
    id TEXT PRIMARY KEY,
    object_id TEXT NOT NULL REFERENCES objects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    signature TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_symbols_object ON symbols(object_id, start_line);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);

CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name,
    signature,
    kind,
    content='symbols',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts(rowid, name, signature, kind)
    VALUES (NEW.rowid, NEW.name, NEW.signature, NEW.kind);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, kind)
    VALUES ('delete', OLD.rowid, OLD.name, OLD.signature, OLD.kind);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, kind)
    VALUES ('delete', OLD.rowid, OLD.name, OLD.signature, OLD.kind);
    INSERT INTO symbols_fts(rowid, name, signature, kind)
    VALUES (NEW.rowid, NEW.name, NEW.signature, NEW.kind);
END;