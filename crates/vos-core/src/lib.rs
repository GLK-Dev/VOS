//! Core domain models, errors, and deterministic path/hash utilities for VOS.

pub mod error;
pub mod models;
pub mod util;

pub use error::VosError;
pub use models::{
    ActiveArea, AreaCoverage, AreaOwnership, AreaRisk, BenchmarkRunRecord, CategoryBenchmarkStats,
    ChangedSinceReport, ConceptExplanation, ContinueSuggestion, CoverageTier, DifficultyStats,
    DocQualityEntry, DocQualityReport, DocQualityTier, DocumentationHealth, DriftFinding,
    EventRecord, EventType, GraphDirection, GraphEdge, GraphHub, GraphTreeNode, HubImportance,
    ObjectDetails, ObjectKind, ObjectRecord, ObjectWhy, Project, ProjectActivity, ProjectContinue,
    ProjectContext, ProjectCoverage, ProjectDrift, ProjectEvolution, ProjectGraph, ProjectHealth,
    ProjectOwnership, ProjectResume, ProjectSnapshot, ProjectStart, ProjectStatus, ProjectSummary,
    QueryIntent, RecoveryDifficulty, RecoveryPrediction, RelatedHit, RelationType, RiskLevel,
    SearchBenchmarkCase, SearchBenchmarkCaseResult, SearchBenchmarkReport, SearchHit, SearchSignal,
    SearchVisibility, SessionStats, SnapshotDiff, WorkSession,
};
