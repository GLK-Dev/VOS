use std::collections::HashMap;
use std::path::Path;

use vos_core::QueryIntent;

const EMBEDDED_CONCEPTS_YAML: &str = include_str!("../../../config/concepts.yaml");

/// Deterministic concept dictionary loaded from `config/concepts.yaml` (or embedded fallback).
#[derive(Debug, Default)]
pub struct ConceptDictionary {
    entries: HashMap<String, Vec<String>>,
}

impl ConceptDictionary {
    /// Loads `config/concepts.yaml` if present in the working directory, else uses embedded defaults.
    pub fn load() -> Self {
        let local_path = Path::new("config/concepts.yaml");
        if local_path.is_file()
            && let Ok(raw) = std::fs::read_to_string(local_path)
            && let Ok(parsed) = serde_yaml::from_str::<HashMap<String, Vec<String>>>(&raw)
        {
            return Self::from_map(parsed);
        }

        let parsed = serde_yaml::from_str::<HashMap<String, Vec<String>>>(EMBEDDED_CONCEPTS_YAML)
            .unwrap_or_default();
        Self::from_map(parsed)
    }

    fn from_map(raw: HashMap<String, Vec<String>>) -> Self {
        let entries = raw
            .into_iter()
            .map(|(k, v)| {
                let key = normalize_phrase(&k);
                let vals = v
                    .into_iter()
                    .map(|s| normalize_phrase(&s))
                    .filter(|s| !s.is_empty())
                    .collect();
                (key, vals)
            })
            .collect();
        Self { entries }
    }

    /// Returns synonym phrases for a normalized query if present in the dictionary.
    pub fn lookup(&self, query: &str) -> Option<&[String]> {
        let norm = normalize_phrase(query);
        self.entries.get(&norm).map(Vec::as_slice)
    }
}

/// Classifies a query into `Code`, `Documentation`, or `Concept` intent using deterministic rules.
pub fn detect_query_intent(
    raw_query: &str,
    terms: &[String],
    concepts: &ConceptDictionary,
) -> QueryIntent {
    if concepts.lookup(raw_query).is_some() {
        return QueryIntent::Concept;
    }

    let has_code_punctuation = raw_query.contains('_')
        || raw_query.contains("::")
        || raw_query.contains('.')
        || raw_query.contains('/');

    if !has_code_punctuation && terms.len() >= 2 {
        return QueryIntent::Concept;
    }

    if terms.iter().any(|t| is_doc_keyword(t)) {
        return QueryIntent::Documentation;
    }

    QueryIntent::Code
}

/// Returns true for architectural/specification documents boosted under `QueryIntent::Concept`.
pub fn is_concept_document(path: &str, title: &str) -> bool {
    let lower_title = title.to_lowercase();
    let lower_path = path.replace('\\', "/").to_lowercase();

    matches!(
        lower_title.as_str(),
        "spec.md"
            | "readme.md"
            | "agents.md"
            | "design_manifesto.md"
            | "whitepaper.md"
            | "architecture.md"
            | "architecture_audit.md"
            | "architecture-rfc.md"
            | "tessera_system_architecture_spec.md"
            | "01_system_overview.md"
            | "0001_init.sql"
            | "project_memory.md"
            | "operating_system.md"
    ) || lower_path.contains("/docs/architecture/")
        || (lower_title.ends_with(".md")
            && (lower_title.contains("architecture")
                || lower_title.contains("spec")
                || lower_title.contains("rfc")
                || lower_title.contains("design")))
}

fn is_doc_keyword(term: &str) -> bool {
    matches!(
        term,
        "roadmap"
            | "release"
            | "installation"
            | "audit"
            | "changelog"
            | "manifesto"
            | "migration"
            | "specification"
            | "spec"
            | "benchmark"
            | "readme"
            | "guide"
    )
}

fn normalize_phrase(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}
