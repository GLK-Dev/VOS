//! Command-line interface for VOS (Project Memory System).

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Local};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use vos_core::util::now_unix;
use vos_indexer::{IndexOptions, index_path_with_options};
use vos_store::{Store, load_benchmark_cases};

#[derive(Debug, Parser)]
#[command(
    name = "vos",
    version,
    about = "VOS — Project Memory System (local index, FTS5 search, context recovery)"
)]
struct Cli {
    /// Path to the VOS SQLite database file (defaults to ~/.vos/vos.db).
    #[arg(long, env = "VOS_DB", global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Manage projects.
    Project {
        #[command(subcommand)]
        action: ProjectCommands,
    },
    /// Idempotently index a file or directory.
    Index {
        /// File or directory path to index.
        path: PathBuf,
        /// Optional project name to associate indexed objects with.
        #[arg(long)]
        project: Option<String>,
        /// Include files ignored by .gitignore / .vosignore.
        #[arg(long)]
        no_ignore: bool,
    },
    /// Full-text search across indexed objects and chunks.
    Search {
        /// Search query string.
        query: String,
        /// Restrict results to a specific project.
        #[arg(long)]
        project: Option<String>,
        /// Maximum number of results to display.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Show scoring signals explaining why each hit ranked where it did.
        #[arg(long)]
        explain: bool,
    },
    /// Show detailed metadata and tags for an object.
    Show {
        /// Object ID, path, or unique title.
        target: String,
    },
    /// Attach manual tags to an object.
    Tag {
        /// Object ID, path, or unique title.
        target: String,
        /// One or more tags to attach.
        #[arg(required = true)]
        tags: Vec<String>,
    },
    /// Recover current project context (prioritized files, breakdown, recent timeline).
    Context {
        /// Project name.
        project: String,
        /// Maximum number of recent objects and events to show.
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Include lockfiles and VCS/editor housekeeping files in recent activity.
        #[arg(long)]
        include_noise: bool,
    },
    /// Show chronological project events grouped by day and event type.
    Timeline {
        /// Project name.
        project: String,
        /// Maximum number of events to show.
        #[arg(long, default_value_t = 30)]
        limit: usize,
        /// Show flat timestamped lines instead of grouping by day.
        #[arg(long)]
        flat: bool,
    },
    /// Show project activity over the last N days grouped by event type.
    Activity {
        /// Project name.
        project: String,
        /// Trailing window in days.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Include lockfiles and VCS/editor housekeeping files.
        #[arg(long)]
        include_noise: bool,
    },
    /// Unified project kickoff view (resume + recent activity + open questions).
    Start {
        /// Project name.
        project: String,
    },    /// Summarize where work stopped and suggest starting points to resume a project.
    Resume {
        /// Project name.
        project: String,
    },
    /// Find related files using FTS5 overlap, shared tags, directory proximity, and session timing.
    Related {
        /// Object ID, path, unique title, or topic keyword.
        target: String,
        /// Restrict related results to a specific project.
        #[arg(long)]
        project: Option<String>,
        /// Maximum number of related objects to display.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Explain why an object matters within its project context.
    Why {
        /// Object ID, path, or title.
        target: String,
        /// Optional project name to disambiguate shared file names.
        #[arg(long)]
        project: Option<String>,
    },
    /// Run quality benchmarks against the current database.
    Benchmark {
        #[command(subcommand)]
        action: BenchmarkCommands,
    },
}

#[derive(Debug, Subcommand)]
enum BenchmarkCommands {
    /// Evaluate Top-1/Top-3/Top-5 search accuracy from a YAML suite.
    Search {
        /// Path to YAML benchmark file.
        #[arg(long, default_value = "tests/search_live.yaml")]
        config: PathBuf,
        /// Persist this run into `benchmark_runs` for regression tracking.
        #[arg(long)]
        save: bool,
    },
    /// Show saved benchmark runs over time.
    History {
        /// Maximum number of recent runs to display.
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommands {
    /// Create a new project bound to a root directory.
    Create {
        /// Unique project name.
        name: String,
        /// Root directory path on disk.
        path: PathBuf,
        /// Optional short description.
        #[arg(long)]
        description: Option<String>,
    },
    /// List all projects.
    List,
}

fn main() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db_path);
    let mut store = Store::open(&db_path)?;

    match cli.command {
        Commands::Project { action } => match action {
            ProjectCommands::Create {
                name,
                path,
                description,
            } => {
                let project = store.create_project(&name, Some(&path), description.as_deref())?;
                println!("Created project '{}' ({})", project.name, project.id);
                if let Some(root) = project.root_path {
                    println!("Root: {root}");
                }
            }
            ProjectCommands::List => {
                let projects = store.list_projects()?;
                if projects.is_empty() {
                    println!("No projects yet. Create one with `vos project create <name> <path>`.");
                } else {
                    for item in projects {
                        let last = item
                            .last_indexed_at
                            .map(format_ts)
                            .unwrap_or_else(|| "never".to_string());
                        println!(
                            "{} ({}) | active: {} | deleted: {} | last_indexed: {}",
                            item.project.name,
                            item.project.id,
                            item.active_objects,
                            item.deleted_objects,
                            last
                        );
                        if let Some(root) = item.project.root_path {
                            println!("  root: {root}");
                        }
                    }
                }
            }
        },
        Commands::Index {
            path,
            project,
            no_ignore,
        } => {
            let report = index_path_with_options(
                &mut store,
                &path,
                project.as_deref(),
                IndexOptions { no_ignore },
            )?;
            println!("Scanned:   {}", report.scanned);
            println!(
                "Updated:   {} (added: {}, modified: {}, restored: {})",
                report.added + report.modified + report.restored,
                report.added,
                report.modified,
                report.restored
            );
            println!("Unchanged: {}", report.unchanged);
            println!("Deleted:   {}", report.deleted);
            if report.skipped > 0 {
                println!("Skipped:   {}", report.skipped);
            }
            println!("Elapsed:   {:.2}s", report.elapsed.as_secs_f64());
        }
        Commands::Search {
            query,
            project,
            limit,
            explain,
        } => {
            let hits = store.search(&query, project.as_deref(), limit)?;
            if hits.is_empty() {
                println!("No matches found for \"{query}\".");
            } else {
                for (idx, hit) in hits.iter().enumerate() {
                    let proj = hit.project_name.as_deref().unwrap_or("-");
                    if explain {
                        println!(
                            "{}. {} [{} | project: {} | intent: {} | score: {:.2}]",
                            idx + 1,
                            hit.title,
                            hit.kind,
                            proj,
                            hit.intent,
                            hit.score
                        );
                    } else {
                        println!(
                            "{}. {} [{} | project: {}]",
                            idx + 1,
                            hit.title,
                            hit.kind,
                            proj
                        );
                    }
                    println!("   {}", hit.path);
                    let compact_snippet = hit.snippet.replace('\n', " ");
                    if !compact_snippet.trim().is_empty() {
                        println!("   {compact_snippet}");
                    }
                    if explain && !hit.signals.is_empty() {
                        println!("   signals:");
                        for sig in &hit.signals {
                            println!("     {:+.2} {}", sig.delta, sig.label);
                        }
                    }
                }
            }
        }
        Commands::Show { target } => {
            let details = store.show_object(&target)?;
            let obj = &details.object;
            println!("Object: {} ({})", obj.title, obj.id);
            println!("Path:   {}", obj.path);
            println!(
                "Kind:   {}{}",
                obj.kind,
                if obj.is_noise { " (noise)" } else { "" }
            );
            println!(
                "Project: {}",
                details.project_name.as_deref().unwrap_or("-")
            );
            println!("Status: {}", if obj.is_deleted { "deleted" } else { "active" });
            println!("Size:   {} bytes | Chunks: {}", obj.size_bytes, details.chunk_count);
            println!("Hash:   {}", obj.content_hash);
            println!("Modified: {}", format_ts(obj.modified_at));
            println!("Indexed:  {}", format_ts(obj.indexed_at));
            if !details.tags.is_empty() {
                let tag_list: Vec<String> = details
                    .tags
                    .iter()
                    .map(|(t, src)| format!("{t} ({src})"))
                    .collect();
                println!("Tags:   {}", tag_list.join(", "));
            }
            if let Some(ref summary) = obj.summary {
                println!("\nSummary:\n{summary}");
            }
        }
        Commands::Tag { target, tags } => {
            let obj = store.add_tags(&target, &tags)?;
            println!("Tagged '{}' ({}) with: {}", obj.title, obj.id, tags.join(", "));
        }
        Commands::Context {
            project,
            limit,
            include_noise,
        } => {
            let ctx = store.project_context(&project, limit, include_noise)?;
            let root_prefix = ctx.project.root_path.as_deref().unwrap_or("");
            println!("Project: {} ({})", ctx.project.name, ctx.project.id);
            if let Some(ref desc) = ctx.project.description {
                println!("About:   {desc}");
            }
            if !root_prefix.is_empty() {
                println!("Root:    {root_prefix}");
            }
            println!(
                "Objects: {} active, {} deleted, {} noise | Updated: {}",
                ctx.active_objects,
                ctx.deleted_objects,
                ctx.noise_objects,
                format_ts(ctx.project.updated_at)
            );
            if !ctx.counts_by_kind.is_empty() {
                let breakdown: Vec<String> = ctx
                    .counts_by_kind
                    .iter()
                    .map(|(k, c)| format!("{k}: {c}"))
                    .collect();
                println!("Kinds:   {}", breakdown.join(", "));
            }

            println!("\nImportant recent files:");
            if ctx.recent_objects.is_empty() {
                println!("  (none)");
            } else {
                for obj in &ctx.recent_objects {
                    let display_path = display_relative(&obj.path, root_prefix);
                    let noise_tag = if obj.is_noise { " [noise]" } else { "" };
                    println!(
                        "  - [{}] {}{} ({})",
                        obj.kind,
                        display_path,
                        noise_tag,
                        format_ts(obj.modified_at)
                    );
                }
            }

            if !include_noise && ctx.noise_objects > 0 {
                let sample: Vec<&str> = ctx
                    .noise_sample
                    .iter()
                    .take(4)
                    .map(|o| o.title.as_str())
                    .collect();
                println!(
                    "\nHidden noise: {} file(s) ({})",
                    ctx.noise_objects,
                    sample.join(", ")
                );
            }

            println!("\nRecent activity:");
            if ctx.recent_events.is_empty() {
                println!("  (none)");
            } else {
                for ev in &ctx.recent_events {
                    let display_path = display_relative(&ev.path, root_prefix);
                    println!(
                        "  - {} | {:8} | {}",
                        format_ts(ev.created_at),
                        ev.event_type,
                        display_path
                    );
                }
            }
        }
        Commands::Timeline {
            project,
            limit,
            flat,
        } => {
            let proj = store.get_project_by_name(&project)?;
            let root_prefix = proj
                .as_ref()
                .and_then(|p| p.root_path.as_deref())
                .unwrap_or("");
            let events = store.project_timeline(&project, limit)?;

            if events.is_empty() {
                println!("No events recorded for project '{project}'.");
            } else if flat {
                for ev in events {
                    let display_path = display_relative(&ev.path, root_prefix);
                    println!(
                        "{} | {:8} | {}",
                        format_ts(ev.created_at),
                        ev.event_type,
                        display_path
                    );
                }
            } else {
                let mut grouped: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
                for ev in events {
                    let day = format_day(ev.created_at);
                    let kind = ev.event_type.as_str().to_string();
                    let rel = display_relative(&ev.path, root_prefix);
                    let entry = match ev.detail {
                        Some(ref d) => format!("{rel} [{d}]"),
                        None => rel.to_string(),
                    };
                    let list = grouped.entry(day).or_default().entry(kind).or_default();
                    if !list.contains(&entry) {
                        list.push(entry);
                    }
                }

                for (day, by_type) in grouped.into_iter().rev() {
                    println!("{day}");
                    for (kind, items) in by_type {
                        println!("\n{kind}:");
                        for item in items {
                            println!("  - {item}");
                        }
                    }
                    println!();
                }
            }
        }
        Commands::Activity {
            project,
            days,
            include_noise,
        } => {
            let activity = store.project_activity(&project, days, include_noise)?;
            let root_prefix = activity.project.root_path.as_deref().unwrap_or("");
            println!(
                "Project: {} | Last {} day(s) (since {})",
                activity.project.name,
                activity.days,
                format_day(activity.since_ts)
            );
            if activity.events.is_empty() {
                println!("No events in the last {} day(s).", activity.days);
            } else {
                let mut by_type: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for ev in &activity.events {
                    let rel = display_relative(&ev.path, root_prefix);
                    let entry = match ev.detail {
                        Some(ref d) => format!("{rel} [{d}]"),
                        None => rel.to_string(),
                    };
                    let list = by_type
                        .entry(ev.event_type.as_str().to_string())
                        .or_default();
                    if !list.contains(&entry) {
                        list.push(entry);
                    }
                }
                for (kind, items) in by_type {
                    println!("\n{kind} ({}):", items.len());
                    for item in items.iter().take(15) {
                        println!("  - {item}");
                    }
                    if items.len() > 15 {
                        println!("  ... and {} more", items.len() - 15);
                    }
                }
            }
        }
        Commands::Start { project } => {
            let start = store.project_start(&project)?;
            let res = &start.resume;
            let root_prefix = res.project.root_path.as_deref().unwrap_or("");
            println!("Project: {} ({} active objects)", res.project.name, res.active_objects);

            println!("\nLast active:");
            match res.last_activity_at {
                Some(ts) => println!("  {} ({})", format_relative(ts), format_day(ts)),
                None => println!("  never"),
            }

            println!("\nSuggested reading:");
            if res.starting_points.is_empty() {
                println!("  (none)");
            } else {
                for (idx, obj) in res.starting_points.iter().enumerate() {
                    println!("  {}. {}", idx + 1, display_relative(&obj.path, root_prefix));
                }
            }

            println!("\nRecent activity:");
            if res.recently_modified.is_empty() {
                println!("  (none)");
            } else {
                for obj in &res.recently_modified {
                    println!(
                        "  - [{}] {} ({})",
                        obj.kind,
                        display_relative(&obj.path, root_prefix),
                        format_day(obj.modified_at)
                    );
                }
            }

            println!("\nMost active areas:");
            if res.most_active_areas.is_empty() {
                println!("  (none)");
            } else {
                for area in &res.most_active_areas {
                    println!(
                        "  - {} ({} recent files, last {})",
                        area.area,
                        area.object_count,
                        format_day(area.latest_modified_at)
                    );
                }
            }

            println!("\nOpen questions:");
            if start.open_questions.is_empty() {
                println!("  (none tracked)");
            } else {
                for q in &start.open_questions {
                    println!("  - {q}");
                }
            }
        }        Commands::Resume { project } => {
            let res = store.project_resume(&project)?;
            let root_prefix = res.project.root_path.as_deref().unwrap_or("");
            println!("Project: {} ({} active objects)", res.project.name, res.active_objects);

            println!("\nLast activity:");
            match res.last_activity_at {
                Some(ts) => println!("  {} ({})", format_relative(ts), format_day(ts)),
                None => println!("  never"),
            }

            println!("\nRecently modified:");
            if res.recently_modified.is_empty() {
                println!("  (none)");
            } else {
                for obj in &res.recently_modified {
                    println!(
                        "  - [{}] {} ({})",
                        obj.kind,
                        display_relative(&obj.path, root_prefix),
                        format_day(obj.modified_at)
                    );
                }
            }

            println!("\nRecent additions:");
            if res.recent_additions.is_empty() {
                println!("  (none)");
            } else {
                for obj in &res.recent_additions {
                    println!("  - {}", display_relative(&obj.path, root_prefix));
                }
            }

            println!("\nMost active areas:");
            if res.most_active_areas.is_empty() {
                println!("  (none)");
            } else {
                for area in &res.most_active_areas {
                    println!(
                        "  - {} ({} recent files, last {})",
                        area.area,
                        area.object_count,
                        format_day(area.latest_modified_at)
                    );
                }
            }

            println!("\nSuggested starting points:");
            if res.starting_points.is_empty() {
                println!("  (none)");
            } else {
                for obj in &res.starting_points {
                    println!("  - {}", display_relative(&obj.path, root_prefix));
                }
            }
        }
        Commands::Related {
            target,
            project,
            limit,
        } => {
            let hits = store.find_related(&target, project.as_deref(), limit)?;
            if hits.is_empty() {
                println!("No related objects found for \"{target}\".");
            } else {
                for (idx, hit) in hits.iter().enumerate() {
                    let proj = hit.project_name.as_deref().unwrap_or("-");
                    println!(
                        "{}. {} [{} | {}] (score: {})",
                        idx + 1,
                        hit.title,
                        hit.kind,
                        proj,
                        hit.score
                    );
                    println!("   {}", hit.path);
                    println!("   signals: {}", hit.reasons.join(", "));
                }
            }
        }
        Commands::Why { target, project } => {
            let why = store.why_object(&target, project.as_deref())?;
            let root_prefix = why.project_root.as_deref().unwrap_or("");
            let rel_path = display_relative(&why.object.path, root_prefix);

            println!("Object:\n  {} ({})", why.object.title, rel_path);
            println!(
                "\nProject:\n  {}",
                why.project_name.as_deref().unwrap_or("(unassigned)")
            );

            println!("\nWhy it matters:");
            println!(
                "  • Modified {} ({})",
                format_relative(why.object.modified_at),
                format_day(why.object.modified_at)
            );
            if why.is_starting_point {
                println!("  • Key project starting point");
            }
            if why.is_active_area {
                println!("  • Located in active area: {}", why.area);
            } else {
                println!("  • Located in area: {}", why.area);
            }
            if !why.tags.is_empty() {
                let tag_names: Vec<&str> = why.tags.iter().map(|(t, _)| t.as_str()).collect();
                println!("  • Tags: {}", tag_names.join(", "));
            }
            if !why.related.is_empty() {
                println!("  • Related files:");
                for rel in &why.related {
                    println!(
                        "    - {} (score: {} | {})",
                        display_relative(&rel.path, root_prefix),
                        rel.score,
                        rel.reasons.join(", ")
                    );
                }
            }
            if !why.recent_events.is_empty() {
                println!("  • Recent events:");
                for ev in &why.recent_events {
                    let detail = ev
                        .detail
                        .as_deref()
                        .map(|d| format!(" [{d}]"))
                        .unwrap_or_default();
                    println!(
                        "    - {}{} ({})",
                        ev.event_type,
                        detail,
                        format_day(ev.created_at)
                    );
                }
            }
        }
        Commands::Benchmark { action } => match action {
            BenchmarkCommands::Search { config, save } => {
                let cases = load_benchmark_cases(&config)?;
                let report = store.run_search_benchmark(&cases)?;

                for case in &report.cases {
                    let t1 = case.top1_hit.map(fmt_hit).unwrap_or("-");
                    let t3 = case.top3_hit.map(fmt_hit).unwrap_or("-");
                    let t5 = case.top5_hit.map(fmt_hit).unwrap_or("-");
                    let cp = if case.cross_project_total > 0 {
                        format!(" | proj: {}/{}", case.cross_project_matched, case.cross_project_total)
                    } else {
                        String::new()
                    };
                    println!(
                        "[{:15}] {:24} | T1:{:4} T3:{:4} T5:{:4}{}",
                        case.category, case.query, t1, t3, t5, cp
                    );
                    let failed_file = case.top1_hit == Some(false)
                        || case.top3_hit == Some(false)
                        || case.top5_hit == Some(false);
                    let failed_proj = case.cross_project_matched < case.cross_project_total;
                    if failed_file || failed_proj {
                        println!("  actual: {}", case.actual_top5.join(", "));
                    }
                }

                println!("\nBenchmark Results (Queries: {}):", report.total_cases);
                if report.top1_total > 0 {
                    println!(
                        "  Top-1: {}/{} ({:.1}%)",
                        report.top1_passed,
                        report.top1_total,
                        pct(report.top1_passed, report.top1_total)
                    );
                }
                if report.top3_total > 0 {
                    println!(
                        "  Top-3: {}/{} ({:.1}%)",
                        report.top3_passed,
                        report.top3_total,
                        pct(report.top3_passed, report.top3_total)
                    );
                }
                if report.top5_total > 0 {
                    println!(
                        "  Top-5: {}/{} ({:.1}%)",
                        report.top5_passed,
                        report.top5_total,
                        pct(report.top5_passed, report.top5_total)
                    );
                }
                println!("  MRR:   {:.3}", report.mrr);
                println!(
                    "  Noise Hits: {} / {} ({:.1}%)",
                    report.noise_hits,
                    report.total_returned_hits,
                    pct(report.noise_hits, report.total_returned_hits)
                );
                if report.cross_project_total > 0 {
                    println!(
                        "  Cross-Project Recall: {}/{} ({:.1}%)",
                        report.cross_project_matched,
                        report.cross_project_total,
                        pct(report.cross_project_matched, report.cross_project_total)
                    );
                }

                if report.categories.len() > 1 {
                    println!("\nBy Category:");
                    for cat in &report.categories {
                        let t3_str = if cat.top3_total > 0 {
                            format!("{}/{}", cat.top3_passed, cat.top3_total)
                        } else {
                            "-".to_string()
                        };
                        let t5_str = if cat.top5_total > 0 {
                            format!("{}/{}", cat.top5_passed, cat.top5_total)
                        } else {
                            "-".to_string()
                        };
                        println!(
                            "  {:18} | cases: {:2} | Top-3: {:5} | Top-5: {:5}",
                            cat.category, cat.cases, t3_str, t5_str
                        );
                    }
                }

                if save {
                    let suite_name = config
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| config.display().to_string());
                    let run_id = store.save_benchmark_run(&suite_name, &report)?;
                    println!("\nSaved benchmark run #{run_id} ({suite_name}).");
                }
            }
            BenchmarkCommands::History { limit } => {
                let runs = store.list_benchmark_runs(limit)?;
                if runs.is_empty() {
                    println!("No benchmark runs saved yet. Run `vos benchmark search --save`.");
                } else {
                    for (idx, run) in runs.iter().enumerate() {
                        let prev = runs[idx + 1..]
                            .iter()
                            .find(|r| r.suite_name == run.suite_name);

                        let t1 = if run.top1_total > 0 {
                            format!("{:.1}%", pct(run.top1_passed, run.top1_total))
                        } else {
                            "-".to_string()
                        };
                        let t3 = if run.top3_total > 0 {
                            format!("{:.1}%", pct(run.top3_passed, run.top3_total))
                        } else {
                            "-".to_string()
                        };
                        let t5_val = pct(run.top5_passed, run.top5_total);
                        let t5_delta = prev
                            .filter(|p| p.top5_total > 0 && run.top5_total > 0)
                            .map(|p| {
                                let d = t5_val - pct(p.top5_passed, p.top5_total);
                                format!(" ({d:+.1}%)")
                            })
                            .unwrap_or_default();
                        let t5 = if run.top5_total > 0 {
                            format!("{t5_val:.1}%{t5_delta}")
                        } else {
                            "-".to_string()
                        };
                        let mrr_delta = prev
                            .map(|p| format!(" ({:+.3})", run.mrr - p.mrr))
                            .unwrap_or_default();
                        let noise = pct(run.noise_hits, run.total_returned_hits);
                        let cp = if run.cross_project_total > 0 {
                            format!("{:.1}%", pct(run.cross_project_matched, run.cross_project_total))
                        } else {
                            "-".to_string()
                        };
                        println!(
                            "#{} {} | {} ({} q) | T1: {} | T3: {} | T5: {} | MRR: {:.3}{} | Noise: {:.1}% | Cross: {}",
                            run.id,
                            format_ts(run.created_at),
                            run.suite_name,
                            run.total_cases,
                            t1,
                            t3,
                            t5,
                            run.mrr,
                            mrr_delta,
                            noise,
                            cp
                        );
                    }
                }
            }
        },
    }

    Ok(())
}

fn fmt_hit(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

fn pct(passed: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (passed as f64 / total as f64) * 100.0
    }
}

fn default_db_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    match home {
        Some(dir) => PathBuf::from(dir).join(".vos").join("vos.db"),
        None => PathBuf::from(".vos").join("vos.db"),
    }
}

fn format_ts(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn format_day(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn format_relative(ts: i64) -> String {
    let diff = now_unix().saturating_sub(ts);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{} minute(s) ago", diff / 60)
    } else if diff < 86_400 {
        format!("{} hour(s) ago", diff / 3600)
    } else {
        format!("{} day(s) ago", diff / 86_400)
    }
}

fn display_relative<'a>(path: &'a str, root: &str) -> &'a str {
    if root.is_empty() {
        return path;
    }
    let trimmed = root.trim_end_matches(['/', '\\']);
    path.strip_prefix(trimmed)
        .map(|rest| rest.trim_start_matches(['/', '\\']))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(path)
}




