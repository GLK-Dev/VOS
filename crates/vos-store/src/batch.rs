use std::collections::HashSet;

use rusqlite::{OptionalExtension, Transaction, params};
use tracing::debug;
use vos_core::util::{is_path_within, new_id};
use vos_core::{EventType, ObjectKind, RelationType};

use crate::StoreError;

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
    pub is_noise: bool,
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

impl UpsertOutcome {
    /// Returns the indexed object ID regardless of outcome variant.
    pub fn object_id(&self) -> &str {
        match self {
            Self::Added(id) | Self::Modified(id) | Self::Restored(id) | Self::Unchanged(id) => id,
        }
    }
}

/// Deterministic directed dependency edge input written during `vos index`.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationInput {
    pub from_object_id: String,
    pub to_object_id: String,
    pub relation_type: RelationType,
    pub confidence: f64,
    pub detail: Option<String>,
}

/// Single-transaction batch handle used by the indexer to avoid per-file commits.
pub struct IndexBatch<'conn> {
    pub(crate) tx: Transaction<'conn>,
    pub(crate) now: i64,
}

impl<'conn> IndexBatch<'conn> {
    /// Idempotently inserts or updates an object and its chunks within the active transaction.
    pub fn upsert_object(&mut self, input: UpsertObjectInput) -> Result<UpsertOutcome, StoreError> {
        let existing: Option<(String, Option<String>, String, i64, i64, String)> = {
            let mut stmt = self.tx.prepare_cached(
                "SELECT id, project_id, content_hash, is_deleted, is_noise, kind
                 FROM objects WHERE path = ?1",
            )?;
            stmt.query_row(params![input.path], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .optional()?
        };

        let is_noise_int = i64::from(input.is_noise);

        let outcome = match existing {
            None => {
                let obj_id = new_id("obj");
                {
                    let mut stmt = self.tx.prepare_cached(
                        "INSERT INTO objects (
                            id, project_id, path, kind, title, summary, content_hash,
                            size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, NULL, ?11)",
                    )?;
                    stmt.execute(params![
                        obj_id,
                        input.project_id,
                        input.path,
                        input.kind.as_str(),
                        input.title,
                        input.summary,
                        input.content_hash,
                        input.size_bytes,
                        input.modified_at,
                        self.now,
                        is_noise_int,
                    ])?;
                }

                insert_chunks(&self.tx, &obj_id, &input.chunks)?;
                insert_event(
                    &self.tx,
                    input.project_id.as_deref(),
                    Some(&obj_id),
                    EventType::Added,
                    &input.path,
                    None,
                    self.now,
                )?;
                touch_project(&self.tx, input.project_id.as_deref(), self.now)?;
                UpsertOutcome::Added(obj_id)
            }
            Some((
                obj_id,
                existing_proj,
                existing_hash,
                is_deleted,
                existing_noise,
                existing_kind,
            )) => {
                let was_deleted = is_deleted != 0;
                if !was_deleted
                    && existing_hash == input.content_hash
                    && existing_proj == input.project_id
                {
                    if (existing_noise != 0) != input.is_noise
                        || existing_kind != input.kind.as_str()
                    {
                        let mut stmt = self.tx.prepare_cached(
                            "UPDATE objects SET is_noise = ?1, kind = ?2 WHERE id = ?3",
                        )?;
                        stmt.execute(params![is_noise_int, input.kind.as_str(), obj_id])?;
                    }
                    debug!(path = %input.path, "object unchanged; skipped");
                    return Ok(UpsertOutcome::Unchanged(obj_id));
                }

                {
                    let mut stmt = self.tx.prepare_cached(
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
                             deleted_at = NULL,
                             is_noise = ?9
                         WHERE id = ?10",
                    )?;
                    stmt.execute(params![
                        input.project_id,
                        input.kind.as_str(),
                        input.title,
                        input.summary,
                        input.content_hash,
                        input.size_bytes,
                        input.modified_at,
                        self.now,
                        is_noise_int,
                        obj_id,
                    ])?;
                }

                {
                    let mut del_chunks = self
                        .tx
                        .prepare_cached("DELETE FROM chunks WHERE object_id = ?1")?;
                    del_chunks.execute(params![obj_id])?;
                }
                insert_chunks(&self.tx, &obj_id, &input.chunks)?;

                let event_type = if was_deleted {
                    EventType::Restored
                } else {
                    EventType::Modified
                };
                insert_event(
                    &self.tx,
                    input.project_id.as_deref(),
                    Some(&obj_id),
                    event_type,
                    &input.path,
                    None,
                    self.now,
                )?;
                touch_project(&self.tx, input.project_id.as_deref(), self.now)?;

                if was_deleted {
                    UpsertOutcome::Restored(obj_id)
                } else {
                    UpsertOutcome::Modified(obj_id)
                }
            }
        };

        Ok(outcome)
    }

    /// Soft-deletes active objects within `scope_path` that were not observed in `seen_paths`.
    pub fn mark_missing_deleted(
        &mut self,
        scope_path: &str,
        project_id: Option<&str>,
        seen_paths: &HashSet<String>,
    ) -> Result<usize, StoreError> {
        let candidates: Vec<(String, Option<String>, String)> = {
            let mut stmt = self.tx.prepare(
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
                let mut upd = self.tx.prepare_cached(
                    "UPDATE objects SET is_deleted = 1, deleted_at = ?1 WHERE id = ?2",
                )?;
                upd.execute(params![self.now, obj_id])?;

                let mut del_rel = self
                    .tx
                    .prepare_cached("DELETE FROM relations WHERE from_id = ?1 OR to_id = ?1")?;
                del_rel.execute(params![obj_id])?;

                insert_event(
                    &self.tx,
                    obj_project_id.as_deref(),
                    Some(&obj_id),
                    EventType::Deleted,
                    &path,
                    None,
                    self.now,
                )?;
                touch_project(&self.tx, obj_project_id.as_deref(), self.now)?;
                deleted_count += 1;
            }
        }

        Ok(deleted_count)
    }

    /// Atomically replaces outgoing deterministic relations for all scanned objects.
    pub fn sync_relations(
        &mut self,
        scanned_object_ids: &[String],
        relations: &[RelationInput],
    ) -> Result<usize, StoreError> {
        {
            let mut del_stmt = self
                .tx
                .prepare_cached("DELETE FROM relations WHERE from_id = ?1")?;
            for from_id in scanned_object_ids {
                del_stmt.execute(params![from_id])?;
            }
        }

        let mut ins_stmt = self.tx.prepare_cached(
            "INSERT OR REPLACE INTO relations (from_id, to_id, relation_type, confidence, detail)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;

        let mut inserted = 0_usize;
        for rel in relations {
            if rel.from_object_id == rel.to_object_id {
                continue;
            }
            ins_stmt.execute(params![
                rel.from_object_id,
                rel.to_object_id,
                rel.relation_type.as_str(),
                rel.confidence,
                rel.detail,
            ])?;
            inserted += 1;
        }

        Ok(inserted)
    }

    /// Commits all indexed object updates, chunk replacements, and events atomically.
    pub fn commit(self) -> Result<(), StoreError> {
        self.tx.commit()?;
        Ok(())
    }
}

pub(crate) fn insert_chunks(
    tx: &Transaction<'_>,
    object_id: &str,
    chunks: &[String],
) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO chunks (id, object_id, chunk_index, text) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (idx, chunk) in chunks.iter().enumerate() {
        let chunk_id = new_id("chk");
        stmt.execute(params![chunk_id, object_id, idx as i64, chunk])?;
    }
    Ok(())
}

pub(crate) fn insert_event(
    tx: &Transaction<'_>,
    project_id: Option<&str>,
    object_id: Option<&str>,
    event_type: EventType,
    path: &str,
    detail: Option<&str>,
    created_at: i64,
) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO events (project_id, object_id, event_type, path, detail, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    stmt.execute(params![
        project_id,
        object_id,
        event_type.as_str(),
        path,
        detail,
        created_at
    ])?;
    Ok(())
}

pub(crate) fn touch_project(
    tx: &Transaction<'_>,
    project_id: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    if let Some(pid) = project_id {
        let mut stmt = tx.prepare_cached("UPDATE projects SET updated_at = ?1 WHERE id = ?2")?;
        stmt.execute(params![now, pid])?;
    }
    Ok(())
}
