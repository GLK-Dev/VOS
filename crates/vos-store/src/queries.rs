use std::path::Path;

use rusqlite::{OptionalExtension, params};
use vos_core::util::canonicalize_path;
use vos_core::{
    EventRecord, EventType, ObjectDetails, ObjectKind, ObjectRecord, Project, ProjectContext,
};

use crate::{Store, StoreError};

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
        let scoped_project = match project_name {
            Some(name) => Some(
                self.get_project_by_name(name)?
                    .ok_or_else(|| StoreError::NotFound("project", name.to_string()))?,
            ),
            None => None,
        };
        let matches_scope = |obj: &ObjectRecord| match &scoped_project {
            Some(proj) => obj.project_id.as_deref() == Some(proj.id.as_str()),
            None => true,
        };

        let mut object = self
            .find_object_by_id_or_path(id_or_path)?
            .filter(&matches_scope);

        if object.is_none() {
            let candidate = Path::new(id_or_path);
            if let Some(ref proj) = scoped_project {
                if candidate.is_absolute() && candidate.exists() {
                    let canonical = canonicalize_path(candidate)?;
                    object = self
                        .find_object_by_id_or_path(&canonical)?
                        .filter(&matches_scope);
                } else if let Some(ref root) = proj.root_path {
                    let joined = Path::new(root).join(candidate);
                    if joined.exists() {
                        let canonical = canonicalize_path(&joined)?;
                        object = self
                            .find_object_by_id_or_path(&canonical)?
                            .filter(&matches_scope);
                    }
                }
            } else if candidate.exists() {
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

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    fn find_object_by_id_or_path(
        &self,
        id_or_path: &str,
    ) -> Result<Option<ObjectRecord>, StoreError> {
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
             WHERE LOWER(o.title) = LOWER(?1)
               AND o.is_deleted = 0
               AND (?2 IS NULL OR p.name = ?2)",
        )?;
        let mut matches = stmt
            .query_map(params![target, project_name], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        if matches.len() == 1 {
            return Ok(matches.pop());
        }
        if matches.len() > 1 {
            let exact: Vec<ObjectRecord> = matches
                .iter()
                .filter(|m| m.title == target)
                .cloned()
                .collect();
            if exact.len() == 1 {
                return Ok(exact.into_iter().next());
            }
            return Err(ambiguous_object_error(target, matches));
        }

        let norm_target = normalize_lookup_suffix(target);
        if norm_target.is_empty() {
            return Ok(None);
        }
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

        match suffix_matches.len() {
            0 => Ok(None),
            1 => Ok(suffix_matches.into_iter().next()),
            _ => Err(ambiguous_object_error(target, suffix_matches)),
        }
    }
}

fn normalize_lookup_suffix(target: &str) -> String {
    let mut s = target.replace('\\', "/").trim().to_lowercase();
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest.to_string();
        } else if let Some(rest) = s.strip_prefix('/') {
            s = rest.to_string();
        } else {
            break s;
        }
    }
}

fn ambiguous_object_error(target: &str, matches: Vec<ObjectRecord>) -> StoreError {
    let mut paths: Vec<String> = matches.into_iter().map(|m| m.path).collect();
    paths.sort();
    let total = paths.len();
    let preview: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
    let extra = if total > 5 {
        format!(" (+{} more)", total - 5)
    } else {
        String::new()
    };
    StoreError::InvalidInput(format!(
        "ambiguous object '{target}', specify --project or path. Matches: {}{extra}",
        preview.join(", ")
    ))
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
        let project = store
            .create_project("ai-os", None, Some("Kernel notes"))
            .unwrap();

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
        let sum: f64 = hits[0].signals.iter().map(|s| s.delta).sum();
        assert!((sum - hits[0].score).abs() < 1e-9);

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
    fn stage2_or_fallback_recovers_cross_chunk_and_partial_matches() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("demo", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/docs/memory.md".to_string(),
                kind: ObjectKind::Note,
                title: "memory.md".to_string(),
                summary: Some("Event stream notes".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 120,
                modified_at: 1000,
                is_noise: false,
                chunks: vec![
                    "Section 1: event tracking for files.".to_string(),
                    "Section 2: structured audit log.".to_string(),
                ],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/docs/changes.md".to_string(),
                kind: ObjectKind::Note,
                title: "changes.md".to_string(),
                summary: Some("Changes summary".to_string()),
                content_hash: "h2".to_string(),
                size_bytes: 90,
                modified_at: 1000,
                is_noise: false,
                chunks: vec!["List of changes across modules.".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let hits = store.search("event log", Some("demo"), 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].title, "memory.md");
        assert!(
            hits[0]
                .signals
                .iter()
                .any(|s| s.label == "all query terms matched")
        );
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

    #[test]
    fn scoped_lookup_resolves_target_within_requested_project_not_cwd() {
        let mut store = Store::open_in_memory().unwrap();
        let proj_a = store.create_project("proj_a", None, None).unwrap();
        let proj_b = store.create_project("proj_b", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj_a.id.clone()),
                path: "/fake/a/README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                summary: Some("A".to_string()),
                content_hash: "ha".to_string(),
                size_bytes: 1,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["A".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj_b.id.clone()),
                path: "/fake/b/README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                summary: Some("B".to_string()),
                content_hash: "hb".to_string(),
                size_bytes: 1,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["B".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        // Even though `README.md` exists in the repo CWD, `--project proj_b` must resolve `proj_b`'s file.
        let details = store
            .show_object_scoped("README.md", Some("proj_b"))
            .unwrap();
        assert_eq!(details.project_name.as_deref(), Some("proj_b"));
        assert_eq!(details.object.path, "/fake/b/README.md");
    }

    #[test]
    fn suffix_lookup_strips_dot_slash_and_surfaces_ambiguity_error() {
        let mut store = Store::open_in_memory().unwrap();
        let proj_a = store.create_project("alpha", None, None).unwrap();
        let proj_b = store.create_project("beta", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        for (pid, path, hash) in [
            (proj_a.id, "/ws/alpha/src/lib.rs", "ha"),
            (proj_b.id, "/ws/beta/src/lib.rs", "hb"),
        ] {
            batch
                .upsert_object(UpsertObjectInput {
                    project_id: Some(pid),
                    path: path.to_string(),
                    kind: ObjectKind::Code,
                    title: "lib.rs".to_string(),
                    summary: None,
                    content_hash: hash.to_string(),
                    size_bytes: 10,
                    modified_at: 100,
                    is_noise: false,
                    chunks: Vec::new(),
                })
                .unwrap();
        }
        batch.commit().unwrap();

        let err = store.show_object("./src/lib.rs").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous object './src/lib.rs'"));
        assert!(msg.contains("/ws/alpha/src/lib.rs"));
        assert!(msg.contains("/ws/beta/src/lib.rs"));

        let scoped = store
            .show_object_scoped(".\\src\\lib.rs", Some("beta"))
            .unwrap();
        assert_eq!(scoped.object.path, "/ws/beta/src/lib.rs");
    }

    #[test]
    fn title_lookup_reports_full_overflow_count_and_prefers_exact_case() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("many", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        for idx in 0..7 {
            batch
                .upsert_object(UpsertObjectInput {
                    project_id: Some(proj.id.clone()),
                    path: format!("/many/c{idx}/lib.rs"),
                    kind: ObjectKind::Code,
                    title: "lib.rs".to_string(),
                    summary: None,
                    content_hash: format!("h{idx}"),
                    size_bytes: 10,
                    modified_at: 100,
                    is_noise: false,
                    chunks: Vec::new(),
                })
                .unwrap();
        }
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/many/docs/Notes.md".to_string(),
                kind: ObjectKind::Note,
                title: "Notes.md".to_string(),
                summary: None,
                content_hash: "hn1".to_string(),
                size_bytes: 10,
                modified_at: 100,
                is_noise: false,
                chunks: Vec::new(),
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id),
                path: "/many/lower/notes.md".to_string(),
                kind: ObjectKind::Note,
                title: "notes.md".to_string(),
                summary: None,
                content_hash: "hn2".to_string(),
                size_bytes: 10,
                modified_at: 100,
                is_noise: false,
                chunks: Vec::new(),
            })
            .unwrap();
        batch.commit().unwrap();

        let err = store.show_object("lib.rs").unwrap_err().to_string();
        assert!(err.contains("(+2 more)"), "unexpected error: {err}");

        let exact = store.show_object("Notes.md").unwrap();
        assert_eq!(exact.object.path, "/many/docs/Notes.md");
    }
}
