//! Core domain models, errors, and deterministic path/hash utilities for VOS.

pub mod error;
pub mod models;
pub mod util;

pub use error::VosError;
pub use models::{
    ActiveArea, EventRecord, EventType, ObjectDetails, ObjectKind, ObjectRecord, Project,
    ProjectActivity, ProjectContext, ProjectResume, ProjectSummary, RelatedHit, SearchHit,
};
