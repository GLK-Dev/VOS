use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn run_vos(db: &Path, args: &[&str]) -> (bool, String, String) {
    let bin = env!("CARGO_BIN_EXE_vos");
    let out = Command::new(bin)
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .expect("execute vos binary");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_vos_ok(db: &Path, args: &[&str]) -> String {
    let (ok, stdout, stderr) = run_vos(db, args);
    assert!(
        ok,
        "vos {:?} failed.\nstdout:\n{stdout}\nstderr:\n{stderr}",
        args
    );
    stdout
}

#[test]
fn cli_full_v1_workflow_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("alpha");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::create_dir_all(proj.join("docs")).unwrap();
    fs::create_dir_all(proj.join("config")).unwrap();

    fs::write(
        proj.join("README.md"),
        "# Alpha\nAlpha is a deterministic project memory engine.",
    )
    .unwrap();
    fs::write(
        proj.join("docs/ROADMAP.md"),
        "# Roadmap\n- [ ] Build scheduler fairness tests\nOPEN QUESTION: keep WAL?",
    )
    .unwrap();
    fs::write(
        proj.join("src/scheduler.rs"),
        "pub fn schedule_task() {\n    // fair queue scheduler\n}\n",
    )
    .unwrap();
    fs::write(
        proj.join("config/concepts.yaml"),
        "event log:\n  - scheduler\n",
    )
    .unwrap();

    let created = assert_vos_ok(
        &db,
        &[
            "project",
            "create",
            "alpha",
            proj.to_str().unwrap(),
            "--description",
            "Alpha workspace",
        ],
    );
    assert!(created.contains("Created project 'alpha'"));

    let indexed = assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "alpha"]);
    assert!(indexed.contains("Scanned:   4"));
    assert!(indexed.contains("added: 4"));

    // Repeat index must be 100% unchanged.
    let reindexed = assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "alpha"]);
    assert!(reindexed.contains("Unchanged: 4"));

    let list = assert_vos_ok(&db, &["project", "list"]);
    assert!(list.contains("alpha"));

    let search = assert_vos_ok(&db, &["search", "scheduler", "--explain"]);
    assert!(search.contains("scheduler.rs"));
    assert!(search.contains("signals:"));

    let tag_out = assert_vos_ok(&db, &["tag", "scheduler.rs", "kernel", "core"]);
    assert!(tag_out.contains("Tagged 'scheduler.rs'"));

    let show_out = assert_vos_ok(&db, &["show", "scheduler.rs"]);
    assert!(show_out.contains("kernel (manual)"));

    let why_out = assert_vos_ok(&db, &["why", "scheduler.rs"]);
    assert!(why_out.contains("Why it matters:"));

    let rel_out = assert_vos_ok(&db, &["related", "scheduler"]);
    assert!(rel_out.contains("scheduler.rs"));

    let resume_out = assert_vos_ok(&db, &["resume", "alpha"]);
    assert!(resume_out.contains("Suggested starting points:"));

    let start_out = assert_vos_ok(&db, &["start", "alpha"]);
    assert!(start_out.contains("Open questions:"));
    assert!(start_out.contains("keep WAL?"));

    let cont_out = assert_vos_ok(&db, &["continue", "alpha"]);
    assert!(cont_out.contains("Continue from:"));

    let ctx_out = assert_vos_ok(&db, &["context", "alpha"]);
    assert!(ctx_out.contains("Important recent files:"));

    let act_out = assert_vos_ok(&db, &["activity", "alpha", "--days", "7"]);
    assert!(act_out.contains("added"));

    let tl_out = assert_vos_ok(&db, &["timeline", "alpha"]);
    assert!(tl_out.contains("scheduler.rs"));

    let status_out = assert_vos_ok(&db, &["status", "alpha"]);
    assert!(status_out.contains("Health: Active"));
    assert!(status_out.contains("Pending work:"));

    let explain_out = assert_vos_ok(&db, &["explain", "project memory", "--project", "alpha"]);
    assert!(explain_out.contains("Files contributing:"));

    let evo_out = assert_vos_ok(&db, &["evolution", "alpha", "--since", "30d"]);
    assert!(evo_out.contains("Evolution: alpha"));

    let drift_out = assert_vos_ok(&db, &["drift", "alpha"]);
    assert!(drift_out.contains("roadmap drift"));

    let cs_out = assert_vos_ok(&db, &["changed-since", "7d", "alpha"]);
    assert!(cs_out.contains("Added (4):"));

    // Run YAML benchmark & save history
    let bench_yaml = tmp.path().join("bench.yaml");
    fs::write(
        &bench_yaml,
        "- query: scheduler\n  expected_top1:\n    - src/scheduler.rs\n",
    )
    .unwrap();
    let bench_out = assert_vos_ok(
        &db,
        &[
            "benchmark",
            "search",
            "--config",
            bench_yaml.to_str().unwrap(),
            "--save",
        ],
    );
    assert!(bench_out.contains("Top-1: 1/1 (100.0%)"));

    let hist_out = assert_vos_ok(&db, &["benchmark", "history"]);
    assert!(hist_out.contains("bench.yaml"));
}

#[test]
fn cli_handles_soft_delete_and_restore_idempotently() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("repo");
    fs::create_dir_all(&proj).unwrap();
    let note = proj.join("notes.md");
    fs::write(&note, "# Notes\nunique_token_alpha\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "repo", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "repo"]);
    assert!(assert_vos_ok(&db, &["search", "unique_token_alpha"]).contains("notes.md"));

    // Delete file and re-index -> hidden from search.
    fs::remove_file(&note).unwrap();
    let del_idx = assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "repo"]);
    assert!(del_idx.contains("Deleted:   1"));
    assert!(assert_vos_ok(&db, &["search", "unique_token_alpha"]).contains("No matches found"));

    // Recreate file and re-index -> restored and searchable again.
    fs::write(&note, "# Notes\nunique_token_alpha restored\n").unwrap();
    let rest_idx = assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "repo"]);
    assert!(rest_idx.contains("restored: 1"));
    assert!(assert_vos_ok(&db, &["search", "unique_token_alpha"]).contains("notes.md"));
    assert!(assert_vos_ok(&db, &["timeline", "repo"]).contains("restored:"));
}

#[test]
fn cli_disambiguates_shared_filenames_by_project_flag() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("README.md"), "# A").unwrap();
    fs::write(b.join("README.md"), "# B").unwrap();

    assert_vos_ok(&db, &["project", "create", "a", a.to_str().unwrap()]);
    assert_vos_ok(&db, &["project", "create", "b", b.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", a.to_str().unwrap(), "--project", "a"]);
    assert_vos_ok(&db, &["index", b.to_str().unwrap(), "--project", "b"]);

    let (ok, _, stderr) = run_vos(&db, &["why", "README.md"]);
    assert!(!ok);
    assert!(stderr.contains("ambiguous object"));

    let scoped = assert_vos_ok(&db, &["why", "README.md", "--project", "a"]);
    assert!(scoped.contains("Project:\n  a"));
}

#[test]
fn cli_builds_and_queries_dependency_graph() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("engine");
    fs::create_dir_all(proj.join("src/transformer")).unwrap();
    fs::create_dir_all(proj.join("docs")).unwrap();

    fs::write(
        proj.join("README.md"),
        "# Engine\nSee [Design](docs/DESIGN.md).\n",
    )
    .unwrap();
    fs::write(proj.join("docs/DESIGN.md"), "# Design\nDetails for `kv_cache.rs`.\n").unwrap();
    fs::write(proj.join("src/lib.rs"), "pub mod transformer;\n").unwrap();
    fs::write(
        proj.join("src/transformer/mod.rs"),
        "pub mod kv_cache;\npub mod attention;\n",
    )
    .unwrap();
    fs::write(proj.join("src/transformer/kv_cache.rs"), "pub struct Kv;\n").unwrap();
    fs::write(
        proj.join("src/transformer/attention.rs"),
        "use super::kv_cache;\n",
    )
    .unwrap();

    assert_vos_ok(&db, &["project", "create", "engine", proj.to_str().unwrap()]);
    let idx_out = assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "engine"]);
    assert!(idx_out.contains("Relations:"));

    let graph_out = assert_vos_ok(&db, &["graph", "engine"]);
    assert!(graph_out.contains("Graph: engine"));
    assert!(graph_out.contains("transformer"));

    let focused = assert_vos_ok(
        &db,
        &[
            "graph",
            "engine",
            "--target",
            "transformer/mod.rs",
            "--direction",
            "both",
            "--depth",
            "2",
        ],
    );
    assert!(focused.contains("kv_cache.rs [rust_mod]"));
    assert!(focused.contains("attention.rs [rust_mod]"));
    assert!(focused.contains("lib.rs [rust_mod]"));

    let why_out = assert_vos_ok(
        &db,
        &["why", "transformer/mod.rs", "--project", "engine"],
    );
    assert!(why_out.contains("Dependencies:"));
    assert!(why_out.contains("Referenced by:"));
}

#[test]
fn cli_reports_activity_ownership_and_relation_weighted_coverage() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("insight");
    fs::create_dir_all(proj.join("docs")).unwrap();
    fs::create_dir_all(proj.join("src/core")).unwrap();
    fs::create_dir_all(proj.join("src/net")).unwrap();

    fs::write(
        proj.join("docs/GUIDE.md"),
        "# Guide\nCore entrypoint: [engine.rs](../src/core/engine.rs).\n",
    )
    .unwrap();
    fs::write(proj.join("src/core/engine.rs"), "pub fn run() {}\n").unwrap();
    fs::write(proj.join("src/net/socket.rs"), "pub fn listen() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "insight", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "insight"]);

    let own_out = assert_vos_ok(&db, &["ownership", "insight", "--days", "14"]);
    assert!(own_out.contains("Ownership: insight"));
    assert!(own_out.contains("Most Active Areas"));
    assert!(own_out.contains("Recently Active (14d)"));
    assert!(own_out.contains("Most Stable Areas"));

    let cov_out = assert_vos_ok(&db, &["coverage", "insight"]);
    assert!(cov_out.contains("Code Files: 2"));
    assert!(cov_out.contains("Docs Files: 1"));
    assert!(cov_out.contains("Coverage:   50.0%"));
    assert!(cov_out.contains("Well Covered"));
    assert!(cov_out.contains("src/core"));
    assert!(cov_out.contains("Underdocumented"));
    assert!(cov_out.contains("src/net"));
}

#[test]
fn cli_continue_status_and_drift_surface_insight_layer_signals() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("hubrepo");
    fs::create_dir_all(proj.join("src/engine")).unwrap();

    fs::write(proj.join("README.md"), "# HubRepo\nOverview doc.\n").unwrap();
    fs::write(
        proj.join("src/engine/mod.rs"),
        "pub mod a;\npub mod b;\npub mod c;\n",
    )
    .unwrap();
    fs::write(proj.join("src/engine/a.rs"), "pub fn a() {}\n").unwrap();
    fs::write(proj.join("src/engine/b.rs"), "pub fn b() {}\n").unwrap();
    fs::write(proj.join("src/engine/c.rs"), "pub fn c() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "hubrepo", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "hubrepo"]);

    let cont_out = assert_vos_ok(&db, &["continue", "hubrepo"]);
    assert!(cont_out.contains("Hub node (3 relations)"));
    assert!(cont_out.contains("Ownership hotspots"));
    assert!(cont_out.contains("Coverage warnings"));
    assert!(cont_out.contains("Dependency hubs"));

    let status_out = assert_vos_ok(&db, &["status", "hubrepo"]);
    assert!(status_out.contains("Critical Risks:"));
    assert!(status_out.contains("Documentation Health:"));
    assert!(status_out.contains("high activity + low coverage"));

    let drift_out = assert_vos_ok(&db, &["drift", "hubrepo"]);
    assert!(drift_out.contains("[structural risk drift]"));
    assert!(drift_out.contains("src/engine"));
}

#[test]
fn cli_promotes_high_risk_hub_with_open_todos_to_critical_risk() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("critrepo");
    fs::create_dir_all(proj.join("src/compute")).unwrap();

    fs::write(proj.join("README.md"), "# CritRepo\nOverview.\n").unwrap();
    fs::write(
        proj.join("src/compute/mod.rs"),
        "// TODO: stabilize batching API\npub mod a;\npub mod b;\npub mod c;\npub mod d;\npub mod e;\npub mod f;\n",
    )
    .unwrap();
    for ch in ['a', 'b', 'c', 'd', 'e', 'f'] {
        fs::write(
            proj.join(format!("src/compute/{ch}.rs")),
            format!("// FIXME: check bounds in {ch}\npub fn fn_{ch}() {{}}\n"),
        )
        .unwrap();
    }

    assert_vos_ok(&db, &["project", "create", "critrepo", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "critrepo"]);

    let status_out = assert_vos_ok(&db, &["status", "critrepo"]);
    assert!(status_out.contains("Critical Risks: 1"));
    assert!(status_out.contains("Critical Areas:"));
    assert!(status_out.contains("src/compute"));
    assert!(status_out.contains("TODOs:"));

    let drift_out = assert_vos_ok(&db, &["drift", "critrepo"]);
    assert!(drift_out.contains("[critical risk]"));
    assert!(drift_out.contains("Critical Risk"));
}
#[test]
fn cli_status_experimental_predictor_is_opt_in_and_explainable() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("expproj");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("README.md"), "# ExpProj\nOPEN QUESTION: keep cache?\n").unwrap();
    fs::write(proj.join("src/lib.rs"), "pub fn init() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "expproj", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "expproj"]);

    // Default `vos status` must stay untouched.
    let baseline = assert_vos_ok(&db, &["status", "expproj"]);
    assert!(!baseline.contains("Experimental Insights:"));
    assert!(!baseline.contains("Recovery Difficulty:"));

    // `--experimental` enables the predictor.
    let exp_out = assert_vos_ok(&db, &["status", "expproj", "--experimental"]);
    assert!(exp_out.contains("Experimental Insights:"));
    assert!(exp_out.contains("Recovery Difficulty:"));
    assert!(exp_out.contains("Factors:"));

    // `VOS_EXPERIMENTAL=1` also enables it cleanly.
    let bin = env!("CARGO_BIN_EXE_vos");
    let env_out = Command::new(bin)
        .env("VOS_EXPERIMENTAL", "1")
        .arg("--db")
        .arg(&db)
        .args(["status", "expproj"])
        .output()
        .expect("execute vos with VOS_EXPERIMENTAL");
    assert!(env_out.status.success());
    let stdout = String::from_utf8_lossy(&env_out.stdout);
    assert!(stdout.contains("Recovery Difficulty:"));
}
