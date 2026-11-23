use std::path::PathBuf;

use vos_indexer::index_path;
use vos_store::{Store, load_benchmark_cases};

#[test]
fn fixture_search_benchmark_meets_accuracy_bar() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");

    let fixtures = repo_root.join("tests").join("fixtures");
    let yaml_path = repo_root.join("tests").join("search_benchmark.yaml");
    let reg_path = repo_root.join("tests").join("search_regressions.yaml");

    let mut store = Store::open_in_memory().expect("in-memory store");

    for name in ["project_a", "project_b", "project_c"] {
        let dir = fixtures.join(name);
        store
            .create_project(name, Some(&dir), None)
            .expect("create fixture project");
        index_path(&mut store, &dir, Some(name)).expect("index fixture project");
    }

    let cases = load_benchmark_cases(&yaml_path).expect("load 50-query fixture YAML");
    assert_eq!(cases.len(), 50);

    let report = store
        .run_search_benchmark(&cases)
        .expect("run fixture search benchmark");

    let top1_pct = (report.top1_passed as f64 / report.top1_total.max(1) as f64) * 100.0;
    let top3_pct = (report.top3_passed as f64 / report.top3_total.max(1) as f64) * 100.0;
    let top5_pct = (report.top5_passed as f64 / report.top5_total.max(1) as f64) * 100.0;
    let noise_pct = (report.noise_hits as f64 / report.total_returned_hits.max(1) as f64) * 100.0;

    assert!(
        top1_pct > 75.0,
        "Top-1 below 75%: {top1_pct:.1}% ({}/{})",
        report.top1_passed,
        report.top1_total
    );
    assert!(
        top3_pct > 90.0,
        "Top-3 below 90%: {top3_pct:.1}% ({}/{})",
        report.top3_passed,
        report.top3_total
    );
    assert!(
        top5_pct > 95.0,
        "Top-5 below 95%: {top5_pct:.1}% ({}/{})",
        report.top5_passed,
        report.top5_total
    );
    assert!(
        noise_pct < 5.0,
        "Noise hits >= 5%: {noise_pct:.1}% ({}/{})",
        report.noise_hits,
        report.total_returned_hits
    );
    assert_eq!(
        report.cross_project_matched, report.cross_project_total,
        "Cross-project recall below 100%"
    );

    // Check historical regressions against the fixture corpus as well.
    let reg_cases = load_benchmark_cases(&reg_path).expect("load regressions YAML");
    let reg_report = store
        .run_search_benchmark(&reg_cases)
        .expect("run regression checks");
    assert_eq!(reg_report.top3_passed, reg_report.top3_total);
}
