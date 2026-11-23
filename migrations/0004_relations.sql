CREATE TABLE IF NOT EXISTS relations (
    from_id       TEXT NOT NULL REFERENCES objects(id) ON DELETE CASCADE,
    to_id         TEXT NOT NULL REFERENCES objects(id) ON DELETE CASCADE,
    relation_type TEXT NOT NULL,
    confidence    REAL NOT NULL,
    detail        TEXT,
    PRIMARY KEY (from_id, to_id, relation_type)
);

CREATE INDEX IF NOT EXISTS idx_relations_from ON relations(from_id);
CREATE INDEX IF NOT EXISTS idx_relations_to ON relations(to_id);
