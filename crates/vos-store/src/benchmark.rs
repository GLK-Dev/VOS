use std::path::Path;

use vos_core::{SearchBenchmarkCase, SearchBenchmarkCaseResult, SearchBenchmarkReport, SearchHit};

use crate::{Store, StoreError};

/// Parses a YAML search benchmark specification file.
pub fn load_benchmark_cases(path: &Path) -> Result<Vec<SearchBenchmarkCase>, StoreError> {
    let raw = std::fs::read_to_string(path)?;
    serde_yaml::from_str(&raw)
        .map_err(|err| StoreError::InvalidInput(format!("invalid benchmark YAML: {err}")))
}

impl Store {
    /// Evaluates a slice of benchmark queries against this store and computes Top-1/Top-3/Top-5 hit rates.
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

        for case in cases {
            let hits = self.search(&case.query, case.project.as_deref(), 5)?;
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

            results.push(SearchBenchmarkCaseResult {
                query: case.query.clone(),
                project: case.project.clone(),
                top1_hit,
                top3_hit,
                top5_hit,
                actual_top5,
            });
        }

        Ok(SearchBenchmarkReport {
            total_cases: cases.len(),
            top1_passed,
            top1_total,
            top3_passed,
            top3_total,
            top5_passed,
            top5_total,
            cases: results,
        })
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
