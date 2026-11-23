use std::collections::{HashMap, HashSet};

use rusqlite::params;
use vos_core::{
    GraphDirection, GraphEdge, GraphHub, GraphTreeNode, ObjectKind, ObjectRecord, ProjectGraph,
    RelationType,
};

use crate::queries::map_object_row;
use crate::{Store, StoreError};

/// Lightweight active object metadata returned for deterministic link resolution.
#[derive(Debug, Clone)]
pub struct ActiveObjectRef {
    pub id: String,
    pub path: String,
    pub kind: ObjectKind,
    pub title: String,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone)]
struct StoredEdge {
    from_id: String,
    to_id: String,
    relation_type: RelationType,
    confidence: f64,
    detail: Option<String>,
}

impl Store {
    /// Lists active non-noise object metadata (`id`, `path`, `kind`, `title`) for link resolution.
    pub fn list_active_object_paths(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<ActiveObjectRef>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, kind, title, project_id
             FROM objects
             WHERE is_deleted = 0
               AND is_noise = 0
               AND (?1 IS NULL OR project_id = ?1)",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            let kind_str: String = row.get(2)?;
            Ok(ActiveObjectRef {
                id: row.get(0)?,
                path: row.get(1)?,
                kind: ObjectKind::from_db(&kind_str),
                title: row.get(3)?,
                project_id: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
    }

    /// Returns direct outgoing and/or incoming deterministic edges for an active object.
    pub fn object_relations(
        &self,
        object_id: &str,
        direction: GraphDirection,
    ) -> Result<Vec<GraphEdge>, StoreError> {
        let where_clause = match direction {
            GraphDirection::Outgoing => "r.from_id = ?1",
            GraphDirection::Incoming => "r.to_id = ?1",
            GraphDirection::Both => "(r.from_id = ?1 OR r.to_id = ?1)",
        };

        let sql = format!(
            "SELECT r.from_id, r.to_id,
                    o_from.path, o_to.path,
                    o_from.title, o_to.title,
                    r.relation_type, r.confidence, r.detail
             FROM relations r
             JOIN objects o_from ON o_from.id = r.from_id
             JOIN objects o_to ON o_to.id = r.to_id
             WHERE {where_clause}
               AND o_from.is_deleted = 0
               AND o_to.is_deleted = 0
               AND o_from.is_noise = 0
               AND o_to.is_noise = 0
             ORDER BY r.confidence DESC, o_to.path ASC, o_from.path ASC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![object_id], |row| {
            let rel_str: String = row.get(6)?;
            Ok(GraphEdge {
                from_object_id: row.get(0)?,
                to_object_id: row.get(1)?,
                from_path: row.get(2)?,
                to_path: row.get(3)?,
                from_title: row.get(4)?,
                to_title: row.get(5)?,
                relation_type: RelationType::from_db(&rel_str),
                confidence: row.get(7)?,
                detail: row.get(8)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
    }

    /// Builds a deterministic dependency graph for a project (or focused on `target`).
    pub fn project_graph(
        &self,
        project_name: &str,
        target: Option<&str>,
        direction: GraphDirection,
        depth: usize,
        limit: usize,
    ) -> Result<ProjectGraph, StoreError> {
        let project = self
            .get_project_by_name(project_name)?
            .ok_or_else(|| StoreError::NotFound("project", project_name.to_string()))?;

        let mut obj_stmt = self.conn.prepare(
            "SELECT id, project_id, path, kind, title, summary, content_hash,
                    size_bytes, modified_at, indexed_at, is_deleted, deleted_at, is_noise
             FROM objects
             WHERE project_id = ?1
               AND is_deleted = 0
               AND is_noise = 0",
        )?;
        let object_rows = obj_stmt
            .query_map(params![project.id], map_object_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let objects_by_id: HashMap<String, ObjectRecord> = object_rows
            .into_iter()
            .map(|o| (o.id.clone(), o))
            .collect();

        let mut edge_stmt = self.conn.prepare(
            "SELECT r.from_id, r.to_id, r.relation_type, r.confidence, r.detail
             FROM relations r
             JOIN objects o_from ON o_from.id = r.from_id
             JOIN objects o_to ON o_to.id = r.to_id
             WHERE o_from.project_id = ?1
               AND o_to.project_id = ?1
               AND o_from.is_deleted = 0
               AND o_to.is_deleted = 0
               AND o_from.is_noise = 0
               AND o_to.is_noise = 0",
        )?;
        let edges = edge_stmt
            .query_map(params![project.id], |row| {
                let rel_str: String = row.get(2)?;
                Ok(StoredEdge {
                    from_id: row.get(0)?,
                    to_id: row.get(1)?,
                    relation_type: RelationType::from_db(&rel_str),
                    confidence: row.get(3)?,
                    detail: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let total_relations = edges.len();
        let mut connected_ids = HashSet::new();
        let mut outgoing_adj: HashMap<String, Vec<StoredEdge>> = HashMap::new();
        let mut incoming_adj: HashMap<String, Vec<StoredEdge>> = HashMap::new();

        for edge in &edges {
            connected_ids.insert(edge.from_id.clone());
            connected_ids.insert(edge.to_id.clone());
            outgoing_adj
                .entry(edge.from_id.clone())
                .or_default()
                .push(edge.clone());
            incoming_adj
                .entry(edge.to_id.clone())
                .or_default()
                .push(edge.clone());
        }

        for list in outgoing_adj.values_mut() {
            sort_edges(list, &objects_by_id, true);
        }
        for list in incoming_adj.values_mut() {
            sort_edges(list, &objects_by_id, false);
        }

        let max_depth = depth.clamp(1, 5);
        let max_hubs = limit.clamp(1, 25);

        let (target_obj, hubs) = if let Some(target_arg) = target {
            let details = self.show_object_scoped(target_arg, Some(&project.name))?;
            let root_obj = details.object;
            let hub = build_hub(
                &root_obj,
                &objects_by_id,
                &outgoing_adj,
                &incoming_adj,
                direction,
                max_depth,
            );
            (Some(root_obj), vec![hub])
        } else {
            let mut candidates: Vec<&ObjectRecord> = connected_ids
                .iter()
                .filter_map(|id| objects_by_id.get(id))
                .collect();

            candidates.sort_by(|a, b| {
                let a_out = outgoing_adj.get(&a.id).map_or(0, Vec::len);
                let a_in = incoming_adj.get(&a.id).map_or(0, Vec::len);
                let b_out = outgoing_adj.get(&b.id).map_or(0, Vec::len);
                let b_in = incoming_adj.get(&b.id).map_or(0, Vec::len);

                let a_deg = a_out + a_in;
                let b_deg = b_out + b_in;

                b_deg
                    .cmp(&a_deg)
                    .then_with(|| b_out.cmp(&a_out))
                    .then_with(|| b.kind.context_priority().cmp(&a.kind.context_priority()))
                    .then_with(|| a.path.cmp(&b.path))
            });

            let hubs = candidates
                .into_iter()
                .take(max_hubs)
                .map(|obj| {
                    build_hub(
                        obj,
                        &objects_by_id,
                        &outgoing_adj,
                        &incoming_adj,
                        direction,
                        max_depth,
                    )
                })
                .collect();

            (None, hubs)
        };

        Ok(ProjectGraph {
            project,
            total_relations,
            connected_objects: connected_ids.len(),
            target: target_obj,
            direction,
            depth: max_depth,
            hubs,
        })
    }
}

fn sort_edges(
    edges: &mut [StoredEdge],
    objects_by_id: &HashMap<String, ObjectRecord>,
    use_to_path: bool,
) {
    edges.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| {
                let pa = if use_to_path {
                    objects_by_id.get(&a.to_id).map_or("", |o| o.path.as_str())
                } else {
                    objects_by_id.get(&a.from_id).map_or("", |o| o.path.as_str())
                };
                let pb = if use_to_path {
                    objects_by_id.get(&b.to_id).map_or("", |o| o.path.as_str())
                } else {
                    objects_by_id.get(&b.from_id).map_or("", |o| o.path.as_str())
                };
                pa.cmp(pb)
            })
            .then_with(|| a.relation_type.as_str().cmp(b.relation_type.as_str()))
    });
}

fn build_hub(
    root: &ObjectRecord,
    objects_by_id: &HashMap<String, ObjectRecord>,
    outgoing_adj: &HashMap<String, Vec<StoredEdge>>,
    incoming_adj: &HashMap<String, Vec<StoredEdge>>,
    direction: GraphDirection,
    max_depth: usize,
) -> GraphHub {
    let outgoing_count = outgoing_adj.get(&root.id).map_or(0, Vec::len);
    let incoming_count = incoming_adj.get(&root.id).map_or(0, Vec::len);

    let outgoing = if matches!(direction, GraphDirection::Outgoing | GraphDirection::Both) {
        let mut visited = HashSet::from([root.id.clone()]);
        expand_tree(
            &root.id,
            GraphDirection::Outgoing,
            max_depth,
            objects_by_id,
            outgoing_adj,
            &mut visited,
        )
    } else {
        Vec::new()
    };

    let incoming = if matches!(direction, GraphDirection::Incoming | GraphDirection::Both) {
        let mut visited = HashSet::from([root.id.clone()]);
        expand_tree(
            &root.id,
            GraphDirection::Incoming,
            max_depth,
            objects_by_id,
            incoming_adj,
            &mut visited,
        )
    } else {
        Vec::new()
    };

    GraphHub {
        object: root.clone(),
        outgoing_count,
        incoming_count,
        outgoing,
        incoming,
    }
}

fn expand_tree(
    node_id: &str,
    direction: GraphDirection,
    remaining_depth: usize,
    objects_by_id: &HashMap<String, ObjectRecord>,
    adj: &HashMap<String, Vec<StoredEdge>>,
    visited: &mut HashSet<String>,
) -> Vec<GraphTreeNode> {
    if remaining_depth == 0 {
        return Vec::new();
    }

    let Some(edges) = adj.get(node_id) else {
        return Vec::new();
    };

    let mut nodes = Vec::new();
    for edge in edges {
        let next_id = if direction == GraphDirection::Outgoing {
            &edge.to_id
        } else {
            &edge.from_id
        };
        let Some(next_obj) = objects_by_id.get(next_id) else {
            continue;
        };

        let is_new = visited.insert(next_id.clone());
        let children = if is_new && remaining_depth > 1 {
            expand_tree(
                next_id,
                direction,
                remaining_depth - 1,
                objects_by_id,
                adj,
                visited,
            )
        } else {
            Vec::new()
        };
        if is_new {
            visited.remove(next_id);
        }

        nodes.push(GraphTreeNode {
            object: next_obj.clone(),
            relation_type: edge.relation_type,
            confidence: edge.confidence,
            detail: edge.detail.clone(),
            direction,
            children,
        });
    }

    nodes
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use vos_core::{GraphDirection, ObjectKind, RelationType};

    use super::*;
    use crate::{RelationInput, UpsertObjectInput};

    fn dummy_input(project_id: &str, path: &str, kind: ObjectKind, title: &str) -> UpsertObjectInput {
        UpsertObjectInput {
            project_id: Some(project_id.to_string()),
            path: path.to_string(),
            kind,
            title: title.to_string(),
            summary: Some(format!("summary for {title}")),
            content_hash: format!("hash_{title}"),
            size_bytes: 64,
            modified_at: 1_700_000_000,
            is_noise: false,
            chunks: vec![format!("content of {title}")],
        }
    }

    #[test]
    fn relations_sync_graph_traversal_cycles_and_cleanup_work() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("demo", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        let id_mod = batch
            .upsert_object(dummy_input(&proj.id, "/demo/src/transformer/mod.rs", ObjectKind::Code, "mod.rs"))
            .unwrap()
            .object_id()
            .to_string();
        let id_kv = batch
            .upsert_object(dummy_input(&proj.id, "/demo/src/transformer/kv_cache.rs", ObjectKind::Code, "kv_cache.rs"))
            .unwrap()
            .object_id()
            .to_string();
        let id_layer = batch
            .upsert_object(dummy_input(&proj.id, "/demo/src/transformer/layer.rs", ObjectKind::Code, "layer.rs"))
            .unwrap()
            .object_id()
            .to_string();

        // Include a bidirectional cycle (mod -> kv_cache -> layer -> mod) to verify cycle safety.
        let inserted = batch
            .sync_relations(
                &[id_mod.clone(), id_kv.clone(), id_layer.clone()],
                &[
                    RelationInput {
                        from_object_id: id_mod.clone(),
                        to_object_id: id_kv.clone(),
                        relation_type: RelationType::RustMod,
                        confidence: 1.0,
                        detail: Some("mod kv_cache".to_string()),
                    },
                    RelationInput {
                        from_object_id: id_mod.clone(),
                        to_object_id: id_layer.clone(),
                        relation_type: RelationType::RustMod,
                        confidence: 1.0,
                        detail: Some("mod layer".to_string()),
                    },
                    RelationInput {
                        from_object_id: id_kv.clone(),
                        to_object_id: id_layer.clone(),
                        relation_type: RelationType::RustUse,
                        confidence: 0.9,
                        detail: Some("use super::layer".to_string()),
                    },
                    RelationInput {
                        from_object_id: id_layer.clone(),
                        to_object_id: id_mod.clone(),
                        relation_type: RelationType::RustUse,
                        confidence: 0.9,
                        detail: Some("use super".to_string()),
                    },
                ],
            )
            .unwrap();
        assert_eq!(inserted, 4);
        batch.commit().unwrap();

        let graph = store
            .project_graph("demo", Some("transformer/mod.rs"), GraphDirection::Both, 4, 5)
            .unwrap();
        assert_eq!(graph.total_relations, 4);
        assert_eq!(graph.connected_objects, 3);
        assert_eq!(graph.hubs[0].outgoing_count, 2);
        assert_eq!(graph.hubs[0].incoming_count, 1);

        let why = store.why_object("transformer/mod.rs", Some("demo")).unwrap();
        assert_eq!(why.outgoing_relations.len(), 2);
        assert_eq!(why.incoming_relations.len(), 1);

        let related = store
            .find_related("transformer/mod.rs", Some("demo"), 5)
            .unwrap();
        assert!(related.iter().any(|r| r.title == "kv_cache.rs" && r.reasons.iter().any(|s| s.contains("rust_mod"))));

        // Re-syncing with same data remains idempotent; deleting kv_cache removes stale edges.
        let mut del_batch = store.begin_index_batch().unwrap();
        let seen = HashSet::from([
            "/demo/src/transformer/mod.rs".to_string(),
            "/demo/src/transformer/layer.rs".to_string(),
        ]);
        let deleted = del_batch
            .mark_missing_deleted("/demo", Some(&proj.id), &seen)
            .unwrap();
        assert_eq!(deleted, 1);
        del_batch.commit().unwrap();

        let after_del = store
            .object_relations(&id_mod, GraphDirection::Outgoing)
            .unwrap();
        assert_eq!(after_del.len(), 1);
        assert_eq!(after_del[0].to_title, "layer.rs");
    }
}
