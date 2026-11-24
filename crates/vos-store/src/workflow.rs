use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, params};
use vos_core::util::{new_id, now_unix};
use vos_core::{
    DifficultyStats, DocumentationHealth, ProjectHealth, ProjectSnapshot, RecoveryDifficulty,
    SessionStats, SnapshotDiff, WorkSession,
};

use crate::recovery::object_area;
use crate::{Store, StoreError};

impl Store {
    fn build_live_snapshot(
        &self,
        project_name: &str,
        label: Option<&str>,
    ) -> Result<ProjectSnapshot, StoreError> {
        let st = self.project_status(project_name)?;
        let res = self.project_resume(project_name)?;
        let now = now_unix();

        let relations_count: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM relations r
             JOIN objects o_from ON o_from.id = r.from_id
             JOIN objects o_to ON o_to.id = r.to_id
             WHERE o_from.project_id = ?1
               AND o_to.project_id = ?1
               AND o_from.is_deleted = 0
               AND o_to.is_deleted = 0",
            params![st.project.id],
            |row| row.get(0),
        )?;

        let root_prefix = st.project.root_path.as_deref().unwrap_or("");
        let active_areas: Vec<String> = st
            .active_areas
            .iter()
            .map(|a| format!("{} ({} files)", a.area, a.object_count))
            .collect();
        let recent_changes: Vec<String> = res
            .recently_modified
            .iter()
            .take(6)
            .map(|o| relative_display(&o.path, root_prefix))
            .collect();

        let clean_label = label
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        Ok(ProjectSnapshot {
            id: 0,
            project_id: st.project.id,
            project_name: st.project.name,
            label: clean_label,
            health: st.health,
            active_objects: st.active_objects,
            deleted_objects: st.deleted_objects,
            relations_count: relations_count.max(0) as usize,
            coverage_ratio: st.coverage_ratio,
            documentation_health: st.documentation_health,
            critical_risks: st.critical_risk_count,
            high_risks: st.high_risk_count,
            open_questions: st.open_questions,
            pending_work: st.pending_work,
            active_areas,
            risk_areas: st.risk_areas,
            recent_changes,
            created_at: now,
        })
    }

    /// Captures and persists a point-in-time snapshot of project status, coverage, and risks.
    pub fn save_snapshot(
        &self,
        project_name: &str,
        label: Option<&str>,
    ) -> Result<ProjectSnapshot, StoreError> {
        let snap = self.build_live_snapshot(project_name, label)?;

        self.conn.execute(
            "INSERT INTO snapshots (
                project_id, label, health, active_objects, deleted_objects,
                relations_count, coverage_ratio, documentation_health,
                critical_risks, high_risks, open_questions_text, pending_work_text,
                active_areas_text, risk_areas_text, recent_changes_text, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                snap.project_id,
                snap.label,
                snap.health.as_str(),
                snap.active_objects as i64,
                snap.deleted_objects as i64,
                snap.relations_count as i64,
                snap.coverage_ratio,
                snap.documentation_health.as_str(),
                snap.critical_risks as i64,
                snap.high_risks as i64,
                encode_list(&snap.open_questions)?,
                encode_list(&snap.pending_work)?,
                encode_list(&snap.active_areas)?,
                encode_list(&snap.risk_areas)?,
                encode_list(&snap.recent_changes)?,
                snap.created_at,
            ],
        )?;

        let id = self.conn.last_insert_rowid();
        self.get_snapshot(id)
    }

    /// Captures and persists point-in-time snapshots for all registered projects ordered by name.
    pub fn save_all_snapshots(
        &self,
        label: Option<&str>,
    ) -> Result<Vec<ProjectSnapshot>, StoreError> {
        let mut projects = self.list_projects()?;
        projects.sort_by(|a, b| a.project.name.cmp(&b.project.name));

        let mut saved = Vec::with_capacity(projects.len());
        for summary in projects {
            saved.push(self.save_snapshot(&summary.project.name, label)?);
        }
        Ok(saved)
    }

    /// Loads a single saved snapshot by numeric ID.
    pub fn get_snapshot(&self, id: i64) -> Result<ProjectSnapshot, StoreError> {
        self.conn
            .query_row(
                "SELECT s.id, s.project_id, p.name, s.label, s.health,
                        s.active_objects, s.deleted_objects, s.relations_count,
                        s.coverage_ratio, s.documentation_health,
                        s.critical_risks, s.high_risks,
                        s.open_questions_text, s.pending_work_text,
                        s.active_areas_text, s.risk_areas_text,
                        s.recent_changes_text, s.created_at
                 FROM snapshots s
                 JOIN projects p ON p.id = s.project_id
                 WHERE s.id = ?1",
                params![id],
                map_snapshot_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound("snapshot", id.to_string()))
    }

    /// Lists saved snapshots, optionally scoped to `project_name` (newest first).
    pub fn list_snapshots(
        &self,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProjectSnapshot>, StoreError> {
        let capped = limit.clamp(1, 100) as i64;
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.project_id, p.name, s.label, s.health,
                    s.active_objects, s.deleted_objects, s.relations_count,
                    s.coverage_ratio, s.documentation_health,
                    s.critical_risks, s.high_risks,
                    s.open_questions_text, s.pending_work_text,
                    s.active_areas_text, s.risk_areas_text,
                    s.recent_changes_text, s.created_at
             FROM snapshots s
             JOIN projects p ON p.id = s.project_id
             WHERE (?1 IS NULL OR p.name = ?1)
             ORDER BY s.created_at DESC, s.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_name, capped], map_snapshot_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Compares two persisted snapshots and computes deltas and resolved/new work items.
    pub fn compare_snapshots(&self, from_id: i64, to_id: i64) -> Result<SnapshotDiff, StoreError> {
        let from = self.get_snapshot(from_id)?;
        let to = self.get_snapshot(to_id)?;
        Ok(compute_snapshot_diff(from, to))
    }

    /// Compares a saved snapshot (or the latest saved snapshot for `project_name`) against live state.
    pub fn compare_snapshot_to_live(
        &self,
        project_name: &str,
        from_id: Option<i64>,
    ) -> Result<SnapshotDiff, StoreError> {
        let from = match from_id {
            Some(id) => {
                let snap = self.get_snapshot(id)?;
                if snap.project_name != project_name {
                    return Err(StoreError::InvalidInput(format!(
                        "snapshot #{id} belongs to project '{}', not '{project_name}'",
                        snap.project_name
                    )));
                }
                snap
            }
            None => self
                .list_snapshots(Some(project_name), 1)?
                .into_iter()
                .next()
                .ok_or_else(|| StoreError::NotFound("snapshot", project_name.to_string()))?,
        };
        let live = self.build_live_snapshot(project_name, Some("live"))?;
        Ok(compute_snapshot_diff(from, live))
    }

    /// Starts a tracked work/recovery session for `project_name`, capturing predicted difficulty.
    pub fn start_session(
        &self,
        project_name: &str,
        note: Option<&str>,
    ) -> Result<WorkSession, StoreError> {
        let st = self.project_status(project_name)?;
        let existing_open: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM work_sessions WHERE project_id = ?1 AND ended_at IS NULL LIMIT 1",
                params![st.project.id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(open_id) = existing_open {
            return Err(StoreError::InvalidInput(format!(
                "active session '{open_id}' already running for '{project_name}'; run `vos session stop {project_name}` first"
            )));
        }

        let cont = self.project_continue(project_name)?;
        let root_prefix = st.project.root_path.as_deref().unwrap_or("");
        let return_point = cont
            .continue_from
            .first()
            .map(|c| relative_display(&c.object.path, root_prefix));

        let pred = self.predict_recovery_difficulty(&st);
        let session_id = new_id("sess");
        let now = now_unix();
        let clean_note = note
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        self.conn.execute(
            "INSERT INTO work_sessions (
                id, project_id, started_at, ended_at, duration_secs,
                predicted_difficulty, predicted_score, recovery_quality,
                return_point, start_note, stop_note, files_changed_count, productive_areas_text
             ) VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5, NULL, ?6, ?7, NULL, 0, '')",
            params![
                session_id,
                st.project.id,
                now,
                pred.difficulty.as_str(),
                pred.score as i64,
                return_point,
                clean_note,
            ],
        )?;

        self.get_session(&session_id)
    }

    /// Stops the active work session (scoped by `project_name` if provided) and computes area metrics.
    pub fn stop_session(
        &self,
        project_name: Option<&str>,
        quality: Option<u8>,
        note: Option<&str>,
    ) -> Result<WorkSession, StoreError> {
        if let Some(q) = quality
            && !(1..=5).contains(&q)
        {
            return Err(StoreError::InvalidInput(format!(
                "recovery quality must be between 1 and 5 (got {q})"
            )));
        }

        let session_id: String = self
            .conn
            .query_row(
                "SELECT ws.id
                 FROM work_sessions ws
                 JOIN projects p ON p.id = ws.project_id
                 WHERE ws.ended_at IS NULL
                   AND (?1 IS NULL OR p.name = ?1)
                 ORDER BY ws.started_at DESC
                 LIMIT 1",
                params![project_name],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::NotFound("active session", project_name.unwrap_or("any").to_string())
            })?;

        let current = self.get_session(&session_id)?;
        let project = self
            .get_project_by_name(&current.project_name)?
            .ok_or_else(|| StoreError::NotFound("project", current.project_name.clone()))?;
        let root_prefix = project.root_path.as_deref().unwrap_or("");
        let now = now_unix().max(current.started_at);
        let duration_secs = now - current.started_at;

        // Gather paths touched during the session via events or filesystem modified_at.
        let mut changed_paths = HashSet::new();
        let mut ev_stmt = self.conn.prepare(
            "SELECT e.path
             FROM events e
             LEFT JOIN objects o ON o.id = e.object_id
             WHERE e.project_id = ?1
               AND COALESCE(o.is_noise, 0) = 0
               AND (
                 (e.event_type IN ('added', 'modified') AND COALESCE(o.modified_at, e.created_at) >= ?2)
                 OR (e.event_type NOT IN ('added', 'modified') AND e.created_at >= ?2)
               )",
        )?;
        let ev_rows = ev_stmt
            .query_map(params![current.project_id, current.started_at], |row| {
                row.get::<_, String>(0)
            })?;
        for row in ev_rows {
            changed_paths.insert(row?);
        }

        let mut obj_stmt = self.conn.prepare(
            "SELECT path FROM objects
             WHERE project_id = ?1
               AND is_deleted = 0
               AND is_noise = 0
               AND modified_at >= ?2",
        )?;
        let obj_rows = obj_stmt
            .query_map(params![current.project_id, current.started_at], |row| {
                row.get::<_, String>(0)
            })?;
        for row in obj_rows {
            changed_paths.insert(row?);
        }

        let mut area_counts: HashMap<String, usize> = HashMap::new();
        for path in &changed_paths {
            let area = object_area(path, root_prefix);
            *area_counts.entry(area).or_insert(0) += 1;
        }
        let mut ranked_areas: Vec<(String, usize)> = area_counts.into_iter().collect();
        ranked_areas.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let productive_areas: Vec<String> = ranked_areas
            .into_iter()
            .take(5)
            .map(|(area, _)| area)
            .collect();

        let clean_note = note
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        self.conn.execute(
            "UPDATE work_sessions
             SET ended_at = ?1,
                 duration_secs = ?2,
                 recovery_quality = COALESCE(?3, recovery_quality),
                 stop_note = COALESCE(?4, stop_note),
                 files_changed_count = ?5,
                 productive_areas_text = ?6
             WHERE id = ?7",
            params![
                now,
                duration_secs,
                quality.map(i64::from),
                clean_note,
                changed_paths.len() as i64,
                encode_list(&productive_areas)?,
                session_id,
            ],
        )?;

        self.get_session(&session_id)
    }

    /// Loads a single work session by ID.
    pub fn get_session(&self, session_id: &str) -> Result<WorkSession, StoreError> {
        self.conn
            .query_row(
                "SELECT ws.id, ws.project_id, p.name, ws.started_at, ws.ended_at,
                        ws.duration_secs, ws.predicted_difficulty, ws.predicted_score,
                        ws.recovery_quality, ws.return_point, ws.start_note,
                        ws.stop_note, ws.files_changed_count, ws.productive_areas_text
                 FROM work_sessions ws
                 JOIN projects p ON p.id = ws.project_id
                 WHERE ws.id = ?1",
                params![session_id],
                map_session_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound("session", session_id.to_string()))
    }

    /// Lists recorded work sessions (newest first).
    pub fn list_sessions(
        &self,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WorkSession>, StoreError> {
        let capped = limit.clamp(1, 100) as i64;
        let mut stmt = self.conn.prepare(
            "SELECT ws.id, ws.project_id, p.name, ws.started_at, ws.ended_at,
                    ws.duration_secs, ws.predicted_difficulty, ws.predicted_score,
                    ws.recovery_quality, ws.return_point, ws.start_note,
                    ws.stop_note, ws.files_changed_count, ws.productive_areas_text
             FROM work_sessions ws
             JOIN projects p ON p.id = ws.project_id
             WHERE (?1 IS NULL OR p.name = ?1)
             ORDER BY ws.started_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_name, capped], map_session_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Aggregates session statistics, return points, productive areas, and Predicted-vs-Actual metrics.
    pub fn session_stats(&self, project_name: Option<&str>) -> Result<SessionStats, StoreError> {
        let sessions = self.list_sessions(project_name, 100)?;
        let total_sessions = sessions.len();
        let active_sessions = sessions.iter().filter(|s| s.ended_at.is_none()).count();
        let completed_sessions = total_sessions - active_sessions;

        let mut durations: Vec<i64> = sessions.iter().filter_map(|s| s.duration_secs).collect();
        durations.sort_unstable();
        let median_duration_secs = if durations.is_empty() {
            None
        } else {
            Some(durations[durations.len() / 2])
        };
        let avg_duration_secs = if durations.is_empty() {
            None
        } else {
            Some(durations.iter().sum::<i64>() / durations.len() as i64)
        };

        let qualities: Vec<f64> = sessions
            .iter()
            .filter_map(|s| s.recovery_quality.map(f64::from))
            .collect();
        let avg_quality = if qualities.is_empty() {
            None
        } else {
            Some(qualities.iter().sum::<f64>() / qualities.len() as f64)
        };

        let mut return_counts: HashMap<String, usize> = HashMap::new();
        let mut area_counts: HashMap<String, usize> = HashMap::new();
        for s in &sessions {
            if let Some(ref rp) = s.return_point {
                *return_counts.entry(rp.clone()).or_insert(0) += 1;
            }
            for area in &s.productive_areas {
                *area_counts.entry(area.clone()).or_insert(0) += 1;
            }
        }

        let mut frequent_return_points: Vec<(String, usize)> = return_counts.into_iter().collect();
        frequent_return_points.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        frequent_return_points.truncate(5);

        let mut most_productive_areas: Vec<(String, usize)> = area_counts.into_iter().collect();
        most_productive_areas.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        most_productive_areas.truncate(5);

        let mut by_predicted_difficulty = Vec::new();
        for tier in [
            RecoveryDifficulty::Easy,
            RecoveryDifficulty::Medium,
            RecoveryDifficulty::Hard,
        ] {
            let bucket: Vec<&WorkSession> = sessions
                .iter()
                .filter(|s| s.predicted_difficulty == tier)
                .collect();
            if bucket.is_empty() {
                continue;
            }
            let durs: Vec<i64> = bucket.iter().filter_map(|s| s.duration_secs).collect();
            let quals: Vec<f64> = bucket
                .iter()
                .filter_map(|s| s.recovery_quality.map(f64::from))
                .collect();
            by_predicted_difficulty.push(DifficultyStats {
                difficulty: tier,
                sessions: bucket.len(),
                avg_duration_secs: (!durs.is_empty())
                    .then(|| durs.iter().sum::<i64>() / durs.len() as i64),
                avg_quality: (!quals.is_empty())
                    .then(|| quals.iter().sum::<f64>() / quals.len() as f64),
            });
        }

        let mut false_alarms = 0;
        let mut hard_scored_sessions = 0;
        let mut missed_difficulty = 0;
        let mut easy_scored_sessions = 0;
        for s in sessions.iter().filter(|s| s.ended_at.is_some()) {
            match s.predicted_difficulty {
                RecoveryDifficulty::Hard
                    if s.duration_secs.is_some() && s.recovery_quality.is_some() =>
                {
                    hard_scored_sessions += 1;
                    false_alarms += usize::from(is_false_alarm(s));
                }
                RecoveryDifficulty::Easy
                    if s.duration_secs.is_some() || s.recovery_quality.is_some() =>
                {
                    easy_scored_sessions += 1;
                    missed_difficulty += usize::from(is_missed_difficulty(s));
                }
                _ => {}
            }
        }

        Ok(SessionStats {
            project_name: project_name.map(str::to_string),
            total_sessions,
            active_sessions,
            completed_sessions,
            median_duration_secs,
            avg_duration_secs,
            avg_quality,
            frequent_return_points,
            most_productive_areas,
            by_predicted_difficulty,
            false_alarms,
            hard_scored_sessions,
            missed_difficulty,
            easy_scored_sessions,
        })
    }
}

/// Thresholds mirror `scripts/analyze_recovery.ps1`: Hard -> quality >= 4 within 5 minutes.
fn is_false_alarm(s: &WorkSession) -> bool {
    s.recovery_quality.is_some_and(|q| q >= 4) && s.duration_secs.is_some_and(|d| d <= 5 * 60)
}

/// Thresholds mirror `scripts/analyze_recovery.ps1`: Easy -> quality <= 2 or >= 15 minutes.
fn is_missed_difficulty(s: &WorkSession) -> bool {
    s.recovery_quality.is_some_and(|q| q <= 2) || s.duration_secs.is_some_and(|d| d >= 15 * 60)
}

fn map_snapshot_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectSnapshot> {
    let health_str: String = row.get(4)?;
    let doc_health_str: String = row.get(9)?;
    let oq_text: String = row.get(12)?;
    let pw_text: String = row.get(13)?;
    let aa_text: String = row.get(14)?;
    let ra_text: String = row.get(15)?;
    let rc_text: String = row.get(16)?;

    Ok(ProjectSnapshot {
        id: row.get(0)?,
        project_id: row.get(1)?,
        project_name: row.get(2)?,
        label: row.get(3)?,
        health: ProjectHealth::from_db(&health_str),
        active_objects: row.get::<_, i64>(5)?.max(0) as usize,
        deleted_objects: row.get::<_, i64>(6)?.max(0) as usize,
        relations_count: row.get::<_, i64>(7)?.max(0) as usize,
        coverage_ratio: row.get(8)?,
        documentation_health: DocumentationHealth::from_db(&doc_health_str),
        critical_risks: row.get::<_, i64>(10)?.max(0) as usize,
        high_risks: row.get::<_, i64>(11)?.max(0) as usize,
        open_questions: decode_list(&oq_text),
        pending_work: decode_list(&pw_text),
        active_areas: decode_list(&aa_text),
        risk_areas: decode_list(&ra_text),
        recent_changes: decode_list(&rc_text),
        created_at: row.get(17)?,
    })
}

fn map_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkSession> {
    let diff_str: String = row.get(6)?;
    let qual_opt: Option<i64> = row.get(8)?;
    let areas_text: String = row.get(13)?;

    Ok(WorkSession {
        id: row.get(0)?,
        project_id: row.get(1)?,
        project_name: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        duration_secs: row.get(5)?,
        predicted_difficulty: RecoveryDifficulty::from_db(&diff_str),
        predicted_score: row.get::<_, i64>(7)?.max(0) as usize,
        recovery_quality: qual_opt.map(|v| v.clamp(1, 5) as u8),
        return_point: row.get(9)?,
        start_note: row.get(10)?,
        stop_note: row.get(11)?,
        files_changed_count: row.get::<_, i64>(12)?.max(0) as usize,
        productive_areas: decode_list(&areas_text),
    })
}

fn encode_list(items: &[String]) -> Result<String, StoreError> {
    if items.is_empty() {
        return Ok(String::new());
    }
    serde_yaml::to_string(items).map_err(|e| StoreError::InvalidInput(format!("encode list: {e}")))
}

fn decode_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    serde_yaml::from_str(trimmed).unwrap_or_default()
}

fn compute_snapshot_diff(from: ProjectSnapshot, to: ProjectSnapshot) -> SnapshotDiff {
    SnapshotDiff {
        active_delta: to.active_objects as isize - from.active_objects as isize,
        deleted_delta: to.deleted_objects as isize - from.deleted_objects as isize,
        relations_delta: to.relations_count as isize - from.relations_count as isize,
        coverage_delta: to.coverage_ratio - from.coverage_ratio,
        critical_risks_delta: to.critical_risks as isize - from.critical_risks as isize,
        high_risks_delta: to.high_risks as isize - from.high_risks as isize,
        added_pending_work: list_diff(&to.pending_work, &from.pending_work),
        resolved_pending_work: list_diff(&from.pending_work, &to.pending_work),
        added_risks: list_diff(&to.risk_areas, &from.risk_areas),
        resolved_risks: list_diff(&from.risk_areas, &to.risk_areas),
        new_active_areas: list_diff(&to.active_areas, &from.active_areas),
        from,
        to,
    }
}

fn list_diff(target: &[String], baseline: &[String]) -> Vec<String> {
    let base_set: HashSet<&str> = baseline.iter().map(String::as_str).collect();
    target
        .iter()
        .filter(|item| !base_set.contains(item.as_str()))
        .cloned()
        .collect()
}

fn relative_display(path: &str, root_prefix: &str) -> String {
    let trimmed = root_prefix.trim_end_matches(['/', '\\']);
    if !trimmed.is_empty() {
        path.strip_prefix(trimmed)
            .map(|r| r.trim_start_matches(['/', '\\']).to_string())
            .unwrap_or_else(|| path.to_string())
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use vos_core::ObjectKind;
    use vos_core::util::now_unix;

    use super::*;
    use crate::UpsertObjectInput;

    #[test]
    fn snapshot_save_compare_and_work_session_lifecycle_work() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("demo", None, None).unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                summary: Some("Demo".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 20,
                modified_at: now - 10,
                is_noise: false,
                chunks: vec!["- [ ] Build scheduler\nOPEN QUESTION: keep WAL?".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let s1 = store.save_snapshot("demo", Some("baseline")).unwrap();
        assert_eq!(s1.active_objects, 1);
        assert_eq!(s1.pending_work.len(), 1);

        let sess = store.start_session("demo", Some("initial resume")).unwrap();
        assert!(sess.ended_at.is_none());
        assert!(store.start_session("demo", None).is_err());

        // Simulate coding activity during session
        let mut batch2 = store.begin_index_batch().unwrap();
        batch2
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "src/scheduler.rs".to_string(),
                kind: ObjectKind::Code,
                title: "scheduler.rs".to_string(),
                summary: Some("Scheduler".to_string()),
                content_hash: "h2".to_string(),
                size_bytes: 40,
                modified_at: now_unix(),
                is_noise: false,
                chunks: vec!["pub fn tick() {}".to_string()],
            })
            .unwrap();
        batch2.commit().unwrap();

        let stopped = store
            .stop_session(Some("demo"), Some(5), Some("implemented tick"))
            .unwrap();
        assert!(stopped.ended_at.is_some());
        assert_eq!(stopped.recovery_quality, Some(5));
        assert!(stopped.files_changed_count >= 1);
        assert!(stopped.productive_areas.contains(&"src".to_string()));

        let s2 = store.save_snapshot("demo", Some("after tick")).unwrap();
        let diff = store.compare_snapshots(s1.id, s2.id).unwrap();
        assert_eq!(diff.active_delta, 1);

        let stats = store.session_stats(Some("demo")).unwrap();
        assert_eq!(stats.completed_sessions, 1);
        assert_eq!(stats.avg_quality, Some(5.0));
    }

    #[test]
    fn aggregates_24_sessions_across_all_difficulty_tiers_and_return_points() {
        let mut store = Store::open_in_memory().unwrap();
        let now = now_unix();

        // Create 3 representative projects spanning Easy, Medium, and Hard difficulty tiers.
        let p_easy = store.create_project("tiny", None, None).unwrap();
        let p_med = store.create_project("mid", None, None).unwrap();
        let p_hard = store.create_project("complex", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        // tiny -> active, documented -> Easy
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(p_easy.id.clone()),
                path: "README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                summary: Some("Tiny".to_string()),
                content_hash: "te".to_string(),
                size_bytes: 10,
                modified_at: now - 60,
                is_noise: false,
                chunks: vec!["# Tiny".to_string()],
            })
            .unwrap();

        // mid -> warm (20d) + 25 code files + Low coverage -> Medium (2 + 2 + 1 = 5)
        for i in 0..25 {
            batch
                .upsert_object(UpsertObjectInput {
                    project_id: Some(p_med.id.clone()),
                    path: format!("src/m_{i}.rs"),
                    kind: ObjectKind::Code,
                    title: format!("m_{i}.rs"),
                    summary: Some("Mid".to_string()),
                    content_hash: format!("tm_{i}"),
                    size_bytes: 10,
                    modified_at: now - 20 * 86_400,
                    is_noise: false,
                    chunks: vec!["pub fn mid() {}".to_string()],
                })
                .unwrap();
        }

        // complex -> dormant (90d) + open questions -> Hard
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(p_hard.id.clone()),
                path: "docs/SPEC.md".to_string(),
                kind: ObjectKind::Note,
                title: "SPEC.md".to_string(),
                summary: Some("Spec".to_string()),
                content_hash: "th1".to_string(),
                size_bytes: 10,
                modified_at: now - 90 * 86_400,
                is_noise: false,
                chunks: vec![
                    "OPEN QUESTION: Q1?\nOPEN QUESTION: Q2?\nOPEN QUESTION: Q3?\nOPEN QUESTION: Q4?"
                        .to_string(),
                ],
            })
            .unwrap();
        for i in 0..25 {
            batch
                .upsert_object(UpsertObjectInput {
                    project_id: Some(p_hard.id.clone()),
                    path: format!("src/mod_{i}.rs"),
                    kind: ObjectKind::Code,
                    title: format!("mod_{i}.rs"),
                    summary: Some("Code".to_string()),
                    content_hash: format!("thc_{i}"),
                    size_bytes: 20,
                    modified_at: now - 90 * 86_400,
                    is_noise: false,
                    chunks: vec!["pub fn x() {}".to_string()],
                })
                .unwrap();
        }
        batch.commit().unwrap();

        // Record 24 sessions (8 per tier) cleanly.
        for _ in 0..8 {
            let s1 = store.start_session("tiny", Some("easy check")).unwrap();
            assert_eq!(s1.predicted_difficulty, RecoveryDifficulty::Easy);
            store
                .stop_session(Some("tiny"), Some(5), Some("fast"))
                .unwrap();

            let s2 = store.start_session("mid", Some("med check")).unwrap();
            assert_eq!(s2.predicted_difficulty, RecoveryDifficulty::Medium);
            store
                .stop_session(Some("mid"), Some(4), Some("ok"))
                .unwrap();

            let s3 = store.start_session("complex", Some("hard check")).unwrap();
            assert_eq!(s3.predicted_difficulty, RecoveryDifficulty::Hard);
            store
                .stop_session(Some("complex"), Some(2), Some("heavy"))
                .unwrap();
        }

        let stats = store.session_stats(None).unwrap();
        assert_eq!(stats.total_sessions, 24);
        assert_eq!(stats.completed_sessions, 24);
        assert_eq!(stats.active_sessions, 0);
        assert_eq!(stats.by_predicted_difficulty.len(), 3);
        assert_eq!(
            stats.by_predicted_difficulty[0].difficulty,
            RecoveryDifficulty::Easy
        );
        assert_eq!(stats.by_predicted_difficulty[0].sessions, 8);
        assert_eq!(stats.by_predicted_difficulty[0].avg_quality, Some(5.0));
        assert_eq!(
            stats.by_predicted_difficulty[1].difficulty,
            RecoveryDifficulty::Medium
        );
        assert_eq!(stats.by_predicted_difficulty[1].avg_quality, Some(4.0));
        assert_eq!(
            stats.by_predicted_difficulty[2].difficulty,
            RecoveryDifficulty::Hard
        );
        assert_eq!(stats.by_predicted_difficulty[2].avg_quality, Some(2.0));
        assert!(!stats.frequent_return_points.is_empty());
    }

    #[test]
    fn compare_snapshot_to_live_tracks_deltas_without_saving_second_snapshot() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("livecmp", None, None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/README.md".to_string(),
                kind: ObjectKind::Note,
                title: "README.md".to_string(),
                summary: Some("Initial".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 50,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["# LiveCmp".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let baseline = store.save_snapshot("livecmp", Some("base")).unwrap();
        assert_eq!(baseline.active_objects, 1);

        let mut batch2 = store.begin_index_batch().unwrap();
        batch2
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/ROADMAP.md".to_string(),
                kind: ObjectKind::Note,
                title: "ROADMAP.md".to_string(),
                summary: Some("roadmap".to_string()),
                content_hash: "h2".to_string(),
                size_bytes: 80,
                modified_at: 200,
                is_noise: false,
                chunks: vec!["- [ ] Add retries".to_string()],
            })
            .unwrap();
        batch2.commit().unwrap();

        let diff = store.compare_snapshot_to_live("livecmp", None).unwrap();
        assert_eq!(diff.from.id, baseline.id);
        assert_eq!(diff.to.id, 0);
        assert_eq!(diff.to.label.as_deref(), Some("live"));
        assert_eq!(diff.active_delta, 1);
        assert_eq!(diff.added_pending_work.len(), 1);
        assert_eq!(store.list_snapshots(Some("livecmp"), 10).unwrap().len(), 1);
    }

    #[test]
    fn save_all_snapshots_captures_every_registered_project() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.save_all_snapshots(Some("empty")).unwrap().is_empty());

        store.create_project("zebra", None, None).unwrap();
        store.create_project("alpha", None, None).unwrap();

        let snaps = store.save_all_snapshots(Some("portfolio-check")).unwrap();
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].project_name, "alpha");
        assert_eq!(snaps[1].project_name, "zebra");
        assert_eq!(snaps[0].label.as_deref(), Some("portfolio-check"));
        assert_eq!(snaps[1].label.as_deref(), Some("portfolio-check"));
        assert_eq!(store.list_snapshots(None, 10).unwrap().len(), 2);
    }

    #[test]
    fn classifies_false_alarm_and_missed_difficulty() {
        let mk = |diff, dur, qual| WorkSession {
            id: "s".into(),
            project_id: "p".into(),
            project_name: "p".into(),
            started_at: 0,
            ended_at: Some(dur),
            duration_secs: Some(dur),
            predicted_difficulty: diff,
            predicted_score: 0,
            recovery_quality: Some(qual),
            return_point: None,
            start_note: None,
            stop_note: None,
            files_changed_count: 0,
            productive_areas: vec![],
        };
        assert!(is_false_alarm(&mk(RecoveryDifficulty::Hard, 300, 4)));
        assert!(!is_false_alarm(&mk(RecoveryDifficulty::Hard, 301, 4)));
        assert!(!is_false_alarm(&mk(RecoveryDifficulty::Hard, 60, 3)));
        assert!(is_missed_difficulty(&mk(RecoveryDifficulty::Easy, 900, 5)));
        assert!(is_missed_difficulty(&mk(RecoveryDifficulty::Easy, 60, 2)));
        assert!(!is_missed_difficulty(&mk(RecoveryDifficulty::Easy, 899, 3)));
    }
}
