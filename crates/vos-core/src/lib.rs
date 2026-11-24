//! Core domain models, errors, and deterministic path/hash utilities for VOS.

pub mod error;
pub mod models;
pub mod util;

pub use error::VosError;
pub use models::{
    ActiveArea, AreaCoverage, AreaOwnership, AreaRisk, BenchmarkRunRecord, CategoryBenchmarkStats,
    ChangedSinceReport, ConceptExplanation, ContinueSuggestion, CoverageTier, DocumentationHealth,
    DriftFinding, EventRecord, EventType, GraphDirection, GraphEdge, GraphHub, GraphTreeNode,
    ObjectDetails, ObjectKind, ObjectRecord, ObjectWhy, Project, ProjectActivity, ProjectContinue,
    ProjectContext, ProjectCoverage, ProjectDrift, ProjectEvolution, ProjectGraph, ProjectHealth,
    ProjectOwnership, ProjectResume, ProjectStart, ProjectStatus, ProjectSummary, QueryIntent,
    RecoveryDifficulty, RecoveryPrediction, RelatedHit, RelationType, RiskLevel, SearchBenchmarkCase, SearchBenchmarkCaseResult,
    SearchBenchmarkReport, SearchHit, SearchSignal, SearchVisibility,
};
