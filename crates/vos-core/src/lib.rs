//! Core domain models, errors, and deterministic path/hash utilities for VOS.

pub mod error;
pub mod models;
pub mod util;

pub use error::VosError;
pub use models::{
    ActiveArea, BenchmarkRunRecord, CategoryBenchmarkStats, ChangedSinceReport, ConceptExplanation,
    ContinueSuggestion, EventRecord, EventType, ObjectDetails, ObjectKind, ObjectRecord,
    ObjectWhy, Project, ProjectActivity, ProjectContinue, ProjectContext, ProjectHealth,
    ProjectResume, ProjectStart, ProjectStatus, ProjectSummary, QueryIntent, RelatedHit,
    SearchBenchmarkCase, SearchBenchmarkCaseResult, SearchBenchmarkReport, SearchHit, SearchSignal,
    SearchVisibility,
};
