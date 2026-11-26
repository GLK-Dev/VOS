//! Command-line interface for VOS (Project Memory System).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, TimeZone};
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;
use vos_core::util::now_unix;
use vos_core::{GraphDirection, GraphTreeNode, ObjectKind};
use vos_indexer::{IndexOptions, index_path_with_options};
use vos_store::{SearchFilters, Store, load_benchmark_cases};

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

    /// Enable experimental insight features (or set VOS_EXPERIMENTAL=1).
    #[arg(long, env = "VOS_EXPERIMENTAL", global = true, value_parser = clap::builder::BoolishValueParser::new())]
    experimental: bool,

    /// Emit machine-readable JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,

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
    /// Idempotently index a file or directory (or all registered projects with --all).
    Index {
        /// File or directory path to index (required unless --all is specified).
        path: Option<PathBuf>,
        /// Optional project name to associate indexed objects with.
        #[arg(long)]
        project: Option<String>,
        /// Refresh all registered projects using their stored root paths.
        #[arg(long)]
        all: bool,
        /// Include files ignored by .gitignore / .vosignore.
        #[arg(long)]
        no_ignore: bool,
    },
    /// Full-text search across indexed objects and chunks (optionally filtered by --tag, --kind, or --since).
    Search {
        /// Search query string (optional when --tag, --kind, or --since is specified).
        query: Option<String>,
        /// Restrict results to a specific project.
        #[arg(long)]
        project: Option<String>,
        /// Filter results by attached tag (works alone or combined with query).
        #[arg(long)]
        tag: Option<String>,
        /// Filter results by object kind (`code`, `note`, `doc`, `config`, `model`, `media`, `other`).
        #[arg(long)]
        kind: Option<CliObjectKind>,
        /// Only return objects modified since `YYYY-MM-DD`, `Nh`, `Nd`, `Nw`, `today`, or `yesterday`.
        #[arg(long)]
        since: Option<String>,
        /// Maximum number of results to display.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Show scoring signals explaining why each hit ranked where it did.
        #[arg(long)]
        explain: bool,
        /// Do not penalize internal search/benchmark configs (e.g. config/*.yaml, tests/*.yaml).
        #[arg(long)]
        include_internal: bool,
    },
    /// Show detailed metadata and tags for an object.
    Show {
        /// Object ID, path, or unique title.
        target: String,
        /// Optional project name to disambiguate shared filenames.
        #[arg(long)]
        project: Option<String>,
    },
    /// Attach manual tags to an object.
    Tag {
        /// Object ID, path, or unique title.
        target: String,
        /// Optional project name to disambiguate shared filenames.
        #[arg(long)]
        project: Option<String>,
        /// One or more tags to attach.
        #[arg(required = true)]
        tags: Vec<String>,
    },
    /// Remove manual tags from an object.
    Untag {
        /// Object ID, path, or unique title.
        target: String,
        /// Optional project name to disambiguate shared filenames.
        #[arg(long)]
        project: Option<String>,
        /// One or more tags to remove.
        #[arg(required = true)]
        tags: Vec<String>,
    },
    /// List tags across indexed objects with object counts.
    Tags {
        /// Optional project filter.
        #[arg(long)]
        project: Option<String>,
        /// Maximum number of tags to show.
        #[arg(long, default_value_t = 30)]
        limit: usize,
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
    /// Summarize how a project evolved since a given date or relative window (default: 30d).
    Evolution {
        /// Project name.
        project: String,
        /// Cutoff date (`YYYY-MM-DD`) or relative time (`12h`, `30d`, `2w`, `today`, `yesterday`).
        #[arg(long, default_value = "30d")]
        since: String,
    },
    /// Detect documentation drift and roadmap-vs-implementation drift.
    Drift {
        /// Project name (required unless --all is specified).
        project: Option<String>,
        /// Audit drift across all registered projects.
        #[arg(long)]
        all: bool,
    },
    /// Decision-support health, pending work, and risk summary for a project (or all projects with --all).
    Status {
        /// Project name (required unless --all is specified).
        project: Option<String>,
        /// Summarize health, risks, and coverage across all registered projects.
        #[arg(long)]
        all: bool,
    },
    /// Explain a concept deterministically using project docs, code, and `config/concepts.yaml`.
    Explain {
        /// Concept or topic to explain.
        concept: String,
        /// Optional project scope.
        #[arg(long)]
        project: Option<String>,
    },
    /// Show objects modified, added, or deleted since YYYY-MM-DD (or Nd, e.g. 7d).
    ChangedSince {
        /// Cutoff date (`YYYY-MM-DD`) or relative time (`12h`, `7d`, `2w`, `today`, `yesterday`).
        since: String,
        /// Optional project name (positional).
        project: Option<String>,
    },
    /// Unified project kickoff view (resume + recent activity + open questions).
    Start {
        /// Project name.
        project: String,
    },
    /// Actionable continuation briefing combining resume, activity, and why.
    Continue {
        /// Project name.
        project: String,
    },
    /// Summarize where work stopped and suggest starting points to resume a project.
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
    /// Show non-git activity ownership hotspots and stable areas for a project.
    Ownership {
        /// Project name.
        project: String,
        /// Recency window in days for `Recently Active`.
        #[arg(long, default_value_t = 14)]
        days: u32,
        /// Maximum number of areas to show per section.
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Analyze documentation coverage ratio and per-area coverage using relations.
    Coverage {
        /// Project name.
        project: String,
    },
    /// Display deterministic file dependency graph for a project or file.
    Graph {
        /// Project name.
        project: String,
        /// Optional target file (ID, path, or unique title) to root the graph on.
        #[arg(long)]
        target: Option<String>,
        /// Traversal direction (`out`, `in`, or `both`).
        #[arg(long, value_enum, default_value_t = CliGraphDirection::Out)]
        direction: CliGraphDirection,
        /// Maximum traversal depth (1..=5).
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// Maximum number of top hub nodes when `--target` is omitted.
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Capture a point-in-time snapshot of project status, coverage, and risks.
    Snapshot {
        /// Project name (required unless --all is specified).
        project: Option<String>,
        /// Capture snapshots for all registered projects.
        #[arg(long)]
        all: bool,
        /// Optional human-readable snapshot label.
        #[arg(long)]
        label: Option<String>,
    },
    /// List saved project snapshots.
    Snapshots {
        /// Optional project name filter.
        project: Option<String>,
        /// Maximum number of snapshots to display.
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Compare two saved snapshots, or compare a saved snapshot against live state with --live.
    CompareSnapshot {
        /// Baseline snapshot numeric ID.
        from_id: Option<i64>,
        /// Target snapshot numeric ID (omit when passing --live).
        to_id: Option<i64>,
        /// Compare baseline (or latest snapshot of --project) against live project state.
        #[arg(long)]
        live: bool,
        /// Project name for --live comparison (defaults to baseline snapshot project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Track recovery & work sessions (`start`, `stop`, `list`, `stats`).
    Session {
        #[command(subcommand)]
        action: SessionCommands,
    },
    /// Verify SQLite integrity, FTS5 index parity, relation hygiene, and project root paths.
    Doctor {
        /// Rebuild FTS5 indexes and prune stale relations pointing to deleted objects.
        #[arg(long)]
        repair: bool,
    },
    /// Run quality benchmarks against the current database.
    Benchmark {
        #[command(subcommand)]
        action: BenchmarkCommands,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliObjectKind {
    Code,
    Note,
    Doc,
    Config,
    Model,
    Media,
    Other,
}

impl From<CliObjectKind> for ObjectKind {
    fn from(value: CliObjectKind) -> Self {
        match value {
            CliObjectKind::Code => Self::Code,
            CliObjectKind::Note => Self::Note,
            CliObjectKind::Doc => Self::Doc,
            CliObjectKind::Config => Self::Config,
            CliObjectKind::Model => Self::Model,
            CliObjectKind::Media => Self::Media,
            CliObjectKind::Other => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliGraphDirection {
    Out,
    In,
    Both,
}

impl From<CliGraphDirection> for GraphDirection {
    fn from(value: CliGraphDirection) -> Self {
        match value {
            CliGraphDirection::Out => Self::Outgoing,
            CliGraphDirection::In => Self::Incoming,
            CliGraphDirection::Both => Self::Both,
        }
    }
}

/// Options for appending a Recovery League Log row when stopping a session.
#[derive(Debug, clap::Args)]
struct SessionLogArgs {
    /// Append a Recovery League Log row to this markdown file (requires --quality).
    #[arg(long = "log", requires = "quality")]
    log: Option<PathBuf>,
    /// First command used during recovery (for --log).
    #[arg(long, default_value = "-")]
    first_command: String,
    /// Days the project was paused (for --log); computed automatically when omitted.
    #[arg(long)]
    days_away: Option<String>,
    /// Helpful signals, comma-separated (for --log).
    #[arg(long, default_value = "-")]
    helpful: String,
    /// Ignored or noisy signals, comma-separated (for --log).
    #[arg(long, default_value = "-")]
    ignored: String,
}

#[derive(Debug, Subcommand)]
enum SessionCommands {
    /// Start tracking a recovery/work session for a project (refreshes index by default).
    Start {
        /// Project name.
        project: String,
        /// Optional session kickoff note.
        #[arg(long)]
        note: Option<String>,
        /// Skip automatic re-indexing of the project root before starting the session.
        #[arg(long)]
        no_index: bool,
    },
    /// Stop the currently open work session and summarize modified areas (refreshes index by default).
    Stop {
        /// Optional project name to close a specific project's open session.
        project: Option<String>,
        /// Optional self-reported Recovery Quality score (1..=5).
        #[arg(long)]
        quality: Option<u8>,
        /// Optional closing summary note.
        #[arg(long)]
        note: Option<String>,
        /// Skip automatic re-indexing before closing the session.
        #[arg(long)]
        no_index: bool,
        #[command(flatten)]
        log_opts: Box<SessionLogArgs>,
    },
    /// List recorded work sessions.
    List {
        /// Optional project filter.
        project: Option<String>,
        /// Maximum number of sessions to show.
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Aggregate session durations, return points, productive areas, and Predicted-vs-Actual stats.
    Stats {
        /// Optional project filter.
        project: Option<String>,
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
        /// Fail with non-zero exit if Top-5 accuracy (%) falls below this threshold.
        #[arg(long)]
        min_top5: Option<f64>,
        /// Fail with non-zero exit if MRR falls below this threshold.
        #[arg(long)]
        min_mrr: Option<f64>,
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
    /// Update a project's name, root directory, or description while preserving history.
    Update {
        /// Current registered project name.
        name: String,
        /// New project name.
        #[arg(long)]
        rename: Option<String>,
        /// New root directory path on disk.
        #[arg(long)]
        path: Option<PathBuf>,
        /// New description (pass empty string to clear).
        #[arg(long)]
        description: Option<String>,
    },
    /// Remove a project and all its indexed objects, snapshots, and sessions from the database.
    Remove {
        /// Registered project name.
        name: String,
    },
}

fn main() -> Result<()> {
    // The derived clap command tree is large; debug builds exceed the default 1 MiB
    // Windows main-thread stack, so run the CLI on a thread with an explicit stack size.
    std::thread::Builder::new()
        .name("vos-main".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(run)
        .context("failed to spawn the CLI thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("CLI thread panicked"))?
}

fn run() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db_path);
    let mut store = Store::open(&db_path)?;

    let experimental = cli.experimental;
    let json = cli.json;
    match cli.command {
        Commands::Project { action } => handle_project(&mut store, action, json)?,
        Commands::Index {
            path,
            project,
            all,
            no_ignore,
        } => {
            if all {
                let batch = index_all_projects(&mut store, no_ignore)?;
                if json {
                    return print_json(&batch);
                }
                print_all_index_report(&batch);
            } else {
                let target_path =
                    path.ok_or_else(|| anyhow::anyhow!("provide a path to index or pass --all"))?;
                let report = index_path_with_options(
                    &mut store,
                    &target_path,
                    project.as_deref(),
                    IndexOptions { no_ignore },
                )?;
                if json {
                    return print_json(&JsonIndexReport::from(&report));
                }
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
                println!("Relations: {}", report.relations);
                if report.skipped > 0 {
                    println!("Skipped:   {}", report.skipped);
                }
                println!("Elapsed:   {:.2}s", report.elapsed.as_secs_f64());
            }
        }
        Commands::Search {
            query,
            project,
            tag,
            kind,
            since,
            limit,
            explain,
            include_internal,
        } => handle_search(
            &store,
            query,
            project,
            tag,
            kind,
            since,
            limit,
            explain,
            include_internal,
            json,
        )?,
        Commands::Show { target, project } => {
            let details = store.show_object_scoped(&target, project.as_deref())?;
            if json {
                return print_json(&details);
            }
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
            println!(
                "Status: {}",
                if obj.is_deleted { "deleted" } else { "active" }
            );
            println!(
                "Size:   {} bytes | Chunks: {}",
                obj.size_bytes, details.chunk_count
            );
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
        Commands::Tag {
            target,
            project,
            tags,
        } => {
            let obj = store.add_tags_scoped(&target, project.as_deref(), &tags)?;
            if json {
                return print_json(&obj);
            }
            println!(
                "Tagged '{}' ({}) with: {}",
                obj.title,
                obj.id,
                tags.join(", ")
            );
        }
        Commands::Untag {
            target,
            project,
            tags,
        } => {
            let obj = store.remove_tags_scoped(&target, project.as_deref(), &tags)?;
            if json {
                return print_json(&obj);
            }
            println!(
                "Untagged '{}' ({}) of: {}",
                obj.title,
                obj.id,
                tags.join(", ")
            );
        }
        Commands::Tags { project, limit } => {
            let items = store.list_tags(project.as_deref(), limit)?;
            if json {
                return print_json(&items);
            }
            if items.is_empty() {
                println!("No tags recorded.");
            } else {
                for item in items {
                    println!("  {:<20} ({})", item.tag, item.object_count);
                }
            }
        }
        Commands::Context {
            project,
            limit,
            include_noise,
        } => {
            let ctx = store.project_context(&project, limit, include_noise)?;
            if json {
                return print_json(&ctx);
            }
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
            if json {
                return print_json(&events);
            }

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
            if json {
                return print_json(&activity);
            }
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
        Commands::Evolution { project, since } => {
            let since_ts = parse_since_arg(&since)?;
            let evo = store.project_evolution(&project, since_ts, &since)?;
            if json {
                return print_json(&evo);
            }
            println!(
                "Evolution: {} (since {} / {})",
                evo.project.name,
                evo.since_label,
                format_day(evo.since_ts)
            );
            println!(
                "\nChanges:\n  Added:    {}\n  Modified: {}\n  Deleted:  {}",
                evo.added_count, evo.modified_count, evo.deleted_count
            );

            println!("\nActive areas since {}:", evo.since_label);
            if evo.new_active_areas.is_empty() {
                println!("  (none)");
            } else {
                for a in &evo.new_active_areas {
                    println!("  - {a}");
                }
            }

            println!("\nInactive areas:");
            if evo.inactive_areas.is_empty() {
                println!("  (none)");
            } else {
                for a in &evo.inactive_areas {
                    println!("  - {a}");
                }
            }

            println!("\nEmerging concepts:");
            if evo.emerging_concepts.is_empty() {
                println!("  (none)");
            } else {
                println!("  {}", evo.emerging_concepts.join(", "));
            }

            println!(
                "\nDocumentation coverage:\n  {}",
                evo.documentation_coverage
            );
            println!(
                "\nSignals:\n  Open questions: {}\n  Pending work:   {}",
                evo.open_questions_count, evo.pending_work_count
            );
        }
        Commands::Drift { project, all } => handle_drift(&store, project, all, json)?,
        Commands::Status { project, all } => {
            if all {
                let rows = collect_all_statuses(&store, experimental)?;
                if json {
                    let payload: Vec<JsonStatusWithExperimental<'_>> = rows
                        .iter()
                        .map(|(st, pred)| JsonStatusWithExperimental {
                            status: st,
                            recovery_prediction: pred.clone(),
                        })
                        .collect();
                    return print_json(&payload);
                }
                print_all_statuses(&rows);
                return Ok(());
            }
            let project =
                project.ok_or_else(|| anyhow::anyhow!("provide a project name or pass --all"))?;
            let st = store.project_status(&project)?;
            let prediction = if experimental {
                Some(store.predict_recovery_difficulty(&st))
            } else {
                None
            };
            if json {
                return print_json(&JsonStatusWithExperimental {
                    status: &st,
                    recovery_prediction: prediction,
                });
            }
            let root_prefix = st.project.root_path.as_deref().unwrap_or("");
            println!(
                "Project: {} ({} active, {} deleted)",
                st.project.name, st.active_objects, st.deleted_objects
            );
            println!("\nHealth: {}", st.health);
            println!("Critical Risks: {}", st.critical_risk_count);
            println!("High Risks: {}", st.high_risk_count);
            println!("Coverage: {:.1}%", st.coverage_ratio);
            println!("Documentation Health: {}", st.documentation_health);

            println!("\nLast change:");
            match st.last_change_at {
                Some(ts) => println!("  {} ({})", format_relative(ts), format_day(ts)),
                None => println!("  never"),
            }

            println!("\nKey documents:");
            if st.key_documents.is_empty() {
                println!("  (none)");
            } else {
                for doc in &st.key_documents {
                    println!("  - {}", display_relative(&doc.path, root_prefix));
                }
            }

            println!("\nOpen questions:");
            if st.open_questions.is_empty() {
                println!("  (none tracked)");
            } else {
                for q in &st.open_questions {
                    println!("  - {q}");
                }
            }

            println!("\nPending work:");
            if st.pending_work.is_empty() {
                println!("  (none tracked)");
            } else {
                for item in &st.pending_work {
                    println!("  - {item}");
                }
            }

            println!("\nActive areas:");
            if st.active_areas.is_empty() {
                println!("  (none)");
            } else {
                for area in &st.active_areas {
                    println!("  - {} ({} recent files)", area.area, area.object_count);
                }
            }

            if !st.critical_areas.is_empty() {
                println!("\nCritical Areas:");
                for (idx, crit) in st.critical_areas.iter().enumerate() {
                    println!("  {}. {}", idx + 1, crit.area);
                    println!(
                        "     Relations: {} | Coverage: Low | TODOs: {}",
                        crit.relation_count, crit.open_todos
                    );
                    println!("     Reason: {}", crit.reason);
                }
            }

            if !st.dependency_hubs.is_empty() {
                println!("\nDependency hubs:");
                for hub in &st.dependency_hubs {
                    println!("  - {hub}");
                }
            }

            println!("\nRisk areas:");
            if st.risk_areas.is_empty() {
                println!("  (none detected)");
            } else {
                for risk in &st.risk_areas {
                    println!("  - {risk}");
                }
            }

            if let Some(ref pred) = prediction {
                print_recovery_prediction(pred);
            }
        }
        Commands::Explain { concept, project } => {
            let exp = store.explain_concept(&concept, project.as_deref())?;
            if json {
                return print_json(&exp);
            }
            println!("Concept: {}", exp.concept);

            println!("\nFiles contributing:");
            if exp.files_contributing.is_empty() {
                println!("  (none)");
            } else {
                for hit in &exp.files_contributing {
                    let proj = hit.project_name.as_deref().unwrap_or("-");
                    println!("  - {} [{}] ({})", hit.title, proj, hit.path);
                }
            }

            println!("\nDefinition (extracted):");
            match exp.definition {
                Some(ref def) => println!("  {def}"),
                None => println!("  (no documentation excerpt found)"),
            }

            println!("\nRelated concepts:");
            if exp.related_concepts.is_empty() {
                println!("  (none)");
            } else {
                for rel in &exp.related_concepts {
                    println!("  - {rel}");
                }
            }

            println!("\nRelevant code:");
            if exp.relevant_code.is_empty() {
                println!("  (none)");
            } else {
                for hit in &exp.relevant_code {
                    let proj = hit.project_name.as_deref().unwrap_or("-");
                    println!("  - {} [{}] ({})", hit.title, proj, hit.path);
                }
            }
        }
        Commands::ChangedSince { since, project } => {
            let since_ts = parse_since_arg(&since)?;
            let report = store.changed_since(since_ts, &since, project.as_deref())?;
            if json {
                return print_json(&report);
            }
            let root_prefix = report.project_root.as_deref().unwrap_or("");
            let scope = report.project_name.as_deref().unwrap_or("all projects");

            println!(
                "Changes in {} since {} ({}):",
                scope,
                report.since_label,
                format_day(report.since_ts)
            );

            println!("\nModified ({}):", report.modified.len());
            if report.modified.is_empty() {
                println!("  (none)");
            } else {
                for obj in report.modified.iter().take(25) {
                    println!(
                        "  - {} ({})",
                        display_relative(&obj.path, root_prefix),
                        format_day(obj.modified_at)
                    );
                }
                if report.modified.len() > 25 {
                    println!("  ... and {} more", report.modified.len() - 25);
                }
            }

            println!("\nAdded ({}):", report.added.len());
            if report.added.is_empty() {
                println!("  (none)");
            } else {
                for obj in report.added.iter().take(25) {
                    println!("  - {}", display_relative(&obj.path, root_prefix));
                }
                if report.added.len() > 25 {
                    println!("  ... and {} more", report.added.len() - 25);
                }
            }

            println!("\nDeleted ({}):", report.deleted.len());
            if report.deleted.is_empty() {
                println!("  (none)");
            } else {
                for obj in report.deleted.iter().take(25) {
                    println!("  - {}", display_relative(&obj.path, root_prefix));
                }
                if report.deleted.len() > 25 {
                    println!("  ... and {} more", report.deleted.len() - 25);
                }
            }
        }
        Commands::Continue { project } => {
            let cont = store.project_continue(&project)?;
            let prediction = if experimental {
                let st = store.project_status(&project)?;
                Some(store.predict_recovery_difficulty(&st))
            } else {
                None
            };
            if json {
                return print_json(&JsonContinueWithExperimental {
                    cont: &cont,
                    recovery_prediction: prediction,
                });
            }
            let res = &cont.resume;
            let root_prefix = res.project.root_path.as_deref().unwrap_or("");
            println!(
                "Project: {} ({} active objects)",
                res.project.name, res.active_objects
            );

            println!("\nLast activity:");
            match res.last_activity_at {
                Some(ts) => println!("  {} ({})", format_relative(ts), format_day(ts)),
                None => println!("  never"),
            }

            if let Some(ref sess) = cont.last_session {
                print_last_session(sess);
            }

            println!("\nContinue from:");
            if cont.continue_from.is_empty() {
                println!("  (none)");
            } else {
                for (idx, item) in cont.continue_from.iter().enumerate() {
                    println!(
                        "  {}. {} — {}",
                        idx + 1,
                        display_relative(&item.object.path, root_prefix),
                        item.reason
                    );
                }
            }

            println!("\nRecent changes:");
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

            if !cont.ownership_areas.is_empty() {
                println!("\nOwnership hotspots (events):");
                for area in cont.ownership_areas.iter().take(4) {
                    println!(
                        "  - {} ({} files, {} events)",
                        area.area, area.file_count, area.event_count
                    );
                }
            }

            if !cont.coverage_warnings.is_empty() {
                println!("\nCoverage warnings (underdocumented):");
                for cov in cont.coverage_warnings.iter().take(4) {
                    println!(
                        "  - {} ({} code files, 0 docs/refs)",
                        cov.area, cov.code_files
                    );
                }
            }

            if !cont.dependency_hubs.is_empty() {
                println!("\nDependency hubs:");
                for hub in cont.dependency_hubs.iter().take(4) {
                    let rel = display_relative(&hub.object.path, root_prefix);
                    println!(
                        "  - {} ({} relations: {} out, {} in)",
                        rel,
                        hub.outgoing_count + hub.incoming_count,
                        hub.outgoing_count,
                        hub.incoming_count
                    );
                }
            }

            println!("\nOpen questions:");
            if cont.open_questions.is_empty() {
                println!("  (none tracked)");
            } else {
                for q in &cont.open_questions {
                    println!("  - {q}");
                }
            }

            if let Some(ref pred) = prediction {
                print_recovery_prediction(pred);
            }
        }
        Commands::Start { project } => handle_start(&store, &project, experimental, json)?,
        Commands::Resume { project } => {
            let res = store.project_resume(&project)?;
            if json {
                return print_json(&res);
            }
            let root_prefix = res.project.root_path.as_deref().unwrap_or("");
            println!(
                "Project: {} ({} active objects)",
                res.project.name, res.active_objects
            );

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
            if json {
                return print_json(&hits);
            }
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
            let impact = if experimental {
                Some(store.experimental_why_impact(&why.object.id)?)
            } else {
                None
            };
            if json {
                return print_json(&JsonWhyWithExperimental {
                    why: &why,
                    experimental_impact: impact,
                });
            }
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
            if !why.outgoing_relations.is_empty() {
                println!("  • Dependencies:");
                for edge in why.outgoing_relations.iter().take(6) {
                    println!(
                        "    - {} [{}]",
                        display_relative(&edge.to_path, root_prefix),
                        edge.relation_type
                    );
                }
                if why.outgoing_relations.len() > 6 {
                    println!("    ... and {} more", why.outgoing_relations.len() - 6);
                }
            }
            if !why.incoming_relations.is_empty() {
                println!("  • Referenced by:");
                for edge in why.incoming_relations.iter().take(6) {
                    println!(
                        "    - {} [{}]",
                        display_relative(&edge.from_path, root_prefix),
                        edge.relation_type
                    );
                }
                if why.incoming_relations.len() > 6 {
                    println!("    ... and {} more", why.incoming_relations.len() - 6);
                }
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
            if let Some(imp) = impact {
                println!("\nExperimental Blast Radius:");
                if let (Some(score), Some(role)) = (imp.hub_score, imp.role.as_deref()) {
                    println!("  Hub Score: {:.2} ({role})", score);
                }
                println!(
                    "  Dependents: {} direct / {} transitive",
                    imp.direct_dependents, imp.transitive_dependents
                );
                if !imp.sample_dependents.is_empty() {
                    println!("  Affected objects:");
                    for dep in &imp.sample_dependents {
                        println!("    - {dep}");
                    }
                }
            }
        }
        Commands::Graph {
            project,
            target,
            direction,
            depth,
            limit,
        } => {
            let graph =
                store.project_graph(&project, target.as_deref(), direction.into(), depth, limit)?;
            let hub_importance = if experimental {
                Some(store.experimental_hub_importance(&project, limit)?)
            } else {
                None
            };
            if json {
                return print_json(&JsonGraphWithExperimental {
                    graph: &graph,
                    hub_importance,
                });
            }
            let root_prefix = graph.project.root_path.as_deref().unwrap_or("");

            if let Some(ref target_obj) = graph.target {
                let target_rel = display_relative(&target_obj.path, root_prefix);
                let hub = &graph.hubs[0];
                println!(
                    "Graph: {} | {} (out: {}, in: {}, depth: {})",
                    graph.project.name,
                    target_rel,
                    hub.outgoing_count,
                    hub.incoming_count,
                    graph.depth
                );
            } else {
                println!(
                    "Graph: {} ({} relations across {} files, direction: {})",
                    graph.project.name,
                    graph.total_relations,
                    graph.connected_objects,
                    graph.direction
                );
            }

            if graph.hubs.is_empty() {
                println!("\n(no deterministic relations indexed; run `vos index` on the project)");
            } else {
                for hub in &graph.hubs {
                    let hub_rel = display_relative(&hub.object.path, root_prefix);
                    println!(
                        "\n{} (out: {}, in: {})",
                        hub_rel, hub.outgoing_count, hub.incoming_count
                    );
                    if !hub.outgoing.is_empty() {
                        if matches!(graph.direction, GraphDirection::Both) {
                            println!("  [outgoing]");
                        }
                        render_graph_tree(&hub.outgoing, root_prefix, "", false);
                    }
                    if !hub.incoming.is_empty() {
                        if matches!(graph.direction, GraphDirection::Both) {
                            println!("  [incoming]");
                        }
                        render_graph_tree(&hub.incoming, root_prefix, "", true);
                    }
                    if hub.outgoing.is_empty() && hub.incoming.is_empty() {
                        println!("  (no edges in selected direction)");
                    }
                }
            }

            if let Some(hubs) = hub_importance
                && !hubs.is_empty()
            {
                println!("\nExperimental Hub Importance:");
                for (idx, h) in hubs.iter().enumerate() {
                    println!(
                        "  {:>2}. {} | Hub Score: {:.2} | {} rels (out: {}, in: {}) | {}",
                        idx + 1,
                        h.path,
                        h.hub_score,
                        h.total_relations,
                        h.out_degree,
                        h.in_degree,
                        h.role
                    );
                }
            }
        }
        Commands::Ownership {
            project,
            days,
            limit,
        } => {
            let own = store.project_ownership(&project, days, limit)?;
            if json {
                return print_json(&own);
            }
            println!(
                "Ownership: {} (areas: {}, window: {}d)",
                own.project.name, own.total_areas, own.days
            );

            println!("\nMost Active Areas");
            if own.most_active_areas.is_empty() {
                println!("  (none)");
            } else {
                for (idx, area) in own.most_active_areas.iter().enumerate() {
                    println!(
                        "  {:>2}. {:<24} {:>3} files (events: {}, recent: {}, last: {})",
                        idx + 1,
                        area.area,
                        area.file_count,
                        area.event_count,
                        area.recent_modified_count,
                        format_day(area.latest_modified_at)
                    );
                }
            }

            println!("\nRecently Active ({}d)", own.days);
            if own.recently_active_areas.is_empty() {
                println!("  (none in window)");
            } else {
                for area in &own.recently_active_areas {
                    println!(
                        "  - {:<24} {} recently modified ({})",
                        area.area,
                        area.recent_modified_count,
                        format_day(area.latest_modified_at)
                    );
                }
            }

            println!("\nMost Stable Areas");
            if own.most_stable_areas.is_empty() {
                println!("  (none)");
            } else {
                for area in &own.most_stable_areas {
                    println!(
                        "  - {:<24} {} files (last modified: {})",
                        area.area,
                        area.file_count,
                        format_day(area.latest_modified_at)
                    );
                }
            }
        }
        Commands::Coverage { project } => {
            let cov = store.project_coverage(&project)?;
            let doc_quality = if experimental {
                Some(store.experimental_doc_quality(&project)?)
            } else {
                None
            };
            if json {
                return print_json(&JsonCoverageWithExperimental {
                    coverage: &cov,
                    doc_quality,
                });
            }
            println!("Coverage: {}", cov.project.name);
            println!("\nCode Files: {}", cov.code_files);
            println!("Docs Files: {}", cov.doc_files);
            println!("Coverage:   {:.1}%", cov.coverage_ratio);

            println!("\nWell Covered");
            if cov.well_covered.is_empty() {
                println!("  (none)");
            } else {
                for area in &cov.well_covered {
                    let refs = area.md_link_refs + area.path_ref_refs;
                    println!(
                        "  - {:<24} code: {}, docs: {}, doc-refs: {}, score: {:.1}",
                        area.area, area.code_files, area.local_docs, refs, area.weighted_score
                    );
                }
            }

            println!("\nPartially Covered");
            if cov.partially_covered.is_empty() {
                println!("  (none)");
            } else {
                for area in &cov.partially_covered {
                    let refs = area.md_link_refs + area.path_ref_refs;
                    println!(
                        "  - {:<24} code: {}, docs: {}, doc-refs: {}, score: {:.1}",
                        area.area, area.code_files, area.local_docs, refs, area.weighted_score
                    );
                }
            }

            println!("\nUnderdocumented");
            if cov.underdocumented.is_empty() {
                println!("  (none)");
            } else {
                for area in cov.underdocumented.iter().take(15) {
                    println!(
                        "  - {:<24} code: {}, docs: 0, doc-refs: 0",
                        area.area, area.code_files
                    );
                }
                if cov.underdocumented.len() > 15 {
                    println!("  ... and {} more", cov.underdocumented.len() - 15);
                }
            }

            if let Some(dq) = doc_quality {
                println!("\nExperimental Documentation Quality:");
                println!(
                    "  Docs Analyzed: {} | Mean Quality Score: {:.1} / 100",
                    dq.total_docs, dq.mean_score
                );
                println!(
                    "  Tiers: Rich: {} | Solid: {} | Stub: {}",
                    dq.rich_count, dq.solid_count, dq.stub_count
                );
                if !dq.docs.is_empty() {
                    println!("  Top Docs:");
                    for doc in dq.docs.iter().take(5) {
                        println!(
                            "    - {} | Score: {}/100 [{}] (md_links: {}, path_refs: {})",
                            doc.path, doc.quality_score, doc.tier, doc.md_links, doc.path_refs
                        );
                    }
                }
            }
        }
        Commands::Snapshot {
            project,
            all,
            label,
        } => handle_snapshot(&store, project, all, label, json)?,
        Commands::Snapshots { project, limit } => {
            let snaps = store.list_snapshots(project.as_deref(), limit)?;
            if json {
                return print_json(&snaps);
            }
            if snaps.is_empty() {
                println!("No snapshots saved yet.");
            } else {
                for s in snaps {
                    let lbl = s
                        .label
                        .as_deref()
                        .map(|l| format!(" [{l}]"))
                        .unwrap_or_default();
                    println!(
                        "#{:<3} | {:<12} | {} | {} | files: {} | rels: {} | cov: {:.1}% | crit: {} high: {}{}",
                        s.id,
                        s.project_name,
                        format_ts(s.created_at),
                        s.health,
                        s.active_objects,
                        s.relations_count,
                        s.coverage_ratio,
                        s.critical_risks,
                        s.high_risks,
                        lbl
                    );
                }
            }
        }
        Commands::CompareSnapshot {
            from_id,
            to_id,
            live,
            project,
        } => {
            let diff = resolve_snapshot_diff(&store, from_id, to_id, live, project)?;
            if json {
                return print_json(&diff);
            }
            let target_label = if diff.to.id == 0 {
                format!(
                    "LIVE ({}, {})",
                    diff.to.project_name,
                    format_ts(diff.to.created_at)
                )
            } else {
                format!(
                    "#{} ({}, {})",
                    diff.to.id,
                    diff.to.project_name,
                    format_ts(diff.to.created_at)
                )
            };
            println!(
                "Snapshot Diff: #{} ({}, {}) -> {}",
                diff.from.id,
                diff.from.project_name,
                format_ts(diff.from.created_at),
                target_label
            );
            println!("\nMetrics Delta:");
            println!(
                "  Health:         {} -> {}",
                diff.from.health, diff.to.health
            );
            println!(
                "  Active Files:   {} ({:+})",
                diff.to.active_objects, diff.active_delta
            );
            println!(
                "  Deleted Files:  {} ({:+})",
                diff.to.deleted_objects, diff.deleted_delta
            );
            println!(
                "  Relations:      {} ({:+})",
                diff.to.relations_count, diff.relations_delta
            );
            println!(
                "  Coverage Ratio: {:.1}% ({:+.1}%)",
                diff.to.coverage_ratio, diff.coverage_delta
            );
            println!(
                "  Critical Risks: {} ({:+})",
                diff.to.critical_risks, diff.critical_risks_delta
            );
            println!(
                "  High Risks:     {} ({:+})",
                diff.to.high_risks, diff.high_risks_delta
            );

            if !diff.added_pending_work.is_empty() {
                println!("\nNew pending work ({}):", diff.added_pending_work.len());
                for item in &diff.added_pending_work {
                    println!("  + {item}");
                }
            }
            if !diff.resolved_pending_work.is_empty() {
                println!(
                    "\nResolved pending work ({}):",
                    diff.resolved_pending_work.len()
                );
                for item in &diff.resolved_pending_work {
                    println!("  - {item}");
                }
            }
            if !diff.added_risks.is_empty() {
                println!("\nNew risk areas ({}):", diff.added_risks.len());
                for item in &diff.added_risks {
                    println!("  + {item}");
                }
            }
            if !diff.resolved_risks.is_empty() {
                println!("\nResolved risk areas ({}):", diff.resolved_risks.len());
                for item in &diff.resolved_risks {
                    println!("  - {item}");
                }
            }
            if !diff.new_active_areas.is_empty() {
                println!("\nNew active areas:");
                for item in &diff.new_active_areas {
                    println!("  + {item}");
                }
            }
        }
        Commands::Session { action } => handle_session(&mut store, action, json)?,
        Commands::Doctor { repair } => {
            let report = store.doctor(repair)?;
            if json {
                return print_json(&report);
            }
            println!(
                "VOS Doctor: {}",
                if report.healthy {
                    "HEALTHY"
                } else {
                    "ISSUES DETECTED"
                }
            );
            println!(
                "  SQLite check:       {} ({})",
                report.sqlite_ok, report.sqlite_detail
            );
            println!(
                "  Objects FTS5 parity: {} / {}",
                report.objects_fts_count, report.objects_count
            );
            println!(
                "  Chunks FTS5 parity:  {} / {}",
                report.chunks_fts_count, report.chunks_count
            );
            println!("  Orphaned objects:    {}", report.orphaned_objects);
            println!("  Stale deleted rels:  {}", report.stale_deleted_relations);
            println!("  Active sessions:     {}", report.active_sessions);
            if !report.missing_project_roots.is_empty() {
                println!("  Missing project roots:");
                for m in &report.missing_project_roots {
                    println!("    - {m}");
                }
            }
            if report.repaired {
                println!("  Repair applied:      FTS5 rebuilt & stale deleted relations pruned");
            }
        }
        Commands::Benchmark { action } => handle_benchmark(&store, action, json)?,
    }

    Ok(())
}

#[inline(never)]
fn handle_project(store: &mut Store, action: ProjectCommands, json: bool) -> Result<()> {
    match action {
        ProjectCommands::Create {
            name,
            path,
            description,
        } => {
            let project = store.create_project(&name, Some(&path), description.as_deref())?;
            if json {
                return print_json(&project);
            }
            println!("Created project '{}' ({})", project.name, project.id);
            if let Some(root) = project.root_path {
                println!("Root: {root}");
            }
        }
        ProjectCommands::List => {
            let projects = store.list_projects()?;
            if json {
                return print_json(&projects);
            }
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
        ProjectCommands::Update {
            name,
            rename,
            path,
            description,
        } => {
            let path_changed = path.is_some();
            let project = store.update_project(
                &name,
                rename.as_deref(),
                path.as_deref(),
                description.as_deref(),
            )?;
            if json {
                return print_json(&project);
            }
            println!("Updated project '{}' ({})", project.name, project.id);
            if let Some(ref root) = project.root_path {
                println!("Root: {root}");
            }
            if let Some(ref desc) = project.description {
                println!("Description: {desc}");
            }
            if path_changed {
                println!(
                    "Hint: run `vos index --all` to re-index files under the updated root path."
                );
            }
        }
        ProjectCommands::Remove { name } => {
            let removed_objects = store.delete_project(&name)?;
            if json {
                return print_json(&JsonProjectRemoveReport {
                    project: name,
                    removed_objects,
                });
            }
            println!(
                "Removed project '{}' ({} indexed object(s) purged).",
                name, removed_objects
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn handle_search(
    store: &Store,
    query: Option<String>,
    project: Option<String>,
    tag: Option<String>,
    kind: Option<CliObjectKind>,
    since: Option<String>,
    limit: usize,
    explain: bool,
    include_internal: bool,
    json: bool,
) -> Result<()> {
    let mapped_kind = kind.map(Into::into);
    let since_ts = since.as_deref().map(parse_since_arg).transpose()?;
    let hits = store.search_with_filters(
        query.as_deref(),
        SearchFilters {
            project: project.as_deref(),
            tag: tag.as_deref(),
            kind: mapped_kind,
            since: since_ts,
            limit,
            include_internal,
        },
    )?;
    if json {
        return print_json(&hits);
    }
    if hits.is_empty() {
        let target = query
            .as_deref()
            .map(|q| format!("\"{q}\""))
            .or_else(|| tag.as_deref().map(|t| format!("tag '{t}'")))
            .or_else(|| mapped_kind.map(|k| format!("kind '{k}'")))
            .or_else(|| since.as_deref().map(|s| format!("since '{s}'")))
            .unwrap_or_else(|| "(empty)".to_string());
        println!("No matches found for {target}.");
        return Ok(());
    }
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
    Ok(())
}

#[inline(never)]
fn handle_start(store: &Store, project: &str, experimental: bool, json: bool) -> Result<()> {
    let start = store.project_start(project)?;
    let prediction = if experimental {
        let st = store.project_status(project)?;
        Some(store.predict_recovery_difficulty(&st))
    } else {
        None
    };
    if json {
        return print_json(&JsonStartWithExperimental {
            start: &start,
            recovery_prediction: prediction,
        });
    }
    let res = &start.resume;
    let root_prefix = res.project.root_path.as_deref().unwrap_or("");
    println!(
        "Project: {} ({} active objects)",
        res.project.name, res.active_objects
    );

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
            println!(
                "  {}. {}",
                idx + 1,
                display_relative(&obj.path, root_prefix)
            );
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

    if let Some(ref pred) = prediction {
        print_recovery_prediction(pred);
    }
    Ok(())
}

#[inline(never)]
fn handle_drift(store: &Store, project: Option<String>, all: bool, json: bool) -> Result<()> {
    if all && project.is_some() {
        anyhow::bail!("pass either a project name or --all, not both");
    }
    if all {
        let reports = store.all_project_drifts()?;
        if json {
            return print_json(&reports);
        }
        if reports.is_empty() {
            println!("No projects registered yet.");
        } else {
            let mut total_findings = 0_usize;
            for drift in &reports {
                let count = drift.findings.len();
                total_findings += count;
                println!("{:<14} | {} finding(s)", drift.project.name, count);
                for finding in &drift.findings {
                    println!("  - [{}] {}", finding.kind, finding.summary);
                }
            }
            println!(
                "\nChecked {} project(s): {} total drift finding(s).",
                reports.len(),
                total_findings
            );
        }
        return Ok(());
    }
    let project = project.ok_or_else(|| anyhow::anyhow!("provide a project name or pass --all"))?;
    let drift = store.project_drift(&project)?;
    if json {
        return print_json(&drift);
    }
    println!("Drift report: {}", drift.project.name);
    if drift.findings.is_empty() {
        println!("  No documentation or roadmap drift detected.");
    } else {
        for finding in &drift.findings {
            println!("\n[{}] {}", finding.kind, finding.summary);
            for ev in &finding.evidence {
                println!("  - {ev}");
            }
        }
    }
    Ok(())
}

#[inline(never)]
fn handle_snapshot(
    store: &Store,
    project: Option<String>,
    all: bool,
    label: Option<String>,
    json: bool,
) -> Result<()> {
    if all && project.is_some() {
        anyhow::bail!("pass either a project name or --all, not both");
    }
    if all {
        let snaps = store.save_all_snapshots(label.as_deref())?;
        if json {
            return print_json(&snaps);
        }
        if snaps.is_empty() {
            println!("No projects registered.");
        } else {
            for s in &snaps {
                let lbl = s
                    .label
                    .as_deref()
                    .map(|l| format!(" [{l}]"))
                    .unwrap_or_default();
                println!(
                    "#{:<3} | {:<12} | {} | files: {} | rels: {} | cov: {:.1}% | crit: {} high: {}{}",
                    s.id,
                    s.project_name,
                    s.health,
                    s.active_objects,
                    s.relations_count,
                    s.coverage_ratio,
                    s.critical_risks,
                    s.high_risks,
                    lbl
                );
            }
            println!("Saved {} snapshot(s).", snaps.len());
        }
        return Ok(());
    }
    let project = project.ok_or_else(|| anyhow::anyhow!("provide a project name or pass --all"))?;
    let snap = store.save_snapshot(&project, label.as_deref())?;
    if json {
        return print_json(&snap);
    }
    let lbl = snap
        .label
        .as_deref()
        .map(|l| format!(" [{l}]"))
        .unwrap_or_default();
    println!(
        "Saved snapshot #{}{} for '{}' at {}",
        snap.id,
        lbl,
        snap.project_name,
        format_ts(snap.created_at)
    );
    println!(
        "  Health: {} | Files: {} | Relations: {} | Coverage: {:.1}% ({}) | Crit: {} | High: {}",
        snap.health,
        snap.active_objects,
        snap.relations_count,
        snap.coverage_ratio,
        snap.documentation_health,
        snap.critical_risks,
        snap.high_risks
    );
    Ok(())
}

#[inline(never)]
fn handle_session(store: &mut Store, action: SessionCommands, json: bool) -> Result<()> {
    match action {
        SessionCommands::Start {
            project,
            note,
            no_index,
        } => {
            if !no_index {
                refresh_project_index_if_present(store, &project)?;
            }
            let s = store.start_session(&project, note.as_deref())?;
            if json {
                return print_json(&s);
            }
            println!(
                "Started session {} for '{}' at {}",
                s.id,
                s.project_name,
                format_ts(s.started_at)
            );
            println!(
                "  Predicted Difficulty: {} (score: {})",
                s.predicted_difficulty, s.predicted_score
            );
            if let Some(ref rp) = s.return_point {
                println!("  Suggested return point: {rp}");
            }
        }
        SessionCommands::Stop {
            project,
            quality,
            note,
            no_index,
            log_opts,
        } => {
            if !no_index {
                let target_project = match project.as_deref() {
                    Some(name) => Some(name.to_string()),
                    None => store
                        .list_sessions(None, 20)?
                        .into_iter()
                        .find(|s| s.ended_at.is_none())
                        .map(|s| s.project_name),
                };
                if let Some(proj_name) = target_project {
                    refresh_project_index_if_present(store, &proj_name)?;
                }
            }
            let s = store.stop_session(project.as_deref(), quality, note.as_deref())?;
            if let Some(log_path) = log_opts.log.as_deref() {
                let date = chrono::Local::now().format("%Y-%m-%d").to_string();
                let days_away = match log_opts.days_away.clone() {
                    Some(v) => v,
                    None => store
                        .session_days_away(&s)?
                        .map_or_else(|| "-".to_string(), |d| d.to_string()),
                };
                let row = vos_store::format_recovery_log_row(
                    &s,
                    &date,
                    &days_away,
                    &log_opts.first_command,
                    &log_opts.helpful,
                    &log_opts.ignored,
                )?;
                append_log_row(log_path, &row)?;
                eprintln!("Logged: {row}");
            }
            if json {
                return print_json(&s);
            }
            let dur = s.duration_secs.unwrap_or(0);
            println!(
                "Stopped session {} for '{}' (duration: {}s)",
                s.id, s.project_name, dur
            );
            println!(
                "  Predicted: {} (score: {}) | Actual Quality: {}",
                s.predicted_difficulty,
                s.predicted_score,
                s.recovery_quality
                    .map(|q| format!("{q}/5"))
                    .unwrap_or_else(|| "-".to_string())
            );
            println!("  Files touched: {}", s.files_changed_count);
            if !s.productive_areas.is_empty() {
                println!("  Productive areas: {}", s.productive_areas.join(", "));
            }
        }
        SessionCommands::List { project, limit } => {
            let sessions = store.list_sessions(project.as_deref(), limit)?;
            if json {
                return print_json(&sessions);
            }
            if sessions.is_empty() {
                println!("No work sessions recorded.");
            } else {
                for s in sessions {
                    let status = match s.duration_secs {
                        Some(d) => format!("{d}s"),
                        None => "active".to_string(),
                    };
                    let qual = s
                        .recovery_quality
                        .map(|q| format!("{q}/5"))
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "{} | {:<12} | {} | {:<8} | pred: {:<6} | qual: {} | files: {} | return: {}",
                        s.id,
                        s.project_name,
                        format_ts(s.started_at),
                        status,
                        s.predicted_difficulty,
                        qual,
                        s.files_changed_count,
                        s.return_point.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        SessionCommands::Stats { project } => {
            let stats = store.session_stats(project.as_deref())?;
            if json {
                return print_json(&stats);
            }
            let scope = stats.project_name.as_deref().unwrap_or("all projects");
            println!(
                "Session Stats ({}): total={} completed={} active={}",
                scope, stats.total_sessions, stats.completed_sessions, stats.active_sessions
            );
            if let Some(med) = stats.median_duration_secs {
                println!(
                    "  Duration: median {}s | mean {}s",
                    med,
                    stats.avg_duration_secs.unwrap_or(med)
                );
            }
            if let Some(avg_q) = stats.avg_quality {
                println!("  Mean Recovery Quality: {:.2} / 5.0", avg_q);
            }
            if !stats.frequent_return_points.is_empty() {
                println!("\nMost frequent return points:");
                for (rp, cnt) in &stats.frequent_return_points {
                    println!("  - {rp} ({cnt}x)");
                }
            }
            if !stats.most_productive_areas.is_empty() {
                println!("\nMost productive areas:");
                for (area, cnt) in &stats.most_productive_areas {
                    println!("  - {area} ({cnt} sessions)");
                }
            }
            println!(
                "\nBenchmark progress: {} / {} scored sessions (v1.1 semantic-layer gate: 20-30)",
                stats.scored_sessions, RECOVERY_GATE_SESSIONS
            );
            if !stats.by_predicted_difficulty.is_empty() {
                println!("\nPredicted vs Actual:");
                for row in &stats.by_predicted_difficulty {
                    let avg_d = row
                        .avg_duration_secs
                        .map(|d| format!("{d}s"))
                        .unwrap_or_else(|| "-".to_string());
                    let avg_q = row
                        .avg_quality
                        .map(|q| format!("{q:.1}/5"))
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "  - {:<6} | sessions: {} | avg duration: {} | avg quality: {}",
                        row.difficulty, row.sessions, avg_d, avg_q
                    );
                }
                if stats.hard_scored_sessions > 0 {
                    println!(
                        "  False Alarms (Hard -> fast & quality >= 4): {} / {}",
                        stats.false_alarms, stats.hard_scored_sessions
                    );
                }
                if stats.easy_scored_sessions > 0 {
                    println!(
                        "  Missed Difficulty (Easy -> slow or quality <= 2): {} / {}",
                        stats.missed_difficulty, stats.easy_scored_sessions
                    );
                }
            }
        }
    }
    Ok(())
}

/// Appends one markdown table row, starting a new line if the file lacks a trailing newline.
const RECOVERY_GATE_SESSIONS: usize = 20;

fn append_log_row(path: &std::path::Path, row: &str) -> Result<()> {
    use std::io::Write;
    let needs_newline = std::fs::read(path)
        .map(|b| !b.is_empty() && !b.ends_with(b"\n"))
        .unwrap_or(false);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open log file {}", path.display()))?;
    if needs_newline {
        writeln!(f)?;
    }
    writeln!(f, "{row}")?;
    Ok(())
}

#[inline(never)]
fn handle_benchmark(store: &Store, action: BenchmarkCommands, json: bool) -> Result<()> {
    match action {
        BenchmarkCommands::Search {
            config,
            save,
            min_top5,
            min_mrr,
        } => {
            let cases = load_benchmark_cases(&config)?;
            let report = store.run_search_benchmark(&cases)?;
            let saved_run_id = if save {
                let suite_name = config
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| config.display().to_string());
                Some((store.save_benchmark_run(&suite_name, &report)?, suite_name))
            } else {
                None
            };
            if json {
                print_json(&JsonBenchmarkSaveResult {
                    report: &report,
                    saved_run_id: saved_run_id.as_ref().map(|(id, _)| *id),
                })?;
                return check_benchmark_thresholds(&report, min_top5, min_mrr);
            }

            for case in &report.cases {
                let t1 = case.top1_hit.map(fmt_hit).unwrap_or("-");
                let t3 = case.top3_hit.map(fmt_hit).unwrap_or("-");
                let t5 = case.top5_hit.map(fmt_hit).unwrap_or("-");
                let cp = if case.cross_project_total > 0 {
                    format!(
                        " | proj: {}/{}",
                        case.cross_project_matched, case.cross_project_total
                    )
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

            if let Some((run_id, suite_name)) = saved_run_id {
                println!("\nSaved benchmark run #{run_id} ({suite_name}).");
            }
            check_benchmark_thresholds(&report, min_top5, min_mrr)?;
        }
        BenchmarkCommands::History { limit } => {
            let runs = store.list_benchmark_runs(limit)?;
            if json {
                return print_json(&runs);
            }
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
                        format!(
                            "{:.1}%",
                            pct(run.cross_project_matched, run.cross_project_total)
                        )
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
    }
    Ok(())
}

fn check_benchmark_thresholds(
    report: &vos_core::SearchBenchmarkReport,
    min_top5: Option<f64>,
    min_mrr: Option<f64>,
) -> Result<()> {
    if let Some(req_top5) = min_top5
        && report.top5_total > 0
    {
        let actual_top5 = pct(report.top5_passed, report.top5_total);
        if actual_top5 + f64::EPSILON < req_top5 {
            anyhow::bail!(
                "benchmark Top-5 {:.1}% fell below required --min-top5 {:.1}%",
                actual_top5,
                req_top5
            );
        }
    }
    if let Some(req_mrr) = min_mrr
        && report.mrr + f64::EPSILON < req_mrr
    {
        anyhow::bail!(
            "benchmark MRR {:.3} fell below required --min-mrr {:.3}",
            report.mrr,
            req_mrr
        );
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
        .map(|dt| {
            dt.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
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

fn parse_since_arg(raw: &str) -> Result<i64> {
    parse_since_arg_at(raw, now_unix(), Local::now().date_naive())
}

fn parse_since_arg_at(raw: &str, reference_now: i64, today: NaiveDate) -> Result<i64> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();

    match lower.as_str() {
        "today" => return local_midnight_timestamp(today, trimmed),
        "yesterday" => {
            let yday = today
                .pred_opt()
                .ok_or_else(|| anyhow::anyhow!("cannot resolve date before {today}"))?;
            return local_midnight_timestamp(yday, trimmed);
        }
        _ => {}
    }

    for (suffix, unit_secs) in [('h', 3_600_i64), ('d', 86_400_i64), ('w', 604_800_i64)] {
        if let Some(num_str) = lower.strip_suffix(suffix)
            && let Ok(amount) = num_str.parse::<i64>()
            && amount >= 0
        {
            return Ok(reference_now.saturating_sub(amount.saturating_mul(unit_secs)));
        }
    }

    let date = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").map_err(|e| {
        anyhow::anyhow!(
            "invalid since '{trimmed}' (expected YYYY-MM-DD, Nh, Nd, Nw, today, or yesterday): {e}"
        )
    })?;
    local_midnight_timestamp(date, trimmed)
}

fn local_midnight_timestamp(date: NaiveDate, label: &str) -> Result<i64> {
    let naive_dt = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid midnight for {label}"))?;
    let local_dt = Local
        .from_local_datetime(&naive_dt)
        .earliest()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve local time for {label}"))?;
    Ok(local_dt.timestamp())
}

fn render_graph_tree(nodes: &[GraphTreeNode], root_prefix: &str, prefix: &str, is_incoming: bool) {
    for (idx, node) in nodes.iter().enumerate() {
        let is_last = idx + 1 == nodes.len();
        let branch = if is_last { "└─" } else { "├─" };
        let rel_path = display_relative(&node.object.path, root_prefix);
        let arrow = if is_incoming { "<- " } else { "" };
        println!(
            "{prefix}{branch} {arrow}{rel_path} [{}]",
            node.relation_type
        );
        if !node.children.is_empty() {
            let child_prefix = if is_last {
                format!("{prefix}   ")
            } else {
                format!("{prefix}│  ")
            };
            render_graph_tree(&node.children, root_prefix, &child_prefix, is_incoming);
        }
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct JsonIndexReport {
    scanned: usize,
    added: usize,
    modified: usize,
    restored: usize,
    unchanged: usize,
    deleted: usize,
    skipped: usize,
    relations: usize,
    elapsed_ms: u128,
}

#[derive(serde::Serialize)]
struct JsonStatusWithExperimental<'a> {
    #[serde(flatten)]
    status: &'a vos_core::ProjectStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_prediction: Option<vos_core::RecoveryPrediction>,
}

#[derive(serde::Serialize)]
struct JsonGraphWithExperimental<'a> {
    #[serde(flatten)]
    graph: &'a vos_core::ProjectGraph,
    #[serde(skip_serializing_if = "Option::is_none")]
    hub_importance: Option<Vec<vos_core::HubImportance>>,
}

#[derive(serde::Serialize)]
struct JsonCoverageWithExperimental<'a> {
    #[serde(flatten)]
    coverage: &'a vos_core::ProjectCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    doc_quality: Option<vos_core::DocQualityReport>,
}

#[derive(serde::Serialize)]
struct JsonWhyWithExperimental<'a> {
    #[serde(flatten)]
    why: &'a vos_core::ObjectWhy,
    #[serde(skip_serializing_if = "Option::is_none")]
    experimental_impact: Option<vos_core::ObjectImpact>,
}

#[derive(serde::Serialize)]
struct JsonBenchmarkSaveResult<'a> {
    #[serde(flatten)]
    report: &'a vos_core::SearchBenchmarkReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_run_id: Option<i64>,
}

#[derive(serde::Serialize)]
struct JsonStartWithExperimental<'a> {
    #[serde(flatten)]
    start: &'a vos_core::ProjectStart,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_prediction: Option<vos_core::RecoveryPrediction>,
}

#[derive(serde::Serialize)]
struct JsonContinueWithExperimental<'a> {
    #[serde(flatten)]
    cont: &'a vos_core::ProjectContinue,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_prediction: Option<vos_core::RecoveryPrediction>,
}

fn print_recovery_prediction(pred: &vos_core::RecoveryPrediction) {
    println!("\nExperimental Insights:");
    println!(
        "  Recovery Difficulty: {} (score: {})",
        pred.difficulty, pred.score
    );
    println!("  Factors:");
    for factor in &pred.factors {
        println!("    + {factor}");
    }
}

fn print_last_session(sess: &vos_core::WorkSession) {
    println!("\nLast work session:");
    if let Some(ended) = sess.ended_at {
        let dur = sess.duration_secs.unwrap_or(0);
        println!(
            "  Ended {} ({}) | Duration: {} | Files touched: {}",
            format_relative(ended),
            format_day(ended),
            format_duration(dur),
            sess.files_changed_count
        );
    } else {
        println!(
            "  Active since {} ({}) | Predicted: {} (score: {})",
            format_relative(sess.started_at),
            format_day(sess.started_at),
            sess.predicted_difficulty,
            sess.predicted_score
        );
    }
    if let Some(ref rp) = sess.return_point {
        println!("  Return point: {rp}");
    }
    if let Some(q) = sess.recovery_quality {
        println!("  Quality: {q}/5");
    }
    if !sess.productive_areas.is_empty() {
        println!("  Productive areas: {}", sess.productive_areas.join(", "));
    }
    if let Some(ref note) = sess.stop_note {
        println!("  Note: {note}");
    } else if let Some(ref note) = sess.start_note {
        println!("  Start note: {note}");
    }
}

fn format_duration(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

impl From<&vos_indexer::IndexReport> for JsonIndexReport {
    fn from(report: &vos_indexer::IndexReport) -> Self {
        Self {
            scanned: report.scanned,
            added: report.added,
            modified: report.modified,
            restored: report.restored,
            unchanged: report.unchanged,
            deleted: report.deleted,
            skipped: report.skipped,
            relations: report.relations,
            elapsed_ms: report.elapsed.as_millis(),
        }
    }
}

#[derive(serde::Serialize)]
struct JsonAllIndexEntry {
    project: String,
    #[serde(flatten)]
    report: JsonIndexReport,
}

#[derive(serde::Serialize)]
struct JsonAllIndexReport {
    projects: Vec<JsonAllIndexEntry>,
    skipped_projects: Vec<String>,
    total_scanned: usize,
    total_added: usize,
    total_modified: usize,
    total_restored: usize,
    total_unchanged: usize,
    total_deleted: usize,
    total_relations: usize,
}

fn index_all_projects(store: &mut Store, no_ignore: bool) -> Result<JsonAllIndexReport> {
    let projects = store.list_projects()?;
    let mut entries = Vec::new();
    let mut skipped_projects = Vec::new();
    let mut total_scanned = 0_usize;
    let mut total_added = 0_usize;
    let mut total_modified = 0_usize;
    let mut total_restored = 0_usize;
    let mut total_unchanged = 0_usize;
    let mut total_deleted = 0_usize;
    let mut total_relations = 0_usize;

    for item in projects {
        let Some(root) = item.project.root_path.as_deref() else {
            skipped_projects.push(format!("{} (no root_path)", item.project.name));
            continue;
        };
        let root_path = Path::new(root);
        if !root_path.exists() {
            skipped_projects.push(format!("{} (missing: {root})", item.project.name));
            continue;
        }
        let report = index_path_with_options(
            store,
            root_path,
            Some(&item.project.name),
            IndexOptions { no_ignore },
        )?;
        total_scanned += report.scanned;
        total_added += report.added;
        total_modified += report.modified;
        total_restored += report.restored;
        total_unchanged += report.unchanged;
        total_deleted += report.deleted;
        total_relations += report.relations;
        entries.push(JsonAllIndexEntry {
            project: item.project.name,
            report: JsonIndexReport::from(&report),
        });
    }

    Ok(JsonAllIndexReport {
        projects: entries,
        skipped_projects,
        total_scanned,
        total_added,
        total_modified,
        total_restored,
        total_unchanged,
        total_deleted,
        total_relations,
    })
}

fn print_all_index_report(batch: &JsonAllIndexReport) {
    if batch.projects.is_empty() {
        println!("No registered projects with accessible paths to index.");
    } else {
        for entry in &batch.projects {
            let upd = entry.report.added + entry.report.modified + entry.report.restored;
            println!(
                "{:<14} | scanned: {:>4} | updated: {:>3} | unchanged: {:>4} | deleted: {:>2} | rels: {:>4} | {}ms",
                entry.project,
                entry.report.scanned,
                upd,
                entry.report.unchanged,
                entry.report.deleted,
                entry.report.relations,
                entry.report.elapsed_ms
            );
        }
        println!(
            "\nTotal ({} projects): scanned={} updated={} unchanged={} deleted={} relations={}",
            batch.projects.len(),
            batch.total_scanned,
            batch.total_added + batch.total_modified + batch.total_restored,
            batch.total_unchanged,
            batch.total_deleted,
            batch.total_relations
        );
    }
    for skipped in &batch.skipped_projects {
        println!("Skipped: {skipped}");
    }
}

fn collect_all_statuses(
    store: &Store,
    experimental: bool,
) -> Result<
    Vec<(
        vos_core::ProjectStatus,
        Option<vos_core::RecoveryPrediction>,
    )>,
> {
    let projects = store.list_projects()?;
    let mut rows = Vec::with_capacity(projects.len());
    for item in projects {
        let st = store.project_status(&item.project.name)?;
        let pred = if experimental {
            Some(store.predict_recovery_difficulty(&st))
        } else {
            None
        };
        rows.push((st, pred));
    }
    rows.sort_by(|a, b| {
        b.0.critical_risk_count
            .cmp(&a.0.critical_risk_count)
            .then_with(|| b.0.high_risk_count.cmp(&a.0.high_risk_count))
            .then_with(|| b.0.last_change_at.cmp(&a.0.last_change_at))
            .then_with(|| a.0.project.name.cmp(&b.0.project.name))
    });
    Ok(rows)
}

fn print_all_statuses(
    rows: &[(
        vos_core::ProjectStatus,
        Option<vos_core::RecoveryPrediction>,
    )],
) {
    if rows.is_empty() {
        println!("No projects registered yet.");
        return;
    }
    for (st, pred) in rows {
        let last = st
            .last_change_at
            .map(format_day)
            .unwrap_or_else(|| "never".to_string());
        let exp_col = match pred {
            Some(p) => format!(" | diff: {:<6} ({})", p.difficulty.as_str(), p.score),
            None => String::new(),
        };
        println!(
            "{:<14} | {:<7} | crit: {:>2} | high: {:>2} | cov: {:>5.1}% ({:<7}) | files: {:>3} | last: {}{}",
            st.project.name,
            st.health.as_str(),
            st.critical_risk_count,
            st.high_risk_count,
            st.coverage_ratio,
            st.documentation_health.as_str(),
            st.active_objects,
            last,
            exp_col
        );
    }
}

fn resolve_snapshot_diff(
    store: &Store,
    from_id: Option<i64>,
    to_id: Option<i64>,
    live: bool,
    project: Option<String>,
) -> Result<vos_core::SnapshotDiff> {
    if live {
        let target_project = match project {
            Some(name) => name,
            None => {
                let id = from_id.ok_or_else(|| {
                    anyhow::anyhow!("provide --project <name> or baseline snapshot ID with --live")
                })?;
                store.get_snapshot(id)?.project_name
            }
        };
        Ok(store.compare_snapshot_to_live(&target_project, from_id)?)
    } else {
        let from =
            from_id.ok_or_else(|| anyhow::anyhow!("provide <FROM_ID> <TO_ID>, or use --live"))?;
        let to = to_id.ok_or_else(|| {
            anyhow::anyhow!("provide <TO_ID> (or pass --live to compare against current state)")
        })?;
        Ok(store.compare_snapshots(from, to)?)
    }
}

fn refresh_project_index_if_present(store: &mut Store, project_name: &str) -> Result<()> {
    if let Some(proj) = store.get_project_by_name(project_name)?
        && let Some(root) = proj.root_path.as_deref()
    {
        let root_path = Path::new(root);
        if root_path.exists() {
            index_path_with_options(
                store,
                root_path,
                Some(&proj.name),
                IndexOptions { no_ignore: false },
            )?;
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct JsonProjectRemoveReport {
    project: String,
    removed_objects: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_supports_relative_units_keywords_and_dates() {
        let now = 1_760_000_000_i64;
        let today = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();

        assert_eq!(
            parse_since_arg_at("12h", now, today).unwrap(),
            now - 12 * 3_600
        );
        assert_eq!(
            parse_since_arg_at("7d", now, today).unwrap(),
            now - 7 * 86_400
        );
        assert_eq!(
            parse_since_arg_at("2W", now, today).unwrap(),
            now - 14 * 86_400
        );
        assert_eq!(parse_since_arg_at("0d", now, today).unwrap(), now);

        let today_ts = parse_since_arg_at(" today ", now, today).unwrap();
        let yesterday_ts = parse_since_arg_at("YESTERDAY", now, today).unwrap();
        let explicit_ts = parse_since_arg_at("2026-11-24", now, today).unwrap();
        assert_eq!(today_ts, explicit_ts);
        assert_eq!(today_ts - yesterday_ts, 86_400);
    }

    #[test]
    fn parse_since_rejects_invalid_inputs() {
        let now = 1_760_000_000_i64;
        let today = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();

        for bad in ["", "-3d", "abc", "2026-13-40", "5m"] {
            assert!(
                parse_since_arg_at(bad, now, today).is_err(),
                "expected error for '{bad}'"
            );
        }
    }

    #[test]
    fn benchmark_thresholds_accept_passing_metrics() {
        let report = vos_core::SearchBenchmarkReport {
            total_cases: 10,
            top1_passed: 2,
            top1_total: 2,
            top3_passed: 5,
            top3_total: 5,
            top5_passed: 9,
            top5_total: 10,
            mrr: 0.85,
            noise_hits: 0,
            total_returned_hits: 40,
            cross_project_matched: 2,
            cross_project_total: 2,
            categories: Vec::new(),
            cases: Vec::new(),
        };
        assert!(check_benchmark_thresholds(&report, Some(90.0), Some(0.85)).is_ok());
    }

    #[test]
    fn benchmark_thresholds_reject_failing_top5_or_mrr() {
        let report = vos_core::SearchBenchmarkReport {
            total_cases: 10,
            top1_passed: 1,
            top1_total: 2,
            top3_passed: 4,
            top3_total: 5,
            top5_passed: 8,
            top5_total: 10,
            mrr: 0.72,
            noise_hits: 0,
            total_returned_hits: 40,
            cross_project_matched: 2,
            cross_project_total: 2,
            categories: Vec::new(),
            cases: Vec::new(),
        };
        assert!(check_benchmark_thresholds(&report, Some(85.0), None).is_err());
        assert!(check_benchmark_thresholds(&report, None, Some(0.75)).is_err());
    }
}
