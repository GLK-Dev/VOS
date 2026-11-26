CREATE TABLE IF NOT EXISTS facts (
    id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    object_id TEXT REFERENCES objects(id) ON DELETE SET NULL,
    path TEXT,
    symbol TEXT,
    fact_text TEXT NOT NULL,
    anchor_hash TEXT,
    author TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    verified_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_facts_project_status ON facts(project_id, status, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_facts_object ON facts(object_id);

CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    fact_text,
    path,
    symbol,
    content='facts',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS facts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, fact_text, path, symbol)
    VALUES (NEW.rowid, NEW.fact_text, COALESCE(NEW.path, ''), COALESCE(NEW.symbol, ''));
END;

CREATE TRIGGER IF NOT EXISTS facts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, fact_text, path, symbol)
    VALUES ('delete', OLD.rowid, OLD.fact_text, COALESCE(OLD.path, ''), COALESCE(OLD.symbol, ''));
END;

CREATE TRIGGER IF NOT EXISTS facts_au AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, fact_text, path, symbol)
    VALUES ('delete', OLD.rowid, OLD.fact_text, COALESCE(OLD.path, ''), COALESCE(OLD.symbol, ''));
    INSERT INTO facts_fts(rowid, fact_text, path, symbol)
    VALUES (NEW.rowid, NEW.fact_text, COALESCE(NEW.path, ''), COALESCE(NEW.symbol, ''));
END;

CREATE TABLE IF NOT EXISTS area_leases (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    area TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    note TEXT,
    claimed_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    UNIQUE(project_id, area)
);

CREATE INDEX IF NOT EXISTS idx_area_leases_project_exp ON area_leases(project_id, expires_at);

CREATE TABLE IF NOT EXISTS agent_handoffs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    from_agent TEXT NOT NULL,
    to_agent TEXT,
    summary TEXT NOT NULL,
    next_step TEXT,
    files_json TEXT,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_handoffs_project_time ON agent_handoffs(project_id, created_at DESC);