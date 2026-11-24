use std::collections::HashMap;

use rusqlite::params;
use vos_core::util::now_unix;
use vos_core::{
    AreaCoverage, AreaOwnership, AreaRisk, CoverageTier, GraphDirection, ObjectKind, ObjectRecord, ProjectCoverage, ProjectGraph,
    ProjectOwnership, RelationType, RiskLevel,
};

use crate::queries::map_object_row;
use crate::recovery::object_area;
use crate::{Store, StoreError};

const LOCAL_DOC_SCORE: f64 = 1.0;
const MD_LINK_SCORE: f64 = 3.0;
const PATH_REF_SCORE: f64 = 3.0;

#[derive(Default)]
struct OwnershipAccum {
    file_count: usize,
    event_count: usize,
    recent_modified_count: usize,
    latest_modified_at: Option<i64>,
}

#[derive(Default)]
struct CoverageAccum {
    code_files: usize,
    local_docs: usize,
    md_link_refs: usize,
    path_ref_refs: usize,
}

impl Store {
    /// Aggregates non-git activity ownership metrics by project directory area.
    pub fn project_ownership(
        &self,
        project_name: &str,
        days: u32,
        limit: usize,
    ) -> Result<ProjectOwnership, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let window_days = days.max(1);
        let since_ts = now_unix().saturating_sub(i64::from(window_days) * 86_400);
        let max_items = limit.clamp(1, 25);
        let root_prefix = project.root_path.as_deref().unwrap_or("");

        let objects = self.load_active_non_noise_objects(&project.id)?;
        let mut by_area: HashMap<String, OwnershipAccum> = HashMap::new();

        for obj in &objects {
            let area = object_area(&obj.path, root_prefix);
            let entry = by_area.entry(area).or_default();
            entry.file_count += 1;
            if obj.modified_at >= since_ts {
                entry.recent_modified_count += 1;
            }
            entry.latest_modified_at = Some(
                entry
                    .latest_modified_at
                    .map_or(obj.modified_at, |prev| prev.max(obj.modified_at)),
            );
        }

        let mut ev_stmt = self.conn.prepare(
            "SELECT e.path
             FROM events e
             LEFT JOIN objects o ON o.id = e.object_id
             WHERE e.project_id = ?1
               AND COALESCE(o.is_noise, 0) = 0",
        )?;
        let ev_paths = ev_stmt
            .query_map(params![project.id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;

        for path in ev_paths {
            let area = object_area(&path, root_prefix);
            by_area.entry(area).or_default().event_count += 1;
        }

        let has_subdir_areas = by_area.keys().any(|a| a != "(root)");
        let mut areas: Vec<AreaOwnership> = by_area
            .into_iter()
            .filter(|(area, acc)| acc.file_count > 0 && (!has_subdir_areas || area != "(root)"))
            .map(|(area, acc)| AreaOwnership {
                area,
                file_count: acc.file_count,
                event_count: acc.event_count,
                recent_modified_count: acc.recent_modified_count,
                activity_score: acc.event_count + acc.recent_modified_count,
                latest_modified_at: acc.latest_modified_at.unwrap_or(0),
            })
            .collect();

        let total_areas = areas.len();

        areas.sort_by(|a, b| {
            b.activity_score
                .cmp(&a.activity_score)
                .then_with(|| b.file_count.cmp(&a.file_count))
                .then_with(|| b.latest_modified_at.cmp(&a.latest_modified_at))
                .then_with(|| a.area.cmp(&b.area))
        });
        let most_active_areas: Vec<AreaOwnership> = areas.iter().take(max_items).cloned().collect();

        let mut recently_active_areas: Vec<AreaOwnership> = areas
            .iter()
            .filter(|a| a.recent_modified_count > 0)
            .cloned()
            .collect();
        recently_active_areas.sort_by(|a, b| {
            b.recent_modified_count
                .cmp(&a.recent_modified_count)
                .then_with(|| b.latest_modified_at.cmp(&a.latest_modified_at))
                .then_with(|| b.event_count.cmp(&a.event_count))
                .then_with(|| a.area.cmp(&b.area))
        });
        recently_active_areas.truncate(max_items);

        let mut stable_candidates: Vec<AreaOwnership> = areas
            .iter()
            .filter(|a| a.recent_modified_count == 0)
            .cloned()
            .collect();
        if stable_candidates.is_empty() {
            stable_candidates = areas;
        }
        stable_candidates.sort_by(|a, b| {
            a.latest_modified_at
                .cmp(&b.latest_modified_at)
                .then_with(|| a.recent_modified_count.cmp(&b.recent_modified_count))
                .then_with(|| a.event_count.cmp(&b.event_count))
                .then_with(|| a.area.cmp(&b.area))
        });
        stable_candidates.truncate(max_items);

        Ok(ProjectOwnership {
            project,
            days: window_days,
            since_ts,
            total_areas,
            most_active_areas,
            recently_active_areas,
            most_stable_areas: stable_candidates,
        })
    }

    /// Evaluates documentation coverage per area combining local docs and deterministic `relations`.
    pub fn project_coverage(&self, project_name: &str) -> Result<ProjectCoverage, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let root_prefix = project.root_path.as_deref().unwrap_or("");
        let objects = self.load_active_non_noise_objects(&project.id)?;

        let mut code_files = 0_usize;
        let mut doc_files = 0_usize;
        let mut by_area: HashMap<String, CoverageAccum> = HashMap::new();

        for obj in &objects {
            let area = object_area(&obj.path, root_prefix);
            match obj.kind {
                ObjectKind::Code => {
                    code_files += 1;
                    by_area.entry(area).or_default().code_files += 1;
                }
                ObjectKind::Note | ObjectKind::Doc => {
                    doc_files += 1;
                    by_area.entry(area).or_default().local_docs += 1;
                }
                _ => {}
            }
        }

        // Count deterministic links (`md_link`, `path_ref`) from docs/notes pointing to code files.
        let mut rel_stmt = self.conn.prepare(
            "SELECT o_to.path, r.relation_type
             FROM relations r
             JOIN objects o_from ON o_from.id = r.from_id
             JOIN objects o_to ON o_to.id = r.to_id
             WHERE o_from.project_id = ?1
               AND o_to.project_id = ?1
               AND o_from.is_deleted = 0
               AND o_to.is_deleted = 0
               AND o_from.is_noise = 0
               AND o_to.is_noise = 0
               AND o_from.kind IN ('note', 'doc')
               AND o_to.kind = 'code'
               AND r.relation_type IN ('md_link', 'path_ref')",
        )?;
        let rel_rows = rel_stmt
            .query_map(params![project.id], |row| {
                let target_path: String = row.get(0)?;
                let rel_type: String = row.get(1)?;
                Ok((target_path, RelationType::from_db(&rel_type)))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for (target_path, rel_type) in rel_rows {
            let area = object_area(&target_path, root_prefix);
            let entry = by_area.entry(area).or_default();
            match rel_type {
                RelationType::MdLink => entry.md_link_refs += 1,
                RelationType::PathRef => entry.path_ref_refs += 1,
                RelationType::RustMod | RelationType::RustUse => {}
            }
        }

        let coverage_ratio = if code_files == 0 {
            if doc_files > 0 { 100.0 } else { 0.0 }
        } else {
            (doc_files as f64 / code_files as f64) * 100.0
        };

        let has_subdir_areas = by_area.keys().any(|a| a != "(root)");
        let mut well_covered = Vec::new();
        let mut partially_covered = Vec::new();
        let mut underdocumented = Vec::new();

        for (area, acc) in by_area {
            if has_subdir_areas && area == "(root)" {
                continue;
            }
            if acc.code_files == 0 && acc.local_docs == 0 {
                continue;
            }

            let weighted_score = (acc.local_docs as f64) * LOCAL_DOC_SCORE
                + (acc.md_link_refs as f64) * MD_LINK_SCORE
                + (acc.path_ref_refs as f64) * PATH_REF_SCORE;

            let tier = if (acc.code_files == 0 && acc.local_docs > 0)
                || (acc.code_files > 0 && weighted_score >= (acc.code_files as f64) * 0.50)
            {
                CoverageTier::WellCovered
            } else if weighted_score > 0.0 {
                CoverageTier::PartiallyCovered
            } else {
                CoverageTier::Underdocumented
            };

            let item = AreaCoverage {
                area,
                code_files: acc.code_files,
                local_docs: acc.local_docs,
                md_link_refs: acc.md_link_refs,
                path_ref_refs: acc.path_ref_refs,
                weighted_score,
                tier,
            };

            match tier {
                CoverageTier::WellCovered => well_covered.push(item),
                CoverageTier::PartiallyCovered => partially_covered.push(item),
                CoverageTier::Underdocumented => underdocumented.push(item),
            }
        }

        well_covered.sort_by(|a, b| {
            b.weighted_score
                .total_cmp(&a.weighted_score)
                .then_with(|| b.local_docs.cmp(&a.local_docs))
                .then_with(|| b.code_files.cmp(&a.code_files))
                .then_with(|| a.area.cmp(&b.area))
        });
        partially_covered.sort_by(|a, b| {
            let ra = a.weighted_score / (a.code_files.max(1) as f64);
            let rb = b.weighted_score / (b.code_files.max(1) as f64);
            rb.total_cmp(&ra)
                .then_with(|| b.weighted_score.total_cmp(&a.weighted_score))
                .then_with(|| a.area.cmp(&b.area))
        });
        underdocumented.sort_by(|a, b| {
            b.code_files
                .cmp(&a.code_files)
                .then_with(|| a.area.cmp(&b.area))
        });

        Ok(ProjectCoverage {
            project,
            code_files,
            doc_files,
            coverage_ratio,
            well_covered,
            partially_covered,
            underdocumented,
        })
    }

/// Computes combined Graph + Ownership + Coverage + TODO structural risks across project areas.
    pub fn analyze_area_risks(&self, project_name: &str) -> Result<Vec<AreaRisk>, StoreError> {
        let (_, _, _, risks) = self.compute_insight_bundle(project_name)?;
        Ok(risks)
    }

    pub(crate) fn compute_insight_bundle(
        &self,
        project_name: &str,
    ) -> Result<(ProjectOwnership, ProjectCoverage, ProjectGraph, Vec<AreaRisk>), StoreError> {
        let ownership = self.project_ownership(project_name, 14, 200)?;
        let coverage = self.project_coverage(project_name)?;
        let graph = self.project_graph(project_name, None, GraphDirection::Both, 1, 500)?;

        let root_prefix = ownership.project.root_path.as_deref().unwrap_or("");
        let trimmed_root = root_prefix.trim_end_matches(['/', '\\']);
        let objects = self.load_active_non_noise_objects(&ownership.project.id)?;
        let todos_by_area = self.count_open_todos_by_area(&ownership.project.id, root_prefix, &objects)?;

        let mut rel_count_by_area: HashMap<String, usize> = HashMap::new();
        let mut top_hub_by_area: HashMap<String, String> = HashMap::new();
        for hub in &graph.hubs {
            let area = object_area(&hub.object.path, root_prefix);
            let deg = hub.outgoing_count + hub.incoming_count;
            if deg == 0 {
                continue;
            }
            *rel_count_by_area.entry(area.clone()).or_insert(0) += deg;
            top_hub_by_area.entry(area).or_insert_with(|| {
                let rel_path = if !trimmed_root.is_empty() {
                    hub.object
                        .path
                        .strip_prefix(trimmed_root)
                        .map(|r| r.trim_start_matches(['/', '\\']))
                        .unwrap_or(&hub.object.path)
                } else {
                    &hub.object.path
                };
                format!("{rel_path} ({deg} relations)")
            });
        }

        let recent_by_area: HashMap<String, usize> = ownership
            .most_active_areas
            .iter()
            .map(|a| (a.area.clone(), a.recent_modified_count))
            .collect();

        let mut risks = Vec::new();
        for cov in coverage
            .underdocumented
            .iter()
            .chain(coverage.partially_covered.iter())
        {
            let relation_count = rel_count_by_area.get(&cov.area).copied().unwrap_or(0);
            let recent_activity = recent_by_area.get(&cov.area).copied().unwrap_or(0);
            let open_todos = todos_by_area.get(&cov.area).copied().unwrap_or(0);

            if relation_count == 0 && recent_activity == 0 {
                continue;
            }

            let risk_score = (relation_count as f64) * 2.0
                + (recent_activity as f64)
                - cov.weighted_score;
            let is_high_base = risk_score >= 15.0;
            let is_low_cov = cov.tier == CoverageTier::Underdocumented;

            let level = if is_high_base && is_low_cov && open_todos > 0 {
                RiskLevel::Critical
            } else if is_high_base {
                RiskLevel::High
            } else if risk_score >= 6.0 {
                RiskLevel::Medium
            } else {
                RiskLevel::Low
            };

            let reason = match level {
                RiskLevel::Critical => {
                    "High dependency hub with low coverage and active unfinished work".to_string()
                }
                RiskLevel::High => {
                    "High dependency hub with insufficient documentation".to_string()
                }
                RiskLevel::Medium => {
                    "Moderate dependency hub needing documentation".to_string()
                }
                RiskLevel::Low => "Low structural risk".to_string(),
            };

            let top_hub = top_hub_by_area.get(&cov.area).cloned();
            risks.push(AreaRisk {
                area: cov.area.clone(),
                level,
                risk_score,
                relation_count,
                recent_activity,
                coverage_tier: cov.tier,
                coverage_score: cov.weighted_score,
                code_files: cov.code_files,
                open_todos,
                top_hub,
                reason,
            });
        }

        risks.sort_by(|a, b| {
            b.level
                .cmp(&a.level)
                .then_with(|| b.risk_score.total_cmp(&a.risk_score))
                .then_with(|| b.open_todos.cmp(&a.open_todos))
                .then_with(|| a.area.cmp(&b.area))
        });

        Ok((ownership, coverage, graph, risks))
    }

    fn count_open_todos_by_area(
        &self,
        project_id: &str,
        root_prefix: &str,
        objects: &[ObjectRecord],
    ) -> Result<HashMap<String, usize>, StoreError> {
        let mut counts: HashMap<String, usize> = HashMap::new();

        // 1. Code chunks containing explicit TODO/FIXME/OPEN QUESTION markers.
        let mut stmt = self.conn.prepare(
            "SELECT o.path, c.text
             FROM chunks c
             JOIN objects o ON o.id = c.object_id
             WHERE o.project_id = ?1
               AND o.is_deleted = 0
               AND o.is_noise = 0
               AND o.kind = 'code'
               AND (
                   c.text LIKE '%TODO%'
                   OR c.text LIKE '%FIXME%'
                   OR c.text LIKE '%OPEN QUESTION%'
               )",
        )?;
        let rows = stmt
            .query_map(params![project_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for (path, text) in rows {
            let area = object_area(&path, root_prefix);
            let mut hit_lines = 0_usize;
            for line in text.lines() {
                if line_has_unfinished_marker(line) {
                    hit_lines += 1;
                }
            }
            if hit_lines > 0 {
                *counts.entry(area).or_insert(0) += hit_lines;
            }
        }

        // 2. Unchecked tasks / open questions in docs referencing an area or its specific files.
        let (open_questions, pending_work) = self.scan_project_markers(project_id)?;
        if !open_questions.is_empty() || !pending_work.is_empty() {
            let mut stems_by_area: HashMap<String, Vec<String>> = HashMap::new();
            for obj in objects {
                if obj.kind != ObjectKind::Code {
                    continue;
                }
                let area = object_area(&obj.path, root_prefix);
                if area == "(root)" {
                    continue;
                }
                let entry = stems_by_area.entry(area.clone()).or_default();
                let area_lower = area.to_ascii_lowercase();
                if !entry.contains(&area_lower) {
                    entry.push(area_lower);
                }
                let title_lower = obj.title.to_ascii_lowercase();
                if !matches!(
                    title_lower.as_str(),
                    "mod.rs" | "lib.rs" | "main.rs" | "index.ts" | "index.js"
                ) && !entry.contains(&title_lower)
                {
                    entry.push(title_lower);
                }
                if let Some(stem) = std::path::Path::new(&obj.title)
                    .file_stem()
                    .and_then(|s| s.to_str())
                {
                    let stem_lower = stem.to_ascii_lowercase();
                    if stem_lower.len() >= 4
                        && !matches!(
                            stem_lower.as_str(),
                            "mod" | "lib" | "main" | "index" | "test" | "tests" | "util" | "utils" | "error" | "models"
                        )
                        && !entry.contains(&stem_lower)
                    {
                        entry.push(stem_lower);
                    }
                }
            }

            for marker in pending_work.iter().chain(open_questions.iter()) {
                let marker_lower = marker.to_ascii_lowercase();
                for (area, terms) in &stems_by_area {
                    if terms.iter().any(|t| marker_lower.contains(t.as_str())) {
                        *counts.entry(area.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        Ok(counts)
    }
    fn load_active_non_noise_objects(&self, project_id: &str) -> Result<Vec<ObjectRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
             FROM objects
             WHERE project_id = ?1
               AND is_deleted = 0
               AND is_noise = 0",
        )?;
        let rows = stmt.query_map(params![project_id], map_object_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
    }
}

fn line_has_unfinished_marker(raw_line: &str) -> bool {
    let lower = raw_line.trim().to_ascii_lowercase();
    lower.contains("// todo")
        || lower.contains("// fixme")
        || lower.contains("# todo")
        || lower.contains("# fixme")
        || lower.contains("/* todo")
        || lower.contains("/* fixme")
        || lower.starts_with("todo:")
        || lower.starts_with("fixme:")
        || lower.starts_with("open question:")
        || lower.contains("todo!(")
}
#[cfg(test)]
mod tests {
    use vos_core::util::now_unix;
    use vos_core::{ObjectKind, RelationType};

    use super::*;
    use crate::{RelationInput, UpsertObjectInput};

    fn obj_input(project_id: &str, path: &str, kind: ObjectKind, modified_at: i64) -> UpsertObjectInput {
        let title = path.rsplit('/').next().unwrap_or(path).to_string();
        UpsertObjectInput {
            project_id: Some(project_id.to_string()),
            path: path.to_string(),
            kind,
            title: title.clone(),
            summary: Some(title.clone()),
            content_hash: format!("hash_{path}_{modified_at}"),
            size_bytes: 100,
            modified_at,
            is_noise: false,
            chunks: vec![title],
        }
    }

    #[test]
    fn ownership_and_relation_weighted_coverage_work() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("atlas", None, None).unwrap();
        let now = now_unix();
        let old = now - 40 * 86_400;

        let mut batch = store.begin_index_batch().unwrap();
        let id_doc = batch
            .upsert_object(obj_input(&proj.id, "docs/ARCH.md", ObjectKind::Note, old))
            .unwrap()
            .object_id()
            .to_string();
        let id_tr1 = batch
            .upsert_object(obj_input(&proj.id, "src/transformer/mod.rs", ObjectKind::Code, now - 60))
            .unwrap()
            .object_id()
            .to_string();
        let _id_tr2 = batch
            .upsert_object(obj_input(&proj.id, "src/transformer/layer.rs", ObjectKind::Code, now - 120))
            .unwrap();
        let _id_net1 = batch
            .upsert_object(obj_input(&proj.id, "src/net/client.rs", ObjectKind::Code, old))
            .unwrap();
        let _id_net2 = batch
            .upsert_object(obj_input(&proj.id, "src/net/server.rs", ObjectKind::Code, old))
            .unwrap();

        // DOC links directly to src/transformer via md_link -> weighted_score = 3.0 >= 2 * 0.5
        batch
            .sync_relations(
                std::slice::from_ref(&id_doc),
                &[RelationInput {
                    from_object_id: id_doc.clone(),
                    to_object_id: id_tr1,
                    relation_type: RelationType::MdLink,
                    confidence: 0.95,
                    detail: Some("link: ../src/transformer/mod.rs".to_string()),
                }],
            )
            .unwrap();
        batch.commit().unwrap();

        let own = store.project_ownership("atlas", 14, 5).unwrap();
        assert_eq!(own.most_active_areas[0].area, "src/transformer");
        assert_eq!(own.recently_active_areas.len(), 1);
        assert_eq!(own.recently_active_areas[0].area, "src/transformer");
        assert!(own.most_stable_areas.iter().any(|a| a.area == "docs"));

        let cov = store.project_coverage("atlas").unwrap();
        assert_eq!(cov.code_files, 4);
        assert_eq!(cov.doc_files, 1);
        assert!(cov.well_covered.iter().any(|a| a.area == "docs"));
        assert!(
            cov.well_covered
                .iter()
                .any(|a| a.area == "src/transformer" && a.md_link_refs == 1 && (a.weighted_score - 3.0).abs() < 1e-6)
        );
        assert!(cov.underdocumented.iter().any(|a| a.area == "src/net" && a.code_files == 2));
    }

    #[test]
    fn continue_status_and_drift_aggregate_insight_layer_signals() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("fusion", None, None).unwrap();
        let now = now_unix();

        let mut batch = store.begin_index_batch().unwrap();
        let id_hub = batch
            .upsert_object(obj_input(&proj.id, "src/core/mod.rs", ObjectKind::Code, now - 60))
            .unwrap()
            .object_id()
            .to_string();
        let id_a = batch
            .upsert_object(obj_input(&proj.id, "src/core/a.rs", ObjectKind::Code, now - 120))
            .unwrap()
            .object_id()
            .to_string();
        let id_b = batch
            .upsert_object(obj_input(&proj.id, "src/core/b.rs", ObjectKind::Code, now - 180))
            .unwrap()
            .object_id()
            .to_string();
        let id_c = batch
            .upsert_object(obj_input(&proj.id, "src/core/c.rs", ObjectKind::Code, now - 240))
            .unwrap()
            .object_id()
            .to_string();
        let _id_readme = batch
            .upsert_object(obj_input(&proj.id, "README.md", ObjectKind::Note, now - 300))
            .unwrap();

        batch
            .sync_relations(
                std::slice::from_ref(&id_hub),
                &[
                    RelationInput {
                        from_object_id: id_hub.clone(),
                        to_object_id: id_a,
                        relation_type: RelationType::RustMod,
                        confidence: 1.0,
                        detail: Some("mod a".to_string()),
                    },
                    RelationInput {
                        from_object_id: id_hub.clone(),
                        to_object_id: id_b,
                        relation_type: RelationType::RustMod,
                        confidence: 1.0,
                        detail: Some("mod b".to_string()),
                    },
                    RelationInput {
                        from_object_id: id_hub.clone(),
                        to_object_id: id_c,
                        relation_type: RelationType::RustMod,
                        confidence: 1.0,
                        detail: Some("mod c".to_string()),
                    },
                ],
            )
            .unwrap();
        batch.commit().unwrap();

        let cont = store.project_continue("fusion").unwrap();
        assert!(!cont.dependency_hubs.is_empty());
        assert!(!cont.coverage_warnings.is_empty());
        assert!(!cont.ownership_areas.is_empty());
        assert!(cont.continue_from.iter().any(|s| s.reason.contains("Hub node")));

        let status = store.project_status("fusion").unwrap();
        assert!((status.coverage_ratio - 25.0).abs() < 0.1);
        assert!(!status.dependency_hubs.is_empty());
        assert!(status.risk_areas.iter().any(|r| r.contains("high activity + low coverage")));

        let drift = store.project_drift("fusion").unwrap();
        assert!(drift.findings.iter().any(|f| f.kind == "structural risk drift" && f.summary.contains("src/core")));
    }

    #[test]
    fn high_risk_underdocumented_hub_with_todos_becomes_critical_risk() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("crit", None, None).unwrap();
        let now = now_unix();

        let mut mod_input = obj_input(&proj.id, "src/compute/mod.rs", ObjectKind::Code, now - 60);
        mod_input.chunks = vec!["// TODO: finish SIMD dispatch\npub mod a;".to_string()];

        let mut batch = store.begin_index_batch().unwrap();
        let id_hub = batch
            .upsert_object(mod_input)
            .unwrap()
            .object_id()
            .to_string();

        let mut rels = Vec::new();
        for name in ["a", "b", "c", "d", "e", "f", "g"] {
            let child_id = batch
                .upsert_object(obj_input(
                    &proj.id,
                    &format!("src/compute/{name}.rs"),
                    ObjectKind::Code,
                    now - 120,
                ))
                .unwrap()
                .object_id()
                .to_string();
            rels.push(RelationInput {
                from_object_id: id_hub.clone(),
                to_object_id: child_id,
                relation_type: RelationType::RustMod,
                confidence: 1.0,
                detail: Some(format!("mod {name}")),
            });
        }
        batch.sync_relations(std::slice::from_ref(&id_hub), &rels).unwrap();
        batch.commit().unwrap();

        let status = store.project_status("crit").unwrap();
        assert_eq!(status.critical_risk_count, 1);
        assert_eq!(status.critical_areas[0].area, "src/compute");
        assert_eq!(status.critical_areas[0].level, RiskLevel::Critical);
        assert_eq!(status.critical_areas[0].open_todos, 1);

        let drift = store.project_drift("crit").unwrap();
        assert!(drift.findings.iter().any(|f| f.kind == "critical risk"));
    }
}