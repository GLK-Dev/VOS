use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::params;
use vos_core::util::search_visibility;
use vos_core::{ObjectKind, QueryIntent, SearchHit, SearchSignal, SearchVisibility, TagSummary};

use crate::concepts::{ConceptDictionary, detect_query_intent, is_concept_document};
use crate::{Store, StoreError};

/// Score bonus when a query token matches the file name or file stem exactly.
const TITLE_EXACT_MATCH_BOOST: f64 = 25.0;
/// Score bonus when a query token (>= 3 chars) appears inside the file name.
const TITLE_CONTAINS_BOOST: f64 = 10.0;
/// Score bonus when a query token (>= 3 chars) appears in the object path.
const PATH_CONTAINS_BOOST: f64 = 5.0;
/// Score bonus when `objects_fts` (title/path/summary) matches directly.
const OBJECT_FTS_SIGNAL_BOOST: f64 = 4.0;
/// Bonus when all query terms of a multi-term query match the document.
const ALL_TERMS_MATCH_BOOST: f64 = 12.0;
/// Bonus when the exact multi-term phrase appears in title, path, summary, or matched chunk.
const PHRASE_MATCH_BOOST: f64 = 8.0;
/// Base bonus for architectural/specification documents under concept queries.
const CONCEPT_DOC_BASE_BOOST: f64 = 8.0;
/// Bonus when an expanded concept synonym from `config/concepts.yaml` matches.
const CONCEPT_SYNONYM_BOOST: f64 = 12.0;
/// Additional score per extra matching chunk in the same file (capped).
const MULTI_CHUNK_STEP_BOOST: f64 = 1.5;
const MULTI_CHUNK_MAX_BOOST: f64 = 6.0;
/// Penalty applied to lockfiles and housekeeping noise during search ranking.
const NOISE_PENALTY: f64 = -10.0;
/// Penalty applied to internal search/benchmark config files unless --include-internal is passed.
const INTERNAL_ARTIFACT_PENALTY: f64 = -20.0;
/// Minimum non-noise Stage 1 (AND) hits before triggering Stage 2 (OR) fallback.
const STAGE2_FALLBACK_MIN_HITS: usize = 3;

#[derive(Clone, Copy)]
enum StageKind {
    Stage1And,
    Stage2Or,
    ConceptExpansion,
}

struct SearchCandidate {
    hit: SearchHit,
    chunk_hits: usize,
    matched_object_fts: bool,
    matched_stage1_and: bool,
    matched_concept_expansion: bool,
    best_chunk_bm25: Option<f64>,
    object_bm25: Option<f64>,
    observed_text_lower: String,
}

impl Store {
    /// Multi-stage FTS5 search with `AND` -> `OR` fallback, concept expansion, and intent-weighted scoring.
    pub fn search(
        &self,
        query: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError> {
        self.search_with_options(query, project_name, limit, false)
    }

    /// Multi-stage FTS5 search with optional inclusion of internal search/benchmark artifacts.
    pub fn search_with_options(
        &self,
        query: &str,
        project_name: Option<&str>,
        limit: usize,
        include_internal: bool,
    ) -> Result<Vec<SearchHit>, StoreError> {
        self.search_filtered(Some(query), project_name, None, limit, include_internal)
    }

    /// Searches indexed objects with optional FTS query, project scope, and tag filter.
    pub fn search_filtered(
        &self,
        query: Option<&str>,
        project_name: Option<&str>,
        tag: Option<&str>,
        limit: usize,
        include_internal: bool,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let clean_tag = tag
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty());
        let clean_query = query.map(str::trim).filter(|q| !q.is_empty());
        let requested_limit = limit.clamp(1, 200);

        let Some(query) = clean_query else {
            let tag_name = clean_tag.ok_or_else(|| {
                StoreError::InvalidInput("provide a search query or --tag".to_string())
            })?;
            return self.search_by_tag(&tag_name, project_name, requested_limit);
        };

        let and_query = build_fts_query(query)?;
        let query_terms: Vec<String> = query
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| t.to_lowercase())
            .collect();
        let phrase_lower = query_terms.join(" ");

        let concepts = ConceptDictionary::load();
        let intent = detect_query_intent(query, &query_terms, &concepts);

        let pool_limit = (requested_limit * 6).max(60) as i64;
        let mut merged: HashMap<String, SearchCandidate> = HashMap::new();

        // Stage 1: strict AND across chunks_fts and objects_fts.
        self.collect_fts_stage(
            &and_query,
            project_name,
            clean_tag.as_deref(),
            pool_limit,
            StageKind::Stage1And,
            intent,
            &mut merged,
        )?;

        // Stage 2: automatic OR fallback on sparse multi-term queries.
        let stage1_clean = merged.values().filter(|c| !c.hit.is_noise).count();
        if query_terms.len() > 1
            && stage1_clean < STAGE2_FALLBACK_MIN_HITS
            && let Some(or_query) = crate::queries::build_fts_or_query(&query_terms)
        {
            self.collect_fts_stage(
                &or_query,
                project_name,
                clean_tag.as_deref(),
                pool_limit,
                StageKind::Stage2Or,
                intent,
                &mut merged,
            )?;
        }

        // Stage 3: deterministic concept expansion from `config/concepts.yaml`.
        let synonyms = concepts.lookup(query).unwrap_or(&[]);
        if !synonyms.is_empty()
            && let Some(concept_fts) = build_concept_fts_query(synonyms)
        {
            self.collect_fts_stage(
                &concept_fts,
                project_name,
                clean_tag.as_deref(),
                pool_limit,
                StageKind::ConceptExpansion,
                intent,
                &mut merged,
            )?;
        }

        let mut hits: Vec<SearchHit> = merged
            .into_values()
            .map(|mut cand| {
                let (score, signals) =
                    score_search_candidate(&cand, &query_terms, &phrase_lower, intent, synonyms, include_internal);
                cand.hit.score = score;
                cand.hit.signals = signals;
                cand.hit
            })
            .collect();

        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(requested_limit);
        Ok(hits)
    }

    /// Lists all active tags with object counts, optionally filtered by project.
    pub fn list_tags(
        &self,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TagSummary>, StoreError> {
        let capped = limit.clamp(1, 200) as i64;
        let mut stmt = self.conn.prepare(
            "SELECT t.tag, COUNT(DISTINCT t.object_id) AS cnt
             FROM tags t
             JOIN objects o ON o.id = t.object_id
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE o.is_deleted = 0
               AND (?1 IS NULL OR p.name = ?1)
             GROUP BY t.tag
             ORDER BY cnt DESC, t.tag ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_name, capped], |row| {
            Ok(TagSummary {
                tag: row.get(0)?,
                object_count: row.get::<_, i64>(1)?.max(0) as usize,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
    }

    fn search_by_tag(
        &self,
        tag_name: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT o.id, p.name, o.title, o.path, o.kind, o.is_noise, COALESCE(o.summary, '')
             FROM tags t
             JOIN objects o ON o.id = t.object_id
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE o.is_deleted = 0
               AND t.tag = ?1
               AND (?2 IS NULL OR p.name = ?2)
             ORDER BY o.is_noise ASC, o.modified_at DESC, o.title ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![tag_name, project_name, limit as i64], |row| {
            let kind_str: String = row.get(4)?;
            let is_noise: i64 = row.get(5)?;
            let kind = ObjectKind::from_db(&kind_str);
            let intent = if matches!(kind, ObjectKind::Note | ObjectKind::Doc) {
                QueryIntent::Documentation
            } else {
                QueryIntent::Code
            };
            Ok(SearchHit {
                object_id: row.get(0)?,
                project_name: row.get(1)?,
                title: row.get(2)?,
                path: row.get(3)?,
                kind,
                is_noise: is_noise != 0,
                snippet: row.get(6)?,
                score: 1.0,
                intent,
                signals: vec![SearchSignal {
                    label: format!("tag match ({tag_name})"),
                    delta: 1.0,
                }],
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_fts_stage(
        &self,
        fts_query: &str,
        project_name: Option<&str>,
        tag_filter: Option<&str>,
        pool_limit: i64,
        stage: StageKind,
        intent: QueryIntent,
        merged: &mut HashMap<String, SearchCandidate>,
    ) -> Result<(), StoreError> {
        let is_stage1 = matches!(stage, StageKind::Stage1And);
        let is_concept = matches!(stage, StageKind::ConceptExpansion);

        {
            let mut stmt = self.conn.prepare(
                "SELECT o.id, p.name, o.title, o.path, o.kind, o.is_noise,
                        snippet(chunks_fts, 0, '[', ']', '...', 24) AS snip,
                        bm25(chunks_fts) AS rank,
                        c.text
                 FROM chunks_fts
                 JOIN chunks c ON c.rowid = chunks_fts.rowid
                 JOIN objects o ON o.id = c.object_id
                 LEFT JOIN projects p ON p.id = o.project_id
                 WHERE chunks_fts MATCH ?1
                   AND o.is_deleted = 0
                   AND (?2 IS NULL OR p.name = ?2)
                   AND (?4 IS NULL OR EXISTS (
                       SELECT 1 FROM tags t WHERE t.object_id = o.id AND t.tag = ?4
                   ))
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(params![fts_query, project_name, pool_limit, tag_filter], |row| {
                let kind_str: String = row.get(4)?;
                let is_noise: i64 = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    ObjectKind::from_db(&kind_str),
                    is_noise != 0,
                    row.get::<_, String>(6)?,
                    row.get::<_, f64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?;

            for row in rows {
                let (id, pname, title, path, kind, is_noise, snippet, bm25_rank, chunk_text) = row?;
                let is_new = !merged.contains_key(&id);
                let entry = merged.entry(id.clone()).or_insert_with(|| SearchCandidate {
                    hit: SearchHit {
                        object_id: id,
                        project_name: pname,
                        title,
                        path,
                        kind,
                        is_noise,
                        snippet: snippet.clone(),
                        score: 0.0,
                        intent,
                        signals: Vec::new(),
                    },
                    chunk_hits: 0,
                    matched_object_fts: false,
                    matched_stage1_and: is_stage1,
                    matched_concept_expansion: is_concept,
                    best_chunk_bm25: None,
                    object_bm25: None,
                    observed_text_lower: String::new(),
                });

                if is_stage1 {
                    entry.matched_stage1_and = true;
                }
                if is_concept {
                    entry.matched_concept_expansion = true;
                }
                if is_stage1 || is_new {
                    entry.chunk_hits += 1;
                }
                if entry.observed_text_lower.len() < 4096 {
                    entry.observed_text_lower.push('\n');
                    entry.observed_text_lower.push_str(&chunk_text.to_lowercase());
                }
                if entry.best_chunk_bm25.is_none_or(|prev| bm25_rank < prev) && !is_concept {
                    entry.best_chunk_bm25 = Some(bm25_rank);
                    entry.hit.snippet = snippet;
                } else if entry.best_chunk_bm25.is_none() {
                    entry.best_chunk_bm25 = Some(bm25_rank);
                    if entry.hit.snippet.trim().is_empty() {
                        entry.hit.snippet = snippet;
                    }
                }
            }
        }

        {
            let mut stmt = self.conn.prepare(
                "SELECT o.id, p.name, o.title, o.path, o.kind, o.is_noise,
                        snippet(objects_fts, -1, '[', ']', '...', 24) AS snip,
                        bm25(objects_fts, 10.0, 3.0, 1.0) AS rank,
                        COALESCE(o.summary, '')
                 FROM objects_fts
                 JOIN objects o ON o.rowid = objects_fts.rowid
                 LEFT JOIN projects p ON p.id = o.project_id
                 WHERE objects_fts MATCH ?1
                   AND o.is_deleted = 0
                   AND (?2 IS NULL OR p.name = ?2)
                   AND (?4 IS NULL OR EXISTS (
                       SELECT 1 FROM tags t WHERE t.object_id = o.id AND t.tag = ?4
                   ))
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(params![fts_query, project_name, pool_limit, tag_filter], |row| {
                let kind_str: String = row.get(4)?;
                let is_noise: i64 = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    ObjectKind::from_db(&kind_str),
                    is_noise != 0,
                    row.get::<_, String>(6)?,
                    row.get::<_, f64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?;

            for row in rows {
                let (id, pname, title, path, kind, is_noise, snippet, bm25_rank, summary) = row?;
                let entry = merged.entry(id.clone()).or_insert_with(|| SearchCandidate {
                    hit: SearchHit {
                        object_id: id,
                        project_name: pname,
                        title,
                        path,
                        kind,
                        is_noise,
                        snippet: snippet.clone(),
                        score: 0.0,
                        intent,
                        signals: Vec::new(),
                    },
                    chunk_hits: 0,
                    matched_object_fts: false,
                    matched_stage1_and: is_stage1,
                    matched_concept_expansion: is_concept,
                    best_chunk_bm25: None,
                    object_bm25: None,
                    observed_text_lower: String::new(),
                });
                if is_stage1 {
                    entry.matched_stage1_and = true;
                }
                if is_concept {
                    entry.matched_concept_expansion = true;
                }
                if !is_concept {
                    entry.matched_object_fts = true;
                }
                entry.object_bm25 = Some(
                    entry
                        .object_bm25
                        .map_or(bm25_rank, |prev| prev.min(bm25_rank)),
                );
                if entry.observed_text_lower.len() < 4096 {
                    entry.observed_text_lower.push('\n');
                    entry.observed_text_lower.push_str(&summary.to_lowercase());
                }
                if entry.hit.snippet.trim().is_empty() {
                    entry.hit.snippet = snippet;
                }
            }
        }

        Ok(())
    }
}

fn score_search_candidate(
    cand: &SearchCandidate,
    query_terms: &[String],
    phrase_lower: &str,
    intent: QueryIntent,
    synonyms: &[String],
    include_internal: bool,
) -> (f64, Vec<SearchSignal>) {
    let mut signals = Vec::new();
    let title_multiplier = if intent == QueryIntent::Concept { 2.0 } else { 1.0 };
    let phrase_multiplier = if intent == QueryIntent::Concept { 2.0 } else { 1.0 };

    let chunk_bm25_mag = cand.best_chunk_bm25.map(|r| (-r).max(0.0)).unwrap_or(0.0);
    let obj_bm25_mag = cand.object_bm25.map(|r| (-r).max(0.0)).unwrap_or(0.0);

    if chunk_bm25_mag >= obj_bm25_mag && chunk_bm25_mag > 0.0 {
        signals.push(SearchSignal {
            label: "chunk fts bm25".to_string(),
            delta: chunk_bm25_mag,
        });
    } else if obj_bm25_mag > 0.0 {
        signals.push(SearchSignal {
            label: "object fts bm25".to_string(),
            delta: obj_bm25_mag,
        });
    }

    if cand.matched_object_fts {
        signals.push(SearchSignal {
            label: "object fts match".to_string(),
            delta: OBJECT_FTS_SIGNAL_BOOST,
        });
    }

    if cand.chunk_hits > 1 {
        let extra = ((cand.chunk_hits - 1) as f64 * MULTI_CHUNK_STEP_BOOST)
            .min(MULTI_CHUNK_MAX_BOOST);
        signals.push(SearchSignal {
            label: format!("multiple chunk hits ({})", cand.chunk_hits),
            delta: extra,
        });
    }

    let title_lower = cand.hit.title.to_lowercase();
    let stem_lower = Path::new(&cand.hit.title)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    let path_lower = cand.hit.path.to_lowercase();

    let mut exact_title_hit = false;
    let mut partial_title_hit = false;
    let mut path_hit = false;
    let mut matched_terms = HashSet::new();

    for term in query_terms {
        if title_lower == *term || stem_lower == *term {
            exact_title_hit = true;
        } else if term.len() >= 3 && title_lower.contains(term) {
            partial_title_hit = true;
        }
        if term.len() >= 3 && path_lower.contains(term) {
            path_hit = true;
        }
        if title_lower.contains(term)
            || path_lower.contains(term)
            || cand.observed_text_lower.contains(term)
        {
            matched_terms.insert(term.as_str());
        }
    }

    if exact_title_hit {
        signals.push(SearchSignal {
            label: "title exact match".to_string(),
            delta: TITLE_EXACT_MATCH_BOOST * title_multiplier,
        });
    } else if partial_title_hit {
        signals.push(SearchSignal {
            label: "title contains term".to_string(),
            delta: TITLE_CONTAINS_BOOST * title_multiplier,
        });
    }

    if path_hit {
        signals.push(SearchSignal {
            label: "path match".to_string(),
            delta: PATH_CONTAINS_BOOST,
        });
    }

    if query_terms.len() > 1 {
        let all_doc_terms = matched_terms.len() == query_terms.len();
        if cand.matched_stage1_and || all_doc_terms {
            signals.push(SearchSignal {
                label: "all query terms matched".to_string(),
                delta: ALL_TERMS_MATCH_BOOST,
            });
        }
        if !phrase_lower.is_empty()
            && (title_lower.contains(phrase_lower)
                || path_lower.contains(phrase_lower)
                || cand.observed_text_lower.contains(phrase_lower))
        {
            signals.push(SearchSignal {
                label: "exact phrase match".to_string(),
                delta: PHRASE_MATCH_BOOST * phrase_multiplier,
            });
        }
    }

    if intent == QueryIntent::Concept && is_concept_document(&cand.hit.path, &cand.hit.title) {
        signals.push(SearchSignal {
            label: "concept document boost".to_string(),
            delta: CONCEPT_DOC_BASE_BOOST * 2.5,
        });
    }

    if cand.matched_concept_expansion && !synonyms.is_empty() {
        let matched_syn = synonyms.iter().find(|syn| {
            title_lower.contains(syn.as_str())
                || path_lower.contains(syn.as_str())
                || cand.observed_text_lower.contains(syn.as_str())
        });
        let label = match matched_syn {
            Some(s) => format!("concept expansion ('{s}')"),
            None => "concept expansion".to_string(),
        };
        signals.push(SearchSignal {
            label,
            delta: CONCEPT_SYNONYM_BOOST,
        });
    }

    if !include_internal && search_visibility(Path::new(&cand.hit.path)) == SearchVisibility::Internal {
        signals.push(SearchSignal {
            label: "internal artifact penalty".to_string(),
            delta: INTERNAL_ARTIFACT_PENALTY,
        });
    }

    if cand.hit.is_noise {
        signals.push(SearchSignal {
            label: "noise penalty".to_string(),
            delta: NOISE_PENALTY,
        });
    }

    let total: f64 = signals.iter().map(|s| s.delta).sum();
    (total, signals)
}

fn build_fts_query(raw: &str) -> Result<String, StoreError> {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();

    if tokens.is_empty() {
        return Err(StoreError::InvalidInput(
            "search query cannot be empty".to_string(),
        ));
    }

    Ok(tokens.join(" AND "))
}

fn build_concept_fts_query(synonyms: &[String]) -> Option<String> {
    let mut clauses = Vec::new();
    for syn in synonyms {
        let clean = syn.trim();
        if clean.is_empty() {
            continue;
        }
        let escaped = clean.replace('"', "\"\"");
        clauses.push(format!("\"{escaped}\""));
    }
    if clauses.is_empty() {
        None
    } else {
        Some(clauses.join(" OR "))
    }
}

#[cfg(test)]
mod tests {
    use vos_core::ObjectKind;

    use super::*;
    use crate::UpsertObjectInput;

    #[test]
    fn concept_expansion_and_concept_doc_boost_surface_spec() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("vos", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/SPEC.md".to_string(),
                kind: ObjectKind::Note,
                title: "SPEC.md".to_string(),
                summary: Some("VOS Project Memory Specification".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 200,
                modified_at: 1000,
                is_noise: false,
                chunks: vec!["Stores chronological events and project timeline entries.".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/src/misc.rs".to_string(),
                kind: ObjectKind::Code,
                title: "misc.rs".to_string(),
                summary: None,
                content_hash: "h2".to_string(),
                size_bytes: 80,
                modified_at: 1000,
                is_noise: false,
                chunks: vec!["// unrelated log output".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let hits = store.search("event log", Some("vos"), 5).unwrap();
        assert_eq!(hits[0].title, "SPEC.md");
        assert_eq!(hits[0].intent, QueryIntent::Concept);
        assert!(hits[0]
            .signals
            .iter()
            .any(|s| s.label == "concept document boost"));
        assert!(hits[0]
            .signals
            .iter()
            .any(|s| s.label.starts_with("concept expansion")));
    }

    #[test]
    fn internal_artifacts_are_penalized_unless_include_internal_is_set() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("vos", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/config/concepts.yaml".to_string(),
                kind: ObjectKind::Code,
                title: "concepts.yaml".to_string(),
                summary: Some("event log: audit log".to_string()),
                content_hash: "h_cfg".to_string(),
                size_bytes: 60,
                modified_at: 1000,
                is_noise: false,
                chunks: vec!["event log:\n  - audit log\n  - event stream".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/SPEC.md".to_string(),
                kind: ObjectKind::Note,
                title: "SPEC.md".to_string(),
                summary: Some("Event log architecture".to_string()),
                content_hash: "h_spec".to_string(),
                size_bytes: 120,
                modified_at: 1000,
                is_noise: false,
                chunks: vec!["Event log and chronological timeline.".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let normal = store.search("event log", Some("vos"), 5).unwrap();
        assert_eq!(normal[0].title, "SPEC.md");

        let diag = store
            .search_with_options("event log", Some("vos"), 5, true)
            .unwrap();
        assert!(!diag.iter().any(|h| h
            .signals
            .iter()
            .any(|s| s.label == "internal artifact penalty")));
    }

    #[test]
    fn search_tag_filter_and_tag_listing_work() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("tagrepo", None, None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/auth.rs".to_string(),
                kind: ObjectKind::Code,
                title: "auth.rs".to_string(),
                summary: Some("Token validation".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 60,
                modified_at: 200,
                is_noise: false,
                chunks: vec!["pub fn verify_token() {}".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/other.rs".to_string(),
                kind: ObjectKind::Code,
                title: "other.rs".to_string(),
                summary: Some("Token helper".to_string()),
                content_hash: "h2".to_string(),
                size_bytes: 60,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["pub fn token_format() {}".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        store
            .add_tags_scoped("auth.rs", Some("tagrepo"), &["auth".into(), "security".into()])
            .unwrap();
        let tags = store.list_tags(Some("tagrepo"), 10).unwrap();
        assert_eq!(tags.len(), 2);

        let tag_only = store
            .search_filtered(None, Some("tagrepo"), Some("auth"), 10, false)
            .unwrap();
        assert_eq!(tag_only.len(), 1);
        assert_eq!(tag_only[0].title, "auth.rs");

        let query_and_tag = store
            .search_filtered(Some("token"), Some("tagrepo"), Some("auth"), 10, false)
            .unwrap();
        assert_eq!(query_and_tag.len(), 1);
        assert_eq!(query_and_tag[0].title, "auth.rs");
    }
}