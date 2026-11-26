//! Multi-agent cognitive bus: hash-anchored facts, area leases, handoffs, and cross-project recall.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use rusqlite::{OptionalExtension, params};
use vos_core::compact::{compact_path, truncate_text};
use vos_core::util::{new_id, now_unix};
use vos_core::{AgentHandoff, AreaLease, FactStatus, ProjectFact, RecallHit};

use crate::{SearchFilters, Store, StoreError};

/// Input payload for `Store::record_handoff`.
#[derive(Debug, Clone)]
pub struct HandoffInput<'a> {
    pub project_name: &'a str,
    pub from_agent: &'a str,
    pub to_agent: Option<&'a str>,
    pub summary: &'a str,
    pub next_step: Option<&'a str>,
    pub files: &'a [String],
    pub release_leases: bool,
}

impl Store {
    /// Stores a hash-anchored verified fact (`vos fact add`).
    pub fn add_fact(
        &mut self,
        project_name: Option<&str>,
        fact_text: &str,
        target_file: Option<&str>,
        symbol: Option<&str>,
        author: Option<&str>,
    ) -> Result<ProjectFact, StoreError> {
        let trimmed = fact_text.trim();
        if trimmed.is_empty() {
            return Err(StoreError::InvalidInput("fact text cannot be empty".into()));
        }

        let (mut project_id, mut resolved_proj_name, mut proj_root) = match project_name {
            Some(name) => {
                let proj = self
                    .get_project_by_name(name)?
                    .ok_or_else(|| StoreError::NotFound("project", name.to_string()))?;
                (Some(proj.id), Some(proj.name), proj.root_path)
            }
            None => (None, None, None),
        };

        let (object_id, rel_path, anchor_hash) = match target_file.map(str::trim).filter(|s| !s.is_empty()) {
            Some(target) => {
                let details = self.show_object_scoped(target, resolved_proj_name.as_deref())?;
                if project_id.is_none() {
                    project_id = details.object.project_id.clone();
                    resolved_proj_name = details.project_name.clone();
                    if let Some(ref pid) = project_id {
                        proj_root = self
                            .conn
                            .query_row("SELECT root_path FROM projects WHERE id = ?1", params![pid], |row| row.get(0))
                            .optional()?
                            .flatten();
                    }
                }
                let rel = compact_path(&details.object.path, proj_root.as_deref());
                (Some(details.object.id), Some(rel), Some(details.object.content_hash))
            }
            None => (None, None, None),
        };

        let now = now_unix();
        let id = new_id("fact");
        let clean_sym = symbol.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let clean_author = author.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);

        self.conn.execute(
            "INSERT INTO facts (id, project_id, object_id, path, symbol, fact_text, anchor_hash, author, status, created_at, updated_at, verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', ?9, ?9, ?9)",
            params![id, project_id, object_id, rel_path, clean_sym, trimmed, anchor_hash, clean_author, now],
        )?;

        Ok(ProjectFact {
            id,
            project_name: resolved_proj_name,
            path: rel_path,
            symbol: clean_sym,
            fact_text: trimmed.to_string(),
            anchor_hash,
            author: clean_author,
            status: FactStatus::Active,
            created_at: now,
            updated_at: now,
            verified_at: now,
        })
    }

    /// Lists project facts (active + stale by default; optionally includes archived).
    pub fn list_facts(
        &self,
        project_name: Option<&str>,
        include_archived: bool,
        limit: usize,
    ) -> Result<Vec<ProjectFact>, StoreError> {
        let capped = limit.clamp(1, 200) as i64;
        let include_arch_int = i64::from(include_archived);
        let mut stmt = self.conn.prepare(
            "SELECT f.id, p.name, f.path, f.symbol, f.fact_text, f.anchor_hash, f.author, f.status,
                    f.created_at, f.updated_at, f.verified_at
             FROM facts f
             LEFT JOIN projects p ON p.id = f.project_id
             WHERE (?1 IS NULL OR p.name = ?1)
               AND (?2 = 1 OR f.status != 'archived')
             ORDER BY CASE f.status WHEN 'stale' THEN 0 WHEN 'active' THEN 1 ELSE 2 END ASC,
                      f.updated_at DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![project_name, include_arch_int, capped], |row| {
            let st: String = row.get(7)?;
            Ok(ProjectFact {
                id: row.get(0)?,
                project_name: row.get(1)?,
                path: row.get(2)?,
                symbol: row.get(3)?,
                fact_text: row.get(4)?,
                anchor_hash: row.get(5)?,
                author: row.get(6)?,
                status: FactStatus::from_db(&st),
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                verified_at: row.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Re-verifies a fact, refreshing its anchored content hash from the bound object if present.
    pub fn verify_fact(&mut self, fact_id: &str) -> Result<ProjectFact, StoreError> {
        let existing: Option<(Option<String>, Option<String>, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT f.object_id, o.content_hash, o.is_deleted
                 FROM facts f
                 LEFT JOIN objects o ON o.id = f.object_id
                 WHERE f.id = ?1",
                params![fact_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        let Some((object_id, current_hash, del_flag)) = existing else {
            return Err(StoreError::NotFound("fact", fact_id.to_string()));
        };

        if object_id.is_some() && del_flag == Some(1) {
            return Err(StoreError::InvalidInput(format!(
                "anchored object for fact '{fact_id}' is deleted; archive instead"
            )));
        }

        let now = now_unix();
        self.conn.execute(
            "UPDATE facts
             SET status = 'active',
                 anchor_hash = COALESCE(?1, anchor_hash),
                 updated_at = ?2,
                 verified_at = ?2
             WHERE id = ?3",
            params![current_hash, now, fact_id],
        )?;

        self.list_facts(None, true, 200)?
            .into_iter()
            .find(|f| f.id == fact_id)
            .ok_or_else(|| StoreError::NotFound("fact", fact_id.to_string()))
    }

    /// Archives a fact without deleting history.
    pub fn archive_fact(&mut self, fact_id: &str) -> Result<(), StoreError> {
        let now = now_unix();
        let changed = self.conn.execute(
            "UPDATE facts SET status = 'archived', updated_at = ?1 WHERE id = ?2",
            params![now, fact_id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound("fact", fact_id.to_string()));
        }
        Ok(())
    }

    /// Claims or renews a workspace area lease for `agent_id` (`vos claim`).
    pub fn claim_area(
        &mut self,
        project_name: &str,
        area: &str,
        agent_id: &str,
        ttl_mins: u64,
        note: Option<&str>,
    ) -> Result<AreaLease, StoreError> {
        let proj = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;
        let clean_area = normalize_area(area)?;
        let clean_agent = agent_id.trim();
        if clean_agent.is_empty() {
            return Err(StoreError::InvalidInput("agent id cannot be empty".into()));
        }

        let now = now_unix();
        let expires_at = now + (ttl_mins.clamp(1, 24 * 60) as i64) * 60;
        self.conn.execute("DELETE FROM area_leases WHERE expires_at <= ?1", params![now])?;

        let conflict: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT agent_id, expires_at FROM area_leases WHERE project_id = ?1 AND area = ?2",
                params![proj.id, clean_area],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        if let Some((holder, exp)) = conflict
            && holder != clean_agent
        {
            return Err(StoreError::InvalidInput(format!(
                "area '{clean_area}' in '{project_name}' is already claimed by '{holder}' until unix {exp}"
            )));
        }

        let id = new_id("lease");
        let clean_note = note.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        self.conn.execute(
            "INSERT INTO area_leases (id, project_id, area, agent_id, note, claimed_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_id, area) DO UPDATE SET
                 agent_id = excluded.agent_id,
                 note = COALESCE(excluded.note, area_leases.note),
                 claimed_at = excluded.claimed_at,
                 expires_at = excluded.expires_at",
            params![id, proj.id, clean_area, clean_agent, clean_note, now, expires_at],
        )?;

        Ok(AreaLease {
            id,
            project_name: proj.name,
            area: clean_area,
            agent_id: clean_agent.to_string(),
            note: clean_note,
            claimed_at: now,
            expires_at,
        })
    }

    /// Releases an active area lease in `project_name` (`vos release`).
    pub fn release_area(
        &mut self,
        project_name: &str,
        area: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<usize, StoreError> {
        let proj = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;
        let now = now_unix();
        self.conn.execute("DELETE FROM area_leases WHERE expires_at <= ?1", params![now])?;

        let norm_area = area.map(normalize_area).transpose()?;
        let changed = self.conn.execute(
            "DELETE FROM area_leases
             WHERE project_id = ?1
               AND (?2 IS NULL OR area = ?2)
               AND (?3 IS NULL OR agent_id = ?3)",
            params![proj.id, norm_area, agent_id.map(str::trim)],
        )?;
        Ok(changed)
    }

    /// Lists unexpired area leases.
    pub fn list_leases(&self, project_name: Option<&str>) -> Result<Vec<AreaLease>, StoreError> {
        let now = now_unix();
        let mut stmt = self.conn.prepare(
            "SELECT l.id, p.name, l.area, l.agent_id, l.note, l.claimed_at, l.expires_at
             FROM area_leases l
             JOIN projects p ON p.id = l.project_id
             WHERE l.expires_at > ?1 AND (?2 IS NULL OR p.name = ?2)
             ORDER BY p.name ASC, l.area ASC",
        )?;
        let rows = stmt.query_map(params![now, project_name], |row| {
            Ok(AreaLease {
                id: row.get(0)?,
                project_name: row.get(1)?,
                area: row.get(2)?,
                agent_id: row.get(3)?,
                note: row.get(4)?,
                claimed_at: row.get(5)?,
                expires_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Records a structured agent-to-agent / session handoff (`vos handoff`).
    pub fn record_handoff(&mut self, input: HandoffInput<'_>) -> Result<AgentHandoff, StoreError> {
        let proj = self
            .get_project_by_name(input.project_name)?
            .ok_or_else(|| StoreError::NotFound("project", input.project_name.to_string()))?;
        let clean_from = input.from_agent.trim();
        let clean_summary = input.summary.trim();
        if clean_from.is_empty() || clean_summary.is_empty() {
            return Err(StoreError::InvalidInput("'from_agent' and 'summary' cannot be empty".into()));
        }

        let now = now_unix();
        let id = new_id("hoff");
        let clean_to = input.to_agent.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let clean_next = input.next_step.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let clean_files: Vec<String> = input.files.iter().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
        let files_json = if clean_files.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&clean_files).map_err(|e| StoreError::InvalidInput(format!("invalid files list: {e}")))?)
        };

        self.conn.execute(
            "INSERT INTO agent_handoffs (id, project_id, from_agent, to_agent, summary, next_step, files_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, proj.id, clean_from, clean_to, clean_summary, clean_next, files_json, now],
        )?;

        if input.release_leases {
            let _ = self.release_area(input.project_name, None, Some(clean_from))?;
        }

        Ok(AgentHandoff {
            id,
            project_name: proj.name,
            from_agent: clean_from.to_string(),
            to_agent: clean_to,
            summary: clean_summary.to_string(),
            next_step: clean_next,
            files: clean_files,
            created_at: now,
        })
    }

    pub(crate) fn cognitive_summary_for_project(
        &self,
        project_id: &str,
        project_name: &str,
    ) -> Result<(usize, Vec<AreaLease>, Option<AgentHandoff>), StoreError> {
        let stale_facts_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM facts WHERE project_id = ?1 AND status = 'stale'",
            params![project_id],
            |row| row.get(0),
        )?;
        let active_leases = self.list_leases(Some(project_name))?;
        let latest_handoff = self
            .conn
            .query_row(
                "SELECT id, from_agent, to_agent, summary, next_step, files_json, created_at
                 FROM agent_handoffs WHERE project_id = ?1 ORDER BY created_at DESC LIMIT 1",
                params![project_id],
                |row| {
                    let fjson: Option<String> = row.get(5)?;
                    let files = fjson.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
                    Ok(AgentHandoff {
                        id: row.get(0)?,
                        project_name: project_name.to_string(),
                        from_agent: row.get(1)?,
                        to_agent: row.get(2)?,
                        summary: row.get(3)?,
                        next_step: row.get(4)?,
                        files,
                        created_at: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok((stale_facts_count.max(0) as usize, active_leases, latest_handoff))
    }

    /// Searches across all projects' facts, handoffs, session notes, Git commits, and objects (`vos recall`).
    pub fn recall_solutions(&self, query: &str, limit: usize) -> Result<Vec<RecallHit>, StoreError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(StoreError::InvalidInput("recall query cannot be empty".into()));
        }
        let capped = limit.clamp(1, 40);
        let mut results = Vec::new();
        let mut seen = HashSet::new();
        let tokens: Vec<String> = trimmed.split_whitespace().filter(|t| !t.is_empty()).map(|t| t.to_ascii_lowercase()).collect();
        let fts_query = tokens.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect::<Vec<_>>().join(" OR ");

        {
            let mut stmt = self.conn.prepare(
                "SELECT f.id, p.name, f.path, f.fact_text, f.status, f.updated_at
                 FROM facts_fts JOIN facts f ON f.rowid = facts_fts.rowid LEFT JOIN projects p ON p.id = f.project_id
                 WHERE facts_fts MATCH ?1 AND f.status != 'archived'
                 ORDER BY CASE f.status WHEN 'active' THEN 0 ELSE 1 END ASC, bm25(facts_fts) ASC LIMIT ?2",
            )?;
            for row in stmt.query_map(params![fts_query, (capped / 2 + 4) as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, i64>(5)?))
            })? {
                let (id, proj, path, text, status, ts) = row?;
                if seen.insert(format!("fact:{id}")) {
                    results.push(RecallHit {
                        source: "fact".into(), project_name: proj, ref_id: id,
                        title: path.unwrap_or_else(|| "project fact".into()), excerpt: truncate_text(&text, 180),
                        timestamp: ts, is_stale: Some(status == "stale"),
                    });
                }
            }
        }

        {
            let mut stmt = self.conn.prepare(
                "SELECT ws.id, p.name, COALESCE(ws.return_point, 'session'), COALESCE(ws.stop_note, ws.start_note, ''), COALESCE(ws.ended_at, ws.started_at)
                 FROM work_sessions ws JOIN projects p ON p.id = ws.project_id
                 WHERE ws.stop_note IS NOT NULL OR ws.start_note IS NOT NULL
                 ORDER BY COALESCE(ws.ended_at, ws.started_at) DESC LIMIT 80",
            )?;
            for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?)))? {
                let (id, proj, title, note, ts) = row?;
                let lower = format!("{title} {note}").to_ascii_lowercase();
                if tokens.iter().any(|t| lower.contains(t)) && seen.insert(format!("session:{id}")) {
                    results.push(RecallHit { source: "session".into(), project_name: Some(proj), ref_id: id, title, excerpt: truncate_text(&note, 180), timestamp: ts, is_stale: None });
                }
            }
        }

        {
            let mut stmt = self.conn.prepare(
                "SELECT h.id, p.name, h.from_agent, h.summary || COALESCE(' | next: ' || h.next_step, ''), h.created_at
                 FROM agent_handoffs h JOIN projects p ON p.id = h.project_id ORDER BY h.created_at DESC LIMIT 40",
            )?;
            for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?)))? {
                let (id, proj, from_agent, body, ts) = row?;
                if tokens.iter().any(|t| body.to_ascii_lowercase().contains(t)) && seen.insert(format!("handoff:{id}")) {
                    results.push(RecallHit { source: "handoff".into(), project_name: Some(proj), ref_id: id, title: format!("handoff from {from_agent}"), excerpt: truncate_text(&body, 180), timestamp: ts, is_stale: None });
                }
            }
        }

        if let Some(first_term) = tokens.first() {
            for proj in self.list_projects()? {
                let Some(root) = proj.project.root_path.as_deref() else { continue };
                let root_path = Path::new(root);
                if !root_path.join(".git").exists() { continue }
                if let Ok(out) = Command::new("git").arg("-C").arg(root_path).args(["log", "-n", "5", "--regexp-ignore-case", &format!("--grep={first_term}"), "--format=%h%x09%ct%x09%s"]).output()
                    && out.status.success()
                {
                    for line in String::from_utf8_lossy(&out.stdout).lines() {
                        let mut parts = line.splitn(3, '\t');
                        let Some(hash) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else { continue };
                        let ts = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
                        let subj = parts.next().unwrap_or("").trim();
                        if seen.insert(format!("git:{}:{hash}", proj.project.name)) {
                            results.push(RecallHit { source: "git_commit".into(), project_name: Some(proj.project.name.clone()), ref_id: hash.into(), title: subj.into(), excerpt: subj.into(), timestamp: ts, is_stale: None });
                        }
                    }
                }
            }
        }

        for hit in self.search_with_filters(Some(trimmed), SearchFilters { limit: capped, ..Default::default() })? {
            if seen.insert(format!("obj:{}", hit.object_id)) {
                results.push(RecallHit { source: "object".into(), project_name: hit.project_name, ref_id: hit.path, title: hit.title, excerpt: truncate_text(&hit.snippet, 160), timestamp: 0, is_stale: None });
            }
        }

        results.truncate(capped);
        Ok(results)
    }
}

fn normalize_area(raw: &str) -> Result<String, StoreError> {
    let clean = raw.trim().replace('\\', "/").trim_matches('/').to_string();
    if clean.is_empty() {
        return Err(StoreError::InvalidInput("area cannot be empty".into()));
    }
    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UpsertObjectInput;
    use vos_core::ObjectKind;

    #[test]
    fn hash_anchored_facts_become_stale_on_file_edit_and_verify_restores() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("mem", None, None).unwrap();
        let mut b1 = store.begin_index_batch().unwrap();
        b1.upsert_object(UpsertObjectInput {
            project_id: Some(proj.id.clone()), path: "src/search.rs".into(), kind: ObjectKind::Code,
            title: "search.rs".into(), summary: Some("ranking".into()), content_hash: "h1".into(),
            size_bytes: 50, modified_at: 100, is_noise: false, chunks: vec!["fn rank() {}".into()],
        }).unwrap();
        b1.commit().unwrap();

        let fact = store.add_fact(Some("mem"), "penalty is -40", Some("src/search.rs"), Some("rank"), Some("me")).unwrap();
        assert_eq!(fact.status, FactStatus::Active);

        let mut b2 = store.begin_index_batch().unwrap();
        b2.upsert_object(UpsertObjectInput {
            project_id: Some(proj.id), path: "src/search.rs".into(), kind: ObjectKind::Code,
            title: "search.rs".into(), summary: Some("ranking v2".into()), content_hash: "h2".into(),
            size_bytes: 60, modified_at: 200, is_noise: false, chunks: vec!["fn rank() {}".into()],
        }).unwrap();
        b2.commit().unwrap();

        assert_eq!(store.list_facts(Some("mem"), false, 10).unwrap()[0].status, FactStatus::Stale);
        let verified = store.verify_fact(&fact.id).unwrap();
        assert_eq!(verified.status, FactStatus::Active);
        assert_eq!(verified.anchor_hash.as_deref(), Some("h2"));
    }

    #[test]
    fn leases_prevent_conflicts_and_handoff_releases_and_recall_finds() {
        let mut store = Store::open_in_memory().unwrap();
        store.create_project("hey", None, None).unwrap();
        store.claim_area("hey", "crates/hey-style", "claude-1", 30, Some("chamfer")).unwrap();
        assert!(store.claim_area("hey", "crates/hey-style", "anti-2", 30, None).is_err());
        store.record_handoff(HandoffInput { project_name: "hey", from_agent: "claude-1", to_agent: Some("anti-2"), summary: "Fixed chamfer tokens", next_step: Some("Run tests"), files: &[], release_leases: true }).unwrap();
        assert!(store.list_leases(Some("hey")).unwrap().is_empty());
        let recall = store.recall_solutions("chamfer", 10).unwrap();
        assert!(recall.iter().any(|r| r.source == "handoff"));
    }
}