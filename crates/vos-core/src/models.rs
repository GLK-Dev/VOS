use serde::Deserialize;
use std::fmt;

/// Classification of an indexed filesystem object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Code,
    Note,
    Doc,
    Config,
    Model,
    Media,
    Other,
}

impl ObjectKind {
    /// Returns the canonical lowercase string stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Note => "note",
            Self::Doc => "doc",
            Self::Config => "config",
            Self::Model => "model",
            Self::Media => "media",
            Self::Other => "other",
        }
    }

    /// Parses a stored kind string, falling back to `Other` for unknown values.
    pub fn from_db(value: &str) -> Self {
        match value {
            "code" => Self::Code,
            "note" => Self::Note,
            "doc" => Self::Doc,
            "config" => Self::Config,
            "model" => Self::Model,
            "media" => Self::Media,
            _ => Self::Other,
        }
    }

    /// Returns true if text content should be chunked for full-text search.
    pub fn is_text_indexable(&self) -> bool {
        matches!(self, Self::Code | Self::Note | Self::Config)
    }

    /// Relative priority used to surface human-meaningful files in `vos context`.
    pub fn context_priority(&self) -> i64 {
        match self {
            Self::Note => 100,
            Self::Doc => 90,
            Self::Code => 80,
            Self::Model => 70,
            Self::Config => 50,
            Self::Media => 20,
            Self::Other => 10,
        }
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lifecycle event recorded for project timeline recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    Added,
    Modified,
    Deleted,
    Restored,
    Tagged,
}

impl EventType {
    /// Returns the canonical lowercase string stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Restored => "restored",
            Self::Tagged => "tagged",
        }
    }

    /// Parses a stored event type string.
    pub fn from_db(value: &str) -> Self {
        match value {
            "added" => Self::Added,
            "modified" => Self::Modified,
            "deleted" => Self::Deleted,
            "restored" => Self::Restored,
            "tagged" => Self::Tagged,
            _ => Self::Modified,
        }
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Top-level project grouping objects and timeline events.
#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: Option<String>,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Aggregated overview returned by `vos project list`.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub project: Project,
    pub active_objects: usize,
    pub deleted_objects: usize,
    pub last_indexed_at: Option<i64>,
}

/// Persisted file or artifact tracked by VOS.
#[derive(Debug, Clone)]
pub struct ObjectRecord {
    pub id: String,
    pub project_id: Option<String>,
    pub path: String,
    pub kind: ObjectKind,
    pub title: String,
    pub summary: Option<String>,
    pub content_hash: String,
    pub size_bytes: i64,
    pub modified_at: i64,
    pub indexed_at: i64,
    pub is_deleted: bool,
    pub deleted_at: Option<i64>,
    pub is_noise: bool,
}

/// Detailed view of a single object including tags and project metadata.
#[derive(Debug, Clone)]
pub struct ObjectDetails {
    pub object: ObjectRecord,
    pub project_name: Option<String>,
    pub tags: Vec<(String, String)>,
    pub chunk_count: usize,
}

/// Individual scoring contribution returned for `vos search --explain`.
#[derive(Debug, Clone)]
pub struct SearchSignal {
    pub label: String,
    pub delta: f64,
}

/// Ranked full-text search hit across `chunks_fts` and `objects_fts`.
/// Detected query intent used to weight title, phrase, and architectural document boosts.
/// Controls default ranking visibility for search infrastructure/config artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchVisibility {
    Normal,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    Code,
    Documentation,
    Concept,
}

impl QueryIntent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Documentation => "documentation",
            Self::Concept => "concept",
        }
    }
}

impl fmt::Display for QueryIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub object_id: String,
    pub project_name: Option<String>,
    pub title: String,
    pub path: String,
    pub kind: ObjectKind,
    pub is_noise: bool,
    pub snippet: String,
    pub score: f64,
    pub intent: QueryIntent,
    pub signals: Vec<SearchSignal>,
}

/// Timeline event entry for a project.
#[derive(Debug, Clone)]
pub struct EventRecord {
    pub id: i64,
    pub project_id: Option<String>,
    pub object_id: Option<String>,
    pub event_type: EventType,
    pub path: String,
    pub detail: Option<String>,
    pub created_at: i64,
}

/// Snapshot returned by `vos context <project>`.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub project: Project,
    pub active_objects: usize,
    pub deleted_objects: usize,
    pub noise_objects: usize,
    pub counts_by_kind: Vec<(String, usize)>,
    pub recent_objects: Vec<ObjectRecord>,
    pub noise_sample: Vec<ObjectRecord>,
    pub recent_events: Vec<EventRecord>,
}

/// Time-windowed project activity report for `vos activity <project>`.
#[derive(Debug, Clone)]
pub struct ProjectActivity {
    pub project: Project,
    pub days: u32,
    pub since_ts: i64,
    pub events: Vec<EventRecord>,
}

/// Aggregated subdirectory activity summary for `vos resume`.
#[derive(Debug, Clone)]
pub struct ActiveArea {
    pub area: String,
    pub object_count: usize,
    pub latest_modified_at: i64,
}

/// Structured context-recovery briefing for `vos resume <project>`.
#[derive(Debug, Clone)]
pub struct ProjectResume {
    pub project: Project,
    pub last_activity_at: Option<i64>,
    pub active_objects: usize,
    pub recently_modified: Vec<ObjectRecord>,
    pub recent_additions: Vec<ObjectRecord>,
    pub most_active_areas: Vec<ActiveArea>,
    pub starting_points: Vec<ObjectRecord>,
}

/// Deterministic relatedness hit returned by `vos related`.
#[derive(Debug, Clone)]
pub struct RelatedHit {
    pub object_id: String,
    pub project_name: Option<String>,
    pub title: String,
    pub path: String,
    pub kind: ObjectKind,
    pub score: i64,
    pub reasons: Vec<String>,
}

/// Explainable context report returned by `vos why <target>`.
#[derive(Debug, Clone)]
pub struct ObjectWhy {
    pub object: ObjectRecord,
    pub project_name: Option<String>,
    pub project_root: Option<String>,
    pub area: String,
    pub is_active_area: bool,
    pub is_starting_point: bool,
    pub tags: Vec<(String, String)>,
    pub related: Vec<RelatedHit>,
    pub outgoing_relations: Vec<GraphEdge>,
    pub incoming_relations: Vec<GraphEdge>,
    pub recent_events: Vec<EventRecord>,
}

/// Actionable entry point with a short rationale for `vos continue`.
#[derive(Debug, Clone)]
pub struct ContinueSuggestion {
    pub object: ObjectRecord,
    pub reason: String,
}

/// Combined resume + activity + explainable kickoff briefing for `vos continue <project>`.
#[derive(Debug, Clone)]
pub struct ProjectContinue {
    pub resume: ProjectResume,
    pub continue_from: Vec<ContinueSuggestion>,
    pub recent_events: Vec<EventRecord>,
    pub open_questions: Vec<String>,
    pub ownership_areas: Vec<AreaOwnership>,
    pub coverage_warnings: Vec<AreaCoverage>,
    pub dependency_hubs: Vec<GraphHub>,
}
/// Combined daily-launch briefing returned by os start <project>.
/// Combined daily-launch briefing returned by `vos start <project>`.
#[derive(Debug, Clone)]
pub struct ProjectStart {
    pub resume: ProjectResume,
    pub recent_events: Vec<EventRecord>,
    pub open_questions: Vec<String>,
}

/// Single search benchmark case loaded from YAML.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchBenchmarkCase {
    pub query: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub expected_top1: Vec<String>,
    #[serde(default)]
    pub expected_top3: Vec<String>,
    #[serde(default)]
    pub expected_top5: Vec<String>,
    #[serde(default)]
    pub expected_projects: Vec<String>,
}

/// Outcome of evaluating a single benchmark query.
#[derive(Debug, Clone)]
pub struct SearchBenchmarkCaseResult {
    pub query: String,
    pub category: String,
    pub project: Option<String>,
    pub top1_hit: Option<bool>,
    pub top3_hit: Option<bool>,
    pub top5_hit: Option<bool>,
    pub reciprocal_rank: Option<f64>,
    pub noise_hits: usize,
    pub returned_hits: usize,
    pub cross_project_matched: usize,
    pub cross_project_total: usize,
    pub actual_top5: Vec<String>,
}

/// Per-category summary inside a benchmark report.
#[derive(Debug, Clone)]
pub struct CategoryBenchmarkStats {
    pub category: String,
    pub cases: usize,
    pub top1_passed: usize,
    pub top1_total: usize,
    pub top3_passed: usize,
    pub top3_total: usize,
    pub top5_passed: usize,
    pub top5_total: usize,
}

/// Aggregate accuracy, MRR, noise rate, and cross-project recall for a search benchmark suite.
#[derive(Debug, Clone)]
pub struct SearchBenchmarkReport {
    pub total_cases: usize,
    pub top1_passed: usize,
    pub top1_total: usize,
    pub top3_passed: usize,
    pub top3_total: usize,
    pub top5_passed: usize,
    pub top5_total: usize,
    pub mrr: f64,
    pub noise_hits: usize,
    pub total_returned_hits: usize,
    pub cross_project_matched: usize,
    pub cross_project_total: usize,
    pub categories: Vec<CategoryBenchmarkStats>,
    pub cases: Vec<SearchBenchmarkCaseResult>,
}

/// Persisted benchmark summary row stored in `benchmark_runs`.
#[derive(Debug, Clone)]
pub struct BenchmarkRunRecord {
    pub id: i64,
    pub suite_name: String,
    pub total_cases: usize,
    pub top1_passed: usize,
    pub top1_total: usize,
    pub top3_passed: usize,
    pub top3_total: usize,
    pub top5_passed: usize,
    pub top5_total: usize,
    pub mrr: f64,
    pub noise_hits: usize,
    pub total_returned_hits: usize,
    pub cross_project_matched: usize,
    pub cross_project_total: usize,
    pub created_at: i64,
}







/// Deterministic project health classification for `vos status <project>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectHealth {
    Active,
    Warm,
    Dormant,
    Empty,
}

impl ProjectHealth {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Warm => "Warm",
            Self::Dormant => "Dormant",
            Self::Empty => "Empty",
        }
    }
}

impl fmt::Display for ProjectHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Decision-support health and signal summary returned by `vos status <project>`.
#[derive(Debug, Clone)]
pub struct ProjectStatus {
    pub project: Project,
    pub health: ProjectHealth,
    pub last_change_at: Option<i64>,
    pub active_objects: usize,
    pub deleted_objects: usize,
    pub key_documents: Vec<ObjectRecord>,
    pub open_questions: Vec<String>,
    pub pending_work: Vec<String>,
    pub active_areas: Vec<ActiveArea>,
    pub coverage_ratio: f64,
    pub documentation_health: DocumentationHealth,
    pub dependency_hubs: Vec<String>,
    pub risk_areas: Vec<String>,
}

/// Deterministic concept explanation assembled from project docs, code, and `config/concepts.yaml`.
#[derive(Debug, Clone)]
pub struct ConceptExplanation {
    pub concept: String,
    pub files_contributing: Vec<SearchHit>,
    pub definition: Option<String>,
    pub related_concepts: Vec<String>,
    pub relevant_code: Vec<SearchHit>,
}

/// Project memory diff since a date returned by `vos changed-since`.
#[derive(Debug, Clone)]
pub struct ChangedSinceReport {
    pub project_name: Option<String>,
    pub project_root: Option<String>,
    pub since_label: String,
    pub since_ts: i64,
    pub modified: Vec<ObjectRecord>,
    pub added: Vec<ObjectRecord>,
    pub deleted: Vec<ObjectRecord>,
}

/// Deterministic project evolution summary returned by `vos evolution <project>`.
#[derive(Debug, Clone)]
pub struct ProjectEvolution {
    pub project: Project,
    pub since_label: String,
    pub since_ts: i64,
    pub added_count: usize,
    pub modified_count: usize,
    pub deleted_count: usize,
    pub new_active_areas: Vec<String>,
    pub inactive_areas: Vec<String>,
    pub emerging_concepts: Vec<String>,
    pub documentation_coverage: String,
    pub open_questions_count: usize,
    pub pending_work_count: usize,
}

/// Single deterministic drift finding returned by `vos drift <project>`.
#[derive(Debug, Clone)]
pub struct DriftFinding {
    pub kind: String,
    pub summary: String,
    pub evidence: Vec<String>,
}

/// Project drift report comparing code activity, documentation freshness, and roadmap tasks.
#[derive(Debug, Clone)]
pub struct ProjectDrift {
    pub project: Project,
    pub findings: Vec<DriftFinding>,
}


/// Deterministic dependency edge type stored in `relations`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationType {
    RustMod,
    RustUse,
    MdLink,
    PathRef,
}

impl RelationType {
    /// Returns the canonical lowercase string stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RustMod => "rust_mod",
            Self::RustUse => "rust_use",
            Self::MdLink => "md_link",
            Self::PathRef => "path_ref",
        }
    }

    /// Parses a stored relation type string, falling back to `PathRef`.
    pub fn from_db(value: &str) -> Self {
        match value {
            "rust_mod" => Self::RustMod,
            "rust_use" => Self::RustUse,
            "md_link" => Self::MdLink,
            _ => Self::PathRef,
        }
    }

    /// Default deterministic confidence associated with each edge source.
    pub fn default_confidence(&self) -> f64 {
        match self {
            Self::RustMod => 1.0,
            Self::MdLink => 0.95,
            Self::RustUse => 0.90,
            Self::PathRef => 0.75,
        }
    }
}

impl fmt::Display for RelationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Traversal direction for `vos graph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDirection {
    Outgoing,
    Incoming,
    Both,
}

impl GraphDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Outgoing => "outgoing",
            Self::Incoming => "incoming",
            Self::Both => "both",
        }
    }
}

impl fmt::Display for GraphDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Directed deterministic dependency edge between two indexed objects.
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub from_object_id: String,
    pub to_object_id: String,
    pub from_path: String,
    pub to_path: String,
    pub from_title: String,
    pub to_title: String,
    pub relation_type: RelationType,
    pub confidence: f64,
    pub detail: Option<String>,
}

/// Recursive node in a dependency tree rendered by `vos graph`.
#[derive(Debug, Clone)]
pub struct GraphTreeNode {
    pub object: ObjectRecord,
    pub relation_type: RelationType,
    pub confidence: f64,
    pub detail: Option<String>,
    pub direction: GraphDirection,
    pub children: Vec<GraphTreeNode>,
}

/// Root or hub entry inside a `ProjectGraph` report.
#[derive(Debug, Clone)]
pub struct GraphHub {
    pub object: ObjectRecord,
    pub outgoing_count: usize,
    pub incoming_count: usize,
    pub outgoing: Vec<GraphTreeNode>,
    pub incoming: Vec<GraphTreeNode>,
}

/// Deterministic project dependency graph returned by `vos graph`.
#[derive(Debug, Clone)]
pub struct ProjectGraph {
    pub project: Project,
    pub total_relations: usize,
    pub connected_objects: usize,
    pub target: Option<ObjectRecord>,
    pub direction: GraphDirection,
    pub depth: usize,
    pub hubs: Vec<GraphHub>,
}
/// Activity ownership metrics for a single project directory/area (`vos ownership`).
#[derive(Debug, Clone)]
pub struct AreaOwnership {
    pub area: String,
    pub file_count: usize,
    pub event_count: usize,
    pub recent_modified_count: usize,
    pub activity_score: usize,
    pub latest_modified_at: i64,
}

/// Aggregated activity ownership summary returned by `vos ownership <project>`.
#[derive(Debug, Clone)]
pub struct ProjectOwnership {
    pub project: Project,
    pub days: u32,
    pub since_ts: i64,
    pub total_areas: usize,
    pub most_active_areas: Vec<AreaOwnership>,
    pub recently_active_areas: Vec<AreaOwnership>,
    pub most_stable_areas: Vec<AreaOwnership>,
}

/// Documentation coverage tier assigned to a project area (`vos coverage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageTier {
    WellCovered,
    PartiallyCovered,
    Underdocumented,
}

impl CoverageTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WellCovered => "well_covered",
            Self::PartiallyCovered => "partially_covered",
            Self::Underdocumented => "underdocumented",
        }
    }
}

impl fmt::Display for CoverageTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Documentation coverage metrics for a single project area (`vos coverage`).
#[derive(Debug, Clone)]
pub struct AreaCoverage {
    pub area: String,
    pub code_files: usize,
    pub local_docs: usize,
    pub md_link_refs: usize,
    pub path_ref_refs: usize,
    pub weighted_score: f64,
    pub tier: CoverageTier,
}

/// Relation-aware documentation coverage summary returned by `vos coverage <project>`.
#[derive(Debug, Clone)]
pub struct ProjectCoverage {
    pub project: Project,
    pub code_files: usize,
    pub doc_files: usize,
    pub coverage_ratio: f64,
    pub well_covered: Vec<AreaCoverage>,
    pub partially_covered: Vec<AreaCoverage>,
    pub underdocumented: Vec<AreaCoverage>,
}
/// Overall documentation health tier reported by `vos status <project>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentationHealth {
    Good,
    Partial,
    Low,
    Empty,
}

impl DocumentationHealth {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Good => "Good",
            Self::Partial => "Partial",
            Self::Low => "Low",
            Self::Empty => "Empty",
        }
    }
}

impl fmt::Display for DocumentationHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Risk tier for deterministic structural drift scoring (`vos drift`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    High,
    Medium,
    Low,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::High => "High Risk",
            Self::Medium => "Medium Risk",
            Self::Low => "Low Risk",
        }
    }
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}