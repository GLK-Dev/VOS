use std::collections::HashMap;
use std::path::Path;

use rusqlite::{OptionalExtension, params};
use vos_core::util::canonicalize_path;
use vos_core::{
    EventRecord, EventType, ObjectDetails, ObjectKind, ObjectRecord, Project, ProjectContext,
    SearchHit,
};

use crate::{Store, StoreError};

/// Score bonus when a query token matches the file name or file stem exactly.
const TITLE_EXACT_MATCH_BOOST: f64 = 25.0;
/// Score bonus when a query token (>= 3 chars) appears inside the file name.
const TITLE_CONTAINS_BOOST: f64 = 10.0;
/// Score bonus when a query token (>= 3 chars) appears in the object path.
const PATH_CONTAINS_BOOST: f64 = 5.0;
/// Score bonus when `objects_fts` (title/path/summary) matches directly.
const OBJECT_FTS_SIGNAL_BOOST: f64 = 4.0;
/// Additional score per extra matching chunk in the same file (capped).
const MULTI_CHUNK_STEP_BOOST: f64 = 1.5;
const MULTI_CHUNK_MAX_BOOST: f64 = 6.0;
/// Penalty applied to lockfiles and housekeeping noise during search ranking.
const NOISE_PENALTY: f64 = -10.0;

struct SearchCandidate {
    hit: SearchHit,
    is_noise: bool,
    chunk_hits: usize,
    matched_object_fts: bool,
    best_chunk_bm25: Option<f64>,
    object_bm25: Option<f64>,
}

impl Store {
    /// Loads full metadata, project binding, and tags for an object by ID, path, or unique title.
    pub fn show_object(&self, id_or_path: &str) -> Result<ObjectDetails, StoreError> {
        self.show_object_scoped(id_or_path, None)
    }

    /// Loads full metadata for an object, using `project_name` to disambiguate shared titles.
    pub fn show_object_scoped(
        &self,
        id_or_path: &str,
        project_name: Option<&str>,
    ) -> Result<ObjectDetails, StoreError> {
        let mut object = self.find_object_by_id_or_path(id_or_path)?;
        if object.is_none() {
            let candidate = Path::new(id_or_path);
            if candidate.exists() {
                let canonical = canonicalize_path(candidate)?;
                object = self.find_object_by_id_or_path(&canonical)?;
            }
        }
        if object.is_none() {
            object = self.find_object_by_title_or_suffix(id_or_path, project_name)?;
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

    /// Performs full-text BM25 search across `chunks_fts` and `objects_fts` with deterministic title/path boosts.
    pub fn search(
        &self,
        query: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let fts_query = build_fts_query(query)?;
        let query_terms: Vec<String> = query
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| t.to_lowercase())
            .collect();

        let requested_limit = limit.clamp(1, 200);
        let pool_limit = (requested_limit * 6).max(60) as i64;
        let mut merged: HashMap<String, SearchCandidate> = HashMap::new();

        {
            let mut stmt = self.conn.prepare(
                "SELECT o.id, p.name, o.title, o.path, o.kind, o.is_noise,
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

            let rows = stmt.query_map(params![fts_query, project_name, pool_limit], |row| {
                let kind_str: String = row.get(4)?;
                let is_noise: i64 = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    ObjectKind::from_db(&kind_str),
                    is_noise != 0,
                    row.get::<_, String>(6)?,
                    row.get::<_, f64>(7)?,
                ))
            })?;

            for row in rows {
                let (id, pname, title, path, kind, is_noise, snippet, bm25_rank) = row?;
                let entry = merged.entry(id.clone()).or_insert_with(|| SearchCandidate {
                    hit: SearchHit {
                        object_id: id,
                        project_name: pname,
                        title,
                        path,
                        kind,
                        snippet: snippet.clone(),
                        score: 0.0,
                    },
                    is_noise,
                    chunk_hits: 0,
                    matched_object_fts: false,
                    best_chunk_bm25: None,
                    object_bm25: None,
                });
                entry.chunk_hits += 1;
                if entry.best_chunk_bm25.is_none_or(|prev| bm25_rank < prev) {
                    entry.best_chunk_bm25 = Some(bm25_rank);
                    entry.hit.snippet = snippet;
                }
            }
        }

        {
            let mut stmt = self.conn.prepare(
                "SELECT o.id, p.name, o.title, o.path, o.kind, o.is_noise,
                        snippet(objects_fts, -1, '[', ']', '...', 24) AS snip,
                        bm25(objects_fts, 10.0, 3.0, 1.0) AS rank
                 FROM objects_fts
                 JOIN objects o ON o.rowid = objects_fts.rowid
                 LEFT JOIN projects p ON p.id = o.project_id
                 WHERE objects_fts MATCH ?1
                   AND o.is_deleted = 0
                   AND (?2 IS NULL OR p.name = ?2)
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(params![fts_query, project_name, pool_limit], |row| {
                let kind_str: String = row.get(4)?;
                let is_noise: i64 = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    ObjectKind::from_db(&kind_str),
                    is_noise != 0,
                    row.get::<_, String>(6)?,
                    row.get::<_, f64>(7)?,
                ))
            })?;

            for row in rows {
                let (id, pname, title, path, kind, is_noise, snippet, bm25_rank) = row?;
                let entry = merged.entry(id.clone()).or_insert_with(|| SearchCandidate {
                    hit: SearchHit {
                        object_id: id,
                        project_name: pname,
                        title,
                        path,
                        kind,
                        snippet: snippet.clone(),
                        score: 0.0,
                    },
                    is_noise,
                    chunk_hits: 0,
                    matched_object_fts: false,
                    best_chunk_bm25: None,
                    object_bm25: None,
                });
                entry.matched_object_fts = true;
                entry.object_bm25 = Some(bm25_rank);
                if entry.hit.snippet.trim().is_empty() {
                    entry.hit.snippet = snippet;
                }
            }
        }

        let mut hits: Vec<SearchHit> = merged
            .into_values()
            .map(|mut cand| {
                cand.hit.score = score_search_candidate(&cand, &query_terms);
                cand.hit
            })
            .collect();

        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(requested_limit);
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

    fn find_object_by_title_or_suffix(
        &self,
        target: &str,
        project_name: Option<&str>,
    ) -> Result<Option<ObjectRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT o.id, o.project_id, o.path, o.kind, o.title, o.summary, o.content_hash,
                    o.size_bytes, o.modified_at, o.indexed_at, o.is_deleted, o.deleted_at, o.is_noise
             FROM objects o
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE o.title = ?1
               AND o.is_deleted = 0
               AND (?2 IS NULL OR p.name = ?2)
             LIMIT 5",
        )?;
        let mut matches = stmt
            .query_map(params![target, project_name], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        if matches.len() == 1 {
            return Ok(matches.pop());
        }
        if matches.len() > 1 {
            let paths: Vec<String> = matches.into_iter().map(|m| m.path).collect();
            return Err(StoreError::InvalidInput(format!(
                "ambiguous object '{target}', specify --project or path. Matches: {}",
                paths.join(", ")
            )));
        }

        // Also try normalized path suffix match (e.g. `docs/ROADMAP.md` or `transformer/mod.rs`).
        let norm_target = target.replace('\\', "/").trim_start_matches('/').to_lowercase();
        let mut all_stmt = self.conn.prepare(
            "SELECT o.id, o.project_id, o.path, o.kind, o.title, o.summary, o.content_hash,
                    o.size_bytes, o.modified_at, o.indexed_at, o.is_deleted, o.deleted_at, o.is_noise
             FROM objects o
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE o.is_deleted = 0
               AND (?1 IS NULL OR p.name = ?1)",
        )?;
        let suffix_matches: Vec<ObjectRecord> = all_stmt
            .query_map(params![project_name], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|o| {
                let norm = o.path.replace('\\', "/").to_lowercase();
                norm == norm_target || norm.ends_with(&format!("/{norm_target}"))
            })
            .collect();

        if suffix_matches.len() == 1 {
            Ok(suffix_matches.into_iter().next())
        } else {
            Ok(None)
        }
    }
}

fn score_search_candidate(cand: &SearchCandidate, query_terms: &[String]) -> f64 {
    let chunk_bm25_mag = cand.best_chunk_bm25.map(|r| (-r).max(0.0)).unwrap_or(0.0);
    let obj_bm25_mag = cand.object_bm25.map(|r| (-r).max(0.0)).unwrap_or(0.0);
    let mut score = chunk_bm25_mag.max(obj_bm25_mag);

    if cand.matched_object_fts {
        score += OBJECT_FTS_SIGNAL_BOOST;
    }

    if cand.chunk_hits > 1 {
        let extra = ((cand.chunk_hits - 1) as f64 * MULTI_CHUNK_STEP_BOOST)
            .min(MULTI_CHUNK_MAX_BOOST);
        score += extra;
    }

    let title_lower = cand.hit.title.to_lowercase();
    let stem_lower = Path::new(&cand.hit.title)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    let path_lower = cand.hit.path.to_lowercase();

    let mut exact_title_hit = false;
    let mut partial_title_hit = false;
    let mut path_hit = false;

    for term in query_terms {
        if title_lower == *term || stem_lower == *term {
            exact_title_hit = true;
        } else if term.len() >= 3 && title_lower.contains(term) {
            partial_title_hit = true;
        }
        if term.len() >= 3 && path_lower.contains(term) {
            path_hit = true;
        }
    }

    if exact_title_hit {
        score += TITLE_EXACT_MATCH_BOOST;
    } else if partial_title_hit {
        score += TITLE_CONTAINS_BOOST;
    }

    if path_hit {
        score += PATH_CONTAINS_BOOST;
    }

    if cand.is_noise {
        score += NOISE_PENALTY;
    }

    score
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

pub(crate) fn map_object_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObjectRecord> {
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

pub(crate) fn build_fts_or_query(terms: &[String]) -> Option<String> {
    let tokens: Vec<String> = terms
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" OR "))
    }
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
    fn exact_title_match_outranks_incidental_chunk_mentions() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("demo", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/src/maturity.rs".to_string(),
                kind: ObjectKind::Code,
                title: "maturity.rs".to_string(),
                summary: Some("fn check_roadmap()".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 120,
                modified_at: 1000,
                is_noise: false,
                chunks: vec!["// roadmap roadmap roadmap checklist item".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/docs/ROADMAP.md".to_string(),
                kind: ObjectKind::Note,
                title: "ROADMAP.md".to_string(),
                summary: Some("# Project Roadmap".to_string()),
                content_hash: "h2".to_string(),
                size_bytes: 80,
                modified_at: 1000,
                is_noise: false,
                chunks: vec!["# Project Roadmap\nPhase 1 milestones.".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let hits = store.search("roadmap", Some("demo"), 5).unwrap();
        assert_eq!(hits[0].title, "ROADMAP.md");
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

