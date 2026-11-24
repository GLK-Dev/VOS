use std::collections::{HashMap, HashSet};

use rusqlite::params;
use vos_core::util::now_unix;
use vos_core::{
    ChangedSinceReport, ConceptExplanation, DocumentationHealth, ObjectKind, ObjectRecord,
    ProjectHealth, ProjectStatus, RiskLevel,
};

use crate::concepts::ConceptDictionary;
use crate::queries::map_object_row;
use crate::recovery::object_area;
use crate::{Store, StoreError};

const ACTIVE_WINDOW_SECS: i64 = 21 * 86_400;
const WARM_WINDOW_SECS: i64 = 60 * 86_400;
const DOC_LAG_THRESHOLD_SECS: i64 = 14 * 86_400;

impl Store {
    /// Produces a deterministic decision-support health, pending-work, and risk report for a project.
    pub fn project_status(&self, project_name: &str) -> Result<ProjectStatus, StoreError> {
        let resume = self.project_resume(project_name)?;
        let now = now_unix();

        let deleted_objects: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM objects WHERE project_id = ?1 AND is_deleted = 1",
            params![resume.project.id],
            |row| row.get(0),
        )?;
        let deleted_count = deleted_objects.max(0) as usize;

        let health = if resume.active_objects == 0 {
            ProjectHealth::Empty
        } else if let Some(last) = resume.last_change_or_activity() {
            let age = now.saturating_sub(last);
            if age <= ACTIVE_WINDOW_SECS {
                ProjectHealth::Active
            } else if age <= WARM_WINDOW_SECS {
                ProjectHealth::Warm
            } else {
                ProjectHealth::Dormant
            }
        } else {
            ProjectHealth::Dormant
        };

        let key_documents: Vec<ObjectRecord> = resume
            .starting_points
            .iter()
            .filter(|o| matches!(o.kind, ObjectKind::Note | ObjectKind::Doc))
            .cloned()
            .collect();

        let (open_questions, pending_work) = self.scan_project_markers(&resume.project.id)?;
        let mut risk_areas = self.compute_risk_areas(
            &resume.project.id,
            resume.active_objects,
            deleted_count,
            health,
            resume.last_activity_at,
        )?;

        let (ownership, coverage, graph, area_risks) = self.compute_insight_bundle(project_name)?;

        let documentation_health = if resume.active_objects == 0
            || (coverage.doc_files == 0 && coverage.code_files == 0)
        {
            DocumentationHealth::Empty
        } else if coverage.doc_files == 0 {
            DocumentationHealth::Low
        } else if coverage.underdocumented.is_empty()
            || (coverage.coverage_ratio >= 25.0
                && coverage.well_covered.len() >= coverage.underdocumented.len())
        {
            DocumentationHealth::Good
        } else if coverage.coverage_ratio >= 10.0
            || !coverage.well_covered.is_empty()
            || !coverage.partially_covered.is_empty()
        {
            DocumentationHealth::Partial
        } else {
            DocumentationHealth::Low
        };

        let root_prefix = resume.project.root_path.as_deref().unwrap_or("");
        let mut dependency_hubs = Vec::new();
        let mut seen_hub_areas = HashSet::new();
        for hub in &graph.hubs {
            let deg = hub.outgoing_count + hub.incoming_count;
            if deg == 0 {
                continue;
            }
            let area = object_area(&hub.object.path, root_prefix);
            let label = if area == "(root)" {
                hub.object.title.clone()
            } else {
                area
            };
            if seen_hub_areas.insert(label.clone()) {
                dependency_hubs.push(format!("{label} ({deg} relations)"));
            }
            if dependency_hubs.len() >= 4 {
                break;
            }
        }

        // Cross ownership + coverage signal: high activity + underdocumented area
        let underdoc_areas: HashSet<&str> = coverage
            .underdocumented
            .iter()
            .map(|c| c.area.as_str())
            .collect();
        let mut extra_risks = 0_usize;
        for area in &ownership.most_active_areas {
            if underdoc_areas.contains(area.area.as_str()) {
                let entry = format!(
                    "{} — high activity + low coverage ({} files, 0 docs/refs)",
                    area.area, area.file_count
                );
                if !risk_areas.contains(&entry) {
                    risk_areas.push(entry);
                    extra_risks += 1;
                    if extra_risks >= 2 {
                        break;
                    }
                }
            }
        }
        let critical_risk_count = area_risks
            .iter()
            .filter(|r| r.level == RiskLevel::Critical)
            .count();
        let high_risk_count = area_risks
            .iter()
            .filter(|r| r.level == RiskLevel::High)
            .count();
        let critical_areas: Vec<_> = area_risks
            .into_iter()
            .filter(|r| r.level == RiskLevel::Critical)
            .take(5)
            .collect();

        for crit in critical_areas.iter().take(2) {
            let entry = format!(
                "{} — Critical Risk ({} relations, {} TODOs, 0 docs/refs)",
                crit.area, crit.relation_count, crit.open_todos
            );
            if !risk_areas.contains(&entry) {
                risk_areas.insert(0, entry);
            }
        }

        Ok(ProjectStatus {
            project: resume.project,
            health,
            last_change_at: resume.last_activity_at,
            active_objects: resume.active_objects,
            deleted_objects: deleted_count,
            key_documents,
            open_questions,
            pending_work,
            active_areas: resume.most_active_areas,
            coverage_ratio: coverage.coverage_ratio,
            documentation_health,
            critical_risk_count,
            high_risk_count,
            critical_areas,
            dependency_hubs,
            risk_areas,
        })
    }

    /// Synthesizes a deterministic concept explanation from indexed docs, code, and `config/concepts.yaml`.
    pub fn explain_concept(
        &self,
        concept: &str,
        project_name: Option<&str>,
    ) -> Result<ConceptExplanation, StoreError> {
        let trimmed = concept.trim();
        if trimmed.is_empty() {
            return Err(StoreError::InvalidInput(
                "concept cannot be empty".to_string(),
            ));
        }

        let hits = self.search_with_options(trimmed, project_name, 15, false)?;
        let mut files_contributing = Vec::new();
        let mut relevant_code = Vec::new();

        for hit in hits {
            if matches!(hit.kind, ObjectKind::Note | ObjectKind::Doc) {
                if files_contributing.len() < 4 {
                    files_contributing.push(hit);
                }
            } else if hit.kind == ObjectKind::Code && relevant_code.len() < 4 {
                relevant_code.push(hit);
            }
        }

        let definition = if let Some(top_doc) = files_contributing.first() {
            self.extract_definition(&top_doc.object_id, trimmed)?
                .or_else(|| Some(clean_snippet(&top_doc.snippet)))
        } else {
            None
        };

        let dict = ConceptDictionary::load();
        let norm_concept = trimmed.to_lowercase();
        let mut related_concepts = Vec::new();

        if let Some(syns) = dict.lookup(&norm_concept) {
            for syn in syns {
                if *syn != norm_concept && !related_concepts.contains(syn) {
                    related_concepts.push(syn.clone());
                }
            }
        }
        let terms: Vec<String> = norm_concept
            .split_whitespace()
            .map(str::to_string)
            .collect();
        for (key, syns) in dict.entries() {
            if key == &norm_concept {
                continue;
            }
            let overlaps = syns.iter().any(|s| s == &norm_concept)
                || terms.iter().any(|t| t.len() >= 4 && key.contains(t));
            if overlaps && !related_concepts.contains(key) {
                related_concepts.push(key.clone());
            }
        }
        related_concepts.truncate(6);

        Ok(ConceptExplanation {
            concept: trimmed.to_string(),
            files_contributing,
            definition,
            related_concepts,
            relevant_code,
        })
    }

    /// Computes a project-memory diff of objects modified, added, or deleted since `since_ts`.
    pub fn changed_since(
        &self,
        since_ts: i64,
        since_label: &str,
        project_name: Option<&str>,
    ) -> Result<ChangedSinceReport, StoreError> {
        let project = match project_name {
            Some(name) => Some(
                self.get_project_by_name(name)?
                    .ok_or_else(|| StoreError::NotFound("project", name.to_string()))?,
            ),
            None => None,
        };
        let pid = project.as_ref().map(|p| p.id.as_str());

        let mut ev_stmt = self.conn.prepare(
            "SELECT e.object_id, e.event_type, e.created_at
             FROM events e
             WHERE (?1 IS NULL OR e.project_id = ?1)
               AND e.created_at >= ?2
               AND e.object_id IS NOT NULL",
        )?;
        let mut events_by_obj: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        let ev_rows = ev_stmt.query_map(params![pid, since_ts], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in ev_rows {
            let (oid, et, ts) = row?;
            events_by_obj.entry(oid).or_default().push((et, ts));
        }

        let mut obj_stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
             FROM objects
             WHERE (?1 IS NULL OR project_id = ?1)
               AND is_noise = 0
               AND (
                   (is_deleted = 0 AND modified_at >= ?2)
                   OR (is_deleted = 1 AND deleted_at >= ?2)
               )
             ORDER BY COALESCE(deleted_at, modified_at) DESC",
        )?;
        let objects = obj_stmt
            .query_map(params![pid, since_ts], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let mut modified = Vec::new();
        let mut added = Vec::new();
        let mut deleted = Vec::new();

        for obj in objects {
            if obj.is_deleted {
                deleted.push(obj);
                continue;
            }

            let obj_events = events_by_obj.get(&obj.id);
            let has_modified_ev = obj_events.is_some_and(|evs| {
                evs.iter()
                    .any(|(et, _)| et == "modified" || et == "restored")
            });
            let added_near_mtime = obj_events.is_some_and(|evs| {
                evs.iter()
                    .any(|(et, ts)| et == "added" && (ts - obj.modified_at).abs() <= 86_400)
            });

            if !has_modified_ev && added_near_mtime {
                added.push(obj);
            } else {
                modified.push(obj);
            }
        }

        Ok(ChangedSinceReport {
            project_name: project.as_ref().map(|p| p.name.clone()),
            project_root: project.and_then(|p| p.root_path),
            since_label: since_label.to_string(),
            since_ts,
            modified,
            added,
            deleted,
        })
    }

    pub(crate) fn scan_project_markers(
        &self,
        project_id: &str,
    ) -> Result<(Vec<String>, Vec<String>), StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT o.id, o.title, c.text
             FROM chunks c
             JOIN objects o ON o.id = c.object_id
             WHERE o.project_id = ?1
               AND o.is_deleted = 0
               AND o.is_noise = 0
               AND o.kind = 'note'
             ORDER BY
               CASE
                 WHEN LOWER(o.title) IN ('roadmap.md', 'spec.md', 'readme.md') THEN 0
                 ELSE 1
               END,
               o.modified_at DESC,
               o.id ASC,
               c.chunk_index ASC
             LIMIT 120",
        )?;

        let rows = stmt
            .query_map(params![project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut open_questions = Vec::new();
        let mut pending_work = Vec::new();
        let mut seen_pending = HashSet::new();
        let mut current_obj_id = String::new();
        let mut active_fence: Option<char> = None;

        for (obj_id, title, text) in rows {
            if obj_id != current_obj_id {
                current_obj_id = obj_id;
                active_fence = None;
            }
            for raw_line in text.lines() {
                let trimmed = raw_line.trim();
                if let Some(next_fence) = update_fence_state(trimmed, active_fence) {
                    active_fence = next_fence;
                    continue;
                }
                if active_fence.is_some() {
                    continue;
                }

                let cleaned = trimmed.trim_start_matches(['-', '*', '>', ' ']).trim();
                let lower = cleaned.to_lowercase();

                if (lower.starts_with("open question:") || lower.starts_with("fixme:"))
                    && open_questions.len() < 5
                {
                    let entry = format!("{title}: {cleaned}");
                    if !open_questions.contains(&entry) {
                        open_questions.push(entry);
                    }
                }

                if pending_work.len() < 6 {
                    let task_text = if let Some(rest) = cleaned.strip_prefix("[ ]") {
                        Some(rest.trim())
                    } else if lower.starts_with("todo:") {
                        Some(cleaned)
                    } else {
                        None
                    };

                    if let Some(task) = task_text
                        && !task.is_empty()
                    {
                        let compact = task.replace("**", "");
                        let key = compact.to_lowercase();
                        if seen_pending.insert(key) {
                            pending_work.push(format!("{title}: {compact}"));
                        }
                    }
                }
            }
        }

        Ok((open_questions, pending_work))
    }

    fn compute_risk_areas(
        &self,
        project_id: &str,
        active_objects: usize,
        deleted_objects: usize,
        health: ProjectHealth,
        last_activity_at: Option<i64>,
    ) -> Result<Vec<String>, StoreError> {
        let mut risks = Vec::new();

        if active_objects == 0 {
            risks.push("no indexed files in project".to_string());
            return Ok(risks);
        }

        if health == ProjectHealth::Dormant
            && let Some(ts) = last_activity_at
        {
            let days = now_unix().saturating_sub(ts) / 86_400;
            risks.push(format!(
                "project dormant ({days} days since last modification)"
            ));
        }

        let has_readme: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM objects
             WHERE project_id = ?1 AND is_deleted = 0 AND LOWER(title) = 'readme.md'",
            params![project_id],
            |row| row.get(0),
        )?;
        if has_readme == 0 {
            risks.push("missing README.md".to_string());
        }

        let latest_code: Option<i64> = self.conn.query_row(
            "SELECT MAX(modified_at) FROM objects
             WHERE project_id = ?1 AND is_deleted = 0 AND is_noise = 0 AND kind = 'code'",
            params![project_id],
            |row| row.get(0),
        )?;
        let latest_doc: Option<i64> = self.conn.query_row(
            "SELECT MAX(modified_at) FROM objects
             WHERE project_id = ?1 AND is_deleted = 0 AND is_noise = 0 AND kind IN ('note', 'doc')",
            params![project_id],
            |row| row.get(0),
        )?;

        match (latest_code, latest_doc) {
            (Some(_), None) => risks.push("no documentation or notes indexed".to_string()),
            (Some(c_ts), Some(d_ts)) if c_ts - d_ts > DOC_LAG_THRESHOLD_SECS => {
                let lag_days = (c_ts - d_ts) / 86_400;
                risks.push(format!(
                    "no recent updates in docs (lag code by {lag_days} days)"
                ));
            }
            _ => {}
        }

        let total = active_objects + deleted_objects;
        if total >= 10 && deleted_objects * 4 > total {
            risks.push(format!(
                "high churn: {deleted_objects}/{total} objects soft-deleted"
            ));
        }

        Ok(risks)
    }

    fn extract_definition(
        &self,
        object_id: &str,
        concept: &str,
    ) -> Result<Option<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT text FROM chunks WHERE object_id = ?1 ORDER BY chunk_index ASC LIMIT 5",
        )?;
        let chunks = stmt
            .query_map(params![object_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let terms: Vec<String> = concept.split_whitespace().map(str::to_lowercase).collect();

        for chunk in &chunks {
            for para in chunk.split("\n\n") {
                let compact = para
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let lower = compact.to_lowercase();
                if compact.len() >= 40 && terms.iter().any(|t| lower.contains(t)) {
                    let truncated: String = compact.chars().take(280).collect();
                    return Ok(Some(if compact.chars().count() > 280 {
                        format!("{truncated}...")
                    } else {
                        truncated
                    }));
                }
            }
        }

        Ok(None)
    }
}

trait LastChangeHelper {
    fn last_change_or_activity(&self) -> Option<i64>;
}

impl LastChangeHelper for vos_core::ProjectResume {
    fn last_change_or_activity(&self) -> Option<i64> {
        self.last_activity_at
    }
}

fn clean_snippet(raw: &str) -> String {
    raw.replace(['[', ']'], "").replace('\n', " ")
}

fn update_fence_state(trimmed: &str, current: Option<char>) -> Option<Option<char>> {
    let fence_char = if trimmed.starts_with("```") {
        '`'
    } else if trimmed.starts_with("~~~") {
        '~'
    } else {
        return None;
    };

    match current {
        Some(active) if active == fence_char && trimmed.chars().all(|c| c == active) => Some(None),
        Some(_) => None,
        None => {
            let after = trimmed.trim_start_matches(fence_char).trim_end();
            let same_line_close = (fence_char == '`' && after.ends_with("```"))
                || (fence_char == '~' && after.ends_with("~~~"));
            if same_line_close {
                None
            } else {
                Some(Some(fence_char))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use vos_core::ObjectKind;
    use vos_core::util::now_unix;

    use super::*;
    use crate::UpsertObjectInput;

    #[test]
    fn status_explain_and_changed_since_derive_real_signals() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("demo", None, None).unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                summary: Some("Project memory overview".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 100,
                modified_at: now - 30 * 86_400,
                is_noise: false,
                chunks: vec![
                    "Project context is the active working set of files and events.".to_string(),
                ],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/ROADMAP.md".to_string(),
                kind: ObjectKind::Note,
                title: "ROADMAP.md".to_string(),
                summary: None,
                content_hash: "h2".to_string(),
                size_bytes: 120,
                modified_at: now - 25 * 86_400,
                is_noise: false,
                chunks: vec![
                    "- [ ] Add scheduler fairness verification\nOPEN QUESTION: keep WAL mode?"
                        .to_string(),
                ],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/src/context.rs".to_string(),
                kind: ObjectKind::Code,
                title: "context.rs".to_string(),
                summary: Some("pub fn project_context()".to_string()),
                content_hash: "h3".to_string(),
                size_bytes: 90,
                modified_at: now - 3600,
                is_noise: false,
                chunks: vec!["// Computes project context snapshot".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let status = store.project_status("demo").unwrap();
        assert_eq!(status.health, ProjectHealth::Active);
        assert_eq!(status.pending_work.len(), 1);
        assert_eq!(status.open_questions.len(), 1);
        assert!(
            status
                .risk_areas
                .iter()
                .any(|r| r.contains("no recent updates in docs"))
        );

        let explained = store
            .explain_concept("project context", Some("demo"))
            .unwrap();
        assert!(!explained.files_contributing.is_empty());
        assert!(explained.definition.is_some());
        assert!(!explained.relevant_code.is_empty());

        let diff = store
            .changed_since(now - 86_400, "yesterday", Some("demo"))
            .unwrap();
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].title, "context.rs");
    }

    #[test]
    fn scan_project_markers_ignores_fenced_code_blocks_across_chunks() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("fence", None, None).unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/fence/docs/guide.md".to_string(),
                kind: ObjectKind::Note,
                title: "guide.md".to_string(),
                summary: None,
                content_hash: "h_guide".to_string(),
                size_bytes: 200,
                modified_at: now - 60,
                is_noise: false,
                chunks: vec![
                    "```md\n- [ ] Fake fenced task\nOPEN QUESTION: fake fenced question?\n".to_string(),
                    "TODO: still inside fence\n```\n- [ ] Real task after fence\nOPEN QUESTION: real question?".to_string(),
                ],
            })
            .unwrap();
        batch.commit().unwrap();

        let (questions, pending) = store.scan_project_markers(&proj.id).unwrap();
        assert_eq!(questions, vec!["guide.md: OPEN QUESTION: real question?"]);
        assert_eq!(pending, vec!["guide.md: Real task after fence"]);
    }
}
