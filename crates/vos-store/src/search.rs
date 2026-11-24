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
/// Score bonus per matching tag when query terms match attached object tags.
const TAG_MATCH_BOOST: f64 = 18.0;
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

/// Optional filters applied to full-text or metadata-only `Store::search_with_filters` queries.
#[derive(Debug, Clone, Copy)]
pub struct SearchFilters<'a> {
    pub project: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub kind: Option<ObjectKind>,
    pub since: Option<i64>,
    pub limit: usize,
    pub include_internal: bool,
}

impl Default for SearchFilters<'_> {
    fn default() -> Self {
        Self {
            project: None,
            tag: None,
            kind: None,
            since: None,
            limit: 10,
            include_internal: false,
        }
    }
}

struct SearchCandidate {
    hit: SearchHit,
    chunk_hits: usize,
    matched_object_fts: bool,
    matched_stage1_and: bool,
    matched_concept_expansion: bool,
    matched_tags: Vec<String>,
    best_chunk_bm25: Option<f64>,
    object_bm25: Option<f64>,
    observed_text_lower: String,
}

impl SearchCandidate {
    #[allow(clippy::too_many_arguments)]
    fn new(
        object_id: String,
        project_name: Option<String>,
        title: String,
        path: String,
        kind: ObjectKind,
        is_noise: bool,
        snippet: String,
        intent: QueryIntent,
    ) -> Self {
        Self {
            hit: SearchHit {
                object_id,
                project_name,
                title,
                path,
                kind,
                is_noise,
                snippet,
                score: 0.0,
                intent,
                signals: Vec::new(),
            },
            chunk_hits: 0,
            matched_object_fts: false,
            matched_stage1_and: false,
            matched_concept_expansion: false,
            matched_tags: Vec::new(),
            best_chunk_bm25: None,
            object_bm25: None,
            observed_text_lower: String::new(),
        }
    }
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
        self.search_with_filters(
            query,
            SearchFilters {
                project: project_name,
                tag,
                kind: None,
                since: None,
                limit,
                include_internal,
            },
        )
    }

    /// Searches indexed objects with optional FTS query, project scope, tag filter, and kind filter.
    pub fn search_with_filters(
        &self,
        query: Option<&str>,
        filters: SearchFilters<'_>,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let project_name = filters.project;
        let include_internal = filters.include_internal;
        let kind_str = filters.kind.map(|k| k.as_str());
        let clean_tag = filters
            .tag
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty());
        let since_ts = filters.since;
        let clean_query = query.map(str::trim).filter(|q| !q.is_empty());
        let requested_limit = filters.limit.clamp(1, 200);

        let Some(query) = clean_query else {
            if clean_tag.is_none() && kind_str.is_none() && since_ts.is_none() {
                return Err(StoreError::InvalidInput(
                    "provide a search query, --tag, --kind, or --since".to_string(),
                ));
            }
            return self.search_without_query(
                clean_tag.as_deref(),
                project_name,
                kind_str,
                since_ts,
                requested_limit,
            );
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
            kind_str,
            since_ts,
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
                kind_str,
                since_ts,
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
                kind_str,
                since_ts,
                pool_limit,
                StageKind::ConceptExpansion,
                intent,
                &mut merged,
            )?;
        }

        // Stage 4: direct matches on attached object tags (even when absent from text/filename).
        self.collect_tag_matches(
            &query_terms,
            &phrase_lower,
            project_name,
            clean_tag.as_deref(),
            kind_str,
            since_ts,
            pool_limit,
            intent,
            &mut merged,
        )?;

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

    fn search_without_query(
        &self,
        tag_name: Option<&str>,
        project_name: Option<&str>,
        kind_filter: Option<&str>,
        since: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT o.id, p.name, o.title, o.path, o.kind, o.is_noise, COALESCE(o.summary, '')
             FROM objects o
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE o.is_deleted = 0
               AND (?1 IS NULL OR EXISTS (
                   SELECT 1 FROM tags t WHERE t.object_id = o.id AND t.tag = ?1
               ))
               AND (?2 IS NULL OR p.name = ?2)
               AND (?3 IS NULL OR o.kind = ?3)
               AND (?5 IS NULL OR o.modified_at >= ?5)
             ORDER BY o.is_noise ASC, o.modified_at DESC, o.title ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![tag_name, project_name, kind_filter, limit as i64, since],
            |row| {
            let kind_str: String = row.get(4)?;
            let is_noise: i64 = row.get(5)?;
            let kind = ObjectKind::from_db(&kind_str);
            let intent = if matches!(kind, ObjectKind::Note | ObjectKind::Doc) {
                QueryIntent::Documentation
            } else {
                QueryIntent::Code
            };
            let mut label_parts = Vec::new();
            if let Some(t) = tag_name {
                label_parts.push(format!("tag={t}"));
            }
            if let Some(k) = kind_filter {
                label_parts.push(format!("kind={k}"));
            }
            if since.is_some() {
                label_parts.push("since".to_string());
            }
            let filter_label = if label_parts.is_empty() {
                "filter match".to_string()
            } else {
                label_parts.join(", ")
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
                    label: filter_label,
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
        kind_filter: Option<&str>,
        since_ts: Option<i64>,
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
                   AND (?5 IS NULL OR o.kind = ?5)
                   AND (?6 IS NULL OR o.modified_at >= ?6)
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(
                params![fts_query, project_name, pool_limit, tag_filter, kind_filter, since_ts],
                |row| {
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
                let entry = merged.entry(id.clone()).or_insert_with(|| {
                    SearchCandidate::new(
                        id,
                        pname,
                        title,
                        path,
                        kind,
                        is_noise,
                        snippet.clone(),
                        intent,
                    )
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
                   AND (?5 IS NULL OR o.kind = ?5)
                   AND (?6 IS NULL OR o.modified_at >= ?6)
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(
                params![fts_query, project_name, pool_limit, tag_filter, kind_filter, since_ts],
                |row| {
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
                let entry = merged.entry(id.clone()).or_insert_with(|| {
                    SearchCandidate::new(
                        id,
                        pname,
                        title,
                        path,
                        kind,
                        is_noise,
                        snippet.clone(),
                        intent,
                    )
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

    #[allow(clippy::too_many_arguments)]
    fn collect_tag_matches(
        &self,
        query_terms: &[String],
        phrase_lower: &str,
        project_name: Option<&str>,
        tag_filter: Option<&str>,
        kind_filter: Option<&str>,
        since_ts: Option<i64>,
        pool_limit: i64,
        intent: QueryIntent,
        merged: &mut HashMap<String, SearchCandidate>,
    ) -> Result<(), StoreError> {
        let mut lookup_tags: Vec<String> = Vec::new();
        for term in query_terms {
            let clean = term
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .to_lowercase();
            if !clean.is_empty() && !lookup_tags.contains(&clean) {
                lookup_tags.push(clean);
            }
        }
        if query_terms.len() > 1 && !phrase_lower.is_empty() && !lookup_tags.contains(&phrase_lower.to_string()) {
            lookup_tags.push(phrase_lower.to_string());
        }
        if lookup_tags.is_empty() {
            return Ok(());
        }

        let mut stmt = self.conn.prepare(
            "SELECT o.id, p.name, o.title, o.path, o.kind, o.is_noise, COALESCE(o.summary, '')
             FROM tags t
             JOIN objects o ON o.id = t.object_id
             LEFT JOIN projects p ON p.id = o.project_id
             WHERE o.is_deleted = 0
               AND t.tag = ?1
               AND (?2 IS NULL OR p.name = ?2)
               AND (?3 IS NULL OR EXISTS (
                   SELECT 1 FROM tags tf WHERE tf.object_id = o.id AND tf.tag = ?3
               ))
               AND (?4 IS NULL OR o.kind = ?4)
               AND (?6 IS NULL OR o.modified_at >= ?6)
             ORDER BY o.is_noise ASC, o.modified_at DESC
             LIMIT ?5",
        )?;

        for tag in lookup_tags {
            let rows = stmt.query_map(
                params![tag, project_name, tag_filter, kind_filter, pool_limit, since_ts],
                |row| {
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
                    ))
                },
            )?;
            for row in rows {
                let (id, pname, title, path, kind, is_noise, summary) = row?;
                let entry = merged.entry(id.clone()).or_insert_with(|| {
                    SearchCandidate::new(id, pname, title, path, kind, is_noise, summary, intent)
                });
                if !entry.matched_tags.contains(&tag) {
                    entry.matched_tags.push(tag.clone());
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

    if !cand.matched_tags.is_empty() {
        signals.push(SearchSignal {
            label: format!("tag match ({})", cand.matched_tags.join(", ")),
            delta: (cand.matched_tags.len() as f64) * TAG_MATCH_BOOST,
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
            || cand.matched_tags.iter().any(|t| t == term)
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

    #[test]
    fn search_kind_filter_works_with_and_without_query() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("kindrepo", None, None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/NOTES.md".to_string(),
                kind: ObjectKind::Note,
                title: "NOTES.md".to_string(),
                summary: Some("Scheduler overview".to_string()),
                content_hash: "h_n".to_string(),
                size_bytes: 50,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["Scheduler design doc.".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id),
                path: "/repo/scheduler.rs".to_string(),
                kind: ObjectKind::Code,
                title: "scheduler.rs".to_string(),
                summary: Some("pub fn run_scheduler()".to_string()),
                content_hash: "h_c".to_string(),
                size_bytes: 80,
                modified_at: 200,
                is_noise: false,
                chunks: vec!["pub fn run_scheduler() {}".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let notes_only = store
            .search_with_filters(
                None,
                SearchFilters {
                    project: Some("kindrepo"),
                    kind: Some(ObjectKind::Note),
                    ..SearchFilters::default()
                },
            )
            .unwrap();
        assert_eq!(notes_only.len(), 1);
        assert_eq!(notes_only[0].title, "NOTES.md");

        let code_query = store
            .search_with_filters(
                Some("scheduler"),
                SearchFilters {
                    project: Some("kindrepo"),
                    kind: Some(ObjectKind::Code),
                    ..SearchFilters::default()
                },
            )
            .unwrap();
        assert_eq!(code_query.len(), 1);
        assert_eq!(code_query[0].title, "scheduler.rs");
    }

    #[test]
    fn search_surfaces_and_boosts_matching_manual_tags() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("tagboost", None, None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/worker.rs".to_string(),
                kind: ObjectKind::Code,
                title: "worker.rs".to_string(),
                summary: Some("Background retry handler".to_string()),
                content_hash: "h1".to_string(),
                size_bytes: 80,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["pub fn retry_job() {}".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id),
                path: "/repo/notes.md".to_string(),
                kind: ObjectKind::Note,
                title: "notes.md".to_string(),
                summary: Some("Billing notes".to_string()),
                content_hash: "h2".to_string(),
                size_bytes: 60,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["Billing notes overview.".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        // `worker.rs` has no textual occurrence of "billing", yet tag match surfaces it cleanly.
        store
            .add_tags_scoped("worker.rs", Some("tagboost"), &["billing".into()])
            .unwrap();
        let hits = store.search("billing", Some("tagboost"), 5).unwrap();
        let worker_hit = hits.iter().find(|h| h.title == "worker.rs").expect("tagged object found");
        assert!(worker_hit.signals.iter().any(|s| s.label == "tag match (billing)"));
    }

    #[test]
    fn search_since_filter_restricts_fts_tag_and_metadata_queries() {
        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("sincerepo", None, None).unwrap();

        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id.clone()),
                path: "/repo/old_sched.rs".to_string(),
                kind: ObjectKind::Code,
                title: "old_sched.rs".to_string(),
                summary: Some("Legacy scheduler".to_string()),
                content_hash: "h_old".to_string(),
                size_bytes: 60,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["pub fn schedule_old() {}".to_string()],
            })
            .unwrap();
        batch
            .upsert_object(crate::UpsertObjectInput {
                project_id: Some(proj.id),
                path: "/repo/new_sched.rs".to_string(),
                kind: ObjectKind::Code,
                title: "new_sched.rs".to_string(),
                summary: Some("Current scheduler".to_string()),
                content_hash: "h_new".to_string(),
                size_bytes: 60,
                modified_at: 1_000,
                is_noise: false,
                chunks: vec!["pub fn schedule_new() {}".to_string()],
            })
            .unwrap();
        batch.commit().unwrap();

        let fts_recent = store
            .search_with_filters(
                Some("scheduler"),
                SearchFilters {
                    project: Some("sincerepo"),
                    since: Some(500),
                    ..SearchFilters::default()
                },
            )
            .unwrap();
        assert_eq!(fts_recent.len(), 1);
        assert_eq!(fts_recent[0].title, "new_sched.rs");

        let meta_recent = store
            .search_with_filters(
                None,
                SearchFilters {
                    project: Some("sincerepo"),
                    since: Some(500),
                    ..SearchFilters::default()
                },
            )
            .unwrap();
        assert_eq!(meta_recent.len(), 1);
        assert_eq!(meta_recent[0].title, "new_sched.rs");
    }
}