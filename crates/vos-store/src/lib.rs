//! SQLite persistence, FTS5 search, and project context queries for VOS.

mod batch;
mod benchmark;
mod concepts;
mod decision;
mod evolution;
mod experimental;
mod graph;
mod insight;
mod queries;
mod recovery;
mod search;
mod workflow;

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use tracing::info;
use vos_core::VosError;
use vos_core::util::{canonicalize_path, is_path_within, new_id, now_unix};
use vos_core::{DoctorReport, EventType, ObjectRecord, Project, ProjectSummary};

pub use batch::{IndexBatch, RelationInput, UpsertObjectInput, UpsertOutcome};
pub use benchmark::load_benchmark_cases;
pub use graph::ActiveObjectRef;

const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../../../migrations/0001_init.sql")),
    ("0002_noise", include_str!("../../../migrations/0002_noise.sql")),
    ("0003_benchmark_history", include_str!("../../../migrations/0003_benchmark_history.sql")),
    ("0004_relations", include_str!("../../../migrations/0004_relations.sql")),
    ("0005_snapshots_and_sessions", include_str!("../../../migrations/0005_snapshots_and_sessions.sql")),
];

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

/// SQLite-backed store for projects, objects, chunks, tags, and timeline events.
pub struct Store {
    pub(crate) conn: Connection,
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
        conn.pragma_update(None, "synchronous", "NORMAL")?;
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

        for &(version, sql) in MIGRATIONS {
            let applied: Option<String> = conn
                .query_row(
                    "SELECT version FROM schema_migrations WHERE version = ?1",
                    params![version],
                    |row| row.get(0),
                )
                .optional()?;

            if applied.is_none() {
                conn.execute_batch(sql)?;
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, now_unix()],
                )?;
                info!(migration = version, "applied SQLite migration");
            }
        }

        Ok(Self { conn })
    }

    /// Starts a single SQLite transaction for a full indexing run.
    pub fn begin_index_batch(&mut self) -> Result<IndexBatch<'_>, StoreError> {
        let tx = self.conn.transaction()?;
        Ok(IndexBatch {
            tx,
            now: now_unix(),
        })
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

    /// Removes a project and all associated objects, chunks, tags, relations, events, snapshots, and sessions.
    pub fn delete_project(&mut self, name: &str) -> Result<usize, StoreError> {
        let project = self
            .get_project_by_name(name)?
            .ok_or_else(|| StoreError::NotFound("project", name.to_string()))?;

        let tx = self.conn.transaction()?;
        let removed_objects = tx.execute(
            "DELETE FROM objects WHERE project_id = ?1",
            params![project.id],
        )?;
        tx.execute("DELETE FROM events WHERE project_id = ?1", params![project.id])?;
        tx.execute("DELETE FROM projects WHERE id = ?1", params![project.id])?;
        tx.commit()?;

        info!(project = %project.name, removed_objects, "removed project");
        Ok(removed_objects)
    }

    /// Loads a project by its unique name.
    pub fn get_project_by_name(&self, name: &str) -> Result<Option<Project>, StoreError> {
        self.conn
            .query_row(
                "SELECT id, name, root_path, description, created_at, updated_at
                 FROM projects WHERE name = ?1",
                params![name],
                queries::map_project_row,
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
        let rows = stmt.query_map([], queries::map_project_row)?;

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
            let project = queries::map_project_row(row)?;
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

    /// Convenience single-object upsert in its own transaction.
    pub fn upsert_object(&mut self, input: UpsertObjectInput) -> Result<UpsertOutcome, StoreError> {
        let mut batch = self.begin_index_batch()?;
        let outcome = batch.upsert_object(input)?;
        batch.commit()?;
        Ok(outcome)
    }

    /// Convenience soft-delete sweep in its own transaction.
    pub fn mark_missing_deleted(
        &mut self,
        scope_path: &str,
        project_id: Option<&str>,
        seen_paths: &HashSet<String>,
    ) -> Result<usize, StoreError> {
        let mut batch = self.begin_index_batch()?;
        let count = batch.mark_missing_deleted(scope_path, project_id, seen_paths)?;
        batch.commit()?;
        Ok(count)
    }

    /// Attaches manual tags to an object identified by ID or path and records a `tagged` event.
    pub fn add_tags(&mut self, id_or_path: &str, tags: &[String]) -> Result<ObjectRecord, StoreError> {
        self.add_tags_scoped(id_or_path, None, tags)
    }

    /// Attaches manual tags to an object, optionally scoped by `project_name` to disambiguate shared titles.
    pub fn add_tags_scoped(
        &mut self,
        id_or_path: &str,
        project_name: Option<&str>,
        tags: &[String],
    ) -> Result<ObjectRecord, StoreError> {
        if tags.is_empty() {
            return Err(StoreError::InvalidInput(
                "at least one tag must be provided".to_string(),
            ));
        }

        let details = self.show_object_scoped(id_or_path, project_name)?;
        let now = now_unix();
        let tx = self.conn.transaction()?;
        let mut added_tags = Vec::new();

        for raw_tag in tags {
            let normalized = raw_tag.trim().to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            let changed = tx.execute(
                "INSERT OR IGNORE INTO tags (object_id, tag, source) VALUES (?1, ?2, 'manual')",
                params![details.object.id, normalized],
            )?;
            if changed > 0 {
                added_tags.push(normalized);
            }
        }

        if !added_tags.is_empty() {
            let detail = added_tags.join(", ");
            batch::insert_event(
                &tx,
                details.object.project_id.as_deref(),
                Some(&details.object.id),
                EventType::Tagged,
                &details.object.path,
                Some(&detail),
                now,
            )?;
            batch::touch_project(&tx, details.object.project_id.as_deref(), now)?;
        }

        tx.commit()?;
        Ok(details.object)
    }

    /// Removes manual tags from an object, optionally scoped by `project_name`.
    pub fn remove_tags_scoped(
        &mut self,
        id_or_path: &str,
        project_name: Option<&str>,
        tags: &[String],
    ) -> Result<ObjectRecord, StoreError> {
        if tags.is_empty() {
            return Err(StoreError::InvalidInput(
                "at least one tag must be provided".to_string(),
            ));
        }

        let details = self.show_object_scoped(id_or_path, project_name)?;
        let tx = self.conn.transaction()?;
        for raw_tag in tags {
            let normalized = raw_tag.trim().to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            tx.execute(
                "DELETE FROM tags WHERE object_id = ?1 AND tag = ?2 AND source = 'manual'",
                params![details.object.id, normalized],
            )?;
        }
        tx.commit()?;
        Ok(details.object)
    }

    /// Verifies SQLite, FTS5 shadow-table parity, relation hygiene, and project root paths.
    pub fn doctor(&mut self, repair: bool) -> Result<DoctorReport, StoreError> {
        let mut repaired = false;
        if repair {
            let tx = self.conn.transaction()?;
            tx.execute(
                "DELETE FROM relations
                 WHERE from_id IN (SELECT id FROM objects WHERE is_deleted = 1)
                    OR to_id IN (SELECT id FROM objects WHERE is_deleted = 1)",
                [],
            )?;
            tx.execute("INSERT INTO objects_fts(objects_fts) VALUES('rebuild')", [])?;
            tx.execute("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')", [])?;
            tx.commit()?;
            repaired = true;
        }

        let sqlite_detail: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        let mut sqlite_ok = sqlite_detail.eq_ignore_ascii_case("ok");

        if sqlite_ok
            && (self
                .conn
                .execute(
                    "INSERT INTO objects_fts(objects_fts) VALUES('integrity-check')",
                    [],
                )
                .is_err()
                || self
                    .conn
                    .execute(
                        "INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')",
                        [],
                    )
                    .is_err())
        {
            sqlite_ok = false;
        }

        let objects_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM objects", [], |r| r.get(0))?;
        let objects_fts_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM objects_fts_docsize", [], |r| r.get(0))?;
        let chunks_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        let chunks_fts_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM chunks_fts_docsize", [], |r| r.get(0))?;
        let orphaned_objects: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM objects WHERE project_id IS NULL",
            [],
            |r| r.get(0),
        )?;
        let stale_deleted_relations: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM relations r
             JOIN objects o_from ON o_from.id = r.from_id
             JOIN objects o_to ON o_to.id = r.to_id
             WHERE o_from.is_deleted = 1 OR o_to.is_deleted = 1",
            [],
            |r| r.get(0),
        )?;
        let active_sessions: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM work_sessions WHERE ended_at IS NULL",
            [],
            |r| r.get(0),
        )?;

        let mut proj_stmt = self
            .conn
            .prepare("SELECT name, root_path FROM projects ORDER BY name ASC")?;
        let missing_project_roots: Vec<String> = proj_stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|(name, root)| {
                let p = root?;
                if !Path::new(&p).exists() {
                    Some(format!("{name} ({p})"))
                } else {
                    None
                }
            })
            .collect();

        let obj_c = objects_count.max(0) as usize;
        let obj_fts_c = objects_fts_count.max(0) as usize;
        let chk_c = chunks_count.max(0) as usize;
        let chk_fts_c = chunks_fts_count.max(0) as usize;
        let orph = orphaned_objects.max(0) as usize;
        let stale_rels = stale_deleted_relations.max(0) as usize;
        let act_sess = active_sessions.max(0) as usize;

        let healthy = sqlite_ok
            && obj_c == obj_fts_c
            && chk_c == chk_fts_c
            && orph == 0
            && stale_rels == 0
            && missing_project_roots.is_empty();

        Ok(DoctorReport {
            sqlite_ok,
            sqlite_detail,
            objects_count: obj_c,
            objects_fts_count: obj_fts_c,
            chunks_count: chk_c,
            chunks_fts_count: chk_fts_c,
            orphaned_objects: orph,
            stale_deleted_relations: stale_rels,
            active_sessions: act_sess,
            missing_project_roots,
            repaired,
            healthy,
        })
    }
}

#[cfg(test)]
mod doctor_tests {
    use super::*;

    #[test]
    fn doctor_detects_and_repairs_stale_relations_and_fts_parity() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("docproj", None, None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        let a = batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/d/a.rs".to_string(),
                kind: vos_core::ObjectKind::Code,
                title: "a.rs".to_string(),
                summary: None,
                content_hash: "ha".to_string(),
                size_bytes: 10,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["fn a() {}".to_string()],
            })
            .unwrap()
            .object_id()
            .to_string();
        let b = batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id),
                path: "/d/b.rs".to_string(),
                kind: vos_core::ObjectKind::Code,
                title: "b.rs".to_string(),
                summary: None,
                content_hash: "hb".to_string(),
                size_bytes: 10,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["fn b() {}".to_string()],
            })
            .unwrap()
            .object_id()
            .to_string();
        batch
            .sync_relations(
                std::slice::from_ref(&a),
                &[RelationInput {
                    from_object_id: a.clone(),
                    to_object_id: b.clone(),
                    relation_type: vos_core::RelationType::RustUse,
                    confidence: 0.9,
                    detail: None,
                }],
            )
            .unwrap();
        batch.commit().unwrap();

        assert!(store.doctor(false).unwrap().healthy);

        // Simulate a soft-deleted target leaving a stale relation edge behind.
        store
            .conn
            .execute("UPDATE objects SET is_deleted = 1 WHERE id = ?1", params![b])
            .unwrap();
        let pre = store.doctor(false).unwrap();
        assert!(!pre.healthy);
        assert_eq!(pre.stale_deleted_relations, 1);

        let fixed = store.doctor(true).unwrap();
        assert!(fixed.repaired);
        assert_eq!(fixed.stale_deleted_relations, 0);
        assert!(fixed.healthy);
    }
}
