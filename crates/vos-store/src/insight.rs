use std::collections::HashMap;

use rusqlite::params;
use vos_core::util::now_unix;
use vos_core::{
    AreaCoverage, AreaOwnership, CoverageTier, ObjectKind, ObjectRecord, ProjectCoverage,
    ProjectOwnership, RelationType,
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
}
