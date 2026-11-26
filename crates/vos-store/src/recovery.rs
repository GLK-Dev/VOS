use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::params;
use vos_core::util::now_unix;
use vos_core::{
    ActiveArea, ContinueSuggestion, EventRecord, EventType, GraphDirection, ObjectKind,
    ObjectRecord, ObjectWhy, ProjectActivity, ProjectContinue, ProjectResume, ProjectStart,
    RelatedHit, RelationType,
};

use crate::queries::{build_fts_or_query, map_object_row};
use crate::{Store, StoreError};

/// Score per shared tag between two objects.
const SHARED_TAG_SCORE: i64 = 40;
/// Score for residing in the exact same directory as the target object.
const SAME_DIR_SCORE: i64 = 30;
/// Score for residing in a sibling module under the same parent folder.
const NEARBY_MODULE_SCORE: i64 = 15;
/// Score for objects modified in the same coding session window.
const SAME_SESSION_SCORE: i64 = 15;
/// Score when a topic term matches the object title or file stem.
const RELATED_TITLE_MATCH_SCORE: i64 = 35;
/// Score when a topic term appears in the object path.
const RELATED_PATH_MATCH_SCORE: i64 = 20;
/// Base score for FTS content/object matches, plus rank & multi-chunk bonuses.
const RELATED_CHUNK_FTS_BASE: i64 = 20;
const RELATED_OBJECT_FTS_BASE: i64 = 15;
const RELATED_FTS_RANK_BONUS_MAX: i64 = 15;
const RELATED_MULTI_CHUNK_BONUS: i64 = 5;

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

    /// Builds an actionable continuation briefing (`resume + activity + why`) for `vos continue`.
    pub fn project_continue(&self, project_name: &str) -> Result<ProjectContinue, StoreError> {
        let start = self.project_start(project_name)?;
        let resume = start.resume;
        let root_prefix = resume.project.root_path.as_deref().unwrap_or("");

        let ownership = self.project_ownership(project_name, 14, 4)?;
        let coverage = self.project_coverage(project_name)?;
        let graph = self.project_graph(project_name, None, GraphDirection::Both, 1, 4)?;

        let mut hub_degrees: HashMap<String, usize> = HashMap::new();
        for hub in &graph.hubs {
            hub_degrees.insert(
                hub.object.id.clone(),
                hub.outgoing_count + hub.incoming_count,
            );
        }

        let mut continue_from = Vec::new();
        let mut seen_ids = HashSet::new();

        // 1. Primary dependency hub when strongly connected (>= 3 relations)
        if let Some(top_hub) = graph.hubs.first() {
            let deg = top_hub.outgoing_count + top_hub.incoming_count;
            if deg >= 3 {
                seen_ids.insert(top_hub.object.id.clone());
                continue_from.push(ContinueSuggestion {
                    object: top_hub.object.clone(),
                    reason: format!("Hub node ({deg} relations)"),
                });
            }
        }

        // 2. Primary anchor document (e.g., ROADMAP.md / SPEC.md / README.md)
        for sp in &resume.starting_points {
            if continue_from.len() >= 2 {
                break;
            }
            if seen_ids.contains(&sp.id) {
                continue;
            }
            let area = object_area(&sp.path, root_prefix);
            let mut reason = if sp.title.eq_ignore_ascii_case("roadmap.md")
                || sp.title.eq_ignore_ascii_case("spec.md")
            {
                format!("key roadmap/spec entry point (area: {area})")
            } else {
                format!("project entry point (area: {area})")
            };
            if let Some(deg) = hub_degrees.get(&sp.id).copied().filter(|d| *d >= 2) {
                reason.push_str(&format!(" | hub node ({deg} relations)"));
            }
            seen_ids.insert(sp.id.clone());
            continue_from.push(ContinueSuggestion {
                object: sp.clone(),
                reason,
            });
        }

        // 3. Most recent active code file not already included (enriched by dependency degree)
        if let Some(code_obj) = resume
            .recently_modified
            .iter()
            .find(|o| o.kind == ObjectKind::Code && !seen_ids.contains(&o.id))
        {
            let area = object_area(&code_obj.path, root_prefix);
            let deg = self
                .object_relations(&code_obj.id, GraphDirection::Both)?
                .len();
            let reason = if deg > 0 {
                format!("latest code activity (area: {area}, {deg} relations)")
            } else {
                format!("latest code activity (area: {area})")
            };
            seen_ids.insert(code_obj.id.clone());
            continue_from.push(ContinueSuggestion {
                object: code_obj.clone(),
                reason,
            });
        }

        // 4. Fill up to 4 with top remaining recently modified items
        for obj in &resume.recently_modified {
            if continue_from.len() >= 4 {
                break;
            }
            if !seen_ids.contains(&obj.id) {
                let area = object_area(&obj.path, root_prefix);
                seen_ids.insert(obj.id.clone());
                continue_from.push(ContinueSuggestion {
                    object: obj.clone(),
                    reason: format!("recently modified {} (area: {area})", obj.kind),
                });
            }
        }

        let coverage_warnings = coverage.underdocumented.into_iter().take(4).collect();
        let last_session = self
            .list_sessions(Some(project_name), 1)?
            .into_iter()
            .next();

        Ok(ProjectContinue {
            resume,
            continue_from,
            recent_events: start.recent_events,
            open_questions: start.open_questions,
            ownership_areas: ownership.most_active_areas,
            coverage_warnings,
            dependency_hubs: graph.hubs,
            last_session,
        })
    }
    /// Builds a unified project kickoff briefing combining resume, recent activity, and open markers.
    pub fn project_start(&self, project_name: &str) -> Result<ProjectStart, StoreError> {
        let resume = self.project_resume(project_name)?;
        let project_id = resume.project.id.clone();

        let mut ev_stmt = self.conn.prepare(
            "SELECT e.id, e.project_id, e.object_id, e.event_type, e.path, e.detail, e.created_at
             FROM events e
             LEFT JOIN objects o ON o.id = e.object_id
             WHERE e.project_id = ?1
               AND COALESCE(o.is_noise, 0) = 0
             ORDER BY e.created_at DESC, e.id DESC
             LIMIT 8",
        )?;
        let recent_events = ev_stmt
            .query_map(params![project_id], |row| {
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
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let (open_questions, _) = self.scan_project_markers(&project_id)?;

        Ok(ProjectStart {
            resume,
            recent_events,
            open_questions,
        })
    }
    /// Explains why an object matters within its project using deterministic signals.
    pub fn why_object(
        &self,
        target: &str,
        project_name: Option<&str>,
    ) -> Result<ObjectWhy, StoreError> {
        let details = self.show_object_scoped(target, project_name)?;
        let obj = details.object;

        let project = match details.project_name.as_deref() {
            Some(name) => self.get_project_by_name(name)?,
            None => None,
        };
        let project_root = project.as_ref().and_then(|p| p.root_path.clone());
        let area = object_area(&obj.path, project_root.as_deref().unwrap_or(""));

        let (is_active_area, is_starting_point) = match details.project_name.as_deref() {
            Some(name) => {
                let resume = self.project_resume(name)?;
                let in_area = resume.most_active_areas.iter().any(|a| a.area == area);
                let is_start = resume.starting_points.iter().any(|s| s.id == obj.id);
                (in_area, is_start)
            }
            None => (false, false),
        };

        let outgoing_relations = self.object_relations(&obj.id, GraphDirection::Outgoing)?;
        let incoming_relations = self.object_relations(&obj.id, GraphDirection::Incoming)?;

        let related =
            self.related_to_object(&obj, &details.tags, details.project_name.as_deref(), 5)?;

        let mut ev_stmt = self.conn.prepare(
            "SELECT id, project_id, object_id, event_type, path, detail, created_at
             FROM events
             WHERE object_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT 5",
        )?;
        let recent_events = ev_stmt
            .query_map(params![obj.id], |row| {
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
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ObjectWhy {
            object: obj,
            project_name: details.project_name,
            project_root,
            area,
            is_active_area,
            is_starting_point,
            tags: details.tags,
            related,
            outgoing_relations,
            incoming_relations,
            recent_events,
        })
    }
    /// Finds related objects using rank-aware FTS5 overlap, title/path signals, shared tags, and session timing.
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
        if let Ok(details) = self.show_object_scoped(trimmed, project_name) {
            return self.related_to_object(
                &details.object,
                &details.tags,
                project_name,
                capped_limit,
            );
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

        // 0. Deterministic dependency graph edges (outgoing + incoming)
        let mut graph_stmt = self.conn.prepare(
            "SELECT CASE WHEN r.from_id = ?1 THEN r.to_id ELSE r.from_id END AS neighbor_id,
                    CASE WHEN r.from_id = ?1 THEN 1 ELSE 0 END AS is_outgoing,
                    o.id, p.name, o.title, o.path, o.kind, o.modified_at, r.relation_type
             FROM relations r
             JOIN objects o ON o.id = CASE WHEN r.from_id = ?1 THEN r.to_id ELSE r.from_id END
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE (r.from_id = ?1 OR r.to_id = ?1)
               AND o.is_deleted = 0
               AND o.is_noise = 0
               AND (?2 IS NULL OR p.name = ?2)",
        )?;
        let graph_rows = graph_stmt.query_map(params![src.id, project_name], |row| {
            let kind_str: String = row.get(6)?;
            let rel_str: String = row.get(8)?;
            Ok((
                row.get::<_, i64>(1)? != 0,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                ObjectKind::from_db(&kind_str),
                row.get::<_, i64>(7)?,
                RelationType::from_db(&rel_str),
            ))
        })?;
        for row in graph_rows {
            let (is_out, id, pname, title, path, kind, mod_at, rel_type) = row?;
            let (score, reason) = match (rel_type, is_out) {
                (RelationType::RustMod, true) => (60, "module dependency (rust_mod)".to_string()),
                (RelationType::RustMod, false) => (60, "module parent (rust_mod)".to_string()),
                (RelationType::MdLink, true) => (55, "linked document (md_link)".to_string()),
                (RelationType::MdLink, false) => (55, "referenced in docs (md_link)".to_string()),
                (RelationType::RustUse, true) => (50, "imports module (rust_use)".to_string()),
                (RelationType::RustUse, false) => (50, "imported by (rust_use)".to_string()),
                (RelationType::PathRef, true) => (35, "references file (path_ref)".to_string()),
                (RelationType::PathRef, false) => (35, "referenced by (path_ref)".to_string()),
            };
            add_signal(
                &mut candidates,
                id,
                pname,
                title,
                path,
                kind,
                mod_at,
                score,
                reason,
            );
        }

        // 1. Shared tags
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
                SHARED_TAG_SCORE,
                format!("shared tag '{tag}'"),
            );
        }

        // 2. Rank-aware FTS5 overlap from object path/module stem + tags
        let mut terms = extract_object_related_terms(&src.path);
        for (tag, _) in src_tags {
            if !terms.contains(tag) {
                terms.push(tag.clone());
            }
        }
        terms.truncate(6);

        if let Some(fts_query) = build_fts_or_query(&terms) {
            self.collect_fts_related(&fts_query, Some(&src.id), project_name, &mut candidates)?;
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
                        SAME_DIR_SCORE,
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
                        NEARBY_MODULE_SCORE,
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
                        SAME_SESSION_SCORE,
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
            self.collect_fts_related(&fts_query, None, project_name, &mut candidates)?;
        }

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
            let title_lower = title.to_lowercase();
            let path_lower = path.to_lowercase();

            for term in &terms {
                if term.len() < 3 {
                    continue;
                }
                if title_lower.contains(term) {
                    add_signal(
                        &mut candidates,
                        id.clone(),
                        pname.clone(),
                        title.clone(),
                        path.clone(),
                        kind,
                        mod_at,
                        RELATED_TITLE_MATCH_SCORE,
                        format!("title matches '{term}'"),
                    );
                }
                if path_lower.contains(term) {
                    add_signal(
                        &mut candidates,
                        id.clone(),
                        pname.clone(),
                        title.clone(),
                        path.clone(),
                        kind,
                        mod_at,
                        RELATED_PATH_MATCH_SCORE,
                        format!("path matches '{term}'"),
                    );
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
                    SHARED_TAG_SCORE,
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
        candidates: &mut HashMap<String, CandidateAccum>,
    ) -> Result<(), StoreError> {
        let mut chunk_stmt = self.conn.prepare(
            "SELECT o.id, p.name, o.title, o.path, o.kind, o.modified_at
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
             LIMIT 60",
        )?;

        let chunk_rows = chunk_stmt
            .query_map(params![fts_query, exclude_id, project_name], |row| {
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

        let mut chunk_seen: HashMap<String, usize> = HashMap::new();
        for (pos, (id, pname, title, path, kind, mod_at)) in chunk_rows.into_iter().enumerate() {
            let count = chunk_seen.entry(id.clone()).or_insert(0);
            *count += 1;
            if *count == 1 {
                let rank_bonus = (RELATED_FTS_RANK_BONUS_MAX - (pos as i64 / 2)).max(0);
                add_signal(
                    candidates,
                    id,
                    pname,
                    title,
                    path,
                    kind,
                    mod_at,
                    RELATED_CHUNK_FTS_BASE + rank_bonus,
                    "content FTS match".to_string(),
                );
            } else if *count <= 4 {
                add_signal(
                    candidates,
                    id,
                    pname,
                    title,
                    path,
                    kind,
                    mod_at,
                    RELATED_MULTI_CHUNK_BONUS,
                    "multiple content matches".to_string(),
                );
            }
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
             ORDER BY bm25(objects_fts, 10.0, 3.0, 1.0)
             LIMIT 30",
        )?;
        let obj_rows = obj_stmt
            .query_map(params![fts_query, exclude_id, project_name], |row| {
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

        for (pos, (id, pname, title, path, kind, mod_at)) in obj_rows.into_iter().enumerate() {
            let rank_bonus = (RELATED_FTS_RANK_BONUS_MAX - pos as i64).max(0);
            add_signal(
                candidates,
                id,
                pname,
                title,
                path,
                kind,
                mod_at,
                RELATED_OBJECT_FTS_BASE + rank_bonus,
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
    let entry = map
        .entry(object_id.clone())
        .or_insert_with(|| CandidateAccum {
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

const GENERIC_MODULE_STEMS: &[&str] = &["mod", "lib", "main", "index"];
const GENERIC_PARENT_DIRS: &[&str] = &["src", "lib", "app", "crates", "apps", "tests"];

fn extract_object_related_terms(raw_path: &str) -> Vec<String> {
    let path = Path::new(raw_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(raw_path);
    let stem_lower = stem.to_lowercase();

    if GENERIC_MODULE_STEMS.contains(&stem_lower.as_str()) {
        let mut cursor = path.parent();
        while let Some(dir) = cursor {
            if let Some(dir_name) = dir.file_name().and_then(|s| s.to_str()) {
                let lower_dir = dir_name.to_lowercase();
                if !GENERIC_PARENT_DIRS.contains(&lower_dir.as_str()) {
                    let terms = split_non_generic_terms(dir_name);
                    if !terms.is_empty() {
                        return terms;
                    }
                }
            }
            cursor = dir.parent();
        }
    }

    extract_title_terms(stem)
}

fn split_non_generic_terms(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split(|c: char| !c.is_alphanumeric()) {
        let lower = part.to_lowercase();
        if lower.len() >= 2
            && !GENERIC_MODULE_STEMS.contains(&lower.as_str())
            && !GENERIC_PARENT_DIRS.contains(&lower.as_str())
            && !matches!(lower.as_str(), "test")
            && !out.contains(&lower)
        {
            out.push(lower);
        }
    }
    out
}

fn extract_title_terms(raw: &str) -> Vec<String> {
    let stem = Path::new(raw)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(raw);

    let mut out = split_non_generic_terms(stem);
    if out.is_empty() && !stem.is_empty() {
        out.push(stem.to_lowercase());
    }
    out
}

pub(crate) fn object_area(path: &str, root_prefix: &str) -> String {
    let trimmed_root = root_prefix.trim_end_matches(['/', '\\']);
    let rel = if !trimmed_root.is_empty() {
        path.strip_prefix(trimmed_root)
            .map(|r| r.trim_start_matches(['/', '\\']))
            .unwrap_or(path)
    } else {
        path
    };

    let parts: Vec<&str> = rel.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.len() <= 1 {
        "(root)".to_string()
    } else if parts.len() == 2 {
        parts[0].to_string()
    } else {
        format!("{}/{}", parts[0], parts[1])
    }
}
fn compute_active_areas(
    non_noise_objects: &[ObjectRecord],
    root_prefix: &str,
    limit: usize,
) -> Vec<ActiveArea> {
    let mut areas: HashMap<String, (usize, i64)> = HashMap::new();
    let trimmed_root = root_prefix.trim_end_matches(['/', '\\']);

    let _ = trimmed_root;
    let has_primary = non_noise_objects.iter().any(|o| o.kind.is_primary_work_kind());
    for obj in non_noise_objects
        .iter()
        .filter(|o| !has_primary || o.kind.is_primary_work_kind())
        .take(40)
    {
        let area = object_area(&obj.path, root_prefix);

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
        assert!(
            resume
                .starting_points
                .iter()
                .any(|o| o.title == "README.md")
        );
        assert!(!resume.most_active_areas.is_empty());

        let related = store
            .find_related("scheduler.rs", Some("tessera"), 5)
            .unwrap();
        assert_eq!(related[0].title, "fairness.rs");
        assert!(related[0].reasons.iter().any(|r| r.contains("shared tag")));
        assert!(
            related[0]
                .reasons
                .iter()
                .any(|r| r.contains("same directory"))
        );

        let by_topic = store.find_related("scheduler", Some("tessera"), 5).unwrap();
        assert_eq!(by_topic[0].title, "scheduler.rs");

        let why = store.why_object("scheduler.rs", Some("tessera")).unwrap();
        assert_eq!(why.project_name.as_deref(), Some("tessera"));
        assert_eq!(why.area, "repo/kernel");
        assert!(why.is_active_area);
        assert!(!why.related.is_empty());
        assert!(!why.recent_events.is_empty());
    }

    #[test]
    fn related_resolves_parent_module_for_generic_mod_rs_files() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("rustallm", None, None).unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/src/transformer/mod.rs".to_string(),
                kind: ObjectKind::Code,
                title: "mod.rs".to_string(),
                summary: Some("pub mod kv_cache;".to_string()),
                content_hash: "h_t_mod".to_string(),
                size_bytes: 50,
                modified_at: now - 100,
                is_noise: false,
                chunks: vec!["pub mod kv_cache; pub mod layer;".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/src/transformer/kv_cache.rs".to_string(),
                kind: ObjectKind::Code,
                title: "kv_cache.rs".to_string(),
                summary: Some("Transformer KV cache".to_string()),
                content_hash: "h_kv".to_string(),
                size_bytes: 80,
                modified_at: now - 110,
                is_noise: false,
                chunks: vec!["// transformer kv cache paging".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/src/engine/mod.rs".to_string(),
                kind: ObjectKind::Code,
                title: "mod.rs".to_string(),
                summary: Some("pub mod runner;".to_string()),
                content_hash: "h_e_mod".to_string(),
                size_bytes: 50,
                modified_at: now - 5000,
                is_noise: false,
                chunks: vec!["pub mod runner;".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let related = store
            .find_related("transformer/mod.rs", Some("rustallm"), 5)
            .unwrap();
        assert_eq!(related[0].title, "kv_cache.rs");
    }

    #[test]
    fn continue_surfaces_latest_work_session() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("contproj", None, None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                summary: Some("Project readme".to_string()),
                content_hash: "h_readme".to_string(),
                size_bytes: 120,
                modified_at: now_unix() - 60,
                is_noise: false,
                chunks: vec!["# ContProj".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let started = store
            .start_session("contproj", Some("check recovery"))
            .unwrap();
        assert!(started.ended_at.is_none());
        let cont_active = store.project_continue("contproj").unwrap();
        let active_sess = cont_active.last_session.expect("active session present");
        assert!(active_sess.ended_at.is_none());
        assert_eq!(active_sess.start_note.as_deref(), Some("check recovery"));

        store
            .stop_session(Some("contproj"), Some(5), Some("Next: wire continue tests"))
            .unwrap();
        let cont_done = store.project_continue("contproj").unwrap();
        let done_sess = cont_done.last_session.expect("completed session present");
        assert!(done_sess.ended_at.is_some());
        assert_eq!(done_sess.recovery_quality, Some(5));
        assert_eq!(
            done_sess.stop_note.as_deref(),
            Some("Next: wire continue tests")
        );
    }

    #[test]
    fn project_start_and_continue_align_open_questions_with_status() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("aligned", None, None).unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/aligned/ROADMAP.md".to_string(),
                kind: ObjectKind::Note,
                title: "ROADMAP.md".to_string(),
                summary: None,
                content_hash: "h_align".to_string(),
                size_bytes: 160,
                modified_at: now - 30,
                is_noise: false,
                chunks: vec![
                    "```md\nOPEN QUESTION: ignored example?\n```\n- [ ] Task item\nTODO: not a question\nOPEN QUESTION: keep WAL?"
                        .to_string(),
                ],
            })
            .unwrap();
        batch.commit().unwrap();

        let start = store.project_start("aligned").unwrap();
        let cont = store.project_continue("aligned").unwrap();
        let status = store.project_status("aligned").unwrap();

        assert_eq!(
            start.open_questions,
            vec!["ROADMAP.md: OPEN QUESTION: keep WAL?"]
        );
        assert_eq!(cont.open_questions, start.open_questions);
        assert_eq!(status.open_questions, start.open_questions);
    }

    #[test]
    fn compute_active_areas_skips_bulk_media_assets_when_work_files_exist() {
        let now = now_unix();
        let mut objs = Vec::new();
        for i in 0..45 {
            objs.push(ObjectRecord {
                id: format!("m{i}"),
                project_id: Some("p".to_string()),
                path: format!("/repo/design/icons/ic_{i}.png"),
                kind: ObjectKind::Media,
                title: format!("ic_{i}.png"),
                summary: None,
                content_hash: "hm".to_string(),
                size_bytes: 10,
                modified_at: now - i as i64,
                indexed_at: now,
                is_deleted: false,
                deleted_at: None,
                is_noise: false,
            });
        }
        objs.push(ObjectRecord {
            id: "c1".to_string(),
            project_id: Some("p".to_string()),
            path: "/repo/crates/ui/lib.rs".to_string(),
            kind: ObjectKind::Code,
            title: "lib.rs".to_string(),
            summary: None,
            content_hash: "hc".to_string(),
            size_bytes: 50,
            modified_at: now - 100,
            indexed_at: now,
            is_deleted: false,
            deleted_at: None,
            is_noise: false,
        });

        let areas = compute_active_areas(&objs, "/repo", 5);
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].area, "crates/ui");
    }
}
