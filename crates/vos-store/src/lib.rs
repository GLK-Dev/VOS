//! SQLite persistence, FTS5 search, and project context queries for VOS.

mod batch;
mod queries;

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use tracing::info;
use vos_core::VosError;
use vos_core::util::{canonicalize_path, is_path_within, new_id, now_unix};
use vos_core::{EventType, ObjectRecord, Project, ProjectSummary};

pub use batch::{IndexBatch, UpsertObjectInput, UpsertOutcome};

const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../../../migrations/0001_init.sql")),
    ("0002_noise", include_str!("../../../migrations/0002_noise.sql")),
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
        if tags.is_empty() {
            return Err(StoreError::InvalidInput(
                "at least one tag must be provided".to_string(),
            ));
        }

        let details = self.show_object(id_or_path)?;
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
}
