//! MCP tool schemas and deterministic VOS store/indexer dispatchers.

use std::path::{Path, PathBuf};

use chrono::{Duration, Local, NaiveDate, TimeZone};
use serde::Serialize;
use serde_json::{Value, json};
use vos_core::compact::{
    CompactContinue, CompactGraph, CompactSearchHit, CompactStatus, CompactWhy,
};
use vos_core::util::now_unix;
use vos_core::{GraphDirection, ObjectKind};
use vos_indexer::{IndexOptions, IndexReport, InitOptions, index_path_with_options, init_project};
use vos_store::{SearchFilters, Store};

use crate::McpError;

#[derive(Serialize)]
struct McpIndexReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
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

impl McpIndexReport {
    fn from_report(project: Option<String>, report: &IndexReport) -> Self {
        Self {
            project,
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

#[derive(Serialize)]
struct McpAllIndexReport {
    projects: Vec<McpIndexReport>,
    skipped_projects: Vec<String>,
    total_scanned: usize,
    total_added: usize,
    total_modified: usize,
    total_restored: usize,
    total_unchanged: usize,
    total_deleted: usize,
    total_relations: usize,
}

/// Returns the JSON Schema tool catalog exposed by `tools/list`.
pub(crate) fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "vos_list_projects",
            "description": "List all registered VOS projects with active/deleted object counts and root paths.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_continue",
            "description": "Get a token-efficient project continuation briefing (last session, entry points, active areas, risks, hub counts, and recovery prediction). Call at task start.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Registered VOS project name." },
                    "experimental": { "type": "boolean", "description": "Include recovery prediction (default: true)." }
                },
                "required": ["project"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_status",
            "description": "Get deterministic project health, coverage ratio, documentation health, and risk summary for one project or all projects.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Project name (required unless all=true)." },
                    "all": { "type": "boolean", "description": "Return status across all registered projects." },
                    "experimental": { "type": "boolean", "description": "Include recovery prediction (default: true)." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_search",
            "description": "Search indexed code, docs, notes, and configs via deterministic FTS5 ranking and structural filters.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Full-text search query (optional if filters are provided)." },
                    "project": { "type": "string", "description": "Restrict search to one project." },
                    "tag": { "type": "string", "description": "Filter by exact tag." },
                    "kind": { "type": "string", "enum": ["code", "note", "doc", "config", "model", "media", "other"], "description": "Filter by object kind." },
                    "since": { "type": "string", "description": "Filter modified since date/offset (YYYY-MM-DD, today, yesterday, 7d, 12h, 2w)." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Max hits to return (default: 10)." },
                    "explain": { "type": "boolean", "description": "Include ranking signals per hit (default: false)." },
                    "include_internal": { "type": "boolean", "description": "Include internal search configs/benchmarks (default: false)." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_show",
            "description": "Inspect metadata, tags, summary, and chunk count for an indexed file or object.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Object ID, title, or relative/suffix file path." },
                    "project": { "type": "string", "description": "Optional project scope to disambiguate paths." }
                },
                "required": ["target"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_why",
            "description": "Explain why a file matters: active area, tags, related objects, bounded dependency edges, and blast-radius impact.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Object ID, title, or relative/suffix file path." },
                    "project": { "type": "string", "description": "Optional project scope to disambiguate paths." },
                    "relation_limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Max incoming/outgoing edges per direction (default: 12)." },
                    "experimental": { "type": "boolean", "description": "Include blast-radius hub impact (default: true)." }
                },
                "required": ["target"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_graph",
            "description": "Explore the deterministic code/documentation dependency graph for a project or specific target file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Registered VOS project name." },
                    "target": { "type": "string", "description": "Optional target file/title; omit for project hub overview." },
                    "direction": { "type": "string", "enum": ["out", "in", "both"], "description": "Traversal direction (default: both)." },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 4, "description": "Traversal depth (default: 1)." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Max hubs or edges per level (default: 5)." },
                    "experimental": { "type": "boolean", "description": "Include hub importance metrics (default: true)." }
                },
                "required": ["project"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_drift",
            "description": "Detect structural documentation drift across one project or all registered projects.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Project name (required unless all=true)." },
                    "all": { "type": "boolean", "description": "Run drift across all registered projects." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_index",
            "description": "Idempotently index or refresh a registered project, directory path, or all registered projects.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Project name; uses stored root_path if path is omitted." },
                    "path": { "type": "string", "description": "Filesystem path to index." },
                    "all": { "type": "boolean", "description": "Refresh all registered projects." },
                    "no_ignore": { "type": "boolean", "description": "Include files ignored by .gitignore/.vosignore." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_session_start",
            "description": "Start a tracked work session for a project, recording predicted recovery difficulty and optional start note.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Registered VOS project name." },
                    "note": { "type": "string", "description": "Optional start/goal note." }
                },
                "required": ["project"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_session_stop",
            "description": "Stop the active work session and save handoff stop note and optional recovery quality rating (1-5).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Optional project name when stopping a specific project session." },
                    "quality": { "type": "integer", "minimum": 1, "maximum": 5, "description": "Recovery quality score 1 (hard) to 5 (instant)." },
                    "note": { "type": "string", "description": "Concise handoff summary for the next session." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_init",
            "description": "Idempotently register a project directory, index it, and scaffold .mcp.json and .vscode/mcp.json.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path to initialize (defaults to '.')." },
                    "name": { "type": "string", "description": "Explicit project name (defaults to directory slug)." },
                    "description": { "type": "string", "description": "Optional short description." },
                    "index": { "type": "boolean", "description": "Index directory immediately (default: true)." },
                    "setup_mcp": { "type": "boolean", "description": "Scaffold .mcp.json and .vscode/mcp.json (default: true)." },
                    "force": { "type": "boolean", "description": "Overwrite existing 'vos' MCP entries (default: false)." },
                    "no_ignore": { "type": "boolean", "description": "Include ignored files when indexing (default: false)." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_coverage",
            "description": "Report relation-weighted documentation coverage by area and optional doc quality scores.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Registered VOS project name." },
                    "experimental": { "type": "boolean", "description": "Include documentation quality metrics (default: true)." }
                },
                "required": ["project"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_evolution",
            "description": "Summarize added/modified/deleted activity, emerging hotspots, and unresolved signals since a date or offset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Registered VOS project name." },
                    "since": { "type": "string", "description": "Start date/offset (YYYY-MM-DD, 30d, 2w, today; default: 30d)." }
                },
                "required": ["project"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_changed_since",
            "description": "List files added, modified, or deleted since a date or relative offset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "since": { "type": "string", "description": "Start date/offset (YYYY-MM-DD, 7d, 12h, today, yesterday)." },
                    "project": { "type": "string", "description": "Optional project filter." }, "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Max tags to return (default: 50)." }
                },
                "required": ["since"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_related",
            "description": "Find files deterministically related to a target via directory proximity, shared tags, co-activity, or links.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Object ID, title, or relative/suffix file path." },
                    "project": { "type": "string", "description": "Optional project scope." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 30, "description": "Max related hits (default: 8)." }
                },
                "required": ["target"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_snapshot",
            "description": "Create, list, or compare live project state against saved VOS snapshots.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "list", "compare_live"], "description": "Operation mode (default: list)." },
                    "project": { "type": "string", "description": "Project name (required for compare_live or single create)." },
                    "all": { "type": "boolean", "description": "For action=create, snapshot all registered projects." },
                    "label": { "type": "string", "description": "Optional label when creating a snapshot." },
                    "from_id": { "type": "integer", "description": "Optional snapshot ID to compare against live state." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Max snapshots to list (default: 10)." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_doctor",
            "description": "Check SQLite + FTS5 integrity, orphan objects, stale deleted relations, and missing project root paths.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repair": { "type": "boolean", "description": "Repair FTS5 drift and clean stale state (default: false)." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_tags",
            "description": "List all attached tags with object counts (optionally scoped to one project).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Optional project filter." }, "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Max tags to return (default: 50)." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_tag",
            "description": "Attach one or more manual tags to an indexed file or object.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Object ID, title, or relative/suffix file path." },
                    "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Tag names to attach." },
                    "project": { "type": "string", "description": "Optional project scope." }
                },
                "required": ["target", "tags"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "vos_untag",
            "description": "Remove one or more manual tags from an indexed file or object.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Object ID, title, or relative/suffix file path." },
                    "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Tag names to remove." },
                    "project": { "type": "string", "description": "Optional project scope." }
                },
                "required": ["target", "tags"],
                "additionalProperties": false
            }
        }),
    ]
}

/// Executes an MCP tool call and returns a compact serialized JSON payload.
pub(crate) fn execute_tool(store: &mut Store, name: &str, args: &Value) -> Result<Value, McpError> {
    let args_obj = match args {
        Value::Null => json!({}),
        Value::Object(_) => args.clone(),
        _ => return Err(McpError::InvalidParams("arguments must be a JSON object".into())),
    };

    match name {
        "vos_list_projects" => {
            let projects = store.list_projects()?;
            Ok(serde_json::to_value(projects)?)
        }
        "vos_continue" => {
            let project = require_str(&args_obj, "project")?;
            let experimental = opt_bool(&args_obj, "experimental")?.unwrap_or(true);
            let cont = store.project_continue(project)?;
            let prediction = if experimental {
                let st = store.project_status(project)?;
                Some(store.predict_recovery_difficulty(&st))
            } else {
                None
            };
            Ok(serde_json::to_value(CompactContinue::from_continue(
                &cont,
                prediction,
            ))?)
        }
        "vos_status" => handle_status(store, &args_obj),
        "vos_search" => handle_search(store, &args_obj),
        "vos_show" => {
            let target = require_str(&args_obj, "target")?;
            let project = opt_str(&args_obj, "project")?;
            let details = store.show_object_scoped(target, project)?;
            Ok(serde_json::to_value(details)?)
        }
        "vos_why" => {
            let target = require_str(&args_obj, "target")?;
            let project = opt_str(&args_obj, "project")?;
            let relation_limit = opt_usize(&args_obj, "relation_limit")?.unwrap_or(12).clamp(1, 50);
            let experimental = opt_bool(&args_obj, "experimental")?.unwrap_or(true);
            let why = store.why_object(target, project)?;
            let impact = if experimental {
                Some(store.experimental_why_impact(&why.object.id)?)
            } else {
                None
            };
            Ok(serde_json::to_value(CompactWhy::from_why(
                &why,
                relation_limit,
                impact,
            ))?)
        }
        "vos_graph" => handle_graph(store, &args_obj),
        "vos_drift" => {
            let all = opt_bool(&args_obj, "all")?.unwrap_or(false);
            let project = opt_str(&args_obj, "project")?;
            if all {
                let reports = store.all_project_drifts()?;
                Ok(serde_json::to_value(reports)?)
            } else if let Some(proj) = project {
                let report = store.project_drift(proj)?;
                Ok(serde_json::to_value(report)?)
            } else {
                Err(McpError::InvalidParams(
                    "provide 'project' or set 'all': true".into(),
                ))
            }
        }
        "vos_index" => handle_index(store, &args_obj),
        "vos_session_start" => {
            let project = require_str(&args_obj, "project")?;
            let note = opt_str(&args_obj, "note")?;
            let session = store.start_session(project, note)?;
            Ok(serde_json::to_value(session)?)
        }
        "vos_session_stop" => {
            let project = opt_str(&args_obj, "project")?;
            let note = opt_str(&args_obj, "note")?;
            let quality = opt_u8(&args_obj, "quality")?;
            let session = store.stop_session(project, quality, note)?;
            Ok(serde_json::to_value(session)?)
        }
        "vos_init" => handle_init(store, &args_obj),
        "vos_coverage" => {
            let project = require_str(&args_obj, "project")?;
            let experimental = opt_bool(&args_obj, "experimental")?.unwrap_or(true);
            let coverage = store.project_coverage(project)?;
            let doc_quality = if experimental {
                Some(store.experimental_doc_quality(project)?)
            } else {
                None
            };
            Ok(json!({
                "coverage": coverage,
                "doc_quality": doc_quality
            }))
        }
        "vos_evolution" => {
            let project = require_str(&args_obj, "project")?;
            let since_raw = opt_str(&args_obj, "since")?.unwrap_or("30d");
            let since_ts = parse_since_arg(since_raw)?;
            let evo = store.project_evolution(project, since_ts, since_raw)?;
            Ok(serde_json::to_value(evo)?)
        }
        "vos_changed_since" => {
            let since_raw = require_str(&args_obj, "since")?;
            let since_ts = parse_since_arg(since_raw)?;
            let project = opt_str(&args_obj, "project")?;
            let report = store.changed_since(since_ts, since_raw, project)?;
            Ok(serde_json::to_value(report)?)
        }
        "vos_related" => {
            let target = require_str(&args_obj, "target")?;
            let project = opt_str(&args_obj, "project")?;
            let limit = opt_usize(&args_obj, "limit")?.unwrap_or(8).clamp(1, 30);
            let hits = store.find_related(target, project, limit)?;
            Ok(serde_json::to_value(hits)?)
        }
        "vos_snapshot" => handle_snapshot(store, &args_obj),
        "vos_doctor" => {
            let repair = opt_bool(&args_obj, "repair")?.unwrap_or(false);
            let report = store.doctor(repair)?;
            Ok(serde_json::to_value(report)?)
        }
        "vos_tags" => {
            let project = opt_str(&args_obj, "project")?;
            let limit = opt_usize(&args_obj, "limit")?.unwrap_or(50).clamp(1, 200);
            let tags = store.list_tags(project, limit)?;
            Ok(serde_json::to_value(tags)?)
        }
        "vos_tag" => {
            let target = require_str(&args_obj, "target")?;
            let project = opt_str(&args_obj, "project")?;
            let tags = require_str_array(&args_obj, "tags")?;
            let obj = store.add_tags_scoped(target, project, &tags)?;
            Ok(serde_json::to_value(obj)?)
        }
        "vos_untag" => {
            let target = require_str(&args_obj, "target")?;
            let project = opt_str(&args_obj, "project")?;
            let tags = require_str_array(&args_obj, "tags")?;
            let obj = store.remove_tags_scoped(target, project, &tags)?;
            Ok(serde_json::to_value(obj)?)
        }
        _ => Err(McpError::UnknownTool(name.to_string())),
    }
}

fn handle_status(store: &Store, args: &Value) -> Result<Value, McpError> {
    let all = opt_bool(args, "all")?.unwrap_or(false);
    let experimental = opt_bool(args, "experimental")?.unwrap_or(true);
    let project = opt_str(args, "project")?;
    if all {
        let projects = store.list_projects()?;
        let mut out = Vec::with_capacity(projects.len());
        for p in projects {
            let st = store.project_status(&p.project.name)?;
            let pred = if experimental {
                Some(store.predict_recovery_difficulty(&st))
            } else {
                None
            };
            out.push(CompactStatus::from_status(&st, pred));
        }
        return Ok(serde_json::to_value(out)?);
    }
    let proj = project.ok_or_else(|| {
        McpError::InvalidParams("provide 'project' or set 'all': true".into())
    })?;
    let st = store.project_status(proj)?;
    let pred = if experimental {
        Some(store.predict_recovery_difficulty(&st))
    } else {
        None
    };
    Ok(serde_json::to_value(CompactStatus::from_status(&st, pred))?)
}

fn handle_search(store: &Store, args: &Value) -> Result<Value, McpError> {
    let query = opt_str(args, "query")?;
    let project = opt_str(args, "project")?;
    let tag = opt_str(args, "tag")?;
    let kind = match opt_str(args, "kind")? {
        Some(raw) => Some(parse_object_kind(raw)?),
        None => None,
    };
    let since = match opt_str(args, "since")? {
        Some(raw) => Some(parse_since_arg(raw)?),
        None => None,
    };
    let limit = opt_usize(args, "limit")?.unwrap_or(10).clamp(1, 50);
    let explain = opt_bool(args, "explain")?.unwrap_or(false);
    let include_internal = opt_bool(args, "include_internal")?.unwrap_or(false);

    if query.is_none() && tag.is_none() && kind.is_none() && since.is_none() {
        return Err(McpError::InvalidParams(
            "provide 'query' or at least one filter ('tag', 'kind', 'since')".into(),
        ));
    }

    let root_prefix = match project {
        Some(name) => store
            .get_project_by_name(name)?
            .and_then(|p| p.root_path),
        None => None,
    };

    let hits = store.search_with_filters(
        query,
        SearchFilters {
            project,
            tag,
            kind,
            since,
            limit,
            include_internal,
        },
    )?;

    let compact_hits: Vec<CompactSearchHit> = hits
        .iter()
        .map(|h| CompactSearchHit::from_hit(h, root_prefix.as_deref(), explain))
        .collect();
    Ok(serde_json::to_value(compact_hits)?)
}

fn handle_graph(store: &Store, args: &Value) -> Result<Value, McpError> {
    let project = require_str(args, "project")?;
    let target = opt_str(args, "target")?;
    let direction = match opt_str(args, "direction")? {
        Some("out") | Some("outgoing") => GraphDirection::Outgoing,
        Some("in") | Some("incoming") => GraphDirection::Incoming,
        Some("both") | None => GraphDirection::Both,
        Some(other) => {
            return Err(McpError::InvalidParams(format!(
                "invalid direction '{other}' (expected out|in|both)"
            )));
        }
    };
    let depth = opt_usize(args, "depth")?.unwrap_or(1).clamp(1, 4);
    let limit = opt_usize(args, "limit")?.unwrap_or(5).clamp(1, 20);
    let experimental = opt_bool(args, "experimental")?.unwrap_or(true);

    let graph = store.project_graph(project, target, direction, depth, limit)?;
    let importance = if experimental {
        Some(store.experimental_hub_importance(project, limit)?)
    } else {
        None
    };
    Ok(serde_json::to_value(CompactGraph::from_graph(
        &graph,
        importance,
    ))?)
}

fn handle_index(store: &mut Store, args: &Value) -> Result<Value, McpError> {
    let all = opt_bool(args, "all")?.unwrap_or(false);
    let no_ignore = opt_bool(args, "no_ignore")?.unwrap_or(false);
    let project = opt_str(args, "project")?.map(ToString::to_string);
    let path = opt_str(args, "path")?.map(PathBuf::from);

    if all {
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
            entries.push(McpIndexReport::from_report(Some(item.project.name), &report));
        }

        return Ok(serde_json::to_value(McpAllIndexReport {
            projects: entries,
            skipped_projects,
            total_scanned,
            total_added,
            total_modified,
            total_restored,
            total_unchanged,
            total_deleted,
            total_relations,
        })?);
    }

    let target_path = match (path, project.as_deref()) {
        (Some(p), _) => p,
        (None, Some(proj_name)) => {
            let proj = store.get_project_by_name(proj_name)?.ok_or_else(|| {
                McpError::InvalidParams(format!("project not found: {proj_name}"))
            })?;
            let root = proj.root_path.ok_or_else(|| {
                McpError::InvalidParams(format!(
                    "project '{proj_name}' has no root_path; pass 'path' explicitly"
                ))
            })?;
            PathBuf::from(root)
        }
        (None, None) => {
            return Err(McpError::InvalidParams(
                "provide 'path', 'project', or set 'all': true".into(),
            ));
        }
    };

    let report = index_path_with_options(
        store,
        &target_path,
        project.as_deref(),
        IndexOptions { no_ignore },
    )?;
    Ok(serde_json::to_value(McpIndexReport::from_report(
        project,
        &report,
    ))?)
}

fn handle_init(store: &mut Store, args: &Value) -> Result<Value, McpError> {
    let raw_path = opt_str(args, "path")?.unwrap_or(".");
    let name = opt_str(args, "name")?;
    let description = opt_str(args, "description")?;
    let index = opt_bool(args, "index")?.unwrap_or(true);
    let setup_mcp = opt_bool(args, "setup_mcp")?.unwrap_or(true);
    let force = opt_bool(args, "force")?.unwrap_or(false);
    let no_ignore = opt_bool(args, "no_ignore")?.unwrap_or(false);

    let report = init_project(
        store,
        InitOptions {
            path: Path::new(raw_path),
            name,
            description,
            index,
            no_ignore,
            setup_mcp,
            force,
        },
    )?;

    Ok(json!({
        "project": report.project,
        "project_created": report.project_created,
        "index_report": report.index_report.as_ref().map(|r| McpIndexReport::from_report(Some(report.project.name.clone()), r)),
        "mcp_configs": report.mcp_configs,
    }))
}

fn handle_snapshot(store: &Store, args: &Value) -> Result<Value, McpError> {
    let action = opt_str(args, "action")?.unwrap_or("list");
    let project = opt_str(args, "project")?;
    let all = opt_bool(args, "all")?.unwrap_or(false);
    let label = opt_str(args, "label")?;
    let limit = opt_usize(args, "limit")?.unwrap_or(10).clamp(1, 50);

    match action {
        "create" => {
            if all {
                let snaps = store.save_all_snapshots(label)?;
                Ok(serde_json::to_value(snaps)?)
            } else if let Some(proj) = project {
                let snap = store.save_snapshot(proj, label)?;
                Ok(serde_json::to_value(snap)?)
            } else {
                Err(McpError::InvalidParams(
                    "provide 'project' or set 'all': true for snapshot action=create".into(),
                ))
            }
        }
        "list" => {
            let snaps = store.list_snapshots(project, limit)?;
            Ok(serde_json::to_value(snaps)?)
        }
        "compare_live" => {
            let proj = project.ok_or_else(|| {
                McpError::InvalidParams("'project' is required for snapshot action=compare_live".into())
            })?;
            let from_id = match args.get("from_id") {
                None | Some(Value::Null) => None,
                Some(Value::Number(n)) => n.as_i64(),
                Some(_) => {
                    return Err(McpError::InvalidParams("'from_id' must be an integer".into()));
                }
            };
            let diff = store.compare_snapshot_to_live(proj, from_id)?;
            Ok(serde_json::to_value(diff)?)
        }
        other => Err(McpError::InvalidParams(format!(
            "unknown snapshot action '{other}' (expected create|list|compare_live)"
        ))),
    }
}

fn require_str_array(obj: &Value, key: &str) -> Result<Vec<String>, McpError> {
    let Some(arr) = obj.get(key).and_then(Value::as_array) else {
        return Err(McpError::InvalidParams(format!(
            "missing required string array argument '{key}'"
        )));
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(McpError::InvalidParams(format!(
                "argument '{key}' entries must be non-empty strings"
            )));
        };
        out.push(s.to_string());
    }
    if out.is_empty() {
        return Err(McpError::InvalidParams(format!(
            "argument '{key}' cannot be empty"
        )));
    }
    Ok(out)
}

fn require_str<'a>(obj: &'a Value, key: &str) -> Result<&'a str, McpError> {
    opt_str(obj, key)?.ok_or_else(|| {
        McpError::InvalidParams(format!("missing required string argument '{key}'"))
    })
}

fn opt_str<'a>(obj: &'a Value, key: &str) -> Result<Option<&'a str>, McpError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        Some(_) => Err(McpError::InvalidParams(format!(
            "argument '{key}' must be a string"
        ))),
    }
}

fn opt_bool(obj: &Value, key: &str) -> Result<Option<bool>, McpError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(McpError::InvalidParams(format!(
            "argument '{key}' must be a boolean"
        ))),
    }
}

fn opt_usize(obj: &Value, key: &str) -> Result<Option<usize>, McpError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let val = n.as_u64().ok_or_else(|| {
                McpError::InvalidParams(format!("argument '{key}' must be a positive integer"))
            })?;
            usize::try_from(val).map(Some).map_err(|_| {
                McpError::InvalidParams(format!("argument '{key}' is out of range"))
            })
        }
        Some(_) => Err(McpError::InvalidParams(format!(
            "argument '{key}' must be an integer"
        ))),
    }
}

fn opt_u8(obj: &Value, key: &str) -> Result<Option<u8>, McpError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let val = n.as_u64().ok_or_else(|| {
                McpError::InvalidParams(format!("argument '{key}' must be an integer"))
            })?;
            u8::try_from(val).map(Some).map_err(|_| {
                McpError::InvalidParams(format!("argument '{key}' is out of range"))
            })
        }
        Some(_) => Err(McpError::InvalidParams(format!(
            "argument '{key}' must be an integer"
        ))),
    }
}

fn parse_object_kind(raw: &str) -> Result<ObjectKind, McpError> {
    match raw.to_ascii_lowercase().as_str() {
        "code" => Ok(ObjectKind::Code),
        "note" => Ok(ObjectKind::Note),
        "doc" => Ok(ObjectKind::Doc),
        "config" => Ok(ObjectKind::Config),
        "model" => Ok(ObjectKind::Model),
        "media" => Ok(ObjectKind::Media),
        "other" => Ok(ObjectKind::Other),
        _ => Err(McpError::InvalidParams(format!(
            "unknown object kind '{raw}'"
        ))),
    }
}

fn parse_since_arg(raw: &str) -> Result<i64, McpError> {
    let reference_now = now_unix();
    let today = Local::now().date_naive();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(McpError::InvalidParams("since cannot be empty".into()));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower == "today" {
        return start_of_local_day(today);
    }
    if lower == "yesterday" {
        let yesterday = today
            .checked_sub_signed(Duration::days(1))
            .ok_or_else(|| McpError::InvalidParams("date underflow".into()))?;
        return start_of_local_day(yesterday);
    }
    if let Some(num) = lower.strip_suffix('d')
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit())
    {
        let days: i64 = num.parse().map_err(|_| {
            McpError::InvalidParams(format!("invalid day offset '{trimmed}'"))
        })?;
        return Ok(reference_now - days * 86_400);
    }
    if let Some(num) = lower.strip_suffix('w')
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit())
    {
        let weeks: i64 = num.parse().map_err(|_| {
            McpError::InvalidParams(format!("invalid week offset '{trimmed}'"))
        })?;
        return Ok(reference_now - weeks * 7 * 86_400);
    }
    if let Some(num) = lower.strip_suffix('h')
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit())
    {
        let hours: i64 = num.parse().map_err(|_| {
            McpError::InvalidParams(format!("invalid hour offset '{trimmed}'"))
        })?;
        return Ok(reference_now - hours * 3_600);
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return start_of_local_day(date);
    }
    if let Ok(ts) = trimmed.parse::<i64>() {
        return Ok(ts);
    }
    Err(McpError::InvalidParams(format!(
        "invalid since value '{trimmed}' (expected YYYY-MM-DD, today, yesterday, Nd, Nh, Nw, or unix timestamp)"
    )))
}

fn start_of_local_day(date: NaiveDate) -> Result<i64, McpError> {
    let naive = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| McpError::InvalidParams("invalid local day".into()))?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| Local.from_local_datetime(&naive).latest())
        .map(|dt| dt.timestamp())
        .ok_or_else(|| McpError::InvalidParams("ambiguous local date".into()))
}