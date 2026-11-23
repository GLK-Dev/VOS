//! SQLite persistence, FTS5 search, and project context queries for VOS.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use tracing::{debug, info};
use vos_core::util::{canonicalize_path, is_path_within, new_id, now_unix};
use vos_core::{
    EventRecord, EventType, ObjectDetails, ObjectKind, ObjectRecord, Project, ProjectContext,
    ProjectSummary, SearchHit, VosError,
};

const MIGRATION_0001: &str = include_str!("../../../migrations/0001_init.sql");

/// Errors returned by the SQLite store layer.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("domain error: {0}")]
    Core(#[from] VosError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0} not found: {1}")]
    NotFound(&'static str, String),

    #[error("{0} already exists: {1}")]
    AlreadyExists(&'static str, String),

    #[error("invalid query: {0}")]
    InvalidInput(String),
}

/// Input payload for upserting an indexed object and its text chunks.
#[derive(Debug, Clone)]
pub struct UpsertObjectInput {
    pub project_id: Option<String>,
    pub path: String,
    pub kind: ObjectKind,
    pub title: String,
    pub summary: Option<String>,
    pub content_hash: String,
    pub size_bytes: i64,
    pub modified_at: i64,
    pub chunks: Vec<String>,
}

/// Result of an idempotent object upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertOutcome {
    Added(String),
    Modified(String),
    Restored(String),
    Unchanged(String),
}

/// SQLite-backed store for projects, objects, chunks, tags, and timeline events.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) a SQLite database at `path` and applies pending migrations.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init_connection(conn)
    }

    /// Opens an isolated in-memory database, primarily for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::init_connection(conn)
    }

    fn init_connection(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version TEXT PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );",
        )?;

        let applied: Option<String> = conn
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = ?1",
                params!["0001_init"],
                |row| row.get(0),
            )
            .optional()?;

        if applied.is_none() {
            conn.execute_batch(MIGRATION_0001)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                params!["0001_init", now_unix()],
            )?;
            info!("applied migration 0001_init");
        }

        Ok(Self { conn })
    }

    /// Creates a new project bound to an optional root directory.
    pub fn create_project(
        &self,
        name: &str,
        root_path: Option<&Path>,
        description: Option<&str>,
    ) -> Result<Project, StoreError> {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return Err(StoreError::InvalidInput(
                "project name cannot be empty".to_string(),
            ));
        }

        if self.get_project_by_name(trimmed_name)?.is_some() {
            return Err(StoreError::AlreadyExists(
                "project",
                trimmed_name.to_string(),
            ));
        }

        let canonical_root = match root_path {
            Some(p) => Some(canonicalize_path(p)?),
            None => None,
        };

        let now = now_unix();
        let project = Project {
            id: new_id("proj"),
            name: trimmed_name.to_string(),
            root_path: canonical_root,
            description: description.map(str::to_string),
            created_at: now,
            updated_at: now,
        };

        self.conn.execute(
            "INSERT INTO projects (id, name, root_path, description, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                project.id,
                project.name,
                project.root_path,
                project.description,
                project.created_at,
                project.updated_at,
            ],
        )?;

        info!(project = %project.name, id = %project.id, "created project");
        Ok(project)
    }

    /// Loads a project by its unique name.
    pub fn get_project_by_name(&self, name: &str) -> Result<Option<Project>, StoreError> {
        self.conn
            .query_row(
                "SELECT id, name, root_path, description, created_at, updated_at
                 FROM projects WHERE name = ?1",
                params![name],
                map_project_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Finds the most specific project whose `root_path` contains `canonical_path`.
    pub fn find_project_for_path(&self, canonical_path: &str) -> Result<Option<Project>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, root_path, description, created_at, updated_at
             FROM projects WHERE root_path IS NOT NULL",
        )?;
        let rows = stmt.query_map([], map_project_row)?;

        let mut best: Option<(usize, Project)> = None;
        for row in rows {
            let project = row?;
            if let Some(ref root) = project.root_path
                && is_path_within(canonical_path, root)
            {
                let len = root.len();
                let replace = best.as_ref().is_none_or(|(best_len, _)| len > *best_len);
                if replace {
                    best = Some((len, project));
                }
            }
        }

        Ok(best.map(|(_, p)| p))
    }

    /// Lists all projects with active/deleted object counts and last index timestamp.
    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, p.root_path, p.description, p.created_at, p.updated_at,
                    COALESCE(SUM(CASE WHEN o.is_deleted = 0 THEN 1 ELSE 0 END), 0) AS active_count,
                    COALESCE(SUM(CASE WHEN o.is_deleted = 1 THEN 1 ELSE 0 END), 0) AS deleted_count,
                    MAX(o.indexed_at) AS last_indexed
             FROM projects p
             LEFT JOIN objects o ON o.project_id = p.id
             GROUP BY p.id
             ORDER BY p.updated_at DESC, p.name ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let project = map_project_row(row)?;
            let active_objects: i64 = row.get(6)?;
            let deleted_objects: i64 = row.get(7)?;
            let last_indexed_at: Option<i64> = row.get(8)?;
            Ok(ProjectSummary {
                project,
                active_objects: active_objects.max(0) as usize,
                deleted_objects: deleted_objects.max(0) as usize,
                last_indexed_at,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
    }

    /// Idempotently inserts or updates an object and its chunks inside one transaction.
    pub fn upsert_object(&mut self, input: UpsertObjectInput) -> Result<UpsertOutcome, StoreError> {
        let now = now_unix();
        let tx = self.conn.transaction()?;

        let existing: Option<(String, Option<String>, String, i64)> = tx
            .query_row(
                "SELECT id, project_id, content_hash, is_deleted FROM objects WHERE path = ?1",
                params![input.path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;

        let outcome = match existing {
            None => {
                let obj_id = new_id("obj");
                tx.execute(
                    "INSERT INTO objects (
                        id, project_id, path, kind, title, summary, content_hash,
                        size_bytes, modified_at, indexed_at, is_deleted, deleted_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, NULL)",
                    params![
                        obj_id,
                        input.project_id,
                        input.path,
                        input.kind.as_str(),
                        input.title,
                        input.summary,
                        input.content_hash,
                        input.size_bytes,
                        input.modified_at,
                        now,
                    ],
                )?;

                insert_chunks(&tx, &obj_id, &input.chunks)?;
                insert_event(
                    &tx,
                    input.project_id.as_deref(),
                    Some(&obj_id),
                    EventType::Added,
                    &input.path,
                    None,
                    now,
                )?;
                touch_project(&tx, input.project_id.as_deref(), now)?;
                UpsertOutcome::Added(obj_id)
            }
            Some((obj_id, existing_proj, existing_hash, is_deleted)) => {
                let was_deleted = is_deleted != 0;
                if !was_deleted
                    && existing_hash == input.content_hash
                    && existing_proj == input.project_id
                {
                    tx.commit()?;
                    debug!(path = %input.path, "object unchanged; skipped");
                    return Ok(UpsertOutcome::Unchanged(obj_id));
                }

                tx.execute(
                    "UPDATE objects
                     SET project_id = ?1,
                         kind = ?2,
                         title = ?3,
                         summary = ?4,
                         content_hash = ?5,
                         size_bytes = ?6,
                         modified_at = ?7,
                         indexed_at = ?8,
                         is_deleted = 0,
                         deleted_at = NULL
                     WHERE id = ?9",
                    params![
                        input.project_id,
                        input.kind.as_str(),
                        input.title,
                        input.summary,
                        input.content_hash,
                        input.size_bytes,
                        input.modified_at,
                        now,
                        obj_id,
                    ],
                )?;

                tx.execute("DELETE FROM chunks WHERE object_id = ?1", params![obj_id])?;
                insert_chunks(&tx, &obj_id, &input.chunks)?;

                let event_type = if was_deleted {
                    EventType::Restored
                } else {
                    EventType::Modified
                };
                insert_event(
                    &tx,
                    input.project_id.as_deref(),
                    Some(&obj_id),
                    event_type,
                    &input.path,
                    None,
                    now,
                )?;
                touch_project(&tx, input.project_id.as_deref(), now)?;

                if was_deleted {
                    UpsertOutcome::Restored(obj_id)
                } else {
                    UpsertOutcome::Modified(obj_id)
                }
            }
        };

        tx.commit()?;
        Ok(outcome)
    }

    /// Soft-deletes active objects within `scope_path` that were not observed in `seen_paths`.
    pub fn mark_missing_deleted(
        &mut self,
        scope_path: &str,
        project_id: Option<&str>,
        seen_paths: &HashSet<String>,
    ) -> Result<usize, StoreError> {
        let now = now_unix();
        let tx = self.conn.transaction()?;

        let candidates: Vec<(String, Option<String>, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, project_id, path FROM objects
                 WHERE is_deleted = 0
                   AND (?1 IS NULL OR project_id = ?1)",
            )?;
            let rows = stmt.query_map(params![project_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut deleted_count = 0_usize;
        for (obj_id, obj_project_id, path) in candidates {
            if is_path_within(&path, scope_path) && !seen_paths.contains(&path) {
                tx.execute(
                    "UPDATE objects SET is_deleted = 1, deleted_at = ?1 WHERE id = ?2",
                    params![now, obj_id],
                )?;
                insert_event(
                    &tx,
                    obj_project_id.as_deref(),
                    Some(&obj_id),
                    EventType::Deleted,
                    &path,
                    None,
                    now,
                )?;
                touch_project(&tx, obj_project_id.as_deref(), now)?;
                deleted_count += 1;
            }
        }

        tx.commit()?;
        Ok(deleted_count)
    }

    /// Attaches manual tags to an existing object identified by ID or path.
    pub fn add_tags(&mut self, id_or_path: &str, tags: &[String]) -> Result<ObjectRecord, StoreError> {
        if tags.is_empty() {
            return Err(StoreError::InvalidInput(
                "at least one tag must be provided".to_string(),
            ));
        }

        let details = self.show_object(id_or_path)?;
        let tx = self.conn.transaction()?;

        for raw_tag in tags {
            let normalized = raw_tag.trim().to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO tags (object_id, tag, source) VALUES (?1, ?2, 'manual')",
                params![details.object.id, normalized],
            )?;
        }

        tx.commit()?;
        Ok(details.object)
    }

    /// Loads full metadata, project binding, and tags for an object by ID, path, or unique title.
    pub fn show_object(&self, id_or_path: &str) -> Result<ObjectDetails, StoreError> {
        let mut object = self.find_object_by_id_or_path(id_or_path)?;
        if object.is_none() {
            let candidate = Path::new(id_or_path);
            if candidate.exists() {
                let canonical = canonicalize_path(candidate)?;
                object = self.find_object_by_id_or_path(&canonical)?;
            }
        }
        if object.is_none() {
            object = self.find_unique_object_by_title(id_or_path)?;
        }

        let object =
            object.ok_or_else(|| StoreError::NotFound("object", id_or_path.to_string()))?;

        let project_name: Option<String> = match object.project_id.as_deref() {
            Some(pid) => self
                .conn
                .query_row(
                    "SELECT name FROM projects WHERE id = ?1",
                    params![pid],
                    |row| row.get(0),
                )
                .optional()?,
            None => None,
        };

        let mut tag_stmt = self
            .conn
            .prepare("SELECT tag, source FROM tags WHERE object_id = ?1 ORDER BY tag ASC")?;
        let tags = tag_stmt
            .query_map(params![object.id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;

        let chunk_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE object_id = ?1",
            params![object.id],
            |row| row.get(0),
        )?;

        Ok(ObjectDetails {
            object,
            project_name,
            tags,
            chunk_count: chunk_count.max(0) as usize,
        })
    }

    /// Performs full-text BM25 search across `chunks_fts` and `objects_fts`.
    pub fn search(
        &self,
        query: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let fts_query = build_fts_query(query)?;
        let capped_limit = limit.clamp(1, 200) as i64;

        let mut merged: HashMap<String, SearchHit> = HashMap::new();

        {
            let mut stmt = self.conn.prepare(
                "SELECT o.id, p.name, o.title, o.path, o.kind,
                        snippet(chunks_fts, 0, '[', ']', '...', 24) AS snip,
                        bm25(chunks_fts) AS rank
                 FROM chunks_fts
                 JOIN chunks c ON c.rowid = chunks_fts.rowid
                 JOIN objects o ON o.id = c.object_id
                 LEFT JOIN projects p ON p.id = o.project_id
                 WHERE chunks_fts MATCH ?1
                   AND o.is_deleted = 0
                   AND (?2 IS NULL OR p.name = ?2)
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(params![fts_query, project_name, capped_limit * 2], |row| {
                let kind_str: String = row.get(4)?;
                Ok(SearchHit {
                    object_id: row.get(0)?,
                    project_name: row.get(1)?,
                    title: row.get(2)?,
                    path: row.get(3)?,
                    kind: ObjectKind::from_db(&kind_str),
                    snippet: row.get(5)?,
                    score: row.get(6)?,
                })
            })?;

            for row in rows {
                let hit = row?;
                insert_best_hit(&mut merged, hit);
            }
        }

        {
            let mut stmt = self.conn.prepare(
                "SELECT o.id, p.name, o.title, o.path, o.kind,
                        snippet(objects_fts, -1, '[', ']', '...', 24) AS snip,
                        bm25(objects_fts, 3.0, 2.0, 1.0) AS rank
                 FROM objects_fts
                 JOIN objects o ON o.rowid = objects_fts.rowid
                 LEFT JOIN projects p ON p.id = o.project_id
                 WHERE objects_fts MATCH ?1
                   AND o.is_deleted = 0
                   AND (?2 IS NULL OR p.name = ?2)
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(params![fts_query, project_name, capped_limit], |row| {
                let kind_str: String = row.get(4)?;
                Ok(SearchHit {
                    object_id: row.get(0)?,
                    project_name: row.get(1)?,
                    title: row.get(2)?,
                    path: row.get(3)?,
                    kind: ObjectKind::from_db(&kind_str),
                    snippet: row.get(5)?,
                    score: row.get(6)?,
                })
            })?;

            for row in rows {
                let hit = row?;
                insert_best_hit(&mut merged, hit);
            }
        }

        let mut hits: Vec<SearchHit> = merged.into_values().collect();
        hits.sort_by(|a, b| a.score.total_cmp(&b.score));
        hits.truncate(limit.clamp(1, 200));
        Ok(hits)
    }

    /// Builds a recovery context snapshot for a project.
    pub fn project_context(
        &self,
        project_name: &str,
        recent_limit: usize,
    ) -> Result<ProjectContext, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let (active_objects, deleted_objects): (i64, i64) = self.conn.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN is_deleted = 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN is_deleted = 1 THEN 1 ELSE 0 END), 0)
             FROM objects
             WHERE project_id = ?1",
            params![project.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let mut kind_stmt = self.conn.prepare(
            "SELECT kind, COUNT(*) AS cnt
             FROM objects
             WHERE project_id = ?1 AND is_deleted = 0
             GROUP BY kind
             ORDER BY cnt DESC, kind ASC",
        )?;
        let counts_by_kind = kind_stmt
            .query_map(params![project.id], |row| {
                let k: String = row.get(0)?;
                let cnt: i64 = row.get(1)?;
                Ok((k, cnt.max(0) as usize))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut recent_stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at
             FROM objects
             WHERE project_id = ?1 AND is_deleted = 0
             ORDER BY modified_at DESC, indexed_at DESC
             LIMIT ?2",
        )?;
        let recent_objects = recent_stmt
            .query_map(params![project.id, recent_limit as i64], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let recent_events = self.project_timeline(&project.name, recent_limit)?;

        Ok(ProjectContext {
            project,
            active_objects: active_objects.max(0) as usize,
            deleted_objects: deleted_objects.max(0) as usize,
            counts_by_kind,
            recent_objects,
            recent_events,
        })
    }

    /// Returns chronological events for a project (newest first).
    pub fn project_timeline(
        &self,
        project_name: &str,
        limit: usize,
    ) -> Result<Vec<EventRecord>, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, object_id, event_type, path, detail, created_at
             FROM events
             WHERE project_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(params![project.id, limit.clamp(1, 200) as i64], |row| {
            let et: String = row.get(3)?;
            Ok(EventRecord {
                id: row.get(0)?,
                project_id: row.get(1)?,
                object_id: row.get(2)?,
                event_type: EventType::from_db(&et),
                path: row.get(4)?,
                detail: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
    }

    fn find_object_by_id_or_path(&self, id_or_path: &str) -> Result<Option<ObjectRecord>, StoreError> {
        self.conn
            .query_row(
                "SELECT id, project_id, path, kind, title, summary, content_hash,
                        size_bytes, modified_at, indexed_at, is_deleted, deleted_at
                 FROM objects
                 WHERE id = ?1 OR path = ?1",
                params![id_or_path],
                map_object_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    fn find_unique_object_by_title(&self, title: &str) -> Result<Option<ObjectRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at
             FROM objects
             WHERE title = ?1 AND is_deleted = 0
             LIMIT 2",
        )?;
        let mut matches = stmt
            .query_map(params![title], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        if matches.len() == 1 {
            Ok(matches.pop())
        } else {
            Ok(None)
        }
    }
}

fn map_project_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        root_path: row.get(2)?,
        description: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn map_object_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObjectRecord> {
    let kind_str: String = row.get(3)?;
    let is_deleted: i64 = row.get(10)?;
    Ok(ObjectRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        path: row.get(2)?,
        kind: ObjectKind::from_db(&kind_str),
        title: row.get(4)?,
        summary: row.get(5)?,
        content_hash: row.get(6)?,
        size_bytes: row.get(7)?,
        modified_at: row.get(8)?,
        indexed_at: row.get(9)?,
        is_deleted: is_deleted != 0,
        deleted_at: row.get(11)?,
    })
}

fn insert_chunks(tx: &rusqlite::Transaction<'_>, object_id: &str, chunks: &[String]) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO chunks (id, object_id, chunk_index, text) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (idx, chunk) in chunks.iter().enumerate() {
        let chunk_id = new_id("chk");
        stmt.execute(params![chunk_id, object_id, idx as i64, chunk])?;
    }
    Ok(())
}

fn insert_event(
    tx: &rusqlite::Transaction<'_>,
    project_id: Option<&str>,
    object_id: Option<&str>,
    event_type: EventType,
    path: &str,
    detail: Option<&str>,
    created_at: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO events (project_id, object_id, event_type, path, detail, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            project_id,
            object_id,
            event_type.as_str(),
            path,
            detail,
            created_at
        ],
    )?;
    Ok(())
}

fn touch_project(
    tx: &rusqlite::Transaction<'_>,
    project_id: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    if let Some(pid) = project_id {
        tx.execute(
            "UPDATE projects SET updated_at = ?1 WHERE id = ?2",
            params![now, pid],
        )?;
    }
    Ok(())
}

fn insert_best_hit(merged: &mut HashMap<String, SearchHit>, hit: SearchHit) {
    match merged.get(&hit.object_id) {
        Some(existing) if existing.score <= hit.score => {}
        _ => {
            merged.insert(hit.object_id.clone(), hit);
        }
    }
}

/// Escapes user terms into an FTS5 `AND` query so punctuation like `-` or `.` never causes syntax errors.
fn build_fts_query(raw: &str) -> Result<String, StoreError> {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();

    if tokens.is_empty() {
        return Err(StoreError::InvalidInput(
            "search query cannot be empty".to_string(),
        ));
    }

    Ok(tokens.join(" AND "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_search_and_soft_delete_stay_consistent() {
        let mut store = Store::open_in_memory().unwrap();
        let project = store.create_project("ai-os", None, Some("Kernel notes")).unwrap();

        let outcome = store
            .upsert_object(UpsertObjectInput {
                project_id: Some(project.id.clone()),
                path: "/tmp/ai-os/scheduler.rs".to_string(),
                kind: ObjectKind::Code,
                title: "scheduler.rs".to_string(),
                summary: Some("pub fn schedule_task()".to_string()),
                content_hash: "hash1".to_string(),
                size_bytes: 120,
                modified_at: 1000,
                chunks: vec!["pub fn schedule_task() { /* round robin */ }".to_string()],
            })
            .unwrap();
        assert!(matches!(outcome, UpsertOutcome::Added(_)));

        let hits = store.search("schedule_task", Some("ai-os"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "scheduler.rs");

        // Idempotent repeat upsert must not emit duplicate events.
        let repeat = store
            .upsert_object(UpsertObjectInput {
                project_id: Some(project.id.clone()),
                path: "/tmp/ai-os/scheduler.rs".to_string(),
                kind: ObjectKind::Code,
                title: "scheduler.rs".to_string(),
                summary: Some("pub fn schedule_task()".to_string()),
                content_hash: "hash1".to_string(),
                size_bytes: 120,
                modified_at: 1000,
                chunks: vec!["pub fn schedule_task() { /* round robin */ }".to_string()],
            })
            .unwrap();
        assert!(matches!(repeat, UpsertOutcome::Unchanged(_)));
        assert_eq!(store.project_timeline("ai-os", 10).unwrap().len(), 1);

        // Soft delete hides the object from FTS5 search while preserving timeline history.
        let deleted = store
            .mark_missing_deleted("/tmp/ai-os", Some(&project.id), &HashSet::new())
            .unwrap();
        assert_eq!(deleted, 1);
        assert!(store.search("schedule_task", None, 10).unwrap().is_empty());
        assert_eq!(store.project_timeline("ai-os", 10).unwrap().len(), 2);
    }
}
