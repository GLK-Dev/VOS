//! Core domain models, errors, and deterministic path/hash utilities for VOS.

pub mod error;
pub mod models;
pub mod util;

pub use error::VosError;
pub use models::{
    ActiveArea, BenchmarkRunRecord, CategoryBenchmarkStats, EventRecord, EventType, ObjectDetails,
    ObjectKind, ObjectRecord, ObjectWhy, Project, ProjectActivity, ProjectContext, ProjectResume,
    ProjectStart, ProjectSummary, RelatedHit, SearchBenchmarkCase, SearchBenchmarkCaseResult,
    SearchBenchmarkReport, SearchHit, SearchSignal,
};
