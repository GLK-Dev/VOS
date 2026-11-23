use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::params;
use vos_core::util::now_unix;
use vos_core::{
    ActiveArea, EventRecord, EventType, ObjectKind, ObjectRecord, ProjectActivity, ProjectResume,
    RelatedHit,
};

use crate::queries::{build_fts_or_query, map_object_row};
use crate::{Store, StoreError};

impl Store {
    /// Returns project events within the trailing `days` window, excluding noise by default.
    pub fn project_activity(
        &self,
        project_name: &str,
        days: u32,
        include_noise: bool,
    ) -> Result<ProjectActivity, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let window_days = days.max(1);
        let since_ts = now_unix().saturating_sub(i64::from(window_days) * 86_400);

        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.project_id, e.object_id, e.event_type, e.path, e.detail, e.created_at
             FROM events e
             LEFT JOIN objects o ON o.id = e.object_id
             WHERE e.project_id = ?1
               AND e.created_at >= ?2
               AND (?3 = 1 OR COALESCE(o.is_noise, 0) = 0)
             ORDER BY e.created_at DESC, e.id DESC
             LIMIT 200",
        )?;

        let events = stmt
            .query_map(
                params![project.id, since_ts, i64::from(include_noise)],
                |row| {
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
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ProjectActivity {
            project,
            days: window_days,
            since_ts,
            events,
        })
    }

    /// Builds a high-signal context recovery briefing for resuming work on a project.
    pub fn project_resume(&self, project_name: &str) -> Result<ProjectResume, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let active_objects: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM objects WHERE project_id = ?1 AND is_deleted = 0",
            params![project.id],
            |row| row.get(0),
        )?;

        let mut obj_stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
             FROM objects
             WHERE project_id = ?1 AND is_deleted = 0 AND is_noise = 0
             ORDER BY modified_at DESC, indexed_at DESC",
        )?;
        let non_noise_objects = obj_stmt
            .query_map(params![project.id], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let last_activity_at = non_noise_objects
            .iter()
            .map(|o| o.modified_at)
            .max()
            .or(Some(project.updated_at));

        let mut recently_modified = non_noise_objects.clone();
        recently_modified.sort_by(|a, b| {
            let ka = (
                a.modified_at.div_euclid(86_400),
                a.kind.context_priority(),
                a.modified_at,
            );
            let kb = (
                b.modified_at.div_euclid(86_400),
                b.kind.context_priority(),
                b.modified_at,
            );
            kb.cmp(&ka)
        });
        recently_modified.truncate(5);

        let mut add_stmt = self.conn.prepare(
            "SELECT o.id, o.project_id, o.path, o.kind, o.title, o.summary, o.content_hash,
                    o.size_bytes, o.modified_at, o.indexed_at, o.is_deleted, o.deleted_at, o.is_noise
             FROM objects o
             LEFT JOIN events e ON e.object_id = o.id AND e.event_type = 'added'
             WHERE o.project_id = ?1 AND o.is_deleted = 0 AND o.is_noise = 0
             GROUP BY o.id
             ORDER BY COALESCE(MAX(e.created_at), o.indexed_at) DESC, o.modified_at DESC
             LIMIT 5",
        )?;
        let recent_additions = add_stmt
            .query_map(params![project.id], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let root_prefix = project.root_path.as_deref().unwrap_or("");
        let most_active_areas = compute_active_areas(&non_noise_objects, root_prefix, 5);
        let starting_points = compute_starting_points(&non_noise_objects, &recently_modified, 4);

        Ok(ProjectResume {
            project,
            last_activity_at,
            active_objects: active_objects.max(0) as usize,
            recently_modified,
            recent_additions,
            most_active_areas,
            starting_points,
        })
    }

    /// Finds related objects using FTS5 overlap, shared tags, directory proximity, and session timing.
    pub fn find_related(
        &self,
        target: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RelatedHit>, StoreError> {
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return Err(StoreError::InvalidInput(
                "related target cannot be empty".to_string(),
            ));
        }

        let capped_limit = limit.clamp(1, 50);
        if let Ok(details) = self.show_object(trimmed) {
            return self.related_to_object(&details.object, &details.tags, project_name, capped_limit);
        }

        self.related_to_topic(trimmed, project_name, capped_limit)
    }

    fn related_to_object(
        &self,
        src: &ObjectRecord,
        src_tags: &[(String, String)],
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RelatedHit>, StoreError> {
        let mut candidates: HashMap<String, CandidateAccum> = HashMap::new();

        // 1. Shared tags (+40 per shared tag)
        let mut tag_stmt = self.conn.prepare(
            "SELECT DISTINCT o.id, p.name, o.title, o.path, o.kind, o.modified_at, t1.tag
             FROM tags t1
             JOIN tags t2 ON t2.tag = t1.tag
             JOIN objects o ON o.id = t2.object_id
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE t1.object_id = ?1
               AND t2.object_id != ?1
               AND o.is_deleted = 0
               AND (?2 IS NULL OR p.name = ?2)",
        )?;
        let tag_rows = tag_stmt.query_map(params![src.id, project_name], |row| {
            let kind_str: String = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                ObjectKind::from_db(&kind_str),
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        for row in tag_rows {
            let (id, pname, title, path, kind, mod_at, tag) = row?;
            add_signal(
                &mut candidates,
                id,
                pname,
                title,
                path,
                kind,
                mod_at,
                40,
                format!("shared tag '{tag}'"),
            );
        }

        // 2. FTS5 lexical overlap from title stem + tags (+35)
        let mut terms = extract_title_terms(&src.title);
        for (tag, _) in src_tags {
            if !terms.contains(tag) {
                terms.push(tag.clone());
            }
        }
        terms.truncate(6);

        if let Some(fts_query) = build_fts_or_query(&terms) {
            self.collect_fts_related(&fts_query, Some(&src.id), project_name, 35, &mut candidates)?;
        }

        // 3. Directory proximity & co-activity window within the same project
        if let Some(ref pid) = src.project_id {
            let src_parent = Path::new(&src.path).parent().map(|p| p.to_path_buf());
            let src_grand = src_parent
                .as_deref()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf());

            let mut sib_stmt = self.conn.prepare(
                "SELECT o.id, p.name, o.title, o.path, o.kind, o.modified_at
                 FROM objects o
                 LEFT JOIN projects p ON p.id = o.project_id
                 WHERE o.project_id = ?1
                   AND o.id != ?2
                   AND o.is_deleted = 0
                   AND o.is_noise = 0
                   AND (?3 IS NULL OR p.name = ?3)",
            )?;

            let sib_rows = sib_stmt
                .query_map(params![pid, src.id, project_name], |row| {
                    let kind_str: String = row.get(4)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        ObjectKind::from_db(&kind_str),
                        row.get::<_, i64>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            let session_peers: usize = sib_rows
                .iter()
                .filter(|(_, _, _, _, _, m)| (m - src.modified_at).abs() <= 1800)
                .count();

            for (id, pname, title, path, kind, mod_at) in sib_rows {
                let obj_parent = Path::new(&path).parent();
                if src_parent.as_deref().is_some() && obj_parent == src_parent.as_deref() {
                    add_signal(
                        &mut candidates,
                        id.clone(),
                        pname.clone(),
                        title.clone(),
                        path.clone(),
                        kind,
                        mod_at,
                        30,
                        "same directory".to_string(),
                    );
                } else if src_grand.as_deref().is_some()
                    && obj_parent.and_then(|p| p.parent()) == src_grand.as_deref()
                {
                    add_signal(
                        &mut candidates,
                        id.clone(),
                        pname.clone(),
                        title.clone(),
                        path.clone(),
                        kind,
                        mod_at,
                        15,
                        "nearby module".to_string(),
                    );
                }

                if session_peers > 0
                    && session_peers <= 25
                    && (mod_at - src.modified_at).abs() <= 1800
                {
                    add_signal(
                        &mut candidates,
                        id,
                        pname,
                        title,
                        path,
                        kind,
                        mod_at,
                        15,
                        "edited in same session".to_string(),
                    );
                }
            }
        }

        Ok(finalize_related(candidates, limit))
    }

    fn related_to_topic(
        &self,
        topic: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RelatedHit>, StoreError> {
        let mut candidates: HashMap<String, CandidateAccum> = HashMap::new();
        let terms = extract_title_terms(topic);

        if let Some(fts_query) = build_fts_or_query(&terms) {
            self.collect_fts_related(&fts_query, None, project_name, 45, &mut candidates)?;
        }

        // Tag & path-segment matches
        let mut obj_stmt = self.conn.prepare(
            "SELECT o.id, p.name, o.title, o.path, o.kind, o.modified_at
             FROM objects o
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE o.is_deleted = 0
               AND o.is_noise = 0
               AND (?1 IS NULL OR p.name = ?1)",
        )?;
        let rows = obj_stmt.query_map(params![project_name], |row| {
            let kind_str: String = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                ObjectKind::from_db(&kind_str),
                row.get::<_, i64>(5)?,
            ))
        })?;

        for row in rows {
            let (id, pname, title, path, kind, mod_at) = row?;
            let path_lower = path.to_lowercase();
            for term in &terms {
                if term.len() >= 3 && path_lower.contains(term) {
                    add_signal(
                        &mut candidates,
                        id.clone(),
                        pname.clone(),
                        title.clone(),
                        path.clone(),
                        kind,
                        mod_at,
                        25,
                        format!("path matches '{term}'"),
                    );
                    break;
                }
            }
        }

        for term in &terms {
            let mut tag_stmt = self.conn.prepare(
                "SELECT o.id, p.name, o.title, o.path, o.kind, o.modified_at
                 FROM tags t
                 JOIN objects o ON o.id = t.object_id
                 LEFT JOIN projects p ON p.id = o.project_id
                 WHERE t.tag = ?1
                   AND o.is_deleted = 0
                   AND (?2 IS NULL OR p.name = ?2)",
            )?;
            let tagged = tag_stmt.query_map(params![term, project_name], |row| {
                let kind_str: String = row.get(4)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    ObjectKind::from_db(&kind_str),
                    row.get::<_, i64>(5)?,
                ))
            })?;
            for row in tagged {
                let (id, pname, title, path, kind, mod_at) = row?;
                add_signal(
                    &mut candidates,
                    id,
                    pname,
                    title,
                    path,
                    kind,
                    mod_at,
                    40,
                    format!("tagged '{term}'"),
                );
            }
        }

        Ok(finalize_related(candidates, limit))
    }

    fn collect_fts_related(
        &self,
        fts_query: &str,
        exclude_id: Option<&str>,
        project_name: Option<&str>,
        weight: i64,
        candidates: &mut HashMap<String, CandidateAccum>,
    ) -> Result<(), StoreError> {
        let mut chunk_stmt = self.conn.prepare(
            "SELECT DISTINCT o.id, p.name, o.title, o.path, o.kind, o.modified_at
             FROM chunks_fts
             JOIN chunks c ON c.rowid = chunks_fts.rowid
             JOIN objects o ON o.id = c.object_id
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE chunks_fts MATCH ?1
               AND o.is_deleted = 0
               AND o.is_noise = 0
               AND (?2 IS NULL OR o.id != ?2)
               AND (?3 IS NULL OR p.name = ?3)
             ORDER BY bm25(chunks_fts)
             LIMIT 30",
        )?;
        let chunk_rows = chunk_stmt.query_map(params![fts_query, exclude_id, project_name], |row| {
            let kind_str: String = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                ObjectKind::from_db(&kind_str),
                row.get::<_, i64>(5)?,
            ))
        })?;
        for row in chunk_rows {
            let (id, pname, title, path, kind, mod_at) = row?;
            add_signal(
                candidates,
                id,
                pname,
                title,
                path,
                kind,
                mod_at,
                weight,
                "content FTS match".to_string(),
            );
        }

        let mut obj_stmt = self.conn.prepare(
            "SELECT o.id, p.name, o.title, o.path, o.kind, o.modified_at
             FROM objects_fts
             JOIN objects o ON o.rowid = objects_fts.rowid
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE objects_fts MATCH ?1
               AND o.is_deleted = 0
               AND o.is_noise = 0
               AND (?2 IS NULL OR o.id != ?2)
               AND (?3 IS NULL OR p.name = ?3)
             ORDER BY bm25(objects_fts, 3.0, 2.0, 1.0)
             LIMIT 20",
        )?;
        let obj_rows = obj_stmt.query_map(params![fts_query, exclude_id, project_name], |row| {
            let kind_str: String = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                ObjectKind::from_db(&kind_str),
                row.get::<_, i64>(5)?,
            ))
        })?;
        for row in obj_rows {
            let (id, pname, title, path, kind, mod_at) = row?;
            add_signal(
                candidates,
                id,
                pname,
                title,
                path,
                kind,
                mod_at,
                weight,
                "title/path FTS match".to_string(),
            );
        }

        Ok(())
    }
}

struct CandidateAccum {
    hit: RelatedHit,
    modified_at: i64,
}

#[allow(clippy::too_many_arguments)]
fn add_signal(
    map: &mut HashMap<String, CandidateAccum>,
    object_id: String,
    project_name: Option<String>,
    title: String,
    path: String,
    kind: ObjectKind,
    modified_at: i64,
    points: i64,
    reason: String,
) {
    let entry = map.entry(object_id.clone()).or_insert_with(|| CandidateAccum {
        hit: RelatedHit {
            object_id,
            project_name,
            title,
            path,
            kind,
            score: 0,
            reasons: Vec::new(),
        },
        modified_at,
    });
    entry.hit.score += points;
    if !entry.hit.reasons.contains(&reason) {
        entry.hit.reasons.push(reason);
    }
}

fn finalize_related(map: HashMap<String, CandidateAccum>, limit: usize) -> Vec<RelatedHit> {
    let mut items: Vec<CandidateAccum> = map.into_values().collect();
    items.sort_by(|a, b| {
        (b.hit.score, b.hit.kind.context_priority(), b.modified_at).cmp(&(
            a.hit.score,
            a.hit.kind.context_priority(),
            a.modified_at,
        ))
    });
    items.truncate(limit);
    items.into_iter().map(|c| c.hit).collect()
}

fn extract_title_terms(raw: &str) -> Vec<String> {
    let stem = Path::new(raw)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(raw);

    let mut out = Vec::new();
    for part in stem.split(|c: char| !c.is_alphanumeric()) {
        let lower = part.to_lowercase();
        if lower.len() >= 2
            && !matches!(lower.as_str(), "mod" | "lib" | "main" | "index" | "src" | "test" | "tests")
            && !out.contains(&lower)
        {
            out.push(lower);
        }
    }
    if out.is_empty() && !stem.is_empty() {
        out.push(stem.to_lowercase());
    }
    out
}

fn compute_active_areas(
    non_noise_objects: &[ObjectRecord],
    root_prefix: &str,
    limit: usize,
) -> Vec<ActiveArea> {
    let mut areas: HashMap<String, (usize, i64)> = HashMap::new();
    let trimmed_root = root_prefix.trim_end_matches(['/', '\\']);

    for obj in non_noise_objects.iter().take(40) {
        let rel = if !trimmed_root.is_empty() {
            obj.path
                .strip_prefix(trimmed_root)
                .map(|r| r.trim_start_matches(['/', '\\']))
                .unwrap_or(&obj.path)
        } else {
            obj.path.as_str()
        };

        let parts: Vec<&str> = rel.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
        let area = if parts.len() <= 1 {
            "(root)".to_string()
        } else if parts.len() == 2 {
            parts[0].to_string()
        } else {
            format!("{}/{}", parts[0], parts[1])
        };

        let entry = areas.entry(area).or_insert((0, obj.modified_at));
        entry.0 += 1;
        entry.1 = entry.1.max(obj.modified_at);
    }

    let mut result: Vec<ActiveArea> = areas
        .into_iter()
        .map(|(area, (object_count, latest_modified_at))| ActiveArea {
            area,
            object_count,
            latest_modified_at,
        })
        .collect();

    result.sort_by(|a, b| {
        (
            b.latest_modified_at.div_euclid(86_400),
            b.object_count,
            b.latest_modified_at,
        )
            .cmp(&(
                a.latest_modified_at.div_euclid(86_400),
                a.object_count,
                a.latest_modified_at,
            ))
    });
    result.truncate(limit);
    result
}

fn compute_starting_points(
    non_noise_objects: &[ObjectRecord],
    recently_modified: &[ObjectRecord],
    limit: usize,
) -> Vec<ObjectRecord> {
    let anchor_names = [
        "readme.md",
        "roadmap.md",
        "spec.md",
        "architecture.md",
        "architecture-rfc.md",
        "design_manifesto.md",
        "whitepaper.md",
        "changelog.md",
    ];

    let mut chosen = Vec::new();
    let mut seen_ids = HashSet::new();

    for anchor in anchor_names {
        if chosen.len() >= 3 {
            break;
        }
        if let Some(found) = non_noise_objects
            .iter()
            .find(|o| o.title.eq_ignore_ascii_case(anchor) && !seen_ids.contains(&o.id))
        {
            seen_ids.insert(found.id.clone());
            chosen.push(found.clone());
        }
    }

    for obj in recently_modified {
        if chosen.len() >= limit {
            break;
        }
        if !seen_ids.contains(&obj.id) && matches!(obj.kind, ObjectKind::Note | ObjectKind::Code) {
            seen_ids.insert(obj.id.clone());
            chosen.push(obj.clone());
        }
    }

    chosen
}

#[cfg(test)]
mod tests {
    use vos_core::util::now_unix;

    use super::*;
    use crate::UpsertObjectInput;

    #[test]
    fn resume_and_related_surface_areas_anchors_and_signals() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store
            .create_project("tessera", None, Some("OS kernel"))
            .unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                summary: Some("Tessera OS overview".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 50,
                modified_at: now - 3600,
                is_noise: false,
                chunks: vec!["Tessera OS overview and scheduler architecture".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/kernel/scx/scheduler.rs".to_string(),
                kind: ObjectKind::Code,
                title: "scheduler.rs".to_string(),
                summary: Some("pub fn schedule()".to_string()),
                content_hash: "h2".to_string(),
                size_bytes: 90,
                modified_at: now - 100,
                is_noise: false,
                chunks: vec!["pub fn schedule() { /* fair queue */ }".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/kernel/scx/fairness.rs".to_string(),
                kind: ObjectKind::Code,
                title: "fairness.rs".to_string(),
                summary: Some("scheduler fairness tests".to_string()),
                content_hash: "h3".to_string(),
                size_bytes: 90,
                modified_at: now - 200,
                is_noise: false,
                chunks: vec!["// tests scheduler queue fairness".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        store
            .add_tags("scheduler.rs", &["scheduler".to_string()])
            .unwrap();
        store
            .add_tags("fairness.rs", &["scheduler".to_string()])
            .unwrap();

        let activity = store.project_activity("tessera", 7, false).unwrap();
        assert!(activity.events.len() >= 3);

        let resume = store.project_resume("tessera").unwrap();
        assert_eq!(resume.active_objects, 3);
        assert!(resume.starting_points.iter().any(|o| o.title == "README.md"));
        assert!(!resume.most_active_areas.is_empty());

        let related = store.find_related("scheduler.rs", Some("tessera"), 5).unwrap();
        assert_eq!(related[0].title, "fairness.rs");
        assert!(related[0].reasons.iter().any(|r| r.contains("shared tag")));
        assert!(related[0].reasons.iter().any(|r| r.contains("same directory")));

        let by_topic = store.find_related("scheduler", Some("tessera"), 5).unwrap();
        assert!(!by_topic.is_empty());
    }
}
