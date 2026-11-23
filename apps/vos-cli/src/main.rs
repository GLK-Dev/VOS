//! Command-line interface for VOS (Project Memory System).

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Local};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use vos_indexer::{IndexOptions, index_path_with_options};
use vos_store::Store;

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
    /// Recover current project context (active files, breakdown, recent timeline).
    Context {
        /// Project name.
        project: String,
        /// Maximum number of recent objects and events to show.
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Show chronological project events (added, modified, deleted, restored).
    Timeline {
        /// Project name.
        project: String,
        /// Maximum number of events to show.
        #[arg(long, default_value_t = 20)]
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
        Commands::Index { path, project, no_ignore } => {
            let report = index_path_with_options(
                &mut store,
                &path,
                project.as_deref(),
                IndexOptions { no_ignore },
            )?;
            println!(
                "Indexed {} file(s): added={}, modified={}, restored={}, unchanged={}, deleted={}, skipped={}",
                report.scanned,
                report.added,
                report.modified,
                report.restored,
                report.unchanged,
                report.deleted,
                report.skipped
            );
        }
        Commands::Search {
            query,
            project,
            limit,
        } => {
            let hits = store.search(&query, project.as_deref(), limit)?;
            if hits.is_empty() {
                println!("No matches found for \"{query}\".");
            } else {
                for (idx, hit) in hits.iter().enumerate() {
                    let proj = hit.project_name.as_deref().unwrap_or("-");
                    println!(
                        "{}. {} [{} | project: {}]",
                        idx + 1,
                        hit.title,
                        hit.kind,
                        proj
                    );
                    println!("   {}", hit.path);
                    let compact_snippet = hit.snippet.replace('\n', " ");
                    if !compact_snippet.trim().is_empty() {
                        println!("   {compact_snippet}");
                    }
                }
            }
        }
        Commands::Show { target } => {
            let details = store.show_object(&target)?;
            let obj = &details.object;
            println!("Object: {} ({})", obj.title, obj.id);
            println!("Path:   {}", obj.path);
            println!("Kind:   {}", obj.kind);
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
        Commands::Context { project, limit } => {
            let ctx = store.project_context(&project, limit)?;
            println!("Project: {} ({})", ctx.project.name, ctx.project.id);
            if let Some(ref desc) = ctx.project.description {
                println!("About:   {desc}");
            }
            if let Some(ref root) = ctx.project.root_path {
                println!("Root:    {root}");
            }
            println!(
                "Objects: {} active, {} deleted | Updated: {}",
                ctx.active_objects,
                ctx.deleted_objects,
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

            println!("\nRecent files:");
            if ctx.recent_objects.is_empty() {
                println!("  (none)");
            } else {
                let root_prefix = ctx.project.root_path.as_deref().unwrap_or("");
                for obj in ctx.recent_objects {
                    let display_path = display_relative(&obj.path, root_prefix);
                    println!(
                        "  - [{}] {} ({})",
                        obj.kind,
                        display_path,
                        format_ts(obj.modified_at)
                    );
                }
            }

            println!("\nRecent activity:");
            if ctx.recent_events.is_empty() {
                println!("  (none)");
            } else {
                for ev in ctx.recent_events {
                    println!(
                        "  - {} | {:8} | {}",
                        format_ts(ev.created_at),
                        ev.event_type,
                        ev.path
                    );
                }
            }
        }
        Commands::Timeline { project, limit } => {
            let events = store.project_timeline(&project, limit)?;
            if events.is_empty() {
                println!("No events recorded for project '{project}'.");
            } else {
                for ev in events {
                    println!(
                        "{} | {:8} | {}",
                        format_ts(ev.created_at),
                        ev.event_type,
                        ev.path
                    );
                }
            }
        }
    }

    Ok(())
}

fn format_ts(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
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

fn default_db_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"));
    match home {
        Some(dir) => PathBuf::from(dir).join(".vos").join("vos.db"),
        None => PathBuf::from(".vos").join("vos.db"),
    }
}
