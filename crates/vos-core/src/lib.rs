//! Core domain models, errors, and deterministic path/hash utilities for VOS.

pub mod error;
pub mod models;
pub mod util;

pub use error::VosError;
pub use models::{
    EventRecord, EventType, ObjectDetails, ObjectKind, ObjectRecord, Project, ProjectContext,
    ProjectSummary, SearchHit,
};
