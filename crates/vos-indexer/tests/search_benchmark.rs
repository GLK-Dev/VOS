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

    let mut store = Store::open_in_memory().expect("in-memory store");

    for name in ["project_a", "project_b", "project_c"] {
        let dir = fixtures.join(name);
        store
            .create_project(name, Some(&dir), None)
            .expect("create fixture project");
        index_path(&mut store, &dir, Some(name)).expect("index fixture project");
    }

    let cases = load_benchmark_cases(&yaml_path).expect("load fixture YAML");
    let report = store
        .run_search_benchmark(&cases)
        .expect("run fixture search benchmark");

    assert_eq!(report.total_cases, 10);
    assert_eq!(
        report.top1_passed, report.top1_total,
        "Top-1 failures: {:?}",
        report.cases
    );
    assert_eq!(
        report.top3_passed, report.top3_total,
        "Top-3 failures: {:?}",
        report.cases
    );
    assert_eq!(
        report.top5_passed, report.top5_total,
        "Top-5 failures: {:?}",
        report.cases
    );
}
