use serde::Deserialize;
use std::fmt;

/// Classification of an indexed filesystem object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Code,
    Note,
    Doc,
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
            "model" => Self::Model,
            "media" => Self::Media,
            _ => Self::Other,
        }
    }

    /// Returns true if text content should be chunked for full-text search.
    pub fn is_text_indexable(&self) -> bool {
        matches!(self, Self::Code | Self::Note)
    }

    /// Relative priority used to surface human-meaningful files in `vos context`.
    pub fn context_priority(&self) -> i64 {
        match self {
            Self::Note => 100,
            Self::Doc => 90,
            Self::Code => 80,
            Self::Model => 70,
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
    pub recent_events: Vec<EventRecord>,
}

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


