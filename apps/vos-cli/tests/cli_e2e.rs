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

    fs::write(proj.join("README.md"), "# ExpProj\nSee [lib](src/lib.rs).\n").unwrap();
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "expproj"]);

    let graph_exp = assert_vos_ok(&db, &["graph", "expproj", "--experimental"]);
    assert!(graph_exp.contains("Experimental Hub Importance:"));
    assert!(graph_exp.contains("Hub Score:"));

    let cov_exp = assert_vos_ok(&db, &["coverage", "expproj", "--experimental"]);
    assert!(cov_exp.contains("Experimental Documentation Quality:"));
    assert!(cov_exp.contains("Mean Quality Score:"));
}

#[test]
fn cli_snapshot_compare_and_session_tracking_work_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("mem");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("README.md"), "# Mem\n- [ ] Task one\n").unwrap();
    fs::write(proj.join("src/lib.rs"), "pub fn base() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "mem", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "mem"]);

    let snap1 = assert_vos_ok(&db, &["snapshot", "mem", "--label", "start"]);
    assert!(snap1.contains("Saved snapshot #1 [start]"));

    let sess_start = assert_vos_ok(&db, &["session", "start", "mem", "--note", "resume work"]);
    assert!(sess_start.contains("Predicted Difficulty:"));

    fs::write(proj.join("src/engine.rs"), "pub fn engine() {}\n").unwrap();
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "mem"]);

    let sess_stop = assert_vos_ok(
        &db,
        &["session", "stop", "mem", "--quality", "5", "--note", "done"],
    );
    assert!(sess_stop.contains("Actual Quality: 5/5"));

    let snap2 = assert_vos_ok(&db, &["snapshot", "mem", "--label", "after"]);
    assert!(snap2.contains("Saved snapshot #2 [after]"));

    let diff_out = assert_vos_ok(&db, &["compare-snapshot", "1", "2"]);
    assert!(diff_out.contains("Active Files:   3 (+1)"));

    let stats_out = assert_vos_ok(&db, &["session", "stats", "mem"]);
    assert!(stats_out.contains("total=1 completed=1 active=0"));
    assert!(stats_out.contains("Mean Recovery Quality: 5.00 / 5.0"));
}

#[test]
fn cli_json_output_and_why_experimental_blast_radius_work_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("jsonproj");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("README.md"), "# JsonProj\nSee [api](src/api.rs).\n").unwrap();
    fs::write(proj.join("benchmark_output.txt"), "Run 1\nPass\n").unwrap();
    fs::write(proj.join("src/core.rs"), "pub fn core_fn() {}\n").unwrap();
    fs::write(proj.join("src/api.rs"), "use crate::core;\npub fn call() {}\n").unwrap();
    fs::write(proj.join("src/cli.rs"), "use crate::api;\n").unwrap();

    let proj_json = assert_vos_ok(&db, &["project", "create", "jsonproj", proj.to_str().unwrap(), "--json"]);
    let proj_val: serde_json::Value = serde_json::from_str(&proj_json).expect("project json");
    assert_eq!(proj_val["name"], "jsonproj");

    let idx_json = assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "jsonproj", "--json"]);
    let idx_val: serde_json::Value = serde_json::from_str(&idx_json).expect("index json");
    assert_eq!(idx_val["added"], 5);

    // Log/benchmark output files must not be classified as markdown/note docs in DocQualityReport.
    let cov_json = assert_vos_ok(&db, &["coverage", "jsonproj", "--experimental", "--json"]);
    let cov_val: serde_json::Value = serde_json::from_str(&cov_json).expect("coverage json");
    assert_eq!(cov_val["doc_quality"]["total_docs"], 1);

    // Baseline `vos why` omits experimental blast radius; `--experimental` enables it.
    let why_base = assert_vos_ok(&db, &["why", "core.rs", "--project", "jsonproj"]);
    assert!(!why_base.contains("Experimental Blast Radius:"));

    let why_exp = assert_vos_ok(&db, &["why", "core.rs", "--project", "jsonproj", "--experimental"]);
    assert!(why_exp.contains("Experimental Blast Radius:"));
    assert!(why_exp.contains("Dependents: 1 direct / 3 transitive"));

    let why_json = assert_vos_ok(&db, &["why", "core.rs", "--project", "jsonproj", "--experimental", "--json"]);
    let why_val: serde_json::Value = serde_json::from_str(&why_json).expect("why json");
    assert_eq!(why_val["experimental_impact"]["direct_dependents"], 1);
    assert_eq!(why_val["experimental_impact"]["transitive_dependents"], 3);

    let status_json = assert_vos_ok(&db, &["status", "jsonproj", "--experimental", "--json"]);
    let st_val: serde_json::Value = serde_json::from_str(&status_json).expect("status json");
    assert!(st_val["recovery_prediction"]["difficulty"].is_string());

    let sess_json = assert_vos_ok(&db, &["session", "start", "jsonproj", "--json"]);
    let s_val: serde_json::Value = serde_json::from_str(&sess_json).expect("session json");
    assert_eq!(s_val["project_name"], "jsonproj");

    assert_vos_ok(
        &db,
        &["session", "stop", "jsonproj", "--quality", "5", "--note", "Next: finalize CLI"],
    );

    let cont_base = assert_vos_ok(&db, &["continue", "jsonproj"]);
    assert!(cont_base.contains("Last work session:"));
    assert!(cont_base.contains("Next: finalize CLI"));
    assert!(!cont_base.contains("Experimental Insights:"));

    let cont_exp_json = assert_vos_ok(&db, &["continue", "jsonproj", "--experimental", "--json"]);
    let cont_val: serde_json::Value = serde_json::from_str(&cont_exp_json).expect("continue json");
    assert_eq!(cont_val["last_session"]["stop_note"], "Next: finalize CLI");
    assert!(cont_val["recovery_prediction"]["difficulty"].is_string());
}

#[test]
fn cli_all_index_status_and_live_snapshot_diff_work_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let p1 = tmp.path().join("alpha");
    let p2 = tmp.path().join("beta");
    fs::create_dir_all(p1.join("src")).unwrap();
    fs::create_dir_all(p2.join("src")).unwrap();
    fs::write(p1.join("README.md"), "# Alpha\n").unwrap();
    fs::write(p1.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    fs::write(p2.join("src/main.rs"), "fn main() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "alpha", p1.to_str().unwrap()]);
    assert_vos_ok(&db, &["project", "create", "beta", p2.to_str().unwrap()]);

    let idx_all_json = assert_vos_ok(&db, &["index", "--all", "--json"]);
    let idx_val: serde_json::Value = serde_json::from_str(&idx_all_json).expect("index --all json");
    assert_eq!(idx_val["total_added"], 3);

    let st_all_json = assert_vos_ok(&db, &["status", "--all", "--experimental", "--json"]);
    let st_arr: Vec<serde_json::Value> = serde_json::from_str(&st_all_json).expect("status --all json");
    assert_eq!(st_arr.len(), 2);
    assert!(st_arr[0]["recovery_prediction"]["difficulty"].is_string());

    assert_vos_ok(&db, &["snapshot", "alpha", "--label", "before"]);
    fs::write(p1.join("README.md"), "# Alpha\n- [ ] Expand alpha\n").unwrap();
    fs::write(p1.join("src/extra.rs"), "pub fn b() {}\n").unwrap();
    assert_vos_ok(&db, &["index", "--all"]);

    let live_diff = assert_vos_ok(&db, &["compare-snapshot", "--project", "alpha", "--live"]);
    assert!(live_diff.contains("-> LIVE (alpha,"));
    assert!(live_diff.contains("Active Files:   3 (+1)"));
    assert!(live_diff.contains("New pending work (1):"));

    let snaps_json = assert_vos_ok(&db, &["snapshots", "alpha", "--json"]);
    let snaps_arr: Vec<serde_json::Value> = serde_json::from_str(&snaps_json).expect("snapshots json");
    assert_eq!(snaps_arr.len(), 1);
}

#[test]
fn cli_session_stop_auto_reindexes_disk_edits_unless_no_index_is_set() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("sessproj");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("README.md"), "# SessProj\n").unwrap();
    fs::write(proj.join("src/lib.rs"), "pub fn v1() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "sessproj", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "sessproj"]);
    std::thread::sleep(std::time::Duration::from_millis(1050));

    assert_vos_ok(&db, &["session", "start", "sessproj", "--note", "auto-refresh test"]);
    // Sleep across a 1-second boundary so file mtime and index event strictly follow started_at.
    std::thread::sleep(std::time::Duration::from_millis(1050));
    fs::write(proj.join("src/new_mod.rs"), "pub fn added() {}\n").unwrap();

    let stop_auto = assert_vos_ok(
        &db,
        &["session", "stop", "sessproj", "--quality", "5", "--json"],
    );
    let val_auto: serde_json::Value = serde_json::from_str(&stop_auto).expect("stop auto json");
    assert_eq!(val_auto["files_changed_count"], 1);
    assert_eq!(val_auto["productive_areas"], serde_json::json!(["src"]));

    // With `--no-index`, unindexed disk edits are ignored until next `vos index`.
    std::thread::sleep(std::time::Duration::from_millis(1050));
    assert_vos_ok(&db, &["session", "start", "sessproj", "--no-index"]);
    std::thread::sleep(std::time::Duration::from_millis(1050));
    fs::write(proj.join("src/ignored_mod.rs"), "pub fn later() {}\n").unwrap();

    let stop_no_idx = assert_vos_ok(
        &db,
        &["session", "stop", "sessproj", "--no-index", "--quality", "4", "--json"],
    );
    let val_no_idx: serde_json::Value = serde_json::from_str(&stop_no_idx).expect("stop no-index json");
    assert_eq!(val_no_idx["files_changed_count"], 0);
}

#[test]
fn cli_show_tag_and_untag_disambiguate_with_project_flag() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let p1 = tmp.path().join("repo1");
    let p2 = tmp.path().join("repo2");
    fs::create_dir_all(p1.join("src")).unwrap();
    fs::create_dir_all(p2.join("src")).unwrap();
    fs::write(p1.join("src/lib.rs"), "pub fn one() {}\n").unwrap();
    fs::write(p2.join("src/lib.rs"), "pub fn two() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "repo1", p1.to_str().unwrap()]);
    assert_vos_ok(&db, &["project", "create", "repo2", p2.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", "--all"]);

    assert_vos_ok(&db, &["tag", "lib.rs", "--project", "repo1", "core", "api"]);
    let shown = assert_vos_ok(&db, &["show", "lib.rs", "--project", "repo1", "--json"]);
    let shown_val: serde_json::Value = serde_json::from_str(&shown).expect("show json");
    assert_eq!(shown_val["project_name"], "repo1");
    assert_eq!(shown_val["tags"].as_array().unwrap().len(), 2);

    assert_vos_ok(&db, &["untag", "lib.rs", "--project", "repo1", "api"]);
    let after_untag = assert_vos_ok(&db, &["show", "lib.rs", "--project", "repo1", "--json"]);
    let after_val: serde_json::Value = serde_json::from_str(&after_untag).expect("show json after untag");
    assert_eq!(after_val["tags"].as_array().unwrap().len(), 1);
    assert_eq!(after_val["tags"][0][0], "core");

    let tags_json = assert_vos_ok(&db, &["tags", "--project", "repo1", "--json"]);
    let tags_arr: Vec<serde_json::Value> = serde_json::from_str(&tags_json).expect("tags json");
    assert_eq!(tags_arr.len(), 1);
    assert_eq!(tags_arr[0]["tag"], "core");

    let by_tag_json = assert_vos_ok(&db, &["search", "--tag", "core", "--project", "repo1", "--json"]);
    let by_tag_arr: Vec<serde_json::Value> = serde_json::from_str(&by_tag_json).expect("search --tag json");
    assert_eq!(by_tag_arr.len(), 1);
    assert_eq!(by_tag_arr[0]["title"], "lib.rs");

    let by_q_tag = assert_vos_ok(&db, &["search", "one", "--tag", "core", "--project", "repo1", "--json"]);
    let by_q_arr: Vec<serde_json::Value> = serde_json::from_str(&by_q_tag).expect("search query --tag json");
    assert_eq!(by_q_arr.len(), 1);

    let by_kind_code = assert_vos_ok(&db, &["search", "--kind", "code", "--project", "repo1", "--json"]);
    let kind_arr: Vec<serde_json::Value> = serde_json::from_str(&by_kind_code).expect("search --kind json");
    assert_eq!(kind_arr.len(), 1);
    assert_eq!(kind_arr[0]["kind"], "Code");

    let by_kind_note = assert_vos_ok(&db, &["search", "one", "--kind", "note", "--project", "repo1", "--json"]);
    assert!(serde_json::from_str::<Vec<serde_json::Value>>(&by_kind_note).unwrap().is_empty());
}

#[test]
fn cli_project_remove_purges_objects_fts_snapshots_and_sessions() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let p1 = tmp.path().join("tempproj");
    fs::create_dir_all(p1.join("src")).unwrap();
    fs::write(p1.join("README.md"), "# TempProj unique_marker_xyz\n").unwrap();
    fs::write(p1.join("src/lib.rs"), "pub fn unique_fn_xyz() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "tempproj", p1.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", p1.to_str().unwrap(), "--project", "tempproj"]);
    assert_vos_ok(&db, &["snapshot", "tempproj", "--label", "tmp"]);
    assert_vos_ok(&db, &["session", "start", "tempproj"]);
    assert_vos_ok(&db, &["session", "stop", "tempproj", "--quality", "4"]);

    let pre_hits = assert_vos_ok(&db, &["search", "unique_marker_xyz", "--json"]);
    assert_eq!(serde_json::from_str::<Vec<serde_json::Value>>(&pre_hits).unwrap().len(), 1);

    let rm_out = assert_vos_ok(&db, &["project", "remove", "tempproj", "--json"]);
    let rm_val: serde_json::Value = serde_json::from_str(&rm_out).expect("remove json");
    assert_eq!(rm_val["project"], "tempproj");
    assert_eq!(rm_val["removed_objects"], 2);

    // Ensure FTS5, snapshots, and sessions have no orphaned entries.
    let post_hits = assert_vos_ok(&db, &["search", "unique_marker_xyz", "--json"]);
    assert!(serde_json::from_str::<Vec<serde_json::Value>>(&post_hits).unwrap().is_empty());
    let snaps = assert_vos_ok(&db, &["snapshots", "--json"]);
    assert!(serde_json::from_str::<Vec<serde_json::Value>>(&snaps).unwrap().is_empty());
    let sess = assert_vos_ok(&db, &["session", "list", "--json"]);
    assert!(serde_json::from_str::<Vec<serde_json::Value>>(&sess).unwrap().is_empty());
}

#[test]
fn cli_doctor_reports_healthy_index_and_missing_root_paths() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("doccli");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("README.md"), "# DocCli\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "doccli", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "doccli"]);

    let doc_json = assert_vos_ok(&db, &["doctor", "--json"]);
    let doc_val: serde_json::Value = serde_json::from_str(&doc_json).expect("doctor json");
    assert_eq!(doc_val["healthy"], true);
    assert_eq!(doc_val["sqlite_ok"], true);
    assert_eq!(doc_val["objects_count"], 1);
    assert_eq!(doc_val["objects_fts_count"], 1);

    let repair_out = assert_vos_ok(&db, &["doctor", "--repair"]);
    assert!(repair_out.contains("VOS Doctor: HEALTHY"));
    assert!(repair_out.contains("Repair applied:"));
}

#[test]
fn cli_since_supports_hours_weeks_today_and_yesterday() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");
    let proj = tmp.path().join("sinceproj");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("src").join("lib.rs"), "pub fn tick() -> u32 { 1 }\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "sinceproj", proj.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", proj.to_str().unwrap(), "--project", "sinceproj"]);

    for since_arg in ["12h", "2w", "today", "yesterday"] {
        let cs_out = assert_vos_ok(&db, &["changed-since", since_arg, "sinceproj", "--json"]);
        let cs_val: serde_json::Value = serde_json::from_str(&cs_out).expect("changed-since json");
        assert_eq!(cs_val["since_label"], since_arg);
        assert!(!cs_val["added"].as_array().unwrap().is_empty());

        let evo_out = assert_vos_ok(&db, &["evolution", "sinceproj", "--since", since_arg, "--json"]);
        let evo_val: serde_json::Value = serde_json::from_str(&evo_out).expect("evolution json");
        assert_eq!(evo_val["project"]["name"], "sinceproj");
        assert_eq!(evo_val["since_label"], since_arg);
        assert!(evo_val["added_count"].as_u64().unwrap() >= 1);
    }

    let bad_out = run_vos(&db, &["changed-since", "-5d", "sinceproj"]);
    assert!(!bad_out.0);
}

#[test]
fn cli_snapshot_all_captures_all_projects_and_rejects_conflicting_args() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("vos.db");

    let empty_all = assert_vos_ok(&db, &["snapshot", "--all"]);
    assert!(empty_all.contains("No projects registered."));

    let p_a = tmp.path().join("alpha");
    let p_b = tmp.path().join("beta");
    fs::create_dir_all(p_a.join("src")).unwrap();
    fs::create_dir_all(p_b.join("src")).unwrap();
    fs::write(p_a.join("README.md"), "# Alpha\n").unwrap();
    fs::write(p_b.join("src").join("lib.rs"), "pub fn b() {}\n").unwrap();

    assert_vos_ok(&db, &["project", "create", "alpha", p_a.to_str().unwrap()]);
    assert_vos_ok(&db, &["project", "create", "beta", p_b.to_str().unwrap()]);
    assert_vos_ok(&db, &["index", "--all"]);

    let snap_json = assert_vos_ok(&db, &["snapshot", "--all", "--label", "pre-break", "--json"]);
    let snaps: Vec<serde_json::Value> = serde_json::from_str(&snap_json).expect("snapshot all json");
    assert_eq!(snaps.len(), 2);
    assert_eq!(snaps[0]["project_name"], "alpha");
    assert_eq!(snaps[1]["project_name"], "beta");
    assert_eq!(snaps[0]["label"], "pre-break");
    assert_eq!(snaps[1]["label"], "pre-break");

    let listed = assert_vos_ok(&db, &["snapshots", "--json"]);
    let listed_arr: Vec<serde_json::Value> = serde_json::from_str(&listed).expect("snapshots json");
    assert_eq!(listed_arr.len(), 2);

    assert!(!run_vos(&db, &["snapshot"]).0);
    assert!(!run_vos(&db, &["snapshot", "--all", "alpha"]).0);
}
