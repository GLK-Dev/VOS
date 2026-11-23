use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::params;
use vos_core::{ObjectKind, SearchHit, SearchSignal};

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
/// Additional score per extra matching chunk in the same file (capped).
const MULTI_CHUNK_STEP_BOOST: f64 = 1.5;
const MULTI_CHUNK_MAX_BOOST: f64 = 6.0;
/// Penalty applied to lockfiles and housekeeping noise during search ranking.
const NOISE_PENALTY: f64 = -10.0;
/// Minimum non-noise Stage 1 (AND) hits before triggering Stage 2 (OR) fallback.
const STAGE2_FALLBACK_MIN_HITS: usize = 3;

struct SearchCandidate {
    hit: SearchHit,
    chunk_hits: usize,
    matched_object_fts: bool,
    matched_stage1_and: bool,
    best_chunk_bm25: Option<f64>,
    object_bm25: Option<f64>,
    observed_text_lower: String,
}

impl Store {
    /// Two-stage FTS5 search: strict `AND` first, with automatic `OR` fallback on sparse multi-term queries.
    pub fn search(
        &self,
        query: &str,
        project_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StoreError> {
        let and_query = build_fts_query(query)?;
        let query_terms: Vec<String> = query
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| t.to_lowercase())
            .collect();
        let phrase_lower = query_terms.join(" ");

        let requested_limit = limit.clamp(1, 200);
        let pool_limit = (requested_limit * 6).max(60) as i64;
        let mut merged: HashMap<String, SearchCandidate> = HashMap::new();

        self.collect_fts_stage(&and_query, project_name, pool_limit, true, &mut merged)?;

        let stage1_clean = merged.values().filter(|c| !c.hit.is_noise).count();
        if query_terms.len() > 1
            && stage1_clean < STAGE2_FALLBACK_MIN_HITS
            && let Some(or_query) = crate::queries::build_fts_or_query(&query_terms)
        {
            self.collect_fts_stage(&or_query, project_name, pool_limit, false, &mut merged)?;
        }

        let mut hits: Vec<SearchHit> = merged
            .into_values()
            .map(|mut cand| {
                let (score, signals) = score_search_candidate(&cand, &query_terms, &phrase_lower);
                cand.hit.score = score;
                cand.hit.signals = signals;
                cand.hit
            })
            .collect();

        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(requested_limit);
        Ok(hits)
    }

    fn collect_fts_stage(
        &self,
        fts_query: &str,
        project_name: Option<&str>,
        pool_limit: i64,
        is_stage1_and: bool,
        merged: &mut HashMap<String, SearchCandidate>,
    ) -> Result<(), StoreError> {
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
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(params![fts_query, project_name, pool_limit], |row| {
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
                        signals: Vec::new(),
                    },
                    chunk_hits: 0,
                    matched_object_fts: false,
                    matched_stage1_and: is_stage1_and,
                    best_chunk_bm25: None,
                    object_bm25: None,
                    observed_text_lower: String::new(),
                });

                if is_stage1_and {
                    entry.matched_stage1_and = true;
                }
                if is_stage1_and || is_new {
                    entry.chunk_hits += 1;
                }
                if entry.observed_text_lower.len() < 4096 {
                    entry.observed_text_lower.push('\n');
                    entry.observed_text_lower.push_str(&chunk_text.to_lowercase());
                }
                if entry.best_chunk_bm25.is_none_or(|prev| bm25_rank < prev) {
                    entry.best_chunk_bm25 = Some(bm25_rank);
                    entry.hit.snippet = snippet;
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
                 ORDER BY rank
                 LIMIT ?3",
            )?;

            let rows = stmt.query_map(params![fts_query, project_name, pool_limit], |row| {
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
                        signals: Vec::new(),
                    },
                    chunk_hits: 0,
                    matched_object_fts: false,
                    matched_stage1_and: is_stage1_and,
                    best_chunk_bm25: None,
                    object_bm25: None,
                    observed_text_lower: String::new(),
                });
                if is_stage1_and {
                    entry.matched_stage1_and = true;
                }
                entry.matched_object_fts = true;
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
) -> (f64, Vec<SearchSignal>) {
    let mut signals = Vec::new();
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
            delta: TITLE_EXACT_MATCH_BOOST,
        });
    } else if partial_title_hit {
        signals.push(SearchSignal {
            label: "title contains term".to_string(),
            delta: TITLE_CONTAINS_BOOST,
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
                delta: PHRASE_MATCH_BOOST,
            });
        }
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
