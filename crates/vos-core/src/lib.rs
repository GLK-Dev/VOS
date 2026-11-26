//! Core domain models, errors, and deterministic path/hash utilities for VOS.

pub mod compact;
pub mod error;
pub mod models;
pub mod util;

pub use error::VosError;
pub use models::{
    ActiveArea, AffectedTest, AffectedTestsReport, AgentHandoff, ArchaeologyReport, AreaCoverage,
    AreaLease, AreaOwnership, AreaRisk, BenchmarkRunRecord, CalibrationReport, CalibrationWeights,
    CategoryBenchmarkStats, ChangedSinceReport, CoChangePartner, CommitTouch, ConceptExplanation,
    ConstellationNode, ConstellationPortfolio, ContextPack, ContinueSuggestion, CoverageTier,
    DifficultyStats, DocQualityEntry, DocQualityReport, DocQualityTier, DoctorReport,
    DocumentationHealth, DriftFinding, EventRecord, EventType, FactStatus, FileSlice, GitContext,
    GraphDirection, GraphEdge, GraphHub, GraphTreeNode, GuardReport, GuardViolation,
    HubImportance, ObjectDetails, ObjectImpact, ObjectKind, ObjectRecord, ObjectWhy, PackedItem,
    PreflightReport, Project, ProjectActivity, ProjectContext, ProjectContinue, ProjectCoverage,
    ProjectDrift, ProjectEvolution, ProjectFact, ProjectGraph, ProjectHealth, ProjectOwnership,
    ProjectResume, ProjectSnapshot, ProjectStart, ProjectStatus, ProjectSummary, QueryIntent,
    RecallHit, RecoveryDifficulty, RecoveryPrediction, RelatedHit, RelationType, RiskLevel,
    SearchBenchmarkCase, SearchBenchmarkCaseResult, SearchBenchmarkReport, SearchHit,
    SearchSignal, SearchVisibility, SessionStats, SnapshotDiff, SymbolInput, SymbolKind,
    SymbolRecord, TagSummary, WorkSession,
};
