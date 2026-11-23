//! Core domain models, errors, and deterministic path/hash utilities for VOS.

pub mod error;
pub mod models;
pub mod util;

pub use error::VosError;
pub use models::{
    ActiveArea, BenchmarkRunRecord, CategoryBenchmarkStats, ChangedSinceReport, ConceptExplanation,
    ContinueSuggestion, DriftFinding, EventRecord, EventType, GraphDirection, GraphEdge, GraphHub,
    GraphTreeNode, ObjectDetails, ObjectKind, ObjectRecord, ObjectWhy, Project, ProjectActivity,
    ProjectContinue, ProjectContext, ProjectDrift, ProjectEvolution, ProjectGraph, ProjectHealth,
    ProjectResume, ProjectStart, ProjectStatus, ProjectSummary, QueryIntent, RelatedHit,
    RelationType, SearchBenchmarkCase, SearchBenchmarkCaseResult, SearchBenchmarkReport, SearchHit,
    SearchSignal, SearchVisibility,
};
