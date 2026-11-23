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
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lifecycle event recorded for project timeline recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    Added,
    Modified,
    Deleted,
    Restored,
}

impl EventType {
    /// Returns the canonical lowercase string stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Restored => "restored",
        }
    }

    /// Parses a stored event type string.
    pub fn from_db(value: &str) -> Self {
        match value {
            "added" => Self::Added,
            "modified" => Self::Modified,
            "deleted" => Self::Deleted,
            "restored" => Self::Restored,
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
}

/// Detailed view of a single object including tags and project metadata.
#[derive(Debug, Clone)]
pub struct ObjectDetails {
    pub object: ObjectRecord,
    pub project_name: Option<String>,
    pub tags: Vec<(String, String)>,
    pub chunk_count: usize,
}

/// Ranked full-text search hit across `chunks_fts` and `objects_fts`.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub object_id: String,
    pub project_name: Option<String>,
    pub title: String,
    pub path: String,
    pub kind: ObjectKind,
    pub snippet: String,
    pub score: f64,
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
    pub counts_by_kind: Vec<(String, usize)>,
    pub recent_objects: Vec<ObjectRecord>,
    pub recent_events: Vec<EventRecord>,
}
