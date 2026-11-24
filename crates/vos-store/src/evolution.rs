use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::params;
use vos_core::{CoverageTier, DriftFinding, ObjectKind, ObjectRecord, ProjectDrift, ProjectEvolution, RiskLevel};

use crate::queries::map_object_row;
use crate::recovery::object_area;
use crate::{Store, StoreError};

const DOC_DRIFT_THRESHOLD_SECS: i64 = 14 * 86_400;

impl Store {
    /// Summarizes how a project has evolved since `since_ts` (changes, area shifts, emerging concepts, doc coverage).
    pub fn project_evolution(
        &self,
        project_name: &str,
        since_ts: i64,
        since_label: &str,
    ) -> Result<ProjectEvolution, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let diff = self.changed_since(since_ts, since_label, Some(project_name))?;
        let root_prefix = project.root_path.as_deref().unwrap_or("");

        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
             FROM objects
             WHERE project_id = ?1 AND is_deleted = 0 AND is_noise = 0",
        )?;
        let objects = stmt
            .query_map(params![project.id], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let mut recent_areas: HashMap<String, usize> = HashMap::new();
        let mut older_areas: HashMap<String, usize> = HashMap::new();
        let mut recent_tokens: HashMap<String, usize> = HashMap::new();
        let mut older_tokens: HashMap<String, usize> = HashMap::new();
        let mut recent_code = 0_usize;
        let mut recent_docs = 0_usize;

        for obj in &objects {
            let area = object_area(&obj.path, root_prefix);
            let is_recent = obj.modified_at >= since_ts;

            if area != "(root)" {
                if is_recent {
                    *recent_areas.entry(area).or_insert(0) += 1;
                } else {
                    *older_areas.entry(area).or_insert(0) += 1;
                }
            }

            if is_recent {
                match obj.kind {
                    ObjectKind::Code => recent_code += 1,
                    ObjectKind::Note | ObjectKind::Doc => recent_docs += 1,
                    _ => {}
                }
                for tok in extract_concept_tokens(obj) {
                    *recent_tokens.entry(tok).or_insert(0) += 1;
                }
            } else {
                for tok in extract_concept_tokens(obj) {
                    *older_tokens.entry(tok).or_insert(0) += 1;
                }
            }
        }

        let mut new_active: Vec<(String, usize, bool)> = recent_areas
            .iter()
            .map(|(area, cnt)| (area.clone(), *cnt, !older_areas.contains_key(area)))
            .collect();
        new_active.sort_by(|a, b| (b.2, b.1, &a.0).cmp(&(a.2, a.1, &b.0)));
        let new_active_areas: Vec<String> = new_active
            .into_iter()
            .take(5)
            .map(|(area, cnt, brand_new)| {
                if brand_new {
                    format!("{area} ({cnt} files, new)")
                } else {
                    format!("{area} ({cnt} updated)")
                }
            })
            .collect();

        let mut inactive: Vec<(String, usize)> = older_areas
            .into_iter()
            .filter(|(area, _)| !recent_areas.contains_key(area))
            .collect();
        inactive.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let inactive_areas: Vec<String> = inactive
            .into_iter()
            .take(5)
            .map(|(area, cnt)| format!("{area} ({cnt} idle files)"))
            .collect();

        let mut scored_concepts: Vec<(String, f64, usize)> = recent_tokens
            .into_iter()
            .filter(|(_, cnt)| *cnt >= 2)
            .map(|(tok, r_cnt)| {
                let o_cnt = older_tokens.get(&tok).copied().unwrap_or(0);
                let lift = (r_cnt as f64) / (1.0 + o_cnt as f64);
                (tok, lift, r_cnt)
            })
            .collect();
        scored_concepts.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.0.cmp(&b.0))
        });
        let emerging_concepts: Vec<String> = scored_concepts
            .into_iter()
            .take(6)
            .map(|(tok, _, _)| tok)
            .collect();

        let documentation_coverage = if recent_docs > 0 && recent_docs * 4 >= recent_code {
            format!("improved ({recent_docs} doc/note vs {recent_code} code files updated)")
        } else if recent_docs > 0 {
            format!("partial ({recent_docs} doc/note vs {recent_code} code files updated)")
        } else if recent_code > 0 {
            format!("lagging (0 doc/note vs {recent_code} code files updated)")
        } else {
            "stable (no recent changes)".to_string()
        };

        let (open_questions, pending_work) = self.scan_project_markers(&project.id)?;

        Ok(ProjectEvolution {
            project,
            since_label: since_label.to_string(),
            since_ts,
            added_count: diff.added.len(),
            modified_count: diff.modified.len(),
            deleted_count: diff.deleted.len(),
            new_active_areas,
            inactive_areas,
            emerging_concepts,
            documentation_coverage,
            open_questions_count: open_questions.len(),
            pending_work_count: pending_work.len(),
        })
    }

    /// Detects deterministic documentation drift and roadmap-vs-implementation drift in a project.
    pub fn project_drift(&self, project_name: &str) -> Result<ProjectDrift, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let root_prefix = project.root_path.as_deref().unwrap_or("");
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
             FROM objects
             WHERE project_id = ?1 AND is_deleted = 0 AND is_noise = 0",
        )?;
        let objects = stmt
            .query_map(params![project.id], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let mut findings = Vec::new();

        // 1. Project-wide documentation drift
        let latest_code = objects
            .iter()
            .filter(|o| o.kind == ObjectKind::Code)
            .map(|o| o.modified_at)
            .max();
        let latest_doc = objects
            .iter()
            .filter(|o| matches!(o.kind, ObjectKind::Note | ObjectKind::Doc))
            .map(|o| o.modified_at)
            .max();

        if let (Some(c_ts), Some(d_ts)) = (latest_code, latest_doc)
            && c_ts - d_ts > DOC_DRIFT_THRESHOLD_SECS
        {
            let lag_days = (c_ts - d_ts) / 86_400;
            findings.push(DriftFinding {
                kind: "documentation drift".to_string(),
                summary: format!("Code changed {lag_days} day(s) more recently than any doc/note"),
                evidence: Vec::new(),
            });
        }

        // 2. Per-area documentation drift (active code areas with zero docs inside the area)
        if let Some(max_ts) = objects.iter().map(|o| o.modified_at).max() {
            let active_cutoff = max_ts.saturating_sub(14 * 86_400);
            let mut area_stats: HashMap<String, (Vec<String>, usize)> = HashMap::new();
            for obj in &objects {
                let area = object_area(&obj.path, root_prefix);
                if area == "(root)" {
                    continue;
                }
                let entry = area_stats.entry(area).or_insert_with(|| (Vec::new(), 0));
                if obj.kind == ObjectKind::Code && obj.modified_at >= active_cutoff {
                    entry.0.push(obj.title.clone());
                } else if matches!(obj.kind, ObjectKind::Note | ObjectKind::Doc) {
                    entry.1 += 1;
                }
            }

            let mut undocumented_areas: Vec<(String, Vec<String>)> = area_stats
                .into_iter()
                .filter(|(_, (code_files, doc_count))| code_files.len() >= 3 && *doc_count == 0)
                .map(|(area, (code_files, _))| (area, code_files))
                .collect();
            undocumented_areas.sort_by_key(|b| std::cmp::Reverse(b.1.len()));

            for (area, code_files) in undocumented_areas.into_iter().take(2) {
                findings.push(DriftFinding {
                    kind: "documentation drift".to_string(),
                    summary: format!(
                        "Active area '{area}' has {} recently modified code files and no local docs",
                        code_files.len()
                    ),
                    evidence: code_files.into_iter().take(3).collect(),
                });
            }
        }

        // 3. Roadmap drift: unchecked ROADMAP/SPEC items that already have matching code files
        let (_, pending_work) = self.scan_project_markers(&project.id)?;
        let code_objects: Vec<&ObjectRecord> = objects
            .iter()
            .filter(|o| {
                o.kind == ObjectKind::Code
                    && !o.path.replace('\\', "/").to_lowercase().contains("/.github/")
            })
            .collect();

        let project_name_lower = project.name.to_lowercase();
        let trimmed_root = root_prefix.trim_end_matches(['/', '\\']);

        for task in pending_work.into_iter().take(8) {
            let task_raw = task.split_once(": ").map(|(_, rest)| rest).unwrap_or(&task);
            let task_body = task_raw.split(" — *Status").next().unwrap_or(task_raw).trim();
            let tokens: Vec<String> = extract_task_keywords(task_body)
                .into_iter()
                .filter(|t| *t != project_name_lower)
                .collect();
            if tokens.is_empty() {
                continue;
            }

            let mut matching_files = Vec::new();
            for code in &code_objects {
                let rel = if !trimmed_root.is_empty() {
                    code.path
                        .strip_prefix(trimmed_root)
                        .map(|r| r.trim_start_matches(['/', '\\']))
                        .unwrap_or(&code.path)
                } else {
                    &code.path
                };

                let title_tokens = tokenize_identifier(
                    Path::new(&code.title)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&code.title),
                );
                let rel_tokens = tokenize_identifier(rel);

                let title_overlap = tokens.iter().filter(|t| title_tokens.contains(*t)).count();
                let path_overlap = tokens.iter().filter(|t| rel_tokens.contains(*t)).count();

                // Require exact token match on file stem, or at least 2 distinct relative-path token matches.
                if title_overlap >= 1 || path_overlap >= 2 {
                    matching_files.push(rel.to_string());
                }
            }

            if !matching_files.is_empty() {
                matching_files.truncate(3);
                let short_task: String = task_body.chars().take(90).collect();
                findings.push(DriftFinding {
                    kind: "roadmap drift".to_string(),
                    summary: format!("Pending roadmap item '{short_task}' has matching code"),
                    evidence: matching_files,
                });
            }
        }

        // 4. Structural graph & critical risk drift via shared Store::analyze_area_risks
        let area_risks = self.analyze_area_risks(project_name)?;
        let structural_risks: Vec<_> = area_risks
            .into_iter()
            .filter(|r| {
                r.relation_count >= 3
                    && (r.coverage_tier == CoverageTier::Underdocumented || r.level != RiskLevel::Low)
            })
            .take(4)
            .collect();

        for r in structural_risks {
            let cov_label = match r.coverage_tier {
                CoverageTier::Underdocumented => "Low (0 docs/refs)",
                CoverageTier::PartiallyCovered => "Partial",
                CoverageTier::WellCovered => "Well Covered",
            };
            let kind = if r.level == RiskLevel::Critical {
                "critical risk".to_string()
            } else {
                "structural risk drift".to_string()
            };
            let mut evidence = vec![
                format!("Relations: {}", r.relation_count),
                format!(
                    "Coverage: {cov_label} (score: {:.1}, code files: {})",
                    r.coverage_score, r.code_files
                ),
                format!("TODOs: {}", r.open_todos),
                format!("Recent activity (14d): {} modified", r.recent_activity),
            ];
            if let Some(hub) = r.top_hub {
                evidence.push(format!("Top hub: {hub}"));
            }
            if r.level == RiskLevel::Critical {
                evidence.push(format!("Reason: {}", r.reason));
            }
            findings.push(DriftFinding {
                kind,
                summary: format!(
                    "Area '{}' is a core dependency hub — {} (score: {:.1})",
                    r.area, r.level, r.risk_score
                ),
                evidence,
            });
        }
        Ok(ProjectDrift { project, findings })
    }
}

fn extract_concept_tokens(obj: &ObjectRecord) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let stem = Path::new(&obj.title)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&obj.title);

    for raw in stem.split(|c: char| !c.is_alphanumeric()) {
        push_if_meaningful(raw, &mut tokens);
    }

    if let Some(ref summary) = obj.summary {
        for raw in summary
            .split(|c: char| !c.is_alphanumeric())
            .take(30)
        {
            push_if_meaningful(raw, &mut tokens);
        }
    }

    tokens
}

fn push_if_meaningful(raw: &str, out: &mut HashSet<String>) {
    let lower = raw.to_lowercase();
    if lower.len() < 4 {
        return;
    }
    if matches!(
        lower.as_str(),
        "main"
            | "test"
            | "tests"
            | "cargo"
            | "toml"
            | "json"
            | "yaml"
            | "rust"
            | "this"
            | "that"
            | "with"
            | "from"
            | "into"
            | "true"
            | "false"
            | "none"
            | "some"
            | "pub"
            | "struct"
            | "impl"
            | "crate"
            | "crates"
            | "readme"
            | "roadmap"
            | "license"
            | "changelog"
            | "package"
            | "version"
            | "workspace"
            | "edition"
            | "dependencies"
            | "name"
            | "description"
            | "path"
    ) {
        return;
    }
    out.insert(lower);
}


fn tokenize_identifier(raw: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for part in raw.split(|c: char| !c.is_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        let lower = part.to_lowercase();
        if lower.len() >= 2 {
            out.insert(lower);
        }
    }
    out
}
fn extract_task_keywords(task: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in task.split(|c: char| !c.is_alphanumeric() && c != '_') {
        for part in raw.split('_') {
            let lower = part.to_lowercase();
            if lower.len() < 4 {
                continue;
            }
            if matches!(
                lower.as_str(),
                "implement"
                    | "build"
                    | "develop"
                    | "support"
                    | "create"
                    | "introduce"
                    | "update"
                    | "status"
                    | "missing"
                    | "based"
                    | "with"
                    | "from"
                    | "into"
                    | "modules"
                    | "module"
                    | "system"
                    | "locally"
                    | "only"
                    | "file"
                    | "files"
                    | "code"
                    | "rust"
                    | "2026"
                    | "model"
                    | "linux"
                    | "state"
                    | "memory"
                    | "userspace"
            ) {
                continue;
            }
            if !out.contains(&lower) {
                out.push(lower);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use vos_core::util::now_unix;
    use vos_core::ObjectKind;

    use super::*;
    use crate::UpsertObjectInput;

    #[test]
    fn evolution_and_drift_surface_area_shifts_and_roadmap_drift() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("demo", None, None).unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/ROADMAP.md".to_string(),
                kind: ObjectKind::Note,
                title: "ROADMAP.md".to_string(),
                summary: None,
                content_hash: "h1".to_string(),
                size_bytes: 90,
                modified_at: now - 20 * 86_400,
                is_noise: false,
                chunks: vec!["- [ ] Implement kv_cache paging engine".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/legacy/old.rs".to_string(),
                kind: ObjectKind::Code,
                title: "old.rs".to_string(),
                summary: None,
                content_hash: "h2".to_string(),
                size_bytes: 50,
                modified_at: now - 40 * 86_400,
                is_noise: false,
                chunks: Vec::new(),
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/transformer/kv_cache.rs".to_string(),
                kind: ObjectKind::Code,
                title: "kv_cache.rs".to_string(),
                summary: Some("Hierarchical kv_cache allocator".to_string()),
                content_hash: "h3".to_string(),
                size_bytes: 120,
                modified_at: now - 3600,
                is_noise: false,
                chunks: vec!["pub struct KvCache;".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let evo = store
            .project_evolution("demo", now - 7 * 86_400, "7d")
            .unwrap();
        assert_eq!(evo.added_count, 1);
        assert!(evo.new_active_areas.iter().any(|a| a.contains("repo/transformer")));
        assert!(evo.inactive_areas.iter().any(|a| a.contains("repo/legacy")));

        let drift = store.project_drift("demo").unwrap();
        assert!(drift.findings.iter().any(|f| f.kind == "documentation drift"));
        assert!(drift.findings.iter().any(|f| f.kind == "roadmap drift"));

        // Ensure strict token matching does not conflate "Intel" with "intelligence.rs".
        let mut batch2 = store.begin_index_batch().unwrap();
        batch2
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/ROADMAP.md".to_string(),
                kind: ObjectKind::Note,
                title: "ROADMAP.md".to_string(),
                summary: None,
                content_hash: "h_intel".to_string(),
                size_bytes: 60,
                modified_at: now - 10 * 86_400,
                is_noise: false,
                chunks: vec!["- [ ] Intel GPU boot support".to_string()],
            })
            .unwrap();
        batch2
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/ai/intelligence.rs".to_string(),
                kind: ObjectKind::Code,
                title: "intelligence.rs".to_string(),
                summary: None,
                content_hash: "h_ai".to_string(),
                size_bytes: 50,
                modified_at: now - 1000,
                is_noise: false,
                chunks: Vec::new(),
            })
            .unwrap();
        batch2.commit().unwrap();

        let drift2 = store.project_drift("demo").unwrap();
        assert!(!drift2
            .findings
            .iter()
            .any(|f| f.evidence.iter().any(|ev| ev.contains("intelligence.rs"))));
    }

    #[test]
    fn config_files_do_not_trigger_documentation_drift() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("cfg", None, None).unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        for lang in ["en", "de", "es"] {
            batch
                .upsert_object(UpsertObjectInput {
                    project_id: Some(proj.id.clone()),
                    path: format!("/repo/locales/{lang}.json"),
                    kind: ObjectKind::Config,
                    title: format!("{lang}.json"),
                    summary: None,
                    content_hash: format!("h_{lang}"),
                    size_bytes: 40,
                    modified_at: now - 1000,
                    is_noise: false,
                    chunks: vec!["{\"hello\": \"world\"}".to_string()],
                })
                .unwrap();
        }
        batch.commit().unwrap();

        let drift = store.project_drift("cfg").unwrap();
        assert!(!drift.findings.iter().any(|f| f.kind == "documentation drift"));
    }
}





