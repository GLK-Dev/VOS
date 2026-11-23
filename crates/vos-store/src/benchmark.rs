use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use rusqlite::params;
use vos_core::util::now_unix;
use vos_core::{
    BenchmarkRunRecord, CategoryBenchmarkStats, SearchBenchmarkCase, SearchBenchmarkCaseResult,
    SearchBenchmarkReport, SearchHit,
};

use crate::{Store, StoreError};

/// Parses a YAML search benchmark specification file.
pub fn load_benchmark_cases(path: &Path) -> Result<Vec<SearchBenchmarkCase>, StoreError> {
    let raw = std::fs::read_to_string(path)?;
    serde_yaml::from_str(&raw)
        .map_err(|err| StoreError::InvalidInput(format!("invalid benchmark YAML: {err}")))
}

impl Store {
    /// Evaluates benchmark queries and computes Top-K accuracy, MRR, noise rate, and cross-project recall.
    pub fn run_search_benchmark(
        &self,
        cases: &[SearchBenchmarkCase],
    ) -> Result<SearchBenchmarkReport, StoreError> {
        let mut results = Vec::with_capacity(cases.len());
        let mut top1_passed = 0_usize;
        let mut top1_total = 0_usize;
        let mut top3_passed = 0_usize;
        let mut top3_total = 0_usize;
        let mut top5_passed = 0_usize;
        let mut top5_total = 0_usize;
        let mut rr_sum = 0.0_f64;
        let mut rr_count = 0_usize;
        let mut noise_hits = 0_usize;
        let mut total_returned_hits = 0_usize;
        let mut cross_matched = 0_usize;
        let mut cross_total = 0_usize;

        let mut cat_map: BTreeMap<String, CategoryBenchmarkStats> = BTreeMap::new();

        for case in cases {
            let category = case
                .category
                .clone()
                .unwrap_or_else(|| "Uncategorized".to_string());
            let hits = self.search(&case.query, case.project.as_deref(), 5)?;

            let case_noise = hits.iter().filter(|h| h.is_noise).count();
            noise_hits += case_noise;
            total_returned_hits += hits.len();

            let actual_top5: Vec<String> = hits
                .iter()
                .map(|h| {
                    let proj = h.project_name.as_deref().unwrap_or("-");
                    format!("{proj}:{}", h.path)
                })
                .collect();

            let top1_hit = if !case.expected_top1.is_empty() {
                top1_total += 1;
                let ok = hits
                    .first()
                    .is_some_and(|h| matches_expected(h, &case.expected_top1));
                if ok {
                    top1_passed += 1;
                }
                Some(ok)
            } else {
                None
            };

            let top3_targets = if !case.expected_top3.is_empty() {
                &case.expected_top3
            } else {
                &case.expected_top1
            };
            let top3_hit = if !top3_targets.is_empty() {
                top3_total += 1;
                let ok = hits
                    .iter()
                    .take(3)
                    .any(|h| matches_expected(h, top3_targets));
                if ok {
                    top3_passed += 1;
                }
                Some(ok)
            } else {
                None
            };

            let top5_targets = if !case.expected_top5.is_empty() {
                &case.expected_top5
            } else if !case.expected_top3.is_empty() {
                &case.expected_top3
            } else {
                &case.expected_top1
            };
            let top5_hit = if !top5_targets.is_empty() {
                top5_total += 1;
                let ok = hits
                    .iter()
                    .take(5)
                    .any(|h| matches_expected(h, top5_targets));
                if ok {
                    top5_passed += 1;
                }
                Some(ok)
            } else {
                None
            };

            // Compute reciprocal rank across the union of specified file expectations.
            let mut mrr_targets: Vec<String> = Vec::new();
            for list in [&case.expected_top1, &case.expected_top3, &case.expected_top5] {
                for item in list {
                    if !mrr_targets.contains(item) {
                        mrr_targets.push(item.clone());
                    }
                }
            }
            let reciprocal_rank = if !mrr_targets.is_empty() {
                rr_count += 1;
                let rr = hits
                    .iter()
                    .position(|h| matches_expected(h, &mrr_targets))
                    .map(|idx| 1.0 / (idx + 1) as f64)
                    .unwrap_or(0.0);
                rr_sum += rr;
                Some(rr)
            } else {
                None
            };

            // Cross-project recall across Top-5 hits.
            let mut case_cross_matched = 0_usize;
            let case_cross_total = case.expected_projects.len();
            if case_cross_total > 0 {
                let returned_projects: HashSet<String> = hits
                    .iter()
                    .filter_map(|h| h.project_name.as_ref().map(|p| p.to_lowercase()))
                    .collect();
                for expected_proj in &case.expected_projects {
                    if returned_projects.contains(&expected_proj.to_lowercase()) {
                        case_cross_matched += 1;
                    }
                }
                cross_matched += case_cross_matched;
                cross_total += case_cross_total;
            }

            let cat_entry = cat_map
                .entry(category.clone())
                .or_insert_with(|| CategoryBenchmarkStats {
                    category: category.clone(),
                    cases: 0,
                    top1_passed: 0,
                    top1_total: 0,
                    top3_passed: 0,
                    top3_total: 0,
                    top5_passed: 0,
                    top5_total: 0,
                });
            cat_entry.cases += 1;
            if let Some(ok) = top1_hit {
                cat_entry.top1_total += 1;
                cat_entry.top1_passed += usize::from(ok);
            }
            if let Some(ok) = top3_hit {
                cat_entry.top3_total += 1;
                cat_entry.top3_passed += usize::from(ok);
            }
            if let Some(ok) = top5_hit {
                cat_entry.top5_total += 1;
                cat_entry.top5_passed += usize::from(ok);
            }

            results.push(SearchBenchmarkCaseResult {
                query: case.query.clone(),
                category,
                project: case.project.clone(),
                top1_hit,
                top3_hit,
                top5_hit,
                reciprocal_rank,
                noise_hits: case_noise,
                returned_hits: hits.len(),
                cross_project_matched: case_cross_matched,
                cross_project_total: case_cross_total,
                actual_top5,
            });
        }

        let mrr = if rr_count == 0 {
            0.0
        } else {
            rr_sum / rr_count as f64
        };

        Ok(SearchBenchmarkReport {
            total_cases: cases.len(),
            top1_passed,
            top1_total,
            top3_passed,
            top3_total,
            top5_passed,
            top5_total,
            mrr,
            noise_hits,
            total_returned_hits,
            cross_project_matched: cross_matched,
            cross_project_total: cross_total,
            categories: cat_map.into_values().collect(),
            cases: results,
        })
    }

    /// Persists a benchmark report into `benchmark_runs` for historical regression tracking.
    pub fn save_benchmark_run(
        &self,
        suite_name: &str,
        report: &SearchBenchmarkReport,
    ) -> Result<i64, StoreError> {
        let now = now_unix();
        self.conn.execute(
            "INSERT INTO benchmark_runs (
                suite_name, total_cases, top1_passed, top1_total, top3_passed, top3_total,
                top5_passed, top5_total, mrr, noise_hits, total_returned_hits,
                cross_project_matched, cross_project_total, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                suite_name,
                report.total_cases as i64,
                report.top1_passed as i64,
                report.top1_total as i64,
                report.top3_passed as i64,
                report.top3_total as i64,
                report.top5_passed as i64,
                report.top5_total as i64,
                report.mrr,
                report.noise_hits as i64,
                report.total_returned_hits as i64,
                report.cross_project_matched as i64,
                report.cross_project_total as i64,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Returns recent saved benchmark runs (newest first).
    pub fn list_benchmark_runs(&self, limit: usize) -> Result<Vec<BenchmarkRunRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, suite_name, total_cases, top1_passed, top1_total, top3_passed, top3_total,
                    top5_passed, top5_total, mrr, noise_hits, total_returned_hits,
                    cross_project_matched, cross_project_total, created_at
             FROM benchmark_runs
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit.clamp(1, 100) as i64], |row| {
            Ok(BenchmarkRunRecord {
                id: row.get(0)?,
                suite_name: row.get(1)?,
                total_cases: row.get::<_, i64>(2)?.max(0) as usize,
                top1_passed: row.get::<_, i64>(3)?.max(0) as usize,
                top1_total: row.get::<_, i64>(4)?.max(0) as usize,
                top3_passed: row.get::<_, i64>(5)?.max(0) as usize,
                top3_total: row.get::<_, i64>(6)?.max(0) as usize,
                top5_passed: row.get::<_, i64>(7)?.max(0) as usize,
                top5_total: row.get::<_, i64>(8)?.max(0) as usize,
                mrr: row.get(9)?,
                noise_hits: row.get::<_, i64>(10)?.max(0) as usize,
                total_returned_hits: row.get::<_, i64>(11)?.max(0) as usize,
                cross_project_matched: row.get::<_, i64>(12)?.max(0) as usize,
                cross_project_total: row.get::<_, i64>(13)?.max(0) as usize,
                created_at: row.get(14)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
    }
}

fn matches_expected(hit: &SearchHit, expected: &[String]) -> bool {
    let norm_actual = hit.path.replace('\\', "/").to_lowercase();
    expected.iter().any(|pattern| {
        let norm_exp = pattern
            .trim()
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_lowercase();
        norm_actual == norm_exp || norm_actual.ends_with(&format!("/{norm_exp}"))
    })
}

