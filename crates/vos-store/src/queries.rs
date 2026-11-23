use std::collections::HashMap;
use std::path::Path;

use rusqlite::{OptionalExtension, params};
use vos_core::util::canonicalize_path;
use vos_core::{
    EventRecord, EventType, ObjectDetails, ObjectKind, ObjectRecord, Project, ProjectContext,
    SearchHit,
};

use crate::{Store, StoreError};

impl Store {
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
                insert_best_hit(&mut merged, row?);
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
                insert_best_hit(&mut merged, row?);
            }
        }

        let mut hits: Vec<SearchHit> = merged.into_values().collect();
        hits.sort_by(|a, b| a.score.total_cmp(&b.score));
        hits.truncate(limit.clamp(1, 200));
        Ok(hits)
    }

    /// Builds a prioritized recovery context snapshot for a project, filtering noise unless requested.
    pub fn project_context(
        &self,
        project_name: &str,
        recent_limit: usize,
        include_noise: bool,
    ) -> Result<ProjectContext, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let (active_objects, deleted_objects, noise_objects): (i64, i64, i64) =
            self.conn.query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN is_deleted = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN is_deleted = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN is_deleted = 0 AND is_noise = 1 THEN 1 ELSE 0 END), 0)
                 FROM objects
                 WHERE project_id = ?1",
                params![project.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
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

        let candidate_pool = (recent_limit * 6).max(50) as i64;
        let mut recent_stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
             FROM objects
             WHERE project_id = ?1
               AND is_deleted = 0
               AND (?2 = 1 OR is_noise = 0)
             ORDER BY modified_at DESC, indexed_at DESC
             LIMIT ?3",
        )?;
        let mut recent_objects = recent_stmt
            .query_map(
                params![project.id, i64::from(include_noise), candidate_pool],
                map_object_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;

        // Rank by activity day first, then human-meaningful kind priority, then exact timestamp.
        recent_objects.sort_by(|a, b| {
            let key_a = (
                a.modified_at.div_euclid(86_400),
                a.kind.context_priority(),
                a.modified_at,
            );
            let key_b = (
                b.modified_at.div_euclid(86_400),
                b.kind.context_priority(),
                b.modified_at,
            );
            key_b.cmp(&key_a)
        });
        recent_objects.truncate(recent_limit);

        let mut noise_stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
             FROM objects
             WHERE project_id = ?1 AND is_deleted = 0 AND is_noise = 1
             ORDER BY modified_at DESC
             LIMIT 8",
        )?;
        let noise_sample = noise_stmt
            .query_map(params![project.id], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let recent_events = self.project_timeline(&project.name, recent_limit)?;

        Ok(ProjectContext {
            project,
            active_objects: active_objects.max(0) as usize,
            deleted_objects: deleted_objects.max(0) as usize,
            noise_objects: noise_objects.max(0) as usize,
            counts_by_kind,
            recent_objects,
            noise_sample,
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
                        size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
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
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
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

pub(crate) fn map_project_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
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
    let is_noise: i64 = row.get(12)?;
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
        is_noise: is_noise != 0,
    })
}

fn insert_best_hit(merged: &mut HashMap<String, SearchHit>, hit: SearchHit) {
    match merged.get(&hit.object_id) {
        Some(existing) if existing.score <= hit.score => {}
        _ => {
            merged.insert(hit.object_id.clone(), hit);
        }
    }
}

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
    use std::collections::HashSet;

    use vos_core::ObjectKind;

    use super::*;
    use crate::{UpsertObjectInput, UpsertOutcome};

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
                is_noise: false,
                chunks: vec!["pub fn schedule_task() { /* round robin */ }".to_string()],
            })
            .unwrap();
        assert!(matches!(outcome, UpsertOutcome::Added(_)));

        let hits = store.search("schedule_task", Some("ai-os"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "scheduler.rs");

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
                is_noise: false,
                chunks: vec!["pub fn schedule_task() { /* round robin */ }".to_string()],
            })
            .unwrap();
        assert!(matches!(repeat, UpsertOutcome::Unchanged(_)));
        assert_eq!(store.project_timeline("ai-os", 10).unwrap().len(), 1);

        let deleted = store
            .mark_missing_deleted("/tmp/ai-os", Some(&project.id), &HashSet::new())
            .unwrap();
        assert_eq!(deleted, 1);
        assert!(store.search("schedule_task", None, 10).unwrap().is_empty());
        assert_eq!(store.project_timeline("ai-os", 10).unwrap().len(), 2);
    }

    #[test]
    fn context_filters_noise_and_prioritizes_notes_over_code_same_day() {
        let mut store = Store::open_in_memory().unwrap();
        let project = store.create_project("demo", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(project.id.clone()),
                path: "/tmp/demo/.gitignore".to_string(),
                kind: ObjectKind::Other,
                title: ".gitignore".to_string(),
                summary: None,
                content_hash: "h_git".to_string(),
                size_bytes: 20,
                modified_at: 86_400 * 10 + 500,
                is_noise: true,
                chunks: Vec::new(),
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(project.id.clone()),
                path: "/tmp/demo/main.rs".to_string(),
                kind: ObjectKind::Code,
                title: "main.rs".to_string(),
                summary: None,
                content_hash: "h_main".to_string(),
                size_bytes: 40,
                modified_at: 86_400 * 10 + 400,
                is_noise: false,
                chunks: Vec::new(),
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(project.id.clone()),
                path: "/tmp/demo/ROADMAP.md".to_string(),
                kind: ObjectKind::Note,
                title: "ROADMAP.md".to_string(),
                summary: None,
                content_hash: "h_road".to_string(),
                size_bytes: 60,
                modified_at: 86_400 * 10 + 100,
                is_noise: false,
                chunks: Vec::new(),
            })
            .unwrap();
        batch.commit().unwrap();

        let ctx_clean = store.project_context("demo", 10, false).unwrap();
        assert_eq!(ctx_clean.noise_objects, 1);
        assert_eq!(ctx_clean.recent_objects.len(), 2);
        assert_eq!(ctx_clean.recent_objects[0].title, "ROADMAP.md");
        assert_eq!(ctx_clean.recent_objects[1].title, "main.rs");

        let ctx_all = store.project_context("demo", 10, true).unwrap();
        assert_eq!(ctx_all.recent_objects.len(), 3);
    }
}
