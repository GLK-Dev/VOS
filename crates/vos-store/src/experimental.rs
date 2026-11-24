use vos_core::util::now_unix;
use std::collections::{HashMap, HashSet, VecDeque};
use rusqlite::params;
use vos_core::{
    DocQualityEntry, DocQualityReport, DocQualityTier, DocumentationHealth, GraphDirection,
    HubImportance, ObjectImpact, ProjectStatus, RecoveryDifficulty, RecoveryPrediction, RelationType,
};

use crate::{Store, StoreError};

impl Store {
    /// Predicts project re-entry difficulty (`Easy`, `Medium`, `Hard`) from existing status signals.
    pub fn predict_recovery_difficulty(&self, status: &ProjectStatus) -> RecoveryPrediction {
        predict_from_status_at(status, now_unix())
    }

    /// Computes normalized centrality (`hub_score` 0.0..=1.0) and structural role per connected node.
    pub fn experimental_hub_importance(
        &self,
        project_name: &str,
        limit: usize,
    ) -> Result<Vec<HubImportance>, StoreError> {
        let graph = self.project_graph(project_name, None, GraphDirection::Both, 1, 500)?;
        let root_prefix = graph.project.root_path.as_deref().unwrap_or("");
        let trimmed_root = root_prefix.trim_end_matches(['/', '\\']);

        let mut scored: Vec<(f64, HubImportance)> = Vec::new();
        let mut max_raw = 0.0_f64;

        for hub in &graph.hubs {
            let in_deg = hub.incoming_count;
            let out_deg = hub.outgoing_count;
            let total = in_deg + out_deg;
            if total == 0 {
                continue;
            }
            let raw = 0.6 * (in_deg as f64) + 0.4 * (out_deg as f64);
            if raw > max_raw {
                max_raw = raw;
            }
            let role = if in_deg >= out_deg.saturating_mul(2) && in_deg >= 2 {
                "Core Provider"
            } else if out_deg >= in_deg.saturating_mul(2) && out_deg >= 2 {
                "Orchestrator"
            } else {
                "Balanced Hub"
            };
            let rel_path = if !trimmed_root.is_empty() {
                hub.object
                    .path
                    .strip_prefix(trimmed_root)
                    .map(|r| r.trim_start_matches(['/', '\\']).to_string())
                    .unwrap_or_else(|| hub.object.path.clone())
            } else {
                hub.object.path.clone()
            };

            scored.push((
                raw,
                HubImportance {
                    path: rel_path,
                    title: hub.object.title.clone(),
                    out_degree: out_deg,
                    in_degree: in_deg,
                    total_relations: total,
                    hub_score: 0.0,
                    role: role.to_string(),
                },
            ));
        }

        if max_raw > 0.0 {
            for (raw, item) in &mut scored {
                item.hub_score = (*raw / max_raw).clamp(0.0, 1.0);
            }
        }

        scored.sort_by(|a, b| {
            b.1.hub_score
                .total_cmp(&a.1.hub_score)
                .then_with(|| b.1.total_relations.cmp(&a.1.total_relations))
                .then_with(|| a.1.path.cmp(&b.1.path))
        });

        let capped = limit.clamp(1, 50);
        Ok(scored.into_iter().take(capped).map(|(_, h)| h).collect())
    }

    /// Evaluates deterministic documentation quality (`0..=100`) without LLMs using size, links, and path refs.
    pub fn experimental_doc_quality(
        &self,
        project_name: &str,
    ) -> Result<DocQualityReport, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;
        let root_prefix = project.root_path.as_deref().unwrap_or("");
        let trimmed_root = root_prefix.trim_end_matches(['/', '\\']);

        let mut doc_stmt = self.conn.prepare(
            "SELECT id, path, title, size_bytes
             FROM objects
             WHERE project_id = ?1
               AND is_deleted = 0
               AND is_noise = 0
               AND kind IN ('note', 'doc')",
        )?;
        let doc_rows = doc_stmt
            .query_map(params![project.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?.max(0) as usize,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut link_stmt = self.conn.prepare(
            "SELECT r.from_id, r.relation_type
             FROM relations r
             JOIN objects o_from ON o_from.id = r.from_id
             JOIN objects o_to ON o_to.id = r.to_id
             WHERE o_from.project_id = ?1
               AND o_to.project_id = ?1
               AND o_from.is_deleted = 0
               AND o_to.is_deleted = 0
               AND o_from.kind IN ('note', 'doc')
               AND r.relation_type IN ('md_link', 'path_ref')",
        )?;
        let link_rows = link_stmt
            .query_map(params![project.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut ref_counts: HashMap<String, (usize, usize)> = HashMap::new();
        for (from_id, rel_str) in link_rows {
            let entry = ref_counts.entry(from_id).or_insert((0, 0));
            match RelationType::from_db(&rel_str) {
                RelationType::MdLink => entry.0 += 1,
                RelationType::PathRef => entry.1 += 1,
                _ => {}
            }
        }

        let mut docs = Vec::with_capacity(doc_rows.len());
        let mut score_sum = 0_usize;
        let mut rich_count = 0_usize;
        let mut solid_count = 0_usize;
        let mut stub_count = 0_usize;

        for (id, path, title, size_bytes) in doc_rows {
            let (md_links, path_refs) = ref_counts.get(&id).copied().unwrap_or((0, 0));
            let substance_pts = (size_bytes / 150).min(40);
            let md_pts = (md_links * 10).min(30);
            let ref_pts = (path_refs * 5).min(20);
            let anchor_pts = if is_canonical_doc_title(&title) { 10 } else { 0 };
            let quality_score = (substance_pts + md_pts + ref_pts + anchor_pts).min(100);

            let tier = if quality_score >= 70 {
                rich_count += 1;
                DocQualityTier::Rich
            } else if quality_score >= 35 {
                solid_count += 1;
                DocQualityTier::Solid
            } else {
                stub_count += 1;
                DocQualityTier::Stub
            };
            score_sum += quality_score;

            let rel_path = if !trimmed_root.is_empty() {
                path.strip_prefix(trimmed_root)
                    .map(|r| r.trim_start_matches(['/', '\\']).to_string())
                    .unwrap_or(path)
            } else {
                path
            };

            docs.push(DocQualityEntry {
                path: rel_path,
                title,
                quality_score,
                tier,
                md_links,
                path_refs,
                size_bytes,
            });
        }

        docs.sort_by(|a, b| {
            b.quality_score
                .cmp(&a.quality_score)
                .then_with(|| b.md_links.cmp(&a.md_links))
                .then_with(|| a.path.cmp(&b.path))
        });

        let total_docs = docs.len();
        let mean_score = if total_docs == 0 {
            0.0
        } else {
            score_sum as f64 / total_docs as f64
        };

        Ok(DocQualityReport {
            project_name: project.name,
            total_docs,
            mean_score,
            rich_count,
            solid_count,
            stub_count,
            docs,
        })
    }
    /// Computes deterministic reverse-dependency blast radius for an object (`vos why --experimental`).
    pub fn experimental_why_impact(&self, object_id: &str) -> Result<ObjectImpact, StoreError> {
        let (obj_path, project_name, root_path): (String, Option<String>, Option<String>) = self
            .conn
            .query_row(
                "SELECT o.path, p.name, p.root_path
                 FROM objects o
                 LEFT JOIN projects p ON p.id = o.project_id
                 WHERE o.id = ?1 AND o.is_deleted = 0",
                params![object_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => {
                    StoreError::NotFound("object", object_id.to_string())
                }
                other => StoreError::from(other),
            })?;

        let trimmed_root = root_path.as_deref().unwrap_or("").trim_end_matches(['/', '\\']);
        let normalize = |p: &str| -> String {
            if !trimmed_root.is_empty() {
                p.strip_prefix(trimmed_root)
                    .map(|r| r.trim_start_matches(['/', '\\']).to_string())
                    .unwrap_or_else(|| p.to_string())
            } else {
                p.to_string()
            }
        };
        let rel_path = normalize(&obj_path);

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(object_id.to_string());
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((object_id.to_string(), 0));

        let mut direct_set: HashSet<String> = HashSet::new();
        let mut ordered_dependents: Vec<(usize, String)> = Vec::new();

        while let Some((curr_id, depth)) = queue.pop_front() {
            let incoming = self.object_relations(&curr_id, GraphDirection::Incoming)?;
            let mut layer_next: Vec<(String, String)> = incoming
                .into_iter()
                .map(|edge| (edge.from_object_id, normalize(&edge.from_path)))
                .collect();
            layer_next.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

            for (dep_id, dep_path) in layer_next {
                if depth == 0 && dep_id != object_id {
                    direct_set.insert(dep_id.clone());
                }
                if visited.insert(dep_id.clone()) {
                    let next_depth = depth + 1;
                    ordered_dependents.push((next_depth, dep_path));
                    queue.push_back((dep_id, next_depth));
                }
            }
        }

        let direct_dependents = direct_set.len();
        let transitive_dependents = ordered_dependents.len();
        ordered_dependents.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let sample_dependents = ordered_dependents
            .into_iter()
            .take(8)
            .map(|(_, p)| p)
            .collect();

        let (hub_score, role) = if let Some(proj_name) = project_name.as_deref() {
            let hubs = self.experimental_hub_importance(proj_name, 50)?;
            if let Some(hub) = hubs.into_iter().find(|h| h.path == rel_path) {
                (Some(hub.hub_score), Some(hub.role))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        Ok(ObjectImpact {
            path: rel_path,
            hub_score,
            role,
            direct_dependents,
            transitive_dependents,
            sample_dependents,
        })
    }
}

fn is_canonical_doc_title(title: &str) -> bool {    let lower = title.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "readme.md"
            | "spec.md"
            | "architecture.md"
            | "roadmap.md"
            | "contributing.md"
            | "whitepaper.md"
            | "design_manifesto.md"
    )
}

pub(crate) fn predict_from_status_at(status: &ProjectStatus, now: i64) -> RecoveryPrediction {
    let mut score = 0_usize;
    let mut factors = Vec::new();

    let (inactivity_pts, inactivity_label) = match status.last_change_at {
        Some(ts) => {
            let days = now.saturating_sub(ts).max(0) / 86_400;
            if days <= 14 {
                (0, format!("Recent activity ({days} days ago) (+0)"))
            } else if days <= 60 {
                (2, format!("Warm inactivity ({days} days ago) (+2)"))
            } else {
                (4, format!("Dormant {days} days (+4)"))
            }
        }
        None => (4, "No recorded activity (+4)".to_string()),
    };
    score += inactivity_pts;
    factors.push(inactivity_label);

    let base_cov_pts: f64 = match status.documentation_health {
        DocumentationHealth::Good => 0.0,
        DocumentationHealth::Partial => 1.0,
        DocumentationHealth::Low => 3.0,
        DocumentationHealth::Empty => 5.0,
    };
    let size_factor: f64 = if status.active_objects < 20 {
        0.25
    } else if status.active_objects < 100 {
        0.75
    } else {
        1.0
    };
    let cov_pts = (base_cov_pts * size_factor).round() as usize;
    score += cov_pts;
    factors.push(format!(
        "Coverage {} x{size_factor:.2} (+{cov_pts})",
        status.documentation_health
    ));

    let risk_pts = status.critical_risk_count * 2;
    score += risk_pts;
    factors.push(format!(
        "Critical Risks: {} (+{risk_pts})",
        status.critical_risk_count
    ));

    let q_pts = status.open_questions.len();
    score += q_pts;
    factors.push(format!("Open Questions: {q_pts} (+{q_pts})"));

    let (size_pts, size_tier) = if status.active_objects < 20 {
        (0, "Small")
    } else if status.active_objects < 100 {
        (1, "Medium")
    } else if status.active_objects < 500 {
        (2, "Large")
    } else {
        (3, "Very Large")
    };
    score += size_pts;
    factors.push(format!(
        "Project Size: {size_tier} ({} files) (+{size_pts})",
        status.active_objects
    ));

    let difficulty = if score <= 3 {
        RecoveryDifficulty::Easy
    } else if score <= 8 {
        RecoveryDifficulty::Medium
    } else {
        RecoveryDifficulty::Hard
    };

    RecoveryPrediction {
        difficulty,
        score,
        factors,
    }
}

#[cfg(test)]
mod tests {
    use vos_core::{DocumentationHealth, Project, ProjectHealth, ProjectStatus, RecoveryDifficulty};

    use super::*;

    fn synthetic_status(
        days_ago: Option<i64>,
        doc_health: DocumentationHealth,
        critical_risk_count: usize,
        open_questions: usize,
        active_objects: usize,
    ) -> (ProjectStatus, i64) {
        let now = 1_800_000_000_i64;
        let last_change_at = days_ago.map(|d| now - d * 86_400);
        let questions = (0..open_questions).map(|i| format!("Q{i}")).collect();
        (
            ProjectStatus {
                project: Project {
                    id: "proj_1".to_string(),
                    name: "sample".to_string(),
                    root_path: None,
                    description: None,
                    created_at: now,
                    updated_at: now,
                },
                health: ProjectHealth::Active,
                last_change_at,
                active_objects,
                deleted_objects: 0,
                key_documents: Vec::new(),
                open_questions: questions,
                pending_work: Vec::new(),
                active_areas: Vec::new(),
                coverage_ratio: 10.0,
                documentation_health: doc_health,
                critical_risk_count,
                high_risk_count: 0,
                critical_areas: Vec::new(),
                dependency_hubs: Vec::new(),
                risk_areas: Vec::new(),
            },
            now,
        )
    }

    #[test]
    fn predicts_easy_medium_and_hard_recovery_deterministically() {
        let (easy_st, now) = synthetic_status(Some(2), DocumentationHealth::Good, 0, 0, 12);
        let easy = predict_from_status_at(&easy_st, now);
        assert_eq!(easy.difficulty, RecoveryDifficulty::Easy);
        assert_eq!(easy.score, 0);

        let (med_st, _) = synthetic_status(Some(20), DocumentationHealth::Partial, 1, 1, 50);
        let med = predict_from_status_at(&med_st, now);
        assert_eq!(med.difficulty, RecoveryDifficulty::Medium);
        assert_eq!(med.score, 2 + 1 + 2 + 1 + 1);

        let (hard_st, _) = synthetic_status(Some(94), DocumentationHealth::Partial, 2, 3, 50);
        let hard = predict_from_status_at(&hard_st, now);
        assert_eq!(hard.difficulty, RecoveryDifficulty::Hard);
        assert_eq!(hard.score, 4 + 1 + 4 + 3 + 1);
        assert!(hard.factors.iter().any(|f| f.contains("Dormant 94 days")));

        // Micro-repo (<20 files) with Empty coverage scales down (`5 * 0.25 -> 1`).
        let (micro_st, _) = synthetic_status(Some(15), DocumentationHealth::Empty, 0, 0, 2);
        let micro = predict_from_status_at(&micro_st, now);
        assert_eq!(micro.difficulty, RecoveryDifficulty::Easy);
        assert_eq!(micro.score, 2 + 1);
    }

    #[test]
    fn experimental_hub_importance_and_doc_quality_score_deterministically() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("lab", None, None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();

        let id_spec = batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "SPEC.md".to_string(),
                kind: vos_core::ObjectKind::Note,
                title: "SPEC.md".to_string(),
                summary: Some("Spec".to_string()),
                content_hash: "hs".to_string(),
                size_bytes: 6000,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["Spec content".to_string()],
            })
            .unwrap()
            .object_id()
            .to_string();
        let id_stub = batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "notes/stub.md".to_string(),
                kind: vos_core::ObjectKind::Note,
                title: "stub.md".to_string(),
                summary: Some("Stub".to_string()),
                content_hash: "hst".to_string(),
                size_bytes: 50,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["Stub".to_string()],
            })
            .unwrap()
            .object_id()
            .to_string();
        let id_core = batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "src/core.rs".to_string(),
                kind: vos_core::ObjectKind::Code,
                title: "core.rs".to_string(),
                summary: Some("Core".to_string()),
                content_hash: "hc".to_string(),
                size_bytes: 200,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["pub fn core() {}".to_string()],
            })
            .unwrap()
            .object_id()
            .to_string();
        let id_api = batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "src/api.rs".to_string(),
                kind: vos_core::ObjectKind::Code,
                title: "api.rs".to_string(),
                summary: Some("Api".to_string()),
                content_hash: "ha".to_string(),
                size_bytes: 200,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["use crate::core;".to_string()],
            })
            .unwrap()
            .object_id()
            .to_string();

        batch
            .sync_relations(
                &[id_spec.clone(), id_api.clone()],
                &[
                    crate::RelationInput {
                        from_object_id: id_spec.clone(),
                        to_object_id: id_core.clone(),
                        relation_type: RelationType::MdLink,
                        confidence: 0.95,
                        detail: None,
                    },
                    crate::RelationInput {
                        from_object_id: id_spec.clone(),
                        to_object_id: id_api.clone(),
                        relation_type: RelationType::PathRef,
                        confidence: 0.75,
                        detail: None,
                    },
                    crate::RelationInput {
                        from_object_id: id_api.clone(),
                        to_object_id: id_core.clone(),
                        relation_type: RelationType::RustUse,
                        confidence: 0.90,
                        detail: None,
                    },
                ],
            )
            .unwrap();
        batch.commit().unwrap();

        let hubs = store.experimental_hub_importance("lab", 5).unwrap();
        assert_eq!(hubs[0].path, "src/core.rs");
        assert!((hubs[0].hub_score - 1.0).abs() < 1e-6);
        assert_eq!(hubs[0].role, "Core Provider");

        let dq = store.experimental_doc_quality("lab").unwrap();
        assert_eq!(dq.total_docs, 2);
        assert_eq!(dq.docs[0].title, "SPEC.md");
        assert!(dq.docs[0].quality_score >= 65);
        assert_eq!(dq.docs[1].title, "stub.md");
        assert_eq!(dq.docs[1].tier, DocQualityTier::Stub);
        let _ = id_stub;
    }

    #[test]
    fn experimental_why_impact_computes_transitive_blast_radius_and_cycles() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("blast", None, None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();

        let mk = |path: &str, title: &str, kind: vos_core::ObjectKind, hash: &str| crate::UpsertObjectInput {
            project_id: Some(proj.id.clone()),
            path: path.to_string(),
            kind,
            title: title.to_string(),
            summary: None,
            content_hash: hash.to_string(),
            size_bytes: 120,
            modified_at: 100,
            is_noise: false,
            chunks: vec![format!("content {path}")],
        };

        let id_core = batch.upsert_object(mk("src/core.rs", "core.rs", vos_core::ObjectKind::Code, "h1")).unwrap().object_id().to_string();
        let id_api = batch.upsert_object(mk("src/api.rs", "api.rs", vos_core::ObjectKind::Code, "h2")).unwrap().object_id().to_string();
        let id_cli = batch.upsert_object(mk("src/cli.rs", "cli.rs", vos_core::ObjectKind::Code, "h3")).unwrap().object_id().to_string();
        let id_spec = batch.upsert_object(mk("SPEC.md", "SPEC.md", vos_core::ObjectKind::Note, "h4")).unwrap().object_id().to_string();

        batch.sync_relations(
            &[id_core.clone(), id_api.clone(), id_cli.clone(), id_spec.clone()],
            &[
                crate::RelationInput {
                    from_object_id: id_api.clone(),
                    to_object_id: id_core.clone(),
                    relation_type: RelationType::RustUse,
                    confidence: 0.9,
                    detail: None,
                },
                crate::RelationInput {
                    from_object_id: id_spec.clone(),
                    to_object_id: id_core.clone(),
                    relation_type: RelationType::MdLink,
                    confidence: 0.95,
                    detail: None,
                },
                crate::RelationInput {
                    from_object_id: id_cli.clone(),
                    to_object_id: id_api.clone(),
                    relation_type: RelationType::RustUse,
                    confidence: 0.9,
                    detail: None,
                },
                crate::RelationInput {
                    from_object_id: id_api.clone(),
                    to_object_id: id_cli.clone(),
                    relation_type: RelationType::PathRef,
                    confidence: 0.7,
                    detail: None,
                },
            ],
        ).unwrap();
        batch.commit().unwrap();

        let impact = store.experimental_why_impact(&id_core).unwrap();
        assert_eq!(impact.path, "src/core.rs");
        assert_eq!(impact.direct_dependents, 2);
        assert_eq!(impact.transitive_dependents, 3);
        assert_eq!(impact.sample_dependents, vec!["SPEC.md", "src/api.rs", "src/cli.rs"]);
        assert!(impact.hub_score.is_some());
        assert_eq!(impact.role.as_deref(), Some("Core Provider"));
    }
}