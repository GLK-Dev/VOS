//! Token-efficient compact JSON projections shared between `vos --compact --json` and `vos mcp`.

use serde::Serialize;

use crate::models::{
    ActiveArea, AgentHandoff, AreaCoverage, AreaLease, AreaOwnership, AreaRisk, ContinueSuggestion, DocumentationHealth,
    EventRecord, EventType, GitContext, GraphDirection, GraphEdge, GraphHub, GraphTreeNode, HubImportance,
    ObjectImpact, ObjectKind, ObjectRecord, ObjectWhy, ProjectContinue, ProjectGraph,
    ProjectHealth, ProjectStatus, QueryIntent, RecoveryPrediction, RelatedHit, RelationType,
    SearchHit, SearchSignal, WorkSession,
};

const fn is_zero(v: &usize) -> bool {
    *v == 0
}

const fn is_false(v: &bool) -> bool {
    !*v
}

/// Strips a project root prefix from `full_path` for concise relative paths in agent outputs.
pub fn compact_path(full_path: &str, root_prefix: Option<&str>) -> String {
    let Some(root) = root_prefix.map(str::trim).filter(|r| !r.is_empty()) else {
        return full_path.to_string();
    };
    let norm_root = root.trim_end_matches(['/', '\\']);
    if let Some(rest) = full_path.strip_prefix(norm_root) {
        let stripped = rest.trim_start_matches(['/', '\\']);
        if stripped.is_empty() {
            ".".to_string()
        } else {
            stripped.replace('\\', "/")
        }
    } else {
        full_path.to_string()
    }
}

/// Normalizes whitespace and truncates text on a UTF-8 character boundary.
pub fn truncate_text(input: &str, max_chars: usize) -> String {
    let normalized: String = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let keep = max_chars.saturating_sub(3);
    let prefix: String = normalized.chars().take(keep).collect();
    format!("{prefix}...")
}

/// Minimal file reference omitting hashes, UUIDs, and verbose summaries.
#[derive(Debug, Clone, Serialize)]
pub struct CompactObjectRef {
    pub path: String,
    pub title: String,
    pub kind: ObjectKind,
    pub modified_at: i64,
    #[serde(skip_serializing_if = "is_false")]
    pub is_noise: bool,
}

impl CompactObjectRef {
    pub fn from_object(obj: &ObjectRecord, root_prefix: Option<&str>) -> Self {
        Self {
            path: compact_path(&obj.path, root_prefix),
            title: obj.title.clone(),
            kind: obj.kind,
            modified_at: obj.modified_at,
            is_noise: obj.is_noise,
        }
    }
}

/// Compact continuation recommendation entry.
#[derive(Debug, Clone, Serialize)]
pub struct CompactContinueSuggestion {
    pub path: String,
    pub title: String,
    pub kind: ObjectKind,
    pub reason: String,
}

impl CompactContinueSuggestion {
    pub fn from_suggestion(item: &ContinueSuggestion, root_prefix: Option<&str>) -> Self {
        Self {
            path: compact_path(&item.object.path, root_prefix),
            title: item.object.title.clone(),
            kind: item.object.kind,
            reason: item.reason.clone(),
        }
    }
}

/// Compact timeline event entry without redundant project/object IDs.
#[derive(Debug, Clone, Serialize)]
pub struct CompactEventRef {
    pub event_type: EventType,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub created_at: i64,
}

impl CompactEventRef {
    pub fn from_event(ev: &EventRecord, root_prefix: Option<&str>) -> Self {
        Self {
            event_type: ev.event_type,
            path: compact_path(&ev.path, root_prefix),
            detail: ev.detail.clone(),
            created_at: ev.created_at,
        }
    }
}

/// Hub summary for `continue` without recursive dependency trees.
#[derive(Debug, Clone, Serialize)]
pub struct CompactContinueHub {
    pub path: String,
    pub title: String,
    pub kind: ObjectKind,
    pub outgoing_count: usize,
    pub incoming_count: usize,
}

impl CompactContinueHub {
    pub fn from_hub(hub: &GraphHub, root_prefix: Option<&str>) -> Self {
        Self {
            path: compact_path(&hub.object.path, root_prefix),
            title: hub.object.title.clone(),
            kind: hub.object.kind,
            outgoing_count: hub.outgoing_count,
            incoming_count: hub.incoming_count,
        }
    }
}

/// Compact resume section nested inside `CompactContinue`.
#[derive(Debug, Clone, Serialize)]
pub struct CompactResume {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    pub last_activity_at: Option<i64>,
    pub active_objects: usize,
    pub recently_modified: Vec<CompactObjectRef>,
    pub recent_additions: Vec<CompactObjectRef>,
    pub most_active_areas: Vec<ActiveArea>,
    pub starting_points: Vec<CompactObjectRef>,
}

/// Token-efficient `vos continue` projection for AI agents.
#[derive(Debug, Clone, Serialize)]
pub struct CompactContinue {
    pub resume: CompactResume,
    pub continue_from: Vec<CompactContinueSuggestion>,
    pub recent_events: Vec<CompactEventRef>,
    pub open_questions: Vec<String>,
    pub ownership_areas: Vec<AreaOwnership>,
    pub coverage_warnings: Vec<AreaCoverage>,
    pub dependency_hubs: Vec<CompactContinueHub>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_session: Option<WorkSession>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitContext>,
    #[serde(skip_serializing_if = "is_zero")]
    pub stale_facts_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub active_leases: Vec<AreaLease>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_handoff: Option<AgentHandoff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_prediction: Option<RecoveryPrediction>,
}

impl CompactContinue {
    pub fn from_continue(
        cont: &ProjectContinue,
        recovery_prediction: Option<RecoveryPrediction>,
    ) -> Self {
        let root = cont.resume.project.root_path.as_deref();
        Self {
            resume: CompactResume {
                project: cont.resume.project.name.clone(),
                root_path: cont.resume.project.root_path.clone(),
                last_activity_at: cont.resume.last_activity_at,
                active_objects: cont.resume.active_objects,
                recently_modified: cont
                    .resume
                    .recently_modified
                    .iter()
                    .map(|o| CompactObjectRef::from_object(o, root))
                    .collect(),
                recent_additions: cont
                    .resume
                    .recent_additions
                    .iter()
                    .map(|o| CompactObjectRef::from_object(o, root))
                    .collect(),
                most_active_areas: cont.resume.most_active_areas.clone(),
                starting_points: cont
                    .resume
                    .starting_points
                    .iter()
                    .map(|o| CompactObjectRef::from_object(o, root))
                    .collect(),
            },
            continue_from: cont
                .continue_from
                .iter()
                .map(|c| CompactContinueSuggestion::from_suggestion(c, root))
                .collect(),
            recent_events: cont
                .recent_events
                .iter()
                .map(|e| CompactEventRef::from_event(e, root))
                .collect(),
            open_questions: cont.open_questions.clone(),
            ownership_areas: cont.ownership_areas.clone(),
            coverage_warnings: cont.coverage_warnings.clone(),
            dependency_hubs: cont
                .dependency_hubs
                .iter()
                .map(|h| CompactContinueHub::from_hub(h, root))
                .collect(),
            last_session: cont.last_session.clone(),
            git: cont.git.clone(),
            stale_facts_count: cont.stale_facts_count,
            active_leases: cont.active_leases.clone(),
            latest_handoff: cont.latest_handoff.clone(),
            recovery_prediction,
        }
    }
}

/// Token-efficient `vos status` projection for AI agents.
#[derive(Debug, Clone, Serialize)]
pub struct CompactStatus {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    pub health: ProjectHealth,
    pub last_change_at: Option<i64>,
    pub active_objects: usize,
    pub deleted_objects: usize,
    pub key_documents: Vec<CompactObjectRef>,
    pub open_questions: Vec<String>,
    pub pending_work: Vec<String>,
    pub active_areas: Vec<ActiveArea>,
    pub coverage_ratio: f64,
    pub documentation_health: DocumentationHealth,
    pub critical_risk_count: usize,
    pub high_risk_count: usize,
    pub critical_areas: Vec<AreaRisk>,
    pub dependency_hubs: Vec<String>,
    pub risk_areas: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitContext>,
    #[serde(skip_serializing_if = "is_zero")]
    pub stale_facts_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub active_leases: Vec<AreaLease>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_handoff: Option<AgentHandoff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_prediction: Option<RecoveryPrediction>,
}

impl CompactStatus {
    pub fn from_status(
        st: &ProjectStatus,
        recovery_prediction: Option<RecoveryPrediction>,
    ) -> Self {
        let root = st.project.root_path.as_deref();
        Self {
            project: st.project.name.clone(),
            root_path: st.project.root_path.clone(),
            health: st.health,
            last_change_at: st.last_change_at,
            active_objects: st.active_objects,
            deleted_objects: st.deleted_objects,
            key_documents: st
                .key_documents
                .iter()
                .map(|o| CompactObjectRef::from_object(o, root))
                .collect(),
            open_questions: st.open_questions.clone(),
            pending_work: st.pending_work.clone(),
            active_areas: st.active_areas.clone(),
            coverage_ratio: st.coverage_ratio,
            documentation_health: st.documentation_health,
            critical_risk_count: st.critical_risk_count,
            high_risk_count: st.high_risk_count,
            critical_areas: st.critical_areas.clone(),
            dependency_hubs: st.dependency_hubs.clone(),
            risk_areas: st.risk_areas.clone(),
            git: st.git.clone(),
            stale_facts_count: st.stale_facts_count,
            active_leases: st.active_leases.clone(),
            latest_handoff: st.latest_handoff.clone(),
            recovery_prediction,
        }
    }
}

/// Compact related-object entry nested inside `CompactWhy`.
#[derive(Debug, Clone, Serialize)]
pub struct CompactRelatedHit {
    pub title: String,
    pub path: String,
    pub kind: ObjectKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    pub score: i64,
    pub reasons: Vec<String>,
}

impl CompactRelatedHit {
    pub fn from_hit(hit: &RelatedHit, root_prefix: Option<&str>) -> Self {
        Self {
            title: hit.title.clone(),
            path: compact_path(&hit.path, root_prefix),
            kind: hit.kind,
            project_name: hit.project_name.clone(),
            score: hit.score,
            reasons: hit.reasons.clone(),
        }
    }
}

/// Compact directed relation endpoint nested inside `CompactWhy`.
#[derive(Debug, Clone, Serialize)]
pub struct CompactEdge {
    pub path: String,
    pub title: String,
    pub relation_type: RelationType,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CompactEdge {
    pub fn from_outgoing(edge: &GraphEdge, root_prefix: Option<&str>) -> Self {
        Self {
            path: compact_path(&edge.to_path, root_prefix),
            title: edge.to_title.clone(),
            relation_type: edge.relation_type,
            confidence: edge.confidence,
            detail: edge.detail.clone(),
        }
    }

    pub fn from_incoming(edge: &GraphEdge, root_prefix: Option<&str>) -> Self {
        Self {
            path: compact_path(&edge.from_path, root_prefix),
            title: edge.from_title.clone(),
            relation_type: edge.relation_type,
            confidence: edge.confidence,
            detail: edge.detail.clone(),
        }
    }
}

/// Token-efficient `vos why` projection with bounded edge previews.
#[derive(Debug, Clone, Serialize)]
pub struct CompactWhy {
    pub object: CompactObjectRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    pub area: String,
    pub is_active_area: bool,
    pub is_starting_point: bool,
    pub tags: Vec<(String, String)>,
    pub related: Vec<CompactRelatedHit>,
    pub outgoing_total: usize,
    pub outgoing_relations: Vec<CompactEdge>,
    pub incoming_total: usize,
    pub incoming_relations: Vec<CompactEdge>,
    pub recent_events: Vec<CompactEventRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_impact: Option<ObjectImpact>,
}

impl CompactWhy {
    pub fn from_why(
        why: &ObjectWhy,
        relation_limit: usize,
        experimental_impact: Option<ObjectImpact>,
    ) -> Self {
        let root = why.project_root.as_deref();
        let cap = relation_limit.max(1);
        Self {
            object: CompactObjectRef::from_object(&why.object, root),
            project_name: why.project_name.clone(),
            area: why.area.clone(),
            is_active_area: why.is_active_area,
            is_starting_point: why.is_starting_point,
            tags: why.tags.clone(),
            related: why
                .related
                .iter()
                .map(|r| CompactRelatedHit::from_hit(r, root))
                .collect(),
            outgoing_total: why.outgoing_relations.len(),
            outgoing_relations: why
                .outgoing_relations
                .iter()
                .take(cap)
                .map(|e| CompactEdge::from_outgoing(e, root))
                .collect(),
            incoming_total: why.incoming_relations.len(),
            incoming_relations: why
                .incoming_relations
                .iter()
                .take(cap)
                .map(|e| CompactEdge::from_incoming(e, root))
                .collect(),
            recent_events: why
                .recent_events
                .iter()
                .map(|e| CompactEventRef::from_event(e, root))
                .collect(),
            experimental_impact,
        }
    }
}

/// Compact recursive dependency node omitting duplicated `ObjectRecord` blobs.
#[derive(Debug, Clone, Serialize)]
pub struct CompactGraphNode {
    pub path: String,
    pub title: String,
    pub kind: ObjectKind,
    pub relation_type: RelationType,
    pub direction: GraphDirection,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CompactGraphNode>,
}

impl CompactGraphNode {
    pub fn from_node(node: &GraphTreeNode, root_prefix: Option<&str>) -> Self {
        Self {
            path: compact_path(&node.object.path, root_prefix),
            title: node.object.title.clone(),
            kind: node.object.kind,
            relation_type: node.relation_type,
            direction: node.direction,
            confidence: node.confidence,
            children: node
                .children
                .iter()
                .map(|c| Self::from_node(c, root_prefix))
                .collect(),
        }
    }
}

/// Compact hub entry nested inside `CompactGraph`.
#[derive(Debug, Clone, Serialize)]
pub struct CompactGraphHub {
    pub path: String,
    pub title: String,
    pub kind: ObjectKind,
    pub outgoing_count: usize,
    pub incoming_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outgoing: Vec<CompactGraphNode>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub incoming: Vec<CompactGraphNode>,
}

impl CompactGraphHub {
    pub fn from_hub(hub: &GraphHub, root_prefix: Option<&str>) -> Self {
        Self {
            path: compact_path(&hub.object.path, root_prefix),
            title: hub.object.title.clone(),
            kind: hub.object.kind,
            outgoing_count: hub.outgoing_count,
            incoming_count: hub.incoming_count,
            outgoing: hub
                .outgoing
                .iter()
                .map(|n| CompactGraphNode::from_node(n, root_prefix))
                .collect(),
            incoming: hub
                .incoming
                .iter()
                .map(|n| CompactGraphNode::from_node(n, root_prefix))
                .collect(),
        }
    }
}

/// Token-efficient `vos graph` projection.
#[derive(Debug, Clone, Serialize)]
pub struct CompactGraph {
    pub project: String,
    pub total_relations: usize,
    pub connected_objects: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<CompactObjectRef>,
    pub direction: GraphDirection,
    pub depth: usize,
    pub hubs: Vec<CompactGraphHub>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub_importance: Option<Vec<HubImportance>>,
}

impl CompactGraph {
    pub fn from_graph(
        graph: &ProjectGraph,
        hub_importance: Option<Vec<HubImportance>>,
    ) -> Self {
        let root = graph.project.root_path.as_deref();
        Self {
            project: graph.project.name.clone(),
            total_relations: graph.total_relations,
            connected_objects: graph.connected_objects,
            target: graph
                .target
                .as_ref()
                .map(|o| CompactObjectRef::from_object(o, root)),
            direction: graph.direction,
            depth: graph.depth,
            hubs: graph
                .hubs
                .iter()
                .map(|h| CompactGraphHub::from_hub(h, root))
                .collect(),
            hub_importance,
        }
    }
}

/// Token-efficient `vos search` hit projection.
#[derive(Debug, Clone, Serialize)]
pub struct CompactSearchHit {
    pub title: String,
    pub path: String,
    pub kind: ObjectKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    pub snippet: String,
    pub score: f64,
    pub intent: QueryIntent,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<SearchSignal>,
}

impl CompactSearchHit {
    pub fn from_hit(
        hit: &SearchHit,
        root_prefix: Option<&str>,
        include_signals: bool,
    ) -> Self {
        Self {
            title: hit.title.clone(),
            path: compact_path(&hit.path, root_prefix),
            kind: hit.kind,
            project_name: hit.project_name.clone(),
            snippet: truncate_text(&hit.snippet, 160),
            score: hit.score,
            intent: hit.intent,
            signals: if include_signals {
                hit.signals.clone()
            } else {
                Vec::new()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_path_strips_windows_and_unix_prefixes() {
        assert_eq!(
            compact_path(r"E:\VSCode projects\Hey\crates\hey-core\src\lib.rs", Some(r"E:\VSCode projects\Hey")),
            "crates/hey-core/src/lib.rs"
        );
        assert_eq!(
            compact_path("/tmp/proj/src/main.rs", Some("/tmp/proj/")),
            "src/main.rs"
        );
        assert_eq!(compact_path("/tmp/other/a.rs", None), "/tmp/other/a.rs");
    }

    #[test]
    fn truncate_text_respects_utf8_boundaries() {
        let out = truncate_text("alpha   beta\n\ngamma delta", 12);
        assert_eq!(out, "alpha bet...");
        assert_eq!(truncate_text("привет мир", 20), "привет мир");
    }
}
