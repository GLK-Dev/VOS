use serde::{Deserialize, Serialize};
use std::fmt;

/// Classification of an indexed filesystem object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: Option<String>,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Aggregated overview returned by `vos project list`.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummary {
    pub project: Project,
    pub active_objects: usize,
    pub deleted_objects: usize,
    pub last_indexed_at: Option<i64>,
}

/// Persisted file or artifact tracked by VOS.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct ObjectDetails {
    pub object: ObjectRecord,
    pub project_name: Option<String>,
    pub tags: Vec<(String, String)>,
    pub chunk_count: usize,
}

/// Individual scoring contribution returned for `vos search --explain`.
#[derive(Debug, Clone, Serialize)]
pub struct SearchSignal {
    pub label: String,
    pub delta: f64,
}

/// Ranked full-text search hit across `chunks_fts` and `objects_fts`.
/// Detected query intent used to weight title, phrase, and architectural document boosts.
/// Controls default ranking visibility for search infrastructure/config artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SearchVisibility {
    Normal,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct ProjectActivity {
    pub project: Project,
    pub days: u32,
    pub since_ts: i64,
    pub events: Vec<EventRecord>,
}

/// Aggregated subdirectory activity summary for `vos resume`.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveArea {
    pub area: String,
    pub object_count: usize,
    pub latest_modified_at: i64,
}

/// Structured context-recovery briefing for `vos resume <project>`.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct ContinueSuggestion {
    pub object: ObjectRecord,
    pub reason: String,
}

/// Combined resume + activity + explainable kickoff briefing for `vos continue <project>`.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectContinue {
    pub resume: ProjectResume,
    pub continue_from: Vec<ContinueSuggestion>,
    pub recent_events: Vec<EventRecord>,
    pub open_questions: Vec<String>,
    pub ownership_areas: Vec<AreaOwnership>,
    pub coverage_warnings: Vec<AreaCoverage>,
    pub dependency_hubs: Vec<GraphHub>,
    pub last_session: Option<WorkSession>,
}
/// Combined daily-launch briefing returned by `vos start <project>`.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectStart {
    pub resume: ProjectResume,
    pub recent_events: Vec<EventRecord>,
    pub open_questions: Vec<String>,
}

/// Single search benchmark case loaded from YAML.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

    pub fn from_db(value: &str) -> Self {
        match value {
            "Active" => Self::Active,
            "Warm" => Self::Warm,
            "Dormant" => Self::Dormant,
            _ => Self::Empty,
        }
    }
}

impl fmt::Display for ProjectHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Decision-support health and signal summary returned by `vos status <project>`.
#[derive(Debug, Clone, Serialize)]
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
    pub critical_risk_count: usize,
    pub high_risk_count: usize,
    pub critical_areas: Vec<AreaRisk>,
    pub dependency_hubs: Vec<String>,
    pub risk_areas: Vec<String>,
}

/// Deterministic concept explanation assembled from project docs, code, and `config/concepts.yaml`.
#[derive(Debug, Clone, Serialize)]
pub struct ConceptExplanation {
    pub concept: String,
    pub files_contributing: Vec<SearchHit>,
    pub definition: Option<String>,
    pub related_concepts: Vec<String>,
    pub relevant_code: Vec<SearchHit>,
}

/// Project memory diff since a date returned by `vos changed-since`.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct DriftFinding {
    pub kind: String,
    pub summary: String,
    pub evidence: Vec<String>,
}

/// Project drift report comparing code activity, documentation freshness, and roadmap tasks.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDrift {
    pub project: Project,
    pub findings: Vec<DriftFinding>,
}

/// Deterministic dependency edge type stored in `relations`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct GraphTreeNode {
    pub object: ObjectRecord,
    pub relation_type: RelationType,
    pub confidence: f64,
    pub detail: Option<String>,
    pub direction: GraphDirection,
    pub children: Vec<GraphTreeNode>,
}

/// Root or hub entry inside a `ProjectGraph` report.
#[derive(Debug, Clone, Serialize)]
pub struct GraphHub {
    pub object: ObjectRecord,
    pub outgoing_count: usize,
    pub incoming_count: usize,
    pub outgoing: Vec<GraphTreeNode>,
    pub incoming: Vec<GraphTreeNode>,
}

/// Deterministic project dependency graph returned by `vos graph`.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct AreaOwnership {
    pub area: String,
    pub file_count: usize,
    pub event_count: usize,
    pub recent_modified_count: usize,
    pub activity_score: usize,
    pub latest_modified_at: i64,
}

/// Aggregated activity ownership summary returned by `vos ownership <project>`.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

    pub fn from_db(value: &str) -> Self {
        match value {
            "Good" => Self::Good,
            "Partial" => Self::Partial,
            "Low" => Self::Low,
            _ => Self::Empty,
        }
    }
}

impl fmt::Display for DocumentationHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Risk tier for deterministic structural drift and status risk analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "Critical Risk",
            Self::High => "High Risk",
            Self::Medium => "Medium Risk",
            Self::Low => "Low Risk",
        }
    }
}

/// Combined Graph + Ownership + Coverage + TODO structural risk assessment for a project area.
#[derive(Debug, Clone, Serialize)]
pub struct AreaRisk {
    pub area: String,
    pub level: RiskLevel,
    pub risk_score: f64,
    pub relation_count: usize,
    pub recent_activity: usize,
    pub coverage_tier: CoverageTier,
    pub coverage_score: f64,
    pub code_files: usize,
    pub open_todos: usize,
    pub top_hub: Option<String>,
    pub reason: String,
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
/// Experimental predicted effort required to resume work in a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RecoveryDifficulty {
    Easy,
    Medium,
    Hard,
}

impl RecoveryDifficulty {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Easy => "Easy",
            Self::Medium => "Medium",
            Self::Hard => "Hard",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "Easy" => Self::Easy,
            "Hard" => Self::Hard,
            _ => Self::Medium,
        }
    }
}

impl fmt::Display for RecoveryDifficulty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Explainable deterministic prediction returned under `--experimental`.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryPrediction {
    pub difficulty: RecoveryDifficulty,
    pub score: usize,
    pub factors: Vec<String>,
}
/// Persisted point-in-time snapshot of project status and risks (`vos snapshot`).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSnapshot {
    pub id: i64,
    pub project_id: String,
    pub project_name: String,
    pub label: Option<String>,
    pub health: ProjectHealth,
    pub active_objects: usize,
    pub deleted_objects: usize,
    pub relations_count: usize,
    pub coverage_ratio: f64,
    pub documentation_health: DocumentationHealth,
    pub critical_risks: usize,
    pub high_risks: usize,
    pub open_questions: Vec<String>,
    pub pending_work: Vec<String>,
    pub active_areas: Vec<String>,
    pub risk_areas: Vec<String>,
    pub recent_changes: Vec<String>,
    pub created_at: i64,
}

/// Deterministic comparison between two saved snapshots (`vos compare-snapshot`).
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotDiff {
    pub from: ProjectSnapshot,
    pub to: ProjectSnapshot,
    pub active_delta: isize,
    pub deleted_delta: isize,
    pub relations_delta: isize,
    pub coverage_delta: f64,
    pub critical_risks_delta: isize,
    pub high_risks_delta: isize,
    pub added_pending_work: Vec<String>,
    pub resolved_pending_work: Vec<String>,
    pub added_risks: Vec<String>,
    pub resolved_risks: Vec<String>,
    pub new_active_areas: Vec<String>,
}

/// Persisted developer work/recovery session (`vos session start|stop`).
#[derive(Debug, Clone, Serialize)]
pub struct WorkSession {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_secs: Option<i64>,
    pub predicted_difficulty: RecoveryDifficulty,
    pub predicted_score: usize,
    pub recovery_quality: Option<u8>,
    pub return_point: Option<String>,
    pub start_note: Option<String>,
    pub stop_note: Option<String>,
    pub files_changed_count: usize,
    pub productive_areas: Vec<String>,
}

/// Breakdown of recorded sessions by predicted difficulty tier.
#[derive(Debug, Clone, Serialize)]
pub struct DifficultyStats {
    pub difficulty: RecoveryDifficulty,
    pub sessions: usize,
    pub avg_duration_secs: Option<i64>,
    pub avg_quality: Option<f64>,
}

/// Aggregated recovery and productivity analytics (`vos session stats`).
#[derive(Debug, Clone, Serialize)]
pub struct SessionStats {
    pub project_name: Option<String>,
    pub total_sessions: usize,
    pub active_sessions: usize,
    pub completed_sessions: usize,
    pub median_duration_secs: Option<i64>,
    pub avg_duration_secs: Option<i64>,
    pub avg_quality: Option<f64>,
    pub frequent_return_points: Vec<(String, usize)>,
    pub most_productive_areas: Vec<(String, usize)>,
    pub by_predicted_difficulty: Vec<DifficultyStats>,
    /// Completed `Hard`-predicted sessions that recovered quickly with high quality.
    pub false_alarms: usize,
    /// Completed `Hard`-predicted sessions with a recorded duration and quality.
    pub hard_scored_sessions: usize,
    /// Completed `Easy`-predicted sessions that were slow or low quality.
    pub missed_difficulty: usize,
    /// Completed `Easy`-predicted sessions with a recorded duration or quality.
    pub easy_scored_sessions: usize,
    /// Completed sessions with both duration and recovery quality (benchmark-ready data points).
    pub scored_sessions: usize,
}

/// Experimental structural centrality metrics for a node in the dependency graph.
#[derive(Debug, Clone, Serialize)]
pub struct HubImportance {
    pub path: String,
    pub title: String,
    pub out_degree: usize,
    pub in_degree: usize,
    pub total_relations: usize,
    pub hub_score: f64,
    pub role: String,
}

/// Experimental documentation substance tier (`Rich`, `Solid`, `Stub`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DocQualityTier {
    Rich,
    Solid,
    Stub,
}

impl DocQualityTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rich => "Rich",
            Self::Solid => "Solid",
            Self::Stub => "Stub",
        }
    }
}

impl fmt::Display for DocQualityTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Experimental per-document deterministic quality score (`0..=100`).
#[derive(Debug, Clone, Serialize)]
pub struct DocQualityEntry {
    pub path: String,
    pub title: String,
    pub quality_score: usize,
    pub tier: DocQualityTier,
    pub md_links: usize,
    pub path_refs: usize,
    pub size_bytes: usize,
}

/// Experimental project documentation quality report (`vos coverage --experimental`).
#[derive(Debug, Clone, Serialize)]
pub struct DocQualityReport {
    pub project_name: String,
    pub total_docs: usize,
    pub mean_score: f64,
    pub rich_count: usize,
    pub solid_count: usize,
    pub stub_count: usize,
    pub docs: Vec<DocQualityEntry>,
}

/// Experimental blast-radius summary for a target object (os why --experimental).
#[derive(Debug, Clone, Serialize)]
pub struct ObjectImpact {
    pub path: String,
    pub hub_score: Option<f64>,
    pub role: Option<String>,
    pub direct_dependents: usize,
    pub transitive_dependents: usize,
    pub sample_dependents: Vec<String>,
}

/// Summary count of a tag attached across active objects.
#[derive(Debug, Clone, Serialize)]
pub struct TagSummary {
    pub tag: String,
    pub object_count: usize,
}

/// Database and index health summary returned by `vos doctor`.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub sqlite_ok: bool,
    pub sqlite_detail: String,
    pub objects_count: usize,
    pub objects_fts_count: usize,
    pub chunks_count: usize,
    pub chunks_fts_count: usize,
    pub orphaned_objects: usize,
    pub stale_deleted_relations: usize,
    pub active_sessions: usize,
    pub missing_project_roots: Vec<String>,
    pub repaired: bool,
    pub healthy: bool,
}
